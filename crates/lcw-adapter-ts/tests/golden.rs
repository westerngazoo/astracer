//! Golden (snapshot) test for graph extraction. The fixture exercises a free
//! function, an exported arrow-const, methods in a class (one calling another),
//! a `new` construction, an unresolved external call, and control flow (for
//! complexity counting).
//!
//! Run `cargo insta review` (or `INSTA_UPDATE=always cargo test`) to update.

use lcw_adapter_ts::TypeScriptTreeSitterAdapter;
use lcw_core::{LanguageAdapter, SourceFile};

const FIXTURE: &str = r#"
export const scale = (x: number): number => x * 2;

class Calc {
    acc: number = 0;

    add(x: number): Calc {
        this.acc += x;
        return this;
    }

    classify(): number {
        if (this.acc > 0) {
            return 1;
        } else if (this.acc < 0) {
            return -1;
        } else {
            return 0;
        }
    }

    static make(): Calc {
        return new Calc();
    }
}

function sum(xs: number[]): number {
    const c = Calc.make();
    for (const x of xs) {
        c.add(scale(x));
    }
    return c.acc;
}

function main(): void {
    const total = sum([1, 2, 3]);
    console.log(total);
}
"#;

#[test]
fn golden_graph_export() {
    let files = vec![SourceFile::new("src/app.ts", FIXTURE)];
    let graph = TypeScriptTreeSitterAdapter::new().parse(&files).unwrap();
    insta::assert_json_snapshot!(graph.export());
}
