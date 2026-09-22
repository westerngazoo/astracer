//! `lcw` - the Live Code Walk command-line interface.
//!
//! A standalone front end over `lcw-engine`, driven by `livewalk.toml`
//! (Manifesto: CLI guided by config files). The navigation commands
//! (`explain`, `flow`, `outline`, `entries`, `calls`) are thin text renderers
//! over `lcw-query`, the same traversal code the viewers use.

mod export;
mod flow;
mod nav;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use lcw_config::Config;
use lcw_core::AnalysisReport;
use lcw_engine::Engine;
use lcw_query::{CallTreeOptions, Direction, EntryKind};

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
    /// Print the code hierarchy (crate ▸ module ▸ type ▸ function) as a tree.
    Outline(OutlineArgs),
    /// List entry points: `main`, uncalled public API, private roots, tests.
    Entries(EntriesArgs),
    /// Print the call tree below (or above) a symbol: what it triggers.
    Calls(CallsArgs),
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
    /// The laid-out graph view the desktop frontend consumes: the report
    /// snapshot plus one `[x, y]` position per node. Serve it as
    /// `fixture.json` next to the built frontend to run the UI in a plain
    /// browser (browser dev mode) without the Tauri shell.
    #[cfg(feature = "viewer")]
    View,
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
    Public,
    Root,
    Test,
}

impl From<EntryKindArg> for EntryKind {
    fn from(k: EntryKindArg) -> Self {
        match k {
            EntryKindArg::Main => EntryKind::Main,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let log = cli.log.as_deref();
    match cli.command {
        Command::Analyze(args) => run_analyze(args, log),
        Command::Explain(args) => run_explain(args, log),
        Command::Flow(args) => run_flow(args, log),
        Command::Outline(args) => run_outline(args, log),
        Command::Entries(args) => run_entries(args, log),
        Command::Calls(args) => run_calls(args, log),
        Command::Init(args) => run_init(args),
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
    log_override: Option<&str>,
) -> Result<AnalysisReport> {
    let search_dir = config_search_dir(path);
    let mut config = Config::resolve(config, &search_dir).context("resolving configuration")?;
    if semantic {
        config.adapter.mode = lcw_config::AdapterMode::Semantic;
    }
    // Telemetry: CLI stays quiet by default so stdout output isn't polluted.
    lcw_telemetry::init(log_override.unwrap_or("warn"));

    Engine::new(config)
        .analyze(path)
        .with_context(|| format!("analyzing {}", path.display()))
}

fn analyze_repo(repo: &RepoArgs, log_override: Option<&str>) -> Result<AnalysisReport> {
    analyze_path(
        &repo.path,
        repo.config.as_deref(),
        repo.semantic,
        log_override,
    )
}

#[cfg(feature = "viewer")]
fn run_shot(args: ShotArgs, log_override: Option<&str>) -> Result<()> {
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));
    let report = analyze_path(&path, args.config.as_deref(), false, log_override)?;

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
    let report = analyze_path(&path, args.config.as_deref(), false, log_override)?;

    let rendered = match args.format {
        Format::Summary => export::format_summary(&report, args.top),
        Format::Json => {
            serde_json::to_string_pretty(&report.export()).context("serializing report to JSON")?
        }
        Format::Dot => export::export_dot(&report.graph),
        Format::Graphml => export::export_graphml(&report.graph),
        #[cfg(feature = "viewer")]
        Format::View => {
            let layout = lcw_layout::layout(&report.graph, &lcw_layout::LayoutParams::default());
            let view = serde_json::json!({
                "report": report.snapshot(),
                "positions": layout.positions,
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
    Ok(())
}

fn run_explain(args: ExplainArgs, log_override: Option<&str>) -> Result<()> {
    let report = analyze_repo(&args.repo, log_override)?;
    let g = &report.graph;

    let hits = lcw_query::resolve(g, &args.symbol);
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
    let card = lcw_query::node_card(g, id).context("building node card")?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&card).context("serializing explain card")?
        );
    } else {
        print!("{}", nav::format_card(&card, args.limit));
    }
    Ok(())
}

fn run_flow(args: FlowArgs, log_override: Option<&str>) -> Result<()> {
    let report = analyze_repo(&args.repo, log_override)?;
    let g = &report.graph;

    let from = lcw_query::resolve(g, &args.from);
    let to = lcw_query::resolve(g, &args.to);
    if from.is_empty() {
        anyhow::bail!("no entry symbol matches {:?}", args.from);
    }
    if to.is_empty() {
        anyhow::bail!("no target symbol matches {:?}", args.to);
    }

    let Some(fp) = lcw_query::shortest_paths(g, &from, &to, args.max_paths.max(1), args.max_depth)
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

fn run_outline(args: OutlineArgs, log_override: Option<&str>) -> Result<()> {
    let report = analyze_repo(&args.repo, log_override)?;
    let g = &report.graph;
    let mut outline = lcw_query::outline(g);
    if let Some(filter) = args.filter.as_deref() {
        outline = outline.prune(filter);
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outline).context("serializing outline")?
        );
    } else {
        let style = nav::OutlineStyle {
            max_depth: args.depth,
            scopes_only: args.scopes_only,
        };
        print!("{}", nav::format_outline(&outline, g, &style));
    }
    Ok(())
}

fn run_entries(args: EntriesArgs, log_override: Option<&str>) -> Result<()> {
    let report = analyze_repo(&args.repo, log_override)?;
    let g = &report.graph;

    let mut entries = lcw_query::entry_points(g);
    if let Some(kind) = args.kind {
        let kind: EntryKind = kind.into();
        entries.retain(|e| e.kind == kind);
    }
    // Cap per kind so a library with thousands of public roots stays readable.
    let mut per_kind = std::collections::HashMap::new();
    entries.retain(|e| {
        let n = per_kind.entry(e.kind).or_insert(0usize);
        *n += 1;
        *n <= args.limit
    });

    let reach: Vec<Option<usize>> = entries
        .iter()
        .map(|e| {
            (args.reach || e.kind == EntryKind::Main)
                .then(|| lcw_query::reach_count(g, e.id, Direction::Callees))
        })
        .collect();

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&nav::entries_json(g, &entries, &reach))
                .context("serializing entries")?
        );
    } else {
        print!("{}", nav::format_entries(g, &entries, &reach));
    }
    Ok(())
}

fn run_calls(args: CallsArgs, log_override: Option<&str>) -> Result<()> {
    let report = analyze_repo(&args.repo, log_override)?;
    let g = &report.graph;

    let Some(root) = lcw_query::best_match(g, &args.symbol) else {
        anyhow::bail!("no internal symbol matches {:?}", args.symbol);
    };
    let direction = if args.callers {
        Direction::Callers
    } else {
        Direction::Callees
    };
    let opts = CallTreeOptions {
        direction,
        max_depth: args.depth,
        max_children: args.width.max(1),
        include_external: args.external,
    };
    let tree = lcw_query::call_tree(g, root, &opts);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&nav::call_tree_json(g, &tree))
                .context("serializing call tree")?
        );
    } else {
        print!("{}", nav::format_call_tree(g, &tree, direction));
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
