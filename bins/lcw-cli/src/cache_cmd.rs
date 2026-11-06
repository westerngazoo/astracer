//! `lcw cache list` / `lcw cache clean` — inspect and prune on-disk caches.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use lcw_engine::cache::{self, CacheCleanOptions, CacheDirEntry};

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// List cache buckets under the Livewalk cache root.
    List,
    /// List cache sizes under the Livewalk cache root; remove with `--all` or
    /// `--legacy-only` plus `--yes`, or preview with `--dry-run`.
    Clean(CleanArgs),
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Delete every cache bucket under the cache root (pair with `--yes` or
    /// `--dry-run`).
    #[arg(long)]
    all: bool,
    /// Actually delete matching cache directories.
    #[arg(long)]
    yes: bool,
    /// Show what would be deleted without removing anything.
    #[arg(long)]
    dry_run: bool,
    /// Only remove legacy `{16-hex}` dirs (no `-{language}` suffix).
    #[arg(long)]
    legacy_only: bool,
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
    println!("total: {} ({:.2} MiB)", format_bytes(total), bytes_to_mib(total));
    Ok(())
}

fn run_clean(args: CleanArgs) -> Result<()> {
    let root = cache::livewalk_cache_root();
    print_root(root.as_deref());

    let list_only = !args.yes && !args.dry_run;
    if list_only {
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
            "hint: `lcw cache clean --all --dry-run` to preview deletions; \
             `lcw cache clean --all --yes` to delete (or add `--legacy-only`)"
        );
        return Ok(());
    }

    if args.all && args.legacy_only {
        bail!("choose only one of --all and --legacy-only");
    }
    if !args.all && !args.legacy_only {
        bail!("specify --all or --legacy-only (try `lcw cache clean --all --dry-run` first)");
    }
    if args.yes && args.dry_run {
        bail!("use either --yes or --dry-run, not both");
    }

    let opts = CacheCleanOptions {
        yes: args.yes,
        dry_run: args.dry_run,
        all: args.all,
        legacy_only: args.legacy_only,
        keep_repo: args.keep_repo,
        ..CacheCleanOptions::default()
    };
    let report = cache::clean_cache_dirs(&opts).context("cleaning cache directories")?;

    if args.dry_run {
        if report.would_remove.is_empty() {
            println!("would remove: 0 directories");
        } else {
            for path in &report.would_remove {
                println!("would remove: {}", path.display());
            }
        }
    } else if report.removed.is_empty() {
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

    let freed = if args.dry_run {
        report.would_free_bytes
    } else {
        report.bytes_freed
    };
    let label = if args.dry_run {
        "would free"
    } else {
        "freed"
    };
    println!(
        "{label} {:.2} MiB ({} bytes)",
        bytes_to_mib(freed),
        freed
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
