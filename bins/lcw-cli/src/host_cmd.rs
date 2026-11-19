//! Host data commands — `lcw view-data`, `lcw snippet`, `lcw languages`,
//! `lcw explain-prompt`: the desktop UI's `EngineTransport` answers, served
//! over stdout so a non-Tauri host (the VS Code extension) hands the same
//! webview the same JSON the Tauri shell does. Each is a thin wrapper over
//! `lcw_desktop_api::host`, which the Tauri commands call too.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::Args;
use lcw_config::Language;
use lcw_desktop_api::{host, AnalyzeArgs, ExplainContext, ReadSourceSnippetArgs};

#[derive(Debug, Args)]
pub struct ViewDataArgs {
    /// Path to the repository (defaults to the current directory).
    path: Option<PathBuf>,

    /// Print the laid-out `GraphView` JSON (report snapshot, one `[x, y]`
    /// per node, crate boxes, repo root). Without it, a one-line summary.
    #[arg(long)]
    json: bool,

    /// Load `graph.sqlite` instead of parsing (falls back to a full analysis
    /// when there is no saved graph). May be stale if sources changed.
    #[arg(long)]
    cached: bool,

    /// Use rust-analyzer semantic resolution (needs a `--features semantic`
    /// build; ignored with `--cached`).
    #[arg(long)]
    semantic: bool,

    /// Fast-mode source language (overrides `livewalk.toml` `[adapter].language`).
    #[arg(long, value_parser = clap::value_parser!(Language))]
    language: Option<Language>,

    /// Stream progress to stderr as JSON lines (`{"phase", "message"}`).
    #[arg(long)]
    progress: bool,
}

#[derive(Debug, Args)]
pub struct SnippetArgs {
    /// Source file: absolute, or relative to `--repo-root`.
    file: String,
    /// First line of the span (1-based).
    start: u32,
    /// Last line of the span (1-based, inclusive).
    end: u32,

    /// Root that resolves a relative `file`.
    #[arg(long, default_value = "")]
    repo_root: String,

    /// Return the whole file (capped at 500 lines) instead of the span.
    #[arg(long)]
    whole_file: bool,

    /// Emit the `SourceSnippet` as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct LanguagesArgs {
    /// Path to the repository (defaults to the current directory).
    path: Option<PathBuf>,

    /// Emit `[{"id", "label"}]` JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub struct ExplainPromptArgs {
    /// Read the `ExplainContext` JSON from this file instead of stdin.
    #[arg(long)]
    context: Option<PathBuf>,

    /// Emit `{"schema", "system", "prompt"}` JSON instead of the prompt text.
    #[arg(long)]
    json: bool,
}

fn path_string(path: Option<PathBuf>) -> String {
    path.unwrap_or_else(|| PathBuf::from("."))
        .to_string_lossy()
        .into_owned()
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    let s = serde_json::to_string(v).context("serializing JSON")?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{s}").context("writing stdout")?;
    Ok(())
}

pub fn run_view_data(args: ViewDataArgs, log_override: Option<&str>) -> Result<()> {
    lcw_telemetry::init(log_override.unwrap_or("warn"));
    let req = AnalyzeArgs {
        path: path_string(args.path),
        semantic: args.semantic,
        use_cache: args.cached,
        language: args.language.map(|l| l.cache_suffix().to_string()),
    };
    let stream = args.progress;
    let view = host::analyze_view(&req, &mut |p| {
        if stream {
            if let Ok(line) = serde_json::to_string(&p) {
                eprintln!("{line}");
            }
        }
    })
    .map_err(|e| anyhow!(e))?;
    if args.json {
        return print_json(&view);
    }
    println!(
        "{} nodes, {} edges, {} crate boxes ({})",
        view.report.graph.nodes.len(),
        view.report.graph.edges.len(),
        view.groups.len(),
        view.repo_root
    );
    Ok(())
}

pub fn run_snippet(args: SnippetArgs) -> Result<()> {
    let snip = host::read_source_snippet(&ReadSourceSnippetArgs {
        file: args.file,
        start_line: args.start,
        end_line: args.end,
        repo_root: args.repo_root,
        whole_file: args.whole_file,
    })
    .map_err(|e| anyhow!(e))?;
    if args.json {
        return print_json(&snip);
    }
    for line in &snip.lines {
        println!("{:>5}  {}", line.number, line.text);
    }
    Ok(())
}

pub fn run_languages(args: LanguagesArgs) -> Result<()> {
    let langs = host::discover_language_choices(&path_string(args.path)).map_err(|e| anyhow!(e))?;
    if args.json {
        return print_json(&langs);
    }
    for l in &langs {
        println!("{}\t{}", l.id, l.label);
    }
    Ok(())
}

pub fn run_explain_prompt(args: ExplainPromptArgs) -> Result<()> {
    let raw = match &args.context {
        Some(p) => {
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
        }
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("reading stdin")?;
            s
        }
    };
    let ctx: ExplainContext = serde_json::from_str(&raw).context("parsing ExplainContext JSON")?;
    let prompt = lcw_desktop_api::explain_prompt(&ctx);
    if args.json {
        return print_json(&serde_json::json!({
            "schema": lcw_agent::SCHEMA,
            "system": lcw_desktop_api::EXPLAIN_SYSTEM_PROMPT,
            "prompt": prompt,
        }));
    }
    print!("{prompt}");
    Ok(())
}
