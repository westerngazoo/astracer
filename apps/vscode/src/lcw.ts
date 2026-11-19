import { spawn } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";

import { CANCELLED, parseProgressLine, Progress } from "./protocol";

/** Exit code `lcw` uses for an ambiguous or unknown symbol (candidates on stdout). */
export const EXIT_AMBIGUOUS = 2;

export class LcwError extends Error {
  constructor(
    message: string,
    readonly code: number | null,
    readonly stdout: string,
  ) {
    super(message);
  }
}

export interface RunOptions {
  cwd?: string;
  stdin?: string;
  onProgress?: (p: Progress) => void;
  /** Milliseconds before the process is killed (default 10 minutes). */
  timeoutMs?: number;
  /** Kills the process on cancellation; the run rejects with [`CANCELLED`]. */
  cancel?: vscode.CancellationToken;
}

/**
 * Runs the `lcw` CLI. Every answer the extension gives — the graph view,
 * snippets, callers, impact, flow — comes from `lcw … --json`, so the
 * extension never traverses the graph itself (AGENTS.md: front ends render).
 */
export class Lcw {
  constructor(private readonly output: vscode.OutputChannel) {}

  /** The binary from `livewalk.lcwPath`; a bare `lcw` falls back to `~/.cargo/bin/lcw`, since editors launched from the Dock often lack the shell PATH. */
  binary(): string {
    const configured =
      vscode.workspace.getConfiguration("livewalk").get<string>("lcwPath")?.trim() || "lcw";
    if (configured !== "lcw") {
      return configured.startsWith("~/") ? path.join(os.homedir(), configured.slice(2)) : configured;
    }
    if (onPath("lcw")) {
      return "lcw";
    }
    const cargo = path.join(os.homedir(), ".cargo", "bin", "lcw");
    return fs.existsSync(cargo) ? cargo : "lcw";
  }

  /** Run `lcw args…`, resolving with stdout. Rejects with stderr's last lines on failure. */
  run(args: string[], opts: RunOptions = {}): Promise<string> {
    const bin = this.binary();
    const started = Date.now();
    return new Promise((resolve, reject) => {
      const child = spawn(bin, args, { cwd: opts.cwd, env: process.env });
      const out: Buffer[] = [];
      let err = "";
      let partial = "";
      let cancelled = false;
      const timer = setTimeout(() => child.kill(), opts.timeoutMs ?? 10 * 60 * 1000);
      const onCancel = opts.cancel?.onCancellationRequested(() => {
        cancelled = true;
        child.kill();
      });
      child.stdout.on("data", (d: Buffer) => out.push(d));
      child.stderr.on("data", (d: Buffer) => {
        const text = partial + d.toString("utf8");
        const lines = text.split("\n");
        partial = lines.pop() ?? "";
        for (const l of lines) {
          const p = parseProgressLine(l);
          if (p) {
            opts.onProgress?.(p);
          } else if (l.trim()) {
            err += l + "\n";
          }
        }
      });
      child.on("error", (e: NodeJS.ErrnoException) => {
        clearTimeout(timer);
        onCancel?.dispose();
        if (e.code === "ENOENT") {
          reject(
            new LcwError(
              `lcw not found (${bin}). Build it with \`cargo build -p lcw-cli --release\` and set "livewalk.lcwPath".`,
              null,
              "",
            ),
          );
        } else {
          reject(new LcwError(`running ${bin}: ${e.message}`, null, ""));
        }
      });
      if (opts.stdin !== undefined) {
        child.stdin.end(opts.stdin);
      } else {
        child.stdin.end();
      }
      child.on("close", (code) => {
        clearTimeout(timer);
        onCancel?.dispose();
        err += partial;
        const stdout = Buffer.concat(out).toString("utf8");
        this.output.appendLine(
          `lcw ${args.join(" ")} -> ${cancelled ? "cancelled" : `exit ${code}`} in ${Date.now() - started} ms (${stdout.length} bytes)`,
        );
        if (err.trim()) {
          this.output.appendLine(err.trimEnd());
        }
        if (cancelled) {
          reject(new LcwError(CANCELLED, code, ""));
        } else if (code === 0) {
          resolve(stdout);
        } else {
          const tail = err.trim().split("\n").slice(-6).join("\n");
          reject(new LcwError(tail || `lcw exited with code ${code}`, code, stdout));
        }
      });
    });
  }

  async json<T>(args: string[], opts: RunOptions = {}): Promise<T> {
    const stdout = await this.run(args, opts);
    return JSON.parse(stdout) as T;
  }
}

function onPath(cmd: string): boolean {
  const dirs = (process.env.PATH ?? "").split(path.delimiter).filter(Boolean);
  const exts = process.platform === "win32" ? [".exe", ".cmd", ""] : [""];
  return dirs.some((d) => exts.some((e) => fs.existsSync(path.join(d, cmd + e))));
}

/** Candidates `lcw` printed for an ambiguous/unknown symbol (exit 2 JSON), if any. */
export function candidatesFrom(e: unknown): string[] {
  if (!(e instanceof LcwError) || e.code !== EXIT_AMBIGUOUS) {
    return [];
  }
  try {
    const v = JSON.parse(e.stdout) as { candidates?: { qualified_name?: string }[] };
    return (v.candidates ?? []).map((c) => c.qualified_name ?? "").filter(Boolean);
  } catch {
    return [];
  }
}
