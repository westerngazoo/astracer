//! Environment checks that run *before* a build, so a missing prerequisite
//! costs one line instead of a failed build.
//!
//! The case that motivated this: without the `wasm32-unknown-unknown` target,
//! `trunk build` starts normally and then dies inside cargo with an `E0463`
//! from every crate in the tree at once — there is no `libcore.rlib` for that
//! target, so the implicit `extern crate core` fails everywhere — and the one
//! `note:` naming the cause scrolls past. Nothing about those screens says
//! "run one rustup command".

use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

/// Which flow the caller is about to use. The checks differ: the native viewer
/// needs no wasm target and no trunk, and saying otherwise would send someone
/// installing a toolchain they will never use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browser,
    Native,
    All,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "browser" => Ok(Mode::Browser),
            "native" => Ok(Mode::Native),
            "all" => Ok(Mode::All),
            other => Err(format!("unknown mode `{other}` (browser | native | all)")),
        }
    }

    fn wants_browser(self) -> bool {
        matches!(self, Mode::Browser | Mode::All)
    }

    fn wants_native(self) -> bool {
        matches!(self, Mode::Native | Mode::All)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Present and usable.
    Ok,
    /// Absent, and the flow cannot run without it.
    Missing,
    /// Could not be determined. Not a failure: an unmanaged toolchain answers
    /// no questions about its targets, and guessing would block a working setup.
    Unknown,
}

#[derive(Debug)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    /// What was found, for the passing case ("trunk 0.21.14").
    pub detail: String,
    /// The exact command that fixes it, for the failing case.
    pub fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn missing(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Missing,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn unknown(name: &'static str, detail: impl Into<String>) -> Self {
        Check {
            name,
            status: Status::Unknown,
            detail: detail.into(),
            fix: None,
        }
    }
}

/// Run the checks for `mode`. `port` of 0 skips the port check.
pub fn run(mode: Mode, repo: Option<&Path>, port: u16) -> Vec<Check> {
    let mut out = vec![check_cargo()];

    let rustup = first_line("rustup", &["show", "active-toolchain"]);
    out.push(match &rustup {
        Some(t) => Check::ok("toolchain", t.clone()),
        None => Check::unknown("toolchain", "no rustup; targets are managed elsewhere"),
    });

    if let Some(repo) = repo {
        out.push(check_repo(repo));
    }

    if mode.wants_browser() {
        out.push(check_wasm_target(rustup.is_some()));
        out.push(check_trunk());
        if port != 0 {
            out.push(check_port(port));
        }
    }

    if mode.wants_native() {
        out.push(Check::ok("gpu backend", expected_backend()));
    }

    out
}

fn check_cargo() -> Check {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    match first_line(&cargo, &["--version"]) {
        Some(v) => Check::ok("cargo", v),
        None => Check::missing(
            "cargo",
            "not on PATH",
            "install Rust from https://rustup.rs",
        ),
    }
}

fn check_repo(repo: &Path) -> Check {
    if repo.is_dir() {
        Check::ok("repository", repo.display().to_string())
    } else {
        Check::missing(
            "repository",
            format!("not a directory: {}", repo.display()),
            "pass a path that exists: lcw-dev ui /path/to/repo",
        )
    }
}

/// `rustup target add` applies to one toolchain, so a target can be installed
/// and still missing for the toolchain cargo resolves to here. The check asks
/// the active toolchain rather than trusting that it was ever added.
fn check_wasm_target(has_rustup: bool) -> Check {
    const TARGET: &str = "wasm32-unknown-unknown";
    if !has_rustup {
        return Check::unknown(
            "wasm target",
            "cannot verify without rustup; the build will say if it is missing",
        );
    }
    match Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
    {
        Ok(o) if o.status.success() => {
            let installed = String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == TARGET);
            if installed {
                Check::ok("wasm target", TARGET)
            } else {
                Check::missing(
                    "wasm target",
                    format!("{TARGET} not installed for the active toolchain"),
                    format!("rustup target add {TARGET}"),
                )
            }
        }
        _ => Check::unknown("wasm target", "rustup did not answer"),
    }
}

fn check_trunk() -> Check {
    match first_line("trunk", &["--version"]) {
        Some(v) => Check::ok("trunk", v),
        None => Check::missing("trunk", "not on PATH", "cargo install trunk --locked"),
    }
}

