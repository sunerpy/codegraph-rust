# CodeGraph for Zed

Provides [CodeGraph](https://github.com/sunerpy/codegraph-rust) — a
deterministic tree-sitter + SQLite/FTS5 code knowledge graph — as an
[MCP context server](https://zed.dev/docs/ai/mcp) inside Zed.

CodeGraph answers structural questions about a codebase ("who calls X", "what
does changing X break", "where is X", "how does this area work") in one
sub-millisecond query instead of dozens of grep + file reads. It has no
AI/LLM inside it — pure pre-computed structure for your agent to consume.

## Install (dev extension)

1. Clone this repository.
2. In Zed, run **`zed: install dev extension`** from the command palette.
3. Select the `editors/zed/` directory.

Zed compiles the extension to WebAssembly and registers a `codegraph` context
server. On first launch the extension downloads the latest CodeGraph release
binary for your platform (see below).

## Auto-update (tracks the latest CodeGraph release)

The extension never pins a CodeGraph version. On each launch it:

1. Checks your Zed settings for an explicit `command` override (see below). If
   present, it uses that verbatim and skips the download.
2. Otherwise resolves the **latest** GitHub release of
   `sunerpy/codegraph-rust`, picks the asset matching your platform
   (`x86_64`/`aarch64` × `unknown-linux-musl` / `apple-darwin` /
   `pc-windows-msvc`), downloads and extracts it, and caches it under a
   version-stamped path (`codegraph-<version>/codegraph`).

Because the cache is keyed on the release version, when the CodeGraph CLI ships
a new release the extension picks up the new binary automatically on the next
launch — **no extension re-publish or manual update required**. If the GitHub
API is unreachable (offline), the extension falls back to the newest binary it
has already cached.

## Use your own CodeGraph binary or pin a project path

If you already installed `codegraph` via the CLI (the one-line installer or
`cargo install`), or you want a specific project's server to pin a `--path`,
add a `command` override in your project's `.zed/settings.json`. The extension
honors it verbatim and skips the download entirely:

```jsonc
{
  "context_servers": {
    "codegraph": {
      "command": {
        "path": "codegraph",
        "args": ["serve", "--mcp", "--path", "/abs/path/to/project"],
        "env": {},
      },
    },
  },
}
```

- `path` — the `codegraph` executable (on PATH or an absolute path).
- `args` — pass `--path <project>` to pin the server to one project (Zed's
  global config cannot inject a per-project path, so use the project-level
  `.zed/settings.json` for this). Omit `--path` to serve read-only off whatever
  index the working directory resolves to.

You can also let `codegraph` write this entry for you:

```bash
cd /your/project
codegraph init --target=zed     # writes .zed/settings.json with an absolute --path
```

## Publishing

Publishing this extension to the public
[`zed-industries/extensions`](https://github.com/zed-industries/extensions)
registry is a separate, later step and is not done here.

## License

MIT — see [`LICENSE`](LICENSE).
