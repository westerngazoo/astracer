//! `lcw` - the Live Code Walk command-line interface.
//!
//! A standalone front end over `lcw-engine`, driven by `livewalk.toml`
//! (Manifesto: CLI guided by config files). The navigation commands
//! (`explain`, `flow`, `outline`, `entries`, `calls`, `callers`, `impact`,
//! `span`, `search`) are thin renderers over `lcw-query`, the same traversal
//! code the viewers use; their `--json` is the schema-1 contract from
//! `lcw-agent`, shared with the `lcw-mcp` server.

mod cache_cmd;
mod export;
mod flow;
mod nav;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use lcw_agent::{json as aj, LoadOptions, PathStyle, Repo};
use lcw_config::{Config, Language};
use lcw_core::{AnalysisReport, CodeGraph, NodeId};
use lcw_engine::{Engine, GraphSource};
use lcw_query::{CallTreeOptions, Direction, EntryKind, Lookup};

/// Live Code Walk & Analysis.
#[derive(Debug, Parser)]
#[command(name = "lcw", version, about = "Live Code Walk & Analysis", long_about = None)]
struct Cli {
    /// Log directive (e.g. `info`, `lcw_engine=debug`). Overrides config.
    #[arg(long, global = true)]
    log: Option<String>,

    /// Approximate token budget for query output: lists (callers, callees,
    /// paths, entries...) shrink until the output fits, ending in a
    /// `+N more` marker (`<list>_more` in JSON).
    #[arg(long, global = true, value_name = "TOKENS")]
    budget: Option<usize>,

    /// Compact output for agents: a budget of 1500 tokens unless `--budget`
    /// is given.
    #[arg(long, global = true)]
    compact: bool,

    /// Pretty-print JSON (indented, multi-line). JSON is single-line by
    /// default to keep agent context small.
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Command,
}

/// Default budget behind `--compact`.
const COMPACT_BUDGET: usize = 1500;

/// Output settings shared by every query command.
#[derive(Debug, Clone, Copy)]
struct Out {
    budget: Option<usize>,
    compact_json: bool,
}

impl Out {
    fn print_json(&self, v: &serde_json::Value) -> Result<()> {
        let s = if self.compact_json {
            serde_json::to_string(v)
        } else {
            serde_json::to_string_pretty(v)
        }
        .context("serializing JSON")?;
        println!("{s}");
        Ok(())
    }
}

/// A failure already reported on stdout (e.g. symbol candidates): exit 2
/// without printing anything else.
#[derive(Debug)]
struct Reported;

impl std::fmt::Display for Reported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reported")
    }
}

impl std::error::Error for Reported {}

#[derive(Debug, Subcommand)]
enum Command {
    /// Analyze a repository and emit a report or graph.
    Analyze(AnalyzeArgs),
    /// Explain one symbol: its metrics, callers (inputs) and callees (outputs).
    Explain(ExplainArgs),
    /// Trace the shortest call path(s) from an entry point to a target symbol.
    Flow(FlowArgs),
    /// Print the code hierarchy (crate ▸ module ▸ type ▸ function) as a tree.
    Outline(OutlineArgs),
    /// List entry points: `main`/`_start`, foreign-ABI exports, uncalled
    /// public API, private roots, tests.
    Entries(EntriesArgs),
    /// Print the call tree below (or above) a symbol: what it triggers.
    Calls(CallsArgs),
    /// Direct callers of a symbol with the call-site `file:line` of each call
    /// (`--depth` for callers of callers).
    Callers(CallersArgs),
    /// Blast radius: transitive callers up to `--depth`, with counts per
    /// depth, files touched and entry points reached.
    Impact(ImpactArgs),
    /// Exact definition range `file:start-end` (attributes and docs
    /// included); `--code` prints just those lines.
    Span(SpanArgs),
    /// Find symbols by name: substring matches best-first, or fuzzy
    /// suggestions when nothing matches.
    Search(SearchArgs),
    /// Write a default `livewalk.toml` to the current directory.
    Init(InitArgs),
    /// Show the on-disk graph store for a repository (if present).
    Graph(GraphArgs),
    /// Load a cached graph from graph.sqlite (no parse). Requires a prior analyze.
    Load(LoadArgs),
    /// Inspect or prune on-disk analysis caches (`~/.cache/livewalk` or
    /// `LIVEWALK_CACHE_HOME/livewalk` by default).
    Cache {
        #[command(subcommand)]
        cmd: cache_cmd::CacheCommand,
    },
    /// Open the interactive graph viewer (native window).
    #[cfg(feature = "viewer")]
    View(AnalyzeArgs),
    /// Render the graph to a PNG image (headless, no window).
    #[cfg(feature = "viewer")]
    Shot(ShotArgs),
}

