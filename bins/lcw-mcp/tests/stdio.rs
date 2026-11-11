//! End-to-end: spawn `lcw-mcp`, speak newline-delimited JSON-RPC over stdio
//! against a tiny fixture repo, and check the protocol and tool results.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::env::temp_dir().join(format!("lcw-mcp-test-{}-{nanos}", std::process::id()));
    let repo = base.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "/// Adds one.\npub fn inc(x: i32) -> i32 {\n    helper(x) + 1\n}\n\nfn helper(x: i32) -> i32 {\n    x\n}\n\npub fn twice(x: i32) -> i32 {\n    inc(inc(x))\n}\n",
    )
    .unwrap();
    (repo, base.join("cache"))
}

#[test]
fn initialize_list_and_call_tools_over_stdio() {
    let (repo, cache) = fixture();
    let repo_s = repo.to_string_lossy().into_owned();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lcw-mcp"))
        .env("LIVEWALK_CACHE_HOME", &cache)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lcw-mcp");

    let call = |id: u64, name: &str, args: Value| json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let msgs = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        call(3, "explain", json!({ "repo": repo_s, "symbol": "helper" })),
        call(
            4,
            "span",
            json!({ "repo": repo_s, "symbol": "inc", "code": true }),
        ),
        call(5, "impact", json!({ "repo": repo_s, "symbol": "helper" })),
        call(6, "explain", json!({ "repo": repo_s, "symbol": "helpr" })),
        json!({ "jsonrpc": "2.0", "id": 7, "method": "no/such" }),
    ];
    {
        let stdin = child.stdin.as_mut().unwrap();
        for m in &msgs {
            writeln!(stdin, "{m}").unwrap();
        }
    }
    drop(child.stdin.take());

    let replies: Vec<Value> = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    assert!(child.wait().unwrap().success());
    // The notification gets no reply.
    assert_eq!(replies.len(), 7, "{replies:#?}");
    let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap();
    let tool_json = |id: u64| -> (Value, bool) {
        let r = &by_id(id)["result"];
        let text = r["content"][0]["text"].as_str().unwrap();
        (
            serde_json::from_str(text).unwrap(),
            r["isError"].as_bool().unwrap(),
        )
    };

    let init = &by_id(1)["result"];
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "lcw-mcp");
    assert!(init["capabilities"]["tools"].is_object());

    let tools: Vec<&str> = by_id(2)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for want in [
        "explain",
        "callers",
        "callees",
        "impact",
        "span",
        "flow",
        "outline",
        "entries",
        "search_symbol",
    ] {
        assert!(tools.contains(&want), "missing tool {want}");
    }

    let (explain, err) = tool_json(3);
    assert!(!err);
    assert_eq!(explain["schema"], 1);
    assert!(explain["qualified_name"]
        .as_str()
        .unwrap()
        .ends_with("helper"));
    assert!(explain["callers"][0]["qualified_name"]
        .as_str()
        .unwrap()
        .ends_with("inc"));
    assert!(explain["at"].as_str().unwrap().ends_with("src/lib.rs:6"));

    let (span, _) = tool_json(4);
    assert_eq!(
        (span["start_line"].as_u64(), span["end_line"].as_u64()),
        (Some(1), Some(4))
    );
    assert_eq!(
        span["code"],
        "/// Adds one.\npub fn inc(x: i32) -> i32 {\n    helper(x) + 1\n}"
    );

    let (impact, _) = tool_json(5);
    assert_eq!(impact["total_callers"], 2);
    assert_eq!(impact["by_depth"], json!([1, 1]));

    let (missing, err) = tool_json(6);
    assert!(err);
    assert_eq!(missing["error"], "not_found");
    assert!(missing["candidates"][0]["qualified_name"]
        .as_str()
        .unwrap()
        .ends_with("helper"));

    assert_eq!(by_id(7)["error"]["code"], -32601);

    let _ = std::fs::remove_dir_all(repo.parent().unwrap());
}
