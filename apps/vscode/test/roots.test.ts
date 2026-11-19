import { strict as assert } from "assert";
import { test } from "node:test";

import { findProjectRoot, RootFs } from "../src/roots";

/** An in-memory tree: path -> file contents ("" for marker folders like `.git`). */
function tree(files: Record<string, string>): RootFs {
  return {
    exists: (p) => p in files,
    read: (p) => files[p],
  };
}

const WS_CARGO = "[workspace]\nmembers = [\"crates/*\"]\n";
const MEMBER = "[package]\nname = \"a\"\n";

test("a member crate's file resolves to the enclosing cargo workspace", () => {
  const fs = tree({
    "/w/Cargo.toml": WS_CARGO,
    "/w/crates/a/Cargo.toml": MEMBER,
    "/w/.git": "",
  });
  assert.equal(findProjectRoot({ target: "/w/crates/a/src/lib.rs", isDir: false, workspace: "/w" }, fs), "/w");
});

test("a standalone crate stays its own root", () => {
  const fs = tree({ "/w/.git": "", "/w/tools/x/Cargo.toml": MEMBER });
  assert.equal(
    findProjectRoot({ target: "/w/tools/x/src/main.rs", isDir: false, workspace: "/w" }, fs),
    "/w/tools/x",
  );
});

test("the nearest non-cargo marker wins over markers further up", () => {
  const fs = tree({
    "/w/Cargo.toml": WS_CARGO,
    "/w/apps/vscode/package.json": "{}",
  });
  assert.equal(
    findProjectRoot({ target: "/w/apps/vscode/src/panel.ts", isDir: false, workspace: "/w" }, fs),
    "/w/apps/vscode",
  );
  const go = tree({ "/m/svc/go.mod": "module x", "/m/.git": "" });
  assert.equal(findProjectRoot({ target: "/m/svc/cmd/main.go", isDir: false, workspace: "/m" }, go), "/m/svc");
  const cpp = tree({ "/c/CMakeLists.txt": "", "/c/build/compile_commands.json": "[]" });
  assert.equal(findProjectRoot({ target: "/c/src/a.cpp", isDir: false, workspace: "/c" }, cpp), "/c");
  const py = tree({ "/p/pyproject.toml": "" });
  assert.equal(findProjectRoot({ target: "/p/pkg/mod.py", isDir: false }, py), "/p");
});

test("the walk is bounded by the workspace folder", () => {
  const fs = tree({ "/home/.git": "", "/home/Cargo.toml": WS_CARGO });
  assert.equal(
    findProjectRoot({ target: "/home/ws/src/a.rs", isDir: false, workspace: "/home/ws" }, fs),
    "/home/ws",
  );
  const member = tree({ "/home/Cargo.toml": WS_CARGO, "/home/ws/Cargo.toml": MEMBER });
  assert.equal(
    findProjectRoot({ target: "/home/ws/src/a.rs", isDir: false, workspace: "/home/ws" }, member),
    "/home/ws",
  );
});

test("without a workspace the walk goes to the filesystem root", () => {
  const fs = tree({ "/r/.git": "" });
  assert.equal(findProjectRoot({ target: "/r/deep/er/a.rs", isDir: false }, fs), "/r");
  assert.equal(findProjectRoot({ target: "/x/y/a.rs", isDir: false }, tree({})), "/x/y");
});

test("a folder with a marker is used directly, even a member crate", () => {
  const fs = tree({ "/w/Cargo.toml": WS_CARGO, "/w/crates/a/Cargo.toml": MEMBER });
  assert.equal(findProjectRoot({ target: "/w/crates/a", isDir: true, workspace: "/w" }, fs), "/w/crates/a");
});

test("a folder without a marker walks up, else is loaded as is", () => {
  const fs = tree({ "/w/Cargo.toml": WS_CARGO, "/w/crates/a/Cargo.toml": MEMBER });
  assert.equal(findProjectRoot({ target: "/w/crates/a/src", isDir: true, workspace: "/w" }, fs), "/w");
  assert.equal(findProjectRoot({ target: "/w/docs", isDir: true, workspace: "/w" }, tree({})), "/w");
  assert.equal(findProjectRoot({ target: "/loose/dir", isDir: true }, tree({})), "/loose/dir");
});

test("a workspace that does not contain the target is ignored", () => {
  const fs = tree({ "/other/.git": "" });
  assert.equal(findProjectRoot({ target: "/other/a/b.rs", isDir: false, workspace: "/w" }, fs), "/other");
});
