# 保持索引最新

本页说明索引如何跟上你的编辑、如何手动更新，以及如何在 CI 中使用 CodeGraph。

## 在后台自动更新

Agent 或编辑器为已建立索引的项目启动 `codegraph serve --mcp` 时，CodeGraph 会为该项目启动一个后台进程，或加入已经在运行的那个。它监听项目文件，重新索引发生变化的文件；为了把连续多次保存合并成一次更新，它会先等待片刻（默认两秒）。同一项目上的所有 Agent、编辑器和终端共用这个进程，最后一个使用者断开几分钟后，它会自行退出。

监听会跳过索引同样跳过的目录（`.gitignore` 中的内容、`node_modules`、`target` 以及其他默认目录），因此庞大的依赖目录不会带来额外开销。修改 `.codegraph/config.toml`、`.codegraph/codegraph.json` 或根目录的 `.gitignore` 后无需重启即可生效。

索引大约比保存晚一秒。在这段时间里，工具读到的文件可能与索引中记录的行号已经对不上。CodeGraph 在展示任何文件的源码之前都会检查该文件，发生变化的文件要么完整展示，要么不展示，绝不会按过期的行号截取。回答会说明哪些文件受到了影响。

在无法可靠监听的地方，监听会自动关闭：WSL 中 `/mnt/` 下的项目，以及项目根目录恰好是你的主目录或文件系统根目录的情况。

## 手动更新

```sh
codegraph sync .            # 重新索引发生变化的文件，移除已删除的文件
codegraph status .          # 索引是否最新，以及之后有哪些变化
codegraph index --force .   # 从头重建，仅在命令要求时使用
```

`sync` 把每个文件的大小和修改时间与索引对比，只重新读取发生变化的文件，并更新这些变化可能影响到的所有引用。它的结果与完整重建完全一致，项目的测试逐字节检查这一点。在 Git 仓库中，只要 Git 的信息可信，`status` 就会借助 Git 找出变化的文件，因此在大型仓库中也很快。

## 在 CI 和脚本中使用

```sh
export CODEGRAPH_NO_DAEMON=1        # 保持在前台运行，不启动后台进程
codegraph init .                    # 已有缓存的索引时改用 `codegraph sync .`
codegraph affected src/pricing.ts -p . --filter 'tests/*'
```

`affected` 接受一组改动过的文件，列出依赖它们的文件（逐层传递）以及其中的测试文件，流水线因此可以只运行改动可能影响到的测试。`--depth` 限制沿依赖方向追踪的层数。

设置 `CODEGRAPH_NO_DAEMON=1` 后，`serve --mcp` 同样在前台运行。同一时间只能有一个这样的服务负责更新某个项目的索引，第二个会退出并指明第一个是谁。`serve --no-watch` 保留服务启动时的那次追赶更新，但之后不再监听。

## 遇到问题时

- `codegraph status .` 会说明它无法使用索引的原因，并给出修复命令。
- `codegraph unlock .` 移除崩溃的进程留下的锁，不会动仍在运行的进程的锁。
- [CLI 参考](../../cli.md#daemon-watch--environment-variables)列出了调整后台进程的全部环境变量，[故障排查](../../troubleshooting.md)说明如何记录诊断日志。
