//! Protocol handling and the tool implementations. Every tool answers with the
//! same schema-1 JSON as the matching `lcw … --json` command (built by
//! `lcw-agent`); the only difference is a default token budget, because MCP
//! results land straight in the agent's context.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lcw_agent::json;
use lcw_agent::{LoadOptions, PathStyle, Repo};
use lcw_config::Language;
use lcw_core::NodeId;
use lcw_query::{CallTreeOptions, Direction, EntryKind, Lookup};
use serde_json::{json, Value};

/// Protocol revision we implement; we echo the client's when it sends one.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Default token budget for tool results (`budget: 0` disables it).
const DEFAULT_BUDGET: usize = 4000;

/// How long a loaded graph is trusted before re-checking source mtimes.
const RECHECK_AFTER: Duration = Duration::from_secs(2);

/// One loaded repository and when its freshness was last verified.
struct Loaded {
    repo: Repo,
    checked: Instant,
}

/// Cache key: canonical root, language override, semantic mode.
type RepoKey = (PathBuf, Option<Language>, bool);

pub struct Server {
    repos: HashMap<RepoKey, Loaded>,
}

/// A tool failure, reported to the client as an `isError` result whose text
/// is a schema-1 error object.
struct ToolError(Value);

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        ToolError(json::error("failed", &format!("{e:#}")))
    }
}

type ToolResult = Result<Value, ToolError>;

impl Server {
    pub fn new() -> Self {
        Server {
            repos: HashMap::new(),
        }
    }

