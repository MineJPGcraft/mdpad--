//! Application shell.
//!
//! Owns the document, the persisted settings, the file actions and the three
//! panels: a toolbar, the document surface, and a status bar whose right-hand end
//! carries the source-mode toggle.
//!
//! Rendering lives in [`crate::edit`]; this file is about orchestration — where
//! keystrokes go, when to reparse, when to write to disk, and how to keep the
//! live view and the raw source in step.

use std::path::PathBuf;

use egui::{FontFamily, FontId, RichText};

use crate::gfx::Rgba;

use crate::config::Config;
use crate::doc::model::{BlockKind, CalloutSyntax, Document, Flavor};
use crate::edit::{self, EditorAction, RenderOpts, ViewState};
use crate::fonts::{self, Report as FontReport};
use crate::theme::Theme;

/// How many undo snapshots to keep.
const UNDO_LIMIT: usize = 200;

/// One undo step: the whole document text plus its path.
struct Snapshot {
    text: String,
    path: Option<PathBuf>,
}

pub struct App {
    config: Config,
    theme: Theme,
    doc: Document,
    view: ViewState,
    fonts: FontReport,

    /// Raw source shown in source mode; kept in sync with `doc.text`.
    source_buffer: String,
    source_mode: bool,
    /// Undo stack, oldest first.
    undo: Vec<Snapshot>,
    /// Set when the close button should be honoured.
    force_quit: bool,
    /// Path shown in the title bar on the last frame.
    last_title: String,
    /// Pending error/notice text shown in the status bar.
    status_note: Option<String>,
}

impl App {
    /// Builds the application, installing fonts and loading settings.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        let fonts = fonts::install(&cc.egui_ctx);
        let theme = theme_from(&config);
        theme.apply_egui_visuals(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // First run: open the sample so the editor has something to show. It
        // exercises every construct the app supports, which also makes it a
        // living smoke test of the renderer.
        let first_run = config.recent.is_empty();
        let doc = if first_run {
            Document::from_file(PathBuf::from("示例.md"), SAMPLE)
        } else {
            Document::new()
        };
        let source_buffer = doc.text.clone();
        let ready = if first_run {
            format!("已载入示例文档 · {} · 右下角可切换源码模式", fonts_summary(&fonts))
        } else {
            format!("已就绪 · {} · 右下角可切换源码模式", fonts_summary(&fonts))
        };
        let mut config = config;
        config.prune_recent();
        Self {
            config,
            theme,
            doc,
            view: ViewState::default(),
            fonts,
            source_buffer,
            source_mode: false,
            undo: Vec::new(),
            force_quit: false,
            last_title: String::new(),
            status_note: Some(ready),
        }
    }

    // -- document mutation -------------------------------------------------

