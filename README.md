# MarkdownPad

纯 Rust 实现的离线所见即所得 Markdown 编辑器，基于 egui 0.37。
**单栏实时渲染——写的时候就是成品的样子，不需要点预览。**

---

## 运行

```bash
cargo run --release
```

或直接运行 `target/release/mdpad--.exe`。首次启动会自动载入一份示例文档，
覆盖全部渲染特性，可直接用来检验效果。

开发期用 `cargo run`（依赖已配置 `opt-level = 2`，否则字形整形与图表布局会慢到不可用）。

```bash
cargo test    # 单元测试与无窗口编辑回归测试
```

---

## 核心特性

### 所见即所得，但不是所见即所得编辑器

大多数 WYSIWYG 编辑器在内存里维护一棵 AST，导出时才序列化成 Markdown，
于是「编辑的东西」和「存下来的东西」并不完全一致，源码往返容易走形。

MarkdownPad 反过来：**你编辑的始终是 Markdown 源码本身**，没有任何隐藏的
AST。渲染层是一个实时视图——它解析源码、决定每段怎么显示，并管理光标与
选区在源码字节和屏幕位置之间的双向映射。

所以：

- 源码模式与渲染模式切换的是**同一份字符串**，不会丢内容、不会走形。
- 在渲染视图里按退格删掉的，就是源码里那几个字符。
- 任何无法解析的写法都不会被"悄悄改写"，它会原样留在文档里。

代价是渲染层必须自己解决所见即所得编辑的核心难题：
语法标记的隐藏与显形。下面就是这一部分。

### 语法标记的自动隐藏

行内扫描器（`src/doc/inline.rs`）把一行拆成**逐字节连续的样式片段**，
同时记录每个语法构造的定界符范围（`*` 的位置、`[text](url)` 里方括号与
圆括号的边界，等等）。

渲染时按光标位置决定：

- 光标**不在**这个构造内 → 只画内容，`**`、`[]()`、`$` 全部隐藏。
- 光标**在**这个构造内 → 定界符以淡色显形，方便定位和编辑。

于是平时看到的是干净的成品，光标一靠近，语法就浮出来。这个切换是逐
构造独立的，同一段里两个强调不会一起显形。

### 源码模式

右下角的按钮一键切换。源码侧仍带语法高亮（标题、强调、代码围栏、链接、
行内公式、图表围栏），并且与渲染视图共享同一份文本与光标。

### 查找与替换

`Ctrl+F` 打开查找栏，`Ctrl+H` 连同替换框一起展开。查找栏是一条独立的顶部
面板，不浮在正文上，也不会挡住它正在指向的那一行。

- 匹配在**源码字节**层面完成，结果直接交给块定位逻辑，所以中文、emoji、
  中英混排的偏移都不会错位。
- 命中后光标会落进那个块并**选中**该词——接着打字就是改写它；跳转前先提交
  正在编辑的块，避免拿旧偏移去定位。
- `Enter` / `Shift+Enter` 在命中之间循环，`Aa` 切换是否区分大小写。
- 「替换」只改当前命中并跳到下一处；「全部替换」一次性重写整篇文档，因此
  **整批替换只是一步撤销**。
- 源码模式下没有块，整个文档就是一个文本框，命中直接在其中选中。
- 少数块（分割线、原始 HTML 卡片）只绘制、没有文本控件。命中它们时会被
  跳过并给出提示，而不是把一个永远没人消费的聚焦请求留在队列里。

大小写折叠只作用于 ASCII：Unicode 折叠会改变字符的字节长度（`İ` 折叠后是
两个字符），那样每个后续偏移都会错位，替换时就会切坏文本。中文没有大小写，
不受影响。

### 草稿与崩溃恢复

编辑器从不自己写用户的文件——「保存」是显式动作。但进程被杀掉（panic、
断电、强制重启）会连同未保存的内容一起丢失，所以文档变脏之后，正文会每隔
数秒镜像到配置目录下的 `draft.json`。