    /// Handle one stdin line; `None` for notifications (no reply).
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(
                    rpc_error(Value::Null, -32700, &format!("parse error: {e}")).to_string(),
                )
            }
        };
        let reply = match msg {
            Value::Array(batch) => {
                let out: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(m)).collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            m => self.handle(m),
        };
        reply.map(|r| r.to_string())
    }

    /// Handle one JSON-RPC message.
    pub fn handle(&mut self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (no id) never get a reply.
        let id = id?;
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_list() })),
            "tools/call" => self.call(&params),
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, m)) => rpc_error(id, code, &m),
        })
    }

    fn call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !tool_list().iter().any(|t| t["name"] == name) {
            return Err((-32602, format!("unknown tool: {name}")));
        }
        let started = Instant::now();
        let out = self.run_tool(name, &args);
        lcw_telemetry::info!(target: "lcw::mcp", tool = name, ms = started.elapsed().as_millis() as u64, "tool call");
        let (value, is_error) = match out {
            Ok(v) => (v, false),
            Err(ToolError(v)) => (v, true),
        };
        Ok(json!({
            "content": [{ "type": "text", "text": value.to_string() }],
            "isError": is_error,
        }))
    }

    fn run_tool(&mut self, name: &str, args: &Value) -> ToolResult {
        let budget = match opt_u64(args, "budget") {
            Some(0) => None,
            Some(b) => Some(b as usize),
            None => Some(DEFAULT_BUDGET),
        };
        let repo = self.repo(args)?;
        let g = &repo.graph;
        let root = repo.root.to_string_lossy().into_owned();
        match name {
            "explain" => {
                let id = find(g, req_str(args, "symbol")?)?;
                let limit = opt_u64(args, "limit").unwrap_or(u64::MAX) as usize;
                Ok(lcw_agent::fit_json(budget, |l| {
                    json::explain(g, id, l.min(limit)).unwrap_or(Value::Null)
                }))
            }
            "callers" | "callees" => {
                let id = find(g, req_str(args, "symbol")?)?;
                let (dir, depth, width) = if name == "callers" {
                    (Direction::Callers, 1, 50)
                } else {
                    (Direction::Callees, 3, 12)
                };
                let depth = opt_u64(args, "depth").map_or(depth, |d| d as u32);
                let width = opt_u64(args, "width").map_or(width, |w| w as usize);
                let external = opt_bool(args, "external").unwrap_or(false);
                Ok(lcw_agent::fit_json(budget, |l| {
                    let opts = CallTreeOptions {
                        direction: dir,
                        max_depth: depth,
                        max_children: width.min(l).max(1),
                        include_external: external,
                    };
                    json::call_tree(g, &lcw_query::call_tree(g, id, &opts), dir, depth)
                }))
            }
            "impact" => {
                let id = find(g, req_str(args, "symbol")?)?;
                let depth = opt_u64(args, "depth").map_or(3, |d| d as u32);
                let imp = lcw_query::impact(g, id, depth)
                    .ok_or_else(|| ToolError(json::error("failed", "symbol out of range")))?;
                Ok(lcw_agent::fit_json(budget, |l| json::impact(g, &imp, l)))
            }
            "span" => {
                let id = find(g, req_str(args, "symbol")?)?;
                let s = lcw_query::span(g, id).ok_or_else(|| {
                    ToolError(json::error("no_span", "symbol has no source location"))
                })?;
                let code = opt_bool(args, "code")
                    .unwrap_or(false)
                    .then(|| json::span_code(&root, &s))
                    .flatten();
                Ok(json::span(&s, code.as_deref()))
            }
            "flow" => {
                let to_q = req_str(args, "to")?;
                let from_q = args.get("from").and_then(Value::as_str).unwrap_or("main");
                let from = find_all(g, from_q)?;
                let to = find_all(g, to_q)?;
                let max_paths = opt_u64(args, "max_paths").map_or(6, |n| n as usize).max(1);
                let max_depth = opt_u64(args, "max_depth").map_or(24, |n| n as u32);
                let snippets = opt_bool(args, "snippets").unwrap_or(false);
                let fp = lcw_query::shortest_paths(g, &from, &to, max_paths, max_depth)
                    .ok_or_else(|| {
                        ToolError(json::error(
                            "no_path",
                            &format!(
                                "no call path from {from_q:?} to {to_q:?} within {max_depth} hops"
                            ),
                        ))
                    })?;
                Ok(lcw_agent::fit_json(budget, |l| {
                    json::flow(g, &fp, snippets, &root, l)
                }))
            }
            "outline" => {
                let mut outline = lcw_query::outline(g);
                if let Some(f) = args.get("filter").and_then(Value::as_str) {
                    outline = outline.prune(f);
                }
                let opts = json::OutlineOpts {
                    max_depth: opt_u64(args, "depth").map(|d| d as u32),
                    scopes_only: opt_bool(args, "scopes_only").unwrap_or(false),
                };
                Ok(lcw_agent::fit_json(budget, |l| {
                    json::outline(g, &outline, &opts, l)
                }))
            }
            "entries" => {
                let mut all = lcw_query::entry_points(g);
                if let Some(k) = args.get("kind").and_then(Value::as_str) {
                    let kind = parse_entry_kind(k).ok_or_else(|| {
                        ToolError(json::error(
                            "bad_argument",
                            "kind must be one of main, exported, public, root, test",
                        ))
                    })?;
                    all.retain(|e| e.kind == kind);
                }
                let reach = opt_bool(args, "reach").unwrap_or(false);
                let limit = opt_u64(args, "limit").map_or(40, |n| n as usize);
                Ok(lcw_agent::fit_json(budget, |l| {
                    let (rows, more) = json::rank_entries(g, all.clone(), reach, limit.min(l));
                    json::entries(g, &rows, more)
                }))
            }
            "search_symbol" => {
                let q = req_str(args, "query")?;
                let limit = opt_u64(args, "limit").map_or(20, |n| n as usize);
                Ok(lcw_agent::fit_json(budget, |l| {
                    json::search(g, q, limit.min(l))
                }))
            }
            _ => unreachable!("checked against tool_list"),
        }
    }

    /// The loaded repo for `args.repo`, loading (or refreshing) as needed.
    fn repo(&mut self, args: &Value) -> Result<&Repo, ToolError> {
        let path = req_str(args, "repo")?;
        let language =
            match args.get("language").and_then(Value::as_str) {
                Some(l) => Some(l.parse::<Language>().map_err(|e| {
                    ToolError(json::error("bad_argument", &format!("language: {e}")))
                })?),
                None => None,
            };
        let semantic = opt_bool(args, "semantic").unwrap_or(false);
        let root = PathBuf::from(path)
            .canonicalize()
            .map_err(|e| ToolError(json::error("bad_argument", &format!("repo {path:?}: {e}"))))?;
        let key: RepoKey = (root.clone(), language, semantic);
        let refresh = opt_bool(args, "refresh").unwrap_or(false);

        let stale = match self.repos.get_mut(&key) {
            None => true,
            Some(_) if refresh => true,
            Some(l) if l.checked.elapsed() < RECHECK_AFTER => false,
            Some(l) => {
                let stale = l
                    .repo
                    .engine()
                    .staleness(&l.repo.root)
                    .map_or(true, |s| s.is_some());
                l.checked = Instant::now();
                stale
            }
        };
        if stale {
            let opts = LoadOptions {
                config: None,
                semantic,
                language,
                use_cache: !refresh,
                paths: PathStyle::Absolute,
            };
            let repo = lcw_agent::load(&root, &opts)?;
            self.repos.insert(
                key.clone(),
                Loaded {
                    repo,
                    checked: Instant::now(),
                },
            );
        }
        Ok(&self.repos[&key].repo)
    }
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "lcw-mcp", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Livewalk call-graph navigation. Prefer `span` (exact file:start-end of a definition) and `explain` (callers/callees with call sites) over reading whole files; use `impact` before changing a function, `search_symbol` when unsure of a name. Every tool takes `repo` (absolute path). Results are schema-1 JSON; lists cut by the token budget carry `<list>_more: N`.",
    })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Resolve a symbol strictly: ambiguity and misses become tool errors that
