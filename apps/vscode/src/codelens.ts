import * as vscode from "vscode";

import { repoRootFor, SymbolCache } from "./nav";
import { callersTitle } from "./protocol";

/**
 * `N callers · impact` above every function, from one `lcw symbols` call per
 * file version (served from the cached graph; `lcw` re-analyzes when sources
 * are newer). Off by default (`livewalk.codeLens`).
 */
export class CallersCodeLens implements vscode.CodeLensProvider {
  private readonly changed = new vscode.EventEmitter<void>();
  readonly onDidChangeCodeLenses = this.changed.event;

  constructor(private readonly cache: SymbolCache) {}

  refresh(): void {
    this.cache.clear();
    this.changed.fire();
  }

  async provideCodeLenses(doc: vscode.TextDocument): Promise<vscode.CodeLens[]> {
    const cfg = vscode.workspace.getConfiguration("livewalk");
    if (!cfg.get<boolean>("codeLens")) {
      return [];
    }
    const langs = cfg.get<string[]>("codeLensLanguages") ?? ["rust"];
    const root = repoRootFor(doc.uri);
    if (!root || !langs.includes(doc.languageId)) {
      return [];
    }
    let symbols;
    try {
      symbols = await this.cache.get(doc);
    } catch {
      return [];
    }
    const lenses: vscode.CodeLens[] = [];
    for (const s of symbols) {
      if (s.start_line < 1 || s.start_line > doc.lineCount) {
        continue;
      }
      const range = new vscode.Range(s.start_line - 1, 0, s.start_line - 1, 0);
      lenses.push(
        new vscode.CodeLens(range, {
          title: callersTitle(s.fan_in),
          tooltip: `Callers of ${s.qualified_name} (jump to a call site)`,
          command: "livewalk.showCallers",
          arguments: [root, s.qualified_name],
        }),
        new vscode.CodeLens(range, {
          title: "impact",
          tooltip: `Transitive callers of ${s.qualified_name} (blast radius)`,
          command: "livewalk.showImpact",
          arguments: [root, s.qualified_name],
        }),
      );
    }
    return lenses;
  }
}
