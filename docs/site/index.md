---
layout: home
title: "CodeGraph：供 Agent 和开发者使用的本地代码知识图谱"
titleTemplate: false
description: CodeGraph 用 tree-sitter 把代码仓库解析成本机的符号与调用索引，回答谁调用了某个函数、改动会影响哪里、一个函数如何调用到另一个函数，可在命令行、MCP 和浏览器查看器中使用，不依赖任何模型。

hero:
  name: CodeGraph
  text: 谁调用了它，改动它会影响哪里
  tagline: CodeGraph 用 tree-sitter 把代码仓库解析成本机的符号、调用和导入索引，并据此回答结构性问题：在命令行中，为编码 Agent 通过 MCP，也在浏览器查看器中。它不包含任何模型，同一个仓库在任何机器上都得到同样的答案。
  actions:
    - theme: brand
      text: 安装
      link: /guide/install
    - theme: alt
      text: 快速开始
      link: /guide/quick-start
    - theme: alt
      text: GitHub
      link: https://github.com/sunerpy/codegraph-rust

home:
  facts:
    - term: 运行平台
      text: Linux、macOS 和 Windows，x86_64 与 ARM64。单个可执行文件，内置 SQLite。
    - term: 你的代码
      text: 索引保存在项目的 .codegraph 目录中，建立索引和查询都不联网。

  visual:
    desktop:
      light: /screens/viewer-symbol-light.webp
      dark: /screens/viewer-symbol-dark.webp
      width: 1440
      height: 900
      alt: CodeGraph 查看器显示方法 IndexPaths::resolve：左侧是它的调用方，中间是源码，右侧是它调用的函数。查看器界面为英文。

  index:
    title: CodeGraph 能做什么
    intro: 浏览器查看器目前是预览功能，需要手动开启；其余功能都属于常规命令。
    groups:
      - name: 查询
        items:
          - title: 了解一块代码
            body: 提一个问题或给出几个名称，按文件返回相关符号的源码、依赖它们的代码，以及它们之间的调用。
            status: available
            link: /guide/quick-start
          - title: 查看符号或文件
            body: 符号的源码及其调用和被调用关系，或带行号的文件内容以及依赖它的文件。
            status: available
            link: /guide/quick-start
          - title: 调用方、被调用方和影响范围
            body: 向上或向下一层，或沿调用方的调用方找出改动会影响的全部代码。
            status: available
            link: /guide/quick-start
          - title: 受影响的测试
            body: 根据改动的文件，列出依赖它们的文件以及其中的测试。
            status: available
            link: /guide/keeping-current
      - name: 接入
        items:
          - title: 面向编码 Agent 的 MCP
            body: 通过标准输入输出提供只读工具，一条命令即可写入每个已安装 Agent 的配置。
            status: available
            link: /guide/agents
          - title: 通过 HTTP 提供 MCP
            body: 在本机通过 streamable HTTP 提供同样的工具，适用于编辑器和远程开发。
            status: available
            link: /guide/agents
          - title: Agent Skill
            body: 告诉 Agent 在什么情况下使用哪个工具的说明，安装到各 Agent 的 Skill 目录。
            status: available
            link: /guide/agents
      - name: 保持最新
        items:
          - title: 监听并更新
            body: 每个项目一个后台进程，由所有使用者共享，重新索引你修改过的文件。
            status: available
            link: /guide/keeping-current
          - title: 增量同步
            body: 只重新读取发生变化的文件，结果与完整重建一致。
            status: available
            link: /guide/keeping-current
      - name: 查看
        items:
          - title: 浏览器查看器
            body: 符号、文件、调用路径、模块结构图、类型层级和未被到达的代码，支持浅色和深色主题。
            status: preview
            link: /guide/viewer
      - name: 语言
        items:
          - title: 完整的符号提取
            body: TypeScript、JavaScript、Python、Go、Rust、Java、C、C++、C#、PHP、Ruby、Swift、Kotlin 等。
            status: available
            link: /en/reference/languages
          - title: 模板和组件
            body: Vue、Svelte、Astro、Razor 和 Liquid 文件，以及 MyBatis mapper XML。
            status: available
            link: /en/reference/languages
          - title: Godot 项目
            body: 场景、资源、脚本、自动加载和信号处理函数，并提供资源审计。
            status: available
            link: /en/reference/godot

  steps:
    title: 从安装到第一个答案
    items:
      - title: 安装
        command: curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
        body: Windows 上使用 PowerShell 脚本，也可以从发布页下载压缩包。
      - title: 为项目建立索引
        command: codegraph init .
        body: 在项目的 .codegraph 目录中建立索引，之后的更新只读取发生变化的文件。
      - title: 提一个问题
        command: codegraph explore "how does checkout compute the total" -p .
        body: 返回相关的源码、依赖它的代码，以及其中的调用关系。
      - title: 接入你的 Agent
        command: codegraph install --yes
        body: 把 MCP 服务写入它找到的每个 Agent 的配置。

  tools:
    columns: [问题, 命令行, MCP 工具]
    rows:
      - question: 这块代码是怎么工作的？
        cli: codegraph explore
        mcp: codegraph_explore
      - question: 查看这个符号或文件
        cli: codegraph node
        mcp: codegraph_node
      - question: 它在哪里定义？
        cli: codegraph search
        mcp: codegraph_search
      - question: 谁调用了它？
        cli: codegraph callers
        mcp: codegraph_callers
      - question: 改动它会影响哪里？
        cli: codegraph impact
        mcp: codegraph_impact
    caption: 两侧运行的是同一个引擎，Agent 和你得到的答案相同。所有工具都是只读的。

  shots:
    flow:
      light: /screens/viewer-flow-light.webp
      dark: /screens/viewer-flow-dark.webp
      width: 1440
      height: 900
      alt: 查看器的 Flow 视图，从 cmd_explore 到 explore_file_header，每次调用一张卡片，下方的表格列出每一步及其置信度。查看器界面为英文。

  languages:
    columns: [深度, 提取内容, 语言]
    rows:
      - depth: 完整符号
        extracted: 函数、类、方法、导入、调用和引用
        members: TypeScript、TSX、JavaScript、JSX、ArkTS、Python、Go、Rust、Java、C、C++、C#、PHP、Ruby、Swift、Kotlin、Dart、Scala、Lua、Luau、Objective-C、R、Solidity、Nix、Terraform、Erlang、GDScript、Pascal、CFML
      - depth: 嵌入内容
        extracted: 宿主文件中嵌入的脚本和模板标记
        members: Vue、Svelte、Astro、Razor、Liquid、MyBatis XML
      - depth: 文件级
        extracted: 文件以及文件之间的引用
        members: YAML、Twig、Properties，以及 Godot 的场景、资源和项目文件
    caption: 表中没有的语言，其文件不会进入图中。可以在 .codegraph/codegraph.json 中把其他扩展名映射到这些语言。

  platforms:
    title: 平台
    intro: 每个版本为每个平台提供一个压缩包，并附带 SHA256SUMS 文件和每个压缩包的构建来源证明。
    columns: [平台, Target, 压缩包]
    rows:
      - name: Linux x86_64
        status: available
        cells: [x86_64-unknown-linux-musl, .tar.gz]
      - name: Linux ARM64
        status: available
        cells: [aarch64-unknown-linux-musl, .tar.gz]
      - name: macOS Intel
        status: available
        cells: [x86_64-apple-darwin, .tar.gz]
      - name: macOS Apple Silicon
        status: available
        cells: [aarch64-apple-darwin, .tar.gz]
      - name: Windows x86_64
        status: available
        cells: [x86_64-pc-windows-msvc, .zip]
      - name: Windows ARM64
        status: available
        cells: [aarch64-pc-windows-msvc, .zip]
    note: Linux 版本是静态链接的，不依赖任何系统库。从源码构建需要 Rust 工具链和 C 编译器。

  privacy:
    title: 哪些内容留在你的电脑上
    intro: CodeGraph 不依赖任何托管服务。
    sendsLabel: 联网内容
    modes:
      - name: 建立索引和查询
        sends: 不联网
        detail: 索引是项目中的一个 SQLite 数据库，解析、引用匹配和所有查询都在本机进行。
      - name: Agent、编辑器和查看器
        sends: 不联网
        detail: MCP 服务只与启动它的程序通信，HTTP 服务和查看器只在本机监听。
      - name: 安装和更新
        sends: 从 GitHub 下载
        detail: 安装脚本和 codegraph self-update 从 GitHub Releases 下载发布版本。

  scope:
    title: 它不做什么
    items:
      - 不运行任何模型，没有 embedding，也没有向量检索；只根据解析出的结构作答。
      - 不评判代码。代码是否正确仍由编译器、lint 工具和测试判断。
      - 看不到运行时行为。反射、运行时回调和由字符串拼出的调用不会形成边。
      - 不会猜测它无法解析的语言。
