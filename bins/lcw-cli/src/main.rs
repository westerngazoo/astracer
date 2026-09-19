//! `lcw` - the Live Code Walk command-line interface.
//!
//! A standalone front end over `lcw-engine`, driven by `livewalk.toml`
//! (Manifesto: CLI guided by config files).

mod export;
mod flow;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use lcw_config::Config;
use lcw_core::{Node, NodeId, NodeKind};
use lcw_engine::Engine;

/// Live Code Walk & Analysis.
#[derive(Debug, Parser)]
#[command(name = "lcw", version, about = "Live Code Walk & Analysis", long_about = None)]
struct Cli {
    /// Log directive (e.g. `info`, `lcw_engine=debug`). Overrides config.
    #[arg(long, global = true)]
    log: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Analyze a repository and emit a report or graph.
    Analyze(AnalyzeArgs),
    /// Explain one symbol: its metrics, callers (inputs) and callees (outputs).
    Explain(ExplainArgs),
    /// Trace the shortest call path(s) from an entry point to a target symbol.
    Flow(FlowArgs),
    /// Write a default `livewalk.toml` to the current directory.
    Init(InitArgs),
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

    /// Which graph the viewer renders: the full call graph, or the aggregated
    /// module view (crates as boxed clusters). Only affects `view`.
    #[cfg(feature = "viewer")]
    #[arg(long = "view", value_enum, default_value_t = ViewKind::Call)]
    view_kind: ViewKind,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Format {
    Summary,
    Json,
    Dot,
    Graphml,
}

/// Which graph a visual command renders.
#[cfg(feature = "viewer")]
#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq)]
enum ViewKind {
    /// The full function-level call graph.
    #[default]
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

    /// Which graph to render: the full call graph, or the aggregated module
    /// view (crates as boxed clusters).
    #[arg(long = "view", value_enum, default_value_t = ViewKind::Call)]
    view_kind: ViewKind,

    /// Highlight one symbol and overlay its detail panel (inputs/outputs).
    /// Only applies to the call view.
    #[arg(long)]
    select: Option<String>,

    /// With `--select` as the target, trace and highlight the shortest call
    /// path from this entry point (adds a breadcrumb). Call view only.
    #[arg(long)]
    flow_from: Option<String>,
}

#[derive(Debug, Args)]
struct ExplainArgs {
    /// Symbol to explain: a substring of a qualified name (e.g. `Engine::analyze`).
    symbol: String,

    /// Repository path.
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Explicit config file. Otherwise `livewalk.toml` is searched for.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Max callers/callees to list on each side.
    #[arg(long, default_value_t = 20)]
    limit: usize,

    /// Emit machine-readable JSON (a compact "node card" for tools/LLMs)
    /// instead of the text view.
    #[arg(long)]
    json: bool,

    /// Use rust-analyzer semantic resolution (accurate cross-crate calls).
    /// Requires a `--features semantic` build; otherwise falls back to fast mode.
    #[arg(long)]
    semantic: bool,
}

#[derive(Debug, Args)]
struct FlowArgs {
    /// Target symbol: where the flow should end (substring of a qualified name).
    to: String,

    /// Entry point where the flow starts (substring of a qualified name).
    #[arg(long, default_value = "main")]
    from: String,

    /// Repository path.
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Explicit config file. Otherwise `livewalk.toml` is searched for.
    #[arg(short, long)]
    config: Option<PathBuf>,

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

    /// Use rust-analyzer semantic resolution so cross-crate/cross-layer calls
    /// resolve. Requires a `--features semantic` build; otherwise falls back to
    /// fast mode (which cannot see cross-crate edges).
    #[arg(long)]
    semantic: bool,

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
struct InitArgs {
    /// Directory to write `livewalk.toml` into (defaults to `.`).
    path: Option<PathBuf>,
    /// Overwrite an existing file.
    #[arg(short, long)]
    force: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Analyze(args) => run_analyze(args, cli.log.as_deref()),
        Command::Explain(args) => run_explain(args, cli.log.as_deref()),
        Command::Flow(args) => run_flow(args, cli.log.as_deref()),
        Command::Init(args) => run_init(args),
        #[cfg(feature = "viewer")]
        Command::View(args) => run_view(args, cli.log.as_deref()),
        #[cfg(feature = "viewer")]
        Command::Shot(args) => run_shot(args, cli.log.as_deref()),
    }
}

#[cfg(feature = "viewer")]
fn run_shot(args: ShotArgs, log_override: Option<&str>) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let search_dir = config_search_dir(&path);
    let config =
        Config::resolve(args.config.as_deref(), &search_dir).context("resolving configuration")?;
    lcw_telemetry::init(log_override.unwrap_or("warn"));

