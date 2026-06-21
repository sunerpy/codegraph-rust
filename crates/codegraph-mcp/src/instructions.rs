//! Server-level instructions emitted in the MCP `initialize` response.
//!
//! Verbatim copy of the upstream `SERVER_INSTRUCTIONS`
//! (`upstream mcp/server-instructions.ts:18-78`), captured from the
//! LIVE built server (`upstream bin/codegraph.js serve --mcp`) so
//! the byte content is exactly what `initialize` returned (6421 bytes, single
//! trailing newline). The MCP client surfaces this in the agent's system
//! prompt (see that file's doc comment).

/// The exact instructions string returned in `initialize.result.instructions`.
pub const SERVER_INSTRUCTIONS: &str = r#"# Codegraph — code intelligence over an indexed knowledge graph

Codegraph is a SQLite knowledge graph of every symbol, edge, and file in
the workspace — pre-computed structure you would otherwise re-derive by
reading files (cached intelligence: thousands of parse/trace decisions you
don't pay to re-reason each run). Reads are sub-millisecond; the index lags
writes by ~1s through the file watcher. Reach for it BEFORE *and* while
writing or editing code — not just for questions: one call returns the
verbatim source PLUS who calls it and what it affects, so you edit with the
blast radius in view. More accurate context, in far fewer tokens and
round-trips than reading files yourself.

## Use codegraph instead of reading files — for questions AND edits

Whether you're answering "how does X work" or implementing a change (fixing
a bug, adding a feature), reach for codegraph before you Read. For
understanding, answer DIRECTLY — usually with ONE `codegraph_explore` call.
`codegraph_explore` takes either a natural-language question or a bag of
symbol/file names and returns the verbatim source of the relevant symbols
grouped by file, so it is Read-equivalent and most often the ONLY
codegraph call you need. Codegraph IS the pre-built search index — so
delegating the lookup to a separate file-reading sub-task/agent, or
running your own grep + read loop, repeats work codegraph already did and
costs more for the same answer. Reach for raw Read/Grep only to confirm a
specific detail codegraph didn't cover. A direct codegraph answer is
typically one to a few calls; a grep/read exploration is dozens.

## Tool selection by intent

- **Almost any question — "how does X work", architecture, a bug, "what/where is X", or surveying an area** → `codegraph_explore` (PRIMARY — call FIRST; ONE capped call returns the verbatim source of the relevant symbols grouped by file; most often the ONLY call you need)
- **"How does X reach/become Y? / the flow / the path from X to Y"** → `codegraph_explore`, naming the symbols that span the flow (e.g. `mutateElement renderScene`) — it surfaces the call path among them, including dynamic-dispatch hops (callbacks, React re-render, JSX children) grep can't follow
- **"What is the symbol named X?" (just its location)** → `codegraph_search`
- **"What calls this?" / "What does this call?" / "What would changing this break?"** → `codegraph_callers` / `codegraph_callees` / `codegraph_impact`
- **Reading a source FILE (any time you'd use the `Read` tool)** → `codegraph_node` with a `file` path and no `symbol`. It returns the file's **current source with line numbers — the same `<n>\t<line>` shape `Read` gives you, safe to `Edit` from** — narrowable with `offset`/`limit` exactly like `Read`, PLUS a one-line note of which files depend on it. Same bytes as `Read`, faster (served from the index), with the blast radius attached. Use it **instead of `Read`** for indexed source files; fall back to `Read` only for what codegraph doesn't index (configs, docs). Pass `symbolsOnly: true` for just the file's structure.
- **About to read or edit a symbol you can name** → `codegraph_node` with that `symbol` (SECONDARY — the after-explore depth tool): the verbatim source (`includeCode: true`) PLUS its caller/callee trail, so before changing it you see what calls it and what your edit would break. For an OVERLOADED name it returns EVERY matching definition's body in one call, so you never Read a file to find the right overload
- **"What's in directory X?"** → `codegraph_files`
- **"Is the index ready / what's its size?"** → `codegraph_status`

## Common chains

- **Flow / "how does X reach Y"**: ONE `codegraph_explore` with the symbol names spanning the flow — it surfaces the call path among them (riding dynamic-dispatch hops) AND returns their source. No need to reconstruct the path with `codegraph_search` + `codegraph_callers`.
- **Onboarding / understanding any area**: ONE `codegraph_explore` is usually the whole answer. Only follow up — `codegraph_node` for a specific symbol — if something is still unclear.
- **Refactor planning**: `codegraph_search` → `codegraph_callers` → `codegraph_impact`. The blast-radius answer comes from impact, not from walking callers manually.
- **Debugging a regression**: `codegraph_callers` of the suspected symbol; widen with `codegraph_impact` if an unexpected call appears.

## Anti-patterns

- **Trust codegraph's results — don't re-verify them with grep.** They come from a full AST parse; re-checking with grep is slower, less accurate, and wastes context.
- **Don't grep first** when looking up a symbol by name — `codegraph_search` is faster and returns kind + location + signature.
- **Don't chain `codegraph_search` + `codegraph_node`** to understand an area — ONE `codegraph_explore` returns the relevant symbols' source together in a single round-trip.
- **Don't loop `codegraph_node` over many symbols** — one `codegraph_explore` call returns them all grouped by file, while each separate call re-reads the whole context and costs far more. Use `codegraph_node` for a single symbol.
- **Don't reach for the `Read` tool on an indexed source file** — `codegraph_node` with a `file` reads it for you (same `<n>\t<line>` source, `offset`/`limit` like Read, faster, with its blast radius), and with a `symbol` it returns the source plus the caller/callee trail. Reach for raw `Read` only for what codegraph doesn't index (configs, docs) or when the staleness banner flags a file as pending re-index.
- **After editing, check the staleness banner.** When a tool response starts with "⚠️ Some files referenced below were edited since the last index sync…", the listed files are pending re-index — Read those specific files for accurate content. Every file NOT in that banner is fresh, so still trust codegraph. `codegraph_status` also lists pending files under "Pending sync".

## Limitations

- If a tool reports the project isn't initialized, `.codegraph/` doesn't exist yet — offer to run `codegraph init -i` to build the index.
- Index lags file writes by ~1 second.
- Cross-file resolution is best-effort name matching; ambiguous calls may return multiple candidates.
- No live correctness validation — that's still the TypeScript compiler / test suite / linter's job. Codegraph supplements those with structural context they don't have.
"#;