**草稿永远不会自动应用。** 下次启动如果存在草稿，状态栏会多出
「恢复草稿（N 分钟前）」与「丢弃」两个按钮：点前者才替换当前文档，且这一步
可以撤销；点后者直接删除草稿。保存成功、新建、正常退出都会清理草稿。

这条约束来自一次真实事故：跨启动恢复「不可验证的持久状态」曾经导致整张纸
空白。能恢复的东西必须是用户看得见、也能拒绝的。

---

## 渲染能力（全部本地实现）

没有任何网络请求，没有外部渲染器，没有 Java 运行时。断网可用。

### LaTeX

Rust 生态没有成熟的 LaTeX 排版库，所以排版引擎是自己写的
（`src/tex/`，约 4900 行）。基线驱动的布局盒模型，全部字形宽度通过字体
度量接口取得，没有硬编码。

支持：

| 类别 | 内容 |
|---|---|
| 希腊字母 | `\alpha` … `\omega`，含 `\varepsilon` `\vartheta` `\varphi` `\varpi` `\digamma` 等异体 |
| 关系符 | `\leq \geq \neq \equiv \approx \cong \propto \preceq \in \notin \vdash \models \lesssim \coloneqq \therefore \implies \iff` … |
| 运算符 | `\pm \times \div \cdot \oplus \otimes \ominus \uplus \sqcup \setminus \circ \bullet` 及各类大算符 |
| 上下标 | `^` `_` 任意次序与嵌套；`\displaystyle` 下大算符取上下限，支持 `\limits` / `\nolimits` 覆盖；**积分默认上下限在右侧**（TeX 的 `\nolimits` 惯例） |
| 堆叠 | `\overset` `\underset` `\stackrel`（按关系符排间距）、`\substack`（多行上下限）、`\overbrace` / `\underbrace`（花括号 + 标注） |
| 分式 | `\frac` `\dfrac` `\tfrac`，分子分母上下居中于横线；`\binom` |
| 根式 | `\sqrt{x}` `\sqrt[n]{x}`，根号随内容伸缩 |
| 定界符 | `\left…\right` 覆盖 `()[]{}||\langle\rangle` 上下限号族；另有 `\big` 系列 |
| 环境 | `matrix pmatrix bmatrix vmatrix Bmatrix cases aligned align gathered array`，另含 `split` `alignedat` `gather*` `multline` `eqnarray` `dcases` 等同构别名；支持 `&` 与 `\\`、`\\[6pt]` 行距 |
| array 列格式 | `lcr`、竖线 `|`、`p{2cm}` / `m{}` / `b{}` 最小列宽、`@{}` 抑制列间距、`*{3}{c}` 重复；行级 `\hline` / `\cline` |
| 样式 | `\text \textbf \textit \textrm \textsf \texttt \mathrm \mathbf \boldsymbol \mathit \mathsf \mathtt \mathbb \mathcal \mathscr \mathfrak \operatorname \operatorname*` |
| 颜色 | `\color{red}` 作用到组结束，`\textcolor{#c0392b}{…}`；支持常用色名与 `#rgb` / `#rrggbb` |
| 重音 | `\hat \widehat \bar \overline \tilde \widetilde \dot \ddot \vec \check \breve \acute \grave \mathring \underline`，**全部按几何绘制**，不依赖字体里组合符号的墨迹位置 |
| 取反与框 | `\not`（叠加小斜线）、`\boxed` / `\fbox` |
| 间距 | `\, \; \: \! \quad \qquad \enspace \enskip \negthinspace \negmedspace \negthickspace \medspace \thickspace \mkern{n mu} \mskip \hspace{1em} \hspace*{1em}`，按 TeX 规则区分关系符（0.2778em）与二元运算符（0.2222em） |
| 幻影与类别 | `\phantom \hphantom \vphantom`；`\mathord \mathop \mathbin \mathrel \mathopen \mathclose \mathpunct \mathinner` 强制原子类别 |

字体直接取自系统：`Cambria Math`（`cambria.ttc` 的第 1 个字面）作为数学字体，
微软雅黑兜底中文，`\mathbb` 走 Unicode 双线体字形（`\mathbb{R}` → `ℝ`）。

