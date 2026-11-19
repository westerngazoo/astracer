/**
 * Turn the Trunk-built frontend (`media/index.html`, absolute `/asset` URLs)
 * into a webview document: every asset URL rewritten to a webview URI, a
 * strict CSP (nonce'd scripts + `'wasm-unsafe-eval'` for the wasm module),
 * and the VS Code bridge loaded before the app. Pure, so it is unit-tested.
 */

export interface HtmlOptions {
  /** Trunk's `index.html`. */
  html: string;
  /** File names in the media folder (e.g. `livewalk-frontend-<hash>.js`). */
  assets: string[];
  /** Webview URI for a media file name. */
  assetUri: (name: string) => string;
  /** Webview URI of `webview/bridge.js`. */
  bridgeUri: string;
  /** `webview.cspSource`. */
  cspSource: string;
  nonce: string;
  /** Repository the UI analyzes on load (the project root), or "". */
  initialRepo: string;
  /** Fast-mode language to analyze on load (`rust`, `cpp`, …). */
  initialLanguage?: string;
  /** Load the saved graph instead of parsing (it is fresh). */
  initialCached?: boolean;
  /** Function to select once the first load finishes. */
  initialFocus?: { file: string; line?: number };
}

export function escapeAttr(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function contentSecurityPolicy(cspSource: string, nonce: string): string {
  return [
    "default-src 'none'",
    `img-src ${cspSource} data: blob:`,
    `style-src ${cspSource} 'unsafe-inline'`,
    `font-src ${cspSource}`,
    `script-src 'nonce-${nonce}' 'wasm-unsafe-eval' ${cspSource}`,
    `connect-src ${cspSource}`,
  ].join("; ");
}

export function buildWebviewHtml(o: HtmlOptions): string {
  let html = o.html;
  // Subresource integrity / CORS mode do not apply to webview resources.
  html = html.replace(/\s+integrity="[^"]*"/g, "").replace(/\s+crossorigin="[^"]*"/g, "");
  for (const name of o.assets) {
    if (name === "index.html") {
      continue;
    }
    const re = new RegExp(`(["'])(?:\\.?/)?${escapeRegExp(name)}\\1`, "g");
    const uri = o.assetUri(name);
    html = html.replace(re, (_m, q: string) => `${q}${uri}${q}`);
  }
  html = html.replace(/<script(?![^>]*\bnonce=)/g, `<script nonce="${o.nonce}"`);
  const head = [
    `<meta http-equiv="Content-Security-Policy" content="${escapeAttr(
      contentSecurityPolicy(o.cspSource, o.nonce),
    )}">`,
    `<meta name="livewalk-initial-repo" content="${escapeAttr(o.initialRepo)}">`,
    `<meta name="livewalk-initial-language" content="${escapeAttr(o.initialLanguage ?? "")}">`,
    `<meta name="livewalk-initial-cached" content="${o.initialCached ? "1" : "0"}">`,
    `<meta name="livewalk-initial-focus" content="${escapeAttr(
      o.initialFocus ? JSON.stringify(o.initialFocus) : "",
    )}">`,
    `<script nonce="${o.nonce}" src="${escapeAttr(o.bridgeUri)}"></script>`,
  ].join("\n    ");
  if (/<head[^>]*>/i.test(html)) {
    html = html.replace(/<head[^>]*>/i, (m) => `${m}\n    ${head}`);
  } else {
    html = `${head}\n${html}`;
  }
  return html;
}
