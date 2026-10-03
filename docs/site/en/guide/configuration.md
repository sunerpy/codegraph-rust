# Configuration

This page lists the settings that change what CodeGraph indexes and how it ranks results.

CodeGraph works without configuration. Settings live in two optional files inside the project's `.codegraph/`
directory, and a few environment variables tune the background process.

## `.codegraph/config.toml`

```toml
[app]
name = "my-project"

[indexing]
exclude = ["static/", "docs/generated/"]
deprioritize = ["vendor/**"]
```

When the file exists it must have an `[app]` table with a `name`; without it every command stops with a
`missing field` error. Every other setting is optional:

| Setting                  | Default                                                                                      | What it does                                                                                                                                                                                                         |
| ------------------------ | -------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `app.name`               | —                                                                                            | A name for the project.                                                                                                                                                                                              |
| `app.log_level`          | `info`                                                                                       | How much CodeGraph logs to standard error.                                                                                                                                                                           |
| `indexing.exclude`       | none                                                                                         | Paths left out of the index, written like `.gitignore` rules (`static/`, `gen*`).                                                                                                                                    |
| `indexing.include`       | none                                                                                         | Paths indexed even though `.gitignore` leaves them out, for source a second version-control system keeps out of Git. `exclude` still wins, and dependency directories such as `node_modules` are never brought back. |
| `indexing.ignore_dirs`   | dependency, build and cache directories such as `node_modules`, `target`, `dist` and `.venv` | Directory names skipped at any depth. A list here **replaces** the default list, so repeat the defaults you want to keep.                                                                                            |
| `indexing.ignore_paths`  | Android `res/` resource directories                                                          | Path patterns skipped by default, in `.gitignore` form.                                                                                                                                                              |
| `indexing.max_file_size` | `1048576` (1 MiB)                                                                            | Larger files are recorded without being parsed.                                                                                                                                                                      |
| `indexing.deprioritize`  | none                                                                                         | Paths that stay indexed but rank below your own code in `search` and `explore`.                                                                                                                                      |
| `watch.enabled`          | `true`                                                                                       | Whether the background process watches the files.                                                                                                                                                                    |
| `watch.debounce_ms`      | `2000`                                                                                       | How long the watcher waits for a burst of changes to settle.                                                                                                                                                         |

The project's `.gitignore` applies too: what Git ignores, CodeGraph does not index. A change to `config.toml` or to
the root `.gitignore` takes effect without a restart; run `codegraph sync` if no background process is running.

## `.codegraph/codegraph.json`

Map file extensions CodeGraph does not know to a language it parses:

```json
{
  "extensions": {
    ".blade": "php",
    ".x": "lua"
  }
}
```

Keys are matched without the leading dot and case-insensitively. A language name CodeGraph does not know is skipped,
and a malformed file is ignored with an error in the log rather than stopping indexing.
[CLI reference](../../../cli.md#custom-extension-mapping-codegraphcodegraphjson)

## Environment variables

| Variable                           | Use                                                                        |
| ---------------------------------- | -------------------------------------------------------------------------- |
| `CODEGRAPH_NO_DAEMON=1`            | Run in the foreground and start no background process, for CI and scripts. |
| `CODEGRAPH_NO_WATCH=1`             | Keep the background process but stop it watching files.                    |
| `CODEGRAPH_WATCH_DEBOUNCE_MS`      | The watcher's pause, in milliseconds.                                      |
| `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` | How long the background process stays after its last client leaves.        |
| `CODEGRAPH_MCP_TOOLS`              | Which MCP tools an agent sees in its tool list.                            |
| `CODEGRAPH_DIR`                    | Use another directory name inside the project instead of `.codegraph`.     |
| `CODEGRAPH_UI=1`                   | Turn on the browser viewer (preview).                                      |

The [CLI reference](../../../cli.md#environment-variable-reference) has the complete list with ranges and defaults.
