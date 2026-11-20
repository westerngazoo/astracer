import { randomBytes } from "crypto";
import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";

import { explainFunction } from "./explain";
import { Lcw } from "./lcw";
import { relocateIn, symbolAtLine } from "./live";
import { openRange, resolveFile } from "./nav";
import { snippetWindow } from "./relocate";
import {
  analyzeArgs,
  AnalyzeParams,
  EchoGuard,
  ExtensionMessage,
  focusMessage,
  isRequest,
  languagesArgs,
  LiveSnippet,
  RequestMessage,
  RevealParams,
  revealTarget,
  SnippetParams,
  stageFraction,
} from "./protocol";
import { buildWebviewHtml } from "./webviewHtml";

/** Answer to one request: a value, or a raw JSON string the bridge parses. */
type Answer = { result: unknown } | { resultJson: string };

/** Editor moves the panels made, so cursor following does not echo them back. */
export const editorEcho = new EchoGuard();

/** `livewalk.syncEditor`: the editor follows the graph and the graph follows the editor. */
export function syncEditorEnabled(): boolean {
  return vscode.workspace.getConfiguration("livewalk").get<boolean>("syncEditor", true);
}

/** What a new panel loads first. */
export interface PanelLoad {
  /** Project root: the panel's identity and the repository analyzed on open. */
  root: string;
  language?: string;
  /** Load the saved graph (it is fresh) instead of parsing. */
  cached: boolean;
  /** Function to select once loaded. */
  focus?: { file: string; line?: number };
}

/**
 * A graph panel: a webview hosting the Leptos frontend (Trunk build in
 * `media/`), whose `VsCodeTransport` requests are answered here by running
 * `lcw` (see `protocol.ts` for the message shapes). One per project root.
 */
export class GraphPanel {
  private static readonly panels = new Map<string, GraphPanel>();

  /** Set once an analyze answered, so focus messages have a graph to land on. */
  private loaded = false;

  /** When the loaded graph's analyze started (files modified later have moved on). */
  private analyzedAt = 0;

  static get(root: string): GraphPanel | undefined {
    return GraphPanel.panels.get(path.resolve(root));
  }

  /** The open panel whose root is the deepest ancestor of `file`. */
  static containing(file: string): GraphPanel | undefined {
    let best: GraphPanel | undefined;
    for (const p of GraphPanel.panels.values()) {
      const rel = path.relative(p.root, file);
      if (rel && !rel.startsWith("..") && !path.isAbsolute(rel)) {
        if (!best || p.root.length > best.root.length) {
          best = p;
        }
      }
    }
    return best;
  }

