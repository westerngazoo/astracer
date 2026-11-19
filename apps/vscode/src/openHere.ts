import * as fs from "fs";
import * as vscode from "vscode";

import { Lcw } from "./lcw";
import { GraphPanel, pickFolder } from "./panel";
import {
  confirmLoadText,
  GraphInfo,
  graphInfoArgs,
  LanguageChoice,
  languagesArgs,
  pickLanguage,
} from "./protocol";
import { findProjectRoot, RootFs } from "./roots";

/** Where a command was invoked from: decides what gets focused once loaded. */
export type Origin = "editor" | "explorer" | "palette";

const nodeFs: RootFs = {
  exists: (p) => fs.existsSync(p),
  read: (p) => {
    try {
      return fs.readFileSync(p, "utf8");
    } catch {
      return undefined;
    }
  },
};

const LOAD = "Load";
const CHOOSE = "Choose another folder…";
const LOAD_ALWAYS = "Load (don't ask again)";

function isDirectory(p: string): boolean {
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
}

/** The project root `lcw` should analyze for a file or folder. */
export function projectRootFor(target: string, isDir = isDirectory(target)): string {
  const workspace = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(target))?.uri.fsPath;
  return findProjectRoot({ target, isDir, workspace }, nodeFs);
}

/**
 * The language to analyze `root` in: the only one present, the clicked
 * file's, else the user's pick. `null` when the user dismissed the pick;
 * `undefined` lets `lcw` use `livewalk.toml`'s.
 */
async function chooseLanguage(lcw: Lcw, root: string, file?: string): Promise<string | undefined | null> {
  let present: LanguageChoice[] = [];
  try {
    present = await lcw.json<LanguageChoice[]>(languagesArgs({ path: root }, root), { cwd: root });
  } catch {
    return undefined;
  }
  if (present.length === 0) {
    return undefined;
  }
  const auto = pickLanguage(present, file);
  if (auto) {
    return auto;
  }
  const chosen = await vscode.window.showQuickPick(
    present.map((l) => ({ label: l.label, description: l.id, id: l.id })),
    { title: `Livewalk: ${root} has several languages — which graph?`, placeHolder: "Language" },
  );
  return chosen ? chosen.id : null;
}

async function graphInfo(lcw: Lcw, root: string, language?: string): Promise<GraphInfo | undefined> {
  try {
    return await lcw.json<GraphInfo>(graphInfoArgs(root, language), { cwd: root, timeoutMs: 20_000 });
  } catch {
    return undefined;
  }
}

/**
 * "Open Graph Here": resolve the project of `uri` (the active editor or first
 * workspace folder when absent), ask before loading it, then open its panel
 * and select what the user was looking at — the function under the cursor
 * (editor), the file's first function (a file in the explorer), nothing (a
 * folder).
 */
export async function openGraphHere(
  context: vscode.ExtensionContext,
  lcw: Lcw,
  uri: vscode.Uri | undefined,
  origin: Origin,
): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  let target = uri?.scheme === "file" ? uri.fsPath : undefined;
  if (!target && origin !== "explorer" && editor?.document.uri.scheme === "file") {
    target = editor.document.uri.fsPath;
  }
  target ??= vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!target) {
    const picked = await pickFolder("", "Open Graph");
    if (!picked) {
      return;
    }
    target = picked;
  }

  const isDir = isDirectory(target);
  let focus: { file: string; line?: number } | undefined;
  if (!isDir) {
    const atCursor = origin !== "explorer" && editor?.document.uri.fsPath === target;
    focus = { file: target, line: atCursor ? editor.selection.active.line + 1 : undefined };
  }

  let root = projectRootFor(target, isDir);
  if (GraphPanel.get(root)) {
    GraphPanel.open(context, lcw, { root, cached: true, focus });
    return;
  }

  let language = await chooseLanguage(lcw, root, focus?.file);
  if (language === null) {
    return;
  }
  let info = await graphInfo(lcw, root, language);

  const cfg = vscode.workspace.getConfiguration("livewalk");
  if (cfg.get<boolean>("confirmBeforeLoad", true)) {
    const choice = await vscode.window.showInformationMessage(
      confirmLoadText(root, language, info),
      { modal: true, detail: "Livewalk builds a call graph of the whole project (powered by lcw)." },
      LOAD,
      CHOOSE,
      LOAD_ALWAYS,
    );
    if (!choice) {
      return;
    }
    if (choice === LOAD_ALWAYS) {
      await cfg.update("confirmBeforeLoad", false, vscode.ConfigurationTarget.Global);
    }
    if (choice === CHOOSE) {
      const picked = await pickFolder(root, "Load graph");
      if (!picked) {
        return;
      }
      root = picked;
      if (GraphPanel.get(root)) {
        GraphPanel.open(context, lcw, { root, cached: true, focus });
        return;
      }
      language = await chooseLanguage(lcw, root, focus?.file);
      if (language === null) {
        return;
      }
      info = await graphInfo(lcw, root, language);
    }
  }

  GraphPanel.open(context, lcw, {
    root,
    language,
    cached: Boolean(info?.exists && info.fresh),
    focus,
  });
}

/** "Reveal in Graph": select the function at the cursor in its project's panel (opening it if needed). */
export async function revealInGraph(context: vscode.ExtensionContext, lcw: Lcw): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.uri.scheme !== "file") {
    void vscode.window.showWarningMessage("Livewalk: open a source file first");
    return;
  }
  const file = editor.document.uri.fsPath;
  const line = editor.selection.active.line + 1;
  const panel = GraphPanel.get(projectRootFor(file, false)) ?? GraphPanel.containing(file);
  if (panel) {
    panel.reveal({ file, line });
    return;
  }
  await openGraphHere(context, lcw, editor.document.uri, "editor");
}
