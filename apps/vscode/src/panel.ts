import { randomBytes } from "crypto";
import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";

import { explainFunction } from "./explain";
import { Lcw } from "./lcw";
import { openRange, resolveFile } from "./nav";
import {
  analyzeArgs,
  AnalyzeParams,
  ExtensionMessage,
  focusMessage,
  isRequest,
  languagesArgs,
  RequestMessage,
  RevealParams,
  revealTarget,
  snippetArgs,
  SnippetParams,
  stageFraction,
} from "./protocol";
import { buildWebviewHtml } from "./webviewHtml";

/** Answer to one request: a value, or a raw JSON string the bridge parses. */
type Answer = { result: unknown } | { resultJson: string };

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
    this.post(focusMessage(file, line));
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
        const resultJson = await this.analyze(params as unknown as AnalyzeParams);
        this.loaded = true;
        return { resultJson };
      }
      case "discoverLanguages":
        return { resultJson: await this.lcw.run(languagesArgs(params, root)) };
      case "readSourceSnippet":
        return {
          resultJson: await this.lcw.run(snippetArgs(params as unknown as SnippetParams)),
        };
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
    const file = resolveFile(t.repoRoot || workspaceRoot, t.file);
    // Keep the graph visible: open the source in the other column.
    const column =
      this.panel.viewColumn === vscode.ViewColumn.One ? vscode.ViewColumn.Two : vscode.ViewColumn.One;
    await openRange(file, t.startLine, t.endLine, column);
  }
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
