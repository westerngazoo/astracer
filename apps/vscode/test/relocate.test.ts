import { strict as assert } from "assert";
import { test } from "node:test";

import {
  containerScore,
  definitionNearByText,
  definitionPattern,
  liveSymbolAt,
  LiveSymbol,
  qualifiedParts,
  relocate,
  relocateBySymbols,
  relocateByText,
  snippetWindow,
  symbolNear,
} from "../src/relocate";

const sym = (name: string, start: number, end: number, containers: string[] = []): LiveSymbol => ({
  name,
  containers,
  start,
  end,
});

/** A Rust file whose `quality` moved 12 lines down since the analyze (it was at 40..=47). */
const SHIFTED = [
  ...Array.from({ length: 51 }, (_, i) => `// added line ${i + 1}`),
  "    fn quality(self) -> f64 {", // 52
  "        0.5",
  "    }", // 54
  "",
  "pub fn graded(prev: Option<&Srs>) -> Srs {", // 56
  "    todo!()",
  "}", // 58
];

test("qualified names split into the name and its containers", () => {
  assert.deepEqual(qualifiedParts("app::review::Rating::quality"), {
    name: "quality",
    containers: ["app", "review", "Rating"],
  });
  assert.deepEqual(qualifiedParts("pkg.mod.run"), { name: "run", containers: ["pkg", "mod"] });
  assert.equal(containerScore(["app", "Rating"], ["impl Rating"]), 1);
  assert.equal(containerScore(["app", "Rating"], ["impl Display for Other"]), 0);
});

test("relocation prefers the language server's symbol", () => {
  const symbols = [sym("quality", 52, 54, ["impl Rating"]), sym("graded", 56, 58)];
  assert.deepEqual(relocate(symbols, SHIFTED, "rust", "app::review::Rating::quality", 40, 47), {
    start: 52,
    end: 54,
    how: "symbol",
  });
});

test("duplicate names go to the matching impl, then to the one nearest the stale line", () => {
  const symbols = [
    sym("new", 10, 14, ["impl Srs"]),
    sym("new", 30, 33, ["impl Rating"]),
    sym("new", 90, 93, ["impl Rating"]),
  ];
  assert.equal(relocateBySymbols(symbols, "app::Rating::new", 80)?.start, 90);
  assert.equal(relocateBySymbols(symbols, "app::Rating::new", 35)?.start, 30);
  assert.equal(relocateBySymbols(symbols, "app::Srs::new", 88)?.start, 10);
  assert.equal(relocateBySymbols(symbols, "app::Other::missing", 10), undefined);
});

test("without symbols, a grep for the definition finds it and keeps the span's length", () => {
  assert.deepEqual(relocate(undefined, SHIFTED, "rust", "app::review::Rating::quality", 40, 42), {
    start: 52,
    end: 54,
    how: "text",
  });
  // A call to `quality()` is not its definition.
  assert.equal(relocateByText(["let q = rating.quality();"], "rust", "x::quality", 1, 1), undefined);
});

test("the grep picks the duplicate nearest the stale line", () => {
  const lines = ["def run():", "  pass", "", "class B:", "  def run(self):", "    pass"];
  assert.equal(relocateByText(lines, "python", "m.A.run", 1, 2)?.start, 1);
  assert.equal(relocateByText(lines, "python", "m.B.run", 6, 7)?.start, 5);
});

test("when nothing matches, the graph's line stands", () => {
  assert.deepEqual(relocate([], ["nothing here"], "rust", "a::gone", 7, 3), { start: 7, end: 7, how: "graph" });
  assert.deepEqual(relocate(undefined, ["x"], undefined, "a::f", 2, 4), { start: 2, end: 4, how: "graph" });
});

test("definition patterns cover each fast-mode language", () => {
  assert.ok(definitionPattern("rust", "parse")!.test("pub(crate) async fn parse<T>(x: T) {"));
  assert.ok(definitionPattern("go", "Run")!.test("func (s *Server) Run(ctx context.Context) error {"));
  assert.ok(definitionPattern("typescript", "load")!.test("export async function load(path: string) {"));
  assert.ok(definitionPattern("typescript", "load")!.test("  private async load(path: string): Promise<void> {"));
  assert.ok(definitionPattern("typescript", "load")!.test("const load = async (p: string) => {"));
  assert.ok(definitionPattern("cpp", "parse")!.test("bool Parser::parse(const std::string& s) {"));
  assert.ok(!definitionPattern("cpp", "parse")!.test("  return parse(s);"));
  assert.ok(!definitionPattern("cpp", "parse")!.test("bool parse(const std::string& s);"));
  assert.equal(definitionPattern(undefined, "x"), undefined);
});

test("the cursor maps to the enclosing live symbol after lines shift", () => {
  const symbols = [sym("quality", 52, 54, ["impl Rating"]), sym("graded", 56, 58)];
  // Line 53 was inside `graded`'s old range, but the live symbols say `quality`.
  assert.equal(liveSymbolAt(symbols, SHIFTED, "rust", 53)?.name, "quality");
  assert.deepEqual(liveSymbolAt(symbols, SHIFTED, "rust", 53)?.containers, ["impl Rating"]);
  // Between functions: the nearest, the one below on a tie.
  assert.equal(symbolNear(symbols, 55)?.name, "graded");
  assert.equal(symbolNear(symbols, 20)?.name, "quality");
});

test("without symbols, the cursor maps to the definition above it by text", () => {
  assert.equal(liveSymbolAt(undefined, SHIFTED, "rust", 53)?.name, "quality");
  assert.equal(liveSymbolAt(undefined, SHIFTED, "rust", 57)?.name, "graded");
  assert.equal(definitionNearByText(SHIFTED, "rust", 3)?.name, "quality", "above every definition: the first below");
  assert.equal(definitionNearByText(SHIFTED, "cpp", 3), undefined);
});

test("snippet windows match lcw's and keep the cursor line", () => {
  assert.deepEqual(snippetWindow(120, 40, 50, undefined, false), { lo: 1, hi: 120 });
  assert.deepEqual(snippetWindow(2000, 100, 110, undefined, false), { lo: 95, hi: 115 });
  assert.deepEqual(snippetWindow(2000, 100, 110, 140, false), { lo: 95, hi: 145 });
  assert.deepEqual(snippetWindow(2000, 100, 110, undefined, true), { lo: 1, hi: 500 });
  assert.deepEqual(snippetWindow(2000, 100, 900, undefined, false), { lo: 100, hi: 599 });
});