两个容易出错的地方也一并按 TeX 的行为做了：

- **二元运算符的类别判定是双向的。** `(-x)` 里的 `-` 不是二元运算符，`a+` 结尾时
  也不是，所以整行先定类别、再排版，只看前一个原子会排错一侧的间距。
- **矩阵类环境居中于数学轴**，不是挂在基线上。此前首行的 ascent 被算了两遍，整块
  会偏低；现在按 TeX 的做法把块的中点对齐到数学轴（0.25em）上。

未识别的命令按源码原样渲染并降低不透明度，同时记入警告列表——不崩溃，
也不悄悄吞掉。

### Mermaid

自研解析器（`src/diagram/mermaid.rs`，约 2400 行）+ 分层布局
（`layout.rs`，约 2500 行），支持：

- **flowchart / graph** — TD/TB/BT/LR/RL 六种方向；13 种节点形状
  （矩形、圆角、体育场、圆、双圆、六边形、平行四边形、圆柱、不对称形…）；
  实线/虚线/粗线箭头，`--x` `--o` `<-->` 等箭头；带标签的边；`~~~` 隐形连线
  （参与分层但不绘制）；链式边 `A --> B --> C`；`&` 扇出扇入（自动取笛卡尔积）；
  `subgraph` 嵌套；v11 的 `@{ shape: rect, label: "x" }` 语法；
  `classDef` / `class` / `A:::cls` / `style` / `linkStyle` 真生效。
- **sequenceDiagram** — `actor` / `participant` / `database` / `queue` 等
  全部参与者类型、全部箭头类型、激活/失活（`+`/`-` 后缀与独立语句）、
  `create` / `destroy`、四种 `Note` 位置写法、
  `loop`/`alt`/`else`/`opt`/`par`/`and`/`critical`/`option`/`break`/`rect` 分块、
  `==` 分节线、`...` 延时、`autonumber` 与 `autonumber off`。
- **pie** — 标签可加引号可裸写，`showData` 控制数值显示。
- **classDiagram** — 类体按「字段 / 方法」自动分格，`<<interface>>`
  `<<abstract>>` 标记，全套继承与依赖关系箭头，命名空间。
- **stateDiagram** — `[*]` 起始/终止圆点（同一文件出现两次会自动区分
  起点与终点）、`state "描述" as X`、复合状态。
- **erDiagram** — 实体属性格、基数关系。
- **mindmap** — 按缩进建树，`((圆))` `(圆角)` `[矩形]` `{{六边形}}` `)云形(`
  与 `id((标签))` 写法，左→右排布。
- **requirementDiagram** — `requirement` / `element` 块变分类框（字段自动分格），
  `A - satisfies -> B` 与 `B <- derives - A` 两种关系方向。
- **kanban** — 列变分组、卡片变节点，左→右排布。
- **journey** — `section` 分带、`步骤: 分数: 参与者` 语法（分数夹在 1–5、缺省给 3）、1–5 分刻度轴与网格、步骤框按分数定位并用折线连成路径、参与者列在框下。
- **timeline** — `section` 分带、`2002 : LinkedIn : Google` 一句多事件、卡片沿轴线上下交替、无事件的时期也占一列。
- **quadrantChart** — `x-axis 低 --> 高` / `y-axis` 轴标题、`quadrant-1..4` 四象限名（右上→左上→左下→右下）、`名称: [x, y]` 数据点（自动夹到 0–1）、点标签在靠近右边界时自动翻到左侧。
- **gitGraph** — `commit id:/tag:/type:` 三种类型（NORMAL / REVERSE / HIGHLIGHT）、`branch` / `checkout` / `switch`（新分支从当前 HEAD 起算，与 `checkout -b` 一致）、`merge`（双亲连线）、`cherry-pick`；每条分支一条泳道，底部带分支图例。
- **block-beta** — `columns N` 网格、`id["标签"]` 格子、`space` / `space:2` 占位、
  `a --> b` 格间箭头；样式指令会给出忽略告警。
