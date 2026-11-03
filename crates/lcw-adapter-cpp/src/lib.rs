//! # lcw-adapter-cpp
//!
//! A Layer 1 [`LanguageAdapter`] for C++, backed by tree-sitter with heuristic
//! call resolution (fast mode), matching the other tree-sitter front ends.

mod extract;

pub use extract::module_path_from;

use lcw_core::{AdapterError, CodeGraph, LanguageAdapter, SourceFile};

/// C++ front end backed by tree-sitter.
#[derive(Debug, Default, Clone, Copy)]
pub struct CppTreeSitterAdapter;

impl CppTreeSitterAdapter {
    pub fn new() -> Self {
        CppTreeSitterAdapter
    }
}

impl LanguageAdapter for CppTreeSitterAdapter {
    fn name(&self) -> &'static str {
        "treesitter-cpp"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cpp", "cc", "cxx", "c++", "h", "hpp", "hh", "hxx"]
    }

    fn parse(&self, files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
        extract::parse_cpp(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{EdgeKind, NodeKind};

    fn parse(src: &str) -> CodeGraph {
        let files = vec![SourceFile::new("sample/main.cpp", src)];
        CppTreeSitterAdapter::new().parse(&files).unwrap()
    }

    #[test]
    fn extracts_functions_and_direct_calls() {
        let g = parse(
            "void helper(int x) { (void)x; }\n\nvoid run() {\n\thelper(1);\n\thelper(2);\n}\n",
        );
        let run = g.node_by_qualified("sample::main::run").expect("run present");
        let helper = g
            .node_by_qualified("sample::main::helper")
            .expect("helper present");
        assert_eq!(g.node(run).kind, NodeKind::Function);
        assert_eq!(g.node(helper).kind, NodeKind::Function);
        let outs: Vec<_> = g
            .edges_out(run)
            .map(|(to, e)| (g.node(to).qualified_name.clone(), e.kind))
            .collect();
        assert!(outs.len() >= 1);
        assert!(outs.iter().all(|(_, k)| *k == EdgeKind::DirectCall));
    }

    #[test]
    fn class_methods_are_qualified() {
        let g = parse(
            "class Counter {\npublic:\n\tvoid bump() {}\n\tvoid run() { bump(); }\n};\n",
        );
        let run = g
            .node_by_qualified("sample::main::Counter::run")
            .expect("run method");
        assert_eq!(g.node(run).kind, NodeKind::Method);
        assert!(g.edges_out(run).any(|(to, e)| {
            g.node(to).qualified_name.contains("bump")
                && (e.kind == EdgeKind::MethodCall || e.kind == EdgeKind::DirectCall)
        }));
    }
}
