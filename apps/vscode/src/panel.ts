import { randomBytes } from "crypto";
import * as fs from "fs";
import * as vscode from "vscode";

import { explainFunction } from "./explain";
import { Lcw } from "./lcw";
import { openRange, repoRootFor, resolveFile } from "./nav";
import {
  analyzeArgs,
  AnalyzeParams,
  ExtensionMessage,
  isRequest,
  languagesArgs,
  RequestMessage,
  RevealParams,
  revealTarget,
  snippetArgs,
  SnippetParams,
} from "./protocol";
import { buildWebviewHtml } from "./webviewHtml";

/** Answer to one request: a value, or a raw JSON string the bridge parses. */
type Answer = { result: unknown } | { resultJson: string };

/**
 * The graph panel: a webview hosting the Leptos frontend (Trunk build in
 * `media/`), whose `VsCodeTransport` requests are answered here by running
 * `lcw` (see `protocol.ts` for the message shapes).
 */
export class GraphPanel {
  private static current: GraphPanel | undefined;

  static show(context: vscode.ExtensionContext, lcw: Lcw): void {
    if (GraphPanel.current) {
      GraphPanel.current.panel.reveal();
      return;
    }
    const media = vscode.Uri.joinPath(context.extensionUri, "media");
    const webviewDir = vscode.Uri.joinPath(context.extensionUri, "webview");
    const panel = vscode.window.createWebviewPanel(
      "livewalk.graph",
      "Livewalk Graph",
      vscode.window.activeTextEditor ? vscode.ViewColumn.Beside : vscode.ViewColumn.Active,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [media, webviewDir],
      },
    );
    GraphPanel.current = new GraphPanel(panel, media, webviewDir, lcw);
  }

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    media: vscode.Uri,
    webviewDir: vscode.Uri,
    private readonly lcw: Lcw,
  ) {
    panel.webview.html = this.html(media, webviewDir);
    panel.webview.onDidReceiveMessage((msg) => void this.onMessage(msg));
    panel.onDidDispose(() => {
      GraphPanel.current = undefined;
    });
  }

  private html(media: vscode.Uri, webviewDir: vscode.Uri): string {
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
      initialRepo: repoRootFor(vscode.window.activeTextEditor?.document.uri) ?? "",
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
    const root = repoRootFor(vscode.window.activeTextEditor?.document.uri) ?? "";
    const params = (req.params ?? {}) as Record<string, unknown>;
    switch (req.method) {
      case "analyze": {
        const resultJson = await this.lcw.run(analyzeArgs(params as unknown as AnalyzeParams, root), {
          cwd: root || undefined,
          onProgress: (payload) => this.post({ type: "progress", payload }),
        });
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
        await this.reveal(params as unknown as RevealParams, root);
        return { result: null };
    }
  }

  private async reveal(p: RevealParams, workspaceRoot: string): Promise<void> {
    const t = revealTarget(p);
    const file = resolveFile(t.repoRoot || workspaceRoot, t.file);
    // Keep the graph visible: open the source in the other column.
    const column =
      this.panel.viewColumn === vscode.ViewColumn.One ? vscode.ViewColumn.Two : vscode.ViewColumn.One;
    await openRange(file, t.startLine, t.endLine, column);
  }
}

async function pickFolder(current: string): Promise<string | null> {
  const picked = await vscode.window.showOpenDialog({
    canSelectFolders: true,
    canSelectFiles: false,
    canSelectMany: false,
    openLabel: "Analyze",
    title: "Select repository",
    defaultUri: current ? vscode.Uri.file(current) : undefined,
  });
  return picked?.[0]?.fsPath ?? null;
}