    /// Pushes an undo snapshot before a mutation.
    fn checkpoint(&mut self) {
        self.undo.push(Snapshot {
            text: self.doc.text.clone(),
            path: self.doc.path.clone(),
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    fn undo(&mut self) {
        let Some(snapshot) = self.undo.pop() else {
            self.note("没有可撤销的操作");
            return;
        };
        self.doc.path = snapshot.path;
        self.doc.set_text(snapshot.text);
        self.after_document_change();
                }

    /// Refreshes derived state after the document text changed.
    fn after_document_change(&mut self) {
        self.source_buffer = self.doc.text.clone();
        self.view.end_session();
        self.view.invalidate();
    }

    fn note(&mut self, text: impl Into<String>) {
        self.status_note = Some(text.into());
    }

    fn set_text(&mut self, text: String) {
        self.checkpoint();
        self.doc.set_text(text);
        self.after_document_change();
    }

    // -- file actions ------------------------------------------------------

    fn new_document(&mut self) {
        if self.doc.dirty && !self.confirm_discard() {
            return;
        }
        self.checkpoint();
        self.doc = Document::new();
        self.after_document_change();
        self.note("已新建文档");
    }

    fn open_document(&mut self) {
        if self.doc.dirty && !self.confirm_discard() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("打开 Markdown / MDX / Markdoc")
            .add_filter(
                "Markdown 家族",
                &["md", "markdown", "mdx", "markdoc", "mdoc", "txt"],
            )
            .add_filter("全部文件", &["*"])
            .set_directory(self.config.start_dir())
            .pick_file()
        else {
            return;
        };
        self.load_path(path);
    }

    fn load_path(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                self.checkpoint();
                self.doc = Document::from_file(path.clone(), &raw);
                self.after_document_change();
                self.config.push_recent(&path);
                self.note(format!("已打开 {}", path.display()));
            }
            Err(err) => self.note(format!("打开失败：{err}")),
        }
    }

    fn save(&mut self) {
        match self.doc.path.clone() {
            Some(path) => self.write_to(path),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) {
        let default_name = self.doc.display_name();
        let Some(path) = rfd::FileDialog::new()
            .set_title("另存为")
            .set_file_name(&default_name)
            .add_filter("Markdown", &["md"])
            .add_filter("MDX", &["mdx"])
            .add_filter("Markdoc", &["markdoc"])
            .set_directory(self.config.start_dir())
            .save_file()
        else {
            return;
        };
        self.doc.flavor = Flavor::from_path(&path);
        self.write_to(path);
    }

    fn write_to(&mut self, path: PathBuf) {
        let mut body = self.doc.text.clone();
        if self.doc.crlf && !body.contains("\r\n") {
            body = crate::doc::model::to_crlf(&body);
        }
        match std::fs::write(&path, body) {
            Ok(()) => {
                self.doc.path = Some(path.clone());
                self.doc.dirty = false;
                self.config.push_recent(&path);
                self.note(format!("已保存 {}", path.display()));
            }
            Err(err) => self.note(format!("保存失败：{err}")),
        }
    }

    /// Asks before throwing away unsaved work.
    fn confirm_discard(&mut self) -> bool {
        let name = self.doc.display_name();
        rfd::MessageDialog::new()
            .set_title("未保存的更改")
            .set_description(format!("{name} 有未保存的更改，确定要放弃吗？"))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
    }

    fn export_html(&mut self) {
        let html = render_html(&self.doc);
        let default_name = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or("document")
            .to_owned()
            + ".html";
        let Some(path) = rfd::FileDialog::new()
            .set_title("导出 HTML")
            .set_file_name(&default_name)
            .add_filter("HTML", &["html"])
            .set_directory(self.config.start_dir())
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, html) {
            Ok(()) => self.note(format!("已导出 {}", path.display())),
            Err(err) => self.note(format!("导出失败：{err}")),
        }
    }

    // -- source mode -------------------------------------------------------

    fn toggle_source(&mut self, ctx: &egui::Context) {
        self.source_mode = !self.source_mode;
        self.config.source_mode = self.source_mode;
        if self.source_mode {
            // Entering: adopt the current document verbatim.
            self.source_buffer = self.doc.text.clone();
            self.view.end_session();
        } else {
            // Leaving: the buffer is already the document, but a direct text set
            // keeps both sides honest if the widget was mid-edit.
            let buffer = self.source_buffer.clone();
            if buffer != self.doc.text {
                self.set_text(buffer);
            }
        }
        self.note(if self.source_mode {
            "已进入源代码模式"
        } else {
            "已返回所见即所得模式"
        });
        ctx.request_repaint();
    }

    /// Loads a built-in sample that exercises every supported construct.
    fn load_sample(&mut self) {
        self.checkpoint();
        self.doc = Document::from_file(PathBuf::from("示例.md"), SAMPLE);
        self.after_document_change();
        self.note("已载入示例文档");
    }

    // -- editing helpers ---------------------------------------------------

    /// Wraps the inline content of the active block in `prefix`/`suffix`.
    fn wrap_active(&mut self, prefix: &str, suffix: &str) {
        let Some(block) = self.active_block().cloned() else {
            self.note("请先点击要编辑的段落");
            return;
        };
        let marker_len = block
            .inline_span
            .start
            .saturating_sub(block.span.start)
            .min(block.span.len());
        let (Some(_), Some(session)) = (Some(()), self.view.active.as_ref()) else {
            return;
        };
        let buffer = session.buffer.clone();
        let inner_end = (marker_len + block.inline_span.len()).min(buffer.len());
        if marker_len > inner_end {
            return;
        }
        let mut next = String::with_capacity(buffer.len() + prefix.len() + suffix.len());
        next.push_str(&buffer[..marker_len]);
        next.push_str(prefix);
        next.push_str(&buffer[marker_len..inner_end]);
        next.push_str(suffix);
        next.push_str(&buffer[inner_end..]);
        if let Some(session) = self.view.active.as_mut() {
            session.buffer = next;
        }
        self.note(format!("已插入 {prefix} … {suffix}"));
    }

    /// The block currently being edited, resolved from the session path.
    fn active_block(&self) -> Option<&crate::doc::model::Block> {
        let path = self.view.active.as_ref()?.path.clone();
        resolve_path(&self.doc.blocks, &path)
    }

    /// Inserts a new block after the active one, or at the end of the document.
    fn insert_block(&mut self, template: &str) {
        let at = match self.view.active.as_ref() {
            Some(session) => {
                let blocks = &self.doc.blocks;
                resolve_path(blocks, &session.path)
                    .map(|b| b.span.end)
                    .unwrap_or(self.doc.text.len())
            }
            None => self.doc.text.len(),
        };
        let mut insert = String::new();
        // Separate from the previous block unless it already ends with a blank line.
        let before = &self.doc.text[..at];
        if !before.is_empty() && !before.ends_with("\n\n") {
            insert.push('\n');
        }
        insert.push_str(template);
        self.checkpoint();
        self.doc.replace_span(at..at, &insert);
        self.after_document_change();
    }

    fn insert_callout(&mut self, variant: crate::doc::model::CalloutVariant) {
        let syntax = self.config.callout_syntax();
        let template = callout_template(syntax, &variant);
        self.insert_block(&template);
    }

    // -- panels ------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let _ = &ctx;
        egui::Panel::top("mdp-toolbar")
            .exact_size(40.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(6.0);
                    if ui
                        .button("新建")
                        .on_hover_text("Ctrl+N")
                        .clicked()
                    {
                        self.new_document();
                    }
                    if ui.button("打开").on_hover_text("Ctrl+O").clicked() {
                        self.open_document();
                    }
                    if ui
                        .button(if self.doc.dirty { "保存 •" } else { "保存" })
                        .on_hover_text("Ctrl+S")
                        .clicked()
                    {
                        self.save();
                    }
                    ui.menu_button("另存为 / 导出", |ui| {
                        if ui.button("另存为 …").clicked() {
                            self.save_as();
                            ui.close();
                        }
                        if ui.button("导出为 HTML").clicked() {
                            self.export_html();
                            ui.close();
                        }
                    });
                    ui.separator();

                    self.format_buttons(ui);
                    ui.separator();
                    self.insert_menus(ui);
                    ui.separator();

                    if ui
                        .button("撤销")
                        .on_hover_text("Ctrl+Z")
                        .clicked()
                    {
                        self.undo();
                    }
                    if ui.button("示例").on_hover_text("载入覆盖全部特性的示例文档").clicked() {
                        self.load_sample();
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        if ui
                            .button(if self.theme.dark { "浅色" } else { "深色" })
                            .on_hover_text("切换主题")
                            .clicked()
                        {
                            self.config.dark = !self.config.dark;
                            self.theme = theme_from(&self.config);
                            self.theme.apply_egui_visuals(&ctx);
                            self.view.invalidate();
                        }
                        self.settings_menu(ui);
                    });
                });
            });
    }

    fn format_buttons(&mut self, ui: &mut egui::Ui) {
        let bold = ui
            .button(RichText::new("B").strong())
            .on_hover_text("加粗（Ctrl+B）")
            .clicked();
        if bold {
            self.wrap_active("**", "**");
        }
        if ui
            .button(RichText::new("I").italics())
            .on_hover_text("斜体（Ctrl+I）")
            .clicked()
        {
            self.wrap_active("*", "*");
        }
        if ui.button("S̶").on_hover_text("删除线").clicked() {
            self.wrap_active("~~", "~~");
        }
        if ui
            .button("</>")
            .on_hover_text("行内代码")
            .clicked()
        {
            self.wrap_active("`", "`");
        }
        if ui.button("H").on_hover_text("高亮").clicked() {
            self.wrap_active("==", "==");
        }
        if ui.button("$").on_hover_text("行内公式").clicked() {
            self.wrap_active("$", "$");
        }
    }

    fn insert_menus(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("标题 ▾", |ui| {
            for level in 1..=4u8 {
                if ui.button(format!("{level} 级标题")).clicked() {
                    self.insert_block(&format!("{} 标题\n", "#".repeat(level as usize)));
                    ui.close();
                }
            }
        });

        ui.menu_button("列表 ▾", |ui| {
            if ui.button("无序列表").clicked() {
                self.insert_block("- 项目\n");
                ui.close();
            }
            if ui.button("有序列表").clicked() {
                self.insert_block("1. 项目\n");
                ui.close();
            }
            if ui.button("任务列表").clicked() {
                self.insert_block("- [ ] 待办\n");
                ui.close();
            }
            if ui.button("引用").clicked() {
                self.insert_block("> 引用内容\n");
                ui.close();
            }
            if ui.button("分隔线").clicked() {
                self.insert_block("---\n");
                ui.close();
            }
        });

        ui.menu_button("提醒框 ▾", |ui| {
            let default = self.config.callout_syntax();
            ui.label(
                RichText::new(format!("当前风格：{}", default.label()))
                    .size(12.0)
                    .color(self.theme.palette.text_faint.to_egui()),
            );
            ui.separator();
            for variant in default.keywords() {
                if ui
                    .button(format!(
                        "{}  ·  {}",
                        variant.label(),
                        variant.key_for(default)
                    ))
                    .clicked()
                {
                    self.insert_callout(variant.clone());
                    ui.close();
                }
            }
            ui.separator();
            ui.label(
                RichText::new(default.sample())
                    .monospace()
                    .size(11.0)
                    .color(self.theme.palette.text_faint.to_egui()),
            );
        });

        ui.menu_button("插入 ▾", |ui| {
            if ui.button("Mermaid 图表").clicked() {
                self.insert_block(
                    "```mermaid\nflowchart TD\n  A[开始] --> B{判断}\n  B -->|是| C[处理]\n  B -->|否| D[结束]\n```\n",
                );
                ui.close();
            }
            if ui.button("PlantUML 时序图").clicked() {
                self.insert_block(
                    "```plantuml\n@startuml\nAlice -> Bob: 请求\nBob --> Alice: 响应\n@enduml\n```\n",
                );
                ui.close();
            }
            if ui.button("PlantUML 类图").clicked() {
                self.insert_block(
                    "```plantuml\n@startuml\nclass Order {\n  +id: String\n  +total(): Int\n}\nclass Item\nOrder \"1\" *-- \"0..*\" Item\n@enduml\n```\n",
                );
                ui.close();
            }
            if ui.button("块级公式").clicked() {
                self.insert_block("$$\n\\int_{0}^{1} x^2 \\, dx = \\frac{1}{3}\n$$\n");
                ui.close();
            }
            if ui.button("表格").clicked() {
                self.insert_block("| 列 A | 列 B |\n|:--|--:|\n| 1 | 2 |\n");
                ui.close();
            }
            if ui.button("代码块").clicked() {
                self.insert_block("```rust\nfn main() {}\n```\n");
                ui.close();
            }
            ui.separator();
            if ui.button("MDX：import 语句").clicked() {
                self.doc.flavor = Flavor::Mdx;
                self.insert_block("import Tabs from '@theme/Tabs';\n");
                ui.close();
            }
            if ui.button("MDX：JSX 区块").clicked() {
                self.doc.flavor = Flavor::Mdx;
                self.insert_block(
                    "<Callout type=\"tip\">\n\n这里是 JSX 组件内的 Markdown。\n\n</Callout>\n",
                );
                ui.close();
            }
            if ui.button("Markdoc：条件标签").clicked() {
                self.doc.flavor = Flavor::Markdoc;
                self.insert_block("{% if $show %}\n条件成立时显示。\n{% /if %}\n");
                ui.close();
            }
            if ui.button("Markdoc：叶子标签").clicked() {
                self.doc.flavor = Flavor::Markdoc;
                self.insert_block("{% callout type=\"note\" /%}\n");
                ui.close();
            }
        });
    }

    fn settings_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("设置 ▾", |ui| {
            ui.label("文档方言");
            for flavor in Flavor::all() {
                if ui
                    .selectable_label(self.doc.flavor == flavor, flavor.label())
                    .clicked()
                {
                    self.doc.flavor = flavor;
                    self.doc.reparse();
                    ui.close();
                }
            }
            ui.separator();
            ui.label("新提醒框风格");
            for syntax in CalloutSyntax::all() {
                if ui
                    .selectable_label(self.config.callout_syntax() == syntax, syntax.label())
                    .clicked()
                {
                    self.config.set_callout_syntax(syntax);
                    ui.close();
                }
            }
            ui.separator();
            ui.label("缩放");
            ui.add(egui::Slider::new(&mut self.config.zoom, 0.7..=2.0).show_value(false));
            ui.label("行距");
            ui.add(egui::Slider::new(&mut self.config.line_height, 1.1..=2.4).show_value(false));
            ui.label("栏宽（0 = 自适应）");
            ui.add(egui::Slider::new(&mut self.config.measure, 0.0..=1200.0).show_value(true));
            ui.checkbox(&mut self.config.show_gutter, "显示块类型标记");
            ui.checkbox(&mut self.config.source_wrap, "源码模式自动换行");
            ui.separator();
            // Font status matters here: without a CJK face Chinese text renders
            // as empty boxes, and without a maths face formulas lose glyphs.
            ui.label(
                RichText::new(self.fonts.summary())
                    .size(11.0)
                    .color(self.theme.palette.text_faint.to_egui()),
            );
            for missing in &self.fonts.missing {
                ui.label(
                    RichText::new(format!("缺字体：{missing}"))
                        .size(11.0)
                        .color(Rgba::hex(0xFBBF24).to_egui()),
                );
            }
            ui.separator();
            if ui.button("保存设置").clicked() {
                match self.config.save() {
                    Ok(()) => self.note("设置已保存"),
                    Err(err) => self.note(format!("设置保存失败：{err}")),
                }
                ui.close();
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::bottom("mdp-status")
            .exact_size(28.0)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(self.doc.display_path())
                            .size(12.0)
                            .color(self.theme.palette.text_muted.to_egui()),
                    );
                    if self.doc.dirty {
                        ui.label(
                            RichText::new("•")
                                .size(12.0)
                                .color(self.theme.palette.accent.to_egui()),
                        );
                    }
                    ui.separator();
                    ui.label(
                        RichText::new(self.doc.flavor.label())
                            .size(12.0)
                            .color(self.theme.palette.text_faint.to_egui()),
                    );
                    let stats = self.doc.stats();
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "{} 词 · {} 字 · {} 行 · 约 {} 分钟",
                            stats.words, stats.chars, stats.lines, stats.reading_minutes
                        ))
                        .size(12.0)
                        .color(self.theme.palette.text_faint.to_egui()),
                    );
                    if let Some(note) = self.status_note.clone() {
                        ui.separator();
                        ui.label(
                            RichText::new(note)
                                .size(12.0)
                                .color(self.theme.palette.accent.to_egui()),
                        );
                    }

                    // ---- bottom-right: the source-mode toggle ----
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        let label = if self.source_mode {
                            "✓ 源代码模式"
                        } else {
                            "源代码模式"
                        };
                        let resp = ui
                            .selectable_label(self.source_mode, RichText::new(label).size(12.0))
                            .on_hover_text(
                                "在所见即所得与 Markdown 源码之间切换（Ctrl+/）\n点击一次显示源码，再点一次回到渲染视图",
                            );
                        if resp.clicked() {
                            self.toggle_source(&ctx);
                        }
                    });
                });
            });
    }

    fn document_surface(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let margin = 24.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let avail = ui.available_width();
                    let measure = if self.config.measure <= 0.0 {
                        (avail - margin * 2.0).max(280.0)
                    } else {
                        self.config.measure.min((avail - margin * 2.0).max(280.0))
                    };
                    let offset = ((avail - measure) * 0.5).max(0.0);
                    ui.horizontal(|ui| {
                        ui.add_space(offset);
                        ui.vertical(|ui| {
                            ui.set_width(measure);
                            if self.source_mode {
                                self.source_pane(ui);
                            } else {
                                let opts = RenderOpts {
                                    show_gutter: self.config.show_gutter,
                                    measure,
                                };
                                if let Some(action) = edit::document_ui(
                                    ui,
                                    &mut self.view,
                                    &self.doc,
                                    &self.theme,
                                    &opts,
                                ) {
                                    self.handle_action(action);
                                }
                            }
                            ui.add_space(120.0);
                        });
                    });
                });
        });
    }

    /// The raw Markdown editor used by source mode.
    fn source_pane(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        let wrap = self.config.source_wrap;
        let mut layouter = move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, width: f32| {
            let mut job = source_job(buf.as_str(), &theme);
            job.wrap.max_width = if wrap { width } else { f32::INFINITY };
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };

        let before = self.source_buffer.clone();
        let resp = ui.add(
            egui::TextEdit::multiline(&mut self.source_buffer)
                .id(egui::Id::unique("mdp-source"))
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY)
                .font(FontId::new(
                    self.theme.typo.code(),
                    FontFamily::Monospace,
                ))
                .layouter(&mut layouter),
        );
        let _ = (resp, before);
        // The buffer is authoritative while source mode is active.
        if self.source_buffer != self.doc.text {
            let text = self.source_buffer.clone();
            self.doc.set_text(text);
            self.view.invalidate();
        }
        ui.label(
            RichText::new("源代码模式：编辑的就是最终保存的 Markdown 文本")
                .size(11.0)
                .color(self.theme.palette.text_faint.to_egui()),
        );
    }

    fn handle_action(&mut self, action: EditorAction) {
        match action {
            EditorAction::Commit { span, text } => {
                if self.doc.text.get(span.clone()).map(str::to_owned).as_deref() != Some(&text) {
                    self.checkpoint();
                    self.doc.replace_span(span, &text);
                }
                self.view.end_session();
                self.source_buffer = self.doc.text.clone();
            }
            EditorAction::Focus { path, caret: _ } => {
                let block = resolve_path(&self.doc.blocks, &path).cloned();
                if let Some(block) = block {
                    self.view.begin_session(path, &self.doc, &block);
                }
            }
            EditorAction::OpenLink(url) => {
                open_url(&url);
            }
            EditorAction::ToggleTask { span, checked } => {
                let current = self.doc.text.get(span.clone()).unwrap_or("").to_owned();
                let updated = set_task_state(&current, checked);
                if updated != current {
                    self.checkpoint();
                    self.doc.replace_span(span, &updated);
                    self.source_buffer = self.doc.text.clone();
                }
            }
            EditorAction::ConvertCallout { span, syntax } => {
                let source = self.doc.text.get(span.clone()).unwrap_or("").to_owned();
                match convert_callout(&source, syntax) {
                    Some(converted) => {
                        self.checkpoint();
                        self.doc.replace_span(span, &converted);
                        self.source_buffer = self.doc.text.clone();
                        self.note(format!("已转换为 {} 风格", syntax.label()));
                    }
                    None => self.note("无法转换此提醒框"),
                }
            }
            EditorAction::DeleteBlock { span } => {
                self.checkpoint();
                self.doc.replace_span(span, "");
                self.source_buffer = self.doc.text.clone();
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let has_focus = ctx.memory(|m| m.focused().is_some());
        let events: Vec<egui::Key> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .map(|(k, _)| k)
                .collect()
        });
        let modifiers = ctx.input(|i| i.modifiers);
        let _ = events;

        if modifiers.command {
            if ctx.input(|i| i.key_pressed(egui::Key::S)) {
                self.save();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::O)) {
                self.open_document();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::N)) {
                self.new_document();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Z)) && !has_focus {
                self.undo();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Slash)) {
                self.toggle_source(ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::B)) && !has_focus {
                self.wrap_active("**", "**");
            }
            if ctx.input(|i| i.key_pressed(egui::Key::I)) && !has_focus {
                self.wrap_active("*", "*");
            }
        }
    }

    fn window_title(&mut self, ctx: &egui::Context) {
        let dirty = if self.doc.dirty { "• " } else { "" };
        let title = format!("{dirty}{} — MarkdownPad", self.doc.display_name());
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Keep the type scale in sync with the zoom setting.
        if (self.theme.typo.scale - self.config.zoom).abs() > f32::EPSILON {
            self.theme.typo.scale = self.config.zoom;
        }
        self.theme.typo.line_height = self.config.line_height;
        self.theme.typo.measure = self.config.measure;

        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        self.toolbar(ui);
        self.status_bar(ui);
        self.document_surface(ui);
        self.window_title(&ctx);

        if self.force_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self) {
        let _ = self.config.save();
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn theme_from(config: &Config) -> Theme {
    let mut theme = Theme::for_mode(config.dark);
    theme.typo.scale = config.zoom;
    theme.typo.line_height = config.line_height;
    theme.typo.measure = config.measure;
    theme
}

fn fonts_summary(report: &FontReport) -> String {
    if report.missing.is_empty() {
        report.summary()
    } else {
        format!("{}；缺失：{}", report.summary(), report.missing.join("、"))
    }
}

/// Resolves a block path against a block tree.
pub fn resolve_path<'a>(
    blocks: &'a [crate::doc::model::Block],
    path: &[usize],
) -> Option<&'a crate::doc::model::Block> {
    let (first, rest) = path.split_first()?;
    let block = blocks.get(*first)?;
    if rest.is_empty() {
        return Some(block);
    }
    match &block.kind {
        BlockKind::Callout(c) => resolve_path(&c.body, rest),
        BlockKind::MdxJsx { children, .. } => resolve_path(children, rest),
        BlockKind::MarkdocTag { body, .. } => resolve_path(body, rest),
        _ => None,
    }
}

