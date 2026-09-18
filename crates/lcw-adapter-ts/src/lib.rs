//! # lcw-adapter-ts
//!
//! A Layer 1 [`LanguageAdapter`] for TypeScript, backed by tree-sitter with
//! heuristic call resolution. It mirrors `lcw-adapter-treesitter` (the Rust
//! front end): fast and error-tolerant so it scales to giant repositories
//! (Manifesto: giant-repo performance target).
//!
//! `.tsx` files are parsed with the TSX grammar (a superset that adds JSX);
//! every node kind this adapter walks is shared with plain TypeScript.

mod extract;

pub use extract::module_path_from;

use lcw_core::{AdapterError, CodeGraph, LanguageAdapter, SourceFile};

/// TypeScript front end backed by tree-sitter.
#[derive(Debug, Default, Clone, Copy)]
pub struct TypeScriptTreeSitterAdapter;

impl TypeScriptTreeSitterAdapter {
    pub fn new() -> Self {
        TypeScriptTreeSitterAdapter
    }
}

impl LanguageAdapter for TypeScriptTreeSitterAdapter {
    fn name(&self) -> &'static str {
        "treesitter-typescript"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "mts", "cts"]
    }

    fn parse(&self, files: &[SourceFile]) -> Result<CodeGraph, AdapterError> {
        extract::parse_typescript(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcw_core::{EdgeKind, NodeKind};

    fn parse(src: &str) -> CodeGraph {
        let files = vec![SourceFile::new("src/app.ts", src)];
        TypeScriptTreeSitterAdapter::new().parse(&files).unwrap()
    }

    #[test]
    fn extracts_functions_and_direct_calls() {
        let g = parse(
            r#"
            function helper(x: number): number { return x + 1; }
            function main() {
                const a = helper(1);
                const b = helper(a);
            }
            "#,
        );
        let main = g.node_by_qualified("app::main").expect("main present");
        let helper = g.node_by_qualified("app::helper").expect("helper present");
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
    fn arrow_consts_are_functions() {
        let g = parse(
            r#"
            export const add = (a: number, b: number): number => a + b;
            function use() { return add(1, 2); }
            "#,
        );
        let add = g.node_by_qualified("app::add").expect("add present");
        let use_fn = g.node_by_qualified("app::use").expect("use present");
        assert_eq!(g.node(add).kind, NodeKind::Function);
        // `export const` is public API.
        assert!(g.node(add).flags.is_pub);
        let (_, _, e) = g
            .edges()
            .find(|(f, t, _)| *f == use_fn && *t == add)
            .unwrap();
        assert_eq!(e.kind, EdgeKind::DirectCall);
    }

    #[test]
    fn class_methods_get_type_qualified_names() {
        let g = parse(
            r#"
            class Counter {
                n = 0;
                incr() { this.n += 1; }
                run() {
                    this.incr();
                    this.incr();
                }
            }
            "#,
        );
        let run = g
            .node_by_qualified("app::Counter::run")
            .expect("run present");
        let incr = g
            .node_by_qualified("app::Counter::incr")
            .expect("incr present");
        assert!(g.node(incr).flags.is_method);
        let (_, _, e) = g.edges().find(|(f, t, _)| *f == run && *t == incr).unwrap();
        assert_eq!(e.kind, EdgeKind::MethodCall);
    }

    #[test]
    fn cyclomatic_complexity_counts_branches() {
        let g = parse(
            r#"
            function classify(x: number): number {
                if (x > 0) {
                    return x > 10 ? 2 : 1;
                } else if (x < 0 && x > -5) {
                    return -1;
                } else {
                    return 0;
                }
            }
            "#,
        );
        let f = g.node_by_qualified("app::classify").unwrap();
        // if + else-if(if) + ternary + `&&` = 4 decision points => CC 5.
        assert_eq!(g.node(f).cyclomatic_complexity(), 5);
    }

    #[test]
    fn unresolved_calls_become_external() {
        let g = parse(
            r#"
            function f() {
                someUnknownFn();
                console.log("hi");
            }
            "#,
        );
        let f = g.node_by_qualified("app::f").unwrap();
        // Both targets are external; the edge kind is Unresolved.
        assert!(g
            .edges()
            .filter(|(from, _, _)| *from == f)
            .all(|(_, to, e)| g.node(to).kind == NodeKind::External
                && e.kind == EdgeKind::Unresolved));
    }
}
