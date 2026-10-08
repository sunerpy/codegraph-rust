# Frequently asked questions

This page answers the questions people ask most often when they start using CodeGraph.

## Does CodeGraph use AI, or send my code anywhere?

No. CodeGraph contains no model of any kind, and indexing and queries run entirely on your computer. The only commands
that go online are the install scripts and `codegraph self-update`, which download releases from GitHub.
[Data and network](../privacy.md) lists everything CodeGraph reads, writes and opens.

## Should I commit `.codegraph/`?

No. It is a local cache that can be rebuilt from the source at any time, and its contents depend on the machine. A
new index directory contains a nested `.gitignore`, so it stays out of `git status` without changing the project's
root `.gitignore`. CodeGraph leaves an index directory that already existed untouched; add that directory to the
project's `.gitignore` yourself if it is still visible.

## A call I can see in the code is missing. Why?

Usually one of these:

- the call goes through something only known at run time: a callback, reflection, a name built from a string;
- the name matches several definitions and the context does not decide between them;
- the file is in a language CodeGraph does not parse, is excluded by `.gitignore` or `config.toml`, or is larger than
  `indexing.max_file_size`;
- the file changed and has not been re-indexed yet: `codegraph status` shows pending changes.

`codegraph node <name>` shows what the index knows about a symbol, which narrows down which case it is.

## It says the index belongs to a different filesystem location.

The project was moved or copied together with its `.codegraph/` directory. An index is tied to the location it was
built in, so CodeGraph refuses to use it elsewhere rather than answer about the wrong files. Run
`codegraph init <project>` in the new location to replace it.

## It says the index was built by a newer CodeGraph version.

A newer release built this index, and the copy of CodeGraph reading it is older. CodeGraph will not guess at a format
it does not know. Update every copy, including the one your agent or editor starts, with `codegraph self-update` or
the install script. After an update, restart the agents and editors that were already running.

## My agent asks for `projectPath` on every call.

The MCP server did not find a project to default to. Index the project with `codegraph init`, start the agent from
inside the project, or pin the project in the agent's config with `-p <project>`.
[Connect a coding agent](../guide/agents.md#which-project-the-server-answers-for)

## A command reports a lock, or seems stuck.

Another CodeGraph process may be writing to the same index; wait for it to finish. If a process crashed and left its
lock behind, `codegraph unlock <project>` removes it and leaves the lock of a live process alone. For a slow index,
`codegraph index --debug-log index.jsonl` records what each file took; [Troubleshooting](../../../troubleshooting.md)
explains the log.

## Does it work on Windows and WSL?

Yes. Windows has its own builds for x86_64 and ARM64. Under WSL, a new index for a project on a Windows drive
(`/mnt/c/…`) goes into its own directory, `.codegraph-wsl/`, because SQLite's locking does not work across the Windows
and WSL boundary; an index already in `.codegraph/` there is kept. File watching under `/mnt/` is off.

## How do I report a problem?

Open an issue on [GitHub](https://github.com/sunerpy/codegraph-rust/issues) with the version (`codegraph --version`),
your platform, the command and its output. For indexing problems, attach the diagnostic log described in
[Troubleshooting](../../../troubleshooting.md). It records project-relative paths and timings, not source text, file
contents or environment variables.
