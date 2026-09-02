# Troubleshooting

## Slow or apparently stalled indexing

`init`, `index`, and `sync` can write an opt-in JSONL diagnostic log:

```bash
codegraph init /path/to/project --debug
codegraph index /path/to/project --debug
codegraph sync /path/to/project --debug

# Choose the output path explicitly; this also enables diagnostics.
codegraph index /path/to/project --debug-log /tmp/codegraph-index.jsonl
```

The default path is:

```text
.codegraph/diagnostics/<command>-<UTC timestamp>-<pid>.jsonl
```

The CLI prints the selected path once, including with `--quiet --debug`.
`init` does not create `.codegraph` early: it starts the log in a temporary
project-root file, then moves it into `diagnostics/` after the rebuild owns a
valid index root.

The log records phase timings, ordered parse/persist progress, one-second
heartbeats, files that remain in one stage for more than five seconds, and
batched reference-resolution mode/progress. Paths are project-relative. It
does not record source text, file contents, environment-variable values, or the
absolute project path. Logs are flushed after every JSON line and are not
deleted automatically.

Tree-sitter parsing is deliberately not timed out, skipped, or cancelled by the
watchdog. A `slow_file` event identifies where work is spending time; it does
not change the index result. A successful index still fully parses every
admitted file. If the user terminates a rebuild, the existing atomic rebuild
protocol leaves it in `phase=building` rather than publishing a partial index.

`--debug` controls the structured JSONL diagnostics. Existing tracing controls
remain separate and compatible:

```bash
CODEGRAPH_DEBUG=1 codegraph index /path/to/project
RUST_LOG=codegraph_resolve=debug codegraph index /path/to/project
```

When reporting an indexing problem, include both:

```bash
codegraph --version
```

and the generated JSONL file. The most useful records are `heartbeat`,
`slow_file`, `file_complete`, `resolution_setup`, and the final `session_end`.
