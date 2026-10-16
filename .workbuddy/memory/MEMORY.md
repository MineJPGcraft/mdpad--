# 项目长期记忆 — MarkdownPad

## 硬性约束（用户明确要求，不可违背）

1. **纯 Rust GUI**。用户否决了 Tauri/WebView 方案，明确要求「直接纯 Rust GUI」，
   并提到「某些付费软件做的也没那么好看」——即视觉效果不是妥协的理由。
2. **完全离线渲染**。用户否决了远端渲染（kroki.io / plantuml.com）。
   Mermaid 官方渲染器依赖 Chromium、PlantUML 是 Java，纯 Rust 下无法调用，
   因此两者都是**自研解析器 + 自研布局 + egui 图元绘制**。
3. **单栏实时渲染，不要预览分栏**。用户原话：「我不要点预览，我就能直接看到，
   边编辑边看到」。这决定了核心架构——单文档视图，编辑与成品同屏。
4. 右下角必须有「源代码模式」开关，点击切换看源码、再点切回渲染。

## 关键架构决策

**编辑对象就是 Markdown 源码本身**，不维护隐藏 AST。
- 好处：源码/渲染模式切换零损耗，不会走形，删字符就是删源码字符。
- 代价：渲染层必须自建光标 ↔ 字节 ↔ 屏幕位置的双向映射，并自己处理
  语法标记的显隐。

**引擎层零 egui 依赖**。`src/gfx.rs` 定义 `Prim` / `Measure` / `Rgba` /
`Rect` / `Font` 抽象；egui 只出现在 `gfx_egui.rs`（绘制桥接）、`edit.rs`
（控件与光标）、`app.rs`（外壳）三处。好处：LaTeX 引擎与图表引擎可脱离
GUI 独立测试。扩展渲染能力的常规路径是 gfx 加图元 → 引擎产出图元 →
edit.rs 塞进块，引擎层不动。

## egui 0.37 的 API 变化（踩过的坑）

0.37 是大版本重构，从旧版本迁移时必须注意：
- `TopBottomPanel` / `SidePanel` / `CentralPanel` **已移除**，统一为
  `Panel::top(..)` / `bottom(..)` / `left(..)` / `right(..)`，且接收 `&mut Ui`
  而非 `&Context`。
- `eframe::App::update` 改为 `App::ui`。
- `ctx.fonts(|f| ...)` 闭包式字体访问取代了旧的 `FontsView` 可变借用。
- `LayoutJob.text` 是 `ByteIndexMap`，取值要用 `job.text.get(range)`，
  不能当 `String` 用。
- `TextEditState` 在 `egui::text_edit` 命名空间，光标通过
  `state.cursor.char_range()` 取 `CCursorRange`（`CharIndex(usize)`）。

## 字体

从系统直取，关键是 `.ttc` 集合文件必须给对 face index：
- `cambria.ttc` **索引 1 = Cambria Math**（LaTeX 引擎依赖它）
- `msyh.ttc` 索引 0 = 微软雅黑（CJK）
- `consola.ttf` / `segoeui.ttf` 单字面，索引 0

## 测试

209 个单元测试，`cargo test` 全绿。无头渲染测试必须用
`output.drop_without_applying_deltas()` 处理纹理增量，否则 egui 会报
纹理错误——这是官方测试辅助函数的用法，不是 bug。

给"画出来的东西"写断言时，基准只能取自 `FullOutput.shapes` 里真实存在的图元
（文字 galley 的 `pos`/`rect`、背景矩形的边），不能拿被测公式反推，否则会写出
自我循环、永远通过的假测试。`Rect` / `Circle` / `LineSegment` 是不同的 shape
变体，表格的竖分隔线在窄布局下会被裁掉，用背景矩形更稳。

## 不留假开关

设置项必须真的被渲染层读到。历史上 `show_gutter` 在设置里有复选框、有 `Config`
字段、还被塞进 `RenderOpts`，但渲染层从未读取——点它没有任何反应。新增设置项时
必须同时在渲染层落点，并确认缓存键覆盖了它（图表字号踩过：改了设置仍显示旧图）。

## 构建注意

`[profile.dev.package."*"] opt-level = 2` 不可去掉：字形整形与图表布局在
opt-level 0 下慢到不可用，dev 构建体验会崩。