/**
 * Live relocation: where a graph definition sits in the *current* text of a
 * file (an unsaved buffer included), since the graph's line numbers are
 * those of the last analyze. Pure (no `vscode` import) so it is unit-tested
 * in plain Node; `live.ts` feeds it the editor's document symbols and text.
 *
 * Order of trust: the language server's document symbols (matched by name,
 * then by the enclosing impl/class/module names), then a grep-like search of
 * the text for the definition (`fn name`, `def name`, …), then the graph's
 * own line. Duplicate names go to the candidate nearest the graph's line.
 */

/** A function-like definition in the live document. Lines are 1-based, inclusive. */
export interface LiveSymbol {
  name: string;
  /** Names of the enclosing symbols, outermost first (`impl Rating`, `tests`, …). */
  containers: string[];
  start: number;
  end: number;
}

export interface Relocation {
  start: number;
  end: number;
  /** What found it: the language server, the text search, or the graph as is. */
  how: "symbol" | "text" | "graph";
}

/** `a::b::Type::name` (or dotted) as its last segment and the segments before it. */
export function qualifiedParts(qualifiedName: string): { name: string; containers: string[] } {
  const segs = qualifiedName.split(/::|\./).filter((s) => s.length > 0);
  return { name: segs[segs.length - 1] ?? "", containers: segs.slice(0, -1) };
}

function identifiers(s: string): string[] {
  return s.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? [];
}

/** How many of a qualified name's container segments the symbol's containers mention. */
export function containerScore(qnContainers: string[], symbolContainers: string[]): number {
  const seen = new Set(symbolContainers.flatMap(identifiers));
  return qnContainers.filter((c) => seen.has(c)).length;
}

/** The candidate with the best container match, then the one nearest `near`. */
function best<T extends { start: number }>(cands: T[], near: number, score: (c: T) => number): T | undefined {
  let pick: T | undefined;
  let pickScore = -1;
  for (const c of cands) {
    const s = score(c);
    if (
      !pick ||
      s > pickScore ||
      (s === pickScore && Math.abs(c.start - near) < Math.abs(pick.start - near))
    ) {
      pick = c;
      pickScore = s;
    }
  }
  return pick;
}

/** The live symbol for `qualifiedName`, nearest `staleLine` among equally good matches. */
export function relocateBySymbols(
  symbols: LiveSymbol[],
  qualifiedName: string,
  staleLine: number,
): LiveSymbol | undefined {
  const { name, containers } = qualifiedParts(qualifiedName);
  if (!name) {
    return undefined;
  }
  return best(
    symbols.filter((s) => s.name === name),
    staleLine,
    (s) => containerScore(containers, s.containers),
  );
}

