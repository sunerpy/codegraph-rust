# CodeGraph Rust Equivalence Oracle

> AS-BUILT 校核（T27）：本文与已提交代码一致。节点 ID 公式
> （`codegraph_core::node_id::generate_node_id`，
> `format!("{file_path}:{kind}:{name}:{line}")` → sha256 → `{kind}:{hex[..32]}`）、
> 文件节点字面量 `file:{file_path}`、内容哈希（`hash_content`）以及
> `FrameworkResolver` 仅扩展点（`crates/codegraph-resolve/src/framework.rs`，
> 零具体实现）均按本文实现。

This document defines the byte-level and semantic parity contract between the
Rust port and the pinned upstream TypeScript reference. The current authoritative
fixture is `crates/codegraph-bench/fixtures/mini/`; the live reference outputs are
stored under `reference/golden/mini/`.

## Node ID Formula

The symbol-node helper computes:

```text
sha256("{filePath}:{kind}:{name}:{line}") -> hex -> first 32 chars
id = "{kind}:{hash32}"
```

Rust mirrors this in `codegraph_core::node_id::generate_node_id()`.

Inputs are part of the compatibility contract:

- `filePath`: project-relative path with `/` separators, for example
  `src/app.ts`.
- `kind`: the serialized `NodeKind::as_str()` value, for example `function`,
  `class`, `method`, or `import`.
- `name`: the exact extracted name. Import nodes use the module specifier, for
  example `./math`.
- `line`: 1-based start line. The tree-sitter call site passes
  `node.startPosition.row + 1`.

## File Node Special Case

Tree-sitter file nodes do not call `generateNodeId()`. The tree-sitter file-node special case uses the literal ID:

```text
file:{filePath}
```

The mini golden data verifies this for all three file nodes, for example
`file:src/app.ts`. Non-file nodes in the same golden set, including imports, use
the hashed `{kind}:{32hex}` form.

Some custom extractors call `generateNodeId(..., 'file', ..., 1)` for their own
file-like nodes; that is a separate custom-extractor path and is not the
tree-sitter file node represented in the mini golden.

## Content Hash Formula

The content hash (`hashContent`)
stores a full lowercase SHA-256 hex digest of the file content in
`files.content_hash`.

Rust mirrors this in `codegraph_core::node_id::hash_content()`. The test fixture
hashes are cross-checked against:

```bash
sqlite3 reference/golden/mini/colby.db \
  "select path,content_hash from files order by path;"
```

## Oracle Tiers

### Tier-1: Byte-identical

Tier-1 fields must match the reference output byte-for-byte and are allowed to
fail tests on any mismatch:

- `nodes` rows, excluding inherently time-varying `updated_at`.
- Node IDs, including the `file:{path}` tree-sitter file-node special case.
- `files.content_hash` values.
- SQLite schema and FTS5 schema/triggers/indexes captured from `.schema`.

### Tier-2: Multiset-identical

Tier-2 data may be compared as unordered multisets when insertion order or rowid
allocation is not semantically stable:

- `edges` keyed by `(source, target, kind)` plus relevant metadata.
- `unresolved_refs` keyed by `(from_node_id, reference_name, reference_kind)` and
  source location.

### Tier-3: Allowlisted behavioral parity

Tier-3 output can differ only when the difference is intentionally documented in
`KNOWN_DIFFS.md`:

- Query output formatting.
- MCP response formatting and summaries.
- Other presentation-layer or non-deterministic fields that preserve semantics.

## Determinism Statement

Node IDs are Tier-1 deterministic. Given the same relative path, serialized
`NodeKind`, extracted name, and 1-based start line, Rust must produce exactly the
same bytes as the reference. The golden test in `crates/codegraph-core/src/node_id.rs`
loads all 13 real nodes from `reference/golden/mini/colby.nodes.json` and proves
that every ID reproduces.

## Harness

The executable oracle lives in `crates/codegraph-bench/src/oracle/` and is the
library entry point for later cross-implementation runs. Later tasks should call:

