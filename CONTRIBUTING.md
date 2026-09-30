# Contributing to CodeGraph-Rust

Thank you for improving CodeGraph. The project values deterministic behavior,
small evidence-backed changes, and explicit static-analysis boundaries.

## Before you start

1. Read [`AGENTS.md`](AGENTS.md), especially the hard invariants and proof matrix.
2. Search existing issues and the upstream ledger before duplicating work.
3. Use an isolated branch/worktree; preserve unrelated local work.
4. For large graph-semantic, schema, protocol, or release changes, open an issue
   or draft PR first so the compatibility boundary is explicit.

By participating, you agree to follow the
[Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately as
specified in [`SECURITY.md`](SECURITY.md), not in a public issue.

## Development setup

The repository pins Rust and all shipped dependencies. Install the non-Rust tools
reported by `make help`/`make tools-check`, then enable the versioned hook:

```bash
git clone https://github.com/sunerpy/codegraph-rust.git
cd codegraph-rust
make hooks
make check
```

Do not replace locked commands with unlocked equivalents. `make ci` aliases `make check`; `make pre-ci` adds an archive package/unpack/execute smoke.

## Work on a change

- Start with the narrowest test that demonstrates the behavior.
- Keep extraction, storage, resolution, graph algorithms, and presentation in
  their owning crates.
- Add ambiguity and negative cases. A false edge is often worse than an
  unresolved reference.
- Update the canonical documentation in the same change as public behavior.
- Run `python3 scripts/docs-check.py` after changing Markdown links, headings,
  README structure, schema/language claims, or agent guides.

### Golden and version-sensitive changes

If nodes, edges, unresolved references, file classification, or resolution
meaning changes:

1. identify the affected corpus or add the smallest focused corpus;
2. decide explicitly whether the extraction version must change;
3. regenerate with the recipe in [`docs/equivalence.md`](docs/equivalence.md);
4. review every changed canonical row and schema statement;
5. prove incremental `sync` and clean indexing converge.

Never hand-edit canonical golden JSON to make a test pass. SQLite `.db` files are
inputs to canonicalization and are not themselves byte-reproducible.

Schema migrations must be forward-safe, replay-tested, and reflected in
[`docs/data-model.md`](docs/data-model.md). Schema version and extraction version
are separate decisions.

## Commit and PR conventions

Use English Conventional Commits:

```text
feat(scope): imperative summary
fix(scope): imperative summary
```

`feat` drives a minor release and `fix` a patch release. Breaking changes use
`feat!` and/or a `BREAKING CHANGE:` footer. Keep the subject imperative, omit a
trailing period, and do not add AI/co-author trailers.

A PR should state:

- the problem and observed evidence;
- the chosen behavior and static/runtime boundary;
- compatibility, schema, extraction-version, and golden impact;
- focused and full validation actually run;
- documentation updated;
- remaining risks or intentionally deferred work.

Complete the PR template. Keep unrelated formatting or refactors out of the
diff. Maintainers may ask for a minimal reproduction before accepting a new
heuristic.

## Validation

During iteration, run affected crate/tests. Before handoff, run:

```bash
make check
git diff --check
```

The complete gate includes locked tests, Clippy with warnings denied, release
build, formatting, workflow/shell lint, repository guardrails, and script
fixtures. Coverage is informational; never delete or weaken tests to move a
percentage.

## Releases

Do not edit workspace versions, the release manifest, the scaffold version
placeholder, tags, or published release assets manually. Release Please owns
all four version surfaces. Maintainers publish only through
the draft-until-verified workflow after exact-SHA CI, all platform archives,
checksums, attestations, and archive smoke pass.
