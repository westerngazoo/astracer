//! # lcw-adapter-treesitter
//!
//! The default Layer 1 [`LanguageAdapter`]: a tree-sitter based Rust parser
//! with heuristic call resolution. Fast and error-tolerant, so it scales to
//! giant repositories (Manifesto: giant-repo performance target).

mod extract;

pub use extract::module_path_from;

use lcw_core::{AdapterError, CodeGraph, FileFragment, LanguageAdapter, SourceFile};

/// Rust front end backed by tree-sitter.
#[derive(Debug, Default, Clone, Copy)]
pub struct RustTreeSitterAdapter;

impl RustTreeSitterAdapter {
    pub fn new() -> Self {
        RustTreeSitterAdapter
    }
}

impl LanguageAdapter for RustTreeSitterAdapter {
    fn name(&self) -> &'static str {
        "treesitter-rust"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn parse(&self, files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
        extract::parse_rust(files)
    }

    fn supports_incremental(&self) -> bool {
        true
    }

    fn extract_fragment(&self, file: &SourceFile) -> Result<FileFragment, AdapterError> {
        extract::extract_file(file)
    }

    fn resolve_fragments(&self, fragments: &[FileFragment]) -> Result<CodeGraph, AdapterError> {
        Ok(extract::resolve_fragments(fragments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{EdgeKind, NodeKind};

    fn parse(src: &str) -> CodeGraph {
        let files = vec![SourceFile::new("src/lib.rs", src)];
        RustTreeSitterAdapter::new().parse(&files).unwrap()
    }

    #[test]
    fn extracts_functions_and_direct_calls() {
        let g = parse(
            r#"
            fn helper(x: i32) -> i32 { x + 1 }
            fn main() {
                let a = helper(1);
                let b = helper(a);
            }
            "#,
        );
        let main = g.node_by_qualified("crate::main").expect("main present");
        let helper = g
            .node_by_qualified("crate::helper")
            .expect("helper present");
        assert_eq!(g.node(helper).kind, NodeKind::Function);
        // main -> helper, merged into one edge with count 2.
        assert_eq!(g.out_degree(main), 1);
        let (_, _, e) = g
            .edges()
            .find(|(f, t, _)| *f == main && *t == helper)
            .unwrap();
        assert_eq!(e.kind, EdgeKind::DirectCall);
        assert_eq!(e.count, 2);
    }

    #[test]
    fn methods_get_type_qualified_names() {
        let g = parse(
            r#"
            struct Counter { n: u32 }
            impl Counter {
                fn incr(&mut self) { self.n += 1; }
                fn run(&mut self) {
                    self.incr();
                    self.incr();
                }
            }
            "#,
        );
        let run = g
            .node_by_qualified("crate::Counter::run")
            .expect("run present");
        let incr = g
            .node_by_qualified("crate::Counter::incr")
            .expect("incr present");
        assert!(g.node(incr).flags.is_method);
        let (_, _, e) = g.edges().find(|(f, t, _)| *f == run && *t == incr).unwrap();
        assert_eq!(e.kind, EdgeKind::MethodCall);
    }

    #[test]
    fn cyclomatic_complexity_counts_branches() {
        let g = parse(
            r#"
            fn classify(x: i32) -> i32 {
                if x > 0 {
                    if x > 10 { 2 } else { 1 }
                } else if x < 0 && x > -5 {
                    -1
                } else {
                    0
                }
            }
            "#,
        );
        let f = g.node_by_qualified("crate::classify").unwrap();
        // if + (else-if is another if) + inner if + `&&` = 4 decision points => CC 5.
        assert_eq!(g.node(f).cyclomatic_complexity(), 5);
    }

    #[test]
    fn incremental_extract_resolve_matches_parse_and_links_cross_file() {
        let a = SourceFile::new("src/a.rs", "fn a() { b(); b(); }");
        let b = SourceFile::new("src/b.rs", "fn b() {}");
        let adapter = RustTreeSitterAdapter::new();

        // Whole-batch parse vs. per-file extract + global resolve: identical.
        let g1 = adapter.parse(&[a.clone(), b.clone()]).unwrap();
        let fa = adapter.extract_fragment(&a).unwrap();
        let fb = adapter.extract_fragment(&b).unwrap();
        let g2 = adapter.resolve_fragments(&[fa, fb]).unwrap();
        assert_eq!(g1.node_count(), g2.node_count());
        assert_eq!(g1.edge_count(), g2.edge_count());

        // The call in a.rs resolves to the real def in b.rs (cross-file), and
        // the two identical call sites merge into one edge with count 2.
        let a_id = g2.node_by_qualified("crate::a::a").unwrap();
        let b_id = g2.node_by_qualified("crate::b::b").unwrap();
        assert_eq!(g2.node(b_id).kind, NodeKind::Function);
        let (_, _, e) = g2
            .edges()
            .find(|(f, t, _)| *f == a_id && *t == b_id)
            .expect("cross-file edge a -> b");
        assert_eq!(e.kind, EdgeKind::DirectCall);
        assert_eq!(e.count, 2);
    }

    #[test]
    fn foreign_abi_exports_are_flagged() {
        let g = parse(
            r#"
            /// Entry the bootloader jumps to. Mentions #[no_mangle] in prose.
            #[no_mangle]
            pub extern "C" fn kmain() -> ! { loop {} }

            #[export_name = "trap_entry"]
            fn trap_handler() {}

            extern "C" fn callback(x: i32) -> i32 { x }

            /// Plain Rust: not reachable from a foreign ABI.
            pub fn helper() {}

            unsafe extern "C" {
                pub fn host_write(fd: i32);
            }
            "#,
        );
        let flags = |qn: &str| g.node(g.node_by_qualified(qn).expect(qn)).flags;
        assert!(flags("crate::kmain").is_exported, "#[no_mangle]");
        assert!(flags("crate::trap_handler").is_exported, "#[export_name]");
        assert!(flags("crate::callback").is_exported, "extern \"C\" fn");
        assert!(!flags("crate::helper").is_exported, "plain pub fn");
        // A declaration inside an `extern` block is an *import*: the symbol is
        // defined elsewhere, so it must never count as an export.
        assert!(!flags("crate::host_write").is_exported, "extern block decl");
    }

    #[test]
    fn type_qualified_calls_bind_only_to_matching_types() {
        let g = parse(
            r#"
            struct A;
            struct B;
            impl A { fn new() -> Self { A } }
            impl B { fn new() -> Self { B } }
            fn f() {
                let _a = A::new();
                let _b = B::new();
                let _v: Vec<u8> = Vec::new();
            }
            "#,
        );
        let f = g.node_by_qualified("crate::f").unwrap();
        let a_new = g.node_by_qualified("crate::A::new").unwrap();
        let b_new = g.node_by_qualified("crate::B::new").unwrap();
        let callees: Vec<_> = g.neighbors_out(f).collect();
        assert!(callees.contains(&a_new));
        assert!(callees.contains(&b_new));
        // `Vec` is not defined here: the call must stay external instead of
        // being glued to an unrelated `new` (which would invent a flow edge).
        let ext = callees
            .iter()
            .copied()
            .find(|&c| g.node(c).kind == NodeKind::External)
            .expect("Vec::new stays external");
        assert_eq!(g.node(ext).qualified_name, "Vec::new");
        assert_eq!(callees.len(), 3);
    }

    #[test]
    fn ambiguous_short_names_prefer_the_callers_crate() {
        // Two crates define `helper`; a call from crate `a` (a different
        // module than either def) must bind to `a::helper`, whatever the file
        // order, not to whichever def happened to be declared first.
        let files = vec![
            SourceFile::new("crates/b/src/lib.rs", "pub fn helper() {}"),
            SourceFile::new("crates/a/src/lib.rs", "pub fn helper() {}"),
            SourceFile::new("crates/a/src/x.rs", "pub fn go() { helper(); }"),
        ];
        let g = RustTreeSitterAdapter::new().parse(&files).unwrap();
        let go = g.node_by_qualified("a::x::go").unwrap();
        let a_helper = g.node_by_qualified("a::helper").unwrap();
        assert_eq!(g.neighbors_out(go).collect::<Vec<_>>(), vec![a_helper]);
    }

    #[test]
    fn unresolved_calls_become_external() {
        let g = parse(
            r#"
            fn f() {
                some_unknown_fn();
                println!("hi");
            }
            "#,
        );
        let f = g.node_by_qualified("crate::f").unwrap();
        // Both targets are external; the edge kind is Unresolved.
        assert!(g
            .edges()
            .filter(|(from, _, _)| *from == f)
            .all(|(_, to, e)| g.node(to).kind == NodeKind::External
                && e.kind == EdgeKind::Unresolved));
    }

    // --- spawn sites: where a new thread of control begins ---------------------

    /// Every edge out of `from`, as (target qualified name, kind).
    fn edges_from(g: &CodeGraph, from: &str) -> Vec<(String, EdgeKind)> {
        let f = g
            .node_by_qualified(from)
            .unwrap_or_else(|| panic!("{from} missing"));
        let mut out: Vec<(String, EdgeKind)> = g
            .edges_out(f)
            .map(|(to, e)| (g.node(to).qualified_name.clone(), e.kind))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn spawn_targets(g: &CodeGraph, from: &str) -> Vec<String> {
        edges_from(g, from)
            .into_iter()
            .filter(|(_, k)| *k == EdgeKind::Spawn)
            .map(|(t, _)| t)
            .collect()
    }

    #[test]
    fn a_spawned_closure_is_its_own_node_and_owns_its_calls() {
        let g = parse(
            r#"
            fn worker(n: u32) -> u32 { n }
            fn main() {
                let h = thread::spawn(move || worker(3));
            }
            "#,
        );
        let closure = "crate::main::<spawned@L4>";
        let id = g.node_by_qualified(closure).expect("closure node");
        assert_eq!(g.node(id).kind, NodeKind::Closure);
        assert_eq!(
            g.node(id).module_path,
            "crate::main",
            "nested under its caller"
        );
        assert_eq!(spawn_targets(&g, "crate::main"), vec![closure.to_string()]);
        // The body runs on the new thread, so its call belongs to the closure —
        // including when the body is a bare expression, not a block.
        assert!(edges_from(&g, closure).contains(&("crate::worker".into(), EdgeKind::DirectCall)));
        assert!(
            !edges_from(&g, "crate::main")
                .iter()
                .any(|(t, _)| t == "crate::worker"),
            "main does not call worker; the thread does"
        );
    }

    #[test]
    fn block_bodies_async_blocks_and_builder_chains() {
        let g = parse(
            r#"
            fn crunch() {}
            fn handle() {}
            fn main() {
                thread::Builder::new().name("x".into()).spawn(move || { crunch(); handle(); });
                tokio::spawn(async move { handle() });
            }
            "#,
        );
        let spawned = spawn_targets(&g, "crate::main");
        assert_eq!(
            spawned,
            vec!["crate::main::<spawned@L5>", "crate::main::<spawned@L6>"]
        );
        assert_eq!(
            edges_from(&g, "crate::main::<spawned@L5>"),
            vec![
                ("crate::crunch".into(), EdgeKind::DirectCall),
                ("crate::handle".into(), EdgeKind::DirectCall)
            ]
        );
        let task = g.node_by_qualified("crate::main::<spawned@L6>").unwrap();
        assert!(
            g.node(task).flags.is_async,
            "an async block is an async task"
        );
        // The builder's own calls still run on main's thread.
        assert!(edges_from(&g, "crate::main")
            .iter()
            .any(|(t, _)| t == "thread::Builder::new"));
    }

    #[test]
    fn a_passed_function_is_spawned_not_left_looking_dead() {
        let g = parse(
            r#"
            fn listener() {}
            mod pool { pub fn run() {} }
            fn main() {
                thread::spawn(listener);
                thread::spawn(pool::run);
            }
            "#,
        );
        assert_eq!(
            spawn_targets(&g, "crate::main"),
            vec!["crate::listener", "crate::pool::run"]
        );
    }

    #[test]
    fn a_variable_is_not_a_function_even_when_a_function_shares_its_name() {
        // Wari's kernel shape: `spawn(slot)` starts a process from a module
        // slot number. A function named `slot` elsewhere must not become a
        // thread root.
        let g = parse(
            r#"
            fn slot() {}
            fn spawn(_s: u8) {}
            fn from_let() { let slot = 7u8; spawn(slot); }
            fn from_param(slot: u8) { spawn(slot); }
            fn from_closure_param() { (0..3).for_each(|slot| spawn(slot)); }
            "#,
        );
        for f in [
            "crate::from_let",
            "crate::from_param",
            "crate::from_closure_param",
        ] {
            assert!(spawn_targets(&g, f).is_empty(), "{f} spawns nothing");
        }
    }

    #[test]
    fn spawn_calls_without_a_runnable_argument_yield_nothing() {
        let g = parse(
            r#"
            fn main() {
                std::process::Command::new("ls").spawn();
                let v: Vec<u32> = (0..3).map(|i| i + 1).collect();
            }
            "#,
        );
        assert!(spawn_targets(&g, "crate::main").is_empty());
        assert!(
            !g.nodes().any(|n| n.kind == NodeKind::Closure),
            "only spawned closures become nodes"
        );
    }

    #[test]
    fn an_unresolvable_spawn_target_is_dropped_not_invented() {
        let g = parse("fn main() { thread::spawn(elsewhere::run); }");
        assert!(spawn_targets(&g, "crate::main").is_empty());
        assert!(
            g.node_by_qualified("elsewhere::run").is_none(),
            "no external placeholder for a thread root that is not there"
        );
    }

    #[test]
    fn a_bare_spawn_target_binds_only_within_its_crate() {
        let files = vec![
            SourceFile::new("jobs/src/lib.rs", "pub fn job() {}"),
            SourceFile::new("app/src/main.rs", "fn main() { std::thread::spawn(job); }"),
            SourceFile::new("app/src/tasks.rs", "pub fn local_job() {}"),
            SourceFile::new(
                "app/src/run.rs",
                "fn go() { std::thread::spawn(local_job); }",
            ),
        ];
        let g = RustTreeSitterAdapter::new().parse(&files).unwrap();
        assert!(
            spawn_targets(&g, "app::main").is_empty(),
            "job is another crate's"
        );
        assert_eq!(
            spawn_targets(&g, "app::run::go"),
            vec!["app::tasks::local_job"]
        );
    }

    #[test]
    fn a_method_call_never_binds_to_a_free_function() {
        let g = parse(
            r#"
            fn join(a: &str) -> String { a.into() }
            struct Buf;
            impl Buf { fn push(&mut self) {} }
            fn run(p: std::path::PathBuf, mut b: Buf) {
                p.join("x");
                b.push();
                join("y");
            }
            "#,
        );
        let out = edges_from(&g, "crate::run");
        assert!(
            out.contains(&("join".into(), EdgeKind::Unresolved)),
            "`p.join()` is a method on someone else's type: {out:?}"
        );
        assert!(out.contains(&("crate::join".into(), EdgeKind::DirectCall)));
        assert!(out.contains(&("crate::Buf::push".into(), EdgeKind::MethodCall)));
    }

    #[test]
    fn a_macro_call_never_binds_to_a_function() {
        let g = parse(
            r#"
            fn matches(x: u8) -> bool { x > 0 }
            fn check(x: u8) -> bool { matches!(x, 1 | 2) || matches(x) }
            "#,
        );
        let out = edges_from(&g, "crate::check");
        assert_eq!(
            out,
            vec![
                ("crate::matches".into(), EdgeKind::DirectCall),
                ("matches".into(), EdgeKind::Unresolved),
            ]
        );
    }

    #[test]
    fn a_bare_call_never_binds_to_a_method_or_an_associated_function() {
        let g = parse(
            r#"
            struct Guard;
            impl Drop for Guard { fn drop(&mut self) {} }
            struct Cfg;
            impl Cfg { fn build() -> Cfg { Cfg } }
            fn run(g: Guard) {
                drop(g);
                build();
                Cfg::build();
            }
            "#,
        );
        let out = edges_from(&g, "crate::run");
        assert!(
            out.contains(&("drop".into(), EdgeKind::Unresolved)),
            "`drop(g)` is std::mem::drop: {out:?}"
        );
        assert!(
            out.contains(&("build".into(), EdgeKind::Unresolved)),
            "a bare `build()` cannot reach `Cfg::build`: {out:?}"
        );
        assert!(out.contains(&("crate::Cfg::build".into(), EdgeKind::AssociatedCall)));
        assert!(
            !out.iter().any(|(t, _)| t.ends_with("::drop")),
            "no edge to Guard's drop: {out:?}"
        );
    }

    #[test]
    fn a_bare_call_never_binds_to_a_method_of_a_primitive_impl() {
        // `impl Double for u32` scopes its method under a lowercase `u32`, so
        // only the method check (not the type-name one) can refuse it.
        let g = parse(
            r#"
            trait Double { fn double(&self) -> u32; }
            impl Double for u32 { fn double(&self) -> u32 { *self * 2 } }
            fn run() -> u32 { double(3) + 3u32.double() }
            "#,
        );
        let out = edges_from(&g, "crate::run");
        assert!(
            out.contains(&("double".into(), EdgeKind::Unresolved)),
            "a bare `double(3)` is not a method call: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|(t, k)| t.ends_with("::double") && *k == EdgeKind::DirectCall),
            "{out:?}"
        );
        assert!(out.iter().any(|(_, k)| *k == EdgeKind::MethodCall));
    }

    #[test]
    fn a_nested_fn_is_found_before_a_same_named_one_elsewhere() {
        let g = parse(
            r#"
            struct Tree;
            impl Tree {
                fn flatten(&self) { fn walk() { walk(); } walk(); }
            }
            fn count() { fn walk() {} walk(); }
            "#,
        );
        assert_eq!(
            edges_from(&g, "crate::count"),
            vec![("crate::count::walk".into(), EdgeKind::DirectCall)]
        );
        assert_eq!(
            edges_from(&g, "crate::Tree::flatten"),
            vec![("crate::Tree::flatten::walk".into(), EdgeKind::DirectCall)]
        );
        assert_eq!(
            edges_from(&g, "crate::Tree::flatten::walk"),
            vec![("crate::Tree::flatten::walk".into(), EdgeKind::DirectCall)],
            "a nested fn's recursion stays in its own scope"
        );
    }

    #[test]
    fn a_bare_spawn_reference_never_starts_a_method() {
        let g = parse(
            r#"
            struct Server;
            impl Server {
                fn serve(&self) {}
                fn listen() {}
                fn start() { std::thread::spawn(Self::listen); }
            }
            fn main() { std::thread::spawn(serve); }
            "#,
        );
        assert!(
            spawn_targets(&g, "crate::main").is_empty(),
            "`serve` takes self; a bare name cannot pass it"
        );
        assert_eq!(
            spawn_targets(&g, "crate::Server::start"),
            vec!["crate::Server::listen"]
        );
    }

    // --- method receivers: precision over name matching ----------------------

    /// Qualified names of the internal (non-external) callees of `from`.
    fn internal_callees(g: &CodeGraph, from: &str) -> Vec<String> {
        let f = g.node_by_qualified(from).expect(from);
        let mut out: Vec<String> = g
            .neighbors_out(f)
            .filter(|&t| g.node(t).kind != NodeKind::External)
            .map(|t| g.node(t).qualified_name.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn callers_of(g: &CodeGraph, to: &str) -> Vec<String> {
        let t = g.node_by_qualified(to).expect(to);
        let mut out: Vec<String> = g
            .edges()
            .filter(|(_, dst, _)| *dst == t)
            .map(|(src, _, _)| g.node(src).qualified_name.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn self_expect_binds_only_to_its_own_impl() {
        let g = parse(
            r#"
            struct Parser;
            impl Parser {
                fn expect(&mut self, c: char) -> Result<(), ()> { Ok(()) }
                fn atom(&mut self) -> Result<(), ()> {
                    self.expect('(')?;
                    self.expect(')')
                }
            }
            fn load(s: &str) -> u32 {
                let n: Option<u32> = s.parse().ok();
                let r: Result<u32, ()> = Ok(1);
                n.expect("n") + r.expect("r") + s.len().checked_add(1).expect("x")
            }
            "#,
        );
        assert_eq!(
            callers_of(&g, "crate::Parser::expect"),
            vec!["crate::Parser::atom"]
        );
        assert!(internal_callees(&g, "crate::load").is_empty());
    }

    #[test]
    fn ubiquitous_methods_on_unknown_receivers_stay_external() {
        let g = parse(
            r#"
            struct Camera;
            impl Camera { fn map(&self, p: u32) -> u32 { p } }
            struct Client;
            impl Client {
                fn get(&self, path: &str) -> u32 { 0 }
                fn expect(&self) {}
            }
            fn run(xs: Vec<u32>, m: std::collections::HashMap<u32, u32>) -> u32 {
                let a: Vec<u32> = xs.iter().map(|x| x + 1).collect();
                let b = m.get(&1).copied().unwrap_or(0);
                let c = std::env::var("X").ok().map(|s| s.len()).expect("set");
                b + c as u32
            }
            "#,
        );
        assert!(
            internal_callees(&g, "crate::run").is_empty(),
            "{:?}",
            internal_callees(&g, "crate::run")
        );
        let ext = edges_from(&g, "crate::run");
        for name in ["map", "get", "expect"] {
            assert!(
                ext.contains(&(name.into(), EdgeKind::Unresolved)),
                "`.{name}()` on an unknown receiver is external: {ext:?}"
            );
        }
    }

    #[test]
    fn a_typed_receiver_binds_to_its_type_even_for_common_names() {
        let g = parse(
            r#"
            struct Camera { zoom: u32 }
            impl Camera {
                fn new() -> Self { Camera { zoom: 1 } }
                fn map(&self, p: u32) -> u32 { p * self.zoom }
            }
            struct Grid;
            impl Grid { fn map(&self, p: u32) -> u32 { p } }
            struct Scene { cam: Camera }
            impl Scene {
                fn by_field(&self) -> u32 { self.cam.map(1) }
            }
            fn by_param(cam: &Camera) -> u32 { cam.map(1) }
            fn by_ctor() -> u32 { let c = Camera::new(); c.map(1) }
            fn by_literal() -> u32 { let c = Camera { zoom: 2 }; c.map(1) }
            fn by_annotation(x: u32) -> u32 { let g: Grid = Grid; g.map(x) }
            fn in_closure(cam: &Camera, xs: &[u32]) -> u32 {
                xs.iter().map(|p| cam.map(*p)).sum()
            }
            fn shadowed(cam: &Camera) -> u32 { let cam = Grid; cam.map(1) }
            "#,
        );
        for f in [
            "Scene::by_field",
            "by_param",
            "by_ctor",
            "by_literal",
            "in_closure",
        ] {
            let f = format!("crate::{f}");
            assert!(
                internal_callees(&g, &f).contains(&"crate::Camera::map".to_string()),
                "{f}: {:?}",
                internal_callees(&g, &f)
            );
            assert!(!internal_callees(&g, &f).contains(&"crate::Grid::map".to_string()));
        }
        assert_eq!(
            internal_callees(&g, "crate::by_annotation"),
            vec!["crate::Grid::map"]
        );
        assert!(
            internal_callees(&g, "crate::shadowed").is_empty(),
            "a name bound with two types is unknown, and `map` is too common to guess"
        );
    }

    #[test]
    fn a_path_qualified_receiver_type_is_not_the_repos_same_named_type() {
        // Akademos' shape: the repo's `Client` wraps `reqwest::Client`.
        let g = parse(
            r#"
            pub struct Client { http: reqwest::Client, me: crate::Client2 }
            pub struct Client2;
            impl Client2 { fn ping(&self) {} }
            impl Client {
                fn new() -> Self { Client { http: reqwest::Client::new(), me: Client2 } }
                fn get(&self, p: &str) -> u32 { 0 }
                fn status(&self) { self.http.get("x"); self.me.ping(); }
                fn fetch(&self) -> u32 { self.get("/a") }
                fn fresh() { let c = reqwest::Client::new(); c.get("y"); }
            }
            "#,
        );
        assert_eq!(
            callers_of(&g, "crate::Client::get"),
            vec!["crate::Client::fetch"]
        );
        assert_eq!(
            internal_callees(&g, "crate::Client::status"),
            vec!["crate::Client2::ping"]
        );
    }

    #[test]
    fn a_unique_uncommon_method_still_binds_without_a_receiver_type() {
        let g = parse(
            r#"
            struct Engine;
            impl Engine { fn analyze_with_progress(&self) {} }
            fn make() -> Engine { Engine }
            fn go() { make().analyze_with_progress(); }
            "#,
        );
        assert_eq!(
            internal_callees(&g, "crate::go"),
            vec!["crate::Engine::analyze_with_progress", "crate::make"]
        );
    }

    #[test]
    fn self_assoc_calls_bind_to_the_impl_type_only() {
        let g = parse(
            r#"
            struct A;
            impl A { fn make() -> Self { A } fn go() { Self::make(); Self::other(); } }
            struct B;
            impl B { fn other() {} }
            "#,
        );
        assert_eq!(internal_callees(&g, "crate::A::go"), vec!["crate::A::make"]);
    }

    #[test]
    fn calls_inside_macro_arguments_create_edges() {
        let g = parse(
            r#"
            pub fn slugify(s: &str) -> String { s.into() }
            mod util { pub fn norm(s: &str) -> String { s.into() } }
            struct P;
            impl P {
                fn peek(&self) -> u8 { 0 }
                fn check(&self) {
                    debug_assert!(self.peek() == 0, "{}", crate::util::norm("x"));
                }
            }
            #[test]
            fn slugs() {
                assert_eq!(slugify("A b"), "a-b");
                assert!(vec![util::norm("x")].len() == 1);
                println!("{}", format!("{}", slugify("nested")));
            }
            "#,
        );
        assert_eq!(
            internal_callees(&g, "crate::slugs"),
            vec!["crate::slugify", "crate::util::norm"]
        );
        assert_eq!(
            internal_callees(&g, "crate::P::check"),
            vec!["crate::P::peek", "crate::util::norm"]
        );
        let (_, _, e) = g
            .edges()
            .find(|(f, t, _)| {
                g.node(*f).qualified_name == "crate::slugs"
                    && g.node(*t).qualified_name == "crate::slugify"
            })
            .unwrap();
        assert_eq!(e.kind, EdgeKind::DirectCall);
        assert_eq!(e.count, 2, "both macro call sites, nested macros included");
    }

    #[test]
    fn macro_argument_methods_on_unknown_receivers_stay_external() {
        let g = parse(
            r#"
            struct Camera;
            impl Camera { fn map(&self, p: u32) -> u32 { p } }
            fn show(xs: Vec<u32>) {
                println!("{:?}", xs.iter().map(|x| x + 1).collect::<Vec<_>>());
            }
            "#,
        );
        assert!(internal_callees(&g, "crate::show").is_empty());
    }

    #[test]
    fn a_unique_method_in_a_macro_needs_a_known_receiver() {
        // `assert_eq!(res.status(), 200)` is about an HTTP response, not the
        // repo's one `Client::status`.
        let g = parse(
            r#"
            struct Client;
            impl Client { fn status_code(&self) -> u16 { 200 } }
            fn check(res: Response, c: &Client) {
                assert_eq!(res.status_code(), 200);
                assert_eq!(c.status_code(), 200);
            }
            fn other(res: Response) { assert_eq!(res.status_code(), 200); }
            "#,
        );
        assert_eq!(
            internal_callees(&g, "crate::check"),
            vec!["crate::Client::status_code"]
        );
        assert!(internal_callees(&g, "crate::other").is_empty());
    }

    #[test]
    fn calling_a_local_closure_is_not_calling_a_same_named_fn() {
        let g = parse(
            r#"
            mod far { pub fn at(n: u32) -> u32 { n } }
            fn at(n: u32) -> u32 { n }
            fn closure() {
                let at = |n: u32| n + 1;
                assert_eq!(at(1), 2);
                at(2);
            }
            fn param(at: impl Fn(u32) -> u32) { at(1); }
            fn item() { at(1); }
            "#,
        );
        assert!(internal_callees(&g, "crate::closure").is_empty());
        assert!(internal_callees(&g, "crate::param").is_empty());
        assert_eq!(internal_callees(&g, "crate::item"), vec!["crate::at"]);
    }

    #[test]
    fn fragments_without_a_receiver_field_still_deserialize() {
        // Fragments cached before receivers existed lack the field.
        let raw = r#"{"caller":"crate::f","kind":"method","path":"m","short":"m",
            "call_site":{"file":0,"start_line":1,"start_col":0,"end_line":1,"end_col":1}}"#;
        let rc: lcw_core::RawCall = serde_json::from_str(raw).unwrap();
        assert_eq!(rc.receiver, None);
        let back = serde_json::to_string(&rc).unwrap();
        assert!(!back.contains("receiver"), "None is omitted: {back}");
    }

    #[test]
    fn nested_and_same_line_spawns_get_distinct_nodes() {
        let g = parse(
            r#"
            fn leaf() {}
            fn main() {
                thread::spawn(move || { thread::spawn(move || leaf()); });
                let (a, b) = (thread::spawn(|| leaf()), thread::spawn(|| leaf()));
            }
            "#,
        );
        let outer = "crate::main::<spawned@L4>";
        assert_eq!(
            spawn_targets(&g, outer),
            vec!["crate::main::<spawned@L4>::<spawned@L4>"],
            "a spawn inside a spawned closure belongs to that closure"
        );
        let from_main = spawn_targets(&g, "crate::main");
        assert_eq!(from_main.len(), 3, "{from_main:?}");
        assert!(from_main.contains(&"crate::main::<spawned@L5>#2".to_string()));
    }
}