fn check_port(port: u16) -> Check {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => {
            drop(l);
            Check::ok("port", format!("127.0.0.1:{port} is free"))
        }
        Err(e) => Check::missing(
            "port",
            format!("cannot bind 127.0.0.1:{port}: {e}"),
            format!("stop whatever holds it, or pass --port {}", port + 1),
        ),
    }
}

/// What wgpu will most likely pick here. Informational only: whether an adapter
/// actually exists cannot be known without initializing one, which costs more
/// than this check is worth.
fn expected_backend() -> String {
    let backend = if cfg!(target_os = "macos") {
        "Metal"
    } else if cfg!(target_os = "windows") {
        "DX12 or Vulkan"
    } else {
        "Vulkan or GL"
    };
    format!("expecting {backend}")
}

/// First line of a command's stdout, or `None` if it cannot be run. A missing
/// program and a program that errors are the same answer here: unusable.
fn first_line(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

/// Print the checks. Returns whether the flow can proceed — `Unknown` never
/// blocks, since it means "not determinable here", not "broken".
pub fn report(checks: &[Check], mode: Mode) -> bool {
    let what = match mode {
        Mode::Browser => "browser dev mode",
        Mode::Native => "the native viewer",
        Mode::All => "both UI modes",
    };
    println!("checking {what}\n");

    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in checks {
        let mark = match c.status {
            Status::Ok => "ok  ",
            Status::Missing => "MISS",
            Status::Unknown => "?   ",
        };
        println!("  [{mark}] {:width$}  {}", c.name, c.detail);
    }

    let missing: Vec<&Check> = checks
        .iter()
        .filter(|c| c.status == Status::Missing)
        .collect();
    if missing.is_empty() {
        println!();
        return true;
    }

    println!("\nfix, then run again:\n");
    for c in &missing {
        if let Some(fix) = &c.fix {
            println!("    {fix}");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_select_their_own_checks() {
        assert!(Mode::parse("browser").unwrap().wants_browser());
        assert!(!Mode::parse("browser").unwrap().wants_native());
        assert!(Mode::parse("native").unwrap().wants_native());
        assert!(!Mode::parse("native").unwrap().wants_browser());
        let all = Mode::parse("all").unwrap();
        assert!(all.wants_browser() && all.wants_native());
        assert!(Mode::parse("gpu").is_err());
    }

    #[test]
    fn native_mode_does_not_demand_the_web_toolchain() {
        let names: Vec<&str> = run(Mode::Native, None, 0).iter().map(|c| c.name).collect();
        assert!(!names.contains(&"trunk"));
        assert!(!names.contains(&"wasm target"));
        assert!(names.contains(&"gpu backend"));
    }

    #[test]
    fn browser_mode_checks_the_web_toolchain_and_the_port() {
        let checks = run(Mode::Browser, None, 0);
        let names: Vec<&str> = checks.iter().map(|c| c.name).collect();
        assert!(names.contains(&"trunk"));
        assert!(names.contains(&"wasm target"));
        // port 0 means "do not check"; anything else is checked.
        assert!(!names.contains(&"port"));
    }

    #[test]
    fn a_missing_directory_is_caught_with_the_path_in_the_message() {
        let c = check_repo(Path::new("/definitely/not/here"));
        assert_eq!(c.status, Status::Missing);
        assert!(c.detail.contains("/definitely/not/here"));
        assert!(c.fix.is_some());
    }

    #[test]
    fn an_existing_directory_passes() {
        assert_eq!(
            check_repo(Path::new(env!("CARGO_MANIFEST_DIR"))).status,
            Status::Ok
        );
    }

    #[test]
    fn a_held_port_is_reported_as_held() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let c = check_port(port);
        assert_eq!(c.status, Status::Missing);
        assert!(c.fix.unwrap().contains(&(port + 1).to_string()));
    }

    #[test]
    fn unknown_never_blocks_but_missing_does() {
        let ok = vec![Check::ok("a", "fine"), Check::unknown("b", "cannot tell")];
        assert!(report(&ok, Mode::All));
        let bad = vec![Check::missing("a", "gone", "install a")];
        assert!(!report(&bad, Mode::All));
    }

    #[test]
    fn a_program_that_is_not_there_yields_none() {
        assert!(first_line("lcw-definitely-not-a-program", &["--version"]).is_none());
    }
}