#[derive(Debug, Args)]
struct AnalyzeArgs {
    /// Path to the repository (defaults to the current directory).
    path: Option<PathBuf>,

    /// Explicit config file. Otherwise `livewalk.toml` is searched for.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = Format::Summary)]
    format: Format,

    /// Write output to a file instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// How many entries to show in summary top-lists.
    #[arg(long, default_value_t = 10)]
    top: usize,

    /// Print cache / graph-store paths after analysis.
    #[arg(short, long)]
    verbose: bool,

    /// Load graph.sqlite instead of re-parsing (requires a prior `lcw analyze`).
    #[arg(long)]
    cached: bool,

    /// Fast-mode source language for this run (overrides `livewalk.toml` `[adapter].language`).
    #[arg(long, value_parser = clap::value_parser!(Language))]
    language: Option<Language>,

    /// Which graph the viewer renders: every function boxed by crate, the
    /// free call graph, or the aggregated module view. Only affects `view`.
    #[cfg(feature = "viewer")]
    #[arg(long = "view", value_enum, default_value_t = ViewKind::Crates)]
    view_kind: ViewKind,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Format {
    Summary,
    Json,
    Dot,
    Graphml,
    /// The laid-out graph view the desktop frontend consumes: the report
    /// snapshot, one `[x, y]` position per node (laid out one box per crate)
    /// and the crate boxes themselves. Serve it as
    /// `fixture.json` next to the built frontend to run the UI in a plain
    /// browser (browser dev mode) without the Tauri shell.
    #[cfg(feature = "viewer")]
    View,
}

/// Which graph a visual command renders.
#[cfg(feature = "viewer")]
#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq)]
enum ViewKind {
    /// Every function, inside a box per crate; the boxes stack in rows by call
    /// order, callers above callees, external code last.
    #[default]
    Crates,
    /// The full function-level call graph as one free force layout.
    Call,
    /// Aggregated module view: one node per module, crates as boxed clusters.
    Module,
}

#[cfg(feature = "viewer")]
#[derive(Debug, Args)]
struct ShotArgs {
    /// Path to the repository (defaults to the current directory).
    path: Option<PathBuf>,

    /// Explicit config file. Otherwise `livewalk.toml` is searched for.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// PNG file to write.
    #[arg(short, long, default_value = "livewalk-graph.png")]
    output: PathBuf,

    /// Image width in pixels.
    #[arg(long, default_value_t = 1600)]
    width: u32,

    /// Image height in pixels.
    #[arg(long, default_value_t = 1000)]
    height: u32,

    /// Which graph to render: every function boxed by crate, the free call
    /// graph, or the aggregated module view.
    #[arg(long = "view", value_enum, default_value_t = ViewKind::Crates)]
    view_kind: ViewKind,

    /// Highlight one symbol and overlay its detail panel (inputs/outputs).
    /// Only applies to the function-level views (`crates`, `call`).
    #[arg(long)]
    select: Option<String>,

    /// With `--select` as the target, trace and highlight the shortest call
    /// path from this entry point (adds a breadcrumb). Call view only.
    #[arg(long)]
    flow_from: Option<String>,
}

/// Where to analyze and how — shared by every navigation command.
#[derive(Debug, Args)]
struct RepoArgs {
    /// Repository path.
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Explicit config file. Otherwise `livewalk.toml` is searched for.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Use rust-analyzer semantic resolution (accurate cross-crate calls).
    /// Requires a `--features semantic` build; otherwise falls back to fast
    /// mode (which cannot see cross-crate edges).
    #[arg(long)]
    semantic: bool,

    /// Fast-mode source language for this run (overrides `livewalk.toml` `[adapter].language`).
    #[arg(long, value_parser = clap::value_parser!(Language))]
    language: Option<Language>,

    /// Always re-analyze. By default a fresh cached graph (`graph.sqlite`,
    /// written after the newest source file changed) is loaded instead.
    #[arg(long)]
    no_cache: bool,
}

#[derive(Debug, Args)]
struct CallersArgs {
    /// Symbol whose callers to list (substring of a qualified name).
    symbol: String,

    #[command(flatten)]
    repo: RepoArgs,

    /// Levels of callers to expand (1 = direct callers only).
    #[arg(long, default_value_t = 1)]
    depth: u32,

