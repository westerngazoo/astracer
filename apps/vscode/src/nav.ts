import * as path from "path";
import * as vscode from "vscode";

import { candidatesFrom, Lcw } from "./lcw";
import { callersTitle, FileSymbol, parseLocation, symbolAt } from "./protocol";

/** The repository `lcw` analyzes for `uri`: its workspace folder (else the first one). */
export function repoRootFor(uri?: vscode.Uri): string | undefined {
  const folder =
    (uri && vscode.workspace.getWorkspaceFolder(uri)) || vscode.workspace.workspaceFolders?.[0];
  return folder?.uri.fsPath;
}

/** Absolute path for a graph file (absolute, or relative to `root`). */
export function resolveFile(root: string, file: string): string {
  return path.isAbsolute(file) ? file : path.join(root, file);
}

/** Open `file` and select lines `start..=end` (1-based). */
export async function openRange(
  file: string,
  start: number,
  end: number = start,
  column: vscode.ViewColumn = vscode.ViewColumn.Active,
): Promise<void> {
  const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
  const last = Math.min(Math.max(end, start), doc.lineCount) - 1;
  const first = Math.min(Math.max(start, 1), doc.lineCount) - 1;
  const range = new vscode.Range(first, 0, last, doc.lineAt(last).text.length);
  const editor = await vscode.window.showTextDocument(doc, {
    viewColumn: column,
    selection: new vscode.Selection(range.start, range.end),
    preview: false,
  });
  editor.revealRange(range, vscode.TextEditorRevealType.InCenterIfOutsideViewport);
}

/** Open a schema-1 `"file:line"` location under `root`. */
export async function openLocation(root: string, at: string | undefined): Promise<void> {
  const loc = at ? parseLocation(at) : undefined;
  if (!loc) {
    void vscode.window.showWarningMessage(`Livewalk: no source location (${at ?? "external"})`);
    return;
  }
  await openRange(resolveFile(root, loc.file), loc.line);
}

interface SymbolsJson {
  file: string;
  symbols: FileSymbol[];
}

/**
 * Per-file `lcw symbols` results. Keyed by path only and cleared on save: the
 * graph describes the file on disk, so unsaved edits must not trigger a query
 * per keystroke.
 */
export class SymbolCache {
  private readonly entries = new Map<string, Promise<FileSymbol[]>>();

  constructor(private readonly lcw: Lcw) {}

  get(doc: vscode.TextDocument): Promise<FileSymbol[]> {
    const root = repoRootFor(doc.uri);
    if (!root || doc.uri.scheme !== "file") {
      return Promise.resolve([]);
    }
    const key = doc.uri.fsPath;
    const hit = this.entries.get(key);
    if (hit) {
      return hit;
    }
    const value = this.lcw
      .json<SymbolsJson>(["symbols", "--json", "--path", root, "--", doc.uri.fsPath], { cwd: root })
      .then((v) => v.symbols ?? []);
    value.catch(() => this.entries.delete(key));
    this.entries.set(key, value);
    return value;
  }

  clear(): void {
    this.entries.clear();
  }
}

/** The function under the cursor, via `lcw symbols`. */
export async function symbolAtCursor(
  cache: SymbolCache,
): Promise<{ symbol: FileSymbol; root: string } | undefined> {
  const editor = vscode.window.activeTextEditor;
  const root = editor && repoRootFor(editor.document.uri);
  if (!editor || !root) {
    void vscode.window.showWarningMessage("Livewalk: open a source file in a workspace folder first");
    return undefined;
  }
  const symbols = await cache.get(editor.document);
  const symbol = symbolAt(symbols, editor.selection.active.line + 1);
  if (!symbol) {
    void vscode.window.showInformationMessage(
      "Livewalk: no function at the cursor in the analyzed graph (save the file, or check the language)",
    );
    return undefined;
  }
  return { symbol, root };
}

interface TreeNode {
  qualified_name: string;
  at?: string;
  site?: string;
  sites?: { file: string; line: number }[];
  children?: TreeNode[];
}

interface LocItem extends vscode.QuickPickItem {
  at?: string;
}

async function pick(title: string, items: LocItem[], root: string): Promise<void> {
  if (items.length === 0) {
    void vscode.window.showInformationMessage(`Livewalk: ${title} — none`);
    return;
  }
  const chosen = await vscode.window.showQuickPick(items, {
    title,
    matchOnDescription: true,
    matchOnDetail: true,
  });
  if (chosen?.at) {
    await openLocation(root, chosen.at);
  }
}

function reportLookup(e: unknown, symbol: string): boolean {
  const c = candidatesFrom(e);
  if (c.length === 0) {
    return false;
  }
  void vscode.window.showWarningMessage(
    `Livewalk: "${symbol}" is ambiguous or unknown. Candidates: ${c.slice(0, 5).join(", ")}`,
  );
  return true;
}