    let engine = Engine::new(config);
    let report = engine
        .analyze(&path)
        .with_context(|| format!("analyzing {}", path.display()))?;

    match args.view_kind {
        ViewKind::Module => {
            let (scene, _labels) = lcw_render::build_module_view(
                &report.graph,
                &lcw_render::ModuleViewOptions::default(),
            );
            lcw_render::native::render_to_png_scene(&scene, args.width, args.height, &args.output)
                .context("rendering screenshot")?;
        }
        ViewKind::Call => {
            let g = &report.graph;
            let layout = lcw_layout::layout(g, &lcw_layout::LayoutParams::default());
            let mut scene = lcw_render::scene::build(g, &layout.positions);
            let mut hud = Vec::new();
            if let Some(sym) = args.select.as_deref() {
                let hits = flow::resolve(g, sym);
                let Some(&target) = hits.first() else {
                    anyhow::bail!("no internal symbol matches {sym:?}");
                };
                let target_idx = target.0 as usize;

                // Optionally trace a path from an entry point to the target.
                let path: Vec<usize> = match args.flow_from.as_deref() {
                    Some(from) => {
                        let from_hits = flow::resolve(g, from);
                        flow::shortest_paths(g, &from_hits, &hits, 1, 64)
                            .map(|fp| fp.paths[0].iter().map(|id| id.0 as usize).collect())
                            .unwrap_or_default()
                    }
                    None => Vec::new(),
                };

                let anchor = path.first().copied();
                let (nodes, edges) =
                    lcw_render::native::highlight(&scene, Some(target_idx), anchor, &path);
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
    let search_dir = config_search_dir(&path);
    let config =
        Config::resolve(args.config.as_deref(), &search_dir).context("resolving configuration")?;
    lcw_telemetry::init(log_override.unwrap_or("info"));

    let engine = Engine::new(config);
    let report = engine
        .analyze(&path)
        .with_context(|| format!("analyzing {}", path.display()))?;

    match args.view_kind {
        ViewKind::Module => {
            let (scene, labels) = lcw_render::build_module_view(
                &report.graph,
                &lcw_render::ModuleViewOptions::default(),
            );
            lcw_render::native::run_scene(scene, labels).context("running the viewer")?;
        }
        ViewKind::Call => {
            let layout = lcw_layout::layout(&report.graph, &lcw_layout::LayoutParams::default());
            lcw_render::native::run(&report.graph, &layout.positions)
                .context("running the viewer")?;
        }
    }
    Ok(())
}

fn run_analyze(args: AnalyzeArgs, log_override: Option<&str>) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let search_dir = config_search_dir(&path);

    let config =
        Config::resolve(args.config.as_deref(), &search_dir).context("resolving configuration")?;

    // Telemetry: CLI stays quiet by default so stdout output isn't polluted.
    let level = log_override.unwrap_or("warn");
    lcw_telemetry::init(level);

    let engine = Engine::new(config);
    let report = engine
        .analyze(&path)
        .with_context(|| format!("analyzing {}", path.display()))?;

    let rendered = match args.format {
        Format::Summary => export::format_summary(&report, args.top),
        Format::Json => {
            serde_json::to_string_pretty(&report.export()).context("serializing report to JSON")?
        }
        Format::Dot => export::export_dot(&report.graph),
        Format::Graphml => export::export_graphml(&report.graph),
    };

    match args.output {
        Some(out) => {
            std::fs::write(&out, rendered).with_context(|| format!("writing {}", out.display()))?;
            eprintln!("wrote {}", out.display());
        }
        None => println!("{rendered}"),
    }
    Ok(())
}

fn kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Closure => "closure",
        NodeKind::External => "external",
    }
}

fn flags_vec(n: &Node) -> Vec<&'static str> {
    let mut v = Vec::new();
    if n.flags.is_pub {
        v.push("pub");
    }
    if n.flags.is_async {
        v.push("async");
    }
    if n.flags.is_unsafe {
        v.push("unsafe");
    }
    if n.flags.is_test {
        v.push("test");
    }
    if n.flags.is_generic {
        v.push("generic");
    }
    v
}

fn flags_str(n: &Node) -> String {
    flags_vec(n).join(" ")
}

