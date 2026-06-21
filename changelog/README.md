# Changelog

This `changelog/` directory is the single source of truth for the project's changelog. Per-major-version changelog files live here, one markdown file per major series:

- `CHANGELOG-v0.x.md` — the `0.x` series
- `CHANGELOG-v1.x.md` — the `1.x` series
- …

**How updates work:**

- The active changelog file (e.g., `CHANGELOG-v0.x.md`) is maintained automatically by release-please in its release PR. The [`release-please-config.json`](../release-please-config.json) sets `"changelog-path": "changelog/CHANGELOG-v0.x.md"`. When a new major series begins, a new `CHANGELOG-vN.x.md` is created and `changelog-path` is bumped.
- GitHub Release notes (shown on the Releases page for each tag) are rendered separately by [git-cliff](https://git-cliff.org) (config: [`cliff.toml`](../cliff.toml)) in the release workflow; that is distinct from the repo's changelog file.

**Do not hand-edit:** These files are auto-generated and excluded from `oxfmt` (see `.oxfmtignore`).
