---
layout: home
title: "CodeGraph: a local code knowledge graph for agents and people"
titleTemplate: false
description: CodeGraph parses a repository with tree-sitter into a local index of symbols and calls, and answers who calls a function, what a change reaches and how one function reaches another, on the command line, over MCP and in a browser viewer. No model involved.

hero:
  name: CodeGraph
  text: Who calls this, and what does changing it reach
  tagline: CodeGraph parses a repository with tree-sitter into a local index of symbols, calls and imports, and answers structural questions from it, on the command line, for coding agents over MCP, and in a browser viewer. There is no model inside, so the same repository gives the same answer on every machine.
  actions:
    - theme: brand
      text: Install
      link: /en/guide/install
    - theme: alt
      text: Quick start
      link: /en/guide/quick-start
    - theme: alt
      text: GitHub
      link: https://github.com/sunerpy/codegraph-rust

home:
  facts:
    - term: Runs on
      text: Linux, macOS and Windows, on x86_64 and ARM64. One executable with SQLite built in.
    - term: Your code
      text: The index stays in the project's .codegraph directory. Indexing and queries never go online.

  visual:
    desktop:
      light: /screens/viewer-symbol-light.webp
      dark: /screens/viewer-symbol-dark.webp
      width: 1440
      height: 900
      alt: The CodeGraph viewer showing the method IndexPaths::resolve, with its callers on the left, its source in the middle and the functions it calls on the right.

  index:
    title: What CodeGraph does
    intro: The browser viewer is a preview and is switched off unless you turn it on; everything else is part of the regular command set.
    groups:
      - name: Ask
        items:
          - title: Explore an area
            body: Ask a question or name a few symbols; get their source grouped by file, what depends on them, and the calls between them.
            status: available
            link: /en/guide/quick-start
          - title: Read a symbol or a file
            body: A symbol's source with what it calls and what calls it, or a file with line numbers and the files that depend on it.
            status: available
            link: /en/guide/quick-start
          - title: Callers, callees and impact
            body: One step either way, or everything a change reaches through callers of callers.
            status: available
            link: /en/guide/quick-start
          - title: Affected tests
            body: From a list of changed files, the files that depend on them and the tests among them.
            status: available
            link: /en/guide/keeping-current
      - name: Connect
        items:
          - title: MCP for coding agents
            body: Read-only tools over standard input and output, written into each installed agent's config by one command.
            status: available
            link: /en/guide/agents
          - title: MCP over HTTP
            body: The same tools over streamable HTTP on the local machine, for editors and remote development.
            status: available
            link: /en/guide/agents
          - title: Agent skill
            body: Instructions that tell an agent which tool to use when, installed into each agent's skill directory.
            status: available
            link: /en/guide/agents
      - name: Stay current
        items:
          - title: Watch and update
            body: One background process per project, shared by every client, re-indexes the files you change.
            status: available
            link: /en/guide/keeping-current
          - title: Incremental sync
            body: Re-reads only what changed and ends up identical to a full rebuild.
            status: available
            link: /en/guide/keeping-current
      - name: See
        items:
          - title: Browser viewer
            body: Symbols, files, call paths, a module map, type hierarchies and unreached code, in light and dark.
            status: preview
            link: /en/guide/viewer
      - name: Languages
        items:
          - title: Full symbol extraction
            body: TypeScript, JavaScript, Python, Go, Rust, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin and more.
            status: available
            link: /en/reference/languages
          - title: Templates and components
            body: Vue, Svelte, Astro, Razor and Liquid files, and MyBatis mapper XML.
            status: available
            link: /en/reference/languages
          - title: Godot projects
            body: Scenes, resources, scripts, autoloads and signal handlers, with a resource audit.
            status: available
            link: /en/reference/godot

  steps:
    title: From install to the first answer
    items:
      - title: Install
        command: curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
        body: Or the PowerShell script on Windows, or an archive from the releases page.
      - title: Index a project
        command: codegraph init .
        body: Builds the index in the project's .codegraph directory. Later updates read only what changed.
      - title: Ask a question
        command: codegraph explore "how does checkout compute the total" -p .
        body: Returns the relevant source, what depends on it and the calls between it.
      - title: Connect your agent
        command: codegraph install --yes
        body: Adds the MCP server to the config of every agent it finds.

  tools:
    columns: [Question, Command line, MCP tool]
    rows:
      - question: How does this area work?
        cli: codegraph explore
        mcp: codegraph_explore
      - question: Show me this symbol or file
        cli: codegraph node
        mcp: codegraph_node
      - question: Where is it defined?
        cli: codegraph search
        mcp: codegraph_search
      - question: Who calls it?
        cli: codegraph callers
        mcp: codegraph_callers
      - question: What does changing it reach?
        cli: codegraph impact
        mcp: codegraph_impact
    caption: Both sides run the same engine, so an agent and you get the same answer. Every tool is read-only.

  shots:
    flow:
      light: /screens/viewer-flow-light.webp
      dark: /screens/viewer-flow-dark.webp
      width: 1440
      height: 900
      alt: The viewer's Flow view from cmd_explore to explore_file_header, one card per call and a table of every hop with its confidence.

  languages:
    columns: [Depth, What is extracted, Languages]
    rows:
      - depth: Full symbols
        extracted: Functions, classes, methods, imports, calls and references
        members: TypeScript, TSX, JavaScript, JSX, ArkTS, Python, Go, Rust, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin, Dart, Scala, Lua, Luau, Objective-C, R, Solidity, Nix, Terraform, Erlang, GDScript, Pascal, CFML
      - depth: Embedded
        extracted: The scripts and template markup inside a host file
        members: Vue, Svelte, Astro, Razor, Liquid, MyBatis XML
      - depth: File level
        extracted: Files and the references between them
        members: YAML, Twig, Properties, and Godot scene, resource and project files
    caption: A file in a language outside this table is not in the graph. Extensions can be mapped onto these languages in .codegraph/codegraph.json.

  platforms:
    title: Platforms
    intro: Every release has an archive for each platform, a SHA256SUMS file and a build attestation per archive.
    columns: [Platform, Target, Archive]
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
    note: The Linux builds are statically linked and need no system library. Building from source needs a Rust toolchain and a C compiler.

  privacy:
    title: What stays on your machine
    intro: CodeGraph depends on no hosted service.
    sendsLabel: Goes online
    modes:
      - name: Indexing and queries
        sends: Nothing
        detail: The index is a SQLite database inside the project. Parsing, resolution and every query run locally.
      - name: Agents, editors and the viewer
        sends: Nothing
        detail: The MCP server talks to the program that started it; the HTTP server and the viewer listen on the local machine.
      - name: Install and updates
        sends: Downloads from GitHub
        detail: The install scripts and codegraph self-update fetch releases from GitHub Releases.

  scope:
    title: What it does not do
    items:
      - It runs no model, no embeddings and no vector search; it answers from the parsed structure only.
      - It does not judge code. Whether it is correct stays with your compiler, linter and tests.
      - It does not see run time. Reflection, runtime callbacks and calls built from strings leave no edge.
      - It does not guess at languages it cannot parse.
