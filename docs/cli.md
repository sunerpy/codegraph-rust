# CLI Subcommand Reference

`codegraph` ships 22 subcommands. All commands accept `--help` for usage details.

## Path Convention

- **Positional or `-p/--path`:** `init`, `uninit`, `index`, `sync`, `status`,
  `callers`, `callees`, `impact`, `affected`, `unlock`, `check`, `export`.
- **`-p/--path` only:** `query`, `files`, `serve`, `audit`.
- **No project path:** `install`, `uninstall`, `skill`, `version`, `self-update`,
  `completions`.

---

## Full Subcommand Table

| Subcommand        | Purpose                                                                                   | Key flags                                                                                                                     |
| ----------------- | ----------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `install`         | Write the codegraph MCP server into each AI agent's config                                | `-t/--target`, `-l/--location`, `--global`, `--local`, `-y/--yes`, `--no-permissions`, `--print-config <id>`, `--prompt-hook` |
| `uninstall`       | Remove codegraph from agent configs (inverse of `install`)                                | `-t/--target`, `-l/--location`, `--global`, `--local`, `-y/--yes`                                                             |
| `skill`           | Install / update / uninstall / check the embedded agent skill                             | `<action>` (install, update, uninstall, status)                                                                               |
| `skill install`   | Write the embedded SKILL.md into each agent's skill directory                             | `-t/--target`, `--global`, `--local`, `-y/--yes`                                                                              |
| `skill update`    | Refresh the installed skill when unchanged by the user                                    | `-t/--target`, `--global`, `--local`, `--force`                                                                               |
| `skill uninstall` | Remove the skill from agent skill directories                                             | `-t/--target`, `--global`, `--local`, `-y/--yes`                                                                              |
| `skill status`    | Report install state per agent (up to date / locally modified / outdated / not installed) | `-t/--target`, `--global`, `--local`                                                                                          |
| `init`            | Initialize `.codegraph/` and run the first full index                                     | `[path]`, `-t/--target` (also write project-level MCP config; default `none`)                                                 |
| `uninit`          | Delete the project's `.codegraph/` index                                                  | `[path]`, `-f/--force`                                                                                                        |
| `index`           | (Re-)index in full                                                                        | `[path]`, `-f/--force`, `-q/--quiet`, `-v/--verbose`                                                                          |
| `sync`            | Sync changes (currently reuses the safe full-index path)                                  | `[path]`, `-q/--quiet`                                                                                                        |
| `status`          | Print index stats (files/nodes/edges/DB size/journal)                                     | `[path]`, `-j/--json`                                                                                                         |
| `query`           | FTS5 + multi-signal scored search                                                         | `<search>`, `-p`, `-l/--limit`, `-k/--kind`, `-j/--json`                                                                      |
| `files`           | List indexed files (tree/flat/grouped)                                                    | `-p`, `--filter`, `--pattern`, `--format`, `--max-depth`, `-j`                                                                |
| `serve`           | Start the server; `--mcp` enters MCP stdio mode                                           | `-p`, `--mcp`, `--no-watch`                                                                                                   |
| `unlock`          | Clear a stale daemon lock (keeps live pids)                                               | `[path]`                                                                                                                      |
| `callers`         | Who calls a symbol (along calls/references/imports)                                       | `<symbol>`, `-p`, `-l`, `-j`                                                                                                  |
| `callees`         | What a symbol calls                                                                       | `<symbol>`, `-p`, `-l`, `-j`                                                                                                  |
| `impact`          | Blast radius of changing a symbol (incoming deps, transitive)                             | `<symbol>`, `-p`, `-d/--depth`, `-j`                                                                                          |
| `affected`        | Given changed files, the affected symbol set                                              | `[files...]`, `-p`, `-d/--depth`, `--filter`                                                                                  |
| `check`           | Detect circular dependencies (each cycle as `a.ts -> b.ts -> a.ts`)                       | `[path]`, `-j/--json`                                                                                                         |
| `audit`           | Read-only Godot resource audit: orphan resources, dangling references, impact             | `-p`, `--orphans`, `--dangling`, `--impact <path>` (≥1 required), `-j/--json`                                                 |
| `export`          | Export the whole code graph as NetworkX node-link JSON                                    | `[path]`, `-o/--out <file>`, `--no-centrality`                                                                                |
| `version`         | Print the codegraph version (same as `--version`)                                         | —                                                                                                                             |
| `self-update`     | Update the binary in place from the latest GitHub release                                 | `--check`, `--force`, `--tag <vX.Y.Z>`                                                                                        |
| `completions`     | Print or install shell completions                                                        | `<shell>` (bash, zsh, fish, powershell, elvish), `--install`                                                                  |