    /// Max callers shown per node.
    #[arg(long, default_value_t = 50)]
    width: usize,

    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ImpactArgs {
    /// Symbol about to change (substring of a qualified name).
    symbol: String,

    #[command(flatten)]
    repo: RepoArgs,

    /// How many caller hops to follow.
    #[arg(long, default_value_t = 3)]
    depth: u32,

    /// Max callers / entry points listed in the text view.
    #[arg(long, default_value_t = 50)]
    limit: usize,

    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SpanArgs {
    /// Symbol to locate (substring of a qualified name).
    symbol: String,

    #[command(flatten)]
    repo: RepoArgs,

    /// Print the definition's source lines instead of the range (with
    /// `--json`, add them as `code`).
    #[arg(long)]
    code: bool,

    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    /// Name or part of one.
    query: String,

    #[command(flatten)]
    repo: RepoArgs,

    /// Max matches.
    #[arg(long, default_value_t = 20)]
    limit: usize,

    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ExplainArgs {
    /// Symbol to explain: a substring of a qualified name (e.g. `Engine::analyze`).
    symbol: String,

    #[command(flatten)]
    repo: RepoArgs,

    /// Max callers/callees to list on each side.
    #[arg(long, default_value_t = 20)]
    limit: usize,

    /// Emit machine-readable JSON (a compact "node card" for tools/LLMs)
    /// instead of the text view.
    #[arg(long)]
    json: bool,

    /// List external / unresolved callees (`Ok`, std methods...) too;
    /// by default they are only counted.
    #[arg(long)]
    external: bool,
}

#[derive(Debug, Args)]
struct FlowArgs {
    /// Target symbol: where the flow should end (substring of a qualified name).
    to: String,

    /// Entry point where the flow starts (substring of a qualified name).
    #[arg(long, default_value = "main")]
    from: String,

    #[command(flatten)]
    repo: RepoArgs,

    /// Max alternate shortest paths to report.
    #[arg(long, default_value_t = 6)]
    max_paths: usize,

    /// Max search depth in hops.
    #[arg(long, default_value_t = 24)]
    max_depth: u32,

    /// Emit the traced path(s) as JSON (a scoped context slice for tools/LLMs)
    /// instead of the text tree.
    #[arg(long)]
    json: bool,

    /// Include each hop's source code in the JSON output (implies --json).
    #[arg(long)]
    snippets: bool,

    /// Also render the flow to this PNG (requires a `--features viewer` build).
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// PNG width in pixels.
    #[arg(long, default_value_t = 2000)]
    width: u32,

    /// PNG height in pixels.
    #[arg(long, default_value_t = 1200)]
    height: u32,
}

#[derive(Debug, Args)]
struct OutlineArgs {
    #[command(flatten)]
    repo: RepoArgs,

    /// Collapse scopes at this depth (0 = crates, 1 = top-level modules...)
    /// into a one-line summary with counts.
    #[arg(long)]
    depth: Option<u32>,

    /// Only keep functions whose qualified name contains this text (and the
    /// scopes that lead to them).
    #[arg(long)]
    filter: Option<String>,

    /// Print scopes only (the architecture at a glance), no functions.
    #[arg(long)]
    scopes_only: bool,

    /// Emit the tree as JSON.
    #[arg(long)]
    json: bool,
}

/// Entry-point kinds selectable on the command line.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum EntryKindArg {
    Main,
    Exported,
    Public,
    Root,
    Test,
}

impl From<EntryKindArg> for EntryKind {
    fn from(k: EntryKindArg) -> Self {
        match k {
            EntryKindArg::Main => EntryKind::Main,
            EntryKindArg::Exported => EntryKind::Exported,
            EntryKindArg::Public => EntryKind::PublicRoot,
            EntryKindArg::Root => EntryKind::Root,
            EntryKindArg::Test => EntryKind::Test,
        }
    }
}

#[derive(Debug, Args)]
struct EntriesArgs {
    #[command(flatten)]
    repo: RepoArgs,

    /// Only list entry points of this kind.
    #[arg(long, value_enum)]
    kind: Option<EntryKindArg>,

    /// Compute, for every entry, how many functions it can reach (`main`s
    /// always get this; it is opt-in for the rest because a library can have
    /// thousands of public roots).
    #[arg(long)]
    reach: bool,

    /// Max entries to print per kind.
    #[arg(long, default_value_t = 40)]
    limit: usize,

    /// Emit the list as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CallsArgs {
    /// Root symbol (substring of a qualified name); `main` is a good start.
    symbol: String,

    #[command(flatten)]
    repo: RepoArgs,

    /// Walk *callers* (who can trigger this) instead of callees.
    #[arg(long)]
    callers: bool,

    /// How many levels to expand below the root.
    #[arg(long, default_value_t = 3)]
    depth: u32,

    /// Max children shown per node.
    #[arg(long, default_value_t = 12)]
    width: usize,

    /// Include external / unresolved targets as leaves.
    #[arg(long)]
    external: bool,

    /// Emit the tree as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Directory to write `livewalk.toml` into (defaults to `.`).
    path: Option<PathBuf>,
    /// Overwrite an existing file.
    #[arg(short, long)]
    force: bool,
}

#[derive(Debug, Args)]
struct GraphArgs {
    /// Path to the repository (defaults to the current directory).
    path: Option<PathBuf>,

    /// Explicit config file. Otherwise `livewalk.toml` is searched for.
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct LoadArgs {
    /// Path to the repository (defaults to the current directory).
    path: Option<PathBuf>,

    /// Explicit config file. Otherwise `livewalk.toml` is searched for.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Open the native graph viewer after loading.
    #[cfg(feature = "viewer")]
    #[arg(long)]
    view: bool,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is::<Reported>() => ExitCode::from(2),
        Err(e) => {
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let log = cli.log.as_deref();
    let out = Out {
        budget: cli.budget.or(cli.compact.then_some(COMPACT_BUDGET)),
        compact_json: !cli.pretty,
    };
    match cli.command {
        Command::Analyze(args) => run_analyze(args, log),
        Command::Explain(args) => run_explain(args, log, out),
        Command::Flow(args) => run_flow(args, log, out),
        Command::Outline(args) => run_outline(args, log, out),
        Command::Entries(args) => run_entries(args, log, out),
        Command::Calls(args) => run_calls(args, log, out),
        Command::Callers(args) => run_callers(args, log, out),
        Command::Impact(args) => run_impact(args, log, out),
        Command::Span(args) => run_span(args, log, out),
        Command::Search(args) => run_search(args, log, out),
        Command::Init(args) => run_init(args),
        Command::Graph(args) => run_graph_info(args),
        Command::Load(args) => run_load(args, log),
        Command::Cache { cmd } => cache_cmd::run(cmd),
        #[cfg(feature = "viewer")]
        Command::View(args) => run_view(args, log),
        #[cfg(feature = "viewer")]
        Command::Shot(args) => run_shot(args, log),
    }
}

/// Resolve the config for `path` (optionally forcing the semantic adapter),
/// initialize telemetry and run the engine. Every navigation command starts
/// here so they all agree on config discovery and fallbacks.
fn analyze_path(
    path: &Path,
    config: Option<&Path>,
    semantic: bool,
    cached: bool,
    language: Option<Language>,
    log_override: Option<&str>,
) -> Result<AnalysisReport> {
    let search_dir = config_search_dir(path);
    let mut config = Config::resolve(config, &search_dir).context("resolving configuration")?;
    if semantic {
        config.adapter.mode = lcw_config::AdapterMode::Semantic;
    }
    if let Some(lang) = language {
        config.adapter.language = lang;
    }
    // Telemetry: CLI stays quiet by default so stdout output isn't polluted.
    lcw_telemetry::init(log_override.unwrap_or("warn"));

    let engine = Engine::new(config);
    if cached {
        eprintln!(
            "note: cached graph may be stale if sources changed since the last `lcw analyze`"
        );
        engine
            .load_from_store(path)
            .with_context(|| format!("loading cached graph for {}", path.display()))
    } else {
        engine
            .analyze(path)
            .with_context(|| format!("analyzing {}", path.display()))
    }
}

/// Load the graph for a navigation command: a fresh cached `graph.sqlite`
/// when there is one (unless `--no-cache`), else a full analysis. Paths are
/// shown relative to the current directory. stdout stays untouched; a stale
/// cache gets one note on stderr.
fn load_repo(repo: &RepoArgs, log_override: Option<&str>) -> Result<Repo> {
    lcw_telemetry::init(log_override.unwrap_or("warn"));
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let opts = LoadOptions {
        config: repo.config.clone(),
        semantic: repo.semantic,
        language: repo.language,
        use_cache: !repo.no_cache,
        paths: PathStyle::RelativeTo(cwd),
    };
    let loaded = lcw_agent::load(&repo.path, &opts)?;
    if let GraphSource::Analyzed(reason) = &loaded.source {
        if !repo.no_cache && *reason != lcw_engine::Staleness::Missing {
            eprintln!("note: re-analyzed ({reason})");
        }
    }
    Ok(loaded)
}

/// Strict symbol lookup. On ambiguity or a miss, print the candidates (text
/// or JSON) to stdout and fail with [`Reported`] (exit code 2).
fn find_symbol(g: &CodeGraph, query: &str, json: bool, out: Out) -> Result<NodeId> {
    let l = lcw_query::lookup(g, query, lcw_agent::MAX_CANDIDATES);
    if let Lookup::Found(id) = l {
        return Ok(id);
    }
    if json {
        if let Some(v) = aj::lookup_error(g, query, &l) {
            out.print_json(&v)?;
        }
    } else {
        print!("{}", nav::format_lookup_error(g, query, &l));
    }
    Err(Reported.into())
}

/// Every match for a flow endpoint (a set); a miss prints fuzzy candidates.
fn find_all(g: &CodeGraph, query: &str, json: bool, out: Out) -> Result<Vec<NodeId>> {
    let hits = lcw_query::resolve(g, query);
    if !hits.is_empty() {
        return Ok(hits);
    }
    let l = Lookup::NotFound(lcw_query::suggest(g, query, lcw_agent::MAX_CANDIDATES));
    if json {
        if let Some(v) = aj::lookup_error(g, query, &l) {
            out.print_json(&v)?;
        }
    } else {
        print!("{}", nav::format_lookup_error(g, query, &l));
    }
    Err(Reported.into())
}

#[cfg(feature = "viewer")]
fn run_shot(args: ShotArgs, log_override: Option<&str>) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let report = analyze_path(
        &path,
        args.config.as_deref(),
        false,
        false,
        None,
        log_override,
    )?;

    match args.view_kind {
        ViewKind::Module => {
            let (scene, _labels) = lcw_render::build_module_view(
                &report.graph,
                &lcw_render::ModuleViewOptions::default(),
            );
            lcw_render::native::render_to_png_scene(&scene, args.width, args.height, &args.output)
                .context("rendering screenshot")?;
        }
        ViewKind::Call | ViewKind::Crates => {
            let g = &report.graph;
            let layout = call_layout(g, args.view_kind);
            let mut scene = lcw_render::scene::build(g, &layout.positions);
            lcw_render::draw_groups(&mut scene, &layout.groups, true);
            let mut hud = Vec::new();
            if let Some(sym) = args.select.as_deref() {
                let hits = lcw_query::resolve(g, sym);
                let Some(&target) = hits.first() else {
                    anyhow::bail!("no internal symbol matches {sym:?}");
                };
                let target_idx = target.0 as usize;

                // Optionally trace a path from an entry point to the target.
                let path: Vec<usize> = match args.flow_from.as_deref() {
                    Some(from) => {
                        let from_hits = lcw_query::resolve(g, from);
                        lcw_query::shortest_paths(g, &from_hits, &hits, 1, 64)
                            .map(|fp| fp.paths[0].iter().map(|id| id.0 as usize).collect())
                            .unwrap_or_default()
                    }
                    None => Vec::new(),
                };

                let anchor = path.first().copied();
                let (nodes, edges) = lcw_render::highlight(&scene, Some(target_idx), anchor, &path);
                scene.nodes = nodes;
                scene.edges = edges;

                let infos = lcw_render::native::build_node_infos(g);
                let panel = lcw_render::native::panel_for(&infos, target_idx, &path);
                hud = lcw_render::hud::build(args.width, args.height, &panel);
                if path.len() > 1 {
                    let chain = path
                        .iter()
                        .map(|&i| g.node(lcw_core::NodeId(i as u32)).name.clone())
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    hud.extend(lcw_render::hud::breadcrumb(
                        args.width,
                        args.height,
                        &format!("flow: {chain}"),
                    ));
                }
            }
            lcw_render::native::render_to_png_scene_with_hud(
                &scene,
                &hud,
                args.width,
                args.height,
                &args.output,
            )
            .context("rendering screenshot")?;
        }
    }
    println!(
        "wrote {} ({}x{}, {:?} view)",
        args.output.display(),
        args.width,
        args.height,
        args.view_kind
    );
    Ok(())
}

#[cfg(feature = "viewer")]
fn run_view(args: AnalyzeArgs, log_override: Option<&str>) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let report = analyze_path(
        &path,
        args.config.as_deref(),
        false,
        args.cached,
        args.language,
        Some(log_override.unwrap_or("info")),
    )?;

    match args.view_kind {
        ViewKind::Module => {
            let (scene, labels) = lcw_render::build_module_view(
                &report.graph,
                &lcw_render::ModuleViewOptions::default(),
            );
            lcw_render::native::run_scene(scene, labels).context("running the viewer")?;
        }
        ViewKind::Call | ViewKind::Crates => {
            let layout = call_layout(&report.graph, args.view_kind);
            lcw_render::native::run(&report.graph, &layout.positions, &layout.groups)
                .context("running the viewer")?;
        }
    }
    Ok(())
}

/// The layout of a function-level view: boxed by crate, or one free force
/// layout over the whole graph.
#[cfg(feature = "viewer")]
fn call_layout(g: &lcw_core::CodeGraph, kind: ViewKind) -> lcw_layout::Layout {
    let params = lcw_layout::LayoutParams::default();
    if kind == ViewKind::Crates {
        lcw_layout::layout_by_crate(g, &params)
    } else {
        lcw_layout::layout(g, &params)
    }
}

fn run_analyze(args: AnalyzeArgs, log_override: Option<&str>) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let search_dir = config_search_dir(&path);
    let cfg =
        Config::resolve(args.config.as_deref(), &search_dir).context("resolving configuration")?;
    let report = analyze_path(
        &path,
        args.config.as_deref(),
        false,
        args.cached,
        args.language,
        log_override,
    )?;

    let rendered = match args.format {
        Format::Summary => export::format_summary(&report, args.top),
        Format::Json => {
            serde_json::to_string_pretty(&report.export()).context("serializing report to JSON")?
        }
        Format::Dot => export::export_dot(&report.graph),
        Format::Graphml => export::export_graphml(&report.graph),
        #[cfg(feature = "viewer")]
        Format::View => {
            let layout =
                lcw_layout::layout_by_crate(&report.graph, &lcw_layout::LayoutParams::default());
            let view = serde_json::json!({
                "report": report.snapshot(),
                "positions": layout.positions,
                "groups": layout.groups,
            });
            serde_json::to_string(&view).context("serializing graph view to JSON")?
        }
    };

    match args.output {
        Some(out) => {
            std::fs::write(&out, rendered).with_context(|| format!("writing {}", out.display()))?;
            eprintln!("wrote {}", out.display());
        }
        None => println!("{rendered}"),
    }
    if args.verbose {
        emit_store_paths(&path, &cfg);
    }
    Ok(())
}

fn run_load(args: LoadArgs, log_override: Option<&str>) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let report = analyze_path(
        &path,
        args.config.as_deref(),
        false,
        true,
        None,
        log_override,
    )?;
    let s = report.summary();
    println!(
        "loaded cached graph: {} nodes, {} edges, {} files",
        s.nodes, s.edges, s.files
    );
    #[cfg(feature = "viewer")]
    if args.view {
        let layout = call_layout(&report.graph, ViewKind::Crates);
        lcw_render::native::run(&report.graph, &layout.positions, &layout.groups)
            .context("running the viewer")?;
    }
    Ok(())
}

fn run_graph_info(args: GraphArgs) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let search_dir = config_search_dir(&path);
    let cfg =
        Config::resolve(args.config.as_deref(), &search_dir).context("resolving configuration")?;
    let db_path = lcw_engine::cache::graph_db_file(&path, &cfg.cache.dir, cfg.cache_variant())
        .context("resolving graph store path")?;
    if !db_path.is_file() {
        anyhow::bail!(
            "no graph store at {} — run `lcw analyze` on this repository first",
            db_path.display()
        );
    }
    let sum = lcw_graph_store::summary(&db_path).context("reading graph store")?;
    println!("graph store: {}", db_path.display());
    println!("  enabled: {}", cfg.graph_store.enabled);
    println!("  nodes:   {}", sum.nodes);
    println!("  edges:   {}", sum.edges);
    println!("  files:   {}", sum.files);
    Ok(())
}

