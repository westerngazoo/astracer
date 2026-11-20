import { strict as assert } from "assert";
import { test } from "node:test";

import {
  analyzeArgs,
  callersTitle,
  isRequest,
  languagesArgs,
  parseLocation,
  parseProgressLine,
  revealTarget,
  snippetArgs,
  symbolAt,
  FileSymbol,
  CANCELLED,
  confirmLoadText,
  EchoGuard,
  focusMessage,
  RevealParams,
  graphInfoArgs,
  languageForFile,
  pickLanguage,
  stageFraction,
} from "../src/protocol";

test("focus messages carry an absolute file and an optional 1-based line", () => {
  assert.deepEqual(focusMessage("/r/src/a.rs", 12), { type: "focus", file: "/r/src/a.rs", line: 12 });
  assert.deepEqual(focusMessage("/r/src/a.rs"), { type: "focus", file: "/r/src/a.rs" });
  assert.deepEqual(focusMessage("/r/a.rs", 0), { type: "focus", file: "/r/a.rs" });
  assert.deepEqual(focusMessage("/r/a.rs", 2.5), { type: "focus", file: "/r/a.rs" });
  assert.throws(() => focusMessage(""));
  // The webview's FocusRequest (transport.rs) decodes exactly these keys.
  assert.deepEqual(Object.keys(JSON.parse(JSON.stringify(focusMessage("/f", 3)))).sort(), ["file", "line", "type"]);
  assert.equal(CANCELLED, "cancelled");
});

test("focus messages can name the live definition under the cursor", () => {
  const m = focusMessage("/r/a.rs", 53, { name: "quality", containers: ["impl Rating"], line: 52 });
  assert.deepEqual(m.symbol, { name: "quality", containers: ["impl Rating"], line: 52 });
  assert.equal(focusMessage("/r/a.rs", 53, { name: "", containers: [], line: 1 }).symbol, undefined);
});

test("languages map from file extensions like lcw's discovery", () => {
  assert.equal(languageForFile("/r/src/main.rs"), "rust");
  assert.equal(languageForFile("a/b.cpp"), "cpp");
  assert.equal(languageForFile("a/b.h"), "cpp");
  assert.equal(languageForFile("a/b.HPP"), "cpp");
  assert.equal(languageForFile("x.ts"), "typescript");
  assert.equal(languageForFile("x.tsx"), "typescript");
  assert.equal(languageForFile("x.py"), "python");
  assert.equal(languageForFile("x.go"), "go");
  assert.equal(languageForFile("README.md"), undefined);
  assert.equal(languageForFile("Makefile"), undefined);
  assert.equal(languageForFile("/a.dir/Makefile"), undefined);
});

test("the clicked file's language is picked when several are present", () => {
  const both = [
    { id: "rust", label: "Rust" },
    { id: "cpp", label: "C++" },
  ];
  assert.equal(pickLanguage([{ id: "go", label: "Go" }], "/x.rs"), "go");
  assert.equal(pickLanguage(both, "/r/native.cpp"), "cpp");
  assert.equal(pickLanguage(both, "/r/src/lib.rs"), "rust");
  assert.equal(pickLanguage(both, "/r/x.py"), undefined);
  assert.equal(pickLanguage(both), undefined);
});

test("stage fractions rise through the analyze phases", () => {
  const order = ["config", "discover", "read", "parse", "analyze", "advise", "graph", "layout", "done"];
  const fs = order.map((phase) => stageFraction({ phase, message: "" }) ?? -1);
  for (let i = 1; i < fs.length; i++) {
    assert.ok(fs[i] > fs[i - 1], `${order[i]} after ${order[i - 1]}`);
  }
  assert.equal(stageFraction({ phase: "done", message: "" }), 1);
  assert.ok(
    stageFraction({ phase: "parse", message: "parsed 3 nodes / 2 edges" })! >
      stageFraction({ phase: "parse", message: "parsing 4 files" })!,
  );
  assert.equal(stageFraction({ phase: "mystery", message: "" }), undefined);
});

test("graph info args and the confirmation text", () => {
  assert.deepEqual(graphInfoArgs("/r", "cpp"), ["graph", "--json", "--language", "cpp", "--", "/r"]);
  assert.deepEqual(graphInfoArgs("/r"), ["graph", "--json", "--", "/r"]);
  assert.match(confirmLoadText("/r", "rust"), /^Load project graph for \/r \(rust\)\? This may take/);
  assert.match(confirmLoadText("/r", "rust", { exists: true, fresh: true, nodes: 10 }), /cached, instant, 10 nodes/);
  assert.match(confirmLoadText("/r", undefined, { exists: true, fresh: false }), /re-parsed/);
});

test("isRequest accepts known methods only", () => {
  assert.ok(isRequest({ type: "request", id: 1, method: "analyze", params: {} }));
  assert.ok(isRequest({ type: "request", id: 2, method: "reveal" }));
  assert.ok(!isRequest({ type: "request", id: 3, method: "rm -rf" }));
  assert.ok(!isRequest({ type: "response", id: 1, method: "analyze" }));
  assert.ok(!isRequest({ type: "request", id: "1", method: "analyze" }));
  assert.ok(!isRequest(null));
});

test("analyze maps the AnalyzeArgs DTO onto lcw view-data", () => {
  assert.deepEqual(analyzeArgs({ path: "/r", semantic: false, useCache: false }, "/ws"), [
    "view-data",
    "--json",
    "--progress",
    "--",
    "/r",
  ]);
  assert.deepEqual(
    analyzeArgs({ path: "  ", semantic: true, useCache: true, language: "cpp" }, "/ws"),
    ["view-data", "--json", "--progress", "--cached", "--semantic", "--language", "cpp", "--", "/ws"],
  );
  assert.deepEqual(
    analyzeArgs({ path: "", semantic: false, useCache: false, language: null }, "/ws").slice(-2),
    ["--", "/ws"],
  );
  assert.throws(() => analyzeArgs({ path: "", semantic: false, useCache: false }, ""), /no repository/);
});

