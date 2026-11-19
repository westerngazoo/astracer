import * as vscode from "vscode";

import { CallersCodeLens } from "./codelens";
import { Lcw } from "./lcw";
import { showCallers, showFlow, showImpact, SymbolCache, symbolAtCursor } from "./nav";
import { openGraphHere, revealInGraph } from "./openHere";
import { GraphPanel } from "./panel";

/** Cursor-follow delay: one focus per pause, not per keystroke or arrow press. */
const FOLLOW_DEBOUNCE_MS = 350;

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("Livewalk");
  const lcw = new Lcw(output);
  const symbols = new SymbolCache(lcw);
  const lens = new CallersCodeLens(symbols);

  const register = (id: string, fn: (...args: never[]) => unknown) =>
    context.subscriptions.push(vscode.commands.registerCommand(id, fn));

  const guarded = (p: Promise<void>) => p.catch(reportError);

  register("livewalk.openGraph", () => guarded(openGraphHere(context, lcw, undefined, "palette")));
  register("livewalk.openGraphHere", (uri?: vscode.Uri) =>
    guarded(openGraphHere(context, lcw, uri, "editor")),
  );
  register("livewalk.openGraphHereExplorer", (uri?: vscode.Uri) =>
    guarded(openGraphHere(context, lcw, uri, "explorer")),
  );
  register("livewalk.revealInGraph", () => guarded(revealInGraph(context, lcw)));

  register("livewalk.showCallers", (root: string, symbol: string) => showCallers(lcw, root, symbol));
  register("livewalk.showImpact", (root: string, symbol: string) => showImpact(lcw, root, symbol));

  register("livewalk.showCallersHere", async () => {
    const at = await symbolAtCursor(symbols).catch(reportError);
    if (at) {
      await showCallers(lcw, at.root, at.symbol.qualified_name);
    }
  });

  register("livewalk.showFlowToHere", async () => {
    const at = await symbolAtCursor(symbols).catch(reportError);
    if (at) {
      await showFlow(lcw, output, at.root, at.symbol.qualified_name);
    }
  });

  register("livewalk.toggleCodeLens", async () => {
    const cfg = vscode.workspace.getConfiguration("livewalk");
    await cfg.update("codeLens", !cfg.get<boolean>("codeLens"), vscode.ConfigurationTarget.Global);
  });

  let follow: NodeJS.Timeout | undefined;
  context.subscriptions.push(
    output,
    vscode.languages.registerCodeLensProvider({ scheme: "file" }, lens),
    vscode.workspace.onDidSaveTextDocument(() => lens.refresh()),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("livewalk")) {
        lens.refresh();
      }
    }),
    vscode.window.onDidChangeTextEditorSelection((e) => {
      if (!vscode.workspace.getConfiguration("livewalk").get<boolean>("followCursor")) {
        return;
      }
      const doc = e.textEditor.document;
      if (doc.uri.scheme !== "file") {
        return;
      }
      clearTimeout(follow);
      follow = setTimeout(() => {
        const line = e.textEditor.selection.active.line + 1;
        GraphPanel.containing(doc.uri.fsPath)?.focus(doc.uri.fsPath, line, true);
      }, FOLLOW_DEBOUNCE_MS);
    }),
    { dispose: () => clearTimeout(follow) },
  );
}

function reportError(e: unknown): undefined {
  void vscode.window.showErrorMessage(`Livewalk: ${(e as Error).message ?? String(e)}`);
  return undefined;
}

export function deactivate(): void {}