fn emit_store_paths(repo: &Path, cfg: &Config) {
    if let Some(p) = lcw_engine::cache::cache_file(repo, &cfg.cache.dir, cfg.cache_variant()) {
        eprintln!("fragment cache: {}", p.display());
    }
    if cfg.graph_store.enabled {
        if let Some(p) = lcw_engine::cache::graph_db_file(repo, &cfg.cache.dir, cfg.cache_variant())
        {
            eprintln!("graph store:    {}", p.display());
        }
    } else {
        eprintln!("graph store:    (disabled via [graph_store].enabled = false)");
    }
}

fn run_explain(args: ExplainArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;
    let id = find_symbol(g, &args.symbol, args.json, out)?;
    let mut card = lcw_query::node_card(g, id).context("building node card")?;

    if args.json {
        let v = lcw_agent::fit_json(out.budget, |l| {
            aj::explain(g, id, l, args.external).unwrap_or(serde_json::Value::Null)
        });
        out.print_json(&v)?;
    } else {
        if !args.external {
            card.outputs.retain(|c| !c.external);
        }
        print!(
            "{}",
            lcw_agent::fit_text(out.budget, args.limit, |l| nav::format_card(&card, l))
        );
    }
    Ok(())
}

fn run_callers(args: CallersArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;
    let root = find_symbol(g, &args.symbol, args.json, out)?;
    let tree = |width: usize| {
        let opts = CallTreeOptions {
            direction: Direction::Callers,
            max_depth: args.depth,
            max_children: width.max(1),
            include_external: false,
        };
        lcw_query::call_tree(g, root, &opts)
    };
    if args.json {
        let v = lcw_agent::fit_json(out.budget, |l| {
            aj::call_tree(g, &tree(args.width.min(l)), Direction::Callers, args.depth)
        });
        out.print_json(&v)?;
    } else {
        print!(
            "{}",
            lcw_agent::fit_text(out.budget, args.width, |w| {
                nav::format_call_tree(g, &tree(w), Direction::Callers)
            })
        );
    }
    Ok(())
}

