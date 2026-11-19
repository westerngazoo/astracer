/**
 * The webview <-> extension message protocol, and how each request maps onto
 * an `lcw` invocation. Pure (no `vscode` import) so it is unit-tested in
 * plain Node.
 *
 * Webview -> extension: `{ type: "request", id, method, params }`.
 * Extension -> webview: `{ type: "response", id, ok: true, result }` (or
 * `resultJson`, a JSON string the bridge parses, for the large graph view),
 * `{ type: "response", id, ok: false, error }` (`error` is exactly
 * [`CANCELLED`] when the user cancelled), `{ type: "progress", payload:
 * { phase, message } }`, and `{ type: "focus", file, line? }` (select the
 * function at `file:line`, or the file's first function without `line`).
 *
 * Param names follow the frontend's camelCase DTOs (`lcw-desktop-api`).
 */

export type Method =
  | "analyze"
  | "discoverLanguages"
  | "readSourceSnippet"
  | "explainFunction"
  | "pickRepoFolder"
  | "reveal";

export const METHODS: readonly Method[] = [
  "analyze",
  "discoverLanguages",
  "readSourceSnippet",
  "explainFunction",
  "pickRepoFolder",
  "reveal",
];

export interface RequestMessage {
  type: "request";
  id: number;
  method: Method;
  params: unknown;
}

export interface Progress {
  phase: string;
  message: string;
}

export interface FocusMessage {
  type: "focus";
  /** Absolute path of the editor file. */
  file: string;
  /** 1-based cursor line; absent to select the file's first function. */
  line?: number;
}

export type ExtensionMessage =
  | { type: "response"; id: number; ok: true; result?: unknown; resultJson?: string }
  | { type: "response"; id: number; ok: false; error: string }
  | { type: "progress"; payload: Progress }
  | FocusMessage;

/** The `error` of an analyze the user cancelled (the webview shows "Cancelled"). */
export const CANCELLED = "cancelled";

/** A `focus` message; `line` is 1-based and dropped unless a positive integer. */
export function focusMessage(file: string, line?: number): FocusMessage {
  if (!file) {
    throw new Error("focus needs a file");
  }
  return typeof line === "number" && Number.isInteger(line) && line > 0
    ? { type: "focus", file, line }
    : { type: "focus", file };
}

/**
 * How far along an analyze is at each `--progress` phase (0..1), in the order
 * `lcw-desktop-api` emits them; the webview overlay keeps the same table.
 */
const STAGES: Record<string, number> = {
  config: 0.05,
  load: 0.4,
  discover: 0.12,
  read: 0.22,
  parse: 0.4,
  analyze: 0.65,
  advise: 0.75,
  graph: 0.8,
  layout: 0.88,
  done: 1,
};

export function stageFraction(p: Progress): number | undefined {
  const f = STAGES[p.phase];
  if (f === undefined) {
    return undefined;
  }
  return p.phase === "parse" && p.message.startsWith("parsed") ? 0.55 : f;
}

/** Fast-mode language (`lcw --language`) of a source file, by extension. */
export function languageForFile(file: string): string | undefined {
  const m = /\.([^./\\]+)$/.exec(file);
  switch (m?.[1].toLowerCase()) {
    case "rs":
      return "rust";
    case "ts":
    case "tsx":
    case "mts":
    case "cts":
      return "typescript";
    case "py":
    case "pyi":
      return "python";
    case "go":
      return "go";
    case "cpp":
    case "cc":
    case "cxx":
    case "c++":
    case "h":
    case "hpp":
    case "hh":
    case "hxx":
      return "cpp";
    default:
      return undefined;
  }
}

export interface LanguageChoice {
  id: string;
  label: string;
}

/**
 * The language to load without asking: the only one present, else the clicked
 * file's when present. `undefined` means "ask" (a quick pick).
 */
export function pickLanguage(present: LanguageChoice[], file?: string): string | undefined {
  if (present.length === 1) {
    return present[0].id;
  }
  const own = file ? languageForFile(file) : undefined;
  return own && present.some((l) => l.id === own) ? own : undefined;
}

/** `lcw graph --json` arguments: the saved graph store of `root` for `language`. */
export function graphInfoArgs(root: string, language?: string): string[] {
  const args = ["graph", "--json"];
  if (language) {
    args.push("--language", language);
  }
  args.push("--", root);
  return args;
}

/** `lcw graph --json` output. */
export interface GraphInfo {
  exists: boolean;
  fresh: boolean;
  nodes?: number | null;
}

/** The "load this project?" prompt. */
export function confirmLoadText(root: string, language: string | undefined, info?: GraphInfo): string {
  const lang = language ? ` (${language})` : "";
  const base = `Load project graph for ${root}${lang}?`;
  if (info?.exists && info.fresh) {
    return `${base} A saved graph is up to date (cached, instant${info.nodes ? `, ${info.nodes} nodes` : ""}).`;
  }
  if (info?.exists) {
    return `${base} Sources changed since the saved graph, so it will be re-parsed. This may take a few seconds on large projects.`;
  }
  return `${base} This may take a few seconds on large projects.`;
}

