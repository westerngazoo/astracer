//! Smoke tests for the agent-facing commands, run against the built `lcw`
//! binary on a tiny fixture repo with an isolated cache home.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base =
            std::env::temp_dir().join(format!("lcw-cli-{tag}-{}-{nanos}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let mut src = String::from("/// Adds one.\npub fn inc(x: i32) -> i32 {\n    hub(x) + 1\n}\n\nfn hub(x: i32) -> i32 {\n    x\n}\n");
        for i in 0..30 {
            src.push_str(&format!(
                "\npub fn caller{i}() -> i32 {{\n    hub({i})\n}}\n"
            ));
        }
        src.push_str("\npub mod a { pub fn new() {} }\npub mod b { pub fn new() {} }\n");
        std::fs::write(repo.join("src/lib.rs"), src).unwrap();
        let cache = base.join("cache");
        Fixture { base, repo, cache }
    }

    fn lcw(&self, args: &[&str]) -> Output {
        self.lcw_in(&self.repo, args)
    }

    fn lcw_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lcw"))
            .args(args)
            .current_dir(cwd)
            .env("LIVEWALK_CACHE_HOME", &self.cache)
            .output()
            .expect("run lcw")
    }

    fn json(&self, args: &[&str]) -> (Value, i32) {
        let out = self.lcw(args);
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
        (v, out.status.code().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn span_prints_exact_range_and_code() {
    let f = Fixture::new("span");
    let o = f.lcw(&["span", "inc"]);
    assert!(o.status.success());
    assert_eq!(stdout(&o), "src/lib.rs:1-4\n");

    let o = f.lcw(&["span", "inc", "--code"]);
    assert_eq!(
        stdout(&o),
        "/// Adds one.\npub fn inc(x: i32) -> i32 {\n    hub(x) + 1\n}\n"
    );

    let (v, code) = f.json(&["span", "hub", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["schema"], 1);
    assert_eq!(v["span"], "src/lib.rs:6-8");
}

#[test]
fn paths_are_repo_relative_from_any_cwd() {
    let f = Fixture::new("paths");
    let repo = f.repo.to_string_lossy().into_owned();
    let elsewhere = f.base.clone();

    let o = f.lcw_in(&elsewhere, &["span", "inc", "--path", &repo]);
    assert!(o.status.success());
    assert_eq!(stdout(&o), "src/lib.rs:1-4\n");

    let o = f.lcw_in(&elsewhere, &["span", "inc", "--code", "--path", &repo]);
    assert!(stdout(&o).starts_with("/// Adds one.\n"), "{}", stdout(&o));

    let o = f.lcw_in(&elsewhere, &["explain", "hub", "--path", &repo]);
    assert!(
        stdout(&o).contains("where:   src/lib.rs:6"),
        "{}",
        stdout(&o)
    );

    let out = f.lcw_in(&elsewhere, &["impact", "hub", "--json", "--path", &repo]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["symbol"]["at"], "src/lib.rs:6");
    assert!(v["callers"][0]["site"]
        .as_str()
        .unwrap()
        .starts_with("src/lib.rs:"));

    let o = f.lcw_in(
        &elsewhere,
        &["span", "inc", "--absolute-paths", "--path", &repo],
    );
    let canon = f.repo.canonicalize().unwrap();
    assert_eq!(
        stdout(&o),
        format!("{}:1-4\n", canon.join("src/lib.rs").display())
    );
}

#[test]
fn flow_snippets_are_text_unless_json() {
    let f = Fixture::new("flow");
    let o = f.lcw(&["flow", "hub", "--from", "inc", "--snippets"]);
    assert!(o.status.success());
    let text = stdout(&o);
    assert!(text.starts_with("flow: "), "{text}");
    assert!(text.contains("repo::inc   [src/lib.rs:1]"), "{text}");
    assert!(text.contains(" 3 |     hub(x) + 1\n"), "{text}");
    assert!(text.contains(" 6 | fn hub(x: i32) -> i32 {\n"), "{text}");

    let (v, code) = f.json(&["flow", "hub", "--from", "inc", "--snippets", "--json"]);
    assert_eq!(code, 0);
    assert!(v["paths"][0][1]["code"]
        .as_str()
        .unwrap()
        .starts_with("fn hub"));
}

#[test]
fn flow_without_from_tries_every_entry_and_says_which() {
    let f = Fixture::new("flow-auto");
    // Two binaries' mains, only one of which reaches `hub`; a function only a
    // test reaches; and a cycle nothing reaches.
    std::fs::write(
        f.repo.join("src/lib.rs"),
        "pub fn inc(x: i32) -> i32 {\n    hub(x) + 1\n}\n\nfn hub(x: i32) -> i32 {\n    x\n}\n\n\
         pub mod tool {\n    pub fn main() {\n        helper();\n    }\n    fn helper() {}\n}\n\n\
         pub mod app {\n    pub fn main() {\n        crate::inc(1);\n    }\n}\n\n\
         fn only_tested() {}\n\n\
         fn ping() {\n    pong();\n}\n\nfn pong() {\n    ping();\n}\n\n\
         #[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        super::only_tested();\n    }\n}\n",
    )
    .unwrap();

    let o = f.lcw(&["flow", "hub"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = stdout(&o);
    assert!(text.starts_with("entry: repo::app::main"), "{text}");
    assert!(
        text.contains("program entry; picked automatically"),
        "{text}"
    );
    assert!(
        text.contains("flow: repo::app::main  →  repo::hub"),
        "{text}"
    );

    let (v, code) = f.json(&["flow", "hub", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(v["source"], "repo::app::main");
    assert_eq!(v["entry_kind"], "program");
    assert!(v.get("test_only").is_none());

    let o = f.lcw(&["flow", "only_tested"]);
    assert!(o.status.success());
    let text = stdout(&o);
    assert!(text.contains("TEST-ONLY entry"), "{text}");
    assert!(text.contains("test-only: no program main"), "{text}");
    assert!(text.contains("repo::tests::t"), "{text}");
    let (v, _) = f.json(&["flow", "only_tested", "--json"]);
    assert_eq!(v["test_only"], true);
    assert_eq!(v["entry_kind"], "test");

    let (v, _) = f.json(&["flow", "hub", "--all-entries", "--json"]);
    let sources: Vec<&str> = v["flows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources[0], "repo::app::main");
    assert!(!sources.contains(&"repo::tool::main"), "{sources:?}");

    let o = f.lcw(&["flow", "ping"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("no entry point"), "{err}");
    assert!(err.contains("lcw impact ping"), "{err}");
}

#[test]
fn impact_callers_and_budget() {
    let f = Fixture::new("impact");
    let (v, _) = f.json(&["impact", "hub", "--json"]);
    assert_eq!(v["schema"], 1);
    assert_eq!(v["total_callers"], 31);
    assert_eq!(v["by_depth"][0], 31);
    assert!(v["callers"][0]["site"]
        .as_str()
        .unwrap()
        .starts_with("src/lib.rs:"));

    let (v, _) = f.json(&["callers", "hub", "--json"]);
    assert_eq!(v["direction"], "callers");
    assert_eq!(v["tree"]["children"].as_array().unwrap().len(), 31);

    // A tight budget cuts the list and says how much was dropped.
    let (v, _) = f.json(&["--budget", "200", "explain", "hub", "--json"]);
    let kept = v["callers"].as_array().unwrap().len();
    assert!(kept < 31);
    assert_eq!(v["callers_more"].as_u64().unwrap() as usize, 31 - kept);

    let o = f.lcw(&["--budget", "100", "impact", "hub"]);
    assert!(stdout(&o).contains("more"), "{}", stdout(&o));
}

#[test]
fn ambiguous_and_unknown_symbols_list_candidates_and_exit_2() {
    let f = Fixture::new("lookup");
    let (v, code) = f.json(&["explain", "new", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(v["error"], "ambiguous");
    assert_eq!(v["candidates"].as_array().unwrap().len(), 2);

    let (v, code) = f.json(&["span", "hbu", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(v["error"], "not_found");
    assert!(v["candidates"][0]["qualified_name"]
        .as_str()
        .unwrap()
        .ends_with("hub"));

    let o = f.lcw(&["impact", "new"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).contains("is ambiguous"));
}

#[test]
fn queries_reuse_a_fresh_cache_and_reanalyze_when_stale() {
    let f = Fixture::new("cache");
    let first = f.lcw(&["span", "hub"]);
    assert!(first.status.success());
    assert!(
        first.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let warm = f.lcw(&["span", "hub"]);
    assert!(warm.stderr.is_empty());
    assert_eq!(warm.stdout, first.stdout);

    touch_future(&f.repo.join("src/lib.rs"));
    let stale = f.lcw(&["span", "hub"]);
    assert!(String::from_utf8_lossy(&stale.stderr).contains("note: re-analyzed"));
    assert_eq!(stale.stdout, first.stdout);

    let forced = f.lcw(&["span", "hub", "--no-cache"]);
    assert!(forced.stderr.is_empty());
}

fn touch_future(p: &Path) {
    let file = std::fs::File::options().write(true).open(p).unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .unwrap();
}
