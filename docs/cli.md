# CLI Subcommand Reference

`codegraph` ships 20 subcommands. All commands accept `--help` for usage details.

## Path Convention

- **Positional or `-p/--path`:** `init`, `uninit`, `index`, `sync`, `status`,
  `callers`, `callees`, `impact`, `affected`, `unlock`, `check`, `export`.
- **`-p/--path` only:** `query`, `files`, `serve`.
- **No project path:** `install`, `uninstall`, `version`, `self-update`,
  `completions`.

---

## Full Subcommand Table

| Subcommand    | Purpose                                                             | Key flags                                                                                                    |
| ------------- | ------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `install`     | Write the codegraph MCP server into each AI agent's config          | `-t/--target`, `-l/--location`, `--global`, `--local`, `-y/--yes`, `--no-permissions`, `--print-config <id>` |
| `uninstall`   | Remove codegraph from agent configs (inverse of `install`)          | `-t/--target`, `-l/--location`, `--global`, `--local`, `-y/--yes`                                            |
| `init`        | Initialize `.codegraph/` and run the first full index               | `[path]`                                                                                                     |
| `uninit`      | Delete the project's `.codegraph/` index                            | `[path]`, `-f/--force`                                                                                       |
| `index`       | (Re-)index in full                                                  | `[path]`, `-f/--force`, `-q/--quiet`, `-v/--verbose`                                                         |
| `sync`        | Sync changes (currently reuses the safe full-index path)            | `[path]`, `-q/--quiet`                                                                                       |
| `status`      | Print index stats (files/nodes/edges/DB size/journal)               | `[path]`, `-j/--json`                                                                                        |
| `query`       | FTS5 + multi-signal scored search                                   | `<search>`, `-p`, `-l/--limit`, `-k/--kind`, `-j/--json`                                                     |
| `files`       | List indexed files (tree/flat/grouped)                              | `-p`, `--filter`, `--pattern`, `--format`, `--max-depth`, `-j`                                               |
| `serve`       | Start the server; `--mcp` enters MCP stdio mode                     | `-p`, `--mcp`, `--no-watch`                                                                                  |
| `unlock`      | Clear a stale daemon lock (keeps live pids)                         | `[path]`                                                                                                     |
| `callers`     | Who calls a symbol (along calls/references/imports)                 | `<symbol>`, `-p`, `-l`, `-j`                                                                                 |
| `callees`     | What a symbol calls                                                 | `<symbol>`, `-p`, `-l`, `-j`                                                                                 |
| `impact`      | Blast radius of changing a symbol (incoming deps, transitive)       | `<symbol>`, `-p`, `-d/--depth`, `-j`                                                                         |
| `affected`    | Given changed files, the affected symbol set                        | `[files...]`, `-p`, `-d/--depth`, `--filter`                                                                 |
| `check`       | Detect circular dependencies (each cycle as `a.ts -> b.ts -> a.ts`) | `[path]`, `-j/--json`                                                                                        |
| `export`      | Export the whole code graph as NetworkX node-link JSON              | `[path]`, `-o/--out <file>`, `--no-centrality`                                                               |
| `version`     | Print the codegraph version (same as `--version`)                   | —                                                                                                            |
| `self-update` | Update the binary in place from the latest GitHub release           | `--check`, `--force`, `--tag <vX.Y.Z>`                                                                       |
| `completions` | Print shell completions to stdout                                   | `<shell>` (bash, zsh, fish, powershell, elvish)                                                              |

---

## `codegraph install` / `uninstall` — wire up AI agents

`install` writes the codegraph MCP server entry into each supported agent's
config file; `uninstall` reverses it. No hand-editing of JSON/TOML required.

Supported agents (`ALL_TARGETS` order): **Claude Code, Cursor, Codex CLI,
opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro.** The written MCP
command launches the Rust binary: `command: "codegraph"`, `args: ["serve",
"--mcp"]` (Cursor also injects `--path`).

```bash
codegraph install --yes                          # auto-detect installed agents, global
codegraph install --target=claude,cursor --yes   # explicit list
codegraph install --target=auto --local          # detected agents, project-local
codegraph install --print-config cursor          # print the snippet only, no write
codegraph uninstall --target=claude --local      # remove one agent's local config
```

