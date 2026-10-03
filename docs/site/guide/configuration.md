# 配置

本页列出影响 CodeGraph 索引范围和结果排序的设置。

CodeGraph 不需要任何配置即可使用。设置保存在项目 `.codegraph/` 目录下的两个可选文件中，另有几个环境变量用于调整后台进程。

## `.codegraph/config.toml`

```toml
[app]
name = "my-project"

[indexing]
exclude = ["static/", "docs/generated/"]
deprioritize = ["vendor/**"]
```

这个文件存在时必须包含 `[app]` 表和其中的 `name`，否则所有命令都会以 `missing field` 错误停止。其余设置都是可选的：

| 设置                     | 默认值                                                           | 作用                                                                                                                                                             |
| ------------------------ | ---------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `app.name`               | —                                                                | 项目名称。                                                                                                                                                       |
| `app.log_level`          | `info`                                                           | CodeGraph 向标准错误输出日志的详细程度。                                                                                                                         |
| `indexing.exclude`       | 无                                                               | 不纳入索引的路径，写法与 `.gitignore` 规则相同（`static/`、`gen*`）。                                                                                            |
| `indexing.include`       | 无                                                               | 即使被 `.gitignore` 排除也要索引的路径，适用于由另一套版本控制系统管理、未纳入 Git 的源码。`exclude` 的优先级更高，`node_modules` 等依赖目录始终不会被重新纳入。 |
| `indexing.ignore_dirs`   | `node_modules`、`target`、`dist`、`.venv` 等依赖、构建和缓存目录 | 在任意层级跳过的目录名。在这里写入的列表会**替换**默认列表，需要保留的默认目录请一并写上。                                                                       |
| `indexing.ignore_paths`  | Android 的 `res/` 资源目录                                       | 默认跳过的路径规则，写法与 `.gitignore` 相同。                                                                                                                   |
| `indexing.max_file_size` | `1048576`（1 MiB）                                               | 超过此大小的文件只记录，不解析。                                                                                                                                 |
| `indexing.deprioritize`  | 无                                                               | 仍然索引，但在 `search` 和 `explore` 中排在你自己的代码之后的路径。                                                                                              |
| `watch.enabled`          | `true`                                                           | 后台进程是否监听文件。                                                                                                                                           |
| `watch.debounce_ms`      | `2000`                                                           | 监听等待一连串变化平息的时间。                                                                                                                                   |

项目的 `.gitignore` 同样生效：Git 忽略的文件，CodeGraph 不会索引。修改 `config.toml` 或根目录的 `.gitignore` 后无需重启即可生效；如果没有后台进程在运行，请执行 `codegraph sync`。

## `.codegraph/codegraph.json`

把 CodeGraph 不认识的文件扩展名映射到它能解析的语言：

```json
{
  "extensions": {
    ".blade": "php",
    ".x": "lua"
  }
}
```

扩展名匹配时不区分大小写，开头的点可有可无。CodeGraph 不认识的语言名称会被跳过；文件格式错误时会被忽略，并在日志中记录错误，不会中断索引。[CLI 参考](../../cli.md#custom-extension-mapping-codegraphcodegraphjson)

## 环境变量

| 变量                               | 用途                                           |
| ---------------------------------- | ---------------------------------------------- |
| `CODEGRAPH_NO_DAEMON=1`            | 在前台运行，不启动后台进程，适用于 CI 和脚本。 |
| `CODEGRAPH_NO_WATCH=1`             | 保留后台进程，但不再监听文件。                 |
| `CODEGRAPH_WATCH_DEBOUNCE_MS`      | 监听的等待时间，单位为毫秒。                   |
| `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` | 最后一个使用者离开后，后台进程继续保留的时间。 |
| `CODEGRAPH_MCP_TOOLS`              | Agent 工具列表中显示哪些 MCP 工具。            |
| `CODEGRAPH_DIR`                    | 在项目中改用其他目录名代替 `.codegraph`。      |
| `CODEGRAPH_UI=1`                   | 开启浏览器查看器（预览）。                     |

完整列表、取值范围和默认值见 [CLI 参考](../../cli.md#environment-variable-reference)。
