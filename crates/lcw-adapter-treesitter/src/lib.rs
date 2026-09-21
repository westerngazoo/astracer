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
}
