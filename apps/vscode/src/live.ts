import * as vscode from "vscode";

import { languageForFile } from "./protocol";
import { liveSymbolAt, LiveSymbol, relocate, Relocation } from "./relocate";

/** How long to wait for the language server's symbols before falling back to text search. */
const SYMBOLS_TIMEOUT_MS = 1500;

const FUNCTION_KINDS = new Set([
  vscode.SymbolKind.Function,
  vscode.SymbolKind.Method,
  vscode.SymbolKind.Constructor,
]);

function flatten(
  items: (vscode.DocumentSymbol | vscode.SymbolInformation)[],
  containers: string[],
  out: LiveSymbol[],
): void {
  for (const s of items) {
    const range = "range" in s ? s.range : s.location.range;
    const outer = "children" in s ? containers : s.containerName ? [s.containerName] : [];
    if (FUNCTION_KINDS.has(s.kind)) {
      out.push({ name: s.name, containers: outer, start: range.start.line + 1, end: range.end.line + 1 });
    }
    if ("children" in s && s.children.length) {
      flatten(s.children, [...containers, s.name], out);
    }
  }
}

/** Function-like document symbols of `doc` from its language server, or `undefined` when none answers in time. */
export async function documentSymbols(doc: vscode.TextDocument): Promise<LiveSymbol[] | undefined> {
  const query = Promise.resolve(
    vscode.commands.executeCommand<(vscode.DocumentSymbol | vscode.SymbolInformation)[] | undefined>(
      "vscode.executeDocumentSymbolProvider",
      doc.uri,
    ),
  ).catch(() => undefined);
  const timeout = new Promise<undefined>((r) => setTimeout(() => r(undefined), SYMBOLS_TIMEOUT_MS));
  const items = await Promise.race([query, timeout]);
  if (!items?.length) {
    return undefined;
  }
  const out: LiveSymbol[] = [];
  flatten(items, [], out);
  return out.length ? out : undefined;
}

function lines(doc: vscode.TextDocument): string[] {
  return doc.getText().split(/\r?\n/);
}

/** Where the graph's `qualifiedName` (analyzed at `start..=end`) is in the live `doc`. */
export async function relocateIn(
  doc: vscode.TextDocument,
  qualifiedName: string,
  start: number,
  end: number,
): Promise<Relocation> {
  const symbols = qualifiedName ? await documentSymbols(doc) : undefined;
  return relocate(symbols, lines(doc), languageForFile(doc.uri.fsPath), qualifiedName, start, end);
}

/** The live definition under (or nearest) `line` (1-based) in `doc`. */
export async function symbolAtLine(doc: vscode.TextDocument, line: number): Promise<LiveSymbol | undefined> {
  return liveSymbolAt(await documentSymbols(doc), lines(doc), languageForFile(doc.uri.fsPath), line);
}
