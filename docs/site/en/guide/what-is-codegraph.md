# What is CodeGraph

This page explains what CodeGraph answers, how it builds those answers, and where its answers stop.

CodeGraph reads a repository once with tree-sitter, records every symbol and every call, import and reference
between symbols in a local SQLite index, and answers structural questions from that index:

- Where is this function defined, and what does it look like?
- Who calls it, and what does it call?
- If I change it, which symbols and files are affected, including callers of callers?
- How does one function reach another?
- Which tests reach this code?

Text search finds strings. It cannot tell a call from a comment that mentions the same name, a method from a
same-named method on another type, or a direct caller from a caller three hops away. CodeGraph answers from the
parsed structure instead.

## How it works

1. **Parse.** Each file is parsed with a grammar for its language. Functions, classes, methods, imports, call sites
   and references become nodes and edges.
2. **Resolve.** References are matched to their definitions across files, through imports first and then by name.
   Every edge records how it was matched and with what confidence, and a reference that matches nothing stays
   unresolved.
3. **Store.** Nodes, edges and a full-text search index go into SQLite inside the project's `.codegraph/` directory.
   SQLite is built into the executable.
4. **Query.** Every command and tool reads that index. Nothing re-scans the repository to answer a question.
5. **Stay current.** A background process watches the project and re-indexes the files that change.
   [Keeping the index current](keeping-current.md) covers it.

## One index, three ways in

| Way in         | For                       | Starts with                                                                |
| -------------- | ------------------------- | -------------------------------------------------------------------------- |
| Command line   | you, scripts and CI       | `codegraph explore`, `codegraph search`, `codegraph impact`                |
| MCP server     | coding agents and editors | `codegraph serve --mcp`, written into agent configs by `codegraph install` |
| Browser viewer | reading unfamiliar code   | `CODEGRAPH_UI=1 codegraph ui` (preview)                                    |

The command line and the MCP tools run the same engine. `codegraph explore` prints exactly what the
`codegraph_explore` tool returns to an agent, so you can check what an agent was told.

## The same answer every time

CodeGraph contains no model: no embeddings, no vector search and no language model. Parsing and resolution are
deterministic, so the same files and configuration produce the same index, and the same question returns the same
answer, on any machine. An incremental `codegraph sync` converges with a full rebuild, and the project's test suite
checks that byte for byte.

## What it does not do

- **No similarity search.** It answers questions about the structure that is in the code. It does not find code by
  "roughly what it means".
- **No judgement.** It shows relationships and reach. Whether the code is correct is still a question for your
  compiler, linter and tests.
- **No runtime knowledge.** Reflection, callbacks registered at runtime and calls built from strings leave no static
  edge, so a path through them does not appear. A call through an interface or trait with several implementations is
  reported as such rather than tied to one of them.
- **A fixed set of languages.** A file in a language CodeGraph does not parse is not in the graph. The
  [language reference](../../../languages.md) lists what each tier extracts.

## Next steps

- [Install](install.md)
- [Quick start](quick-start.md)
- [Connect a coding agent](agents.md)
