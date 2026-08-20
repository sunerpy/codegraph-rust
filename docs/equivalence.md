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
- **UID-form autoloads** — a sidecar-UID SCRIPT autoload
  (`EffectManager="*uid://…"` resolved through `effect_manager.gd.uid` →
  `effect_manager.gd`, with an `EffectManager.apply_effect()` F1 method edge) and
  a header-UID SCENE autoload (`ComboUi="*uid://…"` resolved through
  `combo_ui.tscn`'s `uid=` header, registration-only). Both emit an
  `Autoload`-subkind UNRESOLVED ref; the `.gd.uid` sidecar is NOT indexed (it maps
  to `Language::Unknown`, so it is neither a file record nor a node).

The minimal source corpus lives at `crates/codegraph-bench/fixtures/godot/`
(`project.godot`, `game_flow.gd`, `stage_manager.gd`, `main.tscn`,
`effect_manager.gd`, `effect_manager.gd.uid`, `combo_ui.tscn`).

Regenerate the committed database + canonical JSON reproducibly from the corpus:

```bash
# 1. Copy the corpus to a clean directory (keeps the workspace index out of it).
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

Two properties this recipe does NOT claim, for every fixture below as well:

- **`colby.db` is not byte-reproducible.** SQLite's header carries a change
  counter, so a freshly indexed database differs from the committed one in the
  first page even when every row matches. Only the `--gen-golden` artifacts are
  compared byte-for-byte; the `.db` is an input to that dump, not a golden.
- **`schema.sql` records `.schema` statement ORDER, which can shift.** The order
  reflects how the current binary creates its objects. Regenerating a fixture
  whose committed `schema.sql` was produced by an earlier binary can therefore
  reorder statements (e.g. `idx_edges_identity`) with no schema change. Always
  regenerate `schema.sql` from the database you are committing — steps 3 and 4
  do exactly that, which keeps the pair self-consistent — and review an
  order-only diff as expected rather than as drift.

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
# 1. Copy the corpus to a clean directory (keeps the workspace index out of it).
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

### Python fixture

The dedicated `reference/golden/python/` fixture guards Python bare
class-as-value function references without changing the shared `mini` corpus.
Its three-file source corpus lives at `crates/codegraph-bench/fixtures/python/`
and pins six positive `References` edges:

- same-file class values in a direct return, assignment RHS, registry-pair
  value, call argument, and list literal;
- one cross-file `ImportedClass` value. The import syntax is present, but real
  Python nodes are not marked exported, so import Gate 3b is unreachable. This
  edge is intentionally resolved by Gate 3a's unique cross-file name match at
  confidence 0.8.

It also pins two negative boundaries: a tuple return does not recurse into
`TupleA`/`TupleB`, and a bare `handler` parameter remains an unresolved
`function_ref` rather than resolving to a same-named method. The undefined
`register(...)` calls used by the argument and method shapes legitimately remain
as two unresolved `calls` rows.

Regenerate the committed database and canonical artifacts from a clean corpus:

```bash
# 1. Copy the corpus to a clean directory (keeps the workspace index out of it).
rm -rf /tmp/cg-fixture-python
cp -r crates/codegraph-bench/fixtures/python /tmp/cg-fixture-python

# 2. Index it with OUR release binary (never hand-write the golden).
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/cg-fixture-python

# 3. Commit the produced database as the fixture's colby.db.
mkdir -p reference/golden/python
cp /tmp/cg-fixture-python/.codegraph/codegraph.db reference/golden/python/colby.db

# 4. Dump canonical JSON + schema from that exact database.
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/python/colby.db reference/golden/python
```

As with every fixture, compare only `nodes.json`, `edges.json`, `refs.json`,
`files.json`, and `schema.sql` byte-for-byte. `colby.db` itself is not a
byte-reproducibility contract because SQLite updates its header change counter.
Regenerate `schema.sql` from the database being committed; statement ordering
may differ between binary versions, but its normalized statement set must not.
The tests `generated_golden_matches_committed_python_fixture` and
`python_db_is_self_equivalent_to_python_golden` enforce database/artifact
self-equivalence.

### Kotlin fixture

The dedicated `reference/golden/kotlin/` fixture guards Kotlin callable
signatures (#1495) without changing a shared corpus. Its single source file,
`crates/codegraph-bench/fixtures/kotlin/signatures.kt`, pins five positive
shapes: an explicit return, an inferred return, a generic return, a multiline
class method with a nullable generic return, and an extension function. The
`Processor` primary constructor is the negative boundary: the class signature
stays null and no constructor method is synthesized.

Regenerate the committed database and canonical artifacts from a clean corpus:

```bash
rm -rf /tmp/cg-fixture-kotlin
cp -r crates/codegraph-bench/fixtures/kotlin /tmp/cg-fixture-kotlin
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/cg-fixture-kotlin
mkdir -p reference/golden/kotlin
cp /tmp/cg-fixture-kotlin/.codegraph/codegraph.db reference/golden/kotlin/colby.db
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/kotlin/colby.db reference/golden/kotlin
```

As with every fixture, only the five text artifacts are byte-compared;
`colby.db` is not byte-reproducible. Compare `schema.sql` by normalized statement
set when an older binary changes statement ordering. The tests
`generated_golden_matches_committed_kotlin_fixture` and
`kotlin_db_is_self_equivalent_to_kotlin_golden` enforce database/artifact
self-equivalence.

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
  an `Extends` ref to `ns::Tpl` (qualified head kept, template args stripped).
  Since the C++ namespace-prefix work (Release D) stores `Tpl`'s qualified name as
  `ns::Tpl`, this ref now RESOLVES to a real `Extends` edge (`Q` → `ns::Tpl`) in
  `edges.json` instead of remaining an unresolved ref.

Three further files exercise the Release D C++ extraction gains:

- **namespace prefix + `ns::fn()` resolution** — `namespaced.cpp` defines
  `namespace ns { void compute() {} }` (qualified name `ns::compute`) and calls
  `ns::compute()` from `run_namespaced`; the call resolves to a `Calls` edge via
  the existing qualified-name matcher (no resolver change).
- **template-argument call stripping** — `templated_call.cpp` defines
  `template <typename T> void process(T)` and calls `process<int>(0)`; the
  `<int>` template args are stripped at extraction so the call links to `process`.
- **Unreal-Engine reflection-macro recovery + `.h` C++ detection** — `ue_actor.h`
  is a lean UE header whose only C++ signal is `class ENGINE_API UFoo : public
UObject` plus line-leading `GENERATED_BODY()`/`UPROPERTY(...)`/`UFUNCTION()`,
  a member-level `ENGINE_API`, and no explicit `public:`. Content sniffing
  reclassifies the `.h` to C++, and the offset-preserving pre-parse blanking
  recovers the `UFoo` class + its `Extends UObject` clause (both dropped before).

Five further files exercise the Batch B C/C++ gains:

- **C leading attribute macros** (upstream #1311) — `attr_macro.c` is the one `.c`
  file in this corpus (extension-mapped to `Language::C`, so it guards the C
  walker, not the C++ one). It `#define`s an attribute macro + a `VOID` macro,
  `typedef`s `UINT32`, and declares four functions: two behind the macro
  (`GoodName` with a macro return type, `LostName` with a typedef'd one), one
  without it (`NoAttr`, the control), and one pointer-returning (`PtrRet`).
  tree-sitter's C grammar reads the macro as the type and the real return type as
  the declarator, so before the fix these indexed under the RETURN TYPE's name
  (`VOID` / `UINT32`); the golden now pins all four under their real names with
  their real return types. The blank fires ONLY because `#define SEC_ATTR
__attribute__((section(".init")))` is visible IN THIS FILE — the pass demands
  same-file `#define` proof that a leading token is attribute-like, so this fixture
  also pins that evidence requirement, not just the macro's name.

- **namespaced out-of-line method + fully-qualified call** (upstream #1310) —
  `namespaced_member.hpp` declares `namespace simulator { class ManifestStartup }`
  with a static `Apply`, and `namespaced_member.cpp` defines it OUT OF LINE inside
  the same namespace block, then calls it through the fully-qualified path
  (`simulator::ManifestStartup::Apply(1)`) from a function OUTSIDE the namespace.
  The receiver qualifier is spelled relative to the namespace, so the golden pins
  the method at `simulator::ManifestStartup::Apply` (matching the class node's
  `simulator::ManifestStartup`) plus the `Calls` edge
  `run_manifest → simulator::ManifestStartup::Apply` resolved by
  `qualified-name` — the edge that a namespace-less qualifier loses.

- **out-of-line template method receivers** (upstream #1309) —
  `template_method.cpp` declares `template <typename T> class Box` with `get` /
  `set` and defines both OUT OF LINE (`template <typename T> T Box<T>::get()`).
  The receiver qualifier carries `<T>`, which the class node never spells, so the
  golden pins both methods at `Box::get` / `Box::set` (template args stripped)
  plus the `contains` edges from `class Box` to each — the link that a `Box<T>::`
  qualifier breaks.

A further file exercises the Batch B explicit-operator gain (upstream #1268):

- **explicit operator calls** — `operators.cpp` defines `struct Vec2` with
  `operator+` / `operator[]` / a plain `get`, then calls each through the
  EXPLICIT syntax (`a.operator+(b)`, `a.operator[](3)`, `p->operator+(b)`) plus
  one plain `a.get()` control. tree-sitter-cpp strands the `operator_name` in an
  ERROR child, so before the fix the extractor emitted the bare receiver (`a`)
  and no edge existed; the golden now carries four `Calls` edges resolved by
  `instance-method` at confidence 0.9 (`Vec2::operator+` twice,
  `Vec2::operator[]`, `Vec2::get`).

Seven further files exercise the Tier 3 aggregate work — MSVC COM `interface`
(upstream #1519) and first-class `union` nodes (upstream PR #1516). **This corpus
is the C-FAMILY corpus, not a strictly-C++ one**: it already held a `.c` file, and
Tier 3 adds a `.mm` file whose extension maps to the ObjC spec, so `cpp/` names the
family rather than the dialect.

- **MSVC COM `interface` positives** — `com_interface.hpp` carries `struct SControl`
  as the control plus three `interface` shapes (`IWidget : IBase` with a base clause,
  `IPlain` without one, and `INewlineBrace` with the brace on the next line). The
  keyword is not C++, so tree-sitter reads each as a `function_definition`: the
  container became a `function`, its member `Run` a free `function` instead of a
  `method`, and the base clause vanished. The offset-preserving pre-parse rewrites
  the line-leading keyword to `struct` + 3 spaces (9 bytes → 9 bytes, so every line
  and column is unchanged), and the golden now pins all three as `struct` with
  `IWidget::Run` a `method` and TWO `extends IBase` refs. `int interface_count = 0;`
  is the mid-line negative in the same file.
- **MSVC COM `interface` negatives** — `neg_interface.hpp` holds the five shapes the
  guard must DECLINE, and pins them by extraction-INVARIANCE rather than by "the
  substitution did not fire": a block comment containing `interface INegComment;`, a
  C++/CLI `interface class`, an `interface struct`, a raw string containing a full
  `interface INegGhost {…};` declaration, and a `#define` continuation line carrying
  `interface INegMacro`. The golden pins exactly THREE nodes, the sharpest value
  being `INegCli`'s `docstring` — the verbatim `interface INegComment;`. A
  comment-blind substitution rewrites that string to `struct    INegComment;`, which
  is how a leaking guard is caught. `INegCli`/`INegStructKw` staying `function` is
  pre-existing garbage from the C++/CLI misparse and is deliberately preserved.
- **C union** — `union_agg.c` (the second `.c` file, so the C walker is under test)
  covers `union Named` plus both `typedef union` RE-KINDS (`AnonU` and `NamedU` were
  `type_alias` before and are `union` now — node ids change), with `struct Ctl` /
  `AnonS` as controls and three negatives: `union Fwd;` (bodiless forward
  declaration, stays unindexed, `struct FwdS;` as its control) and
  `union { int q; } anon_var;` (anonymous, mints nothing, as the anonymous struct
  already did). `typedef union NamedTag { … } NamedU;` mints exactly ONE node named
  `NamedU`; the tag never becomes a node.
- **C++ union member method** — `union_agg.cpp` gives `union WithMethod` and
  `struct SWithMethod` a SAME-NAMED `read_field`, plus one call on each receiver.
  Before the fix the union's member leaked to file scope as a free `function`, so
  `w.read_field()` bound `SWithMethod::read_field` — a FALSE edge to the wrong type,
  not merely a missing one. The golden pins each call on its own receiver's member,
  so receiver type has to decide.
- **ObjC union** — `union_agg.mm` is the repo's ONLY Objective-C fixture (`.mm` maps
  to the ObjC spec). It pins `union ObjcU` beside `struct ObjcS`.
- **`Calls` → `Instantiates` promotion** — `instantiate_agg.cpp` has `Reg r = Reg();`
  where the union is the only node of that name (measured pre-fix: a DANGLING `calls
Reg` ref, because no union node existed to bind), with `Ctl c = Ctl();` as the
  control. The golden pins `instantiates mk_union -> union:Reg` and NO surviving
  `calls` edge.
- **`Instantiates` candidate ranking** — `instantiate_rank.cpp` has `Value v{1};`
  competing against a same-named `void Value()`. Measured pre-fix it bound
  `function:Value`, the WRONG target; the golden pins the union winning, with
  `Packet p{2};` as the control. Kept in a SEPARATE file from the promotion case on
  purpose: one file mixing both mechanisms would make either revert-proof ambiguous.

**Why the union/struct members are named `read_field` and not `get`.** `get` is the
only duplicated symbol name in this corpus already (`operators.cpp:5` `Vec2::get`
and `template_method.cpp:12` `Box::get`), and one of those carries a live
2-candidate `calls` edge. Naming the new members `get` would take that candidate set
to four and put an existing, unrelated golden edge at risk of re-pointing — drift in
files this change has no business touching. `read_field` keeps the property that
matters (two same-named members inside the new file, so receiver type must
disambiguate) and drops the one that only adds risk.

The minimal source corpus lives at `crates/codegraph-bench/fixtures/cpp/`
(`attr_macro.c`, `base.hpp`, `com_interface.hpp`, `derived.cpp`,
`instantiate_agg.cpp`, `instantiate_rank.cpp`, `namespaced.cpp`,
`namespaced_member.cpp`, `namespaced_member.hpp`, `neg_interface.hpp`,
`operators.cpp`, `template_method.cpp`, `templated_call.cpp`, `ue_actor.h`,
`union_agg.c`, `union_agg.cpp`, `union_agg.mm` — 17 files). The inheritance base
classes live in a `.hpp` file (not `.h`,
which maps to `Language::C` by extension); `ue_actor.h` deliberately uses `.h` to
guard the content-based C++ reclassification, and `attr_macro.c` uses `.c` so the
C walker (not the C++ one) is the thing under test.

Regenerate the committed database + canonical JSON reproducibly from the corpus:

```bash
# 1. Copy the corpus to a clean directory (keeps the workspace index out of it).
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

### Rust fixture

`reference/golden/rust/` guards the two Tier 3 items that need Rust source, which
no other corpus could reach — before it there was no `.rs` file anywhere under
`crates/codegraph-bench/fixtures/`:

- **unit structs are indexed** (upstream #1513 / PR #1514) — `lib.rs` declares
  `struct UnitStruct;` beside `BraceStruct { … }` and `TupleStruct(u8)`, and gives
  all three an `impl Greet for`. A bodiless `struct NAME;` is a COMPLETE definition
  in Rust, not a forward declaration, so before the fix the unit struct produced no
  node at all — and its `implements` edge went with it, leaving `UnitStruct::greet`
  an orphan naming a type the graph did not contain. The golden pins exactly three
  `struct` nodes and **four** `implements` edges (three structs + the union).
- **`union` is a first-class kind** (upstream PR #1516) — `union Bits` carries an
  inherent `impl` (`raw`) and a trait `impl` (`greet`). The golden pins `union:Bits`
  with `contains union:Bits -> method:raw`, `contains union:Bits -> method:greet` and
  `implements union:Bits -> trait:Greet`.
- **cross-file resolution** — `consumer.rs` calls `u.greet()` / `b.greet()` on a
  `&UnitStruct` and a `&Bits`, so `implements` and method resolution have to work
  across files rather than only within one.

**The Rust corpus deliberately makes NO instantiation claim.** An `instantiates` edge
fires only for the CALL-EXPRESSION construction form: `TupleStruct(2)` yes, a bare
path `UnitStruct` no, a struct literal `Bits { i: 0 }` no. A Rust union is
constructible only as `Bits { … }`, so no Rust union can ever emit that edge —
`make_unit` / `make_bits` are return-type references, not instantiation assertions,
and the golden carries zero `instantiates` edges. Instantiation is pinned in C++
instead (`instantiate_agg.cpp` / `instantiate_rank.cpp`), which is upstream's own
shape. The corpus carries **no `Cargo.toml`**: it is indexed, not compiled, and a
manifest inside the workspace tree could confuse `cargo`.

Regenerate reproducibly (identical recipe to the C++ fixture, substituting `rust`):

```bash
mkdir -p reference/golden/rust
rm -rf /tmp/cg-fixture-rust
cp -r crates/codegraph-bench/fixtures/rust /tmp/cg-fixture-rust
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/cg-fixture-rust
cp /tmp/cg-fixture-rust/.codegraph/codegraph.db reference/golden/rust/colby.db
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/rust/colby.db reference/golden/rust
```

`generated_golden_matches_committed_rust_fixture` and
`rust_db_is_self_equivalent_to_rust_golden` enforce byte-stability.

### Go fixture

`reference/golden/go/` byte-pins `files.generated` — the content-header
generated-file detection ported from upstream #1500 (`16e1749`) plus the
Wrangler double-`by` fix (`57e0854`). It is the repo's only Go corpus, and the
only corpus where `files.generated` is anything other than `0`.

Go is the language that forces content detection to exist: its convention for a
generated file is a **comment banner**, not a filename suffix, so a machine-written
`payroll.go` sitting beside hand-written use-cases is invisible to the path-only
`is_generated_file`. Renaming is not an option — `payroll.go` is a legal, ordinary
Go filename.

Six files pin BOTH values of the flag, three each way:

| fixture              | `generated` | what it guards                                                                                                          |
| -------------------- | ----------- | ----------------------------------------------------------------------------------------------------------------------- |
| `payroll.go`         | **1**       | pattern 1, Go's codified `Code generated … DO NOT EDIT.` — the #1500 defect itself                                      |
| `worker_types.go`    | **1**       | pattern 6 / CG-25, Wrangler's TWO `by` clauses (`Generated by Wrangler by running …`)                                   |
| `api.pb.go`          | **1**       | the PATH signal still writes 1 with no banner present                                                                   |
| `payroll_usecase.go` | **0**       | the must-not-demote side: hand-written code beside a generated sibling                                                  |
| `nightly.go`         | **0**       | pattern 6 PRECISION — one `by` clause (`generated by running the ETL job`) is ordinary prose                            |
| `generator.go`       | **0**       | the comment-line fence: the banner is a `const` string in the function BODY, so a generator's own source is not flagged |

`payroll.go` and `payroll_usecase.go` both define `ComputePay`, which is what makes
the ranking effect observable: without the content signal the generated definition
wins on name overlap alone.

Regenerate reproducibly (identical recipe to the Rust fixture, substituting `go`):

```bash
mkdir -p reference/golden/go
rm -rf /tmp/cg-fixture-go
cp -r crates/codegraph-bench/fixtures/go /tmp/cg-fixture-go
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/cg-fixture-go
cp /tmp/cg-fixture-go/.codegraph/codegraph.db reference/golden/go/colby.db
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/go/colby.db reference/golden/go
```

`generated_golden_matches_committed_go_fixture` and
`go_db_is_self_equivalent_to_go_golden` enforce byte-stability.

### Mini schema rebuild (schema-migration recipe)

`reference/golden/mini/` is the one corpus with **no re-indexable provenance** — it
is upstream-derived, and its `files.modified_at` is a JS `Date.now()`
(`typeof(modified_at) = real`, versus `integer` in every corpus we produce). So the
recipe above cannot regenerate it: re-indexing the fixture would replace upstream's
data with ours.

An in-place `Store::open` migration is also **not** sufficient when a schema
migration adds an index. `CREATE INDEX` APPENDS a `sqlite_master` row and never
reorders an existing one, and the golden `schema.sql` is a `sqlite_master` dump
compared as a strict string, so a migrated corpus accumulates its indexes in
migration order while a freshly-created one carries them in `BASE_SCHEMA` order.
That is why `mini` and `godot` used to carry `idx_edges_identity` LAST while the
other corpora carried it alphabetically.

For a schema migration, rebuild `mini` on the fresh schema and transplant its rows:

```bash
# 1. A fresh index over mini's own fixture → a database created from the CURRENT
#    BASE_SCHEMA, so every index lands in declaration order.
rm -rf /tmp/mini-rebuild && mkdir -p /tmp/mini-rebuild
cp -a crates/codegraph-bench/fixtures/mini/. /tmp/mini-rebuild/
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/mini-rebuild
DB=/tmp/mini-rebuild/.codegraph/codegraph.db

# 2. Replace its freshly-extracted rows with mini's COMMITTED rows, preserving
#    the upstream-derived values instead of re-extracting them. Run from the repo
#    root — the ATTACH path is relative.
sqlite3 "$DB" "
  ATTACH DATABASE 'reference/golden/mini/colby.db' AS src;
  BEGIN;
  DELETE FROM unresolved_refs; DELETE FROM edges; DELETE FROM files;
  DELETE FROM nodes; DELETE FROM project_metadata;
  INSERT INTO nodes            SELECT * FROM src.nodes;
  INSERT INTO edges            SELECT * FROM src.edges;
  INSERT INTO unresolved_refs  SELECT * FROM src.unresolved_refs;
  INSERT INTO project_metadata SELECT * FROM src.project_metadata;
  INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors)
    SELECT path, content_hash, language, size, modified_at, indexed_at, node_count, errors FROM src.files;
  COMMIT;
  DETACH src;"

# 3. Commit as the fixture's colby.db, then re-derive the JSON + schema goldens.
cp "$DB" reference/golden/mini/colby.db
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/mini/colby.db reference/golden/mini
```

Two constraints in that transplant are load-bearing:

- **The `files` insert MUST use an explicit column list.** `SELECT *` fails with
  `table files has N columns but 8 values were supplied` once a column is added,
  and the omitted column correctly takes its `DEFAULT`, which is exactly the
  migrated-but-not-re-extracted semantics the goldens should pin.
- **Do NOT transplant `schema_versions`.** `codegraph_store::test_support::
finalize_current_test_fixture` calls `Store::open` on a COPY of this database
  from seven call sites. Copying `mini`'s rows would leave `MAX(version)` behind
  the current schema on a database that already HAS the new column, so
  `run_pending_migrations` would fire the `ALTER TABLE` against it and every one of
  those tests would die on `duplicate column name: …`. Keeping the fresh rows
  leaves nothing pending, so `Store::open` is a no-op on the copy.

Verify the rebuild preserved provenance and normalised the order:

```bash
sqlite3 reference/golden/mini/colby.db 'SELECT DISTINCT typeof(modified_at) FROM files;'  # real
sqlite3 reference/golden/mini/colby.db 'SELECT COUNT(*) FROM nodes;'                      # 13
sqlite3 reference/golden/mini/colby.db 'SELECT COUNT(*) FROM edges;'                      # 21
diff reference/golden/mini/schema.sql reference/golden/cpp/schema.sql                      # byte-equal
```

That last `diff` is the real proof: the rebuilt `mini` schema is now the same text
as a freshly-created corpus's, so every corpus `schema.sql` shares one hash.

### Metal fixture

A fifth golden fixture, `reference/golden/metal/`, guards Metal Shading Language
support (upstream #1121 / `cc89146`). MSL ≈ C++14 and rides the existing
`tree-sitter-cpp` grammar — `.metal` maps to `Language::Cpp` with **no new
`Language` variant**. It guards the `.metal`-gated `[[attribute]]` blank: MSL's
post-declarator attributes (`float4 position [[position]];`) otherwise misparse a
struct field into a spurious `extends` edge from the struct to the field's own
type. The corpus (`crates/codegraph-bench/fixtures/metal/shader.metal`) defines
`float4`/`float2` structs, a `VertexIn` struct whose fields carry
`[[position]]`/`[[user(locn0)]]` attributes on those self-defined types, and a
`vertex_main` function that calls a `tint` helper. The golden must show:

- `shader.metal` with `"language": "cpp"`;
- `VertexIn`/`float4`/`float2` as ordinary structs with **no `Extends` edge**
  (the attribute blank prevents the spurious `VertexIn extends float4`);
- the intra-shader `vertex_main` → `tint` `Calls` edge.

The `[[attribute]]` blank fires ONLY for `.metal` files; a `.cpp`/`.hpp` with a
regular `[[nodiscard]]` attribute is byte-identical through pre-parse (proven by
the `metal_attribute_blanked_only_for_dot_metal` unit test in `lang/cpp.rs`).

### CUDA fixture

A sixth golden fixture, `reference/golden/cuda/`, guards CUDA support (the
CUDA-language parts of upstream #1172 / `e1a8d88`). CUDA ≈ C++ + dialect tokens
and likewise rides `tree-sitter-cpp` — `.cu`/`.cuh` map to `Language::Cpp` with
**no new `Language` variant**. It guards the CUDA pre-parse blank (execution-space
specifiers + `<<<grid, block>>>` launch configs, offset-preserving and
brace-balance-checked) and macro-defined-kernel name recovery. The corpus
(`crates/codegraph-bench/fixtures/cuda/kernel.cu`) defines a `__global__ void
add_kernel`, a templated `__global__ scale_kernel`, a
`DEFINE_FLASH_FORWARD_KERNEL(my_kernel, …)` macro-defined kernel, and a `launch`
host function with a plain launch and a templated launch. The golden must show:

- `kernel.cu` with `"language": "cpp"`;
- `add_kernel`, `scale_kernel`, `my_kernel`, `launch` as functions — the
  macro kernel under its real name `my_kernel`, NOT `DEFINE_FLASH_FORWARD_KERNEL`;
- host→kernel `Calls` edges `launch` → `add_kernel` and `launch` → `scale_kernel`
  (the `<<<…>>>` blank restores the call; the templated launch rides the
  already-landed template-argument strip).

The CUDA blank fires for `.cu`/`.cuh` files OR any C/C++-family file whose content
carries a strong CUDA marker (`__global__`/`__device__`/`__constant__`/
`cudaStream_t`), so CUDA living in `.h`/`.hpp` headers is recognized.

Regenerate both new fixtures reproducibly (identical recipe to the C++ fixture,
substituting `metal`/`cuda`):

```bash
rm -rf /tmp/cg-fixture-metal && cp -r crates/codegraph-bench/fixtures/metal /tmp/cg-fixture-metal
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 ./target/release/codegraph init /tmp/cg-fixture-metal
cp /tmp/cg-fixture-metal/.codegraph/codegraph.db reference/golden/metal/colby.db
cargo run -p codegraph-bench --bin bench -- --gen-golden reference/golden/metal/colby.db reference/golden/metal
# …and the same for cuda.
```

The `generated_golden_matches_committed_{metal,cuda}_fixture` and
`{metal,cuda}_db_is_self_equivalent_to_{metal,cuda}_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce byte-stability.

### ArkTS fixture

A seventh golden fixture, `reference/golden/arkts/`, guards ArkTS (HarmonyOS /
OpenHarmony `.ets`) extraction (the extraction slice of upstream #1186 /
`9915221`). Unlike Metal/CUDA, ArkTS is a **new `Language::ArkTs` variant** backed
by a **dedicated `tree-sitter-arkts` grammar** — a TypeScript-superset fork that
understands the ArkUI `@Component struct` syntax `tree-sitter-typescript` cannot
parse. `.ets` maps to `Language::ArkTs`; plain `.ts` stays TypeScript. The corpus
(`crates/codegraph-bench/fixtures/arkts/component.ets`) has an `import`, a global
`function helper`, a `function driver` that calls `helper`, a `@Component struct
MyView` with a `build()` method, and a plain `class Model`. The golden must show:

- `component.ets` with `"language": "arkts"`;
- `MyView` as a `NodeKind::Struct` with its `build` method as a member (via the
  existing `extract_struct` path — no walker change);
- `helper`/`driver` functions, the `Model` class, and the `../foo` import node;
- the `driver` → `helper` `Calls` edge (plain `call_expression`).

The ArkUI dynamic-dispatch / callback-synthesizer bridges are DEFERRED — the
port has no callback synthesizer. So `ARKTS_SPEC` uses `call_types =
["call_expression"]` only (no `arkui_component_expression` component-instantiation
edges) and does NOT override `extract_modifiers` (the decorator hook). Adding the
variant is byte-neutral for `colby.schema.sql` (language is a stored TEXT value,
not DDL) and for the six existing goldens (none holds a `.ets` file).

Regenerate reproducibly (identical recipe to the C++ fixture, substituting
`arkts`):

```bash
rm -rf /tmp/cg-fixture-arkts && cp -r crates/codegraph-bench/fixtures/arkts /tmp/cg-fixture-arkts
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 ./target/release/codegraph init /tmp/cg-fixture-arkts
cp /tmp/cg-fixture-arkts/.codegraph/codegraph.db reference/golden/arkts/colby.db
cargo run -p codegraph-bench --bin bench -- --gen-golden reference/golden/arkts/colby.db reference/golden/arkts
```

The `generated_golden_matches_committed_arkts_fixture` and
`arkts_db_is_self_equivalent_to_arkts_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce byte-stability.

### Solidity fixture

An eighth golden fixture, `reference/golden/solidity/`, guards Solidity (`.sol`)
extraction (upstream #1170 / `1441933`). Solidity is a **new `Language::Solidity`
variant** backed by a **dedicated `tree-sitter-solidity` grammar**. `.sol` maps to
`Language::Solidity`. The corpus (`crates/codegraph-bench/fixtures/solidity/`) has
an `IERC20.sol` interface and a `Token.sol` that imports it, declares a file-level
`error` and a file-level `constant`, and a `contract Token is IERC20` carrying a
state variable, an `event`, an `enum`, a `struct`, a `modifier`, a `constructor`,
`fallback`/`receive`, and a `transfer` function guarded by the modifier that
`emit`s the event, plus a `library Math`. What it guards:

- both `.sol` files with `"language": "solidity"`;
- `contract Token` / `library Math` as `NodeKind::Class`, `interface IERC20` as
  `NodeKind::Interface`, `struct Holder` as `NodeKind::Struct`, `enum Status` as
  `NodeKind::Enum` with its `Active`/`Closed` members (bare-text `enum_value`);
- functions/modifiers/methods, including the synthetic `constructor` / `fallback`
  / `receive` method names (nameless grammar nodes);
- state variable / struct member / `event` / `error` as `NodeKind::Field` name
  nodes (direct-`name` field, no `variable_declarator`), including the file-level
  `Unauthorized` error and the file-level `MAX_SUPPLY` constant;
- the `./IERC20.sol` import node + `imports` edge;
- `is`-inheritance emitted as an `Extends` ref, promoted by the EXISTING resolver
  to an `Implements` edge `Token → IERC20` (interface target, present in-corpus);
- `emit`/header `modifier_invocation` `Calls` edges (`transfer → Transfer`,
  `transfer → onlyOwner`), resolved to same-file targets.

Because the fixture is fully self-contained, every ref resolves in-corpus, so
`refs.json` is empty and `edges.json` holds only RESOLVED edges — the expected
post-resolution state. No `FrameworkResolver` impl is involved; the
`Extends → Implements` promotion is the same path Java/C# use
(`resolver.rs:1231-1247`). Adding the variant is byte-neutral for
`colby.schema.sql` (language is a stored TEXT value, not DDL) and for the seven
existing goldens (none holds a `.sol` file).

Regenerate reproducibly (identical recipe to the ArkTS fixture, substituting
`solidity`):

```bash
rm -rf /tmp/cg-fixture-solidity && cp -r crates/codegraph-bench/fixtures/solidity /tmp/cg-fixture-solidity
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 ./target/release/codegraph init /tmp/cg-fixture-solidity
cp /tmp/cg-fixture-solidity/.codegraph/codegraph.db reference/golden/solidity/colby.db
cargo run -p codegraph-bench --bin bench -- --gen-golden reference/golden/solidity/colby.db reference/golden/solidity
```

The `generated_golden_matches_committed_solidity_fixture` and
`solidity_db_is_self_equivalent_to_solidity_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce byte-stability.

### Nix fixture

A ninth golden fixture, `reference/golden/nix/`, guards Nix (`.nix`) extraction
(upstream #1190 / `7f32513`, the extraction slice only). Nix is a **new
`Language::Nix` variant** backed by a **dedicated `tree-sitter-nix` grammar**.
`.nix` maps to `Language::Nix`. Because Nix is an expression language with no
C-family `class`/`struct`/`method`/`enum` node kinds, `NIX_SPEC` has all-empty
type-sets and the extraction is driven by the `Language::Nix`-guarded
`visit_nix_node` walker extension. The corpus
(`crates/codegraph-bench/fixtures/nix/`) has a top-level lambda
`{ pkgs, lib }: …`, a `let … in`, a returned attrset with bindings, an
`import ./foo.nix`, a `pkgs.callPackage ./bar.nix { }`, an `inherit lib;`, an
`imports = [ ./foo.nix ./bar.nix ]` module list, and a curried `build = { src }:
…` lambda. What it guards:

- all three `.nix` files with `"language": "nix"`;
- a `binding` whose value is a lambda → `NodeKind::Function` with a formatted
  curried-param signature (`build` → `{ src }`, `double` → `(x)`);
- a non-lambda `binding` and each `inherit`ed name → `NodeKind::Variable`;
- `import ./foo.nix`, `callPackage ./bar.nix { }`, and the literal
  `imports`-list paths → `NodeKind::Import` nodes + `Imports` refs;
- an `apply_expression` call → `Calls` ref, deduped across curried levels
  (`pkgs.mkDerivation`, `pkgs.callPackage`, `stdenv.mkDerivation`).

The `imports`/`callPackage` path refs to `./foo.nix` / `./bar.nix` resolve
in-corpus (both files exist), so `refs.json` retains only the three unresolved
`Calls` refs — the module-system option-path synthesizer, lexical-scope
resolution gates, callback synthesizer, and import-resolver module-list wiring
that upstream bundles with the same commit are **DEFERRED**, so no new Nix
resolve code binds anything. Adding the variant is byte-neutral for
`colby.schema.sql` (language is a stored TEXT value, not DDL) and for the eight
existing goldens (none holds a `.nix` file).

Regenerate reproducibly (identical recipe to the Solidity fixture, substituting
`nix`):

```bash
rm -rf /tmp/cg-fixture-nix && cp -r crates/codegraph-bench/fixtures/nix /tmp/cg-fixture-nix
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 ./target/release/codegraph init /tmp/cg-fixture-nix
cp /tmp/cg-fixture-nix/.codegraph/codegraph.db reference/golden/nix/colby.db
cargo run -p codegraph-bench --bin bench -- --gen-golden reference/golden/nix/colby.db reference/golden/nix
```

The `generated_golden_matches_committed_nix_fixture` and
`nix_db_is_self_equivalent_to_nix_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce byte-stability.

### Terraform fixture

A tenth golden fixture, `reference/golden/terraform/`, guards Terraform/OpenTofu
(HCL) extraction (upstream #1173 / `6c24f4b`, the extraction slice only).
Terraform is a **new `Language::Terraform` variant** backed by a **dedicated
`tree-sitter-hcl` grammar** (`.tf`/`.tfvars`/`.tofu` → `Language::Terraform`).
HCL is intentionally generic — every top-level construct is a `block`
distinguished only by its first `identifier` child — so `TERRAFORM_SPEC` has
all-empty type-sets and extraction is driven by the `Language::Terraform`-guarded
`visit_terraform_node` walker extension. The corpus
(`crates/codegraph-bench/fixtures/terraform/main.tf`) is a single deterministic
file with a `terraform {}` settings block, a `provider "aws"`, a
`variable "region"`, a `locals` block, a `data "aws_ami" "ubuntu"`, a
`resource "aws_s3_bucket" "b"`, a `module "vpc"`, and two `output` blocks. What
it guards:

- the `.tf` file with `"language": "terraform"`;
- block-type dispatch: `resource`/`data` → `NodeKind::Class` (qualified `T.N` /
  `data.T.N`), `module` → `NodeKind::Module` (`module.M`), `variable`/`output` →
  `NodeKind::Variable` (`var.V` / `output.O`, `is_exported`), `provider` →
  `NodeKind::Namespace` (`provider.P`), `locals` attributes → `NodeKind::Constant`
  per attribute (`local.k`);
- plain attribute-expression traversal refs
  (`var.X`/`local.X`/`module.M`/`data.T.N`/`<type>.<name>`) → `References`, with
  built-in heads (`each`/`count`/`self`/`path`/`terraform`) skipped.

The plain traversal refs with a unique same-file target resolve via the existing
generic qualified-name matcher: `var.region` ×3 → `variable "region"`,
`aws_s3_bucket.b` → the resource, `module.vpc` → the module (each an EDGE, absent
from `refs.json`). The undeclared `aws_kms_key.logs` stays the sole unresolved
`refs.json` row. The module-boundary `TerraformResolver`, `emitModuleWiring`'s
`:`-scoped refs (`module.M:file`/`:var.X`/`:output.X`), the `.tfvars`
top-level-assignment `var.X` ref, and the `module.M:output.<out>` scoped half of
`qualifyReference` are all **DEFERRED** — the port keeps its single
`GodotResolver` — so no `:`-scoped ref is emitted. Adding the variant is
byte-neutral for `colby.schema.sql` (language is a stored TEXT value, not DDL)
and for the nine existing goldens (none holds a `.tf`/`.tfvars`/`.tofu` file).

Regenerate reproducibly (identical recipe to the Nix fixture, substituting
`terraform`):

```bash
rm -rf /tmp/cg-fixture-terraform && cp -r crates/codegraph-bench/fixtures/terraform /tmp/cg-fixture-terraform
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 ./target/release/codegraph init /tmp/cg-fixture-terraform
cp /tmp/cg-fixture-terraform/.codegraph/codegraph.db reference/golden/terraform/colby.db
cargo run -p codegraph-bench --bin bench -- --gen-golden reference/golden/terraform/colby.db reference/golden/terraform
```

The `generated_golden_matches_committed_terraform_fixture` and
`terraform_db_is_self_equivalent_to_terraform_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce byte-stability.

### Erlang fixture

An eleventh golden fixture, `reference/golden/erlang/`, guards Erlang extraction
(upstream #1165 / `6511722`, the extraction slice only). Erlang is a **new
`Language::Erlang` variant** backed by a **dedicated `tree-sitter-erlang`
grammar** (`.erl`/`.hrl` → `Language::Erlang`). Erlang is form-based — a
function's name lives on its `function_clause`, the grammar emits one `fun_decl`
per clause, `record_decl` carries fields as direct children, and
`-spec`/`-callback`/type bodies parse as `call` nodes — so `ERLANG_SPEC` has
all-empty C-family type-sets (only `package_types`/`import_types` are wired, as
upstream) and extraction is driven by the `Language::Erlang`-guarded
`visit_erlang_node` walker extension. The corpus
(`crates/codegraph-bench/fixtures/erlang/m.erl`) is a single deterministic file
with `-module(m)`, `-export([f/1, g/0])`, `-include("foo.hrl")`, `-define(X, 1)`,
`-record(state, {a, b})`, a `-spec f(integer()) -> integer().`, a two-clause
`f/1`, and a `g/0` that references `fun f/1`, constructs `#state{}`, calls the
remote `other:h()`, and self-calls `g()`. What it guards:

- the `.erl` file with `"language": "erlang"`;
- `-module(m)` → `NodeKind::Namespace` (so every function's qualified name is
  `m::f` — the shape the remote-call branch emits, so `mod:f(...)` resolves
  through the standard qualified-name matcher);
- clause-merge dedup: the two `f/1` clauses merge to exactly ONE
  `NodeKind::Function` `f`;
- `-record(state, {a, b})` → `NodeKind::Struct` `state` with `NodeKind::Field`
  children `a` and `b`;
- `-define(X, 1)` → `NodeKind::Constant` `X`;
- `-include("foo.hrl")` → `NodeKind::Import` + an `Imports` file edge;
- local `g()` → a `Calls` edge, remote `other:h()` → a `Calls` ref `other::h`;
- `fun f/1` (function value) and `#state{}` (record usage) → `References`, NOT
  `Calls`;
- the `-spec f(integer()) -> integer().` and `-callback` / record-field
  type-position `call` nodes mint NO bogus type call refs (no `integer` call).

The local `g()` self-call and the `foo.hrl` include resolve; `other::h` (the
`other` module is absent from the fixture) is the sole unresolved `refs.json`
row. The non-Godot framework bridges — `-behaviour` callback contracts,
`gen_server:call/cast(?MODULE|?SERVER)` → `handle_call`/`handle_cast`, the
`spawn`/`apply`/`proc_lib`/`timer`/`rpc` MFA-argument callee lift, var-module
dispatch, and `.app`/`.app.src` resource-tuple wiring — are all **DEFERRED**, so
none of those edges is emitted. Adding the variant is byte-neutral for
`colby.schema.sql` (language is a stored TEXT value, not DDL) and for the ten
existing goldens (none holds a `.erl`/`.hrl` file).

Regenerate reproducibly (identical recipe to the Terraform fixture, substituting
`erlang`):

```bash
rm -rf /tmp/cg-fixture-erlang && cp -r crates/codegraph-bench/fixtures/erlang /tmp/cg-fixture-erlang
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 ./target/release/codegraph init /tmp/cg-fixture-erlang
cp /tmp/cg-fixture-erlang/.codegraph/codegraph.db reference/golden/erlang/colby.db
cargo run -p codegraph-bench --bin bench -- --gen-golden reference/golden/erlang/colby.db reference/golden/erlang
```

The `generated_golden_matches_committed_erlang_fixture` and
`erlang_db_is_self_equivalent_to_erlang_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce byte-stability.

### CFML fixture

A twelfth golden fixture, `reference/golden/cfml/`, guards CFML / ColdFusion
extraction (upstream #1153 / `816bacb`, the scope-B extraction slice only). CFML
is a **new `Language::Cfml` variant** backed by the **dual-grammar
`tree-sitter-cfml`** crate (`.cfc`/`.cfm`/`.cfs` → `Language::Cfml`). A file's
dialect is picked by a first-token sniff (`is_bare_script_cfml`): script files
parse with the bundled `cfscript` grammar and drive the generic type-set
dispatch; tag files parse with the `cfml` tag grammar and are handled by the
`Language::Cfml`-guarded `visit_cfml_node` walker extension. The corpus
(`crates/codegraph-bench/fixtures/cfml/`) has three deterministic files — a
script `Base.cfc`, a tag `Widget.cfm`, and a bare-script `Gadget.cfs`. What it
guards:

- all three files with `"language": "cfml"`;
- `Base.cfc` (script) → `NodeKind::Class` `Base` (named from the FILE — the
  cfscript `component` is unnamed) + `NodeKind::Function` `ping`;
- `Widget.cfm` (tag) → `NodeKind::Class` `Widget` (from the `name` tag-attr) +
  `NodeKind::Method` `doThing` (access `public`, returntype `void`), and a tag
  `extends="Base"` → `Extends`;
- `Gadget.cfs` (bare script) → `NodeKind::Class` `Gadget` (from the FILE) +
  `NodeKind::Property` `x` + `NodeKind::Function` `doThing`, and a script-style
  `extends="Base"` (`component_attribute`) → `Extends`;
- both `extends Base` refs RESOLVE to the `Base.cfc` component (edges);
  `Gadget.doThing`'s `helper()` call → an unresolved `helper` ref.

The `<cfscript>`-in-tag-body re-parse delegation, the `cfquery` SQL-body
extraction (`LANGUAGE_CFQUERY`), and the CFML framework RESOLVER bridges
(FW/1 / ColdBox / CFWheels, dotted/relative inheritance, receiver-type inference)
are all **DEFERRED**. Adding the variant is byte-neutral for `colby.schema.sql`
(language is a stored TEXT value, not DDL) and for the eleven existing goldens
(none holds a `.cfc`/`.cfm`/`.cfs` file).

Regenerate reproducibly (identical recipe, substituting `cfml`):

```bash
rm -rf /tmp/cg-fixture-cfml && cp -r crates/codegraph-bench/fixtures/cfml /tmp/cg-fixture-cfml
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 ./target/release/codegraph init /tmp/cg-fixture-cfml
cp /tmp/cg-fixture-cfml/.codegraph/codegraph.db reference/golden/cfml/colby.db
cargo run -p codegraph-bench --bin bench -- --gen-golden reference/golden/cfml/colby.db reference/golden/cfml
```

The `generated_golden_matches_committed_cfml_fixture` and
`cfml_db_is_self_equivalent_to_cfml_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce byte-stability.

### TypeScript resolution fixture

The dedicated `reference/golden/typescript/` fixture guards TypeScript export
alias and explicit JavaScript-specifier resolution (#1482 / #1482b) without
changing the shared `mini` corpus. Its six-file source corpus lives at
`crates/codegraph-bench/fixtures/typescript/`; source lines are contractual
because node IDs include the declaration line.

The `runAll` function pins six positive `Calls` edges, in source order:

- `viaConstAlias()` → the local `constTarget` function behind
  `export const constAlias = constTarget`;
- `viaNamedAlias()` → the local `namedTarget` function behind
  `export { namedTarget as namedAlias }`;
- `defaultExportAlias()` → the local `defaultTarget` function behind
  `export default defaultTarget`;
- `viaJsSpecifier()` from `./js_target.js` → `jsTarget` in `js_target.ts`;
- `viaCollision()` from `./collision.js` → `collisionTarget` in
  `collision.ts`, ahead of the real `collision.js` file;
- `viaExtensionless()` → `extensionlessTarget` in `extensionless.ts`, preserving
  the existing extensionless behavior.

Two negative rules make false positives visible: `viaMissing` remains the only
unresolved call and import pair in `refs.json`, with no `Calls` edge; and no
`Calls` edge may target either the exported `Constant` `constAlias` or the
JavaScript `collisionTarget` in `collision.js`.

Regenerate the committed database and canonical artifacts from a clean corpus:

```bash
rm -rf /tmp/cg-fixture-typescript
cp -r crates/codegraph-bench/fixtures/typescript /tmp/cg-fixture-typescript
cargo build --release -p codegraph-rs
CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 \
  ./target/release/codegraph init /tmp/cg-fixture-typescript
mkdir -p reference/golden/typescript
cp /tmp/cg-fixture-typescript/.codegraph/codegraph.db reference/golden/typescript/colby.db
cargo run -p codegraph-bench --bin bench -- \
  --gen-golden reference/golden/typescript/colby.db reference/golden/typescript
```

The `generated_golden_matches_committed_typescript_fixture` and
`typescript_db_is_self_equivalent_to_typescript_golden` tests in
`crates/codegraph-bench/tests/equivalence.rs` enforce artifact/database
self-equivalence. As for every fixture below the Godot caveats, do not compare
`colby.db` bytes; compare the four JSON artifacts byte-for-byte and compare
`schema.sql` as a normalized statement set when statement order differs.

### KNOWN_DIFFS.md format

Tier-3 differences are allowlisted by grep-able lines in
`docs/upstream-sync/KNOWN_DIFFS.md` — the single path
`KnownDiffs::repo_doc_path` hardcodes
(`crates/codegraph-bench/src/oracle/diff.rs`):

```text
RULE tier=3 surface=<surface> key=<substring-or-*> justification=<short-token>
```

Only Tier-3 entries can be allowed. Tier-1 byte mismatches and Tier-2 multiset
mismatches always fail; the differ never weakens those tiers to pass —
`KnownDiffs::allows` returns `false` for anything that is not `Tier::Tier3`, and
`parse_rule` rejects `tier=1` / `tier=2` before that, so a Tier-1/Tier-2 rule
cannot even be written down.

The parser is fail-closed: an unparsable document fails every equivalence
assertion instead of being ignored. A `RULE` line is rejected when a token is
not `key=value`, a key or value is empty, a field name is outside
`tier`/`surface`/`key`/`justification`, a field is repeated, `tier` is anything
other than Tier-3, `surface` is outside the five surfaces the differ reports
(`nodes`, `files`, `schema`, `edges`, `unresolved_refs`), or any of the four
fields is missing. Lines inside a fenced code block are documentation, not
rules — including the template above — and an unterminated fence is an error,
because every `RULE` after it would otherwise be skipped silently.
