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

| Subcommand    | Purpose                                                             | Key flags                                                                                                                     |
| ------------- | ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `install`     | Write the codegraph MCP server into each AI agent's config          | `-t/--target`, `-l/--location`, `--global`, `--local`, `-y/--yes`, `--no-permissions`, `--print-config <id>`, `--prompt-hook` |
| `uninstall`   | Remove codegraph from agent configs (inverse of `install`)          | `-t/--target`, `-l/--location`, `--global`, `--local`, `-y/--yes`                                                             |
| `init`        | Initialize `.codegraph/` and run the first full index               | `[path]`                                                                                                                      |
| `uninit`      | Delete the project's `.codegraph/` index                            | `[path]`, `-f/--force`                                                                                                        |
| `index`       | (Re-)index in full                                                  | `[path]`, `-f/--force`, `-q/--quiet`, `-v/--verbose`                                                                          |
| `sync`        | Sync changes (currently reuses the safe full-index path)            | `[path]`, `-q/--quiet`                                                                                                        |
| `status`      | Print index stats (files/nodes/edges/DB size/journal)               | `[path]`, `-j/--json`                                                                                                         |
| `query`       | FTS5 + multi-signal scored search                                   | `<search>`, `-p`, `-l/--limit`, `-k/--kind`, `-j/--json`                                                                      |
| `files`       | List indexed files (tree/flat/grouped)                              | `-p`, `--filter`, `--pattern`, `--format`, `--max-depth`, `-j`                                                                |
| `serve`       | Start the server; `--mcp` enters MCP stdio mode                     | `-p`, `--mcp`, `--no-watch`                                                                                                   |
| `unlock`      | Clear a stale daemon lock (keeps live pids)                         | `[path]`                                                                                                                      |
| `callers`     | Who calls a symbol (along calls/references/imports)                 | `<symbol>`, `-p`, `-l`, `-j`                                                                                                  |
| `callees`     | What a symbol calls                                                 | `<symbol>`, `-p`, `-l`, `-j`                                                                                                  |
| `impact`      | Blast radius of changing a symbol (incoming deps, transitive)       | `<symbol>`, `-p`, `-d/--depth`, `-j`                                                                                          |
| `affected`    | Given changed files, the affected symbol set                        | `[files...]`, `-p`, `-d/--depth`, `--filter`                                                                                  |
| `check`       | Detect circular dependencies (each cycle as `a.ts -> b.ts -> a.ts`) | `[path]`, `-j/--json`                                                                                                         |
| `export`      | Export the whole code graph as NetworkX node-link JSON              | `[path]`, `-o/--out <file>`, `--no-centrality`                                                                                |
| `version`     | Print the codegraph version (same as `--version`)                   | —                                                                                                                             |
| `self-update` | Update the binary in place from the latest GitHub release           | `--check`, `--force`, `--tag <vX.Y.Z>`                                                                                        |
| `completions` | Print or install shell completions                                  | `<shell>` (bash, zsh, fish, powershell, elvish), `--install`                                                                  |

