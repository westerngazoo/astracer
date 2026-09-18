//! # lcw-adapter-go
//!
//! A Layer 1 [`LanguageAdapter`] for Go, backed by tree-sitter with heuristic
//! call resolution. It mirrors `lcw-adapter-treesitter` (the Rust front end):
//! fast and error-tolerant so it scales to giant repositories (Manifesto:
//! giant-repo performance target).
//!
//! Go scoping is package-based: the module path for each file is its declared
//! `package`, and a method's owner type qualifies its name
//! (`package::Type::Method`).

mod extract;

pub use extract::module_path_from;

use lcw_core::{AdapterError, CodeGraph, LanguageAdapter, SourceFile};

/// Go front end backed by tree-sitter.
#[derive(Debug, Default, Clone, Copy)]
pub struct GoTreeSitterAdapter;

impl GoTreeSitterAdapter {
    pub fn new() -> Self {
        GoTreeSitterAdapter
    }
}

impl LanguageAdapter for GoTreeSitterAdapter {
    fn name(&self) -> &'static str {
        "treesitter-go"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn parse(&self, files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
        extract::parse_go(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{EdgeKind, NodeKind};

    fn parse(src: &str) -> CodeGraph {
        let files = vec![SourceFile::new("sample/main.go", src)];
        GoTreeSitterAdapter::new().parse(&files).unwrap()
    }

    #[test]
    fn extracts_functions_and_direct_calls() {
        let g = parse(
            "package sample\n\nfunc helper(x int) int { return x + 1 }\n\nfunc run() {\n\thelper(1)\n\thelper(2)\n}\n",
        );
        let run = g.node_by_qualified("sample::run").expect("run present");
        let helper = g
            .node_by_qualified("sample::helper")
            .expect("helper present");
        assert_eq!(g.node(helper).kind, NodeKind::Function);
        // run -> helper, merged into one edge with count 2.
        assert_eq!(g.out_degree(run), 1);
        let (_, _, e) = g
            .edges()
            .find(|(f, t, _)| *f == run && *t == helper)
            .unwrap();
        assert_eq!(e.kind, EdgeKind::DirectCall);
        assert_eq!(e.count, 2);
    }

    #[test]
    fn methods_get_receiver_qualified_names() {
        let g = parse(
            "package sample\n\ntype Counter struct { n int }\n\nfunc (c *Counter) Incr() { c.n++ }\n\nfunc (c *Counter) Run() {\n\tc.Incr()\n\tc.Incr()\n}\n",
        );
        let run = g
            .node_by_qualified("sample::Counter::Run")
            .expect("Run present");
        let incr = g
            .node_by_qualified("sample::Counter::Incr")
            .expect("Incr present");
        assert!(g.node(incr).flags.is_method);
        // Exported (capitalized) methods are public API.
        assert!(g.node(incr).flags.is_pub);
        let (_, _, e) = g.edges().find(|(f, t, _)| *f == run && *t == incr).unwrap();
        assert_eq!(e.kind, EdgeKind::MethodCall);
    }

    #[test]
    fn unexported_names_are_not_public() {
        let g = parse("package sample\n\nfunc helper() {}\n");
        let helper = g.node_by_qualified("sample::helper").unwrap();
        assert!(!g.node(helper).flags.is_pub);
    }

    #[test]
    fn cyclomatic_complexity_counts_branches() {
        let g = parse(
            "package sample\n\nfunc classify(x int) int {\n\tif x > 0 {\n\t\tif x > 10 {\n\t\t\treturn 2\n\t\t}\n\t\treturn 1\n\t} else if x < 0 && x > -5 {\n\t\treturn -1\n\t}\n\treturn 0\n}\n",
        );
        let f = g.node_by_qualified("sample::classify").unwrap();
        // if + inner if + else-if + `&&` = 4 decision points => CC 5.
        assert_eq!(g.node(f).cyclomatic_complexity(), 5);
    }

    #[test]
    fn unresolved_calls_become_external() {
        let g = parse(
            "package sample\n\nimport \"fmt\"\n\nfunc f() {\n\tunknownFn()\n\tfmt.Println(\"hi\")\n}\n",
        );
        let f = g.node_by_qualified("sample::f").unwrap();
        // Both targets are external; the edge kind is Unresolved.
        assert!(g
            .edges()
            .filter(|(from, _, _)| *from == f)
            .all(|(_, to, e)| g.node(to).kind == NodeKind::External
                && e.kind == EdgeKind::Unresolved));
    }
}
