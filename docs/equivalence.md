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

The minimal source corpus lives at `crates/codegraph-bench/fixtures/cpp/`
(`base.hpp`, `derived.cpp`, `namespaced.cpp`, `templated_call.cpp`,
`ue_actor.h`). The inheritance base classes live in a `.hpp` file (not `.h`,
which maps to `Language::C` by extension); `ue_actor.h` deliberately uses `.h` to
guard the content-based C++ reclassification.

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

### KNOWN_DIFFS.md format

Tier-3 differences are allowlisted by grep-able lines in repo-root
`KNOWN_DIFFS.md`:

```text
RULE tier=3 surface=<surface> key=<substring-or-*> justification=<short-token>
```

Only Tier-3 entries can be allowed. Tier-1 byte mismatches and Tier-2 multiset
mismatches always fail; the differ never weakens those tiers to pass.