/// Rewrites the task checkbox of `source` (a list item) to `checked`.
fn set_task_state(source: &str, checked: bool) -> String {
    let marker = if checked { "[x]" } else { "[ ]" };
    // Replace the first `[ ]` / `[x]` occurrence after the list marker.
    if let Some(open) = source.find('[')
        && let Some(close) = source[open..].find(']')
        && close == 2
    {
        let mut out = String::with_capacity(source.len());
        out.push_str(&source[..open]);
        out.push_str(marker);
        out.push_str(&source[open + 3..]);
        return out;
    }
    source.to_owned()
}

/// Converts a callout between the four supported syntaxes.
///
/// Returns `None` when the source is not a recognisable callout in any syntax.
pub fn convert_callout(source: &str, target: CalloutSyntax) -> Option<String> {
    let (variant, title, body) = parse_callout_any(source)?;
    let keyword = variant.key_for(target);
    let mut out = String::new();
    match target {
        CalloutSyntax::GitHub => {
            out.push_str(&format!("> [!{keyword}]\n"));
            for line in body.lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
        }
        CalloutSyntax::Obsidian => {
            let suffix = title.as_deref().unwrap_or("");
            if suffix.is_empty() {
                out.push_str(&format!("> [!{keyword}]\n"));
            } else {
                out.push_str(&format!("> [!{keyword}] {suffix}\n"));
            }
            for line in body.lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
        }
        CalloutSyntax::VitePress | CalloutSyntax::Docusaurus => {
            let sep = if target == CalloutSyntax::Docusaurus {
                ""
            } else {
                " "
            };
            match title.as_deref() {
                Some(t) if !t.trim().is_empty() => {
                    out.push_str(&format!(":::{keyword}{sep}{t}\n"));
                }
                _ => out.push_str(&format!(":::{keyword}\n")),
            }
            out.push('\n');
            out.push_str(body.trim_end());
            out.push('\n');
            out.push_str(":::\n");
        }
    }
    Some(out)
}

