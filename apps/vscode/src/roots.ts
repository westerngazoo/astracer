/**
 * Which project a right-clicked file or folder belongs to. Pure (filesystem
 * access injected) so it is unit-tested in plain Node.
 */
import * as path from "path";

/** Files (or folders) that mark a project root, any one is enough. */
export const ROOT_MARKERS = [
  "Cargo.toml",
  "package.json",
  "go.mod",
  "pyproject.toml",
  "CMakeLists.txt",
  "compile_commands.json",
  ".git",
] as const;

export interface RootFs {
  exists(p: string): boolean;
  /** File contents, or `undefined` when unreadable. */
  read(p: string): string | undefined;
}

export interface RootQuery {
  /** Absolute path of the clicked file or folder. */
  target: string;
  /** Whether `target` is a folder. */
  isDir: boolean;
  /** The workspace folder containing `target`; the walk never goes above it. */
  workspace?: string;
}

function hasMarker(fs: RootFs, dir: string): boolean {
  return ROOT_MARKERS.some((m) => fs.exists(path.join(dir, m)));
}

function isCargoWorkspace(fs: RootFs, dir: string): boolean {
  const toml = fs.read(path.join(dir, "Cargo.toml"));
  return toml !== undefined && /^\s*\[workspace\]/m.test(toml);
}

/** `dir` and its ancestors up to (and including) `stop`, or the filesystem root. */
function ancestors(dir: string, stop?: string): string[] {
  const out: string[] = [];
  let cur = path.resolve(dir);
  const bound = stop ? path.resolve(stop) : undefined;
  for (;;) {
    out.push(cur);
    if (cur === bound) {
      break;
    }
    const up = path.dirname(cur);
    if (up === cur) {
      break;
    }
    cur = up;
  }
  return out;
}

function inside(child: string, parent: string): boolean {
  const rel = path.relative(parent, child);
  return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel));
}

/**
 * The project root for a clicked resource:
 *
 * * a folder that has a marker is its own root;
 * * otherwise the nearest ancestor with a marker, except that a member
 *   crate's `Cargo.toml` yields to an enclosing `[workspace]` `Cargo.toml`;
 * * the walk is bounded by the workspace folder (a file outside every
 *   workspace walks to the filesystem root);
 * * with no marker anywhere: the workspace folder, else the folder itself
 *   (or the file's folder).
 */
export function findProjectRoot(q: RootQuery, fs: RootFs): string {
  const start = q.isDir ? path.resolve(q.target) : path.dirname(path.resolve(q.target));
  const workspace = q.workspace && inside(start, q.workspace) ? q.workspace : undefined;
  if (q.isDir && hasMarker(fs, start)) {
    return start;
  }
  const chain = ancestors(start, workspace);
  const nearest = chain.find((d) => hasMarker(fs, d));
  if (!nearest) {
    return workspace ? path.resolve(workspace) : start;
  }
  if (fs.exists(path.join(nearest, "Cargo.toml")) && !isCargoWorkspace(fs, nearest)) {
    const above = chain.slice(chain.indexOf(nearest) + 1);
    const ws = above.find((d) => fs.exists(path.join(d, "Cargo.toml")) && isCargoWorkspace(fs, d));
    if (ws) {
      return ws;
    }
  }
  return nearest;
}
