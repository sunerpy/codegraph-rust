# 接入编码 Agent

本页说明如何让编码 Agent 或编辑器通过 MCP 使用 CodeGraph 的索引，以及接入后 Agent 可以问哪些问题。

CodeGraph 以 MCP 服务的形式运行。配置好的 Agent 用工具调用提出结构性问题，不必逐个搜索和读取文件，得到的回答直接包含相关源码以及它们之间的调用关系。

## 自动写入配置

```sh
codegraph install --yes
```

`install` 会找出本机已安装的 Agent，在各自的 MCP 配置中添加一个 `codegraph` 条目，用来启动 `codegraph serve --mcp`。重复运行会原地更新这个条目，同一文件中的其他 MCP 服务保持不变。

支持的 Agent 和编辑器：Claude Code、Cursor、Codex CLI、opencode、Hermes Agent、Gemini CLI、Antigravity IDE、Kiro、Trae、Qoder、Zed、Zuno、VS Code（GitHub Copilot）、GitHub Copilot CLI 和 JetBrains IDE（GitHub Copilot）。

| 需要                               | 运行                                             |
| ---------------------------------- | ------------------------------------------------ |
| 指定 Agent                         | `codegraph install --target=claude,cursor --yes` |
| 写入项目自己的配置，而不是全局配置 | `codegraph install --target=auto --local`        |
| 只查看要写入的条目，不实际写入     | `codegraph install --print-config cursor`        |
| 安装并为当前项目建立索引           | `codegraph install --yes --init`                 |
| 移除这些条目                       | `codegraph uninstall`                            |

有些编辑器在项目之外启动 MCP 服务，也不告诉它打开的是哪个项目。Kiro、Zed 和 VS Code 读取的全局条目无法指定项目，只能回答每次调用中显式指定的索引。对这些编辑器，在项目中运行 `codegraph init --target=<编辑器>`，会写入一个指定该项目的项目级条目，同时为该项目启用实时更新。Cursor 的全局条目会自动指向当前打开的文件夹。[CLI 参考](../../cli.md#codegraph-install--uninstall--wire-up-ai-agents)列出了各 Agent 使用的配置文件。

## 安装 Agent Skill

```sh
codegraph skill install
```

Skill 是一份简短的说明文件，告诉 Agent 在什么情况下使用哪个 CodeGraph 工具。`codegraph skill status` 显示哪些 Agent 已安装以及是否为最新；升级后运行 `codegraph skill update` 即可更新。

## 手动配置

任何 MCP 客户端都可以自行启动这个服务：

```json
{
  "mcpServers": {
    "codegraph": {
      "command": "codegraph",
      "args": ["serve", "--mcp"]
    }
  }
}
```

服务通过标准输入和标准输出通信。如果希望不论客户端在哪里启动服务，都固定使用同一个项目，请在 `args` 中加上 `"-p", "/path/to/project"`。

## 服务为哪个项目作答

不带 `-p` 启动时，服务按以下顺序寻找项目：

1. 工作目录本身或其上层中最近的、已建立索引的目录；
2. 如果没有找到，而工作目录是仓库或工作区的根目录，则使用其下唯一一个已建立索引的项目（必须恰好只有一个）；
3. 客户端连接时声明的工作区，前提是该工作区已建立索引。

如果都没有找到，所有工具仍然可用，但每次调用都必须用 `projectPath` 指定项目。存在多个已建立索引的项目时，服务不会替你猜测。[MCP 参考](../../mcp.md#project-resolution)

## Agent 可以问什么

| 问题                   | MCP 工具            | 对应的命令行                 |
| ---------------------- | ------------------- | ---------------------------- |
| 这块代码是怎么工作的？ | `codegraph_explore` | `codegraph explore "<问题>"` |
| 查看这个符号或文件     | `codegraph_node`    | `codegraph node <名称>`      |
| 它在哪里定义？         | `codegraph_search`  | `codegraph search <名称>`    |
| 谁调用了它？           | `codegraph_callers` | `codegraph callers <名称>`   |
| 它调用了什么？         | `codegraph_callees` | `codegraph callees <名称>`   |
| 改动它会影响哪里？     | `codegraph_impact`  | `codegraph impact <名称>`    |
| 索引是否最新？         | `codegraph_status`  | `codegraph status`           |
| 哪些文件已建立索引？   | `codegraph_files`   | `codegraph files`            |
| 有没有循环导入？       | `codegraph_check`   | `codegraph check`            |
| 导出整张图             | `codegraph_export`  | `codegraph export`           |

Agent 的工具列表中默认显示前四个工具，其余工具仍然可以调用。设置 `CODEGRAPH_MCP_TOOLS` 可以显示更多工具，例如 `CODEGRAPH_MCP_TOOLS=explore,node,search,callers,impact`。所有工具都是只读的，并按 MCP 的约定做了标注，遵循这些标注的 Agent 不会为调用请求确认。

## 通过 HTTP

偏好 HTTP 的编辑器，以及通过 SSH 进行的远程开发，可以使用 streamable HTTP 传输：

```sh
codegraph serve --http            # 监听 127.0.0.1:8111
codegraph serve --http --detach   # 同上，在后台运行
codegraph http list               # 正在运行的 HTTP 服务
codegraph http stop 127.0.0.1:8111
```

HTTP 服务没有身份验证。除非用 `--http-addr` 指定其他地址，它只监听本机；请保持这样的设置，或在它前面加上你自己的访问控制。[MCP 参考](../../mcp.md)

## 让回答保持最新

服务为它负责的每个已建立索引的项目启动或加入一个后台进程，由它监听文件并更新索引。同一项目上的所有 Agent 和编辑器共用这个进程。详见[保持索引最新](keeping-current.md)。
