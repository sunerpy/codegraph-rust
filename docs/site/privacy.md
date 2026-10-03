# 数据与网络

本页列出 CodeGraph 读取、写入和连接的全部内容，方便你决定在哪里运行它。

## 读取什么

你为其建立索引的项目中的文件，范围遵循[配置](guide/configuration.md)中说明的规则：被 `.gitignore`、`config.toml` 和默认跳过列表排除的内容从不读取。

## 写入什么

| 位置                                                                                                                     | 内容                                                                                                                                 | 写入者                                                                          |
| ------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------- |
| `<项目>/.codegraph/`                                                                                                     | SQLite 索引、索引设置，以及后台进程的锁、socket 和日志                                                                               | `init`、`index`、`sync` 和后台进程                                              |
| `<项目>/.codegraph/ui/trails/`                                                                                           | 你在查看器中主动保存的 Trail                                                                                                         | 查看器，仅在你保存时                                                            |
| `<项目>/.codegraph/diagnostics/`                                                                                         | 诊断日志                                                                                                                             | `init`、`index` 和 `sync`，仅在使用 `--debug` 时                                |
| 你指定的文件                                                                                                             | 诊断日志                                                                                                                             | `init`、`index` 或 `sync` 的 `--debug-log <文件>`                               |
| 你指定的文件                                                                                                             | JSON 格式的整张图                                                                                                                    | `codegraph export -o <文件>`                                                    |
| Agent 和编辑器的配置文件                                                                                                 | `codegraph` MCP 条目，以及位于 `<!-- CODEGRAPH_START -->` 和 `<!-- CODEGRAPH_END -->` 之间的说明                                     | `codegraph install`                                                             |
| 项目自己的 Agent 配置文件（`.vscode/mcp.json`、`.zed/settings.json` 等）                                                 | 固定指向该项目的 `codegraph` 条目                                                                                                    | `codegraph init --target=<编辑器>`                                              |
| 各 Agent 的 Skill 目录                                                                                                   | CodeGraph Skill                                                                                                                      | `codegraph skill install`                                                       |
| shell 的补全目录                                                                                                         | 补全脚本                                                                                                                             | `codegraph completions <shell> --install`                                       |
| `%USERPROFILE%\Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1`，或 `CODEGRAPH_PS_PROFILE` 指定的文件       | 加载 PowerShell 补全脚本的一行                                                                                                       | `codegraph completions powershell --install`                                    |
| 用户状态目录：`$XDG_STATE_HOME/codegraph/`，否则为 `~/.local/state/codegraph/`；Windows 上为 `%LOCALAPPDATA%\codegraph\` | 每个运行中的 MCP 服务一个小文件，供 `codegraph mcp list` 和 `codegraph http list` 查找；以及每个用 `--detach` 启动的 HTTP 服务的日志 | `serve --mcp` 和 `serve --http`。服务退出后条目会被清除，日志则保留到你删除为止 |
| 系统临时目录                                                                                                             | 后台进程的 socket，仅在 `.codegraph/` 所在的文件系统无法容纳 socket 时                                                               | 后台进程                                                                        |
| `codegraph` 可执行文件                                                                                                   | 新版本                                                                                                                               | `codegraph self-update`                                                         |

在 WSL 的 Windows 驱动器上，项目目录是 `.codegraph-wsl/` 而不是 `.codegraph/`；`CODEGRAPH_DIR` 可以指定其他名称。

按[卸载](guide/install.md#卸载)的步骤可以移除以上全部内容。

## 联网行为

| 操作                                    | 连接到                   |
| --------------------------------------- | ------------------------ |
| 建立索引、同步、所有查询和所有 MCP 工具 | 不联网                   |
| `codegraph self-update`、安装脚本       | GitHub，用于下载发布版本 |

## 监听连接

| 服务                     | 监听位置                                                            | 说明                                                                                                                                                                                                                       |
| ------------------------ | ------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `codegraph serve --mcp`  | 不监听                                                              | 通过标准输入和标准输出与启动它的程序通信。                                                                                                                                                                                 |
| 后台进程                 | `.codegraph/` 中的本地 socket（文件系统无法容纳时改用系统临时目录） | 只有同一台机器上的进程可以连接，供该项目的 MCP 服务使用。                                                                                                                                                                  |
| `codegraph serve --http` | `127.0.0.1:8111`                                                    | `--http-addr` 可以改用其他地址，包括其他机器能访问到的地址。它**没有身份验证**，而且除非 `CODEGRAPH_HTTP_ALLOWED_HOSTS` 列出了允许的主机，否则接受任何 `Host` 请求头。请让它只在本机监听，或在它前面加上你自己的访问控制。 |
| `codegraph ui`           | 仅 `127.0.0.1`                                                      | 拒绝来自其他网站的请求，只打开索引中记录的文件。                                                                                                                                                                           |

索引及其回答都包含你的源码。任何能访问到 CodeGraph 服务的人，都能读取它所索引的代码。
