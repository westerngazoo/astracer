//! `lcw` - the Live Code Walk command-line interface.
//!
//! A standalone front end over `lcw-engine`, driven by `livewalk.toml`
//! (Manifesto: CLI guided by config files).

mod export;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use lcw_config::Config;
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
    /// Write a default `livewalk.toml` to the current directory.
    Init(InitArgs),
    /// Open the interactive graph viewer (native window).
    #[cfg(feature = "viewer")]
    View(AnalyzeArgs),
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
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Format {
    Summary,
    Json,
    Dot,
    Graphml,
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
        Command::Init(args) => run_init(args),
        #[cfg(feature = "viewer")]
        Command::View(args) => run_view(args, cli.log.as_deref()),
    }
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

    let layout = lcw_layout::layout(&report.graph, &lcw_layout::LayoutParams::default());
    lcw_render::native::run(&report.graph, &layout.positions).context("running the viewer")?;
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
exclude = []
follow_symlinks = false

[lenses]
complexity = true
purity = true
hazards = true
layering = true
paradigm = true
architecture = ["clean"]

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

[telemetry]
level = "info"
no_network = true
"#;
