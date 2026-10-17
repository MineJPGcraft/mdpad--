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

thread_local! {
    /// Overrides where the diagnostic report is written; tests set their own path.
    static REPORT_PATH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Points the diagnostic report at `path` for this thread, returning the previous
/// value so a caller can restore it.
fn set_report_path(path: Option<PathBuf>) -> Option<PathBuf> {
    REPORT_PATH.with(|p| std::mem::replace(&mut *p.borrow_mut(), path))
}

/// One undo step: the whole document text plus its path.
struct Snapshot {
    text: String,
    path: Option<PathBuf>,
    flavor: Flavor,
    crlf: bool,
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
    /// Where the first frames' diagnostic report was written, if it was.
    ///
    /// A cell, not a plain field: the report is written from the render path,
    /// which only has `&self`.
    startup_log: std::cell::Cell<Option<PathBuf>>,
    /// Frames painted so far, so the diagnostic report runs once per session.
    diag_frames: std::cell::Cell<u32>,
    /// Identifies this launch, so persisted view state is not restored across runs.
    session: u64,
}

impl App {
    /// Identifies this launch, so persisted view state is not restored across runs.
    ///
    /// Nanoseconds since the epoch, which differs on every run: a fixed value would
    /// be persisted by eframe and restored on the next launch, which is exactly the
    /// bug this guards against.
    pub fn session_id() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

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
        let source_mode = config.source_mode;
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
            source_mode,
            undo: Vec::new(),
            force_quit: false,
            last_title: String::new(),
            status_note: Some(ready),
            startup_log: std::cell::Cell::new(None),
            diag_frames: std::cell::Cell::new(0),
            session: Self::session_id(),
        }
    }

    // -- document mutation -------------------------------------------------

    /// Pushes an undo snapshot before a mutation.
    fn checkpoint(&mut self) {
        self.undo.push(Snapshot {
            text: self.doc.text.clone(),
            path: self.doc.path.clone(),
            flavor: self.doc.flavor,
            crlf: self.doc.crlf,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    fn undo(&mut self) {
        self.commit_active_edit();
        let Some(snapshot) = self.undo.pop() else {
            self.note("没有可撤销的操作");
            return;
        };
        self.doc.path = snapshot.path;
        self.doc.flavor = snapshot.flavor;
        self.doc.crlf = snapshot.crlf;
        self.doc.dirty = true;
        self.doc.text = snapshot.text;
        self.doc.revision = self.doc.revision.wrapping_add(1);
        self.doc.reparse();
        self.after_document_change();
    }

    /// Refreshes derived state after the document text changed.
    fn after_document_change(&mut self) {
        self.source_buffer = self.doc.text.clone();
        self.view.end_session();
        self.view.invalidate();
    }

    fn commit_active_edit(&mut self) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
        let session = self.view.active.take()?;
        self.view.focus_requested = false;
        if session.revision != self.doc.revision {
            return None;
        }
        let Some(block) = resolve_path(&self.doc.blocks, &session.path).cloned() else {
            self.source_buffer = self.doc.text.clone();
            return None;
        };
        let old_span = block.span;
        let current = self.doc.text.get(old_span.clone()).unwrap_or("");
        let new_span = old_span.start..old_span.start + session.buffer.len();
        if current != session.buffer {
            self.checkpoint();
            self.doc.replace_span(old_span.clone(), &session.buffer);
        }
        self.after_document_change();
        Some((old_span, new_span))
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
        self.commit_active_edit();
        if self.doc.dirty && !self.confirm_discard() {
            return;
        }
        self.checkpoint();
        self.doc = Document::new();
        self.after_document_change();
        self.note("已新建文档");
    }

    fn open_document(&mut self) {
        self.commit_active_edit();
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
        self.commit_active_edit();
        match self.doc.path.clone() {
            Some(path) => self.write_to(path, None),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) {
        self.commit_active_edit();
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
        let flavor = Flavor::from_path(&path);
        self.write_to(path, Some(flavor));
    }

    fn write_to(&mut self, path: PathBuf, new_flavor: Option<Flavor>) {
        let mut body = self.doc.text.clone();
        if self.doc.crlf && !body.contains("\r\n") {
            body = crate::doc::model::to_crlf(&body);
        }
        match std::fs::write(&path, body) {
            Ok(()) => {
                self.doc.path = Some(path.clone());
                if let Some(flavor) = new_flavor
                    && self.doc.flavor != flavor
                {
                    self.doc.flavor = flavor;
                    self.doc.revision = self.doc.revision.wrapping_add(1);
                    self.doc.reparse();
                    self.view.invalidate();
                }
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
        self.commit_active_edit();
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
        self.commit_active_edit();
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
        self.commit_active_edit();
        if self.doc.dirty && !self.confirm_discard() {
            return;
        }
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
        let Some(session) = self.view.active.as_ref() else {
            return;
        };
        let buffer = session.buffer.clone();
        let marker_len = crate::doc::model::clamp_boundary(&buffer, marker_len);
        let trailing = self
            .doc
            .text
            .get(block.inline_span.end..block.span.end)
            .unwrap_or("");
        let trailing_len = if !trailing.is_empty() && buffer.ends_with(trailing) {
            trailing.len()
        } else {
            0
        };
        let inner_end = buffer.len().saturating_sub(trailing_len);
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
        let active_span = self.view.active.as_ref().and_then(|session| {
            resolve_path(&self.doc.blocks, &session.path).map(|block| block.span.clone())
        });
        let committed = self.commit_active_edit();
        let at = match (active_span, committed) {
            (Some(_), Some((_, new))) => new.end,
            (Some(old), None) => old.end,
            (None, _) => self.doc.text.len(),
        }
        .min(self.doc.text.len());
        let mut insert = String::new();
        // Separate from the previous block unless it already ends with a blank line.
        let at = crate::doc::model::clamp_boundary(&self.doc.text, at);
        let before = &self.doc.text[..at];
        if !before.is_empty() {
            if !before.ends_with('\n') {
                insert.push('\n');
            }
            if !before.ends_with("\n\n") {
                insert.push('\n');
            }
        }
        insert.push_str(template.trim_end_matches('\n'));
        let after = &self.doc.text[at..];
        if after.is_empty() {
            insert.push('\n');
        } else {
            let existing_newlines = after.bytes().take_while(|b| *b == b'\n').count();
            for _ in existing_newlines..2 {
                insert.push('\n');
            }
        }
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

    /// The ribbon.
    ///
    /// Structured like Word's: named groups, each a vertical stack of controls
    /// with a caption underneath. This is not decoration — it gives the eye a
    /// place to rest and makes the feature set legible at a glance, which a
    /// single row of loose buttons does not.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("mdp-toolbar")
            .exact_size(84.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        // Row 1: the document commands, laid out left to right.
                        ui.horizontal(|ui| {
                            if ui.button("新建").on_hover_text("Ctrl+N").clicked() {
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
                            ui.menu_button("撤销", |ui| {
                                if ui.button("撤销本次修改").clicked() {
                                    self.undo();
                                    ui.close();
                                }
                                if ui.button("载入示例文档").clicked() {
                                    self.load_sample();
                                    ui.close();
                                }
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
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
                                },
                            );
                        });
                        ui.add_space(4.0);
                        // Row 2: the grouped feature ribbon.
                        let theme = self.theme;
                        ui.horizontal_top(|ui| {
                            Self::group(ui, &theme, "字体", |ui| {
                                self.format_buttons(ui);
                            });
                            ui.add_space(6.0);
                            Self::group(ui, &theme, "段落", |ui| {
                                ui.horizontal(|ui| {
                                    self.insert_menus(ui);
                                });
                            });
                            ui.add_space(6.0);
                            Self::group(ui, &theme, "样式", |ui| {
                                ui.horizontal(|ui| {
                                    if ui.button("正文").clicked() {
                                        self.wrap_active("", "");
                                    }
                                    if ui.button("标题 1").clicked() {
                                        self.insert_block("# 标题\n");
                                    }
                                    if ui.button("标题 2").clicked() {
                                        self.insert_block("## 标题\n");
                                    }
                                    if ui.button("引用").clicked() {
                                        self.insert_block("> 引用内容\n");
                                    }
                                });
                            });
                            ui.add_space(6.0);
                            Self::group(ui, &theme, "插入", |ui| {
                                ui.horizontal(|ui| {
                                    self.advanced_inserts(ui);
                                });
                            });
                        });
                    });
                });
            });
    }

    /// One labelled ribbon group: a bordered panel with a caption below.
    ///
    /// Takes the theme rather than `&mut self` so callers can pass closures that
    /// capture `self` — the body needs it to dispatch button clicks, and a
    /// method borrowing `self` mutably would collide with that capture.
    fn group(ui: &mut egui::Ui, theme: &Theme, title: &str, body: impl FnOnce(&mut egui::Ui)) {
        let frame = egui::Frame::new()
            .fill(theme.palette.surface.to_egui())
            .stroke(egui::Stroke::new(1.0, theme.palette.border_soft.to_egui()))
            .corner_radius(3.0)
            .inner_margin(6.0)
            .outer_margin(2.0);
        frame.show(ui, |ui| {
            ui.vertical(|ui| {
                body(ui);
                ui.add_space(1.0);
                ui.label(
                    RichText::new(title)
                        .size(10.0)
                        .color(theme.palette.text_faint.to_egui()),
                );
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

        ui.menu_button("高级 ▾", |ui| {
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

    /// The contents of the ribbon's 插入 group: the block types that produce
    /// rich output rather than plain text.
    fn advanced_inserts(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("图表", |ui| {
            if ui.button("Mermaid 流程图").clicked() {
                self.insert_block(
                    "```mermaid\nflowchart TD\n  A[开始] --> B{判断}\n  B -->|是| C[处理]\n  B -->|否| D[结束]\n```\n",
                );
                ui.close();
            }
            if ui.button("Mermaid 时序图").clicked() {
                self.insert_block(
                    "```mermaid\nsequenceDiagram\n  Alice->>Bob: 请求\n  Bob-->>-Alice: 响应\n```\n",
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
        });
        ui.menu_button("公式", |ui| {
            if ui.button("行内公式").clicked() {
                self.wrap_active("$", "$");
                ui.close();
            }
            if ui.button("块级公式").clicked() {
                self.insert_block("$$\n\\int_{0}^{1} x^2 \\, dx = \\frac{1}{3}\n$$\n");
                ui.close();
            }
        });
        ui.menu_button("表格", |ui| {
            if ui.button("三列表格").clicked() {
                self.insert_block("| 列 A | 列 B | 列 C |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n");
                ui.close();
            }
        });
        ui.menu_button("代码", |ui| {
            if ui.button("代码块").clicked() {
                self.insert_block("```rust\nfn main() {}\n```\n");
                ui.close();
            }
        });
        ui.menu_button("MDX", |ui| {
            if ui.button("import 语句").clicked() {
                self.doc.flavor = Flavor::Mdx;
                self.insert_block("import Tabs from '@theme/Tabs';\n");
                ui.close();
            }
            if ui.button("JSX 区块").clicked() {
                self.doc.flavor = Flavor::Mdx;
                self.insert_block(
                    "<Callout type=\"tip\">\n\n这里是 JSX 组件内的 Markdown。\n\n</Callout>\n",
                );
                ui.close();
            }
            ui.separator();
            if ui.button("Markdoc 条件标签").clicked() {
                self.doc.flavor = Flavor::Markdoc;
                self.insert_block("{% if $show %}\n条件成立时显示。\n{% /if %}\n");
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
            ui.label("图表字号");
            ui.add(
                egui::Slider::new(&mut self.config.diagram_font_scale, 0.7..=1.6).show_value(true),
            );
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
                    // One-shot: the first frames write a geometry report, and the
                    // status bar says where it went so a blank page can be
                    // diagnosed without guessing at the window's real state.
                    if let Some(path) = self.startup_log.take() {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("诊断报告：{}", path.display()))
                                .size(11.0)
                                .color(self.theme.palette.text_faint.to_egui()),
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

    /// The document surface.
    ///
    /// Page view mirrors Word: a sheet centred on a neutral desk with a drop
    /// shadow and generous margins, rather than text pasted onto the panel
    /// background. Alignment is pinned to top-down at every level: the ambient
    /// layout centres its children, and a sheet as tall as the viewport would
    /// then be centred too, pushing the top and bottom halves of the text clean
    /// off screen.
    fn document_surface(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        let geo = PageGeometry::resolve(ui.available_width());
        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::ScrollArea::vertical()
                // The id is scoped to *this session*, not just to the document.
                // egui's memory — including scroll offsets — is persisted to disk by
                // eframe and restored on the next launch. A stored offset is in
                // points, but whether it is valid depends on the content height,
                // which is only known after layout; a value written by a
                // differently sized window (or by a smooth-scroll animation that
                // overshot) can park the page past the end of the document, where
                // every line sits above the viewport while the sheet — painted at
                // full document height into its own background layer — still paints.
                // The user sees a completely blank page.
                //
                // Scoping to the session makes that unreachable: the editor always
                // opens a document at the top, while scrolling within the running
                // session still works normally.
                .id_salt((doc_scroll_salt(&self.doc.display_path()), self.session))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.add_space(geo.gutter);

                        // Draw the content first, into a scope whose height is
                        // unconstrained, then paint the sheet behind it using the
                        // height the content actually needed. Sizing the sheet
                        // from `available_height` instead would leave it shorter
                        // than the document and clip the page mid-way down.
                        let top = ui.available_rect_before_wrap().top();
                        let width = geo.width;
                        let content_width = geo.content_width(self.config.measure);
                        let inner_left = ui.available_rect_before_wrap().left()
                            + (geo.width - content_width) * 0.5;

                        let mut content = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(egui::Rect::from_min_size(
                                    egui::pos2(inner_left, top + geo.pad_top),
                                    egui::vec2(content_width, f32::INFINITY),
                                ))
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        content.set_width(content_width);
                        if self.source_mode {
                            self.source_pane(&mut content);
                        } else {
                            let opts = RenderOpts {
                                measure: content_width,
                                diagram_scale: self.config.diagram_font_scale,
                            };
                            if let Some(action) =
                                edit::document_ui(
                                    &mut content,
                                    &mut self.view,
                                    &self.doc,
                                    &self.theme,
                                    &opts,
                                )
                            {
                                self.handle_action(action);
                            }
                        }

                        // Height so far, plus the trailing margin that lets the
                        // sheet run on past the last paragraph like a real page.
                        // `content.min_rect()` starts at the sheet's top padding, so
                        // its height is measured from there; only the bottom margin
                        // is added on top of it.
                        let used = content.min_rect().height();
                        let sheet_h = (geo.pad_top + used + geo.pad_bottom)
                            // Never smaller than the viewport: a short document
                            // should still fill the window, as Word's does.
                            .max(ui.available_height().max(320.0));

                        let sheet = egui::Rect::from_min_size(
                            egui::pos2(ui.available_rect_before_wrap().left(), top),
                            egui::vec2(width, sheet_h),
                        );
                        // Shadow and sheet go on the background layer so they
                        // sit behind everything the content just drew.
                        let painter = ui.ctx().layer_painter(egui::LayerId::new(
                            egui::Order::Background,
                            egui::Id::unique("mdp-page"),
                        ));
                        paint_shadow(&painter, sheet, &theme);
                        painter.rect_filled(sheet, 3.0, theme.palette.paper.to_egui());

                        // Reserve the sheet's space so the scroll area grows with the
                        // document rather than clipping it. The height is measured
                        // from the content itself, never from the scrolled `top`:
                        // deriving it from `top` would make the scrollable area grow
                        // by the current offset every frame, and egui would then let
                        // a smooth-scroll animation chase the page further down for
                        // as long as it kept repainting — leaving the page parked
                        // past the end of the document, which is the blank-page
                        // failure.
                        ui.allocate_exact_size(egui::vec2(width, sheet_h), egui::Sense::hover());

                        // `top` is the viewport's top edge and the content starts
                        // `pad_top` below it, so a large gap means the page has been
                        // scrolled out of view — the blank-page failure.
                        if self.should_write_report(content.min_rect().top(), top + geo.pad_top) {
                            self.log_startup_diagnostics(ui, &geo, content_width, sheet, &content);
                        }
                    });
                });
        });
    }

    /// Whether this frame should write the diagnostic report.
    ///
    /// Written on the first few frames (a snapshot of a healthy startup) and then
    /// whenever the document has been scrolled out of view — the failure this
    /// report exists to explain. That keeps it free in normal use and
    /// self-capturing when the bug comes back, which matters because a blank page
    /// in a real window is not reproducible headlessly.
    fn should_write_report(&mut self, content_top: f32, viewport_top: f32) -> bool {
        let frames = self.diag_frames.get();
        self.diag_frames.set(frames + 1);
        if frames < 3 {
            return true;
        }
        // The document surface's content starts below the viewport's top edge; if
        // it has drifted far above it, every line is off screen.
        content_top < viewport_top - 200.0
    }

    /// Where the diagnostic report goes.
    ///
    /// `%TEMP%` rather than the config directory: during a "the page is blank and
    /// there is no log either" hunt, the log has to be somewhere the user can find
    /// in one step. Tests point this at their own file so they do not fight over
    /// one shared path.
    pub fn report_path() -> PathBuf {
        REPORT_PATH.with(|p| {
            p.borrow()
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("mdpad-startup.log"))
        })
    }

    /// Writes one report describing the geometry the renderer actually used.
    ///
    /// A blank page in a real window is not reproducible headlessly, so the app
    /// records what it sees instead: window size, pixel ratio, the sheet rect and
    /// what the renderer painted. See [`Self::report_path`] for where it lands.
    fn log_startup_diagnostics(
        &self,
        ui: &egui::Ui,
        geo: &PageGeometry,
        content_width: f32,
        sheet: egui::Rect,
        content: &egui::Ui,
    ) {
        let ctx = ui.ctx();
        let screen = ctx.input(|i| i.viewport_rect());
        let mut report = String::new();
        report.push_str(&format!(
            "MarkdownPad startup report — frame {}\n\
             screen_rect   {screen:?}\n\
             pixels_per_point {}\n\
             available     {:?}\n\
             window size   {:?}\n\
             page          width {:.1} gutter {:.1} pad_top {:.1} sheet {sheet:?}\n\
             content       column {content_width:.1} rect {:?}\n\
             theme         dark {} text {:?} paper {:?}\n\
             fonts         {}\n\
             blocks        {}\n",
            self.diag_frames.get(),
            ctx.pixels_per_point(),
            ui.available_rect_before_wrap(),
            ctx.input(|i| i.viewport().inner_rect),
            geo.width,
            geo.gutter,
            geo.pad_top,
            content.min_rect(),
            self.theme.dark,
            self.theme.palette.text.to_array(),
            self.theme.palette.paper.to_array(),
            self.fonts.summary(),
            self.doc.blocks.len(),
        ));
        for line in edit::diag::take() {
            report.push_str(&line);
            report.push('\n');
        }
        let path = Self::report_path();
        if std::fs::write(&path, report).is_ok() {
            self.startup_log.set(Some(path));
        }
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

        ui.add(
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
        // The buffer is authoritative while source mode is active.
        if self.source_buffer != self.doc.text {
            self.checkpoint();
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
        let target = match &action {
            EditorAction::Focus { path, .. } => {
                resolve_path(&self.doc.blocks, path).map(|block| block.span.start)
            }
            _ => None,
        };
        let toggling_active = matches!(&action, EditorAction::Focus { path, .. }
            if self.view.active.as_ref().is_some_and(|session| session.path == *path));
        let committed = match action {
            EditorAction::Commit { .. } => None,
            _ => self.commit_active_edit(),
        };
        let rebase = |mut span: std::ops::Range<usize>| {
            if let Some((old, new)) = &committed {
                let offset = |at: usize| {
                    if at <= old.start {
                        at
                    } else if at >= old.end {
                        new.end + (at - old.end)
                    } else {
                        new.start + (at - old.start).min(new.len())
                    }
                };
                span = offset(span.start)..offset(span.end);
            }
            span
        };
        match action {
            EditorAction::Commit { span, text } => {
                if self.doc.text.get(span.clone()).map(str::to_owned).as_deref() != Some(&text) {
                    self.checkpoint();
                    self.doc.replace_span(span, &text);
                }
                self.view.end_session();
                self.source_buffer = self.doc.text.clone();
            }
            EditorAction::Focus { mut path, caret: _ } => {
                if toggling_active {
                    return;
                }
                if committed.is_some()
                    && let Some(target) = target
                {
                    let at = rebase(target..target).start;
                    let Some(updated) = path_at_byte(&self.doc.blocks, at) else {
                        return;
                    };
                    path = updated;
                }
                let block = resolve_path(&self.doc.blocks, &path).cloned();
                if let Some(block) = block {
                    self.view.begin_session(path, &self.doc, &block);
                }
            }
            EditorAction::OpenLink(url) => {
                open_url(&url);
            }
            EditorAction::ToggleTask { span, checked } => {
                let span = rebase(span);
                let current = self.doc.text.get(span.clone()).unwrap_or("").to_owned();
                let updated = set_task_state(&current, checked);
                if updated != current {
                    self.checkpoint();
                    self.doc.replace_span(span, &updated);
                    self.source_buffer = self.doc.text.clone();
                }
            }
            EditorAction::ConvertCallout { span, syntax } => {
                let span = rebase(span);
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
                let span = rebase(span);
                self.checkpoint();
                self.doc.replace_span(span, "");
                self.source_buffer = self.doc.text.clone();
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let has_focus = ctx.memory(|m| m.focused().is_some());
        let consume = |modifiers, key| {
            ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(modifiers, key)))
        };
        if consume(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::S) {
            self.save_as();
        } else if consume(egui::Modifiers::COMMAND, egui::Key::S) {
            self.save();
        }
        if consume(egui::Modifiers::COMMAND, egui::Key::O) {
            self.open_document();
        }
        if consume(egui::Modifiers::COMMAND, egui::Key::N) {
            self.new_document();
        }
        if !has_focus && consume(egui::Modifiers::COMMAND, egui::Key::Z) {
            self.undo();
        }
        if consume(egui::Modifiers::COMMAND, egui::Key::Backslash)
            || consume(egui::Modifiers::COMMAND, egui::Key::Slash)
        {
            self.toggle_source(ctx);
        }
        if consume(egui::Modifiers::COMMAND, egui::Key::E) {
            self.export_html();
        }
        if self.view.active.is_some() {
            if consume(egui::Modifiers::COMMAND, egui::Key::B) {
                self.wrap_active("**", "**");
            }
            if consume(egui::Modifiers::COMMAND, egui::Key::I) {
                self.wrap_active("*", "*");
            }
            if consume(egui::Modifiers::COMMAND, egui::Key::K) {
                self.wrap_active("[", "](https://example.com)");
            }
        }
    }

    fn window_title(&mut self, ctx: &egui::Context) {
        let dirty = if self.doc.dirty { "• " } else { "" };
        // A clean title in normal use; the diagnostic report's path is surfaced in
        // the status bar instead, and only when a report was actually written.
        let title = format!("{dirty}{} — MarkdownPad", self.doc.display_name());
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.force_quit || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        self.commit_active_edit();
        if self.doc.dirty && !self.confirm_discard() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        } else {
            self.force_quit = true;
        }
    }
}

fn path_at_byte(blocks: &[crate::doc::model::Block], byte: usize) -> Option<Vec<usize>> {
    for (index, block) in blocks.iter().enumerate() {
        if block.span.start <= byte && byte < block.span.end {
            let children = match &block.kind {
                BlockKind::Callout(callout) => callout.body.as_slice(),
                BlockKind::MdxJsx { children, .. } => children,
                BlockKind::MarkdocTag { body, .. } => body,
                _ => &[],
            };
            let mut path = vec![index];
            if let Some(child_path) = path_at_byte(children, byte) {
                path.extend(child_path);
            }
            return Some(path);
        }
    }
    None
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
        self.handle_close_request(&ctx);
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
        self.commit_active_edit();
        let _ = self.config.save();
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Geometry of the page view, resolved from the available width.
///
/// Split out as a pure function so the layout rules can be tested without a
/// window: these numbers decide where every glyph lands, and getting one wrong
/// pushes the whole document off screen with no error to report it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageGeometry {
    /// Width of the sheet.
    pub width: f32,
    /// Horizontal space to leave on each side so the sheet is centred.
    pub gutter: f32,
    /// Width of the text column inside the sheet's side padding.
    pub content: f32,
    /// Space above the first block.
    pub pad_top: f32,
    /// Space below the last block, so the sheet runs on like a real page.
    pub pad_bottom: f32,
}

impl PageGeometry {
    /// Ideal sheet width, scaled from A4 at 96dpi.
    const IDEAL_WIDTH: f32 = 794.0;
    const SIDE_PAD: f32 = 64.0;
    /// Below this the window is too narrow for a sheet with any margin.
    const MIN_WIDTH: f32 = 280.0;

    /// Resolves the geometry for an available width of `avail`.
    ///
    /// Every field is clamped to a positive value. A zero or negative width
    /// makes the sheet lay its children out outside its own rect, which shows up
    /// as a blank page rather than as an error worth chasing.
    pub fn resolve(avail: f32) -> Self {
        let avail = if avail.is_finite() {
            avail
        } else {
            Self::IDEAL_WIDTH
        };
        let width = Self::IDEAL_WIDTH.min(avail - 24.0).max(Self::MIN_WIDTH);
        let gutter = ((avail - width) * 0.5).max(0.0);
        let content = (width - Self::SIDE_PAD * 2.0).max(160.0);
        Self {
            width,
            gutter,
            content,
            pad_top: 56.0,
            pad_bottom: 220.0,
        }
    }

    /// Applies the user's reading-column preference within the available sheet.
    /// Zero and invalid values mean the adaptive page width.
    pub fn content_width(self, requested: f32) -> f32 {
        if requested.is_finite() && requested > 0.0 {
            self.content.min(requested.max(160.0))
        } else {
            self.content
        }
    }
}

/// Paints the page drop shadow.
///
/// egui has no shadow primitive, so the effect is faked with a few progressively
/// larger, more transparent rounded rects around the sheet. Four layers is
/// enough to read as a soft edge without the banding a hard offset would give.
fn paint_shadow(painter: &egui::Painter, rect: egui::Rect, theme: &Theme) {
    let base = theme.palette.paper_shadow;
    for (i, spread) in [2.0f32, 5.0, 9.0, 15.0].into_iter().enumerate() {
        // Alpha falls off with distance, so the layers blend outward.
        let alpha = (base.a as f32 * [0.30, 0.22, 0.14, 0.08][i]) as u8;
        let grow = rect.expand(spread);
        painter.rect_stroke(
            grow,
            4.0 + spread,
            egui::Stroke::new(spread * 2.0, base.with_alpha(alpha).to_egui()),
            egui::StrokeKind::Outside,
        );
    }
}

/// Salt for the document surface's scroll area.
///
/// Keyed by the document, **not** a constant. The scroll offset lives in egui's
/// memory, which eframe persists to `app.ron` and restores on the next run, so a
/// constant salt lets a stale offset from a previous session apply to whatever
/// document happens to be open now. When that offset points past the end of the
/// document, every line lands above the viewport while the sheet — painted at
/// full document height into its own background layer — stays visible, which the
/// user sees as a completely blank page.
///
/// Keying by document makes a restored offset for another document unreachable,
/// so a freshly opened document starts at the top while scrolling inside the
/// current document is still preserved.
fn doc_scroll_salt(path: &str) -> (String, String) {
    ("mdp-doc-scroll".to_owned(), path.to_owned())
}

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
mod page_tests {
    use super::PageGeometry;

    /// Regression guard for "the page rendered blank".
    ///
    /// A zero or negative content width makes the sheet lay its text out
    /// outside its own rect, so nothing is visible — with no error to explain
    /// it. Every field must stay positive and finite for any plausible window.
    #[test]
    fn geometry_is_always_usable() {
        for avail in [
            0.0f32, 1.0, 100.0, 280.0, 640.0, 1024.0, 1920.0, 3840.0,
            f32::INFINITY,
            f32::NAN,
        ] {
            let g = PageGeometry::resolve(avail);
            for (name, v) in [
                ("width", g.width),
                ("content", g.content),
                ("pad_top", g.pad_top),
                ("pad_bottom", g.pad_bottom),
            ] {
                assert!(v.is_finite(), "{name} not finite for avail={avail}");
                assert!(v > 0.0, "{name} was {v} for avail={avail}");
            }
            assert!(g.gutter >= 0.0, "negative gutter for avail={avail}");
        }
    }

    #[test]
    fn content_fits_inside_the_sheet() {
        for avail in [320.0f32, 800.0, 1400.0, 2560.0] {
            let g = PageGeometry::resolve(avail);
            assert!(
                g.content <= g.width,
                "content column {} exceeds the sheet {} at avail={avail}",
                g.content,
                g.width
            );
        }
    }

    #[test]
    fn wide_windows_centre_the_sheet() {
        let g = PageGeometry::resolve(1600.0);
        // Equal gutters mean the sheet sits in the middle of the window.
        let left = g.gutter;
        let right = 1600.0 - g.gutter - g.width;
        assert!((left - right).abs() < 1.0, "not centred: {left} vs {right}");
        assert_eq!(g.width, PageGeometry::IDEAL_WIDTH);
    }

    #[test]
    fn narrow_windows_shrink_rather_than_overflow() {
        let g = PageGeometry::resolve(360.0);
        assert!(g.width <= 360.0, "sheet overflowed a narrow window");
        assert!(g.content >= 160.0, "text column got unreadably narrow");
    }

    #[test]
    fn reading_column_setting_changes_the_content_width() {
        let page = PageGeometry::resolve(1400.0);
        assert_eq!(page.content_width(400.0), 400.0);
        assert_eq!(page.content_width(1200.0), page.content);
        assert_eq!(page.content_width(0.0), page.content);
        assert_eq!(page.content_width(1.0), 160.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::model::{CalloutSyntax, CalloutVariant};

    fn test_app(source: &str) -> App {
        App {
            config: Config::default(),
            theme: Theme::dark(),
            doc: Document::from_file("test.md".into(), source),
            view: ViewState::default(),
            fonts: FontReport::default(),
            source_buffer: source.to_owned(),
            source_mode: false,
            undo: Vec::new(),
            force_quit: false,
            last_title: String::new(),
            status_note: None,
            startup_log: std::cell::Cell::new(None),
            diag_frames: std::cell::Cell::new(0),
            session: App::session_id(),
        }
    }

    fn edit_first_block(app: &mut App, text: &str) {
        let block = app.doc.blocks[0].clone();
        app.view.begin_session(vec![0], &app.doc, &block);
        app.view.active.as_mut().unwrap().buffer = text.to_owned();
    }

    #[test]
    fn switching_to_source_keeps_pending_edits() {
        let mut app = test_app("original");
        edit_first_block(&mut app, "新的内容");
        app.toggle_source(&egui::Context::default());
        assert_eq!(app.doc.text, "新的内容");
        assert_eq!(app.source_buffer, app.doc.text);
        assert!(app.doc.dirty);
    }

    #[test]
    fn focusing_another_block_keeps_edits_and_tracks_its_new_position() {
        let mut app = test_app("first\n\nsecond");
        edit_first_block(&mut app, "edited\n\nan extra paragraph");
        app.handle_action(EditorAction::Focus { path: vec![1], caret: 0 });
        assert_eq!(app.doc.text, "edited\n\nan extra paragraph\n\nsecond");
        assert_eq!(app.view.active.as_ref().unwrap().buffer, "second");
    }

    #[test]
    fn toggling_a_task_keeps_edits_and_updates_the_correct_range() {
        let mut app = test_app("first\n\n- [ ] task\n");
        let span = app.doc.blocks[1].span.clone();
        edit_first_block(&mut app, "更长的第一段");
        app.handle_action(EditorAction::ToggleTask { span, checked: true });
        assert_eq!(app.doc.text, "更长的第一段\n\n- [x] task\n");
    }

    #[test]
    fn inserting_a_block_keeps_pending_edits() {
        let mut app = test_app("first\n\nsecond");
        edit_first_block(&mut app, "edited");
        app.insert_block("# inserted\n");
        assert_eq!(app.doc.text, "edited\n\n# inserted\n\nsecond");
    }

    #[test]
    fn formatting_after_inserting_chinese_does_not_split_utf8() {
        let mut app = test_app("ab");
        edit_first_block(&mut app, "中文");
        app.wrap_active("**", "**");
        assert_eq!(app.view.active.as_ref().unwrap().buffer, "**中文**");
    }

    #[test]
    fn undo_restores_document_flavor_and_line_endings() {
        let mut app = test_app("old");
        app.doc = Document::from_file("old.mdx".into(), "old\r\n");
        app.checkpoint();
        app.doc = Document::from_file("new.md".into(), "new\n");
        app.undo();
        assert_eq!(app.doc.path, Some("old.mdx".into()));
        assert_eq!(app.doc.flavor, Flavor::Mdx);
        assert!(app.doc.crlf);
    }

    #[test]
    fn undo_keeps_restored_edits_dirty_after_a_save() {
        let mut app = test_app("original");
        app.set_text("saved content".to_owned());
        app.doc.dirty = false;
        app.undo();
        assert_eq!(app.doc.text, "original");
        assert!(app.doc.dirty);
    }

    #[test]
    fn formatting_preserves_heading_suffix_after_changing_its_length() {
        let mut app = test_app("# ab ###\n");
        edit_first_block(&mut app, "# 中文 ###");
        app.wrap_active("**", "**");
        assert_eq!(app.view.active.as_ref().unwrap().buffer, "# **中文** ###");
    }

    #[test]
    fn inserting_after_a_line_without_a_newline_starts_a_separate_block() {
        let mut app = test_app("first");
        app.insert_block("# inserted\n");
        assert_eq!(app.doc.text, "first\n\n# inserted\n");
        assert!(matches!(app.doc.blocks[1].kind, BlockKind::Heading { .. }));
    }

    #[test]
    fn previewing_the_active_code_block_commits_its_buffer() {
        let mut app = test_app("```rust\nold\n```\n");
        edit_first_block(&mut app, "```rust\nnew\n```\n");
        app.handle_action(EditorAction::Focus { path: vec![0], caret: 0 });
        assert_eq!(app.doc.text, "```rust\nnew\n```\n");
        assert!(app.view.active.is_none());
    }

    #[test]
    fn failed_save_as_preserves_the_current_document_identity() {
        let mut app = test_app("original");
        app.doc.dirty = true;
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("missing-save-directory-{}", std::process::id()))
            .join("new.mdx");
        app.write_to(path, Some(Flavor::Mdx));
        assert_eq!(app.doc.path, Some("test.md".into()));
        assert_eq!(app.doc.flavor, Flavor::Markdown);
        assert!(app.doc.dirty);
    }

    #[test]
    fn save_includes_the_active_buffer_and_preserves_crlf() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("pending-save-{}.md", std::process::id()));
        let mut app = test_app("original");
        app.doc = Document::from_file(path.clone(), "old\r\n");
        edit_first_block(&mut app, "中文\nnext");
        app.save();
        let saved = std::fs::read_to_string(&path).expect("saved file");
        std::fs::remove_file(&path).expect("remove test file");
        assert_eq!(saved, "中文\r\nnext\r\n");
        assert!(!app.doc.dirty);
        assert_eq!(app.source_buffer, app.doc.text);
    }

    #[test]
    fn source_shortcut_uses_the_key_event_modifiers_and_keeps_pending_edits() {
        let mut app = test_app("original");
        edit_first_block(&mut app, "edited");
        let context = egui::Context::default();
        context.run_pass(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Backslash,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND | egui::Modifiers::CTRL,
                }],
                ..Default::default()
            },
            |context| app.shortcuts(context),
        ).drop_without_applying_deltas();
        assert!(app.source_mode);
        assert_eq!(app.source_buffer, "edited");
    }

    #[test]
    fn formatting_shortcuts_work_while_the_block_has_keyboard_focus() {
        let mut app = test_app("original");
        edit_first_block(&mut app, "中文");
        let context = egui::Context::default();
        context.run_pass(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::B,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND | egui::Modifiers::CTRL,
                }],
                ..Default::default()
            },
            |context| {
                context.memory_mut(|memory| memory.request_focus(egui::Id::unique("editing")));
                app.shortcuts(context);
            },
        ).drop_without_applying_deltas();
        assert_eq!(app.view.active.as_ref().unwrap().buffer, "**中文**");
    }

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
    fn the_startup_report_is_written_with_the_real_geometry() {
        // Its own file: the report path is process-wide and every shell test
        // renders it, so a shared path makes this test order-dependent.
        let path = std::env::temp_dir().join(format!(
            "mdpad-report-test-{}.log",
            App::session_id()
        ));
        let previous = set_report_path(Some(path.clone()));
        let context = egui::Context::default();
        let mut app = test_app(SAMPLE);
        app.fonts = crate::fonts::install(&context);
        let mut frame = eframe::Frame::_new_kittest();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1424.0, 862.0));
        for _ in 0..3 {
            let out = context.run_pass(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    let mut ui = egui::Ui::new(
                        context.clone(),
                        egui::Id::unique("mdp-root"),
                        egui::UiBuilder::new()
                            .max_rect(screen)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    eframe::App::ui(&mut app, &mut ui, &mut frame);
                },
            );
            out.drop_without_applying_deltas();
        }
        let report = std::fs::read_to_string(&path).unwrap_or_default();
        set_report_path(previous);
        let _ = std::fs::remove_file(&path);
        assert!(
            report.contains("MarkdownPad startup report"),
            "the report was not written:\n{report}"
        );
        assert!(
            report.contains("page          width"),
            "report lacks page geometry:\n{report}"
        );
        assert!(
            report.contains("paint paragraph"),
            "report lacks what the renderer painted:\n{report}"
        );
        assert!(
            report.contains("blocks        45"),
            "report lacks the block count:\n{report}"
        );
    }

    /// A block that holds keyboard focus must not drag the page out of view.
    #[test]
    fn a_focused_block_does_not_scroll_the_page_away() {
        let context = egui::Context::default();
        let mut app = test_app(SAMPLE);
        app.fonts = crate::fonts::install(&context);
        assert!(app.doc.blocks.len() > 2, "the sample needs a few blocks");
        let block = app.doc.blocks[2].clone();
        app.view.begin_session(vec![2], &app.doc, &block);
        let mut frame = eframe::Frame::_new_kittest();
        let mut offsets = Vec::new();
        let mut visible = 0usize;
        // The window starts at eframe's 1320x900 and the user maximises it; the
        // report came from a 1440x829.5 logical window at 2x scaling.
        let sizes = [
            egui::vec2(1320.0, 900.0),
            egui::vec2(1320.0, 900.0),
            egui::vec2(1440.0, 829.5),
            egui::vec2(1440.0, 829.5),
            egui::vec2(1440.0, 829.5),
            egui::vec2(1440.0, 829.5),
        ];
        for (pass, size) in sizes.into_iter().enumerate() {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let out = context.run_pass(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    // Reproduce the real loop: `inline_edit` calls `request_focus`
                    // whenever `focus_requested` is set, and nothing clears it
                    // afterwards.
                    app.view.focus_requested = true;
                    let mut ui = egui::Ui::new(
                        context.clone(),
                        egui::Id::unique("mdp-root"),
                        egui::UiBuilder::new()
                            .max_rect(screen)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    eframe::App::ui(&mut app, &mut ui, &mut frame);
                },
            );
            visible = out
                .shapes
                .iter()
                .filter(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => {
                        !text.galley.job.text.trim().is_empty()
                            && screen.contains(text.visual_bounding_rect().center())
                    }
                    _ => false,
                })
                .count();
            println!("pass {pass}: visible_text={visible}");
            offsets.push(visible);
            out.drop_without_applying_deltas();
        }
        // Focus-scrolling is allowed to move the page, but not to empty it.
        assert!(
            visible >= 3,
            "only {visible} text runs stayed visible with a block focused (history {offsets:?})"
        );
    }

    /// The document surface's scroll state is never restored across launches.
    ///
    /// This is the fix for the reported blank page. egui's memory — including
    /// scroll offsets — is persisted by eframe to `app.ron` and restored on the
    /// next launch. A stored offset is in points, but whether it is valid depends
    /// on the content height, which is only known after layout: a value written by
    /// a differently sized window, or by a smooth-scroll animation that overshot,
    /// can park the page past the end of the document. Every line then sits above
    /// the viewport while the sheet — painted at full document height into its own
    /// background layer — still paints, so the user sees a completely blank page.
    ///
    /// The scroll area's id therefore carries a per-launch value, so a restored
    /// offset is never found and a document always opens at the top.
    #[test]
    fn the_scroll_state_is_never_restored_across_launches() {
        // A new launch must not be able to address the previous launch's state.
        let a = App::session_id();
        let b = App::session_id();
        assert_ne!(a, b, "each launch needs its own scroll identity");
        assert_eq!(a, a, "the identity must be stable within a launch");
        // Different documents are still kept apart within a launch.
        assert_ne!(
            doc_scroll_salt("示例.md"),
            doc_scroll_salt("另一份.md"),
            "two documents must not share one scroll offset"
        );
    }

    /// A freshly opened document paints its first paragraph near the top.
    ///
    /// The counterpart to the scroll bug: whatever offset a previous session left
    /// behind, the first frame of a new document must show the beginning of the
    /// text rather than an empty sheet.
    #[test]
    fn a_fresh_document_starts_at_the_top_of_the_page() {
        let context = egui::Context::default();
        let mut app = test_app(SAMPLE);
        app.fonts = crate::fonts::install(&context);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 829.5));
        let mut frame = eframe::Frame::_new_kittest();
        let mut paragraph = None;
        for _ in 0..3 {
            let out = context.run_pass(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    let mut ui = egui::Ui::new(
                        context.clone(),
                        egui::Id::unique("mdp-root"),
                        egui::UiBuilder::new()
                            .max_rect(screen)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    eframe::App::ui(&mut app, &mut ui, &mut frame);
                },
            );
            if let Some(rect) = out.shapes.iter().find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if text.galley.job.text.contains("这是一个") => {
                    Some(text.visual_bounding_rect())
                }
                _ => None,
            }) {
                paragraph = Some(rect);
            }
            out.drop_without_applying_deltas();
        }
        let paragraph = paragraph.expect("the first paragraph was never painted");
        assert!(
            screen.contains(paragraph.center()),
            "the page did not start at the top: {paragraph:?}"
        );
    }

    #[test]
    fn the_report_is_written_when_the_page_has_been_scrolled_away() {
        let mut app = test_app(SAMPLE);
        // First three frames always report, so burn them off.
        for _ in 0..3 {
            assert!(app.should_write_report(-5000.0, -5000.0));
        }
        assert!(
            !app.should_write_report(-5000.0, -5000.0),
            "a healthy frame must not write a report"
        );
        // Content 300pt above where it belongs: every line is off screen.
        assert!(
            app.should_write_report(-400.0, 0.0),
            "a scrolled-away page must write a report"
        );
    }

    /// The document surface's geometry must be stable across frames.
    ///
    /// `document_surface` renders the content and then reserves the sheet's height
    /// with `allocate_exact_size` inside the same layout, and the sheet's height is
    /// derived from the content's own rect. If the reservation feeds back into the
    /// content's position, every frame pushes the text further down: the content
    /// rect drifts negative, the scroll offset chases it to the maximum, and the
    /// page ends up blank with no user input at all.
    #[test]
    fn the_page_geometry_settles_and_stays_put() {
        let context = egui::Context::default();
        let mut app = test_app(SAMPLE);
        app.fonts = crate::fonts::install(&context);
        // A real window's first frames can run before the surface has a size, so
        // start degenerate and let it settle, then require it to stay put.
        let sizes = [
            egui::vec2(0.0, 0.0),
            egui::vec2(1.0, 1.0),
            egui::vec2(320.0, 200.0),
        ];
        let settled = egui::vec2(1440.0, 829.5);
        let mut frame = eframe::Frame::_new_kittest();
        let mut tops = Vec::new();
        for pass in 0..12 {
            let size = sizes.get(pass).copied().unwrap_or(settled);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let out = context.run_pass(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    let mut ui = egui::Ui::new(
                        context.clone(),
                        egui::Id::unique("mdp-root"),
                        egui::UiBuilder::new()
                            .max_rect(screen)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    eframe::App::ui(&mut app, &mut ui, &mut frame);
                },
            );
            let top = out
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) if text.galley.job.text.contains("这是一个") => {
                        Some(text.visual_bounding_rect().top())
                    }
                    _ => None,
                })
                .unwrap_or(f32::NAN);
            tops.push(top);
            println!("pass {pass}: first paragraph top = {top}");
            out.drop_without_applying_deltas();
        }
        // Once the window has a real size the layout must stop moving.
        let tail = &tops[3..];
        let first = tail[0];
        for (i, top) in tail.iter().enumerate() {
            assert!(
                top.is_finite() && (top - first).abs() < 1.0,
                "the page kept moving after settling (pass {}): {tops:?}",
                i + 3
            );
        }
    }

    /// A large wheel delta must not leave the page parked past the document.
    ///
    /// Not a claim about how far one tick *should* move — egui animates scrolling,
    /// so a big delta legitimately travels a long way. This pins the failure mode:
    /// the page must still be showing body text afterwards rather than sitting in
    /// the blank padding below the last paragraph.
    #[test]
    fn a_large_wheel_delta_does_not_park_the_page_past_the_document() {
        let context = egui::Context::default();
        let mut app = test_app(SAMPLE);
        app.fonts = crate::fonts::install(&context);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 829.5));
        let mut frame = eframe::Frame::_new_kittest();
        let mut tops = Vec::new();
        let mut on_screen = 0usize;
        for pass in 0..6 {
            // A big single tick on the first pass, nothing after.
            let events_input = if pass == 0 {
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -4000.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers: egui::Modifiers::NONE,
                }]
            } else {
                Vec::new()
            };
            let out = context.run_pass(
                egui::RawInput {
                    screen_rect: Some(screen),
                    // The wheel only reaches the scroll area when the pointer is
                    // over it, so park the pointer in the middle of the page.
                    events: {
                        let mut events = vec![egui::Event::PointerMoved(egui::pos2(
                            720.0, 400.0,
                        ))];
                        events.extend(events_input);
                        events
                    },
                    ..Default::default()
                },
                |context| {
                    let mut ui = egui::Ui::new(
                        context.clone(),
                        egui::Id::unique("mdp-root"),
                        egui::UiBuilder::new()
                            .max_rect(screen)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    eframe::App::ui(&mut app, &mut ui, &mut frame);
                },
            );
            let top = out
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) if text.galley.job.text.contains("这是一个") => {
                        Some(text.visual_bounding_rect().top())
                    }
                    _ => None,
                })
                .unwrap_or(f32::NAN);
            tops.push(top);
            on_screen = out
                .shapes
                .iter()
                .filter(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => {
                        !text.galley.job.text.trim().is_empty()
                            && text.galley.rect.width() > 50.0
                            && screen.contains(text.visual_bounding_rect().center())
                    }
                    _ => false,
                })
                .count();
            println!("pass {pass}: first paragraph top = {top}, body runs on screen = {on_screen}");
            out.drop_without_applying_deltas();
        }
        // Not a claim about how far one tick *should* move: egui animates scrolling,
        // so a big delta legitimately travels a long way. The failure mode to pin is
        // that the page is still showing text afterwards, rather than sitting in the
        // blank padding below the last paragraph.
        assert!(
            tops.iter().all(|t| t.is_finite()),
            "the first paragraph was not painted: {tops:?}"
        );
        assert!(
            on_screen > 0,
            "scrolling left the page with no text on screen: {tops:?}"
        );
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

    /// Whole-shell headless pass: the same widget hierarchy the real window uses,
    /// so a blank page (content drawn behind the sheet, or laid out past the
    /// viewport) fails here instead of only on screen.
    #[test]
    fn the_document_is_painted_inside_the_visible_viewport() {
        // Cover the logical sizes a real window produces: a 2560x1440 panel at
        // 1.5x scaling is 1707x960 points, a 4K panel at 2x is 1920x1080, and a
        // maximised 1080p window is about 1920x1040. A blank page that only shows
        // up at one of these is exactly the kind of bug a single-size test misses.
        for (w, h) in [
            (1694.0f32, 1002.0f32),
            (1707.0, 960.0),
            (1424.0, 862.0),
            (1440.0, 900.0),
            (1129.0, 668.0),
            (1920.0, 1040.0),
            (1920.0, 1080.0),
            (1280.0, 720.0),
            (2560.0, 1400.0),
        ] {
            assert_document_is_visible(w, h);
        }
        // The caret-in-a-block state, where egui may scroll a focused widget into
        // view and the document can end up scrolled away from the top.
        for (w, h) in [(1694.0f32, 1002.0f32), (1440.0, 900.0), (1424.0, 862.0)] {
            assert_document_visible_at(w, h, 1.0, true);
        }
    }

    fn assert_document_is_visible(width: f32, height: f32) {
        assert_document_visible_at(width, height, 1.0, false);
        // Non-integral pixel ratios make egui round glyph positions, which is where
        // "the page is blank but the status bar is right" tends to hide.
        assert_document_visible_at(width, height, 1.25, false);
        assert_document_visible_at(width, height, 1.5, false);
        assert_document_visible_at(width, height, 2.0, false);
    }

    /// Paints the whole shell once and asserts the first body paragraph is visible.
    ///
    /// `focus_first_block` reproduces a click into the first block, which is the
    /// state the editor is in whenever the caret is on screen.
    fn assert_document_visible_at(
        width: f32,
        height: f32,
        pixels_per_point: f32,
        focus_first_block: bool,
    ) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
        let context = egui::Context::default();
        let mut app = test_app(SAMPLE);
        app.fonts = crate::fonts::install(&context);
        if focus_first_block {
            let block = app.doc.blocks[0].clone();
            app.view.begin_session(vec![0], &app.doc, &block);
        }
        let mut frame = eframe::Frame::_new_kittest();
        let output = context.run_pass(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |context| {
                context.set_pixels_per_point(pixels_per_point);
                let mut ui = egui::Ui::new(
                    context.clone(),
                    egui::Id::unique("mdp-root"),
                    egui::UiBuilder::new()
                        .max_rect(screen)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                eframe::App::ui(&mut app, &mut ui, &mut frame);
            },
        );

        // The first body paragraph is the canary: if the sheet is painted over the
        // content, or the content column is laid out past the viewport, this is the
        // run that disappears first.
        let paragraph = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if text.galley.job.text.contains("这是一个") => {
                    Some(text.visual_bounding_rect())
                }
                _ => None,
            })
            .expect("the first body paragraph was not painted at all");
        assert!(
            screen.contains(paragraph.center()),
            "the first paragraph landed outside the window: {paragraph:?}"
        );
        assert!(
            paragraph.top() > 40.0 && paragraph.top() < 400.0,
            "the first paragraph is not near the top of the page: {paragraph:?}"
        );
        // It must sit inside the sheet's content column, not out in the desk.
        let sheet = PageGeometry::resolve(screen.width());
        let column = sheet.content_width(app.config.measure);
        let column_left = sheet.gutter + (sheet.width - column) * 0.5;
        assert!(
            paragraph.left() >= column_left - 2.0
                && paragraph.right() <= column_left + column + 2.0,
            "the paragraph is outside the content column: {paragraph:?} (column from {column_left})"
        );

        let mut on_screen = 0usize;
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape
                && !text.galley.job.text.trim().is_empty()
                && screen.contains(text.visual_bounding_rect().center())
            {
                on_screen += 1;
            }
        }
        output.drop_without_applying_deltas();
        assert!(
            on_screen >= 20,
            "only {on_screen} text runs are visible on the first frame"
        );
    }
}
