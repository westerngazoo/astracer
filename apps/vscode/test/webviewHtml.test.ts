import { strict as assert } from "assert";
import { test } from "node:test";

import { buildWebviewHtml, contentSecurityPolicy, escapeAttr } from "../src/webviewHtml";

const TRUNK = `<!DOCTYPE html><html><head>
<link rel="modulepreload" href="/app-abc.js" crossorigin="anonymous" integrity="sha384-x">
<link rel="preload" href="/app-abc_bg.wasm" crossorigin="anonymous" integrity="sha384-y" as="fetch" type="application/wasm">
<script type="module">
import init, * as bindings from '/app-abc.js';
const wasm = await init({ module_or_path: '/app-abc_bg.wasm' });
</script>
<script>(function(){})()</script>
</head><body></body></html>`;

function build(initialRepo = "/repo") {
  return buildWebviewHtml({
    html: TRUNK,
    assets: ["index.html", "app-abc.js", "app-abc_bg.wasm"],
    assetUri: (n) => `https://res/${n}`,
    bridgeUri: "https://res/webview/bridge.js",
    cspSource: "https://res",
    nonce: "N0NCE",
    initialRepo,
  });
}

test("asset URLs are rewritten to webview URIs in attributes and the module script", () => {
  const html = build();
  assert.ok(html.includes(`href="https://res/app-abc.js"`));
  assert.ok(html.includes(`from 'https://res/app-abc.js'`));
  assert.ok(html.includes(`module_or_path: 'https://res/app-abc_bg.wasm'`));
  assert.ok(!html.includes(`'/app-abc`));
  assert.ok(!html.includes("integrity="));
  assert.ok(!html.includes("crossorigin="));
});

test("every script carries the nonce and the bridge loads first", () => {
  const html = build();
  const scripts = html.match(/<script\b[^>]*>/g) ?? [];
  assert.equal(scripts.length, 3);
  for (const s of scripts) {
    assert.ok(s.includes(`nonce="N0NCE"`), s);
  }
  assert.ok(html.indexOf("bridge.js") < html.indexOf(`type="module"`));
  assert.ok(html.indexOf("Content-Security-Policy") < html.indexOf("<script"));
});

test("CSP allows wasm, nonce'd scripts and webview resources only", () => {
  const csp = contentSecurityPolicy("https://res", "N");
  assert.ok(csp.includes("default-src 'none'"));
  assert.ok(csp.includes("script-src 'nonce-N' 'wasm-unsafe-eval' https://res"));
  assert.ok(csp.includes("connect-src https://res"));
  assert.ok(!csp.includes("unsafe-eval'") || csp.includes("wasm-unsafe-eval"));
});

test("the initial repo is passed as an escaped meta attribute", () => {
  const html = build(`/tmp/a "b" <c>`);
  assert.ok(
    html.includes(
      `<meta name="livewalk-initial-repo" content="/tmp/a &quot;b&quot; &lt;c&gt;">`,
    ),
  );
  assert.equal(escapeAttr(`&"`), "&amp;&quot;");
});

test("language, cache mode and the initial focus travel as meta tags", () => {
  const html = buildWebviewHtml({
    html: TRUNK,
    assets: [],
    assetUri: (n) => n,
    bridgeUri: "b.js",
    cspSource: "s",
    nonce: "n",
    initialRepo: "/r",
    initialLanguage: "cpp",
    initialCached: true,
    initialFocus: { file: "/r/a.cpp", line: 7 },
  });
  assert.ok(html.includes(`<meta name="livewalk-initial-language" content="cpp">`));
  assert.ok(html.includes(`<meta name="livewalk-initial-cached" content="1">`));
  const focus = /name="livewalk-initial-focus" content="([^"]*)"/.exec(html)?.[1] ?? "";
  assert.deepEqual(JSON.parse(focus.replace(/&quot;/g, '"')), { file: "/r/a.cpp", line: 7 });
  assert.ok(build().includes(`<meta name="livewalk-initial-focus" content="">`));
  assert.ok(build().includes(`<meta name="livewalk-initial-cached" content="0">`));
});
