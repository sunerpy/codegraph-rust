# CLI Subcommand Reference

`codegraph` ships 22 subcommands. All commands accept `--help` for usage details.

## Path Convention

- **Positional or `-p/--path`:** `init`, `uninit`, `index`, `sync`, `status`,
  `callers`, `callees`, `impact`, `affected`, `unlock`, `check`, `export`.
- **`-p/--path` only:** `search`, `files`, `serve`, `audit`.
- **No project path:** `install`, `uninstall`, `skill`, `version`, `self-update`,
  `completions`.

---

## Full Subcommand Table

| Subcommand        | Purpose                                                                                   | Key flags                                                                                                                                  |
| ----------------- | ----------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `install`         | Write the codegraph MCP server into each AI agent's config                                | `-t/--target`, `-l/--location`, `--global`, `--local`, `-y/--yes`, `--no-permissions`, `--print-config <id>`, `--prompt-hook`              |
| `uninstall`       | Remove codegraph from agent configs (inverse of `install`)                                | `-t/--target`, `-l/--location`, `--global`, `--local`, `-y/--yes`                                                                          |
| `skill`           | Install / update / uninstall / check the embedded agent skill                             | `<action>` (install, update, uninstall, status)                                                                                            |
| `skill install`   | Write the embedded SKILL.md into each agent's skill directory                             | `-t/--target`, `--global`, `--local`, `-y/--yes`                                                                                           |
| `skill update`    | Preview versions/line counts and refresh the installed skill                              | `-t/--target`, `--global`, `--local`, `--force`, `--diff`, `--dry-run`                                                                     |
| `skill uninstall` | Remove the skill from agent skill directories                                             | `-t/--target`, `--global`, `--local`, `-y/--yes`                                                                                           |
| `skill status`    | Report install state per agent (up to date / locally modified / outdated / not installed) | `-t/--target`, `--global`, `--local`                                                                                                       |
| `init`            | Initialize `.codegraph/` and run the first full index                                     | `[path]`, `-t/--target` (also write project-level MCP config; default `none`)                                                              |
| `uninit`          | Delete the project's `.codegraph/` index                                                  | `[path]`, `-f/--force`                                                                                                                     |
| `index`           | (Re-)index in full                                                                        | `[path]`, `-f/--force`, `-q/--quiet`, `-v/--verbose`                                                                                       |
| `sync`            | Incremental sync: re-index only changed files, drop deleted ones, re-resolve              | `[path]`, `-q/--quiet`                                                                                                                     |
| `status`          | Print index stats (files/nodes/edges/DB size/journal)                                     | `[path]`, `-j/--json`                                                                                                                      |
| `search`          | FTS5 + multi-signal scored symbol search                                                  | `<search>`, `-p`, `-l/--limit`, `-k/--kind`, `-j/--json`, `--strict`                                                                       |
| `files`           | List indexed files (tree/flat/grouped)                                                    | `-p`, `--filter <DIR>`, `--language <LANG>`, `--pattern`, `--format`, `--max-depth`, `-j`                                                  |
| `serve`           | Start the server; `--mcp` enters MCP stdio mode                                           | `-p`, `--mcp`, `--no-watch`                                                                                                                |
| `unlock`          | Clear a stale daemon lock (keeps live pids)                                               | `[path]`                                                                                                                                   |
| `callers`         | Who calls a symbol (along calls/references/imports)                                       | `<symbol>`, `-p`, `-l`, `-j`, `--strict`, `--file <FILE>`                                                                                  |
| `callees`         | What a symbol calls                                                                       | `<symbol>`, `-p`, `-l`, `-j`, `--strict`, `--file <FILE>`                                                                                  |
| `impact`          | Blast radius of changing a symbol (incoming deps, transitive)                             | `<symbol>`, `-p`, `-d/--depth`, `-j`, `--strict`, `--file <FILE>`                                                                          |
| `affected`        | Given changed files, the affected symbol set                                              | `[files...]`, `-p`, `-d/--depth`, `--filter`                                                                                               |
| `check`           | Detect circular dependencies (each cycle as `a.ts -> b.ts -> a.ts`)                       | `[path]`, `-j/--json`                                                                                                                      |
| `audit`           | Read-only Godot resource audit: orphan resources, dangling references, impact             | `-p`, `--orphans`, `--dangling`, `--impact <path>` (≥1 required), `--verify-plan`, `--include <PREFIX>`, `--exclude <PREFIX>`, `-j/--json` |
| `export`          | Export the whole code graph as NetworkX node-link JSON                                    | `[path]`, `-o/--out <file>`, `--no-centrality`                                                                                             |
| `version`         | Print the codegraph version (same as `--version`)                                         | —                                                                                                                                          |
| `self-update`     | Update the binary in place from the latest GitHub release                                 | `--check`, `--force`, `--tag <vX.Y.Z>`                                                                                                     |
| `completions`     | Print or install shell completions                                                        | `<shell>` (bash, zsh, fish, powershell, elvish), `--install`                                                                               |

