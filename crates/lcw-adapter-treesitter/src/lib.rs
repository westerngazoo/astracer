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
