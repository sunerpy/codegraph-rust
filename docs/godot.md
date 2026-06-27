# Godot Static Analysis

CodeGraph statically parses Godot project files (`project.godot`, `.tscn`,
`.tres`, `.gd`) and builds a symbol graph from them — no engine required, no
compilation, no runtime. This page describes what gets extracted, where the
static boundary sits, and what the tool honestly cannot tell you.

---

## What CodeGraph extracts

CodeGraph activates its Godot analysis automatically when a `project.godot`
file is present at the root of the indexed project. All extraction is
deterministic and byte-stable across runs.

### Indexing scope (ignored directories)

By default, CodeGraph excludes `.godot/` and `addons/` from the index, alongside
the standard cross-ecosystem defaults (`node_modules`, `target`, `dist`, `.venv`,
etc.). `.godot/` is the engine's regenerated import/cache directory — never
business source, fully reconstructed by the editor on open. `addons/` holds
vendored third-party editor plugins that would otherwise crowd out first-party
`.gd`/`.tscn`/`.tres` code in search results and impact queries.

Both exclusions are opt-out. To re-include a directory (for example, a team that
keeps first-party code under `addons/`), set a custom `indexing.ignore_dirs` list
in `.codegraph/config.toml`. That list replaces the default set entirely, so
re-list any other directories you still want ignored — e.g. keep `.godot` while
dropping `addons`:

```toml
[indexing]
ignore_dirs = [".godot", "node_modules", "target", "dist", ".venv"]
```

### `project.godot` — autoload singletons, input actions, plugins

| Extracted item                   | Graph representation                                                       |
| -------------------------------- | -------------------------------------------------------------------------- |
| `[AutoLoad]` entry               | A `Constant` node named after the singleton (e.g. `GameState`)             |
| Singleton → script               | `References` edge from the singleton node to the repo-relative script path |
| `[Application]` `run/main_scene` | `References` edge to the `.tscn` scene path                                |
| `[Input]` action                 | A `Constant` node per action name                                          |
| `[EditorPlugins]`                | A marker `Constant` node with `References` edges to each plugin config     |

When a singleton's backing script is confirmed in the index, the singleton node
carries a `signature` of `"autoload -> <path>"` — a machine-readable binding
that `codegraph_callers` and `codegraph_impact` surface without re-parsing.

### `.tscn` — scene files

Each scene file contributes:

| Extracted item                                | Graph representation                                     |
| --------------------------------------------- | -------------------------------------------------------- |
| `[node name="N"]`                             | A `Constant` node per scene node (name, line)            |
| `script = ExtResource(…)`                     | `References` edge: scene node → repo-relative `.gd` path |
| `[connection signal="s" from="…" method="m"]` | `References` edge: source node → handler method name `m` |
| `groups = ["g1","g2"]`                        | `References` edge per group name, from the node          |
| `instance=ExtResource(…)` on a `[node]`       | `Instantiates` edge: node → instanced `.tscn` path       |