```rust
codegraph_bench::oracle::assert_equivalent(rust_db, golden_dir)
```

For the current mini fixture:

```bash
cargo test -p codegraph-bench --test equivalence -- --nocapture
```

### Regenerating goldens

Canonical fixture files are committed under `reference/golden/<corpus>/`:

- `nodes.json`
- `edges.json`
- `refs.json`
- `files.json`
- `schema.sql`

Regenerate from a reference SQLite database with:

```bash
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/mini/colby.db reference/golden/mini
```

The canonicalizer strips inherently unstable timestamp columns
(`nodes.updated_at`, `files.modified_at`, `files.indexed_at`), parses JSON text
columns before re-serializing them with deterministic key order, asserts all
stored paths are relative `/` paths, ignores `edges.id` and
`unresolved_refs.id`, and normalizes `.schema` text with the same rules used by
`crates/codegraph-store/tests/schema_parity.rs`.

### Godot fixture

A second golden fixture, `reference/golden/godot/`, guards Godot-specific
extraction that the mini fixture cannot reach — there are no `.gd`/`.tscn`/
`project.godot` files in `mini`. It captures the framework-resolver output for:

- **F1** — an autoload call (`GameFlow.return_to_map()`) resolving to the unique
  same-named `func` in the bound script (a `framework`-resolved `Calls` edge),
  alongside the coexisting singleton-constant edge.
- **F2** — signal-handler connections (`.connect(_on_pressed.bind(button))` and
  `.connect(Callable(self, "_on_input"))`) resolving to the handler `func`s
  (`Calls` edges).
- **F3** — a `.tscn` `ExtResource` script attachment (`main.tscn` →
  `stage_manager.gd`), captured as a `script_attach` unresolved-ref subkind.

The minimal source corpus lives at `crates/codegraph-bench/fixtures/godot/`
(`project.godot`, `game_flow.gd`, `stage_manager.gd`, `main.tscn`).

Regenerate the committed database + canonical JSON reproducibly from the corpus:

```bash
# 1. Copy the corpus to a clean directory (keeps the workspace .codegraph/ out of it).
rm -rf /tmp/cg-fixture-godot
cp -r crates/codegraph-bench/fixtures/godot /tmp/cg-fixture-godot

# 2. Index it with OUR binary (never hand-write the golden).
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/cg-fixture-godot

# 3. Commit the produced database as the fixture's colby.db.
cp /tmp/cg-fixture-godot/.codegraph/codegraph.db reference/golden/godot/colby.db

# 4. Dump the canonical golden JSON + schema from that database.
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/godot/colby.db reference/golden/godot
```

The extraction and `--gen-golden` steps are both byte-stable: re-running the
index or the dump reproduces identical `nodes.json`/`edges.json`/`refs.json`/
`files.json`/`schema.sql`. The `generated_golden_matches_committed_godot_fixture`
and `upstream_db_is_self_equivalent_to_godot_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce this.

The schema normalization helper is replicated inside `codegraph-bench` rather
than extracted into `codegraph-store` to avoid changing store source during the
parallel CRUD work. It preserves `.schema` statement order, strips optional
`IF NOT EXISTS` from `CREATE TABLE/INDEX/VIRTUAL TABLE/TRIGGER`, trims line
whitespace, removes blank lines, joins statements with `;\n`, and enforces a
final `;\n`.

### Ruby fixture

A third golden fixture, `reference/golden/ruby/`, guards Ruby `receiver.method`
extraction (upstream #1110) that the other fixtures cannot reach — there are no
`.rb` files in `mini`/`godot`. It captures the four receiver-bearing-call edge
shapes:

- **instance-method call** — `@logger.log(message)` resolving to `Logger#log`
  (a `Calls` edge to the METHOD name, not the receiver).
- **class-method call** — `Formatter.shout(message)` resolving to
  `Formatter.shout` (a `Calls` edge to the method name).