  /** Open a panel for `load.root`, or reveal (and focus) the one already open. */
  static open(context: vscode.ExtensionContext, lcw: Lcw, load: PanelLoad): GraphPanel {
    const root = path.resolve(load.root);
    const existing = GraphPanel.panels.get(root);
    if (existing) {
      existing.reveal(load.focus);
      return existing;
    }
    const media = vscode.Uri.joinPath(context.extensionUri, "media");
    const webviewDir = vscode.Uri.joinPath(context.extensionUri, "webview");
    const panel = vscode.window.createWebviewPanel(
      "livewalk.graph",
      `Livewalk: ${path.basename(root) || root}`,
      vscode.window.activeTextEditor ? vscode.ViewColumn.Beside : vscode.ViewColumn.Active,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [media, webviewDir],
      },
    );
    const gp = new GraphPanel(panel, media, webviewDir, lcw, { ...load, root });
    GraphPanel.panels.set(root, gp);
    if (load.focus?.line) {
      // The initial focus meta matches by line; this one also names the live definition.
      gp.focus(load.focus.file, load.focus.line);
    }
    return gp;
  }

  private readonly root: string;

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    media: vscode.Uri,
    webviewDir: vscode.Uri,
    private readonly lcw: Lcw,
    load: PanelLoad,
  ) {
    this.root = load.root;
    panel.webview.html = this.html(media, webviewDir, load);
    panel.webview.onDidReceiveMessage((msg) => void this.onMessage(msg));
    panel.onDidDispose(() => {
      GraphPanel.panels.delete(this.root);
    });
  }

  /** Bring the panel forward (keeping the editor focused) and select `focus`. */
  reveal(focus?: { file: string; line?: number }): void {
    this.panel.reveal(this.panel.viewColumn, true);
    if (focus) {
      this.focus(focus.file, focus.line);
    }
  }

  /**
   * Select the function at `file:line` in the graph. Before the first load
   * the webview holds it until the graph is ready; `onlyIfLoaded` drops it
   * instead (cursor following).
   */
  focus(file: string, line?: number, onlyIfLoaded = false): void {
    if (onlyIfLoaded && !this.loaded) {
      return;
    }
    void this.focusMessageFor(file, line).then((msg) => this.post(msg));
  }

  /** A focus message naming the live definition at `line`, when the file is open in an editor. */
  private async focusMessageFor(file: string, line?: number): Promise<ExtensionMessage> {
    const doc = line ? openDocument(file) : undefined;
    const sym = doc && line ? await symbolAtLine(doc, line).catch(() => undefined) : undefined;
    return focusMessage(file, line, sym && { name: sym.name, containers: sym.containers, line: sym.start });
  }

  private html(media: vscode.Uri, webviewDir: vscode.Uri, load: PanelLoad): string {
    const index = vscode.Uri.joinPath(media, "index.html").fsPath;
    if (!fs.existsSync(index)) {
      return `<!DOCTYPE html><html><body style="font-family:sans-serif;padding:16px">
        <h3>Livewalk frontend not bundled</h3>
        <p>This build of the extension has no <code>media/index.html</code>. Rebuild it with
        <code>scripts/livewalk-dev.sh vscode</code> (runs <code>trunk build --release</code> and copies <code>dist/</code>).</p>
        </body></html>`;
    }
    const webview = this.panel.webview;
    return buildWebviewHtml({
      html: fs.readFileSync(index, "utf8"),
      assets: fs.readdirSync(media.fsPath),
      assetUri: (name) => webview.asWebviewUri(vscode.Uri.joinPath(media, name)).toString(),
      bridgeUri: webview.asWebviewUri(vscode.Uri.joinPath(webviewDir, "bridge.js")).toString(),
      cspSource: webview.cspSource,
      nonce: randomBytes(16).toString("base64"),
      initialRepo: load.root,
      initialLanguage: load.language,
      initialCached: load.cached,
      initialFocus: load.focus,
    });
  }

  private post(msg: ExtensionMessage): void {
    void this.panel.webview.postMessage(msg);
  }

  private async onMessage(msg: unknown): Promise<void> {
    if (!isRequest(msg)) {
      return;
    }
    try {
      const answer = await this.answer(msg);
      this.post({ type: "response", id: msg.id, ok: true, ...answer });
    } catch (e) {
      this.post({ type: "response", id: msg.id, ok: false, error: (e as Error).message ?? String(e) });
    }
  }

  private async answer(req: RequestMessage): Promise<Answer> {
    const root = this.root;
    const params = (req.params ?? {}) as Record<string, unknown>;
    switch (req.method) {
      case "analyze": {
        const started = Date.now();
        const resultJson = await this.analyze(params as unknown as AnalyzeParams);
        this.loaded = true;
        this.analyzedAt = started;
        return { resultJson };
      }
      case "discoverLanguages":
        return { resultJson: await this.lcw.run(languagesArgs(params, root)) };
      case "readSourceSnippet":
        return { result: await this.liveSnippet(params as unknown as SnippetParams, root) };
      case "explainFunction":
        return {
          result: await explainFunction(this.lcw, params.context, String(params.provider ?? "")),
        };
      case "pickRepoFolder":
        return { result: await pickFolder(String(params.current ?? "") || root) };
      case "reveal":
        await this.revealSource(params as unknown as RevealParams, root);
        return { result: null };
    }
  }

  /**
   * `lcw view-data` under a progress notification: the same stages the
   * webview overlay shows, and Cancel kills the process (the request then
   * fails with `CANCELLED`).
   */
  private analyze(p: AnalyzeParams): Thenable<string> {
    const args = analyzeArgs(p, this.root);
    const target = args[args.length - 1];
    return vscode.window.withProgress(
      {
        location: p.useCache ? vscode.ProgressLocation.Window : vscode.ProgressLocation.Notification,
        title: `Livewalk: loading ${path.basename(target) || target}`,
        cancellable: true,
      },
      (progress, token) => {
        let done = 0;
        return this.lcw.run(args, {
          cwd: this.root,
          cancel: token,
          onProgress: (payload) => {
            this.post({ type: "progress", payload });
            const f = stageFraction(payload);
            const increment = f !== undefined && f * 100 > done ? f * 100 - done : undefined;
            done += increment ?? 0;
            progress.report({ message: `${payload.phase} · ${payload.message}`, increment });
          },
        });
      },
    );
  }

  private async revealSource(p: RevealParams, workspaceRoot: string): Promise<void> {
    const t = revealTarget(p);
    if (t.sync && !syncEditorEnabled()) {
      return;
    }
    const file = path.resolve(resolveFile(t.repoRoot || workspaceRoot, t.file));
    let { startLine, endLine } = t;
    if (!t.live) {
      const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
      ({ start: startLine, end: endLine } = await relocateIn(doc, p.qualifiedName ?? "", startLine, endLine));
    }
    editorEcho.revealing(file, startLine, endLine, Date.now());
    await openRange(file, startLine, endLine, this.editorColumn(file), {
      preserveFocus: t.preserveFocus,
      caretOnly: t.preserveFocus,
    });
  }

  /**
   * The Code tab's source, from the live document (an unsaved buffer
   * included) rather than the file as analyzed: the span is relocated by
   * name, so its highlight and the editor's cursor line agree after edits.
   * `changed` says the text moved on since the analyze (dirty, saved later,
   * or the definition moved).
   */
  private async liveSnippet(p: SnippetParams, workspaceRoot: string): Promise<LiveSnippet> {
    const file = path.resolve(resolveFile(p.repoRoot || workspaceRoot, String(p.file ?? "")));
    if (!p.file) {
      throw new Error("no source file for this node");
    }
    let doc: vscode.TextDocument;
    try {
      doc = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
    } catch {
      throw new Error(`could not read ${file} (from ${p.file})`);
    }
    const start = Math.max(1, p.startLine || 1);
    const end = Math.max(start, p.endLine || start);
    const span = await relocateIn(doc, p.qualifiedName ?? "", start, end);
    const hlStart = Math.min(span.start, doc.lineCount);
    const hlEnd = Math.min(Math.max(span.end, hlStart), doc.lineCount);
    const { lo, hi } = snippetWindow(doc.lineCount, hlStart, hlEnd, p.cursorLine, p.wholeFile === true);
    const lines = [];
    for (let n = lo; n <= hi; n++) {
      lines.push({ number: n, text: doc.lineAt(n - 1).text });
    }
    let mtime = 0;
    try {
      mtime = fs.statSync(file).mtimeMs;
    } catch {
      mtime = 0;
    }
    const changed = doc.isDirty || hlStart !== start || (this.analyzedAt > 0 && mtime > this.analyzedAt);
    return {
      file,
      start_line: lo,
      end_line: hi,
      highlight_start: hlStart,
      highlight_end: hlEnd,
      lines,
      changed,
    };
  }

  /**
   * Where to show `file` without covering the graph: the visible editor
   * already showing it, else a group with a tab for it, else the visible
   * editor group beside the panel, else the column next to the panel.
   */
  private editorColumn(file: string): vscode.ViewColumn {
    const own = this.panel.viewColumn;
    const elsewhere = (c: vscode.ViewColumn | undefined): c is vscode.ViewColumn =>
      c !== undefined && c !== own;
    const visible = vscode.window.visibleTextEditors;
    const showing = visible.find((e) => e.document.uri.fsPath === file && elsewhere(e.viewColumn));
    if (showing?.viewColumn) {
      return showing.viewColumn;
    }
    const tabbed = vscode.window.tabGroups.all.find(
      (g) =>
        elsewhere(g.viewColumn) &&
        g.tabs.some((tab) => tab.input instanceof vscode.TabInputText && tab.input.uri.fsPath === file),
    );
    if (tabbed) {
      return tabbed.viewColumn;
    }
    const beside = visible.find((e) => elsewhere(e.viewColumn));
    if (beside?.viewColumn) {
      return beside.viewColumn;
    }
    return own === vscode.ViewColumn.One ? vscode.ViewColumn.Two : vscode.ViewColumn.One;
  }
}

/** The document for `file` if an editor has it open (its live text, unsaved edits included). */
function openDocument(file: string): vscode.TextDocument | undefined {
  return vscode.workspace.textDocuments.find((d) => d.uri.scheme === "file" && d.uri.fsPath === file);
}

export async function pickFolder(current: string, openLabel = "Analyze"): Promise<string | null> {
  const picked = await vscode.window.showOpenDialog({
    canSelectFolders: true,
    canSelectFiles: false,
    canSelectMany: false,
    openLabel,
    title: "Select repository",
    defaultUri: current ? vscode.Uri.file(current) : undefined,
  });
  return picked?.[0]?.fsPath ?? null;
}
