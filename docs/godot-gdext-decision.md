# Decision: gdext is not used for Godot analysis

## The question

A user asked whether CodeGraph could or should use
[`gdext`](https://github.com/godot-rust/gdext) — the godot-rust GDExtension
bindings — to improve Godot/GDScript analysis.

## Verdict

**Rejected, and unnecessary.** `gdext` is a binding for Rust code running inside
a live Godot engine. It exposes no offline API for reading `.gd`, `.tscn`,
`.tres` or `project.godot`, so it cannot do the job CodeGraph does, and it
solves none of the gaps recorded in
[`docs/godot.md`](godot.md#limitations) — those are either genuinely runtime-only
or solvable with better static parsing.

## What gdext actually is

`gdext` compiles Rust into a GDExtension shared library that Godot loads at
startup. Its type surface (`Node`, `Callable`, `NodePath`, …) is generated from
Godot's `extension_api.json`, and those types are only meaningful once the engine
has initialized — a `Gd<Node>` is a handle into a running engine's object
database, not a description of a file on disk. It is not a GDScript parser, not
a `.tscn`/`.tres` reader, and offers no offline API for inspecting project files.
Anything CodeGraph wanted from it would require launching Godot.

To be precise about the build step, because an earlier draft of this document got
it wrong: a plain `gdext` dependency does **not** need a Godot binary present at
build time. Per the crate docs
([docs.rs/godot 0.5.4, "Cargo features"](https://docs.rs/godot/latest/godot/#cargo-features)),
the `api-*` features "Set the **API level** to the specified Godot version, or a
custom-built local binary… If absent, the current Godot minor version is used,
with patch level 0" — i.e. the bindings are generated from an API description
bundled with the crate and pinned by its version. Only the two opt-in features
need an engine or a hand-supplied API: "`api-custom` feature requires specifying
`GDRUST_GODOT_BIN` environment variable with a path to your Godot4 binary. The
`api-custom-json` feature requires specifying `GDRUST_GODOT_API_JSON` environment
variable with a path to your custom-defined `extension_api.json`." The engine
requirement is at **run** time — Godot loads the produced library — and that is
the part that disqualifies `gdext` here.

An earlier draft also argued that `gdext` would break the golden `.schema`
byte-stability invariant because its bindings vary with the installed engine.
That argument does not survive the correction: with the default prebuilt API the
generated surface is fixed by the crate version, so it is as reproducible as any
other pinned dependency, and it would only become runner-dependent under
`api-custom` (engine binary) or `api-custom-json` (hand-supplied JSON) — features
nobody is proposing. The determinism objection is therefore dropped rather than
kept for symmetry. The real determinism risk was never the bindings; it is that
any output derived from a live engine's state is not a pure function of the
corpus, which is the runtime problem stated above.

## Why it disqualifies itself

**1. It requires the engine at runtime; CodeGraph deliberately requires none.**
`docs/godot.md` opens by stating that CodeGraph parses Godot project files
"— no engine required, no compilation, no runtime". Releases are standalone
binaries, and README.md notes that "Linux builds are statically linked (musl) —
no glibc or SQLite system dependency". Users indexing a Godot repo on CI, on a
server, or on a machine without the editor installed are a supported case.
`gdext` code only executes as a GDExtension loaded by a Godot process, so
reaching its types at all would mean shipping or locating an engine and starting
it. `codegraph index` would stop being a self-contained command.

**2. It solves none of the recorded gaps.**
The limitations in [`docs/godot.md`](godot.md#limitations) split cleanly, and
neither half needs an engine binding:

- Runtime-only by nature — `get_node(var)` / `call(method_var)` computed
  targets, `NodePath`s built by string concatenation, and "No runtime
  verification". An in-engine binding does not make a static analyser able to
  know a value that only exists while the game runs; it would only let CodeGraph
  _run_ the project, which is a different tool (`docs/godot.md` already assigns
  runtime load-verification to Godot MCP Pro).
- Statically solvable — scene-backed autoloads not binding the attached script's
  methods, `.tscn` `[connection]` handlers matched by name only, binary `.res`
  files unparsed. Each is a parsing/resolution improvement in existing crates.

## Alternatives worth pursuing instead

Ranked by value against risk. None adds a dependency on the engine.

**1. Bind scene-backed autoload methods (highest value, lowest risk).**
`docs/godot.md` records: "**Scene-backed autoloads are registration-only.** A
`uid://…` autoload that resolves to a `.tscn` (scene autoload) emits the
`project.godot → .tscn` registration reference but no F1 `Autoload.method()`
binding to the scene's attached script." The missing hop is one the resolver
already knows how to make: the `.tscn` extraction already emits `script =
ExtResource(…)` scene-node → `.gd` references, and F1 already binds
`Autoload.method()` to a uniquely-named `func` in a bound script. Following the
scene's root-node script and reusing F1's existing determinism rule (edge only
when exactly one matching `func` exists) closes a concrete, user-visible gap
inside one resolver, with a new golden fixture to pin it. Purely additive.

**2. Move the `.gd` dynamic scanner onto tree-sitter queries (high value,
moderate risk).** `crates/codegraph-resolve/src/frameworks/godot_script.rs` is
today a defensive line/string/paren scanner — `scan_line`, `scan_connect`,
`scan_dollar_node`, `first_arg`, `top_level_comma`, `ident_before` and friends
walk raw bytes per line. It is deliberately tolerant (malformed input is
skipped, never panics), but hand-rolled text windows are exactly the bug class
commit `b7ff1f8b3a0ee5f99912e8a426d6af810eec3b9c` ("fix(resolve): bound the
React Route opening-tag scan to the tag itself") fixed on this branch: a fixed
byte window that never cut at the element's own `>` let a parent route borrow a
sibling's `path`/`element`, producing 2 wrong edges out of 3. The Godot scanner
carries the same shape of risk — multi-line call arguments, comments and strings
containing `connect(`, and nested parens are all handled by ad-hoc byte logic.
Running tree-sitter queries over the parse tree the GDScript grammar already
produces removes that class of error at the root. Moderate risk because the
resolver's output is golden-protected via `reference/golden/godot/`, so the port
must be output-preserving: every existing sentinel and edge has to come out
byte-identical, and the change is only worth making with the current unit tests
plus the golden fixture as the gate.

**3. Upgrade the GDScript grammar (conditional value, low risk).**
The pinned version is `tree-sitter-gdscript = "6.1.0"`
(`crates/codegraph-extract/Cargo.toml`, checksum
`b7a37fe8c0a10c0c39ecd5b2f7db53933f691488f5572409a4d3c0dfeb3f6108` in
`Cargo.lock`, also recorded in [`docs/grammar-manifest.md`](grammar-manifest.md)).
Whether a newer release exists is **unverified** — nothing in this repository
records the upstream crate's latest version, and no network lookup was performed
for this decision. What would settle it: `cargo search tree-sitter-gdscript` or
the crate's registry page, plus the grammar's changelog. Ranked below (2) because
its value is unknown until that check happens: a grammar bump only helps if it
fixes parse gaps CodeGraph actually hits, and any bump is a golden-affecting
change that must be re-verified against `reference/golden/godot/`.

**4. Parse binary `.res` (lowest value).** Recorded as a limitation, but it
means implementing Godot's binary resource format against no specification in
this repository, for files that are usually a compiled form of a `.tres` the
project also has in text form. Not worth the surface area.

Not worth pursuing at all: computed `get_node`/`call` targets and
runtime-constructed `NodePath`s. `docs/godot.md` states the position —
"**Computed targets are unresolved, not fabricated.** … They appear as
`godot:dynamic:<kind>` sentinels, never as concrete edges." The sentinel _is_
the correct answer; replacing it with a guess would be a regression in honesty.

## What remains uncertain

- Whether a newer `tree-sitter-gdscript` release exists, and whether it fixes
  anything CodeGraph hits. Unverified, as above.
- Whether the tree-sitter-query port of `godot_script.rs` can be made exactly
  output-preserving. Unknown until attempted; the golden fixture decides it, and
  a diff there is a stop signal, not something to regenerate around.
- No throughput, latency, or memory benchmark exists for the GDScript path, so
  no performance claim is made here in either direction. If a future argument
  for a different parsing strategy rests on speed, it needs a measurement first.

Evidence that would reopen the gdext question: an offline, engine-free,
version-stable API for reading Godot project files. `gdext` is not that, and is
not trying to be.

`scripts/guardrail.sh` is **not** extended to ban Godot-engine crates as part of
this decision — its forbidden list targets AI/vector/LLM crates, and widening a
CI gate is a separate call. If a hard block is wanted, adding `gdext`/`godot` to
that list is the mechanism; it is recommended here, not done here.

## See also

- [`docs/godot.md`](godot.md) — what CodeGraph extracts from Godot projects, the
  static-vs-runtime boundary, and the limitations argued against above.
- [`AGENTS.md`](../AGENTS.md) — the hard invariants quoted here.
- [`docs/grammar-manifest.md`](grammar-manifest.md) — the pinned GDScript
  grammar and its ABI status.