- **`Const.new` construction** — `Logger.new` recorded as an `Instantiates` edge
  to the receiver class `Logger`, not a `Calls` edge to `new`.
- **bare `include`** — `include Greeting` still records an `Implements` edge
  (regression guard: the receiver.method path must not disturb it).

The minimal source corpus lives at `crates/codegraph-bench/fixtures/ruby/`
(`service.rb`, `logger.rb`).

Regenerate the committed database + canonical JSON reproducibly from the corpus:

```bash
# 1. Copy the corpus to a clean directory (keeps the workspace .codegraph/ out of it).
rm -rf /tmp/cg-fixture-ruby
cp -r crates/codegraph-bench/fixtures/ruby /tmp/cg-fixture-ruby

# 2. Index it with OUR binary (never hand-write the golden).
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/cg-fixture-ruby

# 3. Commit the produced database as the fixture's colby.db.
cp /tmp/cg-fixture-ruby/.codegraph/codegraph.db reference/golden/ruby/colby.db

# 4. Dump the canonical golden JSON + schema from that database.
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/ruby/colby.db reference/golden/ruby
```

Like the Godot fixture, both the index and the dump are byte-stable, and the
`generated_golden_matches_committed_ruby_fixture` and
`upstream_db_is_self_equivalent_to_ruby_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce it.

### C++ fixture

A fourth golden fixture, `reference/golden/cpp/`, guards C++ `base_class_clause`
inheritance extraction (upstream #1043) that the other fixtures cannot reach —
there are no `.cpp`/`.hpp` files in `mini`/`godot`/`ruby`. It captures the
general C++ inheritance shapes plus templated-base stripping:

- **single public base** — `class D : public Base` resolving to `Base`
  (an `Extends` edge; the `public` access specifier is skipped).
- **templated base (stripped)** — `class T : public Container<int>` resolving to
  `Container` (template args stripped to the base name).
- **multiple inheritance** — `class Both : public Container<char>, public Plain`
  emitting two `Extends` edges (to `Container` and `Plain`).
- **struct base** — `struct S : Container<double>` resolving to `Container`
  (struct inheritance goes through the same path as class inheritance).
- **`::`-qualified templated base** — `class Q : public ns::Tpl<int>` recording
  an `Extends` ref to `ns::Tpl` (qualified head kept, template args stripped);
  captured as an unresolved ref in `refs.json`.

The minimal source corpus lives at `crates/codegraph-bench/fixtures/cpp/`
(`base.hpp`, `derived.cpp`). The base classes live in a `.hpp` file (not `.h`,
which maps to `Language::C` where `class` is not valid syntax).

Regenerate the committed database + canonical JSON reproducibly from the corpus:

```bash
# 1. Copy the corpus to a clean directory (keeps the workspace .codegraph/ out of it).
rm -rf /tmp/cg-fixture-cpp
cp -r crates/codegraph-bench/fixtures/cpp /tmp/cg-fixture-cpp

# 2. Index it with OUR binary (never hand-write the golden).
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/cg-fixture-cpp

# 3. Commit the produced database as the fixture's colby.db.
cp /tmp/cg-fixture-cpp/.codegraph/codegraph.db reference/golden/cpp/colby.db

# 4. Dump the canonical golden JSON + schema from that database.
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/cpp/colby.db reference/golden/cpp
```

Like the Ruby fixture, both the index and the dump are byte-stable, and the
`generated_golden_matches_committed_cpp_fixture` and
`cpp_db_is_self_equivalent_to_cpp_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce it.

### KNOWN_DIFFS.md format

Tier-3 differences are allowlisted by grep-able lines in repo-root
`KNOWN_DIFFS.md`:

```text
RULE tier=3 surface=<surface> key=<substring-or-*> justification=<short-token>
```

Only Tier-3 entries can be allowed. Tier-1 byte mismatches and Tier-2 multiset
mismatches always fail; the differ never weakens those tiers to pass.