- **radar-beta** — `axis a["标签"], b[…], …`（至少 3 轴）、`curve 名["标签"]{v1, v2, …}`、
  `max N`；4 层同心环、半透明多边形叠加、底部图例。
- **packet-beta** — `0-15: "字段"` 位范围、`+16: "字段"` 相对宽度、单比特 `3: "标志"`；每 32 位自动换行，顶部有 8 位刻度，超长字段名自动省略号截断。
- **sankey-beta** — `源,目标,流量` CSV 行（支持引号内含逗号），流量带宽按值等比、节点按最长路径分层、节点标签带底色以在流带上方可读。
- **xychart-beta** — `x-axis [分类…]` 或数值轴 `x-axis "标签" 0 --> 10`、`y-axis "标签" 0 --> 100`、`bar` / `line`（可带引号系列名）、多序列并列柱、刻度自动取整（1/2/5×10ⁿ）；`xychart-beta horizontal` 会按竖直方向绘制并在状态栏说明。
- **gantt** — `section` 分组、`日期` 或 `after <id>` 起算、`30d`/`2w`/`3h`
  时长、`done` / `active` / `crit` / `milestone` 标记、`axisFormat` 刻度格式、
  `dateFormat` / `excludes` / `todayMarker` / `tickInterval` 等日历指令安全忽略。

### PlantUML

同样自研（`plantuml.rs`，约 2350 行），复用同一套 IR 与绘制层：

- **时序图**（主力支持）— `participant`/`actor`/`boundary`/`control`/
  `entity`/`database`/`collections`/`queue` 七种参与者类型；
  消息箭头含 `->` `->>` `-->>` `->x` `->o` 与全部反向形式 `←` `<->`；
  `note over A, B` 多行块；`alt`/`else`/`opt`/`loop`/`par`/`break`/
  `critical`/`group`/`rect`；`==` 分节线；`autonumber`。
- **类图** — `abstract class` / `interface` / `enum` / 立体标记，
  成员按字段与方法分格，继承/依赖/组合/聚合全套关系，多重性，
  `package`/`namespace` 嵌套。
- **WBS / mindmap** — `@startwbs` / `@startmindmap`，`*` 层数即层级，
  `left side` / `right side` 标记被忽略；左→右树形排布。
- **甘特图** — `@startgantt`：`Project starts 日期`、`[任务] lasts N days`、
  `starts at [其他]'s end`、`starts 1 day after [其他]'s end`、`[任务] starts 日期`、
  `[任务] ends 日期`（据此反推时长）、`-- 阶段名 --` 分节；复用 Mermaid gantt 的时间轴布局。
- **活动图（activity，beta 语法）** — `start`/`stop`、`:动作;`（可带 `#颜色`）、
  `if (条件) then (标签) / else / elseif / endif`、`while … endwhile`、
  `repeat … repeat while (条件) is (是) not (否)`、`fork / fork again / end fork`、
  `partition "名称" { … }`；分支并排、循环与分区带底色框、分叉用双横杠。
- **用例图 / 组件图**、**状态图**、**ER / IE**。

### 提醒框：四种语法

同一套模型，**双向**支持四种主流写法，可解析也可序列化：

| 风格 | 写法 |
|---|---|
| GitHub | `> [!NOTE]` `> [!TIP]` `> [!WARNING]` `> [!IMPORTANT]` `> [!CAUTION]` |
| Obsidian | `> [!note]` `> [!tip]` `> [!info]` `> [!warning]` `> [!danger]` |
| VitePress | `::: tip 标题` … `:::`（另有 VitePress 独有的 `::: details`） |
| Docusaurus | `:::note` … `:::`（另有 Docusaurus 独有的 `::: caution` `::: admonition`） |

变体统一为 7 类：Note / Tip / Info / Warning / Danger / Details / Caution。

**渲染用的是各家的真实资产与真实结构**，不是照着印象配的近似样式：

