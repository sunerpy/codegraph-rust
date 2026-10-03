# 浏览器查看器 <StatusTag status="preview" />

本页说明如何打开 CodeGraph 的浏览器查看器，以及每个视图的用途。

查看器读取项目已有的索引，在你本机的浏览器中展示：一个符号的调用方、源码和被调用方并排显示，一个文件的大纲，一个函数到另一个函数的调用路径，按模块划分的仓库结构图，类型的继承层级，以及没有任何代码到达的符号。它目前是**预览**功能：默认关闭，需要手动开启，各视图今后仍可能调整。查看器界面只有英文。

## 打开查看器

```sh
CODEGRAPH_UI=1 codegraph ui                # 当前所在的已建立索引的项目
CODEGRAPH_UI=1 codegraph ui ~/code/my-app  # 另一个已建立索引的项目
CODEGRAPH_UI=1 codegraph ui --no-open      # 只打印地址，不打开浏览器
CODEGRAPH_UI=1 codegraph ui --read-only    # 同时拒绝保存 Trail
```

未设置 `CODEGRAPH_UI=1` 时，这个命令会被拒绝，也不会出现在 `--help` 中。查看器监听 `127.0.0.1`，使用 4747 端口或其后第一个空闲端口（`--port` 可以指定），按 `Ctrl+C` 停止。

它从不建立或修改索引：请先用 `codegraph init` 为项目建立索引。它唯一会写入的内容是你主动保存的 **Trail**（在代码中的一段浏览路径），保存在 `.codegraph/ui/trails/` 下。

在任何页面按 `Ctrl+K`（macOS 上是 `⌘K`）都可以搜索符号或文件，也可以直接询问路径，例如 `how does checkout reach tax`。

## Symbol（符号）

<ScreenFigure src="/screens/viewer-symbol-light.webp" dark="/screens/viewer-symbol-dark.webp" width="1440" height="900" alt="方法 IndexPaths::resolve 的 Symbol 视图：左侧是调用方，中间是标出每处调用的源码，右侧是它调用的函数。" />

中间一栏是符号的源码，它发出的每个调用都在页边标出，并连到右侧一栏，右侧列出它调用的内容。左侧一栏按文件分组列出调用方，下方的 **Blast radius**（影响范围）统计改动会波及的范围：直接调用方、三层以内的调用方，以及其中的生产代码文件和测试文件。点击任何名称都会打开对应的符号，顶部的 Trail 栏记录你走过的路径。

## Type hierarchy（类型层级）

<ScreenFigure src="/screens/viewer-hierarchy-light.webp" dark="/screens/viewer-hierarchy-dark.webp" width="1440" height="900" alt="trait FrameworkResolver 的 Symbol 视图，类型层级中列出实现它的八个类型。" />

对于类、接口或 trait，Symbol 视图会增加它的层级：上方是它继承和实现的类型，下方是继承或实现它的类型。经由这类类型分发到多个实现的调用会被标出，因为它没有唯一的静态目标。

## File（文件）

<ScreenFigure src="/screens/viewer-file-light.webp" dark="/screens/viewer-file-dark.webp" width="1440" height="900" alt="index_paths.rs 的 File 视图：左侧是导入它的文件，中间是带有引入和引出数量的大纲，右侧是它自己的导入。" />

文件大纲按源码顺序列出其中的符号，以及每个符号被引用和引用外部的数量。两侧分别是导入它的文件和它导入的文件；索引之外的名称（例如标准库路径）会列出，但没有链接。**Whole-file source**（整个文件）按行显示文件内容及其中的调用。

## Flow（调用路径）

<ScreenFigure src="/screens/viewer-flow-light.webp" dark="/screens/viewer-flow-dark.webp" width="1440" height="900" alt="从 cmd_explore 到 explore_file_header 的 Flow 视图：五张卡片，每张对应一次调用，并在发出下一次调用的那一行打开；下方的表格列出每一步及其置信度。" />

Flow 回答「A 是怎样调用到 B 的」：每张卡片是一步，在发出下一次调用的那一行打开，下方的表格列出每一步的调用位置和匹配置信度。这张截图展示的是 CodeGraph 自身的 `codegraph explore` 命令调用到同时为 MCP 工具服务的引擎。如果两者之间没有调用链，视图会如实说明，并列出常见原因，例如回调或反射调用。

## Map（结构图）

<ScreenFigure src="/screens/viewer-map-light.webp" dark="/screens/viewer-map-dark.webp" width="1440" height="900" alt="crates 目录的结构图：每个 crate 是一个方框，标有符号数和文件数，按依赖关系分层，每个 crate 位于它所依赖的 crate 之上。" />

结构图把文件归入模块，并按依赖关系排列：每个模块位于它所依赖的模块之上一层，因此基础模块在最下方，入口在最上方。连线的粗细表示两个模块之间的调用、导入和类型引用的数量。可以在右侧选择目录和分组深度；**Copy image** 和 **Download SVG** 按当前显示导出结构图。

## Dead code（未被到达的代码）

<ScreenFigure src="/screens/viewer-dead-light.webp" dark="/screens/viewer-dead-dark.webp" width="1440" height="900" alt="Dead code 视图：列出索引中没有任何代码到达的符号；右侧说明有多少没有被引用的符号未列入清单，以及原因。" />

Dead code 列出索引中没有任何代码到达的符号，按大小排列。右侧面板说明清单排除了哪些符号以及原因，例如导出的符号、测试文件中的符号、组件文件中可能被模板引用的符号等。剩下的符号没有静态引用，但这并不能证明它们没有被使用：宏、trait 对象和反射仍可能到达它们。

## Start、Entry points、Screens 和 Steps

Start 页面概括整个索引：索引包含的内容、被依赖最多的符号、执行的起点，以及覆盖范围最广的测试。**Entry points** 完整列出这些起点，已保存的 Trail 排在最前。

**Screens** 和 **Steps** 需要当前版本尚未记录的信息：Screens 会显示没有找到页面跳转，Steps 会说明无法绘制步骤。[查看器参考](../../ui.md#differences-from-upstream)列出了尚未支持的内容。

## 主题

查看器跟随系统的浅色或深色设置，也可以用左侧栏底部的主题控件手动选择。窗口宽度小于 600 px 时，底部标签栏取代左侧栏，Symbol 视图一次只显示一个窗格。

## 安全边界

查看器只监听 `127.0.0.1`。`Host` 或 `Origin` 不属于它自己的请求，以及缺少专用请求头的写入请求，都会被拒绝。它只打开索引中记录的、位于项目内的文件。[查看器参考](../../ui.md#boundary)列出了全部检查。
