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
} from "../src/protocol";

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
  });
  assert.deepEqual(revealTarget({ file: "/abs/a.rs", line: 0, endLine: 3 }), {
    file: "/abs/a.rs",
    repoRoot: "",
    startLine: 1,
    endLine: 3,
  });
  assert.throws(() => revealTarget({ file: "", line: 1, endLine: 1 }), /no source file/);
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