> **Note:** `serve --no-watch` and `CODEGRAPH_NO_WATCH=1` are fully equivalent —
> both disable the live file watcher. See
> [Daemon, watch & environment variables](#daemon-watch--environment-variables)
> for the full env-var reference.

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
codegraph install --prompt-hook                  # also add the Claude UserPromptSubmit hook (opt-in)
codegraph uninstall --target=claude --local      # remove one agent's local config
```

Behavior is idempotent (upsert by the `codegraph` key). `uninstall` removes only
codegraph's own entry and leaves other MCP servers intact. Instruction files are
delimited by `<!-- CODEGRAPH_START -->`/`<!-- CODEGRAPH_END -->` markers.

**`--prompt-hook` (opt-in, Claude Code only).** Passing `--prompt-hook` writes an
additional `UserPromptSubmit` hook into Claude Code's config. Before each prompt
the hook calls `codegraph prompt-hook`, which runs `codegraph_explore` against the
nearest index and prepends relevant structural context to the prompt. This flag is
**off by default** and is never implied by `--yes` — you must pass it explicitly.
No other agent configs are affected.

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

Generates shell completion scripts. Without `--install`, the script prints to
stdout so you can pipe or redirect it wherever you want. With `--install`, the
command writes the script to the standard per-shell location and tells you where.

```bash
codegraph completions bash        # print to stdout
codegraph completions zsh
codegraph completions fish
codegraph completions powershell
codegraph completions elvish

codegraph completions bash --install        # write to the standard location + report path
codegraph completions zsh --install
codegraph completions fish --install
codegraph completions powershell --install
codegraph completions elvish --install
```

`--install` is **idempotent** — re-running it overwrites the completion file in
place and never adds duplicate lines to any rc or profile file. Safe to run again
after a codegraph upgrade.

The design writes a **completion file** and, where needed, a single
**source/dot-source reference** in the shell rc — it does not paste the full
completion script inline into rc files. This keeps rc files small, makes upgrades
a simple file-overwrite, and avoids the PowerShell `UsingMustBeAtStartOfScript`
error that fires when `using namespace` lines land in the middle of a non-empty
`$PROFILE` (see the PowerShell section below).

### Bash

**One command:**

```bash
codegraph completions bash --install
```

Writes to `${XDG_DATA_HOME:-~/.local/share}/bash-completion/completions/codegraph`.
The bash-completion package auto-loads every file in that directory — no `.bashrc`
edit required. Open a new shell and Tab completion works.

**Manual fallback:**

```bash
codegraph completions bash > ~/.local/share/bash-completion/completions/codegraph
```

Or, for the current session only (not persisted across reboots):

```bash
source <(codegraph completions bash)
```

### Zsh

**One command:**

```bash
codegraph completions zsh --install
```

Writes to `~/.zfunc/_codegraph`. If `~/.zfunc` is not yet on your `$fpath`, add
this line to `~/.zshrc` **before** the `compinit` call (the command reminds you
if it detects it's missing):

```zsh
fpath+=~/.zfunc
```

Then open a new shell or run `exec zsh`.

**Manual fallback:**

```bash
codegraph completions zsh > ~/.zfunc/_codegraph
# then ensure fpath+=~/.zfunc is in ~/.zshrc before compinit
```

### Fish

**One command:**

```bash
codegraph completions fish --install
```

Writes to `~/.config/fish/completions/codegraph.fish`. Fish auto-loads every
file in that directory — no `config.fish` edit needed. Open a new shell and Tab
completion works immediately.

**Manual fallback:**

```bash
codegraph completions fish > ~/.config/fish/completions/codegraph.fish
```

### PowerShell

**One command:**

```powershell
codegraph completions powershell --install
```

This does two things:

1. Writes the completion script to a **separate file**:
   `%LOCALAPPDATA%\codegraph\completion.ps1`
2. Appends a single idempotent dot-source line to `$PROFILE`:
   `. "<absolute-path-to-completion.ps1>"`

Re-running keeps exactly one dot-source line in `$PROFILE`.

**Why a separate file, not inline?** The script generated by clap_complete begins
with `using namespace System.Management.Automation`. PowerShell requires `using`
statements at the very start of a script; appending them to a non-empty `$PROFILE`
raises `UsingMustBeAtStartOfScript`. Writing to a separate `.ps1` file (where
`using` is legal at the file's start) and dot-sourcing it sidesteps this entirely.

**Manual fallback:**

```powershell
# 1. Write the script to its own file
codegraph completions powershell > "$env:LOCALAPPDATA\codegraph\completion.ps1"

# 2. Add a dot-source line to $PROFILE (run once)
Add-Content $PROFILE "`n. `"$env:LOCALAPPDATA\codegraph\completion.ps1`""
```

**Tab-completion tip:** PowerShell's default Tab key cycles through candidates one
at a time. To get a menu listing all options at once, press `Ctrl+Space`, or add
this to `$PROFILE`:

```powershell
Set-PSReadLineKeyHandler -Key Tab -Function MenuComplete
```

### Elvish

**One command:**

```bash
codegraph completions elvish --install
```

Writes to `~/.config/codegraph/completion.elv`. Elvish does not have an
auto-load directory for completions, so you need to source the file manually.
Add this line to `~/.config/elvish/rc.elv`:

```elvish
eval (slurp < ~/.config/codegraph/completion.elv)
```

**Manual fallback:**

```bash
codegraph completions elvish > ~/.config/codegraph/completion.elv
# then add: eval (slurp < ~/.config/codegraph/completion.elv) to ~/.config/elvish/rc.elv
```

---

## Daemon, watch & environment variables

### How `serve --mcp` chooses a run mode

The launcher selects a mode in this exact order:

1. `CODEGRAPH_NO_DAEMON=1` is set → **Direct** (foreground, no daemon ever spawned)
2. No `.codegraph/` directory in the project → **Direct** (nothing to share yet)
3. Otherwise → **SpawnOrProxy**: spawn a new shared detached daemon, or proxy to one already running

> `CODEGRAPH_DAEMON_INTERNAL=1` is **internal-only** — it is set automatically on
> the daemon child process by the spawner. Do not set it yourself.

### Detached daemon lifecycle

When the daemon starts, it detaches from the parent process group (Unix:
`process_group(0)`; Windows: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`).
Its stdout and stderr are appended to `.codegraph/daemon.log`. The Unix socket is
at `.codegraph/daemon.sock`; the pid/lock file lives alongside it.

If the daemon crashes and leaves a stale lock:

```bash
codegraph unlock [path]   # removes the stale lock file; live daemon pids are left intact
```

To suppress the daemon entirely in CI or scripted contexts:

```bash
CODEGRAPH_NO_DAEMON=1 codegraph serve --mcp --path /path/to/project
```

### Live file watch

The daemon watches the project for file changes and re-indexes automatically.
Changes are debounced before the re-index triggers. On WSL2, watching files under
`/mnt/` is automatically disabled because recursive `fs.watch` is too slow on
those paths; the reason is surfaced in the log. Two escape hatches:

- `CODEGRAPH_FORCE_WATCH=1` — override the WSL2 `/mnt/` auto-disable. Does **not**
  override an explicit `CODEGRAPH_NO_WATCH=1`.
- `CODEGRAPH_NO_WATCH=1` (or `serve --no-watch`) — disable watching entirely.
  `--no-watch` and `CODEGRAPH_NO_WATCH=1` are fully equivalent.

### Environment variable reference

| Variable                           | Default   | Clamp range  | Meaning                                                          |
| ---------------------------------- | --------- | ------------ | ---------------------------------------------------------------- |
| `CODEGRAPH_NO_DAEMON`              | —         | —            | Force foreground Direct mode; never spawn or proxy a daemon      |
| `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` | `300000`  | 1000–3600000 | Exit after this long with no connected clients                   |
| `CODEGRAPH_DAEMON_MAX_IDLE_MS`     | `1800000` | 1000–3600000 | Hard cap on total daemon lifetime when idle                      |
| `CODEGRAPH_DAEMON_CLIENT_SWEEP_MS` | `30000`   | 50–600000    | How often the daemon sweeps for dead clients                     |
| `CODEGRAPH_WATCH_DEBOUNCE_MS`      | `2000`    | 100–60000    | File-change debounce window before a re-index triggers           |
| `CODEGRAPH_NO_WATCH`               | —         | —            | Disable the live file watcher (equivalent to `serve --no-watch`) |
| `CODEGRAPH_FORCE_WATCH`            | —         | —            | Override WSL2 `/mnt/` auto-disable; does not override `NO_WATCH` |

Values outside the clamp range are silently clamped to the nearest bound.

### Custom extension mapping (`.codegraph/codegraph.json`)

Place a `codegraph.json` inside the `.codegraph/` directory of any project to
teach CodeGraph how to treat files with non-standard extensions:

```jsonc
{
  "extensions": {
    ".x": "lua",
    ".blade": "php",
  },
}
```

Rules:

- Keys are normalized before matching: the leading `.` is stripped and the result
  is lowercased (so `.X` and `.x` are the same key).
- Language names must match the internal `Language` enum (serde names). Unknown
  language names are **silently skipped**.
- Config resolution walks up the directory tree from each source file; the nearest
  `.codegraph/codegraph.json` wins. Results are mtime-cached — absent files are
  cached too, so no repeated I/O on every lookup.
- A malformed JSON file is ignored and the error is logged; it does not abort
  indexing.

### `--prompt-hook` detail

`codegraph prompt-hook` is a hidden subcommand (not shown in `--help`). It accepts
a query as an argument or reads one from stdin, runs `codegraph_explore` against
the nearest index, and prints structured context. If no index is found it prints a
graceful message and exits cleanly; same if no query is provided.

`codegraph install --prompt-hook` writes a `UserPromptSubmit` hook into Claude
Code's config that calls `codegraph prompt-hook` before each prompt. This is
**off by default**. `--yes` never implies it — you must pass `--prompt-hook`
explicitly. The hook entry is delimited by the same
`<!-- CODEGRAPH_START -->`/`<!-- CODEGRAPH_END -->` markers used for the MCP
entry. No other agent configs are touched.

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