/** Quick pick of direct callers (`lcw callers --json`); picking one jumps to its call site. */
export async function showCallers(lcw: Lcw, root: string, symbol: string): Promise<void> {
  try {
    const v = await lcw.json<{ tree: TreeNode }>(
      ["callers", "--json", "--path", root, "--", symbol],
      { cwd: root },
    );
    const items: LocItem[] = [];
    for (const c of v.tree.children ?? []) {
      const sites = c.sites?.length ? c.sites.map((s) => `${s.file}:${s.line}`) : [c.site ?? c.at];
      for (const s of sites) {
        items.push({
          label: `$(call-incoming) ${c.qualified_name}`,
          description: s,
          detail: c.at ? `defined at ${c.at}` : undefined,
          at: s,
        });
      }
    }
    await pick(`${callersTitle(v.tree.children?.length ?? 0)} of ${symbol}`, items, root);
  } catch (e) {
    if (!reportLookup(e, symbol)) {
      void vscode.window.showErrorMessage(`Livewalk callers: ${(e as Error).message}`);
    }
  }
}

interface ImpactJson {
  total_callers: number;
  files: number;
  depth: number;
  entry_points?: { qualified_name: string; entry?: string; depth: number; at: string }[];
  callers?: { qualified_name: string; depth: number; at: string; site?: string; via: string }[];
  callers_more?: number;
}

/** Blast radius (`lcw impact --json`) as a quick pick of transitive callers. */
export async function showImpact(lcw: Lcw, root: string, symbol: string): Promise<void> {
  const depth = vscode.workspace.getConfiguration("livewalk").get<number>("impactDepth") ?? 3;
  try {
    const v = await lcw.json<ImpactJson>(
      ["impact", "--json", "--path", root, "--depth", String(depth), "--limit", "200", "--", symbol],
      { cwd: root },
    );
    const entries: LocItem[] = (v.entry_points ?? []).map((e) => ({
      label: `$(debug-start) ${e.qualified_name}`,
      description: `entry${e.entry ? ` (${e.entry})` : ""} · depth ${e.depth}`,
      detail: e.at,
      at: e.at,
    }));
    const callers: LocItem[] = (v.callers ?? []).map((c) => ({
      label: `${"  ".repeat(Math.max(0, c.depth - 1))}$(call-incoming) ${c.qualified_name}`,
      description: `depth ${c.depth} · via ${c.via}`,
      detail: c.site ?? c.at,
      at: c.site ?? c.at,
    }));
    const items: LocItem[] = [];
    if (entries.length) {
      items.push({ label: "Entry points reached", kind: vscode.QuickPickItemKind.Separator }, ...entries);
    }
    items.push({ label: "Callers", kind: vscode.QuickPickItemKind.Separator }, ...callers);
    const more = v.callers_more ? ` (+${v.callers_more} more)` : "";
    await pick(
      `Impact of ${symbol}: ${v.total_callers} callers in ${v.files} files within ${v.depth} hops${more}`,
      items,
      root,
    );
  } catch (e) {
    if (!reportLookup(e, symbol)) {
      void vscode.window.showErrorMessage(`Livewalk impact: ${(e as Error).message}`);
    }
  }
}

interface Hop {
  qualified_name: string;
  kind: string;
  at: string;
}

interface FlowJson {
  source?: string;
  target?: string;
  entry_kind?: string;
  test_only?: boolean;
  hops?: number;
  paths?: Hop[][];
  error?: string;
  message?: string;
}

/** `lcw flow --json` to the function under the cursor: hops in a quick pick (and the output channel). */
export async function showFlow(
  lcw: Lcw,
  output: vscode.OutputChannel,
  root: string,
  symbol: string,
): Promise<void> {
  let v: FlowJson;
  try {
    v = await lcw.json<FlowJson>(["flow", "--json", "--path", root, "--", symbol], { cwd: root });
  } catch (e) {
    if (!reportLookup(e, symbol)) {
      void vscode.window.showWarningMessage(`Livewalk flow: ${(e as Error).message}`);
    }
    return;
  }
  const paths = v.paths ?? [];
  if (paths.length === 0) {
    void vscode.window.showInformationMessage(
      `Livewalk: no entry point reaches ${symbol}${v.message ? ` — ${v.message}` : ""}`,
    );
    return;
  }
  output.appendLine(`flow to ${symbol} from ${v.source ?? "?"} (${v.entry_kind ?? "entry"})`);
  const items: LocItem[] = [];
  paths.forEach((p, i) => {
    if (paths.length > 1) {
      items.push({ label: `Path ${i + 1}`, kind: vscode.QuickPickItemKind.Separator });
    }
    p.forEach((hop, depth) => {
      output.appendLine(`${"  ".repeat(depth)}${depth ? "└▶ " : ""}${hop.qualified_name}  ${hop.at}`);
      items.push({
        label: `${"  ".repeat(depth)}${depth ? "$(arrow-small-right) " : "$(debug-start) "}${hop.qualified_name}`,
        description: hop.at,
        at: hop.at,
      });
    });
  });
  const tag = v.test_only ? " (only tests reach it)" : "";
  await pick(`Flow to ${symbol}: ${v.hops ?? 0} hops from ${v.source ?? "entry"}${tag}`, items, root);
}