`query` remains a visible backward-compatible alias for `search`; new scripts,
documentation, and diagnostics should use `search`.

> **Note:** `serve --no-watch` and `CODEGRAPH_NO_WATCH=1` are fully equivalent —
> both disable the live file watcher. See
> [Daemon, watch & environment variables](#daemon-watch--environment-variables)
> for the full env-var reference.

> **`init` / `index` refuse a too-broad root.** Running `codegraph init` or
> `codegraph index` against exactly `$HOME` or the filesystem root (`/`) is
> rejected with an error instead of building a home-wide index — that index
> would be enormous and would make a home-launched `serve --mcp` peg a CPU. Run
> these commands inside a specific project directory.

> **`affected` output fields.** `codegraph affected` always emits JSON on stdout
> (there is no `--json` flag). Its keys are `changedFiles` (the input files),
> `affectedTests` (only the traversed dependents that look like test files, per
> `--filter` or the default test-path heuristics), `affectedFiles` (the
> sorted+deduped union of ALL traversed dependents plus the test set, so
> `affectedFiles ⊇ affectedTests`), and `totalDependentsTraversed` (the traversal
> count). `affectedFiles` LISTS the complete affected set — agreeing with
> `impact` / `audit --impact` — where previously only the count was surfaced.

---

## `codegraph install` / `uninstall` — wire up AI agents

`install` writes the codegraph MCP server entry into each supported agent's
config file; `uninstall` reverses it. No hand-editing of JSON/TOML required.

Supported agents (`ALL_TARGETS` order): **Claude Code, Cursor, Codex CLI,
opencode, Hermes Agent, Gemini CLI, Antigravity IDE, Kiro, Trae, Qoder, Zed,
VS Code (`vscode`), GitHub Copilot CLI (`copilot-cli`), JetBrains
(`jetbrains`).**
The written MCP command launches the Rust binary: `command: "codegraph"`, `args: ["serve",
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

> **The three GitHub Copilot targets.** They share the Copilot MCP surface but
> disagree on both the wrapper key and the available locations:
>
> | target        | file                                         | wrapper      | locations      |
> | ------------- | -------------------------------------------- | ------------ | -------------- |
> | `vscode`      | `.vscode/mcp.json` (local)                   | `servers`    | local + global |
> |               | `<config_base>/Code/User/mcp.json` (global)  | `servers`    |                |
> | `copilot-cli` | `~/.copilot/mcp-config.json`                 | `mcpServers` | global only    |
> | `jetbrains`   | `~/.config/github-copilot/intellij/mcp.json` | `servers`    | global only    |
>
> The VS Code **global** entry is a bare `serve --mcp` and deliberately does NOT
> use `${workspaceFolder}`: VS Code expands that variable only in a WORKSPACE
> `mcp.json`, so in the user-level file it would stay literal and point the server
> at a nonexistent directory. Run `codegraph init --target=vscode` per project for
> live watch. The Copilot CLI entry additionally carries `"tools": ["*"]`, without
> which the CLI registers the server but exposes none of its tools.

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
- **Zed** — Zed's global `context_servers` config cannot inject a per-project
  path (no `${workspaceFolder}` expansion). A global `codegraph install --target=zed`
  writes a bare entry (read-only off any existing index). To pin a specific project,
  run `codegraph init --target=zed` inside the project — this writes
  `.zed/settings.json` with an absolute `--path` and is the **only** way to get
  live per-project indexing in Zed.

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
`codegraph_node` instead of a plain file read on indexed source, inspect
`codegraph status` before lifecycle changes, run `codegraph init` when no usable
index exists, and use `codegraph sync` for ordinary manual catch-up.

Four actions:

```bash
codegraph skill install   --yes                         # install into all detected agents (global)
codegraph skill install   --target=claude,cursor --yes  # explicit target list
codegraph skill install   --target=auto --local         # project-local skill dirs
codegraph skill update                                  # version/+/- summary, then refresh
codegraph skill update    --diff                        # also show unified content diff
codegraph skill update    --dry-run --diff              # preview without writing files
codegraph skill update    --force                       # overwrite even locally-modified files
codegraph skill uninstall --target=claude --yes         # remove from one agent
codegraph skill status                                  # report state for all detected agents
codegraph skill status    --target=all                  # report state for every agent
```

Ten of the eleven install targets have a skill directory. `--target` accepts
those ten agent ids (`claude`, `cursor`, `codex`, `opencode`, `hermes`,
`gemini`, `antigravity`, `kiro`, `trae`, `qoder`) plus `auto`, `all`, and
`none`. Note: `zed` is a valid install target but has **no skill directory**
(MCP config only) — passing `--target=zed` to `codegraph skill` is a no-op.
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
| trae        | `Trae/User/skills/codegraph/`         | `.trae/skills/codegraph/`    |
| qoder       | `~/.agents/skills/codegraph/`         | `.qoder/skills/codegraph/`   |

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

Before any write, `skill update` prints the installed-to-embedded version
transition and deterministic added/removed line counts. Add `--diff` for a
three-context unified diff. Add `--dry-run` to print the identical preview while
leaving both `SKILL.md` and its sidecar untouched. `skill status` includes the
same provenance, for example:

```text
Codex CLI: outdated (0.40.1 -> 0.47.0)
opencode: locally modified (base 0.40.1; embedded 0.47.0)
```

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

## `codegraph files` — list indexed files

Lists the files in the index (tree/flat/grouped). Two independent filters:

```bash
codegraph files -p .                            # all indexed files (tree)
codegraph files -p . --filter src/components     # only files UNDER this directory
codegraph files -p . --language gdscript         # only files of this language
codegraph files -p . --filter src --language go  # combine: Go files under src/
```

- **`--filter <DIR>`** — a **path-prefix** filter: keeps only files whose
  repo-relative path starts with `<DIR>` (a leading `./` is also matched). This
  is a directory filter, not a language filter (it is a faithful port of the
  upstream `--filter <dir>` flag and keeps that meaning).
- **`--language <LANG>`** — keeps only files whose language equals `<LANG>`,
  matching the exact names `status` prints (e.g. `gdscript`, `godot_scene`,
  `godot_resource`, `godot_project`, `python`, `rust`). The match is an exact,
  case-sensitive comparison; a `<LANG>` no file uses yields an empty result with
  no error and no hint.

**Symbol count semantics.** The per-file "symbols" count shown by `files` is the
**live count of graph nodes for that file** (`COUNT(*)` over the `nodes` table),
so it stays consistent with what `search`/`callers`/`callees` see. This matters
for Godot `.tscn`/`.tres` files: their scene/resource marker nodes are added by
the framework resolver after the initial extractor, so the stored
`files.node_count` column (which records only the initial extractor's count) can
read `0` while the graph actually holds those nodes. `files` recomputes the
displayed count from the `nodes` table for display only — it never rewrites the
stored `files.node_count` column, so the golden output is unaffected.

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
codegraph audit --orphans --exclude addons/ -p .   # denoise: drop addons/ results
codegraph audit --dangling --include Data/ -p .    # narrow: keep only Data/ results
codegraph audit --impact res://player.gd --verify-plan --json -p .  # derived load/open plan
```

**`-p` is the project root, not a result filter.** `-p/--path` selects which
project to audit (consistent with every other subcommand). To narrow the
**results**, use the CLI-layer prefix filters:

- **`--include <PREFIX>`** — keep only results whose path is under `<PREFIX>`.
- **`--exclude <PREFIX>`** — drop results whose path is under `<PREFIX>`, e.g.
  `--exclude addons/` to denoise a Godot project's vendored plugin tree.

Both are repeatable and `/`-normalized; `--include` keeps a result if it matches
any include prefix, then `--exclude` drops any that match an exclude prefix. The
filters are applied in the CLI layer over the orphan / dangling / impact lists
(matching `filePath` for orphans, `fromFile` for dangling and impact rows); the
underlying graph functions stay pure, so the report is deterministic.

**How references resolve (why this is path-based).** Godot `.tres`/`.tscn`/
`project.godot` files have no tree-sitter grammar, so they get no `file:` graph
node, and their `ExtResource(...)` references stay in the `unresolved_refs` table
(they never become golden-compared `edges`). The audit therefore keys on the
resource's repo-relative **path** — the `files` row plus the path-shaped
`reference_name`s — not on incoming graph edges.

- **`--orphans`** — a `.tres`/`.tscn` whose path no reference names. Sorted by
  path. In `--json`, each orphan carries `reason` (`no_path_reference`),
  `confidence`, and an optional `note`. `confidence` is a **static** signal:
  `"low"` for Godot resource/scene files (whose inbound references can be
  data-driven numeric ids / DSL paths that static analysis does not follow, so
  "orphan" is not proof of zero use), `"high"` otherwise. It is a structural
  caveat, not a runtime guarantee.
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
  on that path's `file:` node (present for `.gd` / grammar-backed files). In
  `--json`, each affected site carries `fromFile`, `line`, `edgeKind`, `target`
  (the changed path, echoed onto every row), and an optional `edgeSubkind`.
  `edgeKind` surfaces the graph EDGE kind that links the site (`references` /
  `instantiates` for resolved edges; the reference's kind for unresolved refs) —
  it is the structural relation, not a domain-semantic label. `edgeSubkind` is a
  finer **structural** extraction label, present for Godot refs only:
  `script_attach`, `scene_instance`, `ext_resource`, `group_member`,
  `signal_method` (and the reserved `gdscript_load_path`). It records _how_ the
  reference was extracted, NOT a domain/business meaning. When `--impact`
  produces no affected sites for a Godot resource/script path, a `note` field
  (text and JSON) flags that data-driven numeric-id / DSL references are not
  included by default — so "nothing references X" is not proof of zero use.
- **`--verify-plan`** (used with `--impact`) — emits a derived
  `verifyPlan` view reshaping the impact result into a load/open plan:
  `{ changed, loadScripts: [res:// .gd], openScenes: [res:// .tscn], reasons:
[{file, line, edgeKind, edgeSubkind?}] }`. Pure CLI reshape of the impact
  data (no new graph queries); `reasons` carry `edgeSubkind` when present.

This is a static structural report. Runtime `ResourceLoader` load-verification
is out of scope (that is Godot MCP Pro's job).

---

## Symbol lookup failures and `--strict`

`callers`, `callees`, and `impact` require an exact symbol match. If the name
does not exist, they exit non-zero and suggest `codegraph search <name>` for
fuzzy search instead of silently substituting the highest-ranked result.
`--strict` retains its separate contract: after an exact match, an empty result
also exits non-zero.

| Case                           | Default                              | `--strict` |
| ------------------------------ | ------------------------------------ | ---------- |
| No exact match (symbol absent) | Fails                                | Fails      |
| Exact match with results       | Succeeds                             | Succeeds   |
| Exact match with zero results  | Succeeds and prints the empty result | Fails      |

### Ancestor-index retargeting on mutating commands

`index`, `sync`, `uninit`, and `unlock` walk UP from the given path to find an
index, so running one inside an unindexed subdirectory operates on the nearest
indexed ANCESTOR. That is intentional — one index serves a whole tree — but it is
now announced on **stderr** instead of happening silently:

```
Warning: /repo/child has no CodeGraph index, so this command resolved to an
         ancestor index at /repo and will operate on THAT project, not on /repo/child.
         Run `codegraph init /repo/child` first if you meant to give it its own index.
```

It matters most for `uninit --force`, which would otherwise delete an index the
user never named. Stdout stays machine-readable and unchanged.

---

### `--file` — disambiguating same-named definitions

When two files define the same symbol, `callers` / `callees` / `impact` merge
both definitions' relatives into one list. `--file <FILE>` keeps only the
definition declared in that file:

```bash
codegraph callers target --file src/alpha.ts      # only alpha.ts's callers
codegraph callees target --file alpha.ts          # a trailing path suffix works
codegraph impact  target --file src/alpha.ts --json
```

The filter matches the whole project-relative path or any **segment-aligned**
trailing suffix, so `other.ts` never selects `my_other.ts`. Windows separators
and a leading `./` are normalized.

A filter that matches no definition is an **error** naming the files that do
define the symbol — reporting an empty relative-set instead would read as "this
symbol is dead". In `--json`, the applied filter is echoed as `"file"`.

---

## `codegraph impact` — edge counts in `--json`

`impact --json` emits `symbol`, `depth`, `nodeCount`, `edgeCount`,
`resourceEdgeCount`, `affected`, and `godotDynamic`. The two counts split like
this:

- **`edgeCount`** — **all** impact edges: the graph-traversal edges reached from
  the matched symbols, **plus** the Godot static resource edges (a `.tscn` /
  `.tres` / `project.godot` referrer of the target file). It is the total, not
  the code-only figure.
- **`resourceEdgeCount`** — just the resource share of that total. Code edges are
  therefore `edgeCount - resourceEdgeCount`; no third field is needed.

The resource count comes from the same referrer set that gets appended to
`affected` — sorted, deduped, and restricted to files the graph traversal did not
already reach — so the count and the list can never contradict each other, and a
referrer that also has a real graph edge (a GDScript `preload`, say) is counted
once rather than twice. A pure-code target reports `resourceEdgeCount: 0` and an
`edgeCount` identical to what earlier versions produced.

This matters for Godot projects, where the referrers of a `.gd` script live in
resource files that have no tree-sitter grammar and so no graph edges of their
own. Such a target used to report `nodeCount: 13, edgeCount: 0` while listing 12
referrers under `affected`; it now reports `edgeCount: 12` with
`resourceEdgeCount: 12`. See [`godot.md`](godot.md#resource-audit-codegraph-audit).

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

## `codegraph mcp list` — see the running stdio MCP servers

`serve --mcp` runs in the foreground, so several stdio MCP processes can be alive
at once — one per client window — with nothing tying them to a project directory.
Each foreground `serve --mcp` registers itself in a global, PID-keyed registry, and
`mcp list` reads it back:

```bash
codegraph mcp list          # table
codegraph mcp list --json   # machine-readable
```

The table columns are `PID`, `STARTED`, `VERSION`, `LAUNCH PROJECT`.
`LAUNCH PROJECT` is last and never truncated — it is the field a human reads to
recognize a stale row. A server launched without `--path` (the Kiro / Qoder shape)
shows `<none>`.

`LAUNCH PROJECT` is the `--path` the server started with, i.e. its **default** —
not a limit on what it can open. A client that passes an absolute `projectPath`
reaches any indexed project, so every live server is a potential holder of any
index. Nothing in the CLI filters on this field; it is there to tell rows apart.

Two other renderings:

- Nothing registered: `No stdio MCP servers registered.` plus a note that older
  codegraph versions do not register at all, so they never appear here — find
  those with your OS process tools.
- Registry unreadable: `registry unavailable at <path>: <error>` plus the same
  process-tool fallback. A missing directory is **not** an outage; it is the
  normal state before the first `serve --mcp` ever runs, and renders as the empty
  case above.

**Every branch exits 0**, the outage included. This is a diagnostic command, and
failing it while someone is debugging would only get in the way.

`--json` output always carries `servers` as an array, so a consumer never has to
branch on shape:

```jsonc
{
  "servers": [
    {
      "pid": 41287,
      "project": "/w/proj",
      "startedAt": 1753900000000,
      "transport": "stdio",
      "version": "0.41.0",
    },
  ],
}
```

`project` is the launch `--path` and is omitted entirely when the server was
started without one. On an outage the array is empty and an extra
`registryUnavailable` key appears:

```jsonc
{ "servers": [], "registryUnavailable": { "path": "…/codegraph/mcp", "error": "…" } }
```

**Why there is no `codegraph mcp stop`.** The HTTP registry is keyed by bind
address and does offer `http stop`; this one is keyed by PID, and that difference
is the whole reason. An entry that outlived a crash names a PID the OS may since
have handed to an unrelated process, and codegraph has no portable way to prove
process instance identity — the daemon lock is an atomic-create placeholder plus a
recorded PID, not an OS advisory lock. Terminating by registered PID could kill an
innocent process, so `list` leaves the decision to a human: it asks you to confirm
the PID really is codegraph (`ps -p <pid> -o command=` on unix,
`tasklist /FI "PID eq <pid>"` on Windows) and only then offers the stop command
(`kill <pid>` on unix, `taskkill /PID <pid> /F` on Windows). A listed row means
"registered, and that PID is alive" — never "that PID is proven to be codegraph".
A `stop` subcommand is gated on landing instance-identity verification first, not
on anything else. Closing the client that launched the server is cleaner than
either way.

**`index` pre-warning.** Before rebuilding an index — a destructive step that
deletes `codegraph.db`, `-wal`, and `-shm` — `codegraph index` checks the same
registry and warns when **any** stdio server is registered, naming each PID, its
launch project, and the stop command. A process still holding those files makes
the delete fail; that is the Windows-only failure mode, since unix can unlink an
open file. The warning goes to stderr, respects `-q/--quiet`, never changes the
exit code, and is silent when the registry holds nothing. The same guidance is
appended when the delete does fail.

The warning deliberately does **not** narrow to servers whose launch project
contains the one being rebuilt. Since any server can be asked to open any indexed
project, narrowing would hide the holder in exactly the case the check exists for —
a server launched elsewhere that a client has since pointed at this project. It is
observability, not a fix: registries only see servers that register, so an absent
holder is still not proof there is none.

## `codegraph status` — WAL diagnostics

`status` reports the SQLite write-ahead log only when a non-empty `-wal` sidecar
exists. Human output adds `WAL Size` immediately after `DB Size`; JSON adds the
top-level integer `walSizeBytes` in raw bytes. A healthy sidecar-free index omits
`walSizeBytes`, preserving the existing JSON key set.

A leftover WAL blocks the strict `Current` read gate, so `status` switches to a
read-only degraded diagnostic instead of querying uncorroborated database rows. It
reports `initialized: false`, `extractionStatus: "current"`, the typed refusal in
`extractionStatusDetail`, and the trustworthy path and DB/WAL size fields; it omits
file/node/edge counts and `journalMode`. Human output identifies this as
`current (blocked by SQLite sidecar)`. `status` never checkpoints, deletes, or
otherwise heals the sidecar.

When the WAL is larger than both the database and the configured WAL limit,
`status` warns that live CodeGraph processes must be stopped before running:

```bash
codegraph sync /path/to/project
```

`sync` and `index` attempt the recovery synchronously before their ordinary writer
acquisition, but only after the previous daemon owner is proven dead and a bounded
exclusive lease succeeds. A live or contended owner is never folded underneath.

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
Its stdout and stderr are appended to `.codegraph/daemon.log`. The Unix socket
is at `.codegraph/daemon.sock`; the pid/lock file lives alongside it.

On filesystems that reject binding an `AF_UNIX` socket inside the project
directory (ExFAT/FAT, some network mounts, WSL DrvFs), the daemon falls back
through a deterministic candidate chain — first the project-dir
`.codegraph/daemon.sock`, then a hashed socket under the system temp dir — and
records the socket it actually bound in the lock file. The pid/lock file always
stays at `.codegraph/daemon.pid`, and clients read the recorded socket from the
lock, so they attach to whichever candidate the daemon chose.

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
`.cache`, `.git`, `.codegraph`, and everything else in the
default ignore set, plus any paths matched by the root `.gitignore`. This pruning applies at any
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

| Variable                           | Default      | Clamp range         | Meaning                                                                                                            |
| ---------------------------------- | ------------ | ------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `CODEGRAPH_NO_DAEMON`              | —            | —                   | Force foreground Direct mode; never spawn or proxy a daemon                                                        |
| `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` | `300000`     | 1000–3600000        | Exit after this long with no connected clients                                                                     |
| `CODEGRAPH_DAEMON_MAX_IDLE_MS`     | `1800000`    | 1000–3600000        | Hard cap on total daemon lifetime when idle                                                                        |
| `CODEGRAPH_DAEMON_CLIENT_SWEEP_MS` | `30000`      | 50–600000           | How often the daemon sweeps for dead clients                                                                       |
| `CODEGRAPH_WATCH_DEBOUNCE_MS`      | `2000`       | 100–60000           | File-change debounce window before a re-index triggers                                                             |
| `CODEGRAPH_NO_WATCH`               | —            | —                   | Disable the live file watcher (equivalent to `serve --no-watch`)                                                   |
| `CODEGRAPH_FORCE_WATCH`            | —            | —                   | Override WSL2 `/mnt/` auto-disable; does not override `NO_WATCH`                                                   |
| `CODEGRAPH_NO_WAL_DEFER`           | —            | `1` enables opt-out | Keep SQLite's default WAL autocheckpoint interval during bulk indexing                                             |
| `CODEGRAPH_WAL_VALVE_MB`           | `256`        | >0; invalid→default | Shared MB threshold for the active WAL valve, resetting `journal_size_limit`, and `status` WAL warning             |
| `CODEGRAPH_MCP_REGISTRY_DIR`       | —            | —                   | Override the stdio MCP registry directory read by `mcp list`                                                       |
| `CODEGRAPH_DIR`                    | `.codegraph` | —                   | Select one non-empty project-local directory name; absolute paths, separators, `.`, `..`, and aliases are rejected |

Timeout/debounce values outside their clamp range are silently clamped to the
nearest bound. `CODEGRAPH_WAL_VALVE_MB` instead falls back to `256` when it is
empty, non-numeric, zero, or too large to convert safely to bytes.

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
- Exactly one file is read: the resolved project's own `codegraph.json` under
  the selected index root. There is no directory-tree walk and no cross-project
  inheritance, so one project can never adopt another's overrides.
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