fn node_file(g: &lcw_core::CodeGraph, n: &Node) -> String {
    g.file_path(n.span.file())
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn run_explain(args: ExplainArgs, log_override: Option<&str>) -> Result<()> {
    let search_dir = config_search_dir(&args.path);
    let mut config =
        Config::resolve(args.config.as_deref(), &search_dir).context("resolving configuration")?;
    if args.semantic {
        config.adapter.mode = lcw_config::AdapterMode::Semantic;
    }
    lcw_telemetry::init(log_override.unwrap_or("warn"));

    let engine = Engine::new(config);
    let report = engine
        .analyze(&args.path)
        .with_context(|| format!("analyzing {}", args.path.display()))?;
    let g = &report.graph;

    let hits = flow::resolve(g, &args.symbol);
    let Some(&id) = hits.first() else {
        anyhow::bail!("no internal symbol matches {:?}", args.symbol);
    };
    if hits.len() > 1 && !args.json {
        eprintln!(
            "note: {} symbols match {:?}; showing the closest",
            hits.len(),
            args.symbol
        );
    }

    let n = g.node(id);

    let mut callers: Vec<NodeId> = g.neighbors_in(id).collect();
    callers.sort_by(|&a, &b| g.node(a).qualified_name.cmp(&g.node(b).qualified_name));
    let mut callees: Vec<NodeId> = g.neighbors_out(id).collect();
    callees.sort_by(|&a, &b| g.node(a).qualified_name.cmp(&g.node(b).qualified_name));

    if args.json {
        let card = serde_json::json!({
            "qualified_name": n.qualified_name,
            "name": n.name,
            "kind": kind_str(n.kind),
            "file": node_file(g, n),
            "line": n.span.start_line,
            "flags": flags_vec(n),
            "metrics": {
                "cyclomatic": n.cyclomatic_complexity(),
                "parameters": n.stats.parameters,
                "lines_of_code": n.stats.lines_of_code,
                "max_nesting": n.stats.max_nesting,
                "decision_points": n.stats.decision_points,
            },
            "inputs": callers.iter().map(|&c| {
                let cn = g.node(c);
                serde_json::json!({ "qualified_name": cn.qualified_name, "name": cn.name })
            }).collect::<Vec<_>>(),
            "outputs": callees.iter().map(|&c| {
                let cn = g.node(c);
                serde_json::json!({
                    "qualified_name": cn.qualified_name,
                    "name": cn.name,
                    "external": cn.kind == NodeKind::External,
                })
            }).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&card).context("serializing explain card")?
        );
        return Ok(());
    }

    println!("{}", n.qualified_name);
    println!("  kind:    {}", kind_str(n.kind));
    println!("  where:   {}", flow::location(g, n));
    println!(
        "  metrics: cc {}  params {}  loc {}  nesting {}",
        n.cyclomatic_complexity(),
        n.stats.parameters,
        n.stats.lines_of_code,
        n.stats.max_nesting
    );
    let flags = flags_str(n);
    if !flags.is_empty() {
        println!("  flags:   {flags}");
    }

    println!("\n  inputs — {} caller(s):", callers.len());
    for &c in callers.iter().take(args.limit) {
        println!("    <- {}", g.node(c).qualified_name);
    }
    if callers.len() > args.limit {
        println!("    ... {} more", callers.len() - args.limit);
    }

    println!("\n  outputs — {} callee(s):", callees.len());
    for &c in callees.iter().take(args.limit) {
        let cn = g.node(c);
        let tag = if cn.kind == NodeKind::External {
            "  (external)"
        } else {
            ""
        };
        println!("    -> {}{}", cn.qualified_name, tag);
    }
    if callees.len() > args.limit {
        println!("    ... {} more", callees.len() - args.limit);
    }
    Ok(())
}

fn run_flow(args: FlowArgs, log_override: Option<&str>) -> Result<()> {
    let search_dir = config_search_dir(&args.path);
    let mut config =
        Config::resolve(args.config.as_deref(), &search_dir).context("resolving configuration")?;
    if args.semantic {
        config.adapter.mode = lcw_config::AdapterMode::Semantic;
    }
    lcw_telemetry::init(log_override.unwrap_or("warn"));

    let engine = Engine::new(config);
    let report = engine
        .analyze(&args.path)
        .with_context(|| format!("analyzing {}", args.path.display()))?;
    let g = &report.graph;

    let from = flow::resolve(g, &args.from);
    let to = flow::resolve(g, &args.to);
    if from.is_empty() {
        anyhow::bail!("no entry symbol matches {:?}", args.from);
    }
    if to.is_empty() {
        anyhow::bail!("no target symbol matches {:?}", args.to);
    }

    let Some(fp) = flow::shortest_paths(g, &from, &to, args.max_paths.max(1), args.max_depth)
    else {
        anyhow::bail!(
            "no call path from {:?} to {:?} (try a different entry, or raise --max-depth)",
            args.from,
            args.to
        );
    };

    if args.json || args.snippets {
        let value = flow::to_json(g, &fp, args.snippets);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).context("serializing flow to JSON")?
        );
    } else {
        print!("{}", flow::format_text(g, &fp));
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
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
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
dir = ""           # empty = per-repo dir under the user cache home
"#;
