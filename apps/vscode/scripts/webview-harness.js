// Serve the extension's webview in a plain browser, for UI work and smoke
// tests without an editor: the same HTML rewrite + CSP the panel uses, the
// real bridge.js, and a mock `acquireVsCodeApi` whose postMessage goes to
// this server, which answers like the extension (by running lcw).
//
//   npm run build && node scripts/webview-harness.js /path/to/repo 8790
//
// Reveal / folder-pick requests are logged instead of opening an editor.
const fs = require("fs");
const http = require("http");
const path = require("path");
const { spawn } = require("child_process");

const { analyzeArgs, isRequest, languagesArgs, revealTarget, snippetArgs } = require("../out/src/protocol");
const { buildWebviewHtml } = require("../out/src/webviewHtml");

const repo = path.resolve(process.argv[2] || process.cwd());
const port = Number(process.argv[3] || 8790);
const lcwBin = process.env.LCW || "lcw";
const origin = `http://127.0.0.1:${port}`;
const root = path.resolve(__dirname, "..");
const media = path.join(root, "media");

const MOCK = `window.acquireVsCodeApi = function () {
  return {
    postMessage: function (msg) {
      fetch("/rpc", { method: "POST", body: JSON.stringify(msg) })
        .then(function (r) { return r.json(); })
        .then(function (list) {
          list.forEach(function (m) { window.dispatchEvent(new MessageEvent("message", { data: m })); });
        });
    },
  };
};
`;

function lcw(args, stdin) {
  return new Promise((resolve, reject) => {
    const child = spawn(lcwBin, args, { cwd: repo });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    child.on("error", reject);
    child.stdin.end(stdin ?? "");
    child.on("close", (code) => (code === 0 ? resolve({ out, err }) : reject(new Error(err.trim() || `exit ${code}`))));
  });
}

async function answer(req) {
  const p = req.params || {};
  switch (req.method) {
    case "analyze": {
      const { out, err } = await lcw(analyzeArgs(p, repo));
      const progress = err
        .split("\n")
        .filter((l) => l.startsWith("{"))
        .map((l) => ({ type: "progress", payload: JSON.parse(l) }));
      return [progress, { resultJson: out }];
    }
    case "discoverLanguages":
      return [[], { resultJson: (await lcw(languagesArgs(p, repo))).out }];
    case "readSourceSnippet":
      return [[], { resultJson: (await lcw(snippetArgs(p))).out }];
    case "explainFunction": {
      const { out } = await lcw(["explain-prompt", "--json"], JSON.stringify(p.context));
      return [[], { result: `(harness: no LLM call)\n\n${JSON.parse(out).prompt}` }];
    }
    case "reveal": {
      const t = revealTarget(p);
      console.log(`REVEAL ${t.file}:${t.startLine}-${t.endLine} (${p.qualifiedName || ""})`);
      return [[], { result: null }];
    }
    case "pickRepoFolder":
      console.log("PICK FOLDER (harness returns null)");
      return [[], { result: null }];
  }
  throw new Error(`unknown method ${req.method}`);
}

function page() {
  return buildWebviewHtml({
    html: fs.readFileSync(path.join(media, "index.html"), "utf8"),
    assets: fs.readdirSync(media),
    assetUri: (name) => `${origin}/media/${name}`,
    bridgeUri: `${origin}/bridge.js`,
    cspSource: origin,
    nonce: "harness",
    initialRepo: repo,
  });
}

const types = { ".js": "text/javascript", ".wasm": "application/wasm", ".html": "text/html" };

http
  .createServer(async (req, res) => {
    try {
      if (req.method === "POST" && req.url === "/rpc") {
        let body = "";
        for await (const chunk of req) body += chunk;
        const msg = JSON.parse(body);
        if (!isRequest(msg)) {
          res.writeHead(400).end("[]");
          return;
        }
        let reply;
        try {
          const [progress, a] = await answer(msg);
          reply = [...progress, { type: "response", id: msg.id, ok: true, ...a }];
        } catch (e) {
          reply = [{ type: "response", id: msg.id, ok: false, error: e.message }];
        }
        console.log(`${msg.method} -> ${reply[reply.length - 1].ok ? "ok" : "error: " + reply[reply.length - 1].error}`);
        res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(reply));
      } else if (req.url === "/" || req.url === "/index.html") {
        res.writeHead(200, { "content-type": "text/html" }).end(page());
      } else if (req.url === "/bridge.js") {
        const bridge = fs.readFileSync(path.join(root, "webview", "bridge.js"), "utf8");
        res.writeHead(200, { "content-type": "text/javascript" }).end(MOCK + bridge);
      } else if (req.url.startsWith("/media/")) {
        const file = path.join(media, path.basename(req.url));
        res.writeHead(200, { "content-type": types[path.extname(file)] || "application/octet-stream" });
        fs.createReadStream(file).pipe(res);
      } else {
        res.writeHead(404).end();
      }
    } catch (e) {
      res.writeHead(500).end(String(e));
    }
  })
  .listen(port, "127.0.0.1", () => console.log(`webview harness for ${repo} on ${origin}/`));
