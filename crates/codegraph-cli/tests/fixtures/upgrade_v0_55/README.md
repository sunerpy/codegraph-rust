# Index written by codegraph v0.55.2

`codegraph.db` is the database the released v0.55.2 binary (schema 10, extraction
version 22) wrote for `crates/codegraph-bench/fixtures/mini/`. The upgrade tests
open it with the current binary to prove that an index from the previous release
migrates: the outdated extraction version triggers one full rebuild.

Provenance:

- Binary: `codegraph 0.55.2` from `codegraph-0.55.2-x86_64-unknown-linux-musl.tar.gz`
  (checked against the release's `SHA256SUMS` and `gh attestation verify`), sha256
  `6c8f0093905b08d17848dfcc05b6d04f88256bca88a89186a71c052b6165d57c`.
- Command: `CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1 codegraph init <copy of
crates/codegraph-bench/fixtures/mini>`, then `.codegraph/codegraph.db` copied
  here.
- `codegraph.db` sha256:
  `12cfc45a0c157873f75567fcd85d8ef0522eeead31bdfe2043513d9cecf16a0b`.

Do not regenerate it with a newer binary: its value is that an old release
wrote it.