/// list candidates.
fn find(g: &lcw_core::CodeGraph, q: &str) -> Result<NodeId, ToolError> {
    let l = lcw_query::lookup(g, q, lcw_agent::MAX_CANDIDATES);
    match l {
        Lookup::Found(id) => Ok(id),
        _ => Err(ToolError(
            json::lookup_error(g, q, &l).unwrap_or(Value::Null),
        )),
    }
}

/// Resolve a symbol to every match (flow endpoints are sets); a miss lists
/// fuzzy candidates.
fn find_all(g: &lcw_core::CodeGraph, q: &str) -> Result<Vec<NodeId>, ToolError> {
    let hits = lcw_query::resolve(g, q);
    if !hits.is_empty() {
        return Ok(hits);
    }
    let l = Lookup::NotFound(lcw_query::suggest(g, q, lcw_agent::MAX_CANDIDATES));
    Err(ToolError(
        json::lookup_error(g, q, &l).unwrap_or(Value::Null),
    ))
}

fn parse_entry_kind(s: &str) -> Option<EntryKind> {
    Some(match s {
        "main" => EntryKind::Main,
        "exported" => EntryKind::Exported,
        "public" => EntryKind::PublicRoot,
        "root" => EntryKind::Root,
        "test" => EntryKind::Test,
        _ => return None,
    })
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| {
        ToolError(json::error(
            "bad_argument",
            &format!("missing string argument `{key}`"),
        ))
    })
}