/// Extracts `(variant, title, body)` from any of the four callout syntaxes.
fn parse_callout_any(
    source: &str,
) -> Option<(crate::doc::model::CalloutVariant, Option<String>, String)> {
    use crate::doc::model::CalloutVariant;
    let text = source.trim_end();
    // Quote form.
    if text.starts_with('>') {
        let mut lines: Vec<String> = Vec::new();
        for line in text.lines() {
            let t = line.trim_start().strip_prefix('>').unwrap_or(line);
            lines.push(t.strip_prefix(' ').unwrap_or(t).to_owned());
        }
        let marker_line = lines.iter().position(|l| !l.trim().is_empty())?;
        let marker = lines[marker_line].trim();
        let rest = marker.strip_prefix("[!")?;
        let close = rest.find(']')?;
        let key = rest.get(..close)?;
        let after = rest.get(close + 1..).unwrap_or("").trim();
        let title = if after.is_empty() {
            None
        } else {
            Some(after.to_owned())
        };
        let variant = CalloutVariant::from_key(CalloutSyntax::Obsidian, key);
        let body = lines[marker_line + 1..].join("\n");
        return Some((variant, title, body.trim_matches('\n').to_owned()));
    }
    // Container form.
    if text.starts_with(":::") {
        let mut lines = text.lines();
        let head = lines.next()?;
        let rest = head.trim_start_matches(':').trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("").trim();
        let title = parts.next().unwrap_or("").trim();
        if key.is_empty() {
            return None;
        }
        let body: Vec<&str> = lines
            .take_while(|l| !l.trim_start().starts_with(":::"))
            .collect();
        let variant = CalloutVariant::from_key(CalloutSyntax::VitePress, key);
        let title = title
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim()
            .to_owned();
        return Some((
            variant,
            if title.is_empty() { None } else { Some(title) },
            body.join("\n").trim_matches('\n').to_owned(),
        ));
    }
    None
}

