# Connect a coding agent

This page shows how to give a coding agent or an editor access to CodeGraph's index over MCP, and what the agent can
then ask.

CodeGraph runs as an MCP server. An agent that has it configured asks structural questions with tool calls instead of
searching and reading files one by one, and gets back the relevant source together with the calls between it.

## Write the configuration for you

```sh
codegraph install --yes
```

`install` finds the agents installed on this computer and adds a `codegraph` entry to each one's MCP configuration.
The entry starts `codegraph serve --mcp`. Running it again updates the entry in place, and other MCP servers in the
same file are left alone.

Supported agents and editors: Claude Code, Cursor, Codex CLI, opencode, Hermes Agent, Gemini CLI, Antigravity IDE,
Kiro, Trae, Qoder, Zed, Zuno, VS Code (GitHub Copilot), GitHub Copilot CLI and JetBrains IDEs (GitHub Copilot).

| You want to                                              | Run                                              |
| -------------------------------------------------------- | ------------------------------------------------ |
| choose the agents                                        | `codegraph install --target=claude,cursor --yes` |
| write the project's own config instead of the global one | `codegraph install --target=auto --local`        |
| see the entry without writing anything                   | `codegraph install --print-config cursor`        |
| install and index the current project in one step        | `codegraph install --yes --init`                 |
| remove the entries again                                 | `codegraph uninstall`                            |

Some editors start the server outside the project and do not tell it which project is open. Kiro, Zed and VS Code
read a global entry that cannot name a project, so it can only answer for indexes passed in each call. For those,
`codegraph init --target=<editor>` inside a project writes a project-level entry that names the project, which also
turns on live updates for it. Cursor's global entry names the open folder itself. The
[CLI reference](../../../cli.md#codegraph-install--uninstall--wire-up-ai-agents) lists the file each agent uses.

## Add the agent skill

```sh
codegraph skill install
```

The skill is a short instruction file that tells an agent when to use which CodeGraph tool. `codegraph skill status`
shows which agents have it and whether it is current; `codegraph skill update` refreshes it after an upgrade.

## Configure it by hand

Any MCP client can start the server itself:

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

The server talks MCP over standard input and output. To pin it to one project regardless of where the client starts
it, add `"-p", "/path/to/project"` to `args`.

## Which project the server answers for

Started without `-p`, the server looks for a project in this order:

1. the nearest directory with an index at or above its working directory;
2. if there is none and the working directory is a repository or workspace root, the one indexed project below it, as
   long as there is exactly one;
3. the workspace the client announces when it connects, if that workspace is indexed.

If none of these gives a project, every tool still works, but each call must name the project with `projectPath`. The
server never guesses between several indexed projects. [MCP reference](../../../mcp.md#project-resolution)

## What the agent can ask

| Question                     | MCP tool            | Same thing on the command line   |
| ---------------------------- | ------------------- | -------------------------------- |
| How does this area work?     | `codegraph_explore` | `codegraph explore "<question>"` |
| Show me this symbol or file  | `codegraph_node`    | `codegraph node <name>`          |
| Where is it defined?         | `codegraph_search`  | `codegraph search <name>`        |
| Who calls it?                | `codegraph_callers` | `codegraph callers <name>`       |
| What does it call?           | `codegraph_callees` | `codegraph callees <name>`       |
| What does changing it reach? | `codegraph_impact`  | `codegraph impact <name>`        |
| Is the index current?        | `codegraph_status`  | `codegraph status`               |
| Which files are indexed?     | `codegraph_files`   | `codegraph files`                |
| Are there import cycles?     | `codegraph_check`   | `codegraph check`                |
| Give me the whole graph      | `codegraph_export`  | `codegraph export`               |

An agent sees the first four tools in its tool list; the others can still be called. Set `CODEGRAPH_MCP_TOOLS` to
list more, for example `CODEGRAPH_MCP_TOOLS=explore,node,search,callers,impact`. Every tool is read-only and marked as
such, so agents that honour MCP's hints do not ask for confirmation.

## Over HTTP

Editors that prefer HTTP, and remote development over SSH, can use the streamable-HTTP transport:

```sh
codegraph serve --http            # listens on 127.0.0.1:8111
codegraph serve --http --detach   # the same, in the background
codegraph http list               # running HTTP servers
codegraph http stop 127.0.0.1:8111
```

The HTTP server has no authentication. It listens on the local machine unless you pass another address with
`--http-addr`; keep it there, or put your own access control in front of it.
[MCP reference](../../../mcp.md)

## Keeping answers current

For each indexed project it serves, the server starts or joins one background process that watches the files and
updates the index. Every agent and editor on that project shares it. [Keeping the index current](keeping-current.md)
