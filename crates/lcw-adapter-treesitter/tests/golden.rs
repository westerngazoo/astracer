//! Golden (snapshot) test for graph extraction. The fixture exercises free
//! functions, methods in an `impl`, a nested module, a macro call, an
//! unresolved external call, and control flow (for complexity counting).
//!
//! Run `cargo insta review` (or `INSTA_UPDATE=always cargo test`) to update.

use lcw_adapter_treesitter::RustTreeSitterAdapter;
use lcw_core::{LanguageAdapter, SourceFile};

const FIXTURE: &str = r#"
mod math {
    pub struct Calc { acc: i64 }

    impl Calc {
        pub fn new() -> Self { Calc { acc: 0 } }

        pub fn add(&mut self, x: i64) -> &mut Self {
            self.acc += x;
            self
        }

        pub fn classify(&self) -> i32 {
            if self.acc > 0 {
                1
            } else if self.acc < 0 {
                -1
            } else {
                0
            }
        }
    }

    pub fn sum(xs: &[i64]) -> i64 {
        let mut c = Calc::new();
        for x in xs {
            c.add(*x);
        }
        c.acc
    }
}

fn main() {
    let total = math::sum(&[1, 2, 3]);
    println!("{}", total);
}
"#;

#[test]
fn golden_graph_export() {
    let files = vec![SourceFile::new("src/lib.rs", FIXTURE)];
    let graph = RustTreeSitterAdapter::new().parse(&files).unwrap();
    insta::assert_json_snapshot!(graph.export());
}
