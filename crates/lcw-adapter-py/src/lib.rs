//! # lcw-adapter-py
//!
//! A Layer 1 [`LanguageAdapter`] for Python, backed by tree-sitter with
//! heuristic call resolution. It mirrors `lcw-adapter-treesitter` (the Rust
//! front end): fast and error-tolerant so it scales to giant repositories
//! (Manifesto: giant-repo performance target).

mod extract;

pub use extract::module_path_from;

use lcw_core::{AdapterError, CodeGraph, LanguageAdapter, SourceFile};

/// Python front end backed by tree-sitter.
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonTreeSitterAdapter;

impl PythonTreeSitterAdapter {
    pub fn new() -> Self {
        PythonTreeSitterAdapter
    }
}

impl LanguageAdapter for PythonTreeSitterAdapter {
    fn name(&self) -> &'static str {
        "treesitter-python"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }

    fn parse(&self, files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
        extract::parse_python(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{EdgeKind, NodeKind};

    fn parse(src: &str) -> CodeGraph {
        let files = vec![SourceFile::new("pkg/mod.py", src)];
        PythonTreeSitterAdapter::new().parse(&files).unwrap()
    }

    #[test]
    fn extracts_functions_and_direct_calls() {
        let g = parse(
            "def helper(x):\n    return x + 1\n\ndef main():\n    a = helper(1)\n    b = helper(a)\n",
        );
        let main = g.node_by_qualified("pkg::mod::main").expect("main present");
        let helper = g
            .node_by_qualified("pkg::mod::helper")
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
            "class Counter:\n    def __init__(self):\n        self.n = 0\n\n    def incr(self):\n        self.n += 1\n\n    def run(self):\n        self.incr()\n        self.incr()\n",
        );
        let run = g
            .node_by_qualified("pkg::mod::Counter::run")
            .expect("run present");
        let incr = g
            .node_by_qualified("pkg::mod::Counter::incr")
            .expect("incr present");
        assert!(g.node(incr).flags.is_method);
        let (_, _, e) = g.edges().find(|(f, t, _)| *f == run && *t == incr).unwrap();
        assert_eq!(e.kind, EdgeKind::MethodCall);
    }

    #[test]
    fn nested_defs_are_their_own_nodes() {
        let g = parse(
            "def outer():\n    def inner():\n        return helper()\n    return inner()\n\ndef helper():\n    return 1\n",
        );
        let outer = g.node_by_qualified("pkg::mod::outer").expect("outer");
        let inner = g
            .node_by_qualified("pkg::mod::outer::inner")
            .expect("inner");
        let helper = g.node_by_qualified("pkg::mod::helper").expect("helper");
        // The call to `helper` lives in `inner`, not `outer`.
        assert!(g.edges().any(|(f, t, _)| f == inner && t == helper));
        assert!(g.edges().any(|(f, t, _)| f == outer && t == inner));
        assert!(!g.edges().any(|(f, t, _)| f == outer && t == helper));
    }

    #[test]
    fn cyclomatic_complexity_counts_branches() {
        let g = parse(
            "def classify(x):\n    if x > 0:\n        if x > 10:\n            return 2\n        return 1\n    elif x < 0 and x > -5:\n        return -1\n    else:\n        return 0\n",
        );
        let f = g.node_by_qualified("pkg::mod::classify").unwrap();
        // if + inner if + elif + `and` = 4 decision points => CC 5.
        assert_eq!(g.node(f).cyclomatic_complexity(), 5);
    }

    #[test]
    fn unresolved_calls_become_external() {
        let g = parse("def f():\n    some_unknown_fn()\n    print(\"hi\")\n");
        let f = g.node_by_qualified("pkg::mod::f").unwrap();
        // Both targets are external; the edge kind is Unresolved.
        assert!(g
            .edges()
            .filter(|(from, _, _)| *from == f)
            .all(|(_, to, e)| g.node(to).kind == NodeKind::External
                && e.kind == EdgeKind::Unresolved));
    }
}