export interface AnalyzeParams {
  path: string;
  semantic: boolean;
  useCache: boolean;
  language?: string | null;
}

export interface SnippetParams {
  file: string;
  startLine: number;
  endLine: number;
  repoRoot?: string;
  wholeFile?: boolean;
}

export interface RevealParams {
  file: string;
  line: number;
  endLine: number;
  repoRoot?: string;
  qualifiedName?: string;
}

export function isRequest(msg: unknown): msg is RequestMessage {
  if (typeof msg !== "object" || msg === null) {
    return false;
  }
  const m = msg as Record<string, unknown>;
  return (
    m.type === "request" &&
    typeof m.id === "number" &&
    typeof m.method === "string" &&
    (METHODS as readonly string[]).includes(m.method)
  );
}

function str(v: unknown, name: string): string {
  if (typeof v !== "string") {
    throw new Error(`${name} must be a string`);
  }
  return v;
}

function line(v: unknown, name: string): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0) {
    throw new Error(`${name} must be a non-negative integer`);
  }
  return v;
}

/** `lcw view-data` arguments for an `analyze` request. An empty path means the workspace folder. */
export function analyzeArgs(p: AnalyzeParams, workspaceRoot: string): string[] {
  const path = (typeof p.path === "string" ? p.path.trim() : "") || workspaceRoot;
  if (!path) {
    throw new Error("no repository path (open a folder or type one in the path box)");
  }
  const args = ["view-data", "--json", "--progress"];
  if (p.useCache) {
    args.push("--cached");
  }
  if (p.semantic) {
    args.push("--semantic");
  }
  if (typeof p.language === "string" && p.language.trim()) {
    args.push("--language", p.language.trim());
  }
  args.push("--", path);
  return args;
}

/** `lcw languages` arguments for a `discoverLanguages` request. */
export function languagesArgs(p: { path?: unknown }, workspaceRoot: string): string[] {
  const path = (typeof p.path === "string" ? p.path.trim() : "") || workspaceRoot;
  return ["languages", "--json", "--", path];
}

/** `lcw snippet` arguments for a `readSourceSnippet` request. */
export function snippetArgs(p: SnippetParams): string[] {
  const args = [
    "snippet",
    "--json",
    "--repo-root",
    typeof p.repoRoot === "string" ? p.repoRoot : "",
  ];
  if (p.wholeFile) {
    args.push("--whole-file");
  }
  args.push(
    "--",
    str(p.file, "file"),
    String(line(p.startLine, "startLine")),
    String(line(p.endLine, "endLine")),
  );
  return args;
}

/** One `--progress` stderr line from `lcw view-data`, or `undefined` for other output. */
export function parseProgressLine(text: string): Progress | undefined {
  const t = text.trim();
  if (!t.startsWith("{")) {
    return undefined;
  }
  try {
    const v = JSON.parse(t) as Record<string, unknown>;
    if (typeof v.phase === "string" && typeof v.message === "string") {
      return { phase: v.phase, message: v.message };
    }
  } catch {
    return undefined;
  }
  return undefined;
}

/** Split `"file:line"` (an `at` / `site` field of the schema-1 JSON). */
export function parseLocation(at: string): { file: string; line: number } | undefined {
  const i = at.lastIndexOf(":");
  if (i <= 0) {
    return undefined;
  }
  const n = Number(at.slice(i + 1));
  if (!Number.isInteger(n) || n <= 0) {
    return undefined;
  }
  return { file: at.slice(0, i), line: n };
}

/** Normalize a `reveal` request: 1-based lines, end never before start. */
export function revealTarget(p: RevealParams): {
  file: string;
  repoRoot: string;
  startLine: number;
  endLine: number;
} {
  const file = str(p.file, "file");
  if (!file) {
    throw new Error("this node has no source file");
  }
  const startLine = Math.max(1, line(p.line, "line"));
  const endLine = Math.max(startLine, typeof p.endLine === "number" ? p.endLine : startLine);
  return { file, repoRoot: typeof p.repoRoot === "string" ? p.repoRoot : "", startLine, endLine };
}

export interface FileSymbol {
  qualified_name: string;
  name: string;
  kind: string;
  start_line: number;
  end_line: number;
  fan_in: number;
  fan_out: number;
}

/** The innermost symbol whose span contains `line` (1-based). */
export function symbolAt(symbols: FileSymbol[], line: number): FileSymbol | undefined {
  let best: FileSymbol | undefined;
  for (const s of symbols) {
    if (s.start_line <= line && line <= s.end_line) {
      if (!best || s.end_line - s.start_line < best.end_line - best.start_line) {
        best = s;
      }
    }
  }
  return best;
}

/** CodeLens title for a symbol. */
export function callersTitle(fanIn: number): string {
  return fanIn === 1 ? "1 caller" : `${fanIn} callers`;
}
