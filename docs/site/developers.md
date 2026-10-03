# 参与开发

本页面向希望从源码构建、修改 CodeGraph 或改进本站点的开发者。

CodeGraph 以 MIT 许可开源，代码、问题和拉取请求都在 [GitHub](https://github.com/sunerpy/codegraph-rust) 上。它是 TypeScript 项目 [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph) 的 Rust 移植版本，独立开发并持续跟进上游；[上游同步记录](https://github.com/sunerpy/codegraph-rust/blob/main/docs/upstream-sync/UPSTREAM.md)列出了已移植的内容。

## 构建与测试

```sh
git clone https://github.com/sunerpy/codegraph-rust
cd codegraph-rust
cargo build --release      # target/release/codegraph
make check                 # 格式、lint、测试和防护检查：与 CI 相同的门禁
make pre-ci                # make check，外加查看器前端检查和发布包冒烟测试
```

Rust 版本固定在 `rust-toolchain.toml` 中；编译 SQLite 和各语言的语法需要 C 编译器。`ui/` 中的查看器前端只有在重新构建时才需要 Node 和 npm；构建产物已提交到仓库，因此 `cargo build` 从不运行 Node。

## 代码组织

工作区由一组 crate 组成，依赖方向只有一个：从共享类型一直到命令行。

| Crate               | 负责                                  |
| ------------------- | ------------------------------------- |
| `codegraph-core`    | 共享类型、配置、节点 ID、日志         |
| `codegraph-extract` | 语言识别，以及基于 tree-sitter 的提取 |
| `codegraph-store`   | SQLite 表结构、迁移、全文检索和查询   |
| `codegraph-resolve` | 导入和名称匹配，以及框架解析器        |
| `codegraph-graph`   | 图遍历、影响范围、搜索排序            |
| `codegraph-mcp`     | MCP 服务及其工具                      |
| `codegraph-watch`   | 增量同步和文件监听                    |
| `codegraph-daemon`  | 后台进程及其锁和登记                  |
| `codegraph-ui`      | 浏览器查看器的服务端和内嵌前端        |
| `codegraph-cli`     | `codegraph` 命令和 Agent 安装器       |
| `codegraph-bench`   | 等价性校验和基准测试，不随版本发布    |

[架构](../architecture.md)和[数据模型](../data-model.md)两页说明了从文件到答案的流程以及其中用到的表。

## 每次改动都要保证

- **输出确定**：同样的源码和配置得到同样的索引，`sync` 与完整重建结果一致。
- **golden 逐字节稳定**：提取结果与参考产物逐字节比对；有意改变输出时，按[等价性说明](../equivalence.md)重新生成。
- **不含模型**：构建中不能引入任何 AI、embedding 或向量相关的依赖，有专门的防护脚本检查。
- **出错即停止**：有歧义或不安全的答案保持未解析或直接拒绝。

完整规则（包括提交信息和发布流程）见[贡献者约定](https://github.com/sunerpy/codegraph-rust/blob/main/AGENTS.md)和 [CONTRIBUTING.md](https://github.com/sunerpy/codegraph-rust/blob/main/CONTRIBUTING.md)。

## 修改本站点

本站点的文字和截图保存在仓库的 `docs/site/` 下，与它们描述的代码放在一起，因此一个改变界面的拉取请求可以同时更新页面。站点本身（主题、组件和发布流程）位于 [sunerpy/firlab](https://github.com/sunerpy/firlab)。`docs/site/README.md` 说明了如何预览改动、写作规则以及截图方法。
