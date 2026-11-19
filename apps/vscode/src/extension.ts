import * as vscode from "vscode";

import { CallersCodeLens } from "./codelens";
import { Lcw } from "./lcw";
import { showCallers, showFlow, showImpact, SymbolCache, symbolAtCursor } from "./nav";
import { GraphPanel } from "./panel";

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("Livewalk");
  const lcw = new Lcw(output);
  const symbols = new SymbolCache(lcw);
  const lens = new CallersCodeLens(symbols);

  const register = (id: string, fn: (...args: never[]) => unknown) =>
    context.subscriptions.push(vscode.commands.registerCommand(id, fn));

  register("livewalk.openGraph", () => GraphPanel.show(context, lcw));

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

  context.subscriptions.push(
    output,
    vscode.languages.registerCodeLensProvider({ scheme: "file" }, lens),
    vscode.workspace.onDidSaveTextDocument(() => lens.refresh()),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("livewalk")) {
        lens.refresh();
      }
    }),
  );
}

function reportError(e: unknown): undefined {
  void vscode.window.showErrorMessage(`Livewalk: ${(e as Error).message ?? String(e)}`);
  return undefined;
}

export function deactivate(): void {}
