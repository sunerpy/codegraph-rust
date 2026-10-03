# Contributing

This page is for people who want to build CodeGraph from source, change it, or improve this site.

CodeGraph is open source under the MIT licence. The code, the issue tracker and the pull requests are on
[GitHub](https://github.com/sunerpy/codegraph-rust). It is a Rust port of the TypeScript project
[colbymchenry/codegraph](https://github.com/colbymchenry/codegraph), developed independently and kept in step with
it; the [upstream ledger](https://github.com/sunerpy/codegraph-rust/blob/main/docs/upstream-sync/UPSTREAM.md)
records what has been ported.

## Build and test

```sh
git clone https://github.com/sunerpy/codegraph-rust
cd codegraph-rust
cargo build --release      # target/release/codegraph
make check                 # formatting, lints, tests, guardrails: the same gate as CI
make pre-ci                # make check, plus the viewer frontend and a release-archive smoke test
```

The Rust version is pinned in `rust-toolchain.toml`; a C compiler is needed for SQLite and the grammars. The viewer
frontend in `ui/` needs Node and npm only to rebuild it; the built bundle is committed, so `cargo build` never runs
Node.

## How the code is organised

The workspace is a set of crates with one direction of dependency, from shared types up to the command line:

| Crate               | Owns                                                        |
| ------------------- | ----------------------------------------------------------- |
| `codegraph-core`    | shared types, configuration, node ids, logging              |
| `codegraph-extract` | language detection and extraction with tree-sitter          |
| `codegraph-store`   | the SQLite schema, migrations, full-text search and queries |
| `codegraph-resolve` | import and name resolution, and framework resolvers         |
| `codegraph-graph`   | traversal, impact, search ranking                           |
| `codegraph-mcp`     | the MCP server and its tools                                |
| `codegraph-watch`   | incremental sync and file watching                          |
| `codegraph-daemon`  | the background process, its locks and registries            |
| `codegraph-ui`      | the browser viewer's server and its embedded frontend       |
| `codegraph-cli`     | the `codegraph` command, the agent installer                |
| `codegraph-bench`   | the equivalence oracle and benchmarks; not shipped          |

The [architecture](../../architecture.md) and [data model](../../data-model.md) pages describe the flow from a file to an
answer and the tables in between.

## What every change keeps

- **Deterministic output.** The same source and configuration produce the same index, and `sync` converges with a
  full rebuild.
- **Byte-stable goldens.** Extraction output is compared byte for byte against reference artifacts; the
  [equivalence page](../../equivalence.md) explains how to regenerate them for an intended change.
- **No model.** No AI, embedding or vector dependency may enter the build; a guardrail script enforces it.
- **Fail closed.** An ambiguous or unsafe answer stays unresolved or is refused.

The [contributor contract](https://github.com/sunerpy/codegraph-rust/blob/main/AGENTS.md) and
[CONTRIBUTING.md](https://github.com/sunerpy/codegraph-rust/blob/main/CONTRIBUTING.md) have the full rules,
including commit messages and the release process.

## Changing this site

The words and screenshots of this site live in the repository under `docs/site/`, next to the code they describe, so
a pull request that changes what you see also updates the page. The site itself (theme, components and publishing)
lives in [sunerpy/firlab](https://github.com/sunerpy/firlab). `docs/site/README.md` explains how to preview a change,
the writing rules and how the screenshots are taken.
