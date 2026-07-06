# CodeGraph for Zed

Provides [CodeGraph](https://github.com/sunerpy/codegraph-rust) — a
deterministic tree-sitter + SQLite/FTS5 code knowledge graph — as an
[MCP context server](https://zed.dev/docs/ai/mcp) inside Zed.

CodeGraph answers structural questions about a codebase ("who calls X", "what
does changing X break", "where is X", "how does this area work") in one
sub-millisecond query instead of dozens of grep + file reads. It has no
AI/LLM inside it — pure pre-computed structure for your agent to consume.

## Install

### Preferred — official registry (once published)

Search for **"CodeGraph"** in Zed's extension registry (`zed: extensions` from
the command palette) and click Install. The extension auto-downloads the
CodeGraph binary for your platform on first launch.

> The extension is being submitted to the
> [`zed-industries/extensions`](https://github.com/zed-industries/extensions)
> registry. Once accepted it will be searchable there. Until then, use the
> dev-install path below.

### Dev install (before publication / for local development)

1. Clone this repository.
2. In Zed, run **`zed: install dev extension`** from the command palette.
3. Select the `editors/zed/` directory.

Zed compiles the extension to WebAssembly and registers a `codegraph` context
server. On first launch the extension downloads the latest CodeGraph release
binary for your platform (see below).

## Auto-update and binary cache location

The extension never pins a CodeGraph version. On each launch it:

1. Checks your Zed settings for an explicit `command` override (see below). If
   present, it uses that verbatim and skips the download.
2. Otherwise resolves the **latest** GitHub release of
   `sunerpy/codegraph-rust`, picks the asset matching your platform
   (`x86_64`/`aarch64` × `unknown-linux-musl` / `apple-darwin` /
   `pc-windows-msvc`), downloads and extracts it, then caches the binary at:

```
codegraph-<version>/codegraph        # Linux / macOS
codegraph-<version>/codegraph.exe    # Windows
```

This path is **relative to the extension's working directory** that Zed manages
(inside Zed's extensions data directory). The full on-disk location is:

| Platform | Full path                                                                                        |
| -------- | ------------------------------------------------------------------------------------------------ |
| Linux    | `~/.local/share/zed/extensions/installed/codegraph/codegraph-<version>/codegraph`                |
| macOS    | `~/Library/Application Support/Zed/extensions/installed/codegraph/codegraph-<version>/codegraph` |
| Windows  | `%APPDATA%\Zed\extensions\installed\codegraph\codegraph-<version>\codegraph.exe`                 |

For example, after downloading version `v0.25.0` on Linux the binary lives at:

```
~/.local/share/zed/extensions/installed/codegraph/codegraph-v0.25.0/codegraph
```

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

## Remote development (SSH)

If you use Zed's remote SSH feature (Zed UI on your local machine, code and
`.codegraph/` index on a remote Linux host), codegraph MCP tools will silently
return empty results. The cause: Zed runs `context_servers` on the **local**
machine even for remote SSH projects, so the extension's downloaded binary cannot
reach the remote index. Native remote MCP execution is not yet implemented in Zed
(as of mid-2026).

**Workaround — ssh bridge.** Add this to your project's `.zed/settings.json` on
the local machine. It replaces the extension's command with `ssh`, which proxies
the MCP JSON-RPC stream transparently to codegraph running on the remote host:

```jsonc
{
  "context_servers": {
    "codegraph": {
      "command": "ssh",
      "args": [
        "-T",
        "<your-ssh-host-alias>",
        "cd /abs/path/to/project && /abs/path/to/codegraph serve --mcp --path /abs/path/to/project",
      ],
      "env": {},
    },
  },
}
```

Use an absolute path to the codegraph binary on the remote host (non-login SSH
shells often lack `~/.cargo/bin` on `PATH`). The `-T` flag disables PTY
allocation, which would otherwise corrupt the JSON-RPC byte stream.

**Workaround — HTTP transport (recommended).** Start `codegraph serve --http` on
the remote host (or forward a port) and point Zed at the local port:

```jsonc
{
  "context_servers": {
    "codegraph": {
      "url": "http://localhost:8111/mcp",
    },
  },
}
```

For the full explanation of each approach and the known caveats, see
[`docs/mcp.md` — Zed over SSH](../../docs/mcp.md#zed-over-ssh-remote-development).

## Publishing to the Zed registry

To submit this extension to the official
[`zed-industries/extensions`](https://github.com/zed-industries/extensions)
registry so users can install it via `zed: extensions`, follow these steps:

1. **Bump the version** in `editors/zed/extension.toml` to match the current
   CodeGraph release (e.g. `version = "0.2.0"`).

2. **Fork** [`zed-industries/extensions`](https://github.com/zed-industries/extensions).

3. **Add this repo as a git submodule** under `extensions/codegraph/`:

   ```bash
   git submodule add https://github.com/sunerpy/codegraph-rust extensions/codegraph
   ```

   The registry expects the extension root (containing `extension.toml`) at
   `extensions/codegraph/editors/zed/`, so point the submodule at the repo root
   and Zed's tooling walks for the manifest.

   > Alternatively, if the registry requires the extension root at the submodule
   > root, extract just `editors/zed/` into a dedicated repo first and submodule
   > that instead.

4. **Add an entry** to the top-level `extensions.toml` in the
   `zed-industries/extensions` repo:

   ```toml
   [codegraph]
   submodule = "extensions/codegraph"
   version = "0.2.0"
   ```

   The `id` field (`codegraph`) must match the `id` in `extension.toml`.

5. **Open a pull request** against `zed-industries/extensions`. The Zed team
   reviews and merges it; once merged the extension becomes searchable in Zed's
   built-in registry.

6. **Subsequent releases**: bump `version` in `extension.toml`, tag the
   codegraph-rust repo, then open another PR to `zed-industries/extensions`
   updating the submodule ref and `extensions.toml` version. The CodeGraph
   binary itself auto-updates via the GitHub release mechanism regardless of
   when the extension version is bumped — so a version bump is only needed when
   the extension's WASM logic changes.

## License

MIT — see [`LICENSE`](LICENSE).
