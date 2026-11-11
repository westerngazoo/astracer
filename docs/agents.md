# Livewalk for AI coding agents

`lcw` (CLI) and `lcw-mcp` (MCP server) answer structural questions about a
codebase from its call graph, so an agent can jump to the exact lines it needs
instead of reading whole files. Both return the same JSON (`"schema": 1`),
built by one crate (`lcw-agent`) on top of `lcw-query`.

## Build

```bash
cd /Users/goose/projects/livewalk
cargo build --release -p lcw-cli -p lcw-mcp
```

Or `scripts/livewalk-dev.sh build-all`, which also checks the desktop app.
Binaries: `target/release/lcw` and `target/release/lcw-mcp`.

## Which command for which question

| Question | CLI | MCP tool |
|---|---|---|
| Exactly which lines define `X`? (to read or edit only those) | `lcw span X` / `--code` | `span` |
| What is `X`, who calls it, what does it call? | `lcw explain X --json` | `explain` |
| Who calls `X`, and on which line? | `lcw callers X` | `callers` |
| What does `X` call, in order? | `lcw calls X` | `callees` |
| What can break if I change `X`? | `lcw impact X --depth 3` | `impact` |
| How does control get from `main` to `X`? | `lcw flow X --from main` | `flow` |
| Where does the program start? | `lcw entries` | `entries` |
| How is the code organized? | `lcw outline --depth 1` | `outline` |
| What is the symbol actually called? | `lcw search name` | `search_symbol` |

A good loop before editing a function: `search` (if unsure of the name), then
`span` to get `file:start-end`, `explain` for its neighbours, and `impact`
before changing a signature. Read the returned range, not the whole file.

## Output conventions

- Every JSON object has `"schema": 1` at the top level.
- Locations are strings: `at` = definition `file:line`, `site` = call site
  `file:line` (in the caller's file), `span` = `file:start-end`. Spans include
  leading attributes and doc comments.
- `--budget <tokens>` shrinks lists until the output fits (about 4 bytes per
  token); a cut list gets a sibling `"<list>_more": N` in JSON and
  `... +N more` in text. `--compact` = a 1500-token budget. Both are global
  flags: `lcw --compact explain X --json`.
- JSON is single-line; add the global `--pretty` to indent it for humans.
- External / unresolved callees (`Ok`, std methods, other crates) are left
  out of `explain` and only counted in `callees_external`; `--external`
  (MCP: `"external": true`) lists them.
- An ambiguous or unknown symbol prints candidates instead of failing silently,
  and the command exits with code 2:
  `{"schema":1,"error":"ambiguous","query":"new","candidates":[{"qualified_name":…,"at":…}]}`.
  Re-run with one of the qualified names.
- stdout carries only the answer. Notes (for example "re-analyzed because 3
  files changed") go to stderr.

## Speed: the cached graph

Each analysis writes `graph.sqlite` under `~/.cache/livewalk/{hash}-{lang}/`
(or `$LIVEWALK_CACHE_HOME/livewalk/`). Query commands load it directly when it
is fresh: written for the same root, same number of source files, and no
source file modified since the analysis started. Otherwise they re-analyze
(incrementally, reusing unchanged files) and rewrite it. `--no-cache` forces a
full analysis.

## MCP server

`lcw-mcp` speaks MCP over stdio (newline-delimited JSON-RPC: `initialize`,
`tools/list`, `tools/call`). Every tool takes `repo` (absolute path) plus
optional `language` (`rust`, `typescript`, `python`, `go`, `cpp`), `budget`
(default 4000 tokens, `0` = unlimited) and `refresh` (force re-analysis). It
keeps each repo's graph in memory and re-checks source mtimes at most every
2 seconds, so repeated questions are answered from memory. Paths in results
are absolute. Set `LCW_MCP_LOG=info` to log tool timings to stderr.

Tools: `explain`, `callers`, `callees`, `impact`, `span`, `flow`, `outline`,
`entries`, `search_symbol`.

### Cursor

Add to `~/.cursor/mcp.json` (or `.cursor/mcp.json` in a project):

```json
{
  "mcpServers": {
    "livewalk": {
      "command": "/Users/goose/projects/livewalk/target/release/lcw-mcp",
      "args": []
    }
  }
}
```

### Claude Code

```bash
claude mcp add --scope user livewalk /Users/goose/projects/livewalk/target/release/lcw-mcp
claude mcp list
```

### Smoke test by hand

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"span","arguments":{"repo":"/Users/goose/projects/livewalk","symbol":"Engine::analyze"}}}' | /Users/goose/projects/livewalk/target/release/lcw-mcp
```

## Rule snippet for agent instructions

Paste into a project's `AGENTS.md` / `CLAUDE.md` / Cursor rule:

```text
Before reading a whole source file, locate code with Livewalk:
- `lcw span <symbol>` gives file:start-end; read only that range.
- `lcw explain <symbol> --json` gives callers/callees with call sites.
- `lcw impact <symbol>` before changing a function's behavior or signature.
- `lcw search <name>` when unsure of a name. Exit code 2 means ambiguous/unknown: pick a qualified name from the candidates.
Use `--compact` (or `--budget N`) to keep output small. With the livewalk MCP server, use its tools of the same names.
```