---

<HomeIndex />

<HomeSteps />

<SplitBlock proof="tools">

## For coding agents

An agent with CodeGraph asks one structural question instead of searching and reading files one at a time, and gets
back the relevant source together with the calls between it. Answers come from the parsed code, so they are the
same on every run.

`codegraph install --yes` writes the MCP server into the config of Claude Code, Cursor, Codex CLI, Kiro, Zed, VS Code
and the other agents it supports.

[Connect a coding agent](guide/agents.md) · [MCP reference](../../mcp.md)

</SplitBlock>

<SplitBlock proof="screen" shot="flow" flip>

## For you, in the browser

The viewer reads the same index: a symbol with its callers and callees, the call path from one function to another,
the repository as a map of modules, the hierarchy of a type, and the code nothing reaches. It runs on your computer
and only reads.

[The browser viewer](guide/viewer.md)

</SplitBlock>

<SplitBlock proof="languages">

## Many languages, at an honest depth

Most languages get full symbol extraction. Components and templates get their embedded scripts and markup, and a few
formats are tracked at file level. The table says which is which, rather than one number for all of them.

[Supported languages](../../languages.md) · [Godot](../../godot.md)

</SplitBlock>

<HomePlatforms />

<HomePrivacy />

## Install

::: code-group

```sh [Linux and macOS]
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
```

```powershell [Windows]
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

```sh [From source]
cargo install --locked --git https://github.com/sunerpy/codegraph-rust codegraph-rs
```

:::

The scripts check every archive against the release's `SHA256SUMS` before installing it. The
[install guide](guide/install.md) covers pinning a version, updating and removing CodeGraph.

<HomeScope />

## Feedback

Bug reports and feature requests go to [GitHub Issues](https://github.com/sunerpy/codegraph-rust/issues). CodeGraph is
MIT-licensed.
