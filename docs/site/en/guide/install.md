# Install

This page covers installing CodeGraph, keeping it up to date and removing it again.

CodeGraph is one executable named `codegraph`. Releases are published on
[GitHub Releases](https://github.com/sunerpy/codegraph-rust/releases) for Linux, macOS and Windows; the package is not
on crates.io.

## Linux and macOS

```sh
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
```

The script picks the archive for your platform, refuses to continue unless the archive's SHA-256 matches the
release's `SHA256SUMS`, and installs `codegraph` into `$HOME/.local/bin`. It needs `curl` or `wget`, `tar`, and
`sha256sum` or `shasum`. It does not edit your shell profile: when the directory is not on your `PATH`, it says so,
and adding it is up to you.

Set `CODEGRAPH_INSTALL_DIR` to install somewhere else.

## Windows

In PowerShell 5.1 or later:

```powershell
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

The script verifies the archive the same way and installs `codegraph.exe` into `%LOCALAPPDATA%\Programs\codegraph`.
It adds that directory to your **user** `PATH` if it is not there yet; open a new terminal for the change to apply.
`CODEGRAPH_INSTALL_DIR` changes the directory here too.

In Git Bash, MSYS2 or Cygwin, use the PowerShell script: the shell script stops and points you to it.

## Install a specific version

Both scripts install the latest release unless `CODEGRAPH_VERSION` names one. To make an install reproducible, take
the script from the same tag:

```sh
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/vX.Y.Z/scripts/install.sh \
  | CODEGRAPH_VERSION=vX.Y.Z sh
```

```powershell
$env:CODEGRAPH_VERSION = "vX.Y.Z"
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/vX.Y.Z/scripts/install.ps1 | iex
```

## Download an archive yourself

Every release carries six archives, one per platform:

| Platform            | Target                       | Archive   |
| ------------------- | ---------------------------- | --------- |
| Linux x86_64        | `x86_64-unknown-linux-musl`  | `.tar.gz` |
| Linux ARM64         | `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS Intel         | `x86_64-apple-darwin`        | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin`       | `.tar.gz` |
| Windows x86_64      | `x86_64-pc-windows-msvc`     | `.zip`    |
| Windows ARM64       | `aarch64-pc-windows-msvc`    | `.zip`    |

The Linux builds are statically linked, so they need no system library. Extract the archive and put `codegraph` on
your `PATH`.

Each release also has `SHA256SUMS`, and every archive carries a build attestation. To check where an archive was built:

```sh
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify codegraph-X.Y.Z-x86_64-unknown-linux-musl.tar.gz \
  --repo sunerpy/codegraph-rust \
  --signer-workflow sunerpy/codegraph-rust/.github/workflows/release.yml \
  --deny-self-hosted-runners
```

## Build from source

With a Rust toolchain and a C compiler (SQLite and the language grammars are compiled from source):

```sh
cargo install --locked --git https://github.com/sunerpy/codegraph-rust codegraph-rs
```

The package is `codegraph-rs`; the command it installs is `codegraph`. The repository pins the Rust version it is
tested with in `rust-toolchain.toml`.

## Check the install

```sh
codegraph --version
```

## Update

```sh
codegraph self-update              # the latest release
codegraph self-update --check      # only report whether a newer release exists
codegraph self-update --tag vX.Y.Z # a specific release
```

`self-update` downloads the archive for your platform from GitHub Releases, verifies it and replaces the running
executable. When `codegraph` lives in a directory you cannot write to, run it with the rights that directory needs.

A new release may change how the index is built. When it does, `codegraph status` reports the index as outdated, and
`codegraph sync` rebuilds it. Use `codegraph index --force` only when a command tells you the index needs recovery.
The [CLI reference](../../../cli.md#extraction-version-upgrades) has the details.

## Uninstall

Remove what CodeGraph wrote, then the executable:

1. **Stop what is running.** `codegraph http list` shows background HTTP servers with their log files. Note the log
   path, stop the server with `codegraph http stop <address>`, then delete the log; the stopped server is no longer
   listed. Quit the agents and editors that use CodeGraph, which ends their MCP servers.
2. **Remove the agent configuration.** `codegraph uninstall` removes the CodeGraph entry from your agents' configs (add
   `--local` for configs inside a project), and `codegraph skill uninstall` removes the skill if you installed it.
3. **Remove each index.** `codegraph uninit --force <project>` stops that project's background process and deletes
   its database and settings. The `.codegraph/` directory stays behind with a few state files and any viewer trails
   you saved; delete the directory to leave nothing.
4. **Remove shell completions**, if you installed them with `codegraph completions <shell> --install`. Delete the
   completion file; for zsh and elvish, also the line you added to your shell configuration; for PowerShell, also the
   line it added to `Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1` in your user folder (or to the
   file `CODEGRAPH_PS_PROFILE` named). The
   [completions reference](../../../cli.md#codegraph-completions--shell-completions) names the files.
5. **Remove files you asked for**: diagnostic logs written with `--debug-log <FILE>` and graphs written with
   `codegraph export -o <FILE>` stay where you put them.
6. **Delete the executable**: `$HOME/.local/bin/codegraph`, or `codegraph.exe` in `%LOCALAPPDATA%\Programs\codegraph`
   on Windows, or the directory you chose with `CODEGRAPH_INSTALL_DIR`. With `cargo install`, run
   `cargo uninstall codegraph-rs`. On Windows, also remove that directory from your user `PATH`.

Neither `uninstall` nor `uninit` removes the executable. [Data and network](../privacy.md) lists everything CodeGraph
writes, including the small registry files running MCP servers keep in your user state directory.