Signal connection edges target the handler method **name** (e.g. `_on_timeout`)
from the scene source. Cross-file resolution — binding that name to the actual
`func _on_timeout` symbol in the script — is handled by the generic name-matcher
in the resolution pass. If the method exists in the indexed `.gd` file, the edge
resolves; if not, it stays as an unresolved reference (which is itself a signal —
see [Honesty signals](#honesty-signals) below).

### `.tres` — text resources

Each `.tres` file emits a single resource marker node (typed from the
`[gd_resource type="..."]` header) and `References` edges for every
`ExtResource(…)` it references:

| Extracted item                  | Graph representation                                        |
| ------------------------------- | ----------------------------------------------------------- |
| `[gd_resource type="T"]`        | One `Constant` marker node named after the resource type    |
| `script = ExtResource(…)`       | `References` edge: marker → repo-relative `.gd` path        |
| Any property `= ExtResource(…)` | `References` edge: marker → referenced `.tres`/`.tscn` path |

A self-contained `.tres` with no external references produces no extra nodes and
no edges — just the file node from ingestion.

### GDScript dynamic patterns (`.gd`)

The resolver recognizes common dynamic-dispatch call sites in `.gd` source and
emits reference edges for **literal** targets. Each edge originates from the
enclosing `func` node (matched by the same deterministic id the base GDScript
extractor uses, so callers and dynamic refs share one attribution point).

| Pattern                                                | Target extracted              | Edge kind    |
| ------------------------------------------------------ | ----------------------------- | ------------ |
| `sig.connect(handler)`                                 | handler method name           | `Calls`      |
| `emit_signal("sig_name")`                              | signal name                   | `References` |
| `some_signal.emit()`                                   | signal name (left of `.emit`) | `References` |
| `get_node("Path/To/Node")`                             | node path string              | `References` |
| `$NodePath` / `$"Quoted/Path"`                         | path string                   | `References` |
| `%UniqueName`                                          | unique node name              | `References` |
| `get_nodes_in_group("g")`                              | group name                    | `References` |
| `add_to_group("g")` / `is_in_group("g")`               | group name                    | `References` |
| `has_method("m")` / `call("m")` / `call_deferred("m")` | method name                   | `Calls`      |

**Autoload access** (`BuffManager.apply()` — an uppercase-initial receiver
matched against the `[AutoLoad]` roster from `project.godot`) resolves to the
singleton's `Constant` node via the framework resolver at confidence 0.9. Only
names that appear in `project.godot`'s autoload table produce an edge; built-in
types (`Vector2`, `Input`, `Color`, class constructors) are rejected by the
roster gate with zero false positives.

**Computed / dynamic targets** — when the argument is a variable, member
expression, or call rather than a string literal — are recorded as
`godot:dynamic:<call-kind>` sentinel references (e.g.
`godot:dynamic:get_node`, `godot:dynamic:call`). These are intentionally left
unresolved and surfaced separately (see [Honesty signals](#honesty-signals)).
A `get_node(some_var)` never produces a fabricated edge.

---

## Static vs. runtime — the boundary

CodeGraph performs **static structure and impact analysis**. It reads source
files on disk and builds a graph from what is textually present. It does not:

- run the Godot engine or editor
- compile or byte-compile any script
- load or verify that a scene actually instantiates at runtime
- simulate input or capture screenshots
- resolve NodePaths or method names that are built from variables at runtime

For "does this scene actually load and run after my change?", you need a
runtime tool such as [godot-mcp](https://github.com/Coding-Solo/godot-mcp),
which controls a live Godot editor session.

The division of labor is:

| Question                                                           | Tool                     |
| ------------------------------------------------------------------ | ------------------------ |
| What script is attached to this scene node?                        | CodeGraph                |
| What signals does this scene connect?                              | CodeGraph                |
| What autoloads does this project declare?                          | CodeGraph                |
| Which functions could be affected by changing `BuffManager.apply`? | CodeGraph                |
| Does the scene actually load without errors at runtime?            | Runtime tool (godot-mcp) |
| Does the script compile in the engine?                             | Runtime tool             |
| Does the animation play correctly?                                 | Runtime tool             |

CodeGraph tells you **what to read and what a change might affect**. A runtime
tool tells you **whether it actually works**. The two complement each other.

---

## Honesty signals

### "No static caller ≠ dead code"

A function reached only via a Godot dynamic link is annotated rather than
silently shown as unreachable. CodeGraph checks three signals:

1. **Scene/resource link** — an unresolved reference whose name matches the
   function's name, originating from a `.tscn`, `.tres`, or `project.godot`
   file. This catches signal-connection handlers, script bindings, and group
   callbacks.
2. **Autoload binding** — the function's file is the script bound to an
   autoload singleton (the singleton carries `signature = "autoload -> <path>"`
   from the post-extract pass).
3. **Dynamic-unresolved sentinels** — the function owns outgoing refs prefixed
   `godot:dynamic:`, meaning it itself calls patterns whose targets could not be
   statically confirmed.

When any of these signals fire and the static caller list is empty, the CLI and
MCP output says:

```
no static callers — may be reached dynamically (Godot signal/get_node/autoload)
```

rather than implying the function is dead. The MCP surface appends this as a
blockquote so agent consumers can parse it reliably.

### Dynamic-unresolved references

Computed call targets that cannot be resolved statically are surfaced as a
distinct block:

```
dynamic / unresolved references (cannot be statically confirmed):
  godot:dynamic:get_node
  godot:dynamic:call
```

These are never bound to a real definition — the sentinel prefix ensures the
name-matcher cannot accidentally produce a false edge. The suffix identifies
which call pattern produced the reference so you can inspect the call site.

---

## Resource audit (`codegraph audit`)

`codegraph audit` is a read-only report over an already-indexed Godot project:
orphan resources, dangling references, and reverse-dependency impact. It adds no
extraction and writes no nodes/edges — it is computed from the existing graph
plus on-disk existence checks, so it never perturbs `check` or golden output.

```bash
codegraph audit --orphans -p .                 # .tres/.tscn resources nothing references
codegraph audit --dangling -p .                # path references whose target is missing on disk
codegraph audit --impact res://buff.tres -p .  # what references a changed path
codegraph audit --orphans --dangling --json -p .
```

Because `.tres`/`.tscn`/`project.godot` files have no tree-sitter grammar, they
get no `file:` graph node and their `ExtResource(…)` references stay in the
unresolved-reference table. The audit therefore keys on the resource's
repo-relative **path** (its `files` row + the path-shaped reference names), not
on incoming graph edges.

- **Orphan** — a `.tres`/`.tscn` whose path no reference names.
- **Dangling** — a path-shaped reference whose target is missing on disk.
  **Exclusion precedence:** a target under `.godot/` or `addons/` is excluded
  first (never dangling, regardless of disk state); then a `godot:dynamic:`
  reference is excluded; only the survivors get the disk-exists check. Dangling
  reports missing resource/script **paths** only — a reference must look like a
  path (contain `/`, or carry a resource extension) to be a candidate. A bare
  `[connection] method="_on_X"` signal handler name is not a path and is never
  reported as dangling, whether or not the handler method exists; signal-method
  resolution is out of scope.
- **Impact** — the reverse-dependency list for a changed path: references that
  name it, plus any resolved incoming edges on that path's `file:` node.

This is a static structural report. Runtime `ResourceLoader` load-verification is
out of scope (that is Godot MCP Pro's job). See [`cli.md`](cli.md) for the full
flag reference.

---

## Optional resource DSL hook

For projects that define custom `.tres` resource types with domain-specific
fields that carry semantic edges (e.g. a `skill_effect` field that references
another resource), CodeGraph supports an opt-in DSL mapping via
`.codegraph/codegraph.json`. List the `[resource]` property names that should
emit a reference edge from their value under `godot.dsl.resourceFields`:

```jsonc
{
  "godot": {
    "dsl": {
      "resourceFields": ["skill_effect", "effect_on"],
    },
  },
}
```

Each listed field name is matched against `key = value` lines inside a `.tres`
`[resource]` block. When a listed field's value is a plain double-quoted string
literal, CodeGraph emits a `References` edge from the resource marker node to
that literal value; when the value is an `ExtResource(…)` handle, the standard
`.tres` extraction already resolves it to the referenced path (no DSL config
needed). Computed, array, or `SubResource(…)` values are left unresolved.

This is **off by default** and has no effect unless `resourceFields` is
explicitly configured. The field list is entirely project-supplied — nothing is
hardcoded (`skill_effect`/`effect_on` above are only examples). Most projects do
not need it — the standard `ExtResource(…)` references in `.tres` files are
extracted automatically without any configuration.

### Opt-in ID capture (`idFields`)

A separate, independent opt-in block captures **bare or compound IDs** inside a
`.tres` `[resource]` body as `godot:id:<kind>:<value>` sentinel references. Use
it when a resource encodes domain IDs that are not `ExtResource(…)` handles —
e.g. a bare `buff_id = 7005`, or a compound `skill_effect = "a:b:9015:c:7005:1000"`
that packs several IDs into one delimited string. Declare each field under
`godot.dsl.idFields`:

```jsonc
{
  "godot": {
    "dsl": {
      "idFields": {
        "buff_id": { "kind": "buff" },
        "skill_effect": { "kind": "skill", "separator": ":", "idSegments": [2, 4] },
      },
    },
  },
}
```

Each key is a `[resource]` property name, matched by exact key against
`key = value` lines. The value is quote-stripped first, then:

- **Bare scalar / no `separator`** — the whole stripped value becomes one ID.
  `buff_id = 7005` → `godot:id:buff:7005`.
- **`separator` + `idSegments`** — the stripped value is split on `separator`
  and the listed `idSegments` parts are captured. **`idSegments` indices are
  0-based** into the split parts; the example `"a:b:9015:c:7005:1000"` with
  `[2, 4]` yields `godot:id:skill:9015` and `godot:id:skill:7005`. An
  **out-of-range index is silently skipped** (tolerant — never an error).
- **`idSegments` without a `separator`** — the segment list is inert and the
  whole stripped value is the single ID.

The captured IDs surface as **`godot:id:*` sentinel references** in
`unresolved_refs` — they are deliberately **never resolved to a node** (there is
no canonical "buff 7005" node), exactly like the `godot:dynamic:*` sentinels. A
colon-delimited literal can never name-match a symbol, so it stays unresolved and
is greppable via `codegraph search godot:id:buff:7005`.

Like `resourceFields`, `idFields` is **off by default** and has no effect unless
explicitly configured. Every field name, kind, separator, and segment index is
**project-supplied — CodeGraph hardcodes no domain field name, ID scheme, or
separator**. A project with no `idFields` block produces output byte-identical to
having no DSL config at all.

---

## Limitations

- **Computed targets are unresolved, not fabricated.** `get_node(var)`,
  `call(method_name_var)`, and similar patterns where the argument is not a
  string literal cannot be resolved statically. They appear as
  `godot:dynamic:<kind>` sentinels, never as concrete edges.
- **No runtime verification.** CodeGraph does not run the engine and cannot
  confirm that a scene loads, a script compiles, or a signal fires at runtime.
- **Signal connections resolved by name.** `.tscn` `[connection]` handler
  methods are matched by name against indexed `.gd` symbols. If the handler is
  in a script that isn't indexed (e.g. a plugin outside the project root), the
  edge stays unresolved.
- **Binary `.res` files not parsed.** Only text `.tres` resources are
  supported. Binary-format `.res` files produce a file node only.
- **`get_node` paths are literal strings.** A `NodePath` built at runtime
  (string concatenation, format strings) is treated as a computed target and
  left unresolved.
- **Autoload recognition requires `project.godot`.** Without a `project.godot`
  at the project root, the Godot resolver does not activate and no Godot-specific
  edges are emitted — `.gd` files are still indexed by the base GDScript
  extractor (functions, classes, signals, extends, preload edges).
- **`.godot/` and `addons/` are skipped by default.** Both directories are
  excluded from indexing so engine cache and vendored plugins don't bury
  first-party code in search results. Re-include a directory via a custom
  `indexing.ignore_dirs` list in `.codegraph/config.toml`. See
  [Indexing scope](#indexing-scope-ignored-directories).

---

## See also

- [`docs/languages.md`](languages.md) — GDScript (Tier 1), GodotScene,
  GodotResource, GodotProject file types.
- [`docs/mcp.md`](mcp.md) — `codegraph_callers` and `codegraph_impact` tools
  that surface the Godot honesty signals.
- [`docs/architecture.md`](architecture.md) — `FrameworkResolver` extension
  point and the resolution pipeline.
