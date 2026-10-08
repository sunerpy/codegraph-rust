# 常见问题

本页回答刚开始使用 CodeGraph 时最常遇到的问题。

## CodeGraph 用了 AI 吗？会把我的代码发到别处吗？

没有，也不会。CodeGraph 不包含任何模型，索引和查询完全在你的电脑上进行。只有安装脚本和 `codegraph self-update` 会联网，用于从 GitHub 下载发布版本。[数据与网络](../privacy.md)列出了 CodeGraph 读取、写入和连接的全部内容。

## 要把 `.codegraph/` 提交到仓库吗？

不需要。它是本地缓存，随时可以从源码重建，内容也与所在机器有关。新建的索引目录包含嵌套的 `.gitignore`，无需修改项目根目录的 `.gitignore`，也不会出现在 `git status` 中。如果索引目录原本已经存在，CodeGraph 会保持原样；该目录仍可见时，请自行把它加入项目的 `.gitignore`。

## 代码里明明有的调用，为什么查不到？

通常是以下原因之一：

- 调用要到运行时才能确定：回调、反射、由字符串拼出的名称；
- 名称能匹配到多个定义，而上下文无法判断是哪一个；
- 文件所用的语言 CodeGraph 不能解析，或者被 `.gitignore`、`config.toml` 排除，或者大于 `indexing.max_file_size`；
- 文件刚修改，尚未重新索引：`codegraph status` 会显示待处理的变化。

`codegraph node <名称>` 会显示索引中关于某个符号的全部信息，可以据此判断是哪种情况。

## 提示索引属于另一个文件系统位置

项目连同 `.codegraph/` 目录一起被移动或复制了。索引与建立它的位置绑定，CodeGraph 不会在别处使用它，以免回答的是错误的文件。请在新位置运行 `codegraph init <项目>` 替换它。

## 提示索引由更新的 CodeGraph 版本建立

这份索引由较新的版本建立，而读取它的 CodeGraph 版本较旧。CodeGraph 不会猜测它不认识的格式。请用 `codegraph self-update` 或安装脚本更新所有副本，包括 Agent 或编辑器启动的那一个；更新后，重启已经在运行的 Agent 和编辑器。

## Agent 每次调用都要求提供 `projectPath`

MCP 服务没有找到可以默认使用的项目。请用 `codegraph init` 为项目建立索引，在项目目录中启动 Agent，或在 Agent 的配置中用 `-p <项目>` 固定项目。[接入编码 Agent](../guide/agents.md#服务为哪个项目作答)

## 命令提示有锁，或者长时间没有反应

可能有另一个 CodeGraph 进程正在写入同一份索引，请等待它完成。如果有进程崩溃后留下了锁，`codegraph unlock <项目>` 会移除它，仍在运行的进程的锁不受影响。索引速度慢时，`codegraph index --debug-log index.jsonl` 会记录每个文件的耗时，日志的含义见[故障排查](../../troubleshooting.md)。

## 能在 Windows 和 WSL 中使用吗？

可以。Windows 有 x86_64 和 ARM64 两个版本。在 WSL 中，位于 Windows 驱动器上（`/mnt/c/…`）的项目，新建的索引会放在单独的 `.codegraph-wsl/` 目录中，因为 SQLite 的锁在 Windows 与 WSL 之间无法生效；该位置已有的 `.codegraph/` 索引会继续使用。`/mnt/` 下不会监听文件。

## 如何报告问题？

在 [GitHub](https://github.com/sunerpy/codegraph-rust/issues) 上提交 issue，附上版本号（`codegraph --version`）、平台、所用命令及其输出。索引相关的问题请附上[故障排查](../../troubleshooting.md)中介绍的诊断日志。日志记录的是项目内的相对路径和耗时，不包含源码文本、文件内容和环境变量。