fn run_impact(args: ImpactArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;
    let root = find_symbol(g, &args.symbol, args.json, out)?;
    let imp = lcw_query::impact(g, root, args.depth).context("computing impact")?;
    if args.json {
        let v = lcw_agent::fit_json(out.budget, |l| aj::impact(g, &imp, l));
        out.print_json(&v)?;
    } else {
        print!(
            "{}",
            lcw_agent::fit_text(out.budget, args.limit, |l| nav::format_impact(g, &imp, l))
        );
    }
    Ok(())
}

fn run_span(args: SpanArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;
    let id = find_symbol(g, &args.symbol, args.json, out)?;
    let Some(s) = lcw_query::span(g, id) else {
        anyhow::bail!("{} has no source location", g.node(id).qualified_name);
    };
    // Displayed paths are relative to the current directory, so they read as-is.
    let code = if args.code {
        Some(aj::span_code("", &s).with_context(|| format!("reading {}", s.file))?)
    } else {
        None
    };
    if args.json {
        out.print_json(&aj::span(&s, code.as_deref()))?;
    } else if let Some(code) = code {
        println!("{code}");
    } else {
        println!("{}", s.range());
    }
    Ok(())
}

fn run_search(args: SearchArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;
    let v = lcw_agent::fit_json(out.budget, |l| {
        aj::search(g, &args.query, args.limit.min(l))
    });
    if args.json {
        out.print_json(&v)?;
    } else {
        print!("{}", nav::format_search(&v));
    }
    // No real match (only fuzzy suggestions, or nothing): fail like a lookup.
    if v.get("fuzzy").is_some() || v["matches"].as_array().map_or(true, |m| m.is_empty()) {
        return Err(Reported.into());
    }
    Ok(())
}

