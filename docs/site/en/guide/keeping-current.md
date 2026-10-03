# Keeping the index current

This page explains how the index follows your edits, how to update it yourself, and how to use CodeGraph in CI.

## In the background

When an agent or editor starts `codegraph serve --mcp` for an indexed project, CodeGraph starts one background
process for that project, or joins the one already running. It watches the project's files and re-indexes the ones
that change, after a short pause that groups a burst of saves into one update (two seconds by default). Every agent,
editor and terminal on the project shares that process, and it exits on its own a few minutes after the last one
disconnects.

The watcher skips the directories the index skips (`.gitignore`, `node_modules`, `target` and the other defaults), so
large dependency trees cost nothing. Editing `.codegraph/config.toml`, `.codegraph/codegraph.json` or the root
`.gitignore` takes effect without a restart.

The index trails a save by about a second. In that window a tool may read a file whose stored line numbers no longer
match its bytes. CodeGraph checks every file before it shows source from it, and a changed file is shown whole or
not at all, never cut at stale line numbers. The answer says which files were affected.

Watching is turned off automatically where it cannot work well: on WSL for projects under `/mnt/`, and when the
project root would be your home directory or the filesystem root.

## By hand

```sh
codegraph sync .            # re-index what changed, drop what was deleted
codegraph status .          # is the index current, and what changed since
codegraph index --force .   # rebuild from scratch, only when a command asks for it
```

`sync` compares each file's size and modification time with the index, re-reads only what changed, and updates every
reference those changes can move. Its result is the same as a full rebuild; the project's tests check that byte for
byte. In a Git repository, `status` finds changed files through Git when it can trust it, which keeps it fast on
large trees.

## In CI and scripts

```sh
export CODEGRAPH_NO_DAEMON=1        # stay in the foreground; start no background process
codegraph init .                    # or `codegraph sync .` on a cached index
codegraph affected src/pricing.ts -p . --filter 'tests/*'
```

`affected` takes changed files and lists the files that depend on them, transitively, and the test files among them,
so a pipeline can run only the tests a change can reach. `--depth` limits how far it follows dependents.

With `CODEGRAPH_NO_DAEMON=1`, `serve --mcp` also stays in the foreground. Only one such server may keep a project's
index up to date at a time; a second one exits and names the first. `serve --no-watch` keeps a server's first
catch-up but stops it from watching afterwards.

## When something is stuck

- `codegraph status .` explains an index it cannot use and prints the command that fixes it.
- `codegraph unlock .` removes a lock left behind by a process that crashed. It leaves a live process's lock alone.
- The [CLI reference](../../../cli.md#daemon-watch--environment-variables) lists every environment variable that tunes
  the background process, and [Troubleshooting](../../../troubleshooting.md) shows how to record a diagnostic log.
