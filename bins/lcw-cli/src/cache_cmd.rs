//! `lcw cache list` / `lcw cache clean` — inspect and prune on-disk caches.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use lcw_engine::cache::{self, CacheCleanOptions, CacheDirEntry};

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// List cache buckets under the Livewalk cache root with on-disk sizes.
    List,
    /// Remove cache buckets. Deletion requires `--yes` (see `--legacy-only`,
    /// `--older-than`).
    Clean(CleanArgs),
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Actually delete matching cache directories (required for any removal).
    #[arg(long)]
    yes: bool,
    /// Only remove legacy `{16-hex}` dirs (no `-{language}` suffix).
    #[arg(long)]
    legacy_only: bool,
    /// Only remove buckets whose newest mtime is older than this many days.
    #[arg(long = "older-than", value_name = "DAYS")]
    older_than: Option<u64>,
    /// Never delete buckets for this repository root.
    #[arg(long)]
    keep_repo: Option<PathBuf>,
}

pub fn run(cmd: CacheCommand) -> Result<()> {
    match cmd {
        CacheCommand::List => run_list(),
        CacheCommand::Clean(args) => run_clean(args),
    }
}

fn run_list() -> Result<()> {
    let root = cache::livewalk_cache_root();
    print_root(root.as_deref());
    let report = cache::clean_cache_dirs(&CacheCleanOptions::default())
        .context("listing cache directories")?;
    if report.listed.is_empty() {
        println!("(no cache buckets)");
        return Ok(());
    }
    for entry in &report.listed {
        print_entry(entry);
    }
    let total: u64 = report.listed.iter().map(|e| e.size_bytes).sum();
    println!(
        "total: {} ({:.2} MiB)",
        format_bytes(total),
        bytes_to_mib(total)
    );
    Ok(())
}

fn run_clean(args: CleanArgs) -> Result<()> {
    let root = cache::livewalk_cache_root();
    print_root(root.as_deref());

    if !args.yes {
        let report = cache::clean_cache_dirs(&CacheCleanOptions::default())
            .context("listing cache directories")?;
        if report.listed.is_empty() {
            println!("(no cache buckets)");
        } else {
            for entry in &report.listed {
                print_entry(entry);
            }
            let total: u64 = report.listed.iter().map(|e| e.size_bytes).sum();
            println!(
                "total: {} ({:.2} MiB)",
                format_bytes(total),
                bytes_to_mib(total)
            );
        }
        println!(
            "hint: pass `--yes` to delete (e.g. `lcw cache clean --yes`, \
             `lcw cache clean --yes --legacy-only`, \
             `lcw cache clean --yes --older-than 30`)"
        );
        return Ok(());
    }

    if args.legacy_only && args.older_than.is_some() {
        bail!("choose only one of --legacy-only and --older-than");
    }

    let opts = CacheCleanOptions {
        yes: true,
        legacy_only: args.legacy_only,
        older_than_days: args.older_than,
        keep_repo: args.keep_repo,
        ..CacheCleanOptions::default()
    };
    let report = cache::clean_cache_dirs(&opts).context("cleaning cache directories")?;

    if report.removed.is_empty() {
        println!("removed: 0 directories");
    } else {
        for path in &report.removed {
            println!("removed: {}", path.display());
        }
    }

    if !report.skipped_keep.is_empty() {
        for path in &report.skipped_keep {
            eprintln!("kept (--keep-repo): {}", path.display());
        }
    }

    println!(
        "freed {:.2} MiB ({} bytes)",
        bytes_to_mib(report.bytes_freed),
        report.bytes_freed
    );
    Ok(())
}

fn print_root(root: Option<&std::path::Path>) {
    match root {
        Some(p) => println!("cache root: {}", p.display()),
        None => println!("cache root: (unknown — set HOME or LIVEWALK_CACHE_HOME)"),
    }
}

fn print_entry(entry: &CacheDirEntry) {
    let legacy = if entry.legacy_superseded {
        " legacy-superseded"
    } else if cache::is_legacy_cache_dir_name(&entry.name) {
        " legacy"
    } else {
        ""
    };
    println!(
        "  {:>8}  {:.2} MiB  {}{}",
        format_bytes(entry.size_bytes),
        bytes_to_mib(entry.size_bytes),
        entry.name,
        legacy
    );
}

fn format_bytes(n: u64) -> String {
    if n >= 1_073_741_824 {
        format!("{:.1} GiB", n as f64 / 1_073_741_824.0)
    } else if n >= 1_048_576 {
        format!("{:.1} MiB", n as f64 / 1_048_576.0)
    } else if n >= 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn bytes_to_mib(n: u64) -> f64 {
    n as f64 / (1024.0 * 1024.0)
}