fn run_flow(args: FlowArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;
    let json = args.json || args.snippets;

    let from = find_all(g, &args.from, json, out)?;
    let to = find_all(g, &args.to, json, out)?;

    let Some(fp) = lcw_query::shortest_paths(g, &from, &to, args.max_paths.max(1), args.max_depth)
    else {
        anyhow::bail!(
            "no call path from {:?} to {:?} (try a different entry, or raise --max-depth)",
            args.from,
            args.to
        );
    };

    if json {
        let v = lcw_agent::fit_json(out.budget, |l| aj::flow(g, &fp, args.snippets, "", l));
        out.print_json(&v)?;
    } else {
        print!(
            "{}",
            lcw_agent::fit_text(out.budget, fp.paths.len(), |l| {
                let mut cut = fp.clone();
                cut.paths.truncate(l);
                flow::format_text(g, &cut)
            })
        );
    }

    if let Some(out) = args.output.as_ref() {
        #[cfg(feature = "viewer")]
        {
            let fg = flow::to_flow_graph(g, &fp);
            let (scene, _picks) =
                lcw_render::build_flow_view(&fg, &lcw_render::FlowOptions::default());
            lcw_render::native::render_to_png_scene(&scene, args.width, args.height, out)
                .context("rendering flow PNG")?;
            println!("\nwrote {} ({}x{})", out.display(), args.width, args.height);
        }
        #[cfg(not(feature = "viewer"))]
        {
            let _ = (out, args.width, args.height);
            anyhow::bail!(
                "--output needs the viewer build: cargo build -p lcw-cli --features viewer"
            );
        }
    }
    Ok(())
}

