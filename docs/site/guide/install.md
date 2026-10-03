# 安装

本页介绍如何安装 CodeGraph、保持更新，以及如何完整卸载。

CodeGraph 是一个名为 `codegraph` 的可执行文件。Linux、macOS 和 Windows 的版本发布在 [GitHub Releases](https://github.com/sunerpy/codegraph-rust/releases)，没有发布到 crates.io。

## Linux 和 macOS

```sh
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
```

脚本会选择适合当前平台的压缩包，只有在压缩包的 SHA-256 与该版本 `SHA256SUMS` 中的记录一致时才继续，然后把 `codegraph` 安装到 `$HOME/.local/bin`。脚本需要 `curl` 或 `wget`、`tar`，以及 `sha256sum` 或 `shasum`。它不会修改 shell 配置文件：如果该目录不在 `PATH` 中，脚本会提示，由你自行添加。

设置 `CODEGRAPH_INSTALL_DIR` 可以安装到其他目录。

## Windows

在 PowerShell 5.1 或更高版本中运行：

```powershell
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex
```

脚本以同样的方式校验压缩包，把 `codegraph.exe` 安装到 `%LOCALAPPDATA%\Programs\codegraph`。如果该目录还不在**用户** `PATH` 中，脚本会把它加进去；重新打开终端后生效。这里同样可以用 `CODEGRAPH_INSTALL_DIR` 指定目录。

在 Git Bash、MSYS2 或 Cygwin 中请使用 PowerShell 脚本：shell 脚本会停止并提示改用它。

## 安装指定版本

两个脚本默认安装最新版本，设置 `CODEGRAPH_VERSION` 可以指定版本。需要可复现的安装时，请从同一个标签获取脚本：

```sh
curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/vX.Y.Z/scripts/install.sh \
  | CODEGRAPH_VERSION=vX.Y.Z sh
```

```powershell
$env:CODEGRAPH_VERSION = "vX.Y.Z"
irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/vX.Y.Z/scripts/install.ps1 | iex
```

## 自行下载压缩包

每个版本包含六个压缩包，每个平台一个：

| 平台                | Target                       | 压缩包    |
| ------------------- | ---------------------------- | --------- |
| Linux x86_64        | `x86_64-unknown-linux-musl`  | `.tar.gz` |
| Linux ARM64         | `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS Intel         | `x86_64-apple-darwin`        | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin`       | `.tar.gz` |
| Windows x86_64      | `x86_64-pc-windows-msvc`     | `.zip`    |
| Windows ARM64       | `aarch64-pc-windows-msvc`    | `.zip`    |

Linux 版本是静态链接的，不依赖任何系统库。解压后把 `codegraph` 放到 `PATH` 中即可。

每个版本还附带 `SHA256SUMS`，每个压缩包都有构建来源证明（attestation）。校验方法：

```sh
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify codegraph-X.Y.Z-x86_64-unknown-linux-musl.tar.gz \
  --repo sunerpy/codegraph-rust \
  --signer-workflow sunerpy/codegraph-rust/.github/workflows/release.yml \
  --deny-self-hosted-runners
```

## 从源码构建

需要 Rust 工具链和 C 编译器（SQLite 和各语言的语法都从源码编译）：

```sh
cargo install --locked --git https://github.com/sunerpy/codegraph-rust codegraph-rs
```

包名是 `codegraph-rs`，安装的命令是 `codegraph`。仓库在 `rust-toolchain.toml` 中固定了测试所用的 Rust 版本。

## 检查安装

```sh
codegraph --version
```

## 更新

```sh
codegraph self-update              # 最新版本
codegraph self-update --check      # 只检查是否有新版本
codegraph self-update --tag vX.Y.Z # 指定版本
```

`self-update` 从 GitHub Releases 下载适合当前平台的压缩包，校验后替换正在运行的可执行文件。如果 `codegraph` 所在目录需要更高权限才能写入，请以相应权限运行。

新版本可能会改变索引的构建方式。这种情况下，`codegraph status` 会报告索引已过期，运行 `codegraph sync` 即可重建。只有在命令明确提示索引需要恢复时，才使用 `codegraph index --force`。详见 [CLI 参考](../../cli.md#extraction-version-upgrades)。

## 卸载

先移除 CodeGraph 写入的内容，再删除可执行文件：

1. **停止正在运行的服务**：`codegraph http list` 列出后台 HTTP 服务及其日志文件。先记下日志路径，再用 `codegraph http stop <地址>` 停止服务，然后删除该日志；服务停止后不会再出现在列表中。退出使用 CodeGraph 的 Agent 和编辑器，它们的 MCP 服务会随之结束。
2. **移除 Agent 配置**：`codegraph uninstall` 从各 Agent 的配置中移除 CodeGraph 条目（项目内的配置加 `--local`）；如果安装过 Skill，运行 `codegraph skill uninstall`。
3. **移除每个项目的索引**：`codegraph uninit --force <项目>` 会停止该项目的后台进程，删除数据库和设置。之后 `.codegraph/` 目录仍会保留几个状态文件以及你在查看器中保存的 Trail；删除这个目录即可不留任何内容。
4. **移除 shell 补全**：如果用 `codegraph completions <shell> --install` 安装过补全，删除补全文件；zsh 和 elvish 还要删除你添加到 shell 配置中的那一行；PowerShell 还要删除它添加到用户目录下 `Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1`（或 `CODEGRAPH_PS_PROFILE` 指定的文件）中的那一行。[补全参考](../../cli.md#codegraph-completions--shell-completions)列出了这些文件。
5. **删除你指定位置的文件**：用 `--debug-log <文件>` 写出的诊断日志和用 `codegraph export -o <文件>` 导出的图，都保留在你指定的位置。
6. **删除可执行文件**：`$HOME/.local/bin/codegraph`，Windows 上是 `%LOCALAPPDATA%\Programs\codegraph` 中的 `codegraph.exe`，或你用 `CODEGRAPH_INSTALL_DIR` 指定的目录。通过 `cargo install` 安装的，运行 `cargo uninstall codegraph-rs`。Windows 上还要从用户 `PATH` 中移除该目录。

`uninstall` 和 `uninit` 都不会删除可执行文件。[数据与网络](../privacy.md)列出了 CodeGraph 写入的全部内容，包括运行中的 MCP 服务在用户状态目录中保留的小型登记文件。