| 风格 | 结构 | 图标 | 标题 |
|---|---|---|---|
| GitHub | 4px 左侧竖线，无底色 | octicon（info / light-bulb / report / alert / stop） | 词首大写（`Note`） |
| Obsidian | 1px 描边 + 10% 底色，圆角 4 | lucide（pencil / info / flame / triangle-alert / zap / octagon-alert） | 原样 |
| VitePress | 柔和底色，边框默认透明，圆角 8 | 无（默认主题不显示图标） | 原样 |
| Docusaurus | 4px 左侧竖线 + 淡底色 | 主题自带 SVG，尺寸 1.6em | 全大写（`text-transform: uppercase`） |

配色也按各家发布的值取：GitHub 用 primer 的 accent/success/done/attention/danger，
Obsidian 用它 `app.css` 里的 `--color-*-rgb`。

图标路径数据原样取自 octicons（MIT）、lucide（ISC）与 Docusaurus 主题（MIT），
放在 `assets/icons/`，由 `tools/extract_icons.py` 生成 `src/icon_data.rs`。
运行时零网络请求，**也不再依赖系统字体里恰好有没有某个符号字形**：图标是矢量，
按当前缩放绘制，`gfx` 侧新增了多边形填充（非零环绕与奇偶两种规则都要支持，
因为 octicons 靠反向缠绕挖孔、Docusaurus 显式用 `fill-rule="evenodd"`）。

**判定规则是按块独立的**，不是整篇统一：文档里同时出现 `::: tip`
（VitePress 惯例，带空格）和 `:::note`（Docusaurus 惯例，不带空格）时，
两者能被正确区分，不会因为看到第一个 `:::` 就把后面全判成同一种。
遇到只有两种共有关键词（`tip`/`info`/`warning`/`danger`）无法区分的情况，
若文档内还有任一方的独有语法，则用它决定整篇；都没有则默认 VitePress。

设置里可指定导出风格，转换是实打实的语法换写（`convert_callout`）。

### MDX 与 Markdoc

按扩展名自动识别（`.mdx` / `.markdoc`），状态栏显示当前文档类型。

- **MDX** — `import` / `export` 语句渲染为语义化的组件块（不当作普通段落）；
  JSX 标签 `<Foo />`、`<Foo>…</Foo>` 成对识别为组件块，内部 Markdown
  照常渲染。
- **Markdoc** — `{% tag %}` / `{% tag attr="x" %}` 单标签、`{% #if %}`
  等逻辑块，配对标签内的内容正常渲染。

两者都能正确显示、正确编辑，并保留原始源码写法。

---

## 快捷键

| 操作 | 快捷键 |
|---|---|
| 源码模式切换 | `Ctrl+\` |
| 保存 | `Ctrl+S` |
| 另存为 | `Ctrl+Shift+S` |
| 打开 | `Ctrl+O` |
| 新建 | `Ctrl+N` |
| 导出 HTML | `Ctrl+E` |
| 加粗 / 斜体 | `Ctrl+B` / `Ctrl+I` |
| 插入链接 | `Ctrl+K` |
| 查找 / 替换 | `Ctrl+F` / `Ctrl+H` |
| 下一处 / 上一处 | `Enter` / `Shift+Enter` |
| 关闭查找栏 | `Esc` |

工具栏提供标题级别、粗斜体、删除线、行内代码、引用、有序/无序/任务列表、
代码围栏、表格、分割线、提醒框（7 种变体 × 4 种语法）、图表（3 种引擎）、
数学（行内/块级）、MDX 组件、Markdoc 标签的插入。「打开」是一个菜单：
第一项是文件对话框，下面是最近打开的 12 个文件；已经从磁盘消失的条目会被
自动摘掉，而不是点了才报错。

---

## 代码结构

```
src/
  gfx.rs              渲染无关的绘图图元与文本度量契约（引擎层不碰 egui）
  gfx_egui.rs         gfx → egui 的桥接层，全项目唯一直接画图元的地方
  theme.rs            主题：配色、字号阶、行高、7 类提醒框外观
  fonts.rs            系统字体挂载（按 ttc face 索引精确挂载）
  config.rs           持久化设置与最近文件列表
  find.rs             查找/替换的匹配与光标逻辑（字节区间，无 egui）
  recovery.rs         崩溃恢复草稿的读写（纯文件 I/O，无 egui）
  icon.rs             提醒框图标：SVG 路径解析 → 曲线展平 → 填充/描边几何
  icon_data.rs        官方图标路径数据（由 tools/extract_icons.py 生成）

  doc/
    model.rs          Document、Block/BlockKind、提醒框模型、字数统计
    block.rs          块级解析器：标题/列表/表格/围栏/提醒框/MDX/Markdoc
    inline.rs         行内扫描器：样式片段 + 构造定界符范围

  tex/                LaTeX 排版引擎
    parse.rs          分词器与 AST
    layout.rs         基线驱动布局 + 图元生成

  diagram/
    model.rs          图表 IR（Graph / Sequence / Pie 三个变体）
    mermaid.rs        Mermaid 前端
    plantuml.rs       PlantUML 前端
    layout.rs         分层布局 / 时序图布局 / 饼图布局
    draw.rs           图元生成

  edit.rs             就地编辑核心：块级渲染、光标映射、语法标记显隐
  app.rs              应用外壳：面板、工具栏、状态栏、文件操作、设置
