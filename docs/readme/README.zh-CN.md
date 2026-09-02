# CodeGraph-Rust — 中文说明

[![CI](https://github.com/sunerpy/codegraph-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/sunerpy/codegraph-rust/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/sunerpy/codegraph-rust/branch/main/graph/badge.svg)](https://codecov.io/gh/sunerpy/codegraph-rust)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](#许可证)

> 一个确定性的代码知识图谱（code knowledge graph）：基于 tree-sitter 解析、以
> SQLite/FTS5 落库，为 AI 编码代理与开发者提供可遍历的符号/调用/依赖关系。

> English version: [`../../README.md`](../../README.md)

CodeGraph 读取代码库，用 tree-sitter 抽取符号及其关系，落到每个项目独立的
SQLite 数据库（含 FTS5 检索索引），并通过 CLI 与 MCP stdio 服务器对外暴露。
二进制内部无任何 AI/LLM——输出确定性、字节稳定。

---

## 目录

- [快速上手](#快速上手)
- [安装](#安装)
- [MCP 快速注册](#mcp-快速注册)
- [安装 Agent 技能](#安装-agent-技能codegraph-skill)
- [在 IDE 中使用 CodeGraph](#在-ide-中使用-codegraph)
- [CodeGraph for Zed（编辑器扩展）](#codegraph-for-zed编辑器扩展)
- [结合 LLM 使用](#结合-llm-使用)
- [守护进程、文件监听与配置](#守护进程文件监听与配置)
- [CLI 子命令](#cli-子命令)
- [Shell 补全（Tab Completion）](#shell-补全tab-completion)
- [CodeGraph 的能力范围](#codegraph-的能力范围)
- [支持的语言](#支持的语言)
- [文档](#文档)
- [许可证](#许可证)

---

## 快速上手

**安装（Linux / macOS）：**

```sh
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
```

**安装（Windows，PowerShell）：**

```powershell
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

**索引项目并查询：**

```bash
codegraph init  /path/to/project                   # 创建 .codegraph/ 并执行首次索引
codegraph search "<symbol>" -p /path/to/project    # 全文符号检索（`query` 为兼容别名）
codegraph serve --mcp --path /path/to/project      # 为 AI 代理启动 MCP 服务器（--path 可选，默认 cwd）
codegraph install --yes --init                     # 接线已检测代理并初始化 cwd
```

项目路径的参数形式按命令类别区分：生命周期命令使用可选位置路径
（如 `codegraph status .`），代码查询命令则使用 `-p/--path`
（如 `codegraph search "<symbol>" -p .`）。如果参数被拒绝，应先运行
`codegraph <command> --help`，不要直接退回全文检索。

---

## 安装

CLI 包名为 **`codegraph-rs`**，安装后的二进制命令名为 **`codegraph`**。SQLite
内联编译（无需系统 SQLite）。不发布到 crates.io。

<details>
<summary><b>🤖 给 AI 代理 / LLM —— 请先读这里</b></summary>

你是 AI 编码代理。CodeGraph 是一个确定性的代码知识图谱（tree-sitter +
SQLite/FTS5），用**一次亚毫秒查询**回答代码库的结构性问题——"谁调用了 X""改 X 会
影响什么""X 在哪""这块怎么工作"——**而不是几十次 grep + 读文件**。它返回逐字源码
加调用者/被调用者/影响图，让你用**远更少的 token 和往返**拿到更准的上下文。它内部
无 AI/LLM——纯预计算结构，供**你**消费。

**安装 + 索引项目** —— 优先用一键安装脚本（直接下载预编译二进制，无需 Rust
工具链、无需编译等待）：

```bash
# Linux / macOS —— 一键安装（推荐）
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
# Windows（PowerShell 5.1+）
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex

# 回退方案 —— 从源码构建（仅当你已有 Rust 工具链时）
cargo install --git https://github.com/sunerpy/codegraph-rust codegraph-rs   # 二进制：`codegraph`

codegraph status /path/to/project --json # 先检查索引健康状态
codegraph init /path/to/project          # 创建首个可用索引
codegraph sync /path/to/project          # 普通新增、修改、删除文件
codegraph index /path/to/project         # 有意执行完整重建
codegraph index --force /path/to/project # 仅在诊断明确要求时使用
```

**作为 MCP 服务器使用（推荐给代理）**，它通过 stdio 走 MCP：

```bash
codegraph serve --mcp                          # 默认使用 cwd（推荐：用 codegraph install 自动注册）
codegraph serve --mcp --path /path/to/project  # 可选：固定到指定项目
```

自动注册进你的代理配置（Claude Code / Cursor / Codex CLI / opencode / Hermes /
Gemini CLI / Antigravity / Kiro / Trae / Qoder / Zed / Zuno / VS Code /
Copilot CLI / JetBrains）：

```bash
codegraph install --yes              # 检测已安装的代理并接线
codegraph install --yes --init       # 无人值守的一次性安装 + cwd 建索引
```

**可调用的 MCP 工具**（对已索引源码优先用这些而非 grep/read）：

| 工具                                      | 用途                                                                               |
| ----------------------------------------- | ---------------------------------------------------------------------------------- |
| `codegraph_explore`                       | 首选——"X 怎么工作"、架构、某条流程、概览一块区域。一次返回相关符号源码按文件分组。 |
| `codegraph_search`                        | 按名字定位符号（kind + file:line + 签名）                                          |
| `codegraph_node`                          | 读符号/文件的逐字源码 + 调用者/被调用者轨迹（更聪明的 `Read`）                     |
| `codegraph_callers` / `codegraph_callees` | 谁调用它 / 它调用了什么                                                            |
| `codegraph_impact`                        | 改某符号的影响半径（传递闭包）                                                     |
| `codegraph_files` / `codegraph_status`    | 列目录 / 查索引就绪状态                                                            |

**经验法则**：读文件**之前**先用 `codegraph_explore`；信任它的结果（完整 AST 解析，
别用 grep 复核）；重构影响半径用 `codegraph_impact` 而非手工遍历调用者。索引比文件
写入滞后约 1 秒；工具响应会标注过期文件。问题若明确提到源码路径，就把路径直接放进
explore 查询：显式路径会被标准化并固定在模糊结果之前，无法解析的路径会明确报告。

</details>

### 一键安装脚本

最快的方式——脚本自动检测平台、下载对应二进制并放到 PATH：

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh

# Windows（PowerShell 5.1+）
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

设置 `CODEGRAPH_VERSION=v0.3.0` 可固定安装指定版本而非最新版。

### 预编译二进制

从 [Releases](https://github.com/sunerpy/codegraph-rust/releases) 页面下载对应平台的
压缩包，解压后把 `codegraph` 放到 `PATH`。产物命名为
`codegraph-<version>-<target>.<ext>`：

| 平台    | 架构                    | Target                       | 格式    |
| ------- | ----------------------- | ---------------------------- | ------- |
| Linux   | x86_64（静态 musl）     | `x86_64-unknown-linux-musl`  | .tar.gz |
| Linux   | aarch64（静态 musl）    | `aarch64-unknown-linux-musl` | .tar.gz |
| macOS   | x86_64                  | `x86_64-apple-darwin`        | .tar.gz |
| macOS   | aarch64 (Apple Silicon) | `aarch64-apple-darwin`       | .tar.gz |
| Windows | x86_64                  | `x86_64-pc-windows-msvc`     | .zip    |
| Windows | aarch64（ARM64）        | `aarch64-pc-windows-msvc`    | .zip    |

Linux 版本静态链接 musl，可在任意发行版运行，无 glibc/SQLite 系统依赖。

### 用 cargo 从 git 安装

```bash
cargo install --git https://github.com/sunerpy/codegraph-rust codegraph-rs
```

完整源码构建（optimized 二进制 + 开发者目标），见
[`../architecture.md`](../architecture.md) 或运行 `make help`。

---

## MCP 快速注册

添加到你的代理 MCP 配置文件，或运行 `codegraph install --yes` 自动写入：

```jsonc
{
  "mcpServers": {
    "codegraph": {
      "command": "codegraph",
      "args": ["serve", "--mcp"],
    },
  },
}
```

**默认（不带 `-p`）：** 一份配置即可覆盖所有项目。stdio 服务先向上查找索引；
若启动目录本身是 workspace 根（含 `.git` 或 workspace manifest），还会在确定性
边界内自动采用唯一一个已索引子项目。多个候选绝不猜测，而会在 stderr/工具提示中
按序列出；此时每次调用传 `projectPath`，或用 `--path` 固定项目。MCP 握手中的
`rootUri`/`workspaceFolders`/`roots` 仍可用于采用项目。**可选 `-p <path>` /
`--path <path>`：**
固定指向单个项目（例如
`"args": ["serve", "--mcp", "-p", "/abs/path/to/project"]`）。

支持的代理：Claude Code、Cursor、Codex CLI、opencode、Hermes Agent、
Gemini CLI、Antigravity IDE、Kiro、Trae、Qoder、Zed、Zuno、VS Code、
GitHub Copilot CLI、JetBrains IDE。

```bash
codegraph install --yes                          # 自动检测已安装代理
codegraph install --yes --init                   # 接线代理后初始化 cwd
codegraph install --target=claude,cursor --yes   # 显式指定
codegraph install --target=auto --local          # 项目级配置
codegraph install --target=codex --local --yes   # .codex/config.toml + AGENTS.md + 本地 Skill
```

Codex 的项目级配置只有在仓库被 Codex 标记为可信后才会启用。`install` 默认绝不
创建索引；只有显式 `--init` 才会在安装成功后初始化，`--print-config` 始终提前返回，
既不写配置也不建索引。

完整 MCP 工具参考：[`../mcp.md`](../mcp.md)。

---

## 安装 Agent 技能（`codegraph skill`）

除了写入 MCP 服务器配置，CodeGraph 还可以把一个 `SKILL.md` 直接安装到每个代理的
技能目录中。该技能教会代理如何将 CodeGraph 用于代码研究与项目初始化：在 grep/read
之前优先调用 `codegraph_explore`，对已索引的源码用 `codegraph_node` 代替直接读文件，
先用 `codegraph status` 判断索引状态，在没有可用索引时运行 `codegraph init`，
普通手动追赶则使用 `codegraph sync`。

```bash
codegraph skill install --yes                         # 安装到所有已检测到的代理（全局）
codegraph skill install --target=claude,cursor --yes  # 显式指定代理列表
codegraph skill install --target=auto --local         # 写入项目级技能目录
codegraph skill update                                # 显示版本/+/-摘要后刷新
codegraph skill update --diff                         # 同时显示统一内容差异
codegraph skill update --dry-run --diff               # 仅预览，不写文件
codegraph skill update --force                        # 强制覆盖，即使已被本地修改
codegraph skill uninstall --target=claude --yes       # 从单个代理中移除
codegraph skill status                                # 查看所有已检测代理的安装状态
```

15 个安装目标中有 11 个具备技能目录（Claude Code、Cursor、Codex CLI、opencode、
Hermes Agent、Gemini CLI、Antigravity IDE、Kiro、Trae、Qoder、Zuno）。Zuno 复用标准
`~/.agents/skills` / `.agents/skills`，因此与 Codex 同时选择时仍然幂等。默认位置为
`--global`；传入 `--local` 可写入项目树。Hermes 仅支持全局安装。Zed 与三个 GitHub
Copilot 目标仅支持 MCP 配置，无技能目录。

**更新语义。** `skill update` 用 git blob SHA-1 对比已安装文件与内嵌版本的内容哈希。
未被修改的文件会自动刷新；经过手动编辑的文件会被跳过并提示"locally modified"（可传
`--force` 强制覆盖）。紧邻 `SKILL.md` 的附属文件 `.codegraph-skill.json` 记录了安装
时的哈希值，更新检查据此区分"已过时"与"本地修改"两种状态。写入前，
`skill update` 会显示已安装版本到内嵌版本的迁移，以及确定性的新增/删除行数；
`--diff` 输出统一差异，`--dry-run` 输出相同预览但不修改文件。`skill status`
也会显示版本来源，例如 `outdated (0.40.1 -> 0.47.0)`。

对于由安装器维护指令块的代理，`skill update` 还会同步刷新 marker 之间的内容，
并保留 marker 外的全部用户规则。Zuno 的全局路径是
`$XDG_CONFIG_HOME/zuno/AGENTS.md`（通常为 `~/.config/zuno/AGENTS.md`），项目级路径
是仓库根 `AGENTS.md`。例如：

```bash
codegraph skill update --target=zuno --global
```

`--dry-run` 同样只预览 AGENTS.md 的变化，不会写入。

完整参考（含各代理技能路径）：[`../cli.md`](../cli.md)。

---

## 在 IDE 中使用 CodeGraph

`codegraph install` 为每个支持的代理/IDE 写入 MCP 服务器配置。索引能否保持实时更新，
取决于 IDE 是否支持 `${workspaceFolder}` 变量替换。

- **Cursor / Trae** — 全局配置使用 `--path ${workspaceFolder}`，一份配置自动跟随每个项目窗口，保存即更新。
- **Kiro / Qoder** — 全局条目写入裸 `serve --mcp`（无 `--path`），工具对现有索引只读访问。在各项目内运行 `codegraph init --target=kiro` 可获得实时监听。
- **Zed** — 全局 `~/.config/zed/settings.json`（Linux/macOS）或 `%APPDATA%\Zed\settings.json`（Windows）写入裸条目。在项目内运行 `codegraph init --target=zed` 写入带绝对 `--path` 的 `.zed/settings.json`——这是 Zed 获得项目级实时索引的唯一方式。
- **Zuno** — 全局配置位于 `$XDG_CONFIG_HOME/zuno/zuno.json[c]`（通常是 `~/.config/zuno/`），项目配置位于 `.zuno/zuno.json[c]`。安装器写入 `mcp.codegraph`，并自动迁移旧的 `mcp.codegraph-mcp-server` 键，同时保留 JSONC 注释和其他 MCP 项。
- **Codex CLI** — 全局安装保持 `~/.codex/config.toml` 与 `~/.codex/AGENTS.md`；项目级安装写入 `.codex/config.toml`、仓库根 `AGENTS.md` 和 `.agents/skills/codegraph`。需在 Codex 中信任该仓库才会启用本地层。
- **Claude Code、Cursor、opencode、Hermes、Gemini CLI、Antigravity** — 使用各自原生的 MCP 配置结构，守护进程可到达项目时获得实时监听。

**在 Kiro、Qoder 或 Zed 中获得实时自动更新。** 在每个项目中运行一次：

```bash
cd /your/project
codegraph init --target=kiro    # 或 --target=qoder
codegraph init --target=zed     # 写入带绝对 --path 的 .zed/settings.json
```

> **Zed 远程开发（SSH）。** Zed 在本地客户端运行 MCP `context_servers`，而非远程主机。
> 若在远程 SSH 会话中工具返回空结果，请使用 `ssh` 桥接命令或 HTTP 传输。安装器已在
> `settings.json` 中写入两种远程备选（`//` 注释，取消注释即可）。详见
> [`docs/mcp.md` — Zed over SSH](../mcp.md#zed-over-ssh-remote-development)。

完整的各 IDE 配置细节与 Kiro/Qoder/Zed HTTP 备选：[`../mcp.md`](../mcp.md)。

---

## CodeGraph for Zed（编辑器扩展）

项目下的 [`editors/zed/`](../../editors/zed/) 目录包含一个独立的 Zed 扩展，将
CodeGraph 注册为 Zed 的 `context_servers` 上下文服务器，并自动下载适合当前平台的
二进制——无需单独安装步骤。

### 安装

**推荐——官方市场（发布后）：**

在 Zed 扩展市场（命令面板执行 `zed: extensions`）搜索 **"CodeGraph"** 并点击安装。
扩展会在首次启动时自动为当前平台下载 CodeGraph 二进制。

> 本扩展正在提交至
> [`zed-industries/extensions`](https://github.com/zed-industries/extensions)
> 注册表，审核通过后即可在市场搜到。在此之前请使用下方的开发者模式安装。

**开发者模式安装（发布前 / 本地开发）：**

1. 克隆本仓库。
2. 在 Zed 中打开命令面板，执行 **`zed: install dev extension`**。
3. 选择 `editors/zed/` 目录。

Zed 会将扩展编译为 WebAssembly 并注册 `codegraph` 上下文服务器。首次启动时，
扩展会自动下载当前平台对应的最新 CodeGraph 发布版本二进制。

### 自动更新与二进制缓存位置

扩展不固定 CodeGraph 版本。每次启动时，它会解析 `sunerpy/codegraph-rust` 在
GitHub 上的**最新**发布版本，选取匹配当前平台的资产文件，下载并解压，缓存二进制至：

```
codegraph-<version>/codegraph        # Linux / macOS
codegraph-<version>/codegraph.exe    # Windows
```

该路径**相对于 Zed 为本扩展管理的工作目录**（位于 Zed 扩展数据目录内，通常为：
Linux `~/.local/share/zed/extensions/installed/codegraph/`，
macOS `~/Library/Application Support/Zed/extensions/installed/codegraph/`，
Windows `%APPDATA%\Zed\extensions\installed\codegraph\`）。

例如，在 Linux 上下载 `v0.25.0` 后，二进制位于：

```
~/.local/share/zed/extensions/installed/codegraph/codegraph-v0.25.0/codegraph
```

当 CodeGraph CLI 发布新版本时，扩展会在下次启动时自动获取新二进制——**无需重新发布
扩展或手动更新**。GitHub API 不可达时，扩展会回退到本地已缓存的最新版本。

### 使用自己的二进制覆盖

若你已通过 CLI 安装了 `codegraph`，或想为特定项目固定路径，可在项目的
`.zed/settings.json` 中添加 `command` 覆盖项。扩展会直接使用该命令并跳过下载：

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

也可以让安装器自动写入：

```bash
cd /your/project
codegraph init --target=zed     # 写入带绝对 --path 的 .zed/settings.json
```

详见 [`editors/zed/README.md`](../../editors/zed/README.md) 及
[`../mcp.md`](../mcp.md#zed----context_servers-config) 中的 Zed `context_servers`
配置说明。

---

## 结合 LLM 使用

codegraph 本身**不内置 LLM**，但天生为"喂给 LLM"而设计。推荐分工：
codegraph 出确定性结构事实（调用图、影响半径、中心性），毫秒级；你的 LLM
只对已定位的小上下文做语义分析。

两种模式：**MCP**（代理直接调用 codegraph 工具）或**后端编排**（你的服务调
`export`/`explore` 组装 prompt 后喂 LLM）。可运行范例：

```bash
python examples/llm_orchestration.py --repo . --query "how does indexing work"
```

详见 [`../../examples/llm_orchestration.py`](../../examples/llm_orchestration.py)。
两种模式均不触碰 no-AI guardrail——guardrail 只禁"codegraph 二进制内部塞 LLM 库"。

---

## 守护进程、文件监听与配置

在已索引的项目中运行 `codegraph serve --mcp` 时，CodeGraph 会自动在后台启动一个
**共享的、脱离终端的守护进程**。同一项目的多个 MCP 客户端通过 Unix socket
（`.codegraph/daemon.sock`）共用该 daemon，所有客户端断开且空闲超时后自动退出。

常用操作：

```bash
codegraph unlock [path]   # 清除崩溃后残留的失效锁
CODEGRAPH_NO_DAEMON=1     # 强制前台模式（CI 场景适用）
```

| 变量                          | 默认值 | 说明                                 |
| ----------------------------- | ------ | ------------------------------------ |
| `CODEGRAPH_NO_DAEMON`         | —      | 强制前台直连模式，永不启动守护进程   |
| `CODEGRAPH_WATCH_DEBOUNCE_MS` | `2000` | 文件变动触发重建前的去抖延迟（毫秒） |
| `CODEGRAPH_NO_WATCH`          | —      | 关闭实时文件监听                     |
| `CODEGRAPH_FORCE_WATCH`       | —      | 覆盖 WSL2 `/mnt/` 自动禁用           |

HTTP MCP 服务（`serve --http`，默认绑定 `127.0.0.1:8111`）是适用于 Web/远程客户端
和 Zed 远程 SSH 场景的独立传输方式：

```bash
codegraph serve --http           # 前台，绑定 127.0.0.1:8111
codegraph serve --http --detach  # 后台运行
codegraph http list / stop       # 管理运行中的 HTTP 服务
```

自定义扩展名映射写在 `.codegraph/codegraph.json`；可选的 Claude prompt hook
通过 `codegraph install --prompt-hook` 启用。
运行中的 watcher 会热重载该文件、当前索引根的 `config.toml`，以及项目根
`.gitignore`，并执行一次完整 reconcile：新排除的文件会被移出图谱，新纳入或新扩展名
文件会被加入，无需重启 MCP 服务。TOML 损坏时保留最后一次有效 scope 并报告错误；
JSON 继续沿用宽容的空 override 语义。

若希望 vendor、生成代码等继续被索引和直接查询，但在排序中低于一方代码，可配置：

```toml
[indexing]
deprioritize = ["vendor/**", "generated/**"]
```

兼容格式 `.codegraph/codegraph.json` 也接受顶层 `deprioritize` 数组。JSON 规则先执行，
TOML 规则后执行并最终生效；有序 `!pattern` 例外采用 last-match-wins。该配置只影响
search/explore 排名，不删除文件、节点或边；长驻 MCP 进程会在每次请求重新读取当前
项目策略，无需重启。

完整参考——守护进程生命周期、所有环境变量、HTTP 注册表、扩展名映射、Claude hook：
[`../cli.md`](../cli.md#daemon-watch--environment-variables)。

---

## CLI 子命令

核心命令：`init`、`index`、`sync`、`search`、`explore`、`node`、`files`、
`status`、`serve`、`callers`、`callees`、`impact`、`affected`、`check`、
`audit`、`export`、`unlock`、`http`、`mcp`。

代理 / 安装命令：`install`、`uninstall`、`skill`、`self-update`、`completions`。

路径约定：大多数遍历命令把项目路径作位置参数或 `-p/--path`；`search`/`files`/
`serve` 用 `-p/--path`。`query` 继续作为 `search` 的向后兼容别名。

> **完整参考（含所有标志）：** [`../cli.md`](../cli.md)

---

## Shell 补全（Tab Completion）

```bash
codegraph completions bash --install        # Bash
codegraph completions zsh --install         # Zsh
codegraph completions fish --install        # Fish
codegraph completions powershell --install  # PowerShell
codegraph completions elvish --install      # Elvish
```

不加 `--install` 则输出到 stdout，可自行重定向。完整的各 shell 安装位置与说明：
[`../cli.md`](../cli.md)。

---

## CodeGraph 的能力范围

**做什么：** 确定性代码结构提取，支持 38 种语言（TypeScript、Python、Go、Rust、
Java、C/C++、C#、Vue、Svelte、GDScript 等——详见
[`../languages.md`](../languages.md)），跨文件解析（含 Godot 项目图），图遍历，
FTS5 检索，全图导出（含确定性 PageRank 中心性），MCP/CLI 表面，golden 字节稳定输出。

提取按文件 fail-closed：若源码树超过确定性的 256 层逻辑 named-node 深度限制，
CodeGraph 会记录稳定的文件错误，不保留该文件的部分节点、边或未解析引用，并继续
索引项目中的其他文件。后续增量 `sync` 会删除该文件原有图谱，并与
`index --force` 收敛到相同结果。

图谱正确性还覆盖：Erlang `module::function/arity` 身份、Rust 泛型/引用/qualified
`impl` 归属与经字段类型验证的 `self.field.method()`、JS/TS 对象字面量 namespace
成员、TypeScript/JavaScript 配置继承 alias、Python import alias、Lua function
expression callable、无扩展名导入解析到 `.xsjs`/`.xsjslib`，以及 `.h` 中普通派生
C++ class/struct。精确静态分析边界见 [`../languages.md`](../languages.md)。

**不做什么：** 二进制内部无任何 AI/向量/嵌入/LLM（硬约束，`scripts/guardrail.sh`
强制执行）；无语义检索；不新增固定 `LANGUAGES` 集以外的语言。

---

## 支持的语言

CodeGraph 支持 **38 种语言**，按提取深度分为三个层级：

**Tier 1 — 完整符号提取（29 种）：** TypeScript、TSX、JavaScript、JSX、ArkTS、
Python、Go、Rust、Java、C、C++、C#、PHP、Ruby、Swift、Kotlin、Dart、Scala、Lua、
Luau、Objective-C、R、Solidity、Nix、Terraform、Erlang、CFML、GDScript、Pascal。

**Tier 2 — 嵌入式 / 模板提取（6 种）：** Vue（`<script>` 委托给 TS/JS）、
Svelte（脚本块委托）、Astro、Razor/`.cshtml`、Liquid（Shopify 模板与 sections）、
XML/MyBatis mapper。

**Tier 3 — 仅文件级索引（3 类、6 个内部语言变体）：** YAML、Twig、Properties
以及 GodotScene、GodotResource、GodotProject——作为文件节点索引，具体 Godot
语义边由 framework resolver 补充。

完整列表（含各语言扩展名和说明）：[`../languages.md`](../languages.md)。

---

## 文档

- [`../architecture.md`](../architecture.md) — crate 依赖图、提取/解析/遍历/检索流水线、daemon/watch 生命周期。
- [`../data-model.md`](../data-model.md) — SQLite/FTS5 存储契约。
- [`../equivalence.md`](../equivalence.md) — 3 层等价预言机、golden 再生流程、KNOWN_DIFFS 规则格式。
- [`../languages.md`](../languages.md) — 支持语言完整列表，按提取深度分层。
- [`../godot.md`](../godot.md) — Godot 静态分析参考：`.tscn`/`.tres`/`project.godot`/`.gd` 的提取内容、静态与运行时边界、动态可达性诚实信号。
- [`../grammar-manifest.md`](../grammar-manifest.md) / [`../embedded-extraction.md`](../embedded-extraction.md) — 语言支持与提取层级（工程 ABI 细节）。
- [`../cli.md`](../cli.md) — 完整 CLI 子命令参考（22 个子命令，所有标志）、守护进程/监听环境变量、HTTP MCP 服务、自定义扩展名映射、Claude hook。
- [`../mcp.md`](../mcp.md) — MCP 服务器协议、全部 10 个工具、JSON-RPC 详情、各 IDE 配置、Zed 远程 SSH。
- [`../troubleshooting.md`](../troubleshooting.md) — `init` / `index` / `sync` 卡顿时的 JSONL 调试日志、隐私边界与反馈清单。
- [`../../examples/`](../../examples/) — codegraph + LLM 编排示例。

---

## 许可证

MIT，详见 [`../../LICENSE-MIT`](../../LICENSE-MIT)。
