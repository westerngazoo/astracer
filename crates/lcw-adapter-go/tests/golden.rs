//! Golden (snapshot) test for graph extraction. The fixture exercises a free
//! function, a struct type with pointer- and value-receiver methods (one method
//! calling another), a `main` that constructs and drives it, an unresolved
//! external call, and control flow (for complexity counting).
//!
//! Run `cargo insta review` (or `INSTA_UPDATE=always cargo test`) to update.

use lcw_adapter_go::GoTreeSitterAdapter;
use lcw_core::{LanguageAdapter, SourceFile};

const FIXTURE: &str = r#"
package sample

import "fmt"

func scale(x int) int {
	return x * 2
}

type Calc struct {
	acc int
}

func (c *Calc) Add(x int) *Calc {
	c.acc += scale(x)
	return c
}

func (c Calc) Classify() int {
	if c.acc > 0 {
		return 1
	} else if c.acc < 0 {
		return -1
	}
	return 0
}

func main() {
	c := &Calc{}
	c.Add(1)
	c.Add(2)
	fmt.Println(c.Classify())
}
"#;

#[test]
fn golden_graph_export() {
    let files = vec![SourceFile::new("sample/calc.go", FIXTURE)];
    let graph = GoTreeSitterAdapter::new().parse(&files).unwrap();
    insta::assert_json_snapshot!(graph.export());
}
