//! Cross-platform developer task runner for Live Code Walk.
//!
//! This replaces the POSIX shell script that used to drive browser dev mode.
//! The analyzer crates are portable — every adapter derives module paths from
//! `Path::components()`, so a backslash never reaches a qualified name — but
//! the *way in* was not: a `sh` script needs Git Bash or WSL on Windows, and it
//! reached for `python3 -m http.server`, which is `python` or `py` there. The
//! tooling was the only thing keeping this repository off a platform its own
//! code already supports.
//!
//! So the runner is a dependency-free Rust binary instead: no shell, no Python,
//! one implementation for macOS, Linux and Windows.
//!
//! ```text
//! cargo xtask doctor [--mode browser|native|all]
//! cargo xtask ui   [REPO] [--port N] [--no-open]
//! cargo xtask view [REPO] [--module]
//! ```

mod doctor;
mod serve;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use doctor::{report, Mode};

const USAGE: &str = "\
Live Code Walk developer tasks.

USAGE:
    cargo xtask <COMMAND> [OPTIONS]

COMMANDS:
    doctor          Check the environment and print what is missing
    ui [REPO]       Browser dev mode: build the UI, analyze REPO, serve it
    view [REPO]     Native viewer window (wgpu)

OPTIONS:
    --mode <M>      doctor only: browser | native | all   (default: all)
    --port <N>      ui only: listen port                  (default: 8765)
    --no-open       ui only: do not open a browser
    --module        view only: module view, not the call graph
    -h, --help      Print this message

REPO defaults to this repository, so `cargo xtask ui` analyzes Live Code Walk
itself.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\nerror: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let Some(cmd) = args.first() else {
        print!("{USAGE}");
        return Err("no command given".into());
    };
    if cmd == "-h" || cmd == "--help" || cmd == "help" {
        print!("{USAGE}");
        return Ok(());
    }

    let opts = Options::parse(&args[1..])?;
    match cmd.as_str() {
        "doctor" => cmd_doctor(&opts),
        "ui" => cmd_ui(&opts),
        "view" => cmd_view(&opts),
        other => {
            print!("{USAGE}");
            Err(format!("unknown command: {other}"))
        }
    }
}

/// Every flag this runner takes, parsed by hand. Reaching for a CLI crate here
/// would make the tool that diagnoses a broken toolchain depend on a working
/// one; three subcommands do not need it.
#[derive(Debug, Default)]
struct Options {
    repo: Option<PathBuf>,
    port: Option<u16>,
    no_open: bool,
    module: bool,
    mode: Option<Mode>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut out = Options::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--port" => {
                    let v = it.next().ok_or("--port needs a value")?;
                    out.port = Some(v.parse().map_err(|_| format!("bad port: {v}"))?);
                }
                "--mode" => {
                    let v = it.next().ok_or("--mode needs a value")?;
                    out.mode = Some(Mode::parse(v)?);
                }
                "--no-open" => out.no_open = true,
                "--module" => out.module = true,
                flag if flag.starts_with('-') => return Err(format!("unknown flag: {flag}")),
                positional => {
                    if out.repo.replace(PathBuf::from(positional)).is_some() {
                        return Err(format!("unexpected extra argument: {positional}"));
                    }
                }
            }
        }
        Ok(out)
    }

    /// The repository to analyze: the argument, or this one.
    fn target_repo(&self) -> PathBuf {
        self.repo
            .clone()
            .unwrap_or_else(|| repo_root().to_path_buf())
    }

    fn port(&self) -> u16 {
        self.port.unwrap_or(8765)
    }
}

/// This repository's root, resolved at compile time so the runner works from
/// any working directory (`cargo run` does not chdir to the workspace root).
fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ always has a parent")
}

fn frontend_dir() -> PathBuf {
    repo_root().join("apps").join("desktop").join("frontend")
}

/// Where the browser bundle is built. Deliberately not `apps/desktop/frontend/
/// dist`: that directory belongs to the Tauri build, which embeds it at compile
/// time, and two flows writing one directory is how you end up staring at a
/// placeholder page wondering what you broke.
fn browser_out() -> PathBuf {
    frontend_dir().join("target").join("browser-dev")
}

fn cmd_doctor(opts: &Options) -> Result<(), String> {
    let mode = opts.mode.unwrap_or(Mode::All);
    let checks = doctor::run(mode, opts.repo.as_deref(), opts.port.unwrap_or(8765));
    if report(&checks, mode) {
        Ok(())
    } else {
        Err("environment is not ready; see the fixes above".into())
    }
}

