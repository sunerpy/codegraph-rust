# Data and network

This page lists everything CodeGraph reads, writes and connects to, so you can decide where to run it.

## What it reads

The files of the project you index, within the rules described in [Configuration](guide/configuration.md): what
`.gitignore`, `config.toml` and the default skip lists leave out is never read.

## What it writes

| Where                                                                                                                             | What                                                                                                                                                      | Written by                                                                                                            |
| --------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `<project>/.codegraph/`                                                                                                           | The SQLite index, its settings and the background process's lock, socket and log                                                                          | `init`, `index`, `sync`, and the background process                                                                   |
| `<project>/.codegraph/ui/trails/`                                                                                                 | Trails you chose to save in the viewer                                                                                                                    | the viewer, only when you save one                                                                                    |
| `<project>/.codegraph/diagnostics/`                                                                                               | Diagnostic logs                                                                                                                                           | `init`, `index` and `sync`, only with `--debug`                                                                       |
| The file you name                                                                                                                 | A diagnostic log                                                                                                                                          | `--debug-log <FILE>` on `init`, `index` or `sync`                                                                     |
| The file you name                                                                                                                 | The whole graph as JSON                                                                                                                                   | `codegraph export -o <FILE>`                                                                                          |
| Your agents' and editors' config files                                                                                            | The `codegraph` MCP entry, and instruction blocks between `<!-- CODEGRAPH_START -->` and `<!-- CODEGRAPH_END -->`                                         | `codegraph install`                                                                                                   |
| A project's own agent config files (`.vscode/mcp.json`, `.zed/settings.json` and so on)                                           | A `codegraph` entry pinned to that project                                                                                                                | `codegraph init --target=<editor>`                                                                                    |
| Your agents' skill directories                                                                                                    | The CodeGraph skill                                                                                                                                       | `codegraph skill install`                                                                                             |
| Your shell's completion directory                                                                                                 | A completion script                                                                                                                                       | `codegraph completions <shell> --install`                                                                             |
| `%USERPROFILE%\Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1`, or the file in `CODEGRAPH_PS_PROFILE`               | One line that loads the PowerShell completion script                                                                                                      | `codegraph completions powershell --install`                                                                          |
| Your user state directory: `$XDG_STATE_HOME/codegraph/`, else `~/.local/state/codegraph/`; `%LOCALAPPDATA%\codegraph\` on Windows | One small file per running MCP server, so `codegraph mcp list` and `codegraph http list` can find them, and a log per HTTP server started with `--detach` | `serve --mcp` and `serve --http`. Entries are cleared once the server has exited; the logs stay until you delete them |
| The system temp directory                                                                                                         | The background process's socket, only on filesystems that cannot hold one inside `.codegraph/`                                                            | the background process                                                                                                |
| The `codegraph` executable                                                                                                        | The new version                                                                                                                                           | `codegraph self-update`                                                                                               |

On a WSL Windows drive the project directory is `.codegraph-wsl/` instead of `.codegraph/`, and `CODEGRAPH_DIR`
chooses another name.

[Uninstall](guide/install.md#uninstall) removes all of it.

## What goes online

| Action                                         | Connects to                   |
| ---------------------------------------------- | ----------------------------- |
| Indexing, sync, every query and every MCP tool | nothing                       |
| `codegraph self-update`, the install scripts   | GitHub, to download a release |

## What listens for connections

| Server                   | Listens on                                                                                          | Notes                                                                                                                                                                                                                                                                                       |
| ------------------------ | --------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `codegraph serve --mcp`  | nothing                                                                                             | It talks over standard input and output with the program that started it.                                                                                                                                                                                                                   |
| The background process   | a local socket in `.codegraph/` (or the system temp directory where the filesystem cannot hold one) | Only processes on the same machine can connect; the project's MCP servers use it.                                                                                                                                                                                                           |
| `codegraph serve --http` | `127.0.0.1:8111`                                                                                    | `--http-addr` can choose another address, including one other machines can reach. It has **no authentication**, and it accepts any `Host` header unless `CODEGRAPH_HTTP_ALLOWED_HOSTS` lists the allowed ones. Keep it on the local machine, or put your own access control in front of it. |
| `codegraph ui`           | `127.0.0.1` only                                                                                    | It refuses requests from other sites and opens only files the index names.                                                                                                                                                                                                                  |

The index and its answers contain your source code. Anyone who can reach a CodeGraph server can read the code it
indexes.