---

<HomeIndex />

<HomeSteps />

<SplitBlock proof="tools">

## 面向编码 Agent

接入 CodeGraph 的 Agent 只需提出一个结构性问题，不必逐个搜索和读取文件，得到的回答直接包含相关源码以及它们之间的调用关系。答案来自解析出的代码，每次运行都相同。

`codegraph install --yes` 会把 MCP 服务写入 Claude Code、Cursor、Codex CLI、Kiro、Zed、VS Code 以及其他受支持 Agent 的配置。

[接入编码 Agent](guide/agents.md) · [MCP 参考](../mcp.md)

</SplitBlock>

<SplitBlock proof="screen" shot="flow" flip>

## 在浏览器中查看

查看器读取同一份索引：一个符号的调用方和被调用方、一个函数到另一个函数的调用路径、按模块划分的仓库结构图、类型的继承层级，以及没有任何代码到达的代码。它在你的电脑上运行，只读取，不修改。

[浏览器查看器](guide/viewer.md)

</SplitBlock>

<SplitBlock proof="languages">

## 支持多种语言，并如实说明深度

大多数语言提供完整的符号提取；组件和模板提取其中嵌入的脚本和标记；少数格式只记录到文件级。表格分别列出，不用一个笼统的数字概括。

[支持的语言](../languages.md) · [Godot](../godot.md)

</SplitBlock>

<HomePlatforms />

<HomePrivacy />

## 安装

::: code-group

```sh [Linux 和 macOS]
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
```

```powershell [Windows]
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

```sh [从源码]
cargo install --locked --git https://github.com/sunerpy/codegraph-rust codegraph-rs
```

:::

安装脚本在安装前会用该版本的 `SHA256SUMS` 校验每个压缩包。[安装指南](guide/install.md)介绍了如何安装指定版本、更新和卸载。

<HomeScope />

## 反馈

问题报告和功能建议请提交到 [GitHub Issues](https://github.com/sunerpy/codegraph-rust/issues)。CodeGraph 以 MIT 许可发布。