test("discoverLanguages defaults to the workspace folder", () => {
  assert.deepEqual(languagesArgs({ path: "" }, "/ws"), ["languages", "--json", "--", "/ws"]);
  assert.deepEqual(languagesArgs({ path: "/r" }, "/ws"), ["languages", "--json", "--", "/r"]);
});

test("readSourceSnippet maps onto lcw snippet", () => {
  assert.deepEqual(
    snippetArgs({ file: "src/a.rs", startLine: 3, endLine: 9, repoRoot: "/r", wholeFile: false }),
    ["snippet", "--json", "--repo-root", "/r", "--", "src/a.rs", "3", "9"],
  );
  assert.deepEqual(
    snippetArgs({ file: "-weird.rs", startLine: 1, endLine: 1, wholeFile: true }),
    ["snippet", "--json", "--repo-root", "", "--whole-file", "--", "-weird.rs", "1", "1"],
  );
  assert.throws(() => snippetArgs({ file: "a.rs", startLine: -1, endLine: 2 }), /startLine/);
  assert.throws(
    () => snippetArgs({ file: 3 as unknown as string, startLine: 1, endLine: 2 }),
    /file must be a string/,
  );
});

test("progress lines are JSON objects with phase and message", () => {
  assert.deepEqual(parseProgressLine('{"phase":"parse","message":"parsing 3 files"}\n'), {
    phase: "parse",
    message: "parsing 3 files",
  });
  assert.equal(parseProgressLine("note: cached graph may be stale"), undefined);
  assert.equal(parseProgressLine("{not json"), undefined);
  assert.equal(parseProgressLine('{"phase":1}'), undefined);
});

test("locations split on the last colon", () => {
  assert.deepEqual(parseLocation("src/lib.rs:42"), { file: "src/lib.rs", line: 42 });
  assert.deepEqual(parseLocation("C:/repo/a.rs:7"), { file: "C:/repo/a.rs", line: 7 });
  assert.equal(parseLocation("<external>"), undefined);
  assert.equal(parseLocation("a.rs:0"), undefined);
});

test("reveal targets are clamped to a valid 1-based range", () => {
  assert.deepEqual(revealTarget({ file: "a.rs", line: 10, endLine: 4, repoRoot: "/r" }), {
    file: "a.rs",
    repoRoot: "/r",
    startLine: 10,
    endLine: 10,
    preserveFocus: false,
    sync: false,
    live: false,
  });
  assert.deepEqual(revealTarget({ file: "/abs/a.rs", line: 0, endLine: 3 }), {
    file: "/abs/a.rs",
    repoRoot: "",
    startLine: 1,
    endLine: 3,
    preserveFocus: false,
    sync: false,
    live: false,
  });
  assert.throws(() => revealTarget({ file: "", line: 1, endLine: 1 }), /no source file/);
});

test("sync reveals from the webview keep their flags", () => {
  // The webview's RevealArgs (transport.rs) serializes camelCase booleans.
  const t = revealTarget({ file: "a.rs", line: 3, endLine: 9, preserveFocus: true, sync: true });
  assert.equal(t.preserveFocus, true);
  assert.equal(t.sync, true);
  const loose = { file: "a.rs", line: 3, endLine: 9, preserveFocus: "yes", sync: 1 } as unknown as RevealParams;
  assert.equal(revealTarget(loose).preserveFocus, false);
  assert.equal(revealTarget(loose).sync, false);
});

test("the echo guard recognizes cursor events caused by the panel's own reveals", () => {
  const g = new EchoGuard(1000);
  assert.equal(g.isEcho("/r/a.rs", 5, 0), false);
  assert.equal(g.revealedSince(0), false);
  g.revealing("/r/a.rs", 10, 20, 5000);
  assert.equal(g.isEcho("/r/a.rs", 10, 5100), true);
  assert.equal(g.isEcho("/r/a.rs", 20, 5900), true);
  assert.equal(g.isEcho("/r/a.rs", 21, 5100), false, "outside the revealed span");
  assert.equal(g.isEcho("/r/b.rs", 10, 5100), false, "another file");
  assert.equal(g.isEcho("/r/a.rs", 10, 6001), false, "after the window");
  assert.equal(g.revealedSince(4900), true, "a follow scheduled before the reveal is stale");
  assert.equal(g.revealedSince(5001), false);
  g.revealing("/r/a.rs", 8, 3, 7000);
  assert.equal(g.isEcho("/r/a.rs", 8, 7000), true, "end before start collapses to one line");
  assert.equal(g.isEcho("/r/a.rs", 3, 7000), false);
});

test("symbolAt picks the innermost enclosing definition", () => {
  const sym = (qualified_name: string, start_line: number, end_line: number): FileSymbol => ({
    qualified_name,
    name: qualified_name,
    kind: "function",
    start_line,
    end_line,
    fan_in: 0,
    fan_out: 0,
  });
  const syms = [sym("outer", 1, 30), sym("inner", 10, 12), sym("other", 40, 50)];
  assert.equal(symbolAt(syms, 11)?.qualified_name, "inner");
  assert.equal(symbolAt(syms, 5)?.qualified_name, "outer");
  assert.equal(symbolAt(syms, 35), undefined);
  assert.equal(callersTitle(1), "1 caller");
  assert.equal(callersTitle(0), "0 callers");
});