```

分层原则：**引擎层完全不接触 egui**。`gfx.rs` 定义了平台无关的
`Prim` / `Measure` 抽象，LaTeX 引擎与图表引擎只依赖它，因此它们可以
脱离 GUI 独立测试（大部分测试运行在这一层）。egui 只出现在
`gfx_egui.rs`（绘制桥接）、`edit.rs`（控件与光标）、`app.rs`（外壳）三处。

新增渲染能力的路径通常是：往 `gfx.rs` 加图元 → 引擎层产出图元 → 在
`edit.rs` 里把图元塞进一个块。引擎层不需要任何改动。

---

## 已知边界

诚实说明当前没有覆盖的部分：

- **LaTeX** 是常用子集，不是 TeX 的完整实现。没有 `\middle`、`\cancel`、
  `\xrightarrow`、`\genfrac`、编号与交叉引用（`\tag` / `\label` 静默忽略）；
  `array` 的 `>{}` / `<{}` 列修饰只解析不生效；`p{2cm}` 这类绝对长度按
  1em = 10pt 的参考字号换算。`\frac` 不做行内自动缩放（行内分母取 0.72 倍字号），
  连字符与断行等 TeX 细节未做。`\mathbb` 对 Unicode 无对应字形的字母会退化为下划线。
  上下标不会自动拉开避让（同一位置上的 `x_i^2` 叠得很紧）。
- **Mermaid / PlantUML** 覆盖了常用语法，主题也**真的生效**：Mermaid 的
  `classDef` / `class` / `A:::cls` / `style` / `linkStyle` 与 PlantUML 的
  `skinparam`（含 `skinparam class { … }` 块写法）、`#颜色` 后缀会改变填充、
  描边、文字色与线宽，`stroke-dasharray` 走虚线。没有对应语义的声明
  （`font-weight`、`opacity`、`rx` 等）仍然只解析不生效。时序图 activation
  条按语句配对，遇到不配对的 `deactivate` 会安全忽略而非报错。
- **表格**可正常渲染与编辑，但不是所见即所得的单元格网格——表格整块
  通过「编辑表格源码」进入源码态编辑，因为 Markdown 表格的行内语法
  很容易在直接改列宽时被破坏。
- **MDX / Markdoc** 的语义块能正确识别与往返，但只渲染其结构，不执行
  MDX 组件代码，也不求值 Markdoc 逻辑表达式——离线编辑器里没有运行时。
- **撤销**使用整份文档快照，最多保留 200 步，不跨会话保存。崩溃恢复只保留
  **一份**草稿（当前文档），不是完整的历史；正常退出不会留下草稿。关闭窗口或
  切换文档时仍会确认是否放弃未保存的修改。