> **Note:** `serve --no-watch` and `CODEGRAPH_NO_WATCH=1` are fully equivalent —
> both disable the live file watcher. See
> [Daemon, watch & environment variables](#daemon-watch--environment-variables)
> for the full env-var reference.

> **`init` / `index` refuse a too-broad root.** Running `codegraph init` or
> `codegraph index` against exactly `$HOME` or the filesystem root (`/`) is
> rejected with an error instead of building a home-wide index — that index
> would be enormous and would make a home-launched `serve --mcp` peg a CPU. Run
> these commands inside a specific project directory.

---

## `codegraph install` / `uninstall` — wire up AI agents

`install` writes the codegraph MCP server entry into each supported agent's
config file; `uninstall` reverses it. No hand-editing of JSON/TOML required.

Supported agents (`ALL_TARGETS` order): **Claude Code, Cursor, Codex CLI,
opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro.** The written MCP
command launches the Rust binary: `command: "codegraph"`, `args: ["serve",
"--mcp"]` (Cursor injects `--path`; Kiro injects `--path` only on a project-local
install).

> **Kiro must be installed project-level.** Kiro launches its stdio MCP
> subprocess from `$HOME` and its `initialize` carries no workspace root and no
> `roots` capability, so a bare `serve --mcp` would degrade to home safe mode.
> Run `codegraph install --target=kiro --local` from each project root — that
> pins the project's absolute `--path`. A **global** Kiro install intentionally
> writes **no** MCP entry (and removes a stale one left by an older version),
> because Kiro CLI does not expand `${workspaceFolder}` in `mcp.json` args: a
> global `--path ${workspaceFolder}` would resolve to a literal, non-existent
> directory and break the watcher and catch-up sync.

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

### Editor adaptation: agents that need a project `--path`

Some editors launch the MCP subprocess from a non-project working directory and
do not advertise the project root in the MCP `initialize` handshake. For those, a
bare `serve --mcp` cannot find the project and degrades to home safe mode, so the
installer pins an explicit `--path`:

- **Cursor** — `install` injects `--path` automatically (local install pins the
  project dir; global uses `${workspaceFolder}`, which Cursor expands).
- **Kiro** — install **project-level** only: `--path` is the concrete project dir.
  A global Kiro install writes no entry, because Kiro CLI does not expand
  `${workspaceFolder}` (see the note above).

### `codegraph init --target` — index and wire an editor in one step

`init` accepts `-t/--target` to also write **project-level** MCP config right after
indexing — the project-scoped analog of `install --target=… --local`. It accepts
the same target values as `install` (csv ids such as `kiro,cursor`, plus `auto`,
`all`, `none`) and **defaults to `none`** (index only, no config written). The
config and its `--path` are written under the project being initialized, even when
the `[path]` argument differs from the current directory. It is idempotent.

```bash
codegraph init                       # index only — no MCP config written (default none)
codegraph init --target=kiro         # index, then write this project's .kiro/settings/mcp.json with --path
codegraph init . --target=kiro,cursor  # index + wire both editors project-level
codegraph init /path/to/proj -t auto  # index that project + wire detected editors there
```

---

## `codegraph skill` — install the agent skill into your agents

`codegraph skill` installs a bundled `SKILL.md` into each supported agent's skill
directory. The skill teaches the agent to use CodeGraph for code research and
project onboarding: reach for `codegraph_explore` before grep/read, use
`codegraph_node` instead of a plain file read on indexed source, and run
`codegraph init` when no `.codegraph/` index is present.

Four actions:

```bash
codegraph skill install   --yes                         # install into all detected agents (global)
codegraph skill install   --target=claude,cursor --yes  # explicit target list
codegraph skill install   --target=auto --local         # project-local skill dirs
codegraph skill update                                  # refresh if unchanged by user
codegraph skill update    --force                       # overwrite even locally-modified files
codegraph skill uninstall --target=claude --yes         # remove from one agent
codegraph skill status                                  # report state for all detected agents
codegraph skill status    --target=all                  # report state for every agent
```

All eight supported agents have a skill directory. `--target` accepts the same
agent ids as `codegraph install` (`claude`, `cursor`, `codex`, `opencode`,
`hermes`, `gemini`, `antigravity`, `kiro`) plus `auto`, `all`, and `none`.
Default location is `--global`; pass `--local` to write into the project tree.
Hermes supports global only (no automatic project-scope for skills).

### Per-agent skill paths

| Agent       | Global skill dir                      | Local skill dir              |
| ----------- | ------------------------------------- | ---------------------------- |
| claude      | `~/.claude/skills/codegraph/`         | `.claude/skills/codegraph/`  |
| cursor      | `~/.cursor/skills/codegraph/`         | `.cursor/skills/codegraph/`  |
| codex       | `~/.agents/skills/codegraph/`         | `.agents/skills/codegraph/`  |
| opencode    | `~/.config/opencode/skill/codegraph/` | `.opencode/skill/codegraph/` |
| hermes      | `~/.hermes/skills/codegraph/`         | (global only)                |
| gemini      | `~/.gemini/skills/codegraph/`         | `.gemini/skills/codegraph/`  |
| antigravity | `~/.gemini/config/skills/codegraph/`  | `.agents/skills/codegraph/`  |
| kiro        | `~/.kiro/skills/codegraph/`           | `.kiro/skills/codegraph/`    |

Note: opencode uses the singular `skill/` directory name (not `skills/`).
Codex and Antigravity share `.agents/skills/` for local installs — writing both
targets locally is idempotent (same content and hash).

### Update semantics

`skill update` compares the installed file's content hash against the embedded
version using a git blob SHA-1:

- **Unchanged** — installed file matches the embedded version; nothing to do.
- **Update** — installed file was written by codegraph and is now outdated; the
  file is refreshed automatically.
- **Locally modified** — the file has been edited by hand (hash drifted from the
  recorded install hash); the file is **skipped** with a "locally modified — use
  `--force` to overwrite" note. Pass `--force` to overwrite anyway.

A small sidecar file (`.codegraph-skill.json`) next to `SKILL.md` records the
installed hash, version, and timestamp. Deleting the sidecar causes the update
check to treat the file as locally modified (conservative).

---

## `codegraph self-update` — upgrade in place from GitHub Releases

Detects your platform, downloads the matching
`codegraph-<version>-<target>.<ext>` asset from the
[Releases](https://github.com/sunerpy/codegraph-rust/releases) page, verifies it,
and atomically replaces the current executable. A plain `self-update` resolves
the latest release directly and upgrades in one run, regardless of how many
versions behind you are.

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

## `codegraph audit` — read-only Godot resource audit

`audit` is a separate, **read-only** analysis surface for Godot projects. It is
computed entirely from the existing graph plus on-disk existence checks — it adds
no extraction and writes no nodes/edges, so it is golden-neutral and never
perturbs `check` or any other output. It is its own subcommand (not a flag on
`check`), so `check`'s parser, `--help`, and output stay unchanged.

At least one mode flag is required:

```bash
codegraph audit --orphans -p .                 # .tres/.tscn resources nothing references
codegraph audit --dangling -p .                # path references whose target is missing on disk
codegraph audit --impact res://buff.tres -p .  # what references a given changed path
codegraph audit --orphans --dangling --json -p .   # combine modes; structured JSON output
```

**How references resolve (why this is path-based).** Godot `.tres`/`.tscn`/
`project.godot` files have no tree-sitter grammar, so they get no `file:` graph
node, and their `ExtResource(...)` references stay in the `unresolved_refs` table
(they never become golden-compared `edges`). The audit therefore keys on the
resource's repo-relative **path** — the `files` row plus the path-shaped
`reference_name`s — not on incoming graph edges.

- **`--orphans`** — a `.tres`/`.tscn` whose path no reference names. Sorted by
  path.
- **`--dangling`** — a path-shaped reference (`reference_name` contains `/` and
  ends in `.tres`/`.tscn`/`.gd`/`.res`, or whose language is a Godot non-script
  language) whose target does not exist on disk under the project root.
  **Exclusion precedence:** (1) a normalized target under `.godot/` or `addons/`
  is excluded first (never dangling, regardless of disk state); (2) then a
  `godot:dynamic:` reference is excluded; (3) only the survivors get the
  disk-exists check. `--dangling` reports missing resource/script **paths**
  only — a reference must look like a path (contain `/`, or carry a resource
  extension) to be a candidate. A bare `[connection] method="_on_X"` signal
  handler name is not a path and is never reported, whether or not the handler
  method exists; signal-method resolution is out of scope.
- **`--impact <path>`** — the reverse-dependency list for a changed path: every
  reference whose normalized target equals it, plus any resolved incoming edges
  on that path's `file:` node (present for `.gd` / grammar-backed files).

This is a static structural report. Runtime `ResourceLoader` load-verification
is out of scope (that is Godot MCP Pro's job).

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
those paths; the reason is surfaced in the log.

The watcher registers per-directory watches only on non-ignored directories,
pruning `node_modules`, `.venv`, `__pycache__`, `target`, `dist`, `.godot`,
`.cache`, `.git`, `.codegraph`, and everything else in the default ignore set,
plus any paths matched by the root `.gitignore`. This pruning applies at any
nesting depth, so an `node_modules` buried several levels deep is never walked.
This keeps the total watch count well inside the OS inotify limit on large trees
and makes daemon startup fast. A newly-created non-ignored directory is picked up
automatically on its create event — no restart required.

The watcher is also auto-disabled when the resolved project root is the
filesystem root (`/`) or the current user's home directory (`$HOME`). This
commonly happens when an IDE or agent (e.g. Kiro) launches `codegraph serve
--mcp` with no `--path` and its working directory resolves to `$HOME`. In that
case the watcher is disabled and the reason is logged. Clients that advertise
MCP roots support are asked for `roots/list`; once the server adopts their first
indexed workspace root, it starts or attaches to that root's shared daemon and
proxies the current stdio session to it. The remedy for clients that do not
support roots: open a specific project folder, let the client send its workspace
root via the MCP `initialize` handshake, or pass `--path <project>` explicitly.
`CODEGRAPH_FORCE_WATCH=1` does **not** override this guard (it only overrides the
WSL2 `/mnt/` disable).

Three escape hatches:

- `CODEGRAPH_FORCE_WATCH=1` — override the WSL2 `/mnt/` auto-disable only. Does
  **not** override the home/root guard or an explicit `CODEGRAPH_NO_WATCH=1`.
- `CODEGRAPH_NO_WATCH=1` (or `serve --no-watch`) — disable watching entirely.
  `--no-watch` and `CODEGRAPH_NO_WATCH=1` are fully equivalent.
- `--path <project>` — pin to a specific project root, avoiding the home/root
  guard entirely.

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
