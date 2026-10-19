# Index written at schema 9

`codegraph.db` is the database a release build of commit `3878d52` (schema 9,
extraction version 22, the head before schema 10 added `resolved_refs`) wrote
for `crates/codegraph-bench/fixtures/mini/`. The upgrade tests open it with the
current binary to prove that a current index one schema behind migrates in
place on its first incremental sync, without a rebuild, and is not trusted for
stored references until a full index.

Provenance:

- Binary: `cargo build --release --locked -p codegraph-rs --bin codegraph` at
  `3878d5232057da036ed02ef152528a0806d75415` (`codegraph 0.54.0`), sha256
  `e576ec8c08cd37434204e46e51c52e58b163c0d23f6066f924aee8df58340f31`.
- Command: `CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 codegraph init <copy of
  crates/codegraph-bench/fixtures/mini>`, then `.codegraph/codegraph.db` copied
  here.
- `codegraph.db` sha256:
  `70f708187bb53b86a7d07a606ef58f10171d9f6fa4fea658eb71c8d6410209a7`.

Do not regenerate it with a newer binary: its value is that a schema-9 binary
wrote it.
