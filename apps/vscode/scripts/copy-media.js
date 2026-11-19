// Copy the Trunk build of the Leptos frontend (apps/desktop/frontend/dist)
// into media/, where the webview loads it from. Run `trunk build --release`
// in apps/desktop/frontend first (scripts/livewalk-dev.sh vscode does both).
const fs = require("fs");
const path = require("path");

const dist = path.resolve(__dirname, "../../desktop/frontend/dist");
const media = path.resolve(__dirname, "../media");

if (!fs.existsSync(path.join(dist, "index.html"))) {
  console.error(`no frontend build at ${dist}; run: cd apps/desktop/frontend && trunk build --release`);
  process.exit(1);
}
fs.rmSync(media, { recursive: true, force: true });
fs.mkdirSync(media, { recursive: true });
let bytes = 0;
for (const name of fs.readdirSync(dist)) {
  const from = path.join(dist, name);
  if (!fs.statSync(from).isFile()) {
    continue;
  }
  fs.copyFileSync(from, path.join(media, name));
  bytes += fs.statSync(from).size;
}
console.log(`copied ${dist} -> ${media} (${(bytes / 1e6).toFixed(1)} MB)`);