fn run_outline(args: OutlineArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;
    let mut outline = lcw_query::outline(g);
    if let Some(filter) = args.filter.as_deref() {
        outline = outline.prune(filter);
    }
    if args.json {
        let opts = aj::OutlineOpts {
            max_depth: args.depth,
            scopes_only: args.scopes_only,
        };
        let v = lcw_agent::fit_json(out.budget, |l| aj::outline(g, &outline, &opts, l));
        out.print_json(&v)?;
    } else {
        // Over budget: collapse deeper and deeper scopes until it fits.
        let render = |max_depth| {
            let style = nav::OutlineStyle {
                max_depth,
                scopes_only: args.scopes_only,
            };
            nav::format_outline(&outline, g, &style)
        };
        let mut text = render(args.depth);
        if let Some(b) = out.budget {
            let mut d = args.depth.unwrap_or(4).min(4);
            while lcw_agent::estimate_tokens(&text) > b && d > 0 {
                d -= 1;
                text = render(Some(d));
            }
        }
        print!("{text}");
    }
    Ok(())
}

fn run_entries(args: EntriesArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;

    let mut entries = lcw_query::entry_points(g);
    if let Some(kind) = args.kind {
        let kind: EntryKind = kind.into();
        entries.retain(|e| e.kind == kind);
    }
    // Ranked by kind, then by how much code each entry drives (always
    // computed for `main`s and foreign-ABI exports: on bare-metal or WASM code
    // those are the real starting points), capped per kind.
    let rows = |limit: usize| aj::rank_entries(g, entries.clone(), args.reach, limit);

    if args.json {
        let v = lcw_agent::fit_json(out.budget, |l| {
            let (r, more) = rows(args.limit.min(l));
            aj::entries(g, &r, more)
        });
        out.print_json(&v)?;
    } else {
        print!(
            "{}",
            lcw_agent::fit_text(out.budget, args.limit, |l| {
                let (r, more) = rows(l);
                let (e, reach): (Vec<_>, Vec<_>) = r.into_iter().unzip();
                let mut s = nav::format_entries(g, &e, &reach);
                if more > 0 {
                    s.push_str(&format!("\n... +{more} more\n"));
                }
                s
            })
        );
    }
    Ok(())
}