fn cmd_view(opts: &Options) -> Result<(), String> {
    let repo = opts.target_repo();
    let checks = doctor::run(Mode::Native, Some(&repo), 0);
    if !report(&checks, Mode::Native) {
        return Err("environment is not ready; see the fixes above".into());
    }

    step("building the viewer (wgpu + winit; the first build is the slow one)");
    cargo(&[
        "build",
        "--release",
        "-p",
        "lcw-cli",
        "--features",
        "viewer",
    ])?;

    step("opening the viewer");
    let mut args = vec!["view".to_string(), path_arg(&repo)];
    if opts.module {
        args.push("--view".into());
        args.push("module".into());
    }
    println!("    press `m` to jump to the primary entry point, `f` to anchor a flow\n");
    run_tool(&lcw_binary("release"), &args)
}

fn cmd_ui(opts: &Options) -> Result<(), String> {
    let repo = opts.target_repo();
    let port = opts.port();
    let checks = doctor::run(Mode::Browser, Some(&repo), port);
    if !report(&checks, Mode::Browser) {
        return Err("environment is not ready; see the fixes above".into());
    }

    let out = browser_out();
    step("building the analyzer");
    cargo(&["build", "-p", "lcw-cli", "--features", "viewer"])?;

    step(&format!("building the wasm UI into {}", out.display()));
    trunk_build(&out)?;

    step(&format!("analyzing {}", repo.display()));
    let fixture = out.join("fixture.json");
    run_tool(
        &lcw_binary("debug"),
        &[
            "analyze".into(),
            path_arg(&repo),
            "--format".into(),
            "view".into(),
            "-o".into(),
            path_arg(&fixture),
        ],
    )?;

    let url = format!("http://127.0.0.1:{port}/");
    println!("\n    {url}");
    println!("    type `fixture.json` in the path box, press Analyze, then Entry\n");
    if !opts.no_open {
        open_url(&url);
    }
    serve::listen(&out, port)
}

fn step(what: &str) {
    println!("==> {what}");
}

/// The `cargo` that invoked us, so a `+toolchain` or a non-default rustup
/// toolchain is inherited rather than silently swapped for whatever is on PATH.
fn cargo(args: &[&str]) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .args(args)
        .current_dir(repo_root())
        .status()
        .map_err(|e| format!("could not run {cargo}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed", args.join(" ")))
    }
}

fn trunk_build(out: &Path) -> Result<(), String> {
    let status = Command::new("trunk")
        .args(["build", "--release", "--dist"])
        .arg(out)
        .current_dir(frontend_dir())
        .status()
        .map_err(|e| format!("could not run trunk: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("`trunk build` failed".into())
    }
}

fn run_tool(bin: &Path, args: &[String]) -> Result<(), String> {
    let status = Command::new(bin)
        .args(args)
        .status()
        .map_err(|e| format!("could not run {}: {e}", bin.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} exited with {status}", bin.display()))
    }
}

fn lcw_binary(profile: &str) -> PathBuf {
    repo_root()
        .join("target")
        .join(profile)
        .join(format!("lcw{}", std::env::consts::EXE_SUFFIX))
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Hand a URL to the desktop. Three platforms, three openers — the reason the
/// viewer's own "open this file" key used to do nothing on Windows.
fn open_url(url: &str) {
    let spawned = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn().is_ok()
    } else if cfg!(target_os = "windows") {
        // `start` is a cmd builtin, not a program; the empty string is the
        // window title, which `start` would otherwise take the URL for.
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .is_ok()
    } else {
        Command::new("xdg-open").arg(url).spawn().is_ok()
    };
    if !spawned {
        println!("    (could not open a browser automatically; open the URL above)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_one_positional() {
        let a = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let o = Options::parse(&a("/tmp/repo --port 9000 --no-open")).unwrap();
        assert_eq!(o.repo, Some(PathBuf::from("/tmp/repo")));
        assert_eq!(o.port(), 9000);
        assert!(o.no_open);

        let o = Options::parse(&[]).unwrap();
        assert_eq!(o.port(), 8765);
        assert_eq!(o.target_repo(), repo_root());
    }

    #[test]
    fn rejects_bad_input_instead_of_guessing() {
        let a = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(Options::parse(&a("--port")).is_err());
        assert!(Options::parse(&a("--port lots")).is_err());
        assert!(Options::parse(&a("--frobnicate")).is_err());
        assert!(Options::parse(&a("one two")).is_err());
        assert!(Options::parse(&a("--mode sideways")).is_err());
    }

    #[test]
    fn repo_root_holds_the_workspace() {
        assert!(repo_root().join("Cargo.toml").is_file());
        assert!(frontend_dir().join("Trunk.toml").is_file());
        assert!(browser_out().starts_with(frontend_dir().join("target")));
    }
}
