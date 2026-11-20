// Serve the extension's webview in a plain browser, for UI work and smoke
// tests without an editor: the same HTML rewrite + CSP the panel uses, the
// real bridge.js, and a mock `acquireVsCodeApi` whose postMessage goes to
// this server, which answers like the extension (by running lcw).
//
//   npm run build && node scripts/webview-harness.js /path/to/repo 8790
//
// Progress and focus messages stream to the page over server-sent events
// (/events), like the extension's postMessage. Environment:
//   LCW            lcw binary (default: lcw on PATH)
//   DELAY_MS       hold each analyze answer this long (to look at the overlay)
//   LANGUAGE       initial language meta (e.g. rust)
//   CACHED=1       initial load from the saved graph
//   FOCUS          initial focus, `file:line` or `file` (absolute or repo-relative)
// GET /focus?file=<path>&line=<n> pushes a focus message (like Reveal in Graph).
// GET /fail-next makes the next analyze fail; /cancel-next makes it "cancelled".
// Reveal / folder-pick requests are logged instead of opening an editor.
const fs = require("fs");
const http = require("http");
const path = require("path");
const { spawn } = require("child_process");

const {
  analyzeArgs,
  CANCELLED,
  focusMessage,
  isRequest,
  languagesArgs,
  parseLocation,
  parseProgressLine,
  revealTarget,
  snippetArgs,
} = require("../out/src/protocol");
const { buildWebviewHtml } = require("../out/src/webviewHtml");

const repo = path.resolve(process.argv[2] || process.cwd());
const port = Number(process.argv[3] || 8790);
const lcwBin = process.env.LCW || "lcw";
const delayMs = Number(process.env.DELAY_MS || 0);
const origin = `http://127.0.0.1:${port}`;
const root = path.resolve(__dirname, "..");
const media = path.join(root, "media");

const MOCK = `window.acquireVsCodeApi = function () {
  var deliver = function (m) { window.dispatchEvent(new MessageEvent("message", { data: m })); };
  new EventSource("/events").onmessage = function (e) { deliver(JSON.parse(e.data)); };
  return {
    postMessage: function (msg) {
      fetch("/rpc", { method: "POST", body: JSON.stringify(msg) })
        .then(function (r) { return r.json(); })
        .then(function (list) { list.forEach(deliver); });
    },
  };
};
`;

/** Open `/events` streams. */
const streams = new Set();
function push(msg) {
  for (const res of streams) {
    res.write(`data: ${JSON.stringify(msg)}\n\n`);
  }
}

let nextOutcome = "ok";

function absolute(file) {
  return path.isAbsolute(file) ? file : path.join(repo, file);
}

function lcw(args, stdin, onProgress) {
  return new Promise((resolve, reject) => {
    const child = spawn(lcwBin, args, { cwd: repo });
    let out = "";
    let err = "";
    let partial = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => {
      const lines = (partial + d).split("\n");
      partial = lines.pop();
      for (const l of lines) {
        const p = parseProgressLine(l);
        if (p) onProgress?.(p);
        else err += l + "\n";
      }
    });
    child.on("error", reject);
    child.stdin.end(stdin ?? "");
    child.on("close", (code) => (code === 0 ? resolve(out) : reject(new Error(err.trim() || `exit ${code}`))));
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function answer(req) {
  const p = req.params || {};
  switch (req.method) {
    case "analyze": {
      const outcome = nextOutcome;
      nextOutcome = "ok";
      const progress = (payload) => push({ type: "progress", payload });
      if (delayMs) {
        progress({ phase: "config", message: "loading configuration" });
        await sleep(delayMs / 2);
      }
      if (outcome === "cancel") throw new Error(CANCELLED);
      if (outcome === "fail") throw new Error("harness: simulated failure (lcw exited with code 1)");
      const out = await lcw(analyzeArgs(p, repo), undefined, progress);
      if (delayMs) await sleep(delayMs / 2);
      return { resultJson: out };
    }
    case "discoverLanguages":
      return { resultJson: await lcw(languagesArgs(p, repo)) };
    case "readSourceSnippet":
      return { resultJson: await lcw(snippetArgs(p)) };
    case "explainFunction": {
      const out = await lcw(["explain-prompt", "--json"], JSON.stringify(p.context));
      return { result: `(harness: no LLM call)\n\n${JSON.parse(out).prompt}` };
    }
    case "reveal": {
      const t = revealTarget(p);
      const how = [t.sync && "sync", t.preserveFocus && "preserveFocus"].filter(Boolean).join(",");
      console.log(`REVEAL ${t.file}:${t.startLine}-${t.endLine} (${p.qualifiedName || ""})${how ? ` [${how}]` : ""}`);
      return { result: null };
    }
    case "pickRepoFolder":
      console.log("PICK FOLDER (harness returns null)");
      return { result: null };
  }
  throw new Error(`unknown method ${req.method}`);
}

function initialFocus() {
  const f = process.env.FOCUS;
  if (!f) return undefined;
  const loc = parseLocation(f);
  return loc ? { file: absolute(loc.file), line: loc.line } : { file: absolute(f) };
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
    initialLanguage: process.env.LANGUAGE || undefined,
    initialCached: process.env.CACHED === "1",
    initialFocus: initialFocus(),
  });
}

const types = { ".js": "text/javascript", ".wasm": "application/wasm", ".html": "text/html" };

http
  .createServer(async (req, res) => {
    try {
      const url = new URL(req.url, origin);
      if (req.method === "POST" && url.pathname === "/rpc") {
        let body = "";
        for await (const chunk of req) body += chunk;
        const msg = JSON.parse(body);
        if (!isRequest(msg)) {
          res.writeHead(400).end("[]");
          return;
        }
        let reply;
        try {
          reply = { type: "response", id: msg.id, ok: true, ...(await answer(msg)) };
        } catch (e) {
          reply = { type: "response", id: msg.id, ok: false, error: e.message };
        }
        console.log(`${msg.method} -> ${reply.ok ? "ok" : "error: " + reply.error}`);
        res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify([reply]));
      } else if (url.pathname === "/events") {
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        res.write(": connected\n\n");
        streams.add(res);
        req.on("close", () => streams.delete(res));
      } else if (url.pathname === "/focus") {
        const file = absolute(url.searchParams.get("file") || "");
        const line = Number(url.searchParams.get("line") || 0) || undefined;
        push(focusMessage(file, line));
        console.log(`FOCUS ${file}${line ? ":" + line : ""}`);
        res.writeHead(200).end("ok");
      } else if (url.pathname === "/fail-next" || url.pathname === "/cancel-next") {
        nextOutcome = url.pathname === "/fail-next" ? "fail" : "cancel";
        res.writeHead(200).end(nextOutcome);
      } else if (url.pathname === "/" || url.pathname === "/index.html") {
        res.writeHead(200, { "content-type": "text/html" }).end(page());
      } else if (url.pathname === "/bridge.js") {
        const bridge = fs.readFileSync(path.join(root, "webview", "bridge.js"), "utf8");
        res.writeHead(200, { "content-type": "text/javascript" }).end(MOCK + bridge);
      } else if (url.pathname.startsWith("/media/")) {
        const file = path.join(media, path.basename(url.pathname));
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
