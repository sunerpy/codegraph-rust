# Index written by codegraph v0.54.0

`codegraph.db` is the database the released v0.54.0 binary (schema 8, extraction
version 21) wrote for `crates/codegraph-bench/fixtures/mini/`. The upgrade tests
open it with the current binary to prove that an index from the previous release
migrates: the schema migrates in place and the outdated extraction version
triggers one full rebuild.

Provenance:

- Binary: `codegraph 0.54.0`, sha256
  `9774545e1e565f90cbda944854f15a77431b3f3b606063b009db1c69f48d222b`.
- Command: `CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 codegraph init <copy of
crates/codegraph-bench/fixtures/mini>`, then `.codegraph/codegraph.db` copied
  here.
- `codegraph.db` sha256:
  `83b4bbe6cf5f2485c2887432d368a3a7fbf07d419c8b9b0f8dd0f7b643b03faf`.

Do not regenerate it with a newer binary: its value is that an old release
wrote it.