Behavior is idempotent (upsert by the `codegraph` key). `uninstall` removes only
codegraph's own entry and leaves other MCP servers intact. Instruction files are
delimited by `<!-- CODEGRAPH_START -->`/`<!-- CODEGRAPH_END -->` markers.

---

## `codegraph self-update` — upgrade in place from GitHub Releases

Detects your platform, downloads the matching
`codegraph-<version>-<target>.<ext>` asset from the
[Releases](https://github.com/sunerpy/codegraph-rust/releases) page, verifies it,
and atomically replaces the current executable.

```bash
codegraph self-update              # update to the latest release
codegraph self-update --check      # only report whether a newer version exists
codegraph self-update --force      # reinstall even if already current
codegraph self-update --tag v0.3.0 # pin a specific release tag
```

If codegraph lives on a root-owned path (e.g. `/usr/local/bin`), run with
appropriate privileges. Windows assets are `.zip`; if `self-update` cannot fetch
them automatically, reinstall via
`cargo install --git https://github.com/sunerpy/codegraph-rust codegraph-rs`.

---

## `codegraph export` — whole-graph export + centrality

Exports the entire code graph as **NetworkX node-link JSON**
(`{directed, multigraph, graph, nodes, links, edges}`).

```bash
codegraph export --path . --out graph.json   # with deterministic centrality (default)
codegraph export --path .                    # print to stdout
codegraph export --path . --no-centrality    # skip the PageRank pass (faster on huge graphs)
```

**Node fields:** `id`, `label` (=name), `kind`, `file_type` (`File` -> `"file"`,
other symbols -> `"code"`), `source_file` (=file_path), `qualified_name`,
`language`, `start_line`, `end_line`, `signature`; with centrality, also
`pagerank`, `god_score` (=pagerank), `in_degree`, `out_degree`.

**Edge fields** (under both `links` and `edges`): `source`, `target`,
`relation` (=kind), `kind`, `line`, `metadata`.

Centrality is a deterministic pure-Rust PageRank (damping 0.85, 30 iterations,
id-sorted order — byte-reproducible), computed over dependency edges only
(excluding structural `contains` edges). Higher `god_score` = more central
("god node"), i.e. higher change-risk and read priority.

---

## `codegraph completions` — shell completions

Prints completion scripts to stdout. Pipe to your shell's completions directory
or source inline.

```bash
codegraph completions bash        # Bash
codegraph completions zsh         # Zsh
codegraph completions fish        # Fish
codegraph completions powershell  # PowerShell
codegraph completions elvish      # Elvish
```

**Quick setup examples:**

```bash
# Bash — add to ~/.bashrc
source <(codegraph completions bash)

# Zsh — save to a completions directory on your $fpath
codegraph completions zsh > "${fpath[1]}/_codegraph"

# Fish
codegraph completions fish > ~/.config/fish/completions/codegraph.fish
```

---

## Supported languages

The language set is the fixed `LANGUAGES` constant, in three extraction tiers.

**tree-sitter grammars (regular symbol extraction):** TypeScript, TSX, JavaScript,
JSX, Python, Go, Rust, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin, Dart, Pascal,
Scala, Lua, Luau, Objective-C, R.

**embedded / custom extractors:** Vue, Svelte, Astro, Razor, Liquid, MyBatis XML,
DFM/FMX.

**file-level-only (0 symbols at the extract stage):** YAML, Twig, Properties.

`html` / `css` / `json` / `sql` are not in the extraction model and are not
extracted. See [`grammar-manifest.md`](grammar-manifest.md) and
[`embedded-extraction.md`](embedded-extraction.md) for the full grammar manifest
and embedded-language extraction detail.

---

## Scope and non-goals

**Does:** deterministic code-structure extraction, cross-file resolution, graph
traversal, FTS5 search, whole-graph export / centrality, MCP/CLI surfaces, and
golden byte-stable output.

**Does not:**

- No AI / vector / embedding / LLM path anywhere inside the binary (hard
  constraint, guardrail-enforced; LLM combination happens in the orchestration
  layer).
- No semantic search; search is FTS5 + deterministic scoring only.
- Concrete `FrameworkResolver`s exist for React / Vue / NestJS; other framework
  resolution is deferred.
- No languages beyond the fixed `LANGUAGES` set.