function escape(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** A line defining `name` in `language` (`lcw --language` ids), or `undefined` for unknown ones. */
export function definitionPattern(language: string | undefined, name: string): RegExp | undefined {
  const n = escape(name);
  switch (language) {
    case "rust":
      return new RegExp(`\\bfn\\s+${n}\\b`);
    case "python":
      return new RegExp(`^\\s*(?:async\\s+)?def\\s+${n}\\s*\\(`);
    case "go":
      return new RegExp(`^\\s*func\\s+(?:\\([^)]*\\)\\s*)?${n}\\s*[[(]`);
    case "typescript":
      return new RegExp(
        `(?:\\bfunction\\s*\\*?\\s*${n}\\b)|(?:\\b${n}\\s*[:=]\\s*(?:async\\s*)?(?:function\\b|\\([^)]*\\)\\s*(?::[^=]+)?=>))|` +
          `(?:^\\s*(?:(?:public|private|protected|static|async|readonly|override|get|set)\\s+)*\\*?${n}\\s*(?:<[^>]*>)?\\s*\\([^;]*$)`,
      );
    case "cpp":
      return new RegExp(`^(?!\\s*(?:return|if|while|for|switch|else|case)\\b)[^;=]*(?:^|[\\s*&:~])${n}\\s*\\([^;]*$`);
    default:
      return undefined;
  }
}

/** Grep the live lines for `qualifiedName`'s definition, nearest `staleStart`; keeps the span's length. */
export function relocateByText(
  lines: string[],
  language: string | undefined,
  qualifiedName: string,
  staleStart: number,
  staleEnd: number,
): { start: number; end: number } | undefined {
  const { name } = qualifiedParts(qualifiedName);
  const re = name ? definitionPattern(language, name) : undefined;
  if (!re) {
    return undefined;
  }
  const hits: { start: number }[] = [];
  lines.forEach((text, i) => {
    if (re.test(text)) {
      hits.push({ start: i + 1 });
    }
  });
  const hit = best(hits, staleStart, () => 0);
  if (!hit) {
    return undefined;
  }
  const len = Math.max(0, staleEnd - staleStart);
  return { start: hit.start, end: Math.min(lines.length, hit.start + len) };
}

/** Where the graph's `qualifiedName` (at `staleStart..=staleEnd`) is now. */
export function relocate(
  symbols: LiveSymbol[] | undefined,
  lines: string[],
  language: string | undefined,
  qualifiedName: string,
  staleStart: number,
  staleEnd: number,
): Relocation {
  const sym = symbols && qualifiedName ? relocateBySymbols(symbols, qualifiedName, staleStart) : undefined;
  if (sym) {
    return { start: sym.start, end: Math.max(sym.start, sym.end), how: "symbol" };
  }
  const text = qualifiedName ? relocateByText(lines, language, qualifiedName, staleStart, staleEnd) : undefined;
  if (text) {
    return { ...text, how: "text" };
  }
  return { start: staleStart, end: Math.max(staleStart, staleEnd), how: "graph" };
}

/**
 * The live symbol a cursor on `line` belongs to: the innermost containing
 * it, else the nearest (ties go to the one below, as in `lcw-query`'s
 * `nearest_at`).
 */
export function symbolNear(symbols: LiveSymbol[], line: number): LiveSymbol | undefined {
  let inner: LiveSymbol | undefined;
  for (const s of symbols) {
    if (s.start <= line && line <= s.end && (!inner || s.end - s.start < inner.end - inner.start)) {
      inner = s;
    }
  }
  if (inner) {
    return inner;
  }
  let near: LiveSymbol | undefined;
  let key = [Infinity, Infinity];
  for (const s of symbols) {
    const k = line < s.start ? [s.start - line, 0] : [line - s.end, 1];
    if (k[0] < key[0] || (k[0] === key[0] && k[1] < key[1])) {
      near = s;
      key = k;
    }
  }
  return near;
}

/** Any definition line in `language`, capturing its name. */
function anyDefinition(language: string | undefined): RegExp | undefined {
  switch (language) {
    case "rust":
      return /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)/;
    case "python":
      return /^\s*(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(/;
    case "go":
      return /^\s*func\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*[[(]/;
    case "typescript":
      return /\bfunction\s*\*?\s*([A-Za-z_$][A-Za-z0-9_$]*)/;
    default:
      return undefined;
  }
}

/**
 * Text fallback for [`symbolNear`]: the nearest definition at or above
 * `line`, else the first below it. No end line or containers are known.
 */
export function definitionNearByText(
  lines: string[],
  language: string | undefined,
  line: number,
): LiveSymbol | undefined {
  const re = anyDefinition(language);
  if (!re) {
    return undefined;
  }
  const at = (i: number): LiveSymbol | undefined => {
    const m = re.exec(lines[i] ?? "");
    return m ? { name: m[1], containers: [], start: i + 1, end: i + 1 } : undefined;
  };
  for (let i = Math.min(line, lines.length) - 1; i >= 0; i--) {
    const s = at(i);
    if (s) {
      return s;
    }
  }
  for (let i = line; i < lines.length; i++) {
    const s = at(i);
    if (s) {
      return s;
    }
  }
  return undefined;
}

/** The live definition a cursor on `line` (1-based) belongs to: symbols first, then text. */
export function liveSymbolAt(
  symbols: LiveSymbol[] | undefined,
  lines: string[],
  language: string | undefined,
  line: number,
): LiveSymbol | undefined {
  return (symbols?.length ? symbolNear(symbols, line) : undefined) ?? definitionNearByText(lines, language, line);
}

/** Lines `lcw snippet` would return around a span, also keeping `cursor` in view. */
export const SNIPPET_CONTEXT = 5;
export const SNIPPET_MAX_LINES = 500;

/** The 1-based inclusive window of a snippet: whole files up to the cap, else the span (+cursor) with context. */
export function snippetWindow(
  lineCount: number,
  hlStart: number,
  hlEnd: number,
  cursor: number | undefined,
  wholeFile: boolean,
): { lo: number; hi: number } {
  if (lineCount <= 0) {
    return { lo: 1, hi: 0 };
  }
  if (wholeFile || lineCount <= SNIPPET_MAX_LINES) {
    return { lo: 1, hi: Math.min(lineCount, SNIPPET_MAX_LINES) };
  }
  let lo = Math.max(1, Math.min(hlStart, cursor ?? hlStart) - SNIPPET_CONTEXT);
  let hi = Math.min(lineCount, Math.max(hlEnd, cursor ?? hlEnd) + SNIPPET_CONTEXT);
  if (hi - lo + 1 > SNIPPET_MAX_LINES) {
    lo = Math.max(1, Math.min(hlStart, cursor ?? hlStart));
    hi = Math.min(lineCount, lo + SNIPPET_MAX_LINES - 1);
  }
  return { lo, hi };
}