/// Builds the Markdown for a new callout in the given syntax.
pub fn callout_template(
    syntax: CalloutSyntax,
    variant: &crate::doc::model::CalloutVariant,
) -> String {
    let keyword = variant.key_for(syntax);
    match syntax {
        CalloutSyntax::GitHub => format!("> [!{keyword}]\n> 内容\n"),
        CalloutSyntax::Obsidian => format!("> [!{keyword}] 标题\n> 内容\n"),
        // The space is load-bearing: VitePress writes `::: tip`, Docusaurus
        // writes `:::tip`, and that is how a reader (and our parser) tells the
        // two container styles apart when they share a keyword.
        CalloutSyntax::VitePress => format!("::: {keyword} 标题\n\n内容\n\n:::\n"),
        CalloutSyntax::Docusaurus => format!(":::{keyword} 标题\n\n内容\n\n:::\n"),
    }
}

/// Opens a URL with the platform's default handler.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Highlights raw Markdown for source mode: every delimiter stays visible, but
/// structure and inline constructs are coloured.
fn source_job(text: &str, theme: &Theme) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let size = theme.typo.code();
    let plain = egui::TextFormat {
        font_id: FontId::new(size, FontFamily::Monospace),
        color: theme.palette.text.to_egui(),
        ..Default::default()
    };
    let mut in_fence = false;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            job.append("\n", 0.0, plain.clone());
        }
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        let is_fence = (trimmed.starts_with("```") || trimmed.starts_with("~~~"))
            && trimmed
                .chars()
                .filter(|c| *c == '`' || *c == '~')
                .count()
                >= 3;
        if is_fence {
            in_fence = !in_fence;
            job.append(
                line,
                0.0,
                egui::TextFormat {
                    font_id: FontId::new(size, FontFamily::Monospace),
                    color: theme.palette.text_faint.to_egui(),
                    ..Default::default()
                },
            );
            continue;
        }
        if in_fence {
            job.append(
                line,
                0.0,
                egui::TextFormat {
                    font_id: FontId::new(size, FontFamily::Monospace),
                    color: theme.palette.code_text.to_egui(),
                    ..Default::default()
                },
            );
            continue;
        }
        if lead > 0 {
            job.append(&line[..lead], 0.0, plain.clone());
        }
        let t = trimmed;
        if t.starts_with('#') {
            let level = t.chars().take_while(|c| *c == '#').count().min(6) as u8;
            job.append(
                t,
                0.0,
                egui::TextFormat {
                    font_id: FontId::new(
                        theme.typo.heading(level).min(size * 1.7),
                        FontFamily::Monospace,
                    ),
                    color: theme.palette.text.to_egui(),
                    ..Default::default()
                },
            );
            continue;
        }
        if t.starts_with('>') {
            job.append(
                t,
                0.0,
                egui::TextFormat {
                    font_id: FontId::new(size, FontFamily::Monospace),
                    color: theme.palette.text_muted.to_egui(),
                    italics: true,
                    ..Default::default()
                },
            );
            continue;
        }
        // Inline constructs, with every delimiter revealed (the caret range spans
        // the whole line, so nothing is hidden).
        let sub = edit::source_inline_job(t, theme);
        for section in &sub.sections {
            let range = section.byte_range.start.0..section.byte_range.end.0;
            if let Some(piece) = sub.text.get(range) {
                job.append(piece, section.leading_space, section.format.clone());
            }
        }
    }
    job
}