fn opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// Tool definitions: name, description and JSON Schema for the arguments.
fn tool_list() -> Vec<Value> {
    let common = json!({
        "repo": { "type": "string", "description": "Absolute path to the repository root." },
        "language": { "type": "string", "enum": ["rust", "typescript", "python", "go", "cpp"], "description": "Source language (default: livewalk.toml, else rust)." },
        "budget": { "type": "integer", "minimum": 0, "description": "Approximate max tokens for the result (default 4000, 0 = unlimited). Cut lists get `<list>_more`." },
        "refresh": { "type": "boolean", "description": "Force a re-analysis instead of using the cached graph." },
    });
    let symbol = json!({ "type": "string", "description": "Symbol: a qualified name or a unique part of one (e.g. `Engine::analyze`). Ambiguous or unknown names return candidates." });
    let tool = |name: &str, desc: &str, extra: Value, required: &[&str]| -> Value {
        let mut props = common.as_object().cloned().unwrap_or_default();
        if let Value::Object(m) = extra {
            props.extend(m);
        }
        let mut req = vec!["repo"];
        req.extend_from_slice(required);
        json!({
            "name": name,
            "description": desc,
            "inputSchema": { "type": "object", "properties": props, "required": req },
        })
    };
    vec![
        tool(
            "explain",
            "One symbol's card: definition location and span, metrics, direct callers and callees with call sites.",
            json!({ "symbol": symbol, "limit": { "type": "integer", "minimum": 1, "description": "Max callers/callees per side." } }),
            &["symbol"],
        ),
        tool(
            "callers",
            "Who calls this symbol: a caller tree (default depth 1) with the call-site file:line of each call.",
            json!({ "symbol": symbol, "depth": { "type": "integer", "minimum": 1 }, "width": { "type": "integer", "minimum": 1, "description": "Max children per node." } }),
            &["symbol"],
        ),
        tool(
            "callees",
            "What this symbol calls, in call order: a callee tree (default depth 3) with call sites.",
            json!({ "symbol": symbol, "depth": { "type": "integer", "minimum": 1 }, "width": { "type": "integer", "minimum": 1 }, "external": { "type": "boolean", "description": "Include external/unresolved targets." } }),
            &["symbol"],
        ),
        tool(
            "impact",
            "Blast radius before a change: transitive callers up to `depth` (default 3) with counts per depth, files touched, entry points reached, and file:line + call site per caller.",
            json!({ "symbol": symbol, "depth": { "type": "integer", "minimum": 1 } }),
            &["symbol"],
        ),
        tool(
            "span",
            "Exact definition range `file:start-end` (attributes/docs included) so you can read or edit only those lines; `code: true` returns the body.",
            json!({ "symbol": symbol, "code": { "type": "boolean" } }),
            &["symbol"],
        ),
        tool(
            "flow",
            "Shortest call path(s) from an entry (default `main`) to a target symbol; `snippets: true` adds each hop's code.",
            json!({
                "to": { "type": "string", "description": "Target symbol." },
                "from": { "type": "string", "description": "Start symbol (default `main`)." },
                "max_paths": { "type": "integer", "minimum": 1 },
                "max_depth": { "type": "integer", "minimum": 1 },
                "snippets": { "type": "boolean" },
            }),
            &["to"],
        ),
        tool(
            "outline",
            "Code hierarchy crate > module > type > function with locations and complexity.",
            json!({
                "filter": { "type": "string", "description": "Keep functions whose qualified name contains this." },
                "depth": { "type": "integer", "minimum": 0, "description": "Collapse scopes at this depth (0 = crates)." },
                "scopes_only": { "type": "boolean" },
            }),
            &[],
        ),
        tool(
            "entries",
            "Entry points: main, foreign-ABI exports, uncalled public API, private roots, tests.",
            json!({
                "kind": { "type": "string", "enum": ["main", "exported", "public", "root", "test"] },
                "reach": { "type": "boolean", "description": "Count reachable functions for every entry." },
                "limit": { "type": "integer", "minimum": 1, "description": "Max entries per kind (default 40)." },
            }),
            &[],
        ),
        tool(
            "search_symbol",
            "Find symbols by name (substring, best first; fuzzy suggestions when nothing matches).",
            json!({ "query": { "type": "string" }, "limit": { "type": "integer", "minimum": 1 } }),
            &["query"],
        ),
    ]
}