fn run_calls(args: CallsArgs, log_override: Option<&str>, out: Out) -> Result<()> {
    let repo = load_repo(&args.repo, log_override)?;
    let g = &repo.graph;
    let root = find_symbol(g, &args.symbol, args.json, out)?;
    let direction = if args.callers {
        Direction::Callers
    } else {
        Direction::Callees
    };
    let tree = |width: usize| {
        let opts = CallTreeOptions {
            direction,
            max_depth: args.depth,
            max_children: width.max(1),
            include_external: args.external,
        };
        lcw_query::call_tree(g, root, &opts)
    };

    if args.json {
        let v = lcw_agent::fit_json(out.budget, |l| {
            aj::call_tree(g, &tree(args.width.min(l)), direction, args.depth)
        });
        out.print_json(&v)?;
    } else {
        print!(
            "{}",
            lcw_agent::fit_text(out.budget, args.width, |w| {
                nav::format_call_tree(g, &tree(w), direction)
            })
        );
    }
    Ok(())
}

fn run_init(args: InitArgs) -> Result<()> {
    let dir = args.path.unwrap_or_else(|| PathBuf::from("."));
    let target = dir.join(lcw_config::CONFIG_FILE_NAME);
    if target.exists() && !args.force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            target.display()
        );
    }
    std::fs::write(&target, DEFAULT_CONFIG)
        .with_context(|| format!("writing {}", target.display()))?;
    println!("wrote {}", target.display());
    Ok(())
}

/// Directory from which to search upward for a config file.
fn config_search_dir(path: &Path) -> PathBuf {
    lcw_agent::config_search_dir(path)
}

const DEFAULT_CONFIG: &str = r#"# Live Code Walk & Analysis - project configuration.
# See https://example.com/livewalk for the full reference.

[project]
vertical = "auto"   # auto | embedded | game-engine | backend | full-stack

[adapter]
mode = "fast"       # fast (tree-sitter) | semantic (rust-analyzer)
language = "rust"   # fast-mode language: rust | typescript | python | go
exclude = []
follow_symlinks = false

[lenses]
complexity = true
purity = true
hazards = true
layering = true
paradigm = true
architecture = ["clean"]
extended = false    # opt-in graph-shape lenses: cycles, dead code, god fns, hotspots, unstable deps

[metrics]
cyclomatic_max = 10
max_parameters = 5
max_nesting = 4
max_function_loc = 60
coverage_min = 0.7
heap_sensitivity = 1.0

[suggestions]
scalability = 1.0
maintainability = 1.0
robustness = 1.0
latency = 1.0
performance = 1.0
max_suggestions = 50
extended = false    # opt-in advisors for the extended lenses above

[telemetry]
level = "info"
no_network = true

[cache]
enabled = true     # incremental: re-parse only changed files between runs
dir = ""           # empty = ~/.cache/livewalk/{hash}/ (never inside the repo)

[graph_store]
enabled = true     # persist graph.sqlite next to fragments.json after analyze
"#;