/// Renders a standalone HTML preview of the document.
fn render_html(doc: &Document) -> String {
    let mut body = String::new();
    for block in &doc.blocks {
        html_block(&mut body, block);
    }
    format!(
        "<!doctype html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n<style>\nbody{{max-width:780px;margin:48px auto;padding:0 20px;font-family:-apple-system,'Segoe UI',system-ui,sans-serif;line-height:1.7;color:#1f2328}}\npre{{background:#f3f4f6;padding:12px 14px;border-radius:8px;overflow:auto}}\ncode{{font-family:Consolas,monospace;font-size:.92em}}\nblockquote{{margin:0;padding-left:14px;border-left:3px solid #d3d8e0;color:#57606a}}\ntable{{border-collapse:collapse}}\nth,td{{border:1px solid #d8dde5;padding:6px 10px}}\n.callout{{border-left:4px solid;padding:10px 14px;border-radius:6px;margin:12px 0}}\n</style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        title = doc.display_name(),
        body = body
    )
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn html_block(out: &mut String, block: &crate::doc::model::Block) {
    match &block.kind {
        BlockKind::Heading { level, text } => {
            out.push_str(&format!("<h{level}>{}</h{level}>\n", html_escape(text)));
        }
        BlockKind::Paragraph { text } => {
            out.push_str(&format!("<p>{}</p>\n", html_escape(text)));
        }
        BlockKind::ListItem { text, checked, .. } => {
            let prefix = match checked {
                Some(true) => "[x] ",
                Some(false) => "[ ] ",
                None => "",
            };
            out.push_str(&format!("<li>{}{}</li>\n", prefix, html_escape(text)));
        }
        BlockKind::Quote { text } => {
            out.push_str(&format!("<blockquote>{}</blockquote>\n", html_escape(text)));
        }
        BlockKind::Callout(c) => {
            let accent = match c.variant {
                crate::doc::model::CalloutVariant::Warning
                | crate::doc::model::CalloutVariant::Caution => "#9a6700",
                crate::doc::model::CalloutVariant::Danger
                | crate::doc::model::CalloutVariant::Failure => "#cf222e",
                crate::doc::model::CalloutVariant::Tip
                | crate::doc::model::CalloutVariant::Success => "#1a7f37",
                _ => "#0969da",
            };
            out.push_str(&format!(
                "<div class=\"callout\" style=\"border-color:{accent}\">\n"
            ));
            if let Some(title) = c.display_title() {
                out.push_str(&format!(
                    "<p><strong style=\"color:{accent}\">{}</strong></p>\n",
                    html_escape(&title)
                ));
            }
            for child in &c.body {
                html_block(out, child);
            }
            out.push_str("</div>\n");
        }
        BlockKind::Code { lang, code, .. } => {
            out.push_str(&format!(
                "<pre data-lang=\"{}\"><code>{}</code></pre>\n",
                html_escape(lang),
                html_escape(code)
            ));
        }
        BlockKind::Math { tex } => {
            out.push_str(&format!(
                "<p class=\"math\"><code>{}</code></p>\n",
                html_escape(tex)
            ));
        }
        BlockKind::Table { header, rows, .. } => {
            out.push_str("<table>\n<thead><tr>");
            for cell in header {
                out.push_str(&format!("<th>{}</th>", html_escape(cell)));
            }
            out.push_str("</tr></thead>\n<tbody>\n");
            for row in rows {
                out.push_str("<tr>");
                for cell in row {
                    out.push_str(&format!("<td>{}</td>", html_escape(cell)));
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</tbody></table>\n");
        }
        BlockKind::Rule => out.push_str("<hr>\n"),
        BlockKind::Html { raw } => out.push_str(raw),
        BlockKind::FrontMatter { raw, .. } => {
            out.push_str(&format!("<pre>{}</pre>\n", html_escape(raw)));
        }
        BlockKind::MdxEsm { raw } => {
            out.push_str(&format!(
                "<pre data-kind=\"esm\"><code>{}</code></pre>\n",
                html_escape(raw)
            ));
        }
        BlockKind::MdxJsx {
            name,
            props,
            children,
            ..
        } => {
            out.push_str(&format!(
                "<div class=\"jsx\" data-component=\"{}\" data-props=\"{}\">\n",
                html_escape(name),
                html_escape(props)
            ));
            for child in children {
                html_block(out, child);
            }
            out.push_str("</div>\n");
        }
        BlockKind::MarkdocTag {
            name, attrs, body, ..
        } => {
            out.push_str(&format!(
                "<div class=\"markdoc\" data-tag=\"{}\" data-attrs=\"{}\">\n",
                html_escape(name),
                html_escape(attrs)
            ));
            for child in body {
                html_block(out, child);
            }
            out.push_str("</div>\n");
        }
    }
}

/// The built-in sample document, covering every construct the editor supports.
const SAMPLE: &str = r#"---
title: MarkdownPad 特性示例
---

# MarkdownPad 特性示例

这是一个**所见即所得**的 Markdown 编辑器：你在这里看到的排版*就是*最终结果，
不需要再记 `**加粗**` 这类语法。右下角的「源代码模式」可以随时看到原文。

## 1. 基础排版

行内样式：**加粗**、*斜体*、***粗斜体***、~~删除线~~、==高亮==、`行内代码`、
以及[超链接](https://example.com)。

- 无序列表
- 支持**嵌套**样式
- 任务列表：
- [x] 已完成的事项
- [ ] 未完成的事项

1. 有序列表
2. 第二项

> 这是普通引用。

## 2. LaTeX 公式

行内公式：$E = mc^2$，以及 $a^2 + b^2 = c^2$。

块级公式由本地排版引擎绘制（离线可用）：

$$
\int_{0}^{\infty} \frac{x^{3}}{e^{x} - 1}\, dx = \frac{\pi^{4}}{15}
$$

$$
\begin{pmatrix} a & b \\ c & d \end{pmatrix}
\begin{pmatrix} x \\ y \end{pmatrix}
= \begin{pmatrix} ax + by \\ cx + dy \end{pmatrix}
$$

## 3. 提醒框（四种风格）

GitHub 风格：

> [!NOTE]
> GitHub 的 alert 语法：`> [!NOTE]`。

Obsidian 风格（带自定义标题）：

> [!tip] 自定义标题
> Obsidian 支持标题与折叠标记。

VitePress 风格：

::: tip 提示
VitePress 的容器语法。
:::

Docusaurus 风格：

:::caution 注意
Docusaurus 的 admonition 语法。
:::

## 4. Mermaid 图表（本地渲染，无需联网）

```mermaid
flowchart TD
  A[输入 Markdown] --> B{包含图表?}
  B -->|是| C[本地解析与布局]
  B -->|否| D[直接排版]
  C --> E[绘制到画布]
  D --> E
  E --> F[所见即所得]
```

```mermaid
sequenceDiagram
  participant U as 用户
  participant E as 编辑器
  U->>E: 输入 Mermaid 源码
  E->>E: 解析为图结构
  E-->>U: 立即绘制
```

```mermaid
pie title 渲染耗时占比
  "解析" : 15
  "布局" : 45
  "绘制" : 40
```

## 5. PlantUML 图表（本地渲染）

```plantuml
@startuml
participant "调用方" as C
participant "编辑器内核" as K
C -> K : 打开文档
K --> C : 词法 / 语法分析
C -> K : 编辑某一块
K -> K : 仅重解析受影响区域
K --> C : 增量刷新
@enduml
```

```plantuml
@startuml
class Document {
  +text: String
  +blocks: Vec<Block>
  +reparse()
}
class Block {
  +kind: BlockKind
  +span: Range
}
class Callout
Document "1" *-- "0..*" Block
Block <|-- Callout
@enduml
```

## 6. MDX 与 Markdoc

MDX 允许在 Markdown 里写 import / export 与 JSX：

import Tabs from '@theme/Tabs';

<Callout type="tip">

JSX 组件内部的 Markdown 依然正常排版，包括**加粗**与 `代码`。

</Callout>

Markdoc 使用 `{% %}` 标签：

{% callout type="note" /%}

{% if $showAdvanced %}

条件成立时显示这段内容。

{% /if %}

## 7. 表格

| 特性 | 是否支持 | 说明 |
|:--|:--:|--:|
| LaTeX | 是 | 本地排版 |
| Mermaid | 是 | 本地解析与绘制 |
| PlantUML | 是 | 本地解析与绘制 |
| MDX | 是 | 结构渲染 |
| Markdoc | 是 | 结构渲染 |

---

以上全部内容都在同一份 Markdown 文本中，且**完全离线**渲染。
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::model::{CalloutSyntax, CalloutVariant};

    #[test]
    fn sample_parses_and_exercises_every_feature() {
        let doc = Document::from_file("sample.md".into(), SAMPLE);
        assert!(!doc.blocks.is_empty());

        let has = |f: &dyn Fn(&BlockKind) -> bool| doc.blocks.iter().any(|b| f(&b.kind));
        assert!(has(&|k| matches!(k, BlockKind::FrontMatter { .. })), "front matter");
        assert!(has(&|k| matches!(k, BlockKind::Heading { .. })), "heading");
        assert!(has(&|k| matches!(k, BlockKind::Table { .. })), "table");
        assert!(has(&|k| matches!(k, BlockKind::Math { .. })), "math");
        assert!(has(&|k| matches!(k, BlockKind::Rule)), "rule");
        assert!(has(&|k| matches!(k, BlockKind::MdxEsm { .. })), "mdx esm");
        assert!(has(&|k| matches!(k, BlockKind::MdxJsx { .. })), "mdx jsx");
        assert!(
            has(&|k| matches!(k, BlockKind::MarkdocTag { .. })),
            "markdoc"
        );

        let callouts: Vec<CalloutSyntax> = doc.callout_syntaxes();
        for syntax in CalloutSyntax::all() {
            assert!(
                callouts.contains(&syntax),
                "sample should demonstrate {syntax:?}, found {callouts:?}"
            );
        }

        let diagrams: Vec<_> = doc
            .blocks
            .iter()
            .filter_map(|b| b.diagram_engine())
            .collect();
        assert!(
            diagrams.iter().filter(|e| **e == crate::diagram::Engine::Mermaid).count() >= 3,
            "expected several mermaid fences"
        );
        assert!(
            diagrams
                .iter()
                .filter(|e| **e == crate::diagram::Engine::PlantUml)
                .count()
                >= 2
        );
    }

    #[test]
    fn callout_conversion_covers_all_four_syntaxes() {
        // GitHub → VitePress → Obsidian → Docusaurus → GitHub
        let original = "> [!WARNING]\n> be careful\n";
        let vp = convert_callout(original, CalloutSyntax::VitePress).expect("to vitepress");
        assert!(vp.starts_with(":::warning"));
        assert!(vp.trim_end().ends_with(":::"));

        let ob = convert_callout(&vp, CalloutSyntax::Obsidian).expect("to obsidian");
        assert!(ob.starts_with("> [!warning]"), "{ob}");

        let dx = convert_callout(&ob, CalloutSyntax::Docusaurus).expect("to docusaurus");
        assert!(dx.starts_with(":::caution"), "{dx}");

        let gh = convert_callout(&dx, CalloutSyntax::GitHub).expect("to github");
        // `caution` is one of GitHub's five alert keywords, so it survives
        // verbatim rather than being remapped to `warning`.
        assert!(gh.starts_with("> [!CAUTION]"), "{gh}");

        // Body text must survive the round trip.
        assert!(gh.contains("be careful"));

        // Garbage in, `None` out — never a panic.
        assert!(convert_callout("just a paragraph", CalloutSyntax::GitHub).is_none());
        assert!(convert_callout("> [!", CalloutSyntax::GitHub).is_none());
        assert!(convert_callout(":::", CalloutSyntax::GitHub).is_none());
    }

    #[test]
    fn callout_templates_reparse_into_the_same_syntax() {
        for syntax in CalloutSyntax::all() {
            let text = callout_template(syntax, &CalloutVariant::Tip);
            let doc = Document::from_file("t.md".into(), &text);
            let found = doc.blocks.iter().find_map(|b| match &b.kind {
                BlockKind::Callout(c) => Some(c.syntax),
                _ => None,
            });
            assert_eq!(
                found,
                Some(syntax),
                "template for {syntax:?} did not reparse as {syntax:?}"
            );
        }
    }

    #[test]
    fn resolve_path_walks_into_nested_callouts() {
        let doc = Document::from_file(
            "t.md".into(),
            "::: tip T\n\ninner paragraph\n\n:::\n",
        );
        let callout = &doc.blocks[0];
        assert!(matches!(callout.kind, BlockKind::Callout(_)));
        let inner = resolve_path(&doc.blocks, &[0, 0]).expect("nested block");
        assert!(matches!(inner.kind, BlockKind::Paragraph { .. }));
        assert!(resolve_path(&doc.blocks, &[99]).is_none());
    }

    #[test]
    fn task_toggle_rewrites_only_the_checkbox() {
        assert_eq!(set_task_state("- [ ] todo\n", true), "- [x] todo\n");
        assert_eq!(set_task_state("- [x] done\n", false), "- [ ] done\n");
        // No checkbox present: unchanged.
        assert_eq!(set_task_state("plain\n", true), "plain\n");
    }

    #[test]
    fn source_highlighting_preserves_the_text_exactly() {
        let theme = Theme::dark();
        let src = "# H\n\n- a **b**\n\n```rust\nlet x = 1;\n```\n\n> q\n";
        let job = source_job(src, &theme);
        assert_eq!(job.text, src, "highlighting must not alter the source");
    }

    #[test]
    fn html_export_mentions_all_block_families() {
        let doc = Document::from_file("t.md".into(), SAMPLE);
        let html = render_html(&doc);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<h1>"), "headings");
        assert!(html.contains("<table>"), "tables");
        assert!(html.contains("class=\"callout\""), "callouts");
        assert!(html.contains("<pre"), "code");
        assert!(html.contains("data-component="), "jsx");
        assert!(html.contains("data-tag="), "markdoc");
        // Escaping must be applied to user text.
        assert!(!html.contains("<script>alert"));
    }

    #[test]
    fn html_escape_covers_the_dangerous_characters() {
        assert_eq!(
            html_escape("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
    }
}
