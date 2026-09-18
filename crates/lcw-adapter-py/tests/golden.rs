//! Golden (snapshot) test for graph extraction. The fixture exercises a class
//! with methods (one calling another), a module-level function that constructs
//! and drives the class, a nested `def`, an unresolved external call, and
//! control flow (for complexity counting).
//!
//! Run `cargo insta review` (or `INSTA_UPDATE=always cargo test`) to update.

use lcw_adapter_py::PythonTreeSitterAdapter;
use lcw_core::{LanguageAdapter, SourceFile};

const FIXTURE: &str = r#"
class Calc:
    def __init__(self):
        self.acc = 0

    def add(self, x):
        self.acc += x
        return self

    def classify(self):
        if self.acc > 0:
            return 1
        elif self.acc < 0:
            return -1
        else:
            return 0


def total(xs):
    c = Calc()
    for x in xs:
        c.add(x)
    return c.acc


def main():
    result = total([1, 2, 3])
    print(result)
"#;

#[test]
fn golden_graph_export() {
    let files = vec![SourceFile::new("pkg/calc.py", FIXTURE)];
    let graph = PythonTreeSitterAdapter::new().parse(&files).unwrap();
    insta::assert_json_snapshot!(graph.export());
}
