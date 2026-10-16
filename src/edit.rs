//! Live-render document view.
//!
//! The document is drawn as a vertical stack of blocks on a page. Every block is
//! painted in its **final** form — headings are large, bold text is bold,
//! formulas are typeset, diagrams are drawn, callouts are tinted panels — and
//! the block the caret is in becomes an editable `TextEdit` *in place*.
//!
//! Markdown syntax is never painted in this mode. Delimiters such as `**`,
//! `[]()` and `$` stay in the buffer but render at transparent colour, and block
//! markers like `## ` or `- ` are replaced by real bullets, quote bars and
//! heading styles. That is what makes it feel like Word rather than like a
//! markup preview: there is nothing to read but the document itself.
//!
//! That is the whole trick behind "edit and see it at the same time": there is no
//! preview pane, because the editable widget and the rendered view are the same
//! widget. The source text is never rewritten by rendering — the editable buffer
//! for a block is a slice of the document, and a commit replaces exactly that
//! slice, so untouched Markdown survives byte-for-byte.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use egui::widgets::text_edit::{TextBuffer, TextEditState};
use egui::{Color32, FontFamily, FontId, RichText, Sense, Ui};

use crate::diagram::{self, Engine};
use crate::doc::inline::{Inline, Style as InlineStyle};
use crate::doc::model::{
    Align, Block, BlockKind, Callout, CalloutSyntax, Document, ListKind, MarkdocTagKind,
};
use crate::gfx::{Font, Measure, Rgba};
use crate::gfx_egui::{self, EguiMeasure};
use crate::tex;
use crate::theme::Theme;

/// Horizontal padding inside a table cell, in points.
///
/// Shared by the layout and the tests, because a cell that is padded on one side
/// and flush on the other is a defect that no type error catches.
const TABLE_PAD_X: f32 = 10.0;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Which block is being edited, plus its working buffer.
#[derive(Clone, Debug)]
pub struct EditSession {
    /// Index path into the block tree (nesting is real: callouts, JSX, ...).
    pub path: Vec<usize>,
    /// The block's source slice, which is what the user edits.
    pub buffer: String,
    /// Document revision the session started at; a mismatch ends the session.
    pub revision: u64,
}

/// A cached diagram render.
struct DiagramEntry {
    prims: Arc<Vec<crate::gfx::Prim>>,
    size: [f32; 2],
    warnings: Vec<String>,
    summary: String,
    error: Option<String>,
}

/// A cached formula render.
struct MathEntry {
    prims: Arc<Vec<crate::gfx::Prim>>,
    size: [f32; 2],
    baseline: f32,
    warnings: Vec<String>,
}

/// Persistent view state: edit session, render caches, transient notices.
#[derive(Default)]
pub struct ViewState {
    pub active: Option<EditSession>,
    diagram_cache: HashMap<u64, DiagramEntry>,
    math_cache: HashMap<u64, MathEntry>,
    /// Shown under the toolbar for a few seconds.
    pub notice: Option<(String, f64)>,
    /// Set when a block was activated programmatically and should take focus.
    pub focus_requested: bool,
}

impl ViewState {
    /// Ends the current edit session.
    pub fn end_session(&mut self) {
        self.active = None;
    }

    /// Drops caches after a theme or zoom change.
    pub fn invalidate(&mut self) {
        self.diagram_cache.clear();
        self.math_cache.clear();
    }

    pub fn show_notice(&mut self, text: impl Into<String>, now: f64) {
        self.notice = Some((text.into(), now));
    }

    /// Begins editing `path`, seeding the buffer from the document.
    pub fn begin_session(&mut self, path: Vec<usize>, doc: &Document, block: &Block) {
        self.active = Some(EditSession {
            path,
            buffer: doc.text.get(block.span.clone()).unwrap_or("").to_owned(),
            revision: doc.revision,
        });
        self.focus_requested = true;
    }
}

/// Something the renderer wants the application to do.
#[derive(Clone, Debug)]
pub enum EditorAction {
    /// Replace a document range with edited text.
    Commit { span: Range<usize>, text: String },
    /// Open a link in the browser.
    OpenLink(String),
    /// Rewrite a callout in a different syntax.
    ConvertCallout {
        /// The callout's byte range, used to re-find it after a reparse.
        span: Range<usize>,
        syntax: CalloutSyntax,
    },
    /// Flip a task list item.
    ToggleTask { span: Range<usize>, checked: bool },
    /// Delete a block.
    DeleteBlock { span: Range<usize> },
    /// Move the caret into a block at a byte offset.
    Focus { path: Vec<usize>, caret: usize },
}

/// Layout options the app passes down.
#[derive(Clone, Copy, Debug)]
pub struct RenderOpts {
    /// Width of the reading column, in points.
    pub measure: f32,
    /// Extra scale applied to diagram text, on top of the UI zoom.
    pub diagram_scale: f32,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            measure: 760.0,
            diagram_scale: 1.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Renders the whole document. Returns at most one action per frame.
pub fn document_ui(
    ui: &mut Ui,
    state: &mut ViewState,
    doc: &Document,
    theme: &Theme,
    opts: &RenderOpts,
) -> Option<EditorAction> {
    // A stale session (the document changed underneath us) must not survive.
    if let Some(session) = &state.active
        && session.revision != doc.revision
    {
        state.active = None;
    }

    let mut action: Option<EditorAction> = None;
    let mut ctx = BlockCtx {
        state,
        doc,
        theme,
        opts,
        level: 0,
    };
    for (i, block) in doc.blocks.iter().enumerate() {
        let path = vec![i];
        if let Some(a) = ctx.block_ui(ui, block, &path) {
            merge_action(&mut action, a);
        }
    }
    action
}

fn merge_action(current: &mut Option<EditorAction>, next: EditorAction) {
    if matches!(next, EditorAction::Commit { .. })
        && current.as_ref().is_some_and(|action| !matches!(action, EditorAction::Commit { .. }))
    {
        return;
    }
    *current = Some(next);
}

/// Per-frame rendering context threaded through the recursive renderer.
struct BlockCtx<'a> {
    state: &'a mut ViewState,
    doc: &'a Document,
    theme: &'a Theme,
    opts: &'a RenderOpts,
    level: usize,
}

impl BlockCtx<'_> {
    /// Renders one block. Returns an action if the user did something.
    fn block_ui(&mut self, ui: &mut Ui, block: &Block, path: &[usize]) -> Option<EditorAction> {
        // Vertical rhythm.
        //
        // Word's paragraph spacing is generous, and a Markdown document needs
        // more than most because blocks are visually distinct: a 6pt gap made
        // callouts, code cards and the JSX/Markdoc blocks look welded together.
        // The space is added *before* each block so the first paragraph does not
        // inherit a gap from the top of the page.
        let before = match &block.kind {
            // Headings need to stand clear of the text they introduce.
            BlockKind::Heading { level, .. } if *level <= 2 => 26.0,
            BlockKind::Heading { .. } => 20.0,
            BlockKind::Rule => 20.0,
            // Nothing is shown above front matter, so no leading gap.
            BlockKind::FrontMatter { .. } => 0.0,
            // Boxes get more air than plain paragraphs, or their borders touch.
            BlockKind::Callout(_)
            | BlockKind::Code { .. }
            | BlockKind::Math { .. }
            | BlockKind::MdxEsm { .. }
            | BlockKind::MdxJsx { .. }
            | BlockKind::MarkdocTag { .. }
            | BlockKind::Html { .. } => 14.0,
            _ => 10.0,
        };
        // Inside a container (callout, JSX, Markdoc) the padding is the
        // container's job, so nested blocks must not each add their own leading
        // gap — otherwise the first child sits flush against the container's
        // header while later ones drift away from it.
        let nested = self.level > 0;
        if !nested {
            ui.add_space(before);
        }

        let active = self
            .state
            .active
            .as_ref()
            .is_some_and(|s| s.path == path);

        if active
            && matches!(&block.kind, BlockKind::Table { .. } | BlockKind::MarkdocTag { .. })
        {
            return self.code_edit(ui, block, path);
        }

        match &block.kind {
            BlockKind::Heading { level, .. } => self.heading_ui(ui, block, path, *level, active),
            BlockKind::Paragraph { .. } => {
                self.inline_ui(ui, block, path, active, InlineVisual::body(self.theme))
            }
            BlockKind::ListItem {
                kind,
                index,
                level,
                checked,
                ..
            } => self.list_item_ui(ui, block, path, *kind, *index, *level, *checked, active),
            BlockKind::Quote { .. } => self.quote_ui(ui, block, path, active),
            BlockKind::Callout(c) => self.callout_ui(ui, block, path, c, active),
            BlockKind::Code { .. } => self.code_ui(ui, block, path, active),
            BlockKind::Math { .. } => self.math_ui(ui, block, path, active),
            BlockKind::Table { align, header, rows } => {
                self.table_ui(ui, align, header, rows, block, path)
            }
            BlockKind::Rule => {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 1.0),
                    Sense::hover(),
                );
                ui.painter().rect_filled(
                    rect,
                    0.0,
                    self.theme.palette.rule.to_egui(),
                );
                None
            }
            BlockKind::Html { raw } => self.card_ui(ui, "HTML", raw, self.theme.palette.text_muted),
            BlockKind::FrontMatter { delimiter, raw } => {
                self.card_ui(ui, &format!("{} 元数据", delimiter), raw, self.theme.palette.text_faint)
            }
            BlockKind::MdxEsm { raw } => self.card_ui(ui, "ESM", raw, Rgba::hex(0xC792EA)),
            BlockKind::MdxJsx {
                name,
                props,
                self_closing,
                children,
                ..
            } => self.jsx_ui(ui, path, name, props, *self_closing, children),
            BlockKind::MarkdocTag {
                kind,
                name,
                attrs,
                body,
                ..
            } => self.markdoc_ui(ui, path, *kind, name, attrs, body, block),
        }
    }

    // -- headings ----------------------------------------------------------

    fn heading_ui(
        &mut self,
        ui: &mut Ui,
        block: &Block,
        path: &[usize],
        level: u8,
        active: bool,
    ) -> Option<EditorAction> {
        let size = self.theme.typo.heading(level);
        let visual = InlineVisual {
            size,
            bold: true,
            level,
            italic: false,
        };
        self.inline_ui(ui, block, path, active, visual)
    }

    // -- inline content ----------------------------------------------------

    /// Renders a block whose payload is a single run of inline Markdown.
    fn inline_ui(
        &mut self,
        ui: &mut Ui,
        block: &Block,
        path: &[usize],
        active: bool,
        visual: InlineVisual,
    ) -> Option<EditorAction> {
        let marker_len = block
            .inline_span
            .start
            .saturating_sub(block.span.start)
            .min(block.span.len());
        let inner_len = block.inline_span.len();
        let span = block.span.clone();

        // Indent nested content (list levels, callout bodies).
        let indent = self.indent_for(path);
        if indent > 0.0 {
            ui.horizontal(|ui| {
                ui.add_space(indent);
            });
        }

        if active {
            self.inline_edit(ui, block, path, visual)
        } else {
            let source = self.doc.text.get(span.clone()).unwrap_or("");
            let mut math = Vec::new();
            let mut job = inline_job(
                source,
                marker_len,
                inner_len,
                InlineMode::Wysiwyg,
                self.theme,
                visual,
                Some(&mut math),
            );
            job.wrap.max_width = ui.available_width().min(self.opts.measure).max(1.0);
            let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
            let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
            // `painter.galley` consumes the `Arc`, but the math overlay needs it
            // to resolve cursor positions, so keep a handle.
            let placed = Arc::clone(&galley);
            ui.painter().galley(
                rect.min,
                galley,
                self.theme.palette.text.to_egui(),
            );
            // Typeset inline formulas over the text now that the galley knows
            // where each placeholder landed.
            paint_inline_math(ui.ctx(), ui.painter(), &placed, &math);
            if resp.clicked() {
                return Some(EditorAction::Focus {
                    path: path.to_vec(),
                    caret: 0,
                });
            }
            if resp.hovered() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Text);
            }
            None
        }
    }

    /// The editable form of an inline block.
    fn inline_edit(
        &mut self,
        ui: &mut Ui,
        block: &Block,
        path: &[usize],
        visual: InlineVisual,
    ) -> Option<EditorAction> {
        let id = block_id(ui, path);
        let theme = *self.theme;
        let original_marker = self
            .doc
            .text
            .get(block.span.start..block.inline_span.start)
            .unwrap_or("")
            .to_owned();
        let original_trailing = self
            .doc
            .text
            .get(block.inline_span.end..block.span.end)
            .unwrap_or("")
            .to_owned();
        let render = move |ui: &Ui, buf: &dyn TextBuffer, wrap: f32| {
            let text = buf.as_str();
            let _ = caret_chars(ui, id);
            let marker_len = if !original_marker.is_empty() && text.starts_with(&original_marker) {
                original_marker.len()
            } else {
                0
            };
            let trailing_len = if !original_trailing.is_empty() && text.ends_with(&original_trailing)
            {
                original_trailing.len()
            } else {
                0
            };
            let inner_len = text.len().saturating_sub(marker_len + trailing_len);
            let mut job = inline_job(
                text,
                marker_len.min(text.len()),
                inner_len,
                InlineMode::Wysiwyg,
                &theme,
                visual,
                None,
            );
            job.wrap.max_width = wrap.max(1.0);
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };

        let mut layouter = render;
        let session = self.state.active.as_mut()?;
        let buffer = &mut session.buffer;

        let resp = ui.add(
            egui::TextEdit::multiline(buffer)
                .id(id)
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY)
                .desired_rows(1)
                .layouter(&mut layouter),
        );

        if self.state.focus_requested {
            resp.request_focus();
            self.state.focus_requested = false;
        }

        let commit = resp.lost_focus()
            || ui.input(|i| i.key_pressed(egui::Key::Escape));

        if commit {
            let text = self.state.active.as_ref()?.buffer.clone();
            let span = block.span.clone();
            return Some(EditorAction::Commit { span, text });
        }
        None
    }

    // -- list items --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn list_item_ui(
        &mut self,
        ui: &mut Ui,
        block: &Block,
        path: &[usize],
        kind: ListKind,
        index: u64,
        level: u8,
        checked: Option<bool>,
        active: bool,
    ) -> Option<EditorAction> {
        // Word-like hanging indent: the bullet sits in the gutter and the text
        // block starts at a fixed column, so wrapped lines align under the
        // first character rather than under the marker.
        const GUTTER: f32 = 26.0;
        let indent = f32::from(level) * GUTTER;
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_space(indent);
            match checked {
                Some(done) => {
                    let mut value = done;
                    let resp = ui.checkbox(&mut value, "");
                    if resp.changed() {
                        action = Some(EditorAction::ToggleTask {
                            span: block.span.clone(),
                            checked: value,
                        });
                    }
                }
                None => {
                    let (label, color) = match kind {
                        ListKind::Unordered => {
                            // Nested levels get progressively different glyphs,
                            // as Word does, so depth reads without indent alone.
                            let glyph = ["•", "◦", "▪"][(level as usize).min(2)];
                            (glyph.to_owned(), self.theme.palette.text)
                        }
                        ListKind::Ordered => {
                            (format!("{index}."), self.theme.palette.text)
                        }
                    };
                    ui.allocate_ui(
                        egui::vec2(GUTTER - 4.0, self.theme.body_size() * 1.4),
                        |ui| {
                            ui.label(
                                RichText::new(label)
                                    .color(color.to_egui())
                                    .size(self.theme.body_size()),
                            );
                        },
                    );
                }
            }
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - GUTTER).max(40.0));
                if let Some(a) = self.inline_ui(
                    ui,
                    block,
                    path,
                    active,
                    InlineVisual::body(self.theme),
                ) {
                    merge_action(&mut action, a);
                }
            });
        });
        action
    }

    // -- quote -------------------------------------------------------------

    fn quote_ui(
        &mut self,
        ui: &mut Ui,
        block: &Block,
        path: &[usize],
        active: bool,
    ) -> Option<EditorAction> {
        // Word renders a quote as a coloured bar plus italic text. The `> `
        // characters stay in the source but are hidden by `inline_ui`.
        let bar_w = 3.0;
        let pad = 14.0;
        let index = ui.painter().add(egui::Shape::Noop);
        let mut visual = InlineVisual::body(self.theme);
        visual.italic = true;
        let inner = ui.horizontal(|ui| {
            ui.add_space(bar_w + pad);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - pad);
                self.inline_ui(ui, block, path, active, visual)
            })
            .inner
        });
        let rect = inner.response.rect;
        let bar = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(bar_w, rect.height()),
        );
        ui.painter().set(
            index,
            egui::Shape::rect_filled(bar, 2.0, self.theme.palette.quote_bar.to_egui()),
        );
        inner.inner
    }

    // -- callouts ----------------------------------------------------------

    fn callout_ui(
        &mut self,
        ui: &mut Ui,
        block: &Block,
        path: &[usize],
        callout: &Callout,
        active: bool,
    ) -> Option<EditorAction> {
        let look = self.theme.callout_look(callout.syntax, &callout.variant);
        let pad = 12.0;
        let bar_w = look.bar.unwrap_or(0.0);
        let left = if bar_w > 0.0 { bar_w + pad } else { pad };

        let bg_index = ui.painter().add(egui::Shape::Noop);
        let bar_index = ui.painter().add(egui::Shape::Noop);

        let mut action: Option<EditorAction> = None;
        let inner = ui.horizontal(|ui| {
            ui.add_space(left);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - pad);

                // Title row: icon + label, plus a syntax switcher on the right.
                let title = callout.display_title();
                if title.is_some() || look.icon.is_some() {
                    ui.horizontal(|ui| {
                        if let Some(icon) = look.icon {
                            ui.label(
                                RichText::new(icon)
                                    .color(look.accent.to_egui())
                                    .size(self.theme.body_size()),
                            );
                            ui.add_space(4.0);
                        }
                        if let Some(title) = &title {
                            let text = if look.uppercase_title {
                                title.to_uppercase()
                            } else {
                                title.clone()
                            };
                            ui.label(
                                RichText::new(text)
                                    .color(look.title.to_egui())
                                    .size(self.theme.body_size())
                                    .strong(),
                            );
                        }
                        // Trailing controls.
                        let syntax = callout.syntax;
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.menu_button(
                                    RichText::new(format!("{} ⌄", syntax.label()))
                                        .size(self.theme.typo.small())
                                        .color(self.theme.palette.text_faint.to_egui()),
                                    |ui| {
                                        for candidate in CalloutSyntax::all() {
                                            if ui
                                                .selectable_label(
                                                    candidate == syntax,
                                                    candidate.label(),
                                                )
                                                .clicked()
                                            {
                                                action = Some(EditorAction::ConvertCallout {
                                                    span: block.span.clone(),
                                                    syntax: candidate,
                                                });
                                                ui.close();
                                            }
                                        }
                                        ui.separator();
                                        ui.label(
                                            RichText::new(candidate_hint(syntax))
                                                .size(self.theme.typo.small())
                                                .color(self.theme.palette.text_faint.to_egui()),
                                        );
                                    },
                                );
                                if active {
                                    ui.label(
                                        RichText::new("编辑中")
                                            .size(self.theme.typo.small())
                                            .color(self.theme.palette.text_faint.to_egui()),
                                    );
                                }
                            },
                        );
                    });
                }

                // Body: real blocks, each independently editable.
                if callout.body.is_empty() {
                    let resp = ui.label(
                        RichText::new("（空提醒框）")
                            .italics()
                            .size(self.theme.body_size())
                            .color(self.theme.palette.text_faint.to_egui()),
                    );
                    if resp.clicked() {
                        action = Some(EditorAction::Focus {
                            path: path.to_vec(),
                            caret: 0,
                        });
                    }
                } else {
                    for (i, child) in callout.body.iter().enumerate() {
                        let mut child_path = path.to_vec();
                        child_path.push(i);
                        if let Some(a) = self.block_ui(ui, child, &child_path) {
                            merge_action(&mut action, a);
                        }
                    }
                }
            });
        });

        let rect = inner.response.rect;
        ui.painter().set(
            bg_index,
            egui::Shape::rect_filled(rect, look.radius, look.tint.to_egui()),
        );
        if bar_w > 0.0 {
            let bar = egui::Rect::from_min_size(rect.min, egui::vec2(bar_w, rect.height()));
            ui.painter().set(
                bar_index,
                egui::Shape::rect_filled(bar, 0.0, look.accent.to_egui()),
            );
        }
        if let Some(border) = look.border {
            ui.painter().rect_stroke(
                rect,
                look.radius,
                egui::Stroke::new(border.width, border.color.to_egui()),
                egui::StrokeKind::Inside,
            );
        }
        // A hairline above/below keeps GitHub's flat style readable.
        if look.bar.is_none() && look.border.is_none() {
            let hair = egui::Stroke::new(1.0, look.accent.with_alpha(0x55).to_egui());
            let top = rect.min.y;
            let bottom = rect.max.y;
            ui.painter().line_segment(
                [
                    egui::pos2(rect.min.x, top),
                    egui::pos2(rect.max.x, top),
                ],
                hair,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(rect.min.x, bottom),
                    egui::pos2(rect.max.x, bottom),
                ],
                hair,
            );
        }

        action
    }

    // -- code and diagrams -------------------------------------------------

    fn code_ui(
        &mut self,
        ui: &mut Ui,
        block: &Block,
        path: &[usize],
        active: bool,
    ) -> Option<EditorAction> {
        let BlockKind::Code { lang, code, .. } = &block.kind else {
            return None;
        };
        let engine = Engine::from_fence_info(lang);

        let pad = 12.0;
        let bg_index = ui.painter().add(egui::Shape::Noop);

        let mut action = None;
        let inner = ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - pad);

                // Header: language chip, diagram summary, edit toggle.
                ui.horizontal(|ui| {
                    let label = if lang.is_empty() {
                        "text".to_owned()
                    } else {
                        lang.clone()
                    };
                    ui.label(
                        RichText::new(label.to_uppercase())
                            .size(self.theme.typo.small() * 0.92)
                            .color(self.theme.palette.text_faint.to_egui()),
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let hint = if active { "预览" } else { "编辑源码" };
                            if ui
                                .small_button(hint)
                                .on_hover_text("切换代码块的源码 / 渲染视图")
                                .clicked()
                            {
                                action = Some(EditorAction::Focus {
                                    path: path.to_vec(),
                                    caret: 0,
                                });
                            }
                        },
                    );
                });
                ui.add_space(4.0);

                if active {
                    if let Some(a) = self.code_edit(ui, block, path) {
                        merge_action(&mut action, a);
                    }
                } else if let Some(engine) = engine {
                    self.diagram_view(ui, engine, code, block);
                } else {
                    let job = code_job(code, self.theme, lang);
                    let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                    ui.allocate_exact_size(galley.size(), Sense::hover());
                    // `allocate_exact_size` returned the rect but we need it for
                    // painting, so grab it again from the Ui's last rect.
                    let rect = ui.min_rect();
                    let top = egui::pos2(rect.min.x, rect.max.y - galley.size().y);
                    ui.painter().galley(
                        top,
                        galley,
                        self.theme.palette.code_text.to_egui(),
                    );
                }
            });
        });

        let rect = inner.response.rect;
        ui.painter().set(
            bg_index,
            egui::Shape::rect_filled(rect.expand2(egui::vec2(pad, 8.0)), 8.0, self.theme.palette.code_bg.to_egui()),
        );
        action
    }

    /// The editable form of a fenced code block, including its fences.
    fn code_edit(&mut self, ui: &mut Ui, block: &Block, path: &[usize]) -> Option<EditorAction> {
        let id = block_id(ui, path);
        let theme = *self.theme;
        let mut layouter = move |ui: &Ui, buf: &dyn TextBuffer, wrap: f32| {
            let job = code_job(buf.as_str(), &theme, "", );
            let _ = wrap;
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };

        let session = self.state.active.as_mut()?;
        let buffer = &mut session.buffer;
        let resp = ui.add(
            egui::TextEdit::multiline(buffer)
                .id(id)
                .frame(egui::Frame::NONE)
                .desired_width(f32::INFINITY)
                .layouter(&mut layouter),
        );
        if self.state.focus_requested {
            resp.request_focus();
            self.state.focus_requested = false;
        }
        if resp.lost_focus() {
            let text = self.state.active.as_ref()?.buffer.clone();
            return Some(EditorAction::Commit {
                span: block.span.clone(),
                text,
            });
        }
        None
    }

    /// Renders a Mermaid / PlantUML fence, using the cache.
    fn diagram_view(&mut self, ui: &mut Ui, engine: Engine, code: &str, block: &Block) {
        let zoom = self.theme.typo.scale;
        let scale = zoom * self.opts.diagram_scale;
        // The cache key has to carry every input that changes the drawing, or a
        // settings change would keep showing the pre-change render forever.
        let key = cache_key(
            engine,
            code,
            self.theme.dark,
            self.theme.typo.body(),
            self.opts.diagram_scale,
        );
        if !self.state.diagram_cache.contains_key(&key) {
            let measure = EguiMeasure::new(ui.ctx());
            let mut diagram_theme = self.theme.diagram_theme();
            diagram_theme.font_scale = scale;
            let render = diagram::render(
                code,
                engine,
                &diagram_theme,
                self.theme.typo.body(),
                &measure,
            );
            self.state.diagram_cache.insert(
                key,
                DiagramEntry {
                    prims: Arc::new(render.prims),
                    size: render.size,
                    warnings: render.warnings,
                    summary: render.summary,
                    error: render.error,
                },
            );
            // Bound the cache: diagram renders are cheap to redo.
            if self.state.diagram_cache.len() > 240 {
                self.state.diagram_cache.clear();
            }
        }
        let entry = match self.state.diagram_cache.get(&key) {
            Some(e) => e,
            None => return,
        };

        let width = entry.size[0].max(1.0);
        let height = entry.size[1].max(1.0);
        let avail = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(avail, height + 16.0), Sense::click());

        // Centre the diagram horizontally inside the available width.
        let x = rect.min.x + ((avail - width) * 0.5).max(0.0);
        let origin = egui::pos2(x, rect.min.y + 8.0);

        // A card behind the diagram, so transparent SVG-less engines still read
        // as a figure rather than floating strokes.
        ui.painter().rect_filled(
            rect,
            8.0,
            self.theme.palette.surface_alt.fade(0.45).to_egui(),
        );
        gfx_egui::paint_clipped(ui.ctx(), ui.painter(), origin, rect, &entry.prims);

        if resp.clicked() {
            let _ = block;
        }
        if resp.hovered() {
            resp.clone().on_hover_text(format!(
                "{} · 点击上方「编辑源码」修改",
                entry.summary
            ));
        }
        if let Some(err) = &entry.error {
            ui.label(
                RichText::new(format!("图表渲染失败：{err}"))
                    .size(self.theme.typo.small())
                    .color(Rgba::hex(0xF87171).to_egui()),
            );
        }
        if !entry.warnings.is_empty() {
            ui.label(
                RichText::new(format!("{} 项语法提示：{}", entry.warnings.len(), entry.warnings.join("；")))
                    .size(self.theme.typo.small())
                    .color(self.theme.palette.text_faint.to_egui()),
            );
        }
    }

    // -- math --------------------------------------------------------------

    fn math_ui(
        &mut self,
        ui: &mut Ui,
        block: &Block,
        path: &[usize],
        active: bool,
    ) -> Option<EditorAction> {
        let BlockKind::Math { tex: source } = &block.kind else {
            return None;
        };

        if active {
            return self.code_edit(ui, block, path);
        }

        let base = self.theme.typo.body() * 1.25;
        let color = self.theme.palette.text;
        let key = cache_key_math(source, base, color);
        if !self.state.math_cache.contains_key(&key) {
            let measure = EguiMeasure::new(ui.ctx());
            let render = tex::render(source, base, color, &measure);
            self.state.math_cache.insert(
                key,
                MathEntry {
                    prims: Arc::new(render.prims),
                    size: render.size,
                    baseline: render.baseline,
                    warnings: render.warnings,
                },
            );
            if self.state.math_cache.len() > 400 {
                self.state.math_cache.clear();
            }
        }
        let entry = match self.state.math_cache.get(&key) {
            Some(e) => e,
            None => return None,
        };

        let avail = ui.available_width();
        let height = entry.size[1].max(base) + 16.0;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(avail, height), Sense::click());
        let w = entry.size[0];
        let origin = egui::pos2(rect.min.x + ((avail - w) * 0.5).max(0.0), rect.min.y + 8.0);
        gfx_egui::paint_clipped(ui.ctx(), ui.painter(), origin, rect, &entry.prims);
        if resp.clicked() {
            return Some(EditorAction::Focus {
                path: path.to_vec(),
                caret: 0,
            });
        }
        let _ = entry.baseline;
        if !entry.warnings.is_empty() {
            ui.label(
                RichText::new(entry.warnings.join("；"))
                    .size(self.theme.typo.small())
                    .color(self.theme.palette.text_faint.to_egui()),
            );
        }
        None
    }

    // -- tables ------------------------------------------------------------

    fn table_ui(
        &mut self,
        ui: &mut Ui,
        align: &[Align],
        header: &[String],
        rows: &[Vec<String>],
        block: &Block,
        path: &[usize],
    ) -> Option<EditorAction> {
        let mut action = None;
        let active = self
            .state
            .active
            .as_ref()
            .is_some_and(|s| s.path == path);

        // egui's public Grid is gone in 0.37, and a hand-rolled table gives us
        // the per-cell alignment the GFM syntax asks for anyway.
        let cols = header
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        if cols == 0 {
            return None;
        }
        let measure = EguiMeasure::new(ui.ctx());
        let body_font = Font::new(self.theme.body_size());
        let head_font = Font::new(self.theme.body_size()).bold();
        let pad_x = 10.0f32;

        let cell_text = |cell: &str| Inline::scan(cell).plain_text(cell);
        let mut widths = vec![0.0f32; cols];
        for (c, cell) in header.iter().enumerate().take(cols) {
            widths[c] = widths[c].max(measure.measure(&cell_text(cell), head_font)[0]);
        }
        for row in rows {
            for (c, cell) in row.iter().enumerate().take(cols) {
                widths[c] = widths[c].max(measure.measure(&cell_text(cell), body_font)[0]);
            }
        }
        for w in &mut widths {
            *w = (*w + pad_x * 2.0).clamp(48.0, 320.0);
        }
        let total: f32 = widths.iter().sum();
        let line_h = self.theme.line_height(self.theme.body_size());

        let paint_row = |ui: &mut Ui, cells: &[String], is_header: bool| {
            let row_text: Vec<&str> = (0..cols)
                .map(|c| cells.get(c).map(String::as_str).unwrap_or(""))
                .collect();
            let (rect, _) = ui.allocate_exact_size(egui::vec2(total, line_h), Sense::hover());
            let painter = ui.painter();
            if is_header {
                painter.rect_filled(rect, 0.0, self.theme.palette.table_head.to_egui());
            }
            painter.rect_stroke(
                rect,
                0.0,
                egui::Stroke::new(1.0, self.theme.palette.table_border.to_egui()),
                egui::StrokeKind::Inside,
            );
            let mut x = rect.min.x;
            for (c, cell) in row_text.iter().enumerate() {
                if c > 0 {
                    painter.line_segment(
                        [
                            egui::pos2(x, rect.min.y),
                            egui::pos2(x, rect.max.y),
                        ],
                        egui::Stroke::new(1.0, self.theme.palette.table_border.to_egui()),
                    );
                }
                let font = if is_header {
                    head_font
                } else {
                    body_font
                };
                let job = inline_job(
                    cell,
                    0,
                    cell.len(),
                    InlineMode::Wysiwyg,
                    self.theme,
                    InlineVisual {
                        size: font.size,
                        bold: is_header,
                        level: 0,
                        italic: false,
                    },
                    None,
                );
                let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                let align = align.get(c).copied().unwrap_or(Align::None);
                // The column spans `widths[c]`; the padding is taken off once, on
                // the side the text is pushed away from. Subtracting it twice left
                // right- and centre-aligned cells floating one padding too far from
                // the edge their neighbours were aligned to.
                let inner = (widths[c] - pad_x * 2.0).max(0.0);
                let tx = match align {
                    Align::Center => x + pad_x + (inner - galley.size().x) * 0.5,
                    Align::Right => x + pad_x + (inner - galley.size().x),
                    _ => x + pad_x,
                };
                let ty = rect.min.y + (line_h - galley.size().y) * 0.5;
                // Header and body share one ink colour; the header reads as a
                // header through its bold face and tinted cell background.
                painter.galley(
                    egui::pos2(tx, ty),
                    galley,
                    self.theme.palette.text.to_egui(),
                );
                x += widths[c];
            }
        };

        egui::ScrollArea::horizontal()
            .id_salt(("mdp-table", path.to_vec()))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    paint_row(ui, header, true);
                    for row in rows {
                        paint_row(ui, row, false);
                    }
                });
            });

        if ui
            .small_button(RichText::new("编辑表格源码").size(self.theme.typo.small()))
            .clicked()
            && !active
        {
            action = Some(EditorAction::Focus {
                path: path.to_vec(),
                caret: 0,
            });
        }
        let _ = block;
        action
    }

    // -- MDX / Markdoc -----------------------------------------------------

    fn jsx_ui(
        &mut self,
        ui: &mut Ui,
        path: &[usize],
        name: &str,
        props: &str,
        self_closing: bool,
        children: &[Block],
    ) -> Option<EditorAction> {
        let accent = Rgba::hex(0xC792EA);
        const PAD_X: f32 = 12.0;
        const PAD_Y: f32 = 10.0;
        let index = ui.painter().add(egui::Shape::Noop);
        let mut action = None;
        let inner = ui.horizontal(|ui| {
            ui.add_space(PAD_X);
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - PAD_X).max(40.0));
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("<{name}>"))
                            .monospace()
                            .size(self.theme.typo.small())
                            .color(accent.to_egui()),
                    );
                    if !props.trim().is_empty() {
                        ui.label(
                            RichText::new(props)
                                .monospace()
                                .size(self.theme.typo.small())
                                .color(self.theme.palette.text_faint.to_egui()),
                        );
                    }
                });
                if !self_closing && !children.is_empty() {
                    // A clear gap under the opening tag, so the children read as
                    // the component's body rather than as part of the tag line.
                    ui.add_space(10.0);
                    for (i, child) in children.iter().enumerate() {
                        let mut child_path = path.to_vec();
                        child_path.push(i);
                        if let Some(a) = self.block_ui(ui, child, &child_path) {
                            merge_action(&mut action, a);
                        }
                    }
                }
                // Bottom padding, so the body never touches the card's edge.
                ui.add_space(PAD_Y);
            });
        });
        let rect = inner.response.rect;
        ui.painter().set(
            index,
            egui::Shape::rect_filled(
                rect.expand2(egui::vec2(PAD_X, PAD_Y)),
                8.0,
                accent.with_alpha(0x14).to_egui(),
            ),
        );
        action
    }

    #[allow(clippy::too_many_arguments)]
    fn markdoc_ui(
        &mut self,
        ui: &mut Ui,
        path: &[usize],
        kind: MarkdocTagKind,
        name: &str,
        attrs: &str,
        body: &[Block],
        block: &Block,
    ) -> Option<EditorAction> {
        let accent = match kind {
            MarkdocTagKind::Conditional => Rgba::hex(0xFBBF24),
            MarkdocTagKind::Reference => Rgba::hex(0x67E8F9),
            MarkdocTagKind::Comment => self.theme.palette.text_faint,
            _ => Rgba::hex(0x4ADE80),
        };
        const PAD_X: f32 = 12.0;
        const PAD_Y: f32 = 10.0;
        let index = ui.painter().add(egui::Shape::Noop);
        let mut action = None;
        let inner = ui.horizontal(|ui| {
            ui.add_space(PAD_X);
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - PAD_X).max(40.0));
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(match kind {
                            MarkdocTagKind::Conditional => format!("{{% {name} %}}"),
                            MarkdocTagKind::Reference => format!("{{% #{name} %}}"),
                            MarkdocTagKind::Comment => "{% comment %}".to_owned(),
                            _ => format!("{{% {name} %}}"),
                        })
                        .monospace()
                        .size(self.theme.typo.small())
                        .color(accent.to_egui()),
                    );
                    if !attrs.trim().is_empty() {
                        ui.label(
                            RichText::new(attrs)
                                .monospace()
                                .size(self.theme.typo.small())
                                .color(self.theme.palette.text_faint.to_egui()),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(RichText::new("源码").size(self.theme.typo.small()))
                            .on_hover_text("以 Markdoc 源码编辑此块")
                            .clicked()
                        {
                            action = Some(EditorAction::Focus {
                                path: path.to_vec(),
                                caret: 0,
                            });
                        }
                        ui.label(
                            RichText::new(kind.label())
                                .size(self.theme.typo.small() * 0.92)
                                .color(self.theme.palette.text_faint.to_egui()),
                        );
                    });
                });
                if kind != MarkdocTagKind::Comment && !body.is_empty() {
                    ui.add_space(10.0);
                    for (i, child) in body.iter().enumerate() {
                        let mut child_path = path.to_vec();
                        child_path.push(i);
                        if let Some(a) = self.block_ui(ui, child, &child_path) {
                            merge_action(&mut action, a);
                        }
                    }
                }
                // Bottom padding, matching the top.
                ui.add_space(PAD_Y);
            });
        });
        let rect = inner.response.rect;
        ui.painter().set(
            index,
            egui::Shape::rect_filled(
                rect.expand2(egui::vec2(PAD_X, PAD_Y)),
                6.0,
                accent.with_alpha(0x12).to_egui(),
            ),
        );
        let _ = block;
        action
    }

    /// A simple monospace card used for front matter, ESM and raw HTML.
    fn card_ui(
        &mut self,
        ui: &mut Ui,
        label: &str,
        body: &str,
        accent: Rgba,
    ) -> Option<EditorAction> {
        // The padding must be *inside* the allocated rect. Painting the card
        // with `rect.expand2(..)` while the layout reserved only `rect` made
        // neighbouring cards overlap: each one bled into the next block's space
        // and the two borders ended up touching.
        const PAD_X: f32 = 12.0;
        const PAD_Y: f32 = 10.0;

        let index = ui.painter().add(egui::Shape::Noop);
        let inner = ui.horizontal(|ui| {
            ui.add_space(PAD_X);
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - PAD_X).max(40.0));
                ui.label(
                    RichText::new(label)
                        .size(self.theme.typo.small())
                        .color(accent.to_egui()),
                );
                ui.add_space(4.0);
                let job = code_job(body.trim_end(), self.theme, "");
                let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
                ui.painter()
                    .galley(rect.min, galley, self.theme.palette.text.to_egui());
                // Trailing pad keeps the code clear of the card's bottom edge.
                ui.add_space(PAD_Y);
            });
        });
        ui.painter().set(
            index,
            egui::Shape::rect_filled(
                // Expand only to the left and up by the padding already consumed
                // as layout space; the right and bottom are covered by the
                // explicit padding added above.
                inner.response.rect.expand2(egui::vec2(PAD_X, PAD_Y)),
                6.0,
                self.theme.palette.surface_alt.fade(0.5).to_egui(),
            ),
        );
        None
    }

    /// Extra left indent for a block, derived from its nesting depth.
    fn indent_for(&self, path: &[usize]) -> f32 {
        // Only top-level list items carry explicit indentation; nested bodies are
        // already indented by their container.
        if path.len() > 1 {
            4.0
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------------------
// Inline layout
// ---------------------------------------------------------------------------

/// Base typography for an inline run.
#[derive(Clone, Copy, Debug)]
pub struct InlineVisual {
    pub size: f32,
    pub bold: bool,
    /// Heading level, `0` for body text. Used to shrink delimiters relative to
    /// the text they mark.
    pub level: u8,
    /// Applies italics to plain runs. Block quotes use this so they read the way
    /// Word renders a quotation rather than looking like ordinary body text.
    pub italic: bool,
}

impl InlineVisual {
    fn body(theme: &Theme) -> Self {
        Self {
            size: theme.typo.body(),
            bold: false,
            level: 0,
            italic: false,
        }
    }
}

/// Formatting for source characters that must not be seen.
///
/// The width matters as much as the colour. The caret model maps buffer
/// character indices onto galley columns, so a delimiter must still advance the
/// cursor — collapsing it to exactly zero would make the caret jump around
/// unpredictably inside a construct. Font size 1pt is the smallest that still
/// advances, and at that size the glyph is sub-pixel: invisible in practice,
/// while `**` still costs under two points of width.
///
/// Letter spacing is deliberately left at zero. An earlier version subtracted a
/// multiple of the *body* size here, which dragged whole lines off the left edge
/// of the page and made the text disappear entirely.
fn hidden_format(base: &egui::TextFormat) -> egui::TextFormat {
    const HIDDEN_PT: f32 = 1.0;
    egui::TextFormat {
        font_id: FontId::new(HIDDEN_PT, base.font_id.family.clone()),
        color: Color32::TRANSPARENT,
        ..Default::default()
    }
}

/// What a [`inline_job`] call is allowed to show.
///
/// The distinction matters because the two modes have opposite goals. WYSIWYG
/// must look like the finished document — no asterisks, no brackets, no hashes,
/// *ever*, including with the caret parked inside a construct. Source mode must
/// show the exact bytes that will be saved, because that is its whole purpose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InlineMode {
    /// Finished-document appearance. Syntax characters are invisible.
    Wysiwyg,
    /// Raw text with syntax highlighting. Everything is visible.
    Source,
}

/// Builds the `LayoutJob` for a run of inline Markdown.
///
/// `marker_len` bytes at the start of `text` are the block's own marker (`## `,
/// `- `, `> `). In WYSIWYG mode they are always hidden: the block renderer has
/// already drawn a real bullet, quote bar or heading style in their place, so
/// painting the raw characters too would double them up. The following
/// `inner_len` bytes are the inline content and are scanned for constructs.
/// A run of inline math that needs to be painted over the text layout.
///
/// `LayoutJob` only accepts text, but real math needs rules for fractions,
/// radicals and overlines. The span is therefore filled with a single
/// placeholder character, and the typeset result is painted on top once the
/// galley has told us where that character ended up.
#[derive(Clone, Debug)]
pub struct InlineMathSpan {
    /// Character offset of the placeholder within the job's text.
    pub char_index: usize,
    /// The LaTeX source to typeset.
    pub source: String,
    /// Font size the formula should be laid out at.
    pub size: f32,
    /// Colour for the formula.
    pub color: Rgba,
}

/// Builds the `LayoutJob` for a run of inline Markdown.
///
/// `marker_len` bytes at the start of `text` are the block's own marker (`## `,
/// `- `, `> `). In WYSIWYG mode they are always hidden: the block renderer has
/// already drawn a real bullet, quote bar or heading style in their place, so
/// painting the raw characters too would double them up. The following
/// `inner_len` bytes are the inline content and are scanned for constructs.
///
/// When `math_spans` is given, WYSIWYG-mode `$…$` runs are replaced by a
/// placeholder and appended there so the caller can typeset over them.
fn inline_job(
    text: &str,
    marker_len: usize,
    inner_len: usize,
    mode: InlineMode,
    theme: &Theme,
    visual: InlineVisual,
    mut math_spans: Option<&mut Vec<InlineMathSpan>>,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let text_len = text.len();
    let marker_len = crate::doc::model::clamp_boundary(text, marker_len);
    let inner_end = crate::doc::model::clamp_boundary(text, marker_len.saturating_add(inner_len));

    let marker_fmt = egui::TextFormat {
        font_id: FontId::new(visual.size, family_of(visual.bold, false)),
        color: theme.palette.text_faint.to_egui(),
        ..Default::default()
    };
    if marker_len > 0 {
        let fmt = match mode {
            InlineMode::Wysiwyg => hidden_format(&marker_fmt),
            InlineMode::Source => marker_fmt.clone(),
        };
        job.append(&text[..marker_len], 0.0, fmt);
    }

    let inner = &text[marker_len..inner_end];
    if !inner.is_empty() {
        let scan = Inline::scan_at(inner, marker_len);
        for span in &scan.spans {
            let is_delim = span
                .construct
                .and_then(|id| scan.constructs.get(id))
                .is_some_and(|c| c.is_delimiter(&span.range));

            // In WYSIWYG, a math run is typeset over the text flow rather than
            // printed as source. `tex::render` output is drawing primitives, not
            // glyphs, so the job gets a zero-ink placeholder that marks where
            // the formula belongs; the caller paints over it afterwards.
            if mode == InlineMode::Wysiwyg && span.style.math && !is_delim {
                if let Some(out) = math_spans
                    .as_deref_mut()
                {
                    let source = span.text(text).trim().to_owned();
                    let char_index = job.text.chars().count();
                    let size = visual.size;
                    let color = theme.palette.text;
                    out.push(InlineMathSpan {
                        char_index,
                        source,
                        size,
                        color,
                    });
                    job.append(
                        MATH_PLACEHOLDER,
                        0.0,
                        egui::TextFormat {
                            font_id: FontId::new(size * 0.6, family_of(false, false)),
                            color: Color32::TRANSPARENT,
                            ..Default::default()
                        },
                    );
                    continue;
                }
            }

            let fmt = match mode {
                // WYSIWYG: delimiters vanish, whatever the caret is doing.
                InlineMode::Wysiwyg if is_delim => hidden_format(&marker_fmt),
                // Source mode: delimiters stay visible, quieter than content so
                // the structure reads as secondary.
                InlineMode::Source if is_delim => egui::TextFormat {
                    font_id: FontId::new(
                        visual.size * 0.9,
                        family_of(visual.bold, span.style.code),
                    ),
                    color: theme
                        .palette
                        .text_faint
                        .lerp(theme.palette.accent, 0.35)
                        .to_egui(),
                    ..Default::default()
                },
                _ => content_format(span.style, theme, visual),
            };
            // `scan_at` reported offsets in *document* coordinates (they start at
            // `marker_len`), so they must be sliced from the whole block source —
            // slicing from `inner` would silently drop or duplicate characters.
            let piece = span.text(text);
            job.append(piece, 0.0, fmt);
        }
    }

    if inner_end < text_len {
        job.append(
            &text[inner_end..],
            0.0,
            egui::TextFormat {
                font_id: FontId::new(visual.size, family_of(visual.bold, false)),
                color: theme.palette.text.to_egui(),
                ..Default::default()
            },
        );
    }
    job
}

/// The character that stands in for an inline formula in the text job.
///
/// U+FFFC OBJECT REPLACEMENT CHARACTER: it has exactly one char index, so the
/// mapping back to a screen position stays trivial, and no font is expected to
/// have a visible glyph for it.
const MATH_PLACEHOLDER: &str = "\u{FFFC}";

/// Typesets the collected [`InlineMathSpan`]s over a laid-out galley.
///
/// Runs *after* the galley exists, because that is the only way to know where a
/// placeholder landed. The placeholder is painted over, so it must be invisible
/// in the first place.
pub fn paint_inline_math(
    ctx: &egui::Context,
    painter: &egui::Painter,
    galley: &egui::Galley,
    spans: &[InlineMathSpan],
) {
    // `pos_from_cursor` already returns absolute coordinates, so the galley's own
    // placement is baked into the rect and needs no further offset.
    if spans.is_empty() {
        return;
    }
    let measure = EguiMeasure::new(ctx);
    for span in spans {
        if span.source.is_empty() {
            continue;
        }
        // The placeholder's rect gives the insertion point and the row's
        // baseline; the formula is drawn from that origin.
        let cc = egui::text::CCursor::new(egui::text::CharIndex(span.char_index));
        let rect = galley.pos_from_cursor(cc).min;
        let laid = crate::tex::render(&span.source, span.size, span.color, &measure);
        if laid.prims.is_empty() {
            continue;
        }
        // Centre the formula on the placeholder's row so it sits on the same
        // baseline as the surrounding text rather than at the top of the row.
        let row_h = galley.rect.height().max(laid.size[1]);
        let baseline_y = rect.y - (row_h - laid.size[1]) * 0.5;
        gfx_egui::paint(ctx, painter, egui::pos2(rect.x, baseline_y), &laid.prims);
    }
}

/// Inline highlighting for source mode.
///
/// Source mode must show the real text, so every delimiter stays visible and the
/// job only adds syntax colouring on top.
pub fn source_inline_job(line: &str, theme: &Theme) -> egui::text::LayoutJob {
    inline_job(
        line,
        0,
        line.len(),
        InlineMode::Source,
        theme,
        InlineVisual::body(theme),
        None,
    )
}

/// Text format for a content span.
fn content_format(style: InlineStyle, theme: &Theme, visual: InlineVisual) -> egui::TextFormat {
    let p = &theme.palette;
    let mut fmt = egui::TextFormat {
        font_id: FontId::new(visual.size, family_of(visual.bold_strong(style), style.code)),
        color: p.text.to_egui(),
        ..Default::default()
    };
    if style.strong {
        // No bold face for every script: the family switch does the work.
        fmt.font_id = FontId::new(visual.size, family_of(true, style.code));
    }
    if style.em {
        fmt.italics = true;
    }
    // A block-level italic (quote) applies to runs that did not ask for their
    // own emphasis, but must not override explicit bold-italic or code.
    if visual.italic && !style.code && !style.math && !style.mdx {
        fmt.italics = true;
    }
    if style.strike {
        fmt.strikethrough = egui::Stroke::new(1.0, p.text_muted.to_egui());
    }
    if style.code {
        fmt.color = p.code_text.to_egui();
        fmt.background = p.code_bg.to_egui();
    }
    if style.highlight {
        fmt.background = p.accent_soft.to_egui();
    }
    if style.link {
        fmt.color = p.link.to_egui();
        fmt.underline = egui::Stroke::new(1.0, p.link.to_egui());
    }
    if style.math {
        fmt.font_id = FontId::new(visual.size, egui::FontFamily::Name("mdp-math".into()));
        fmt.color = p.accent.to_egui();
    }
    if style.mdx {
        fmt.font_id = FontId::new(visual.size * 0.94, egui::FontFamily::Monospace);
        fmt.color = Rgba::hex(0xC792EA).to_egui();
        fmt.background = Rgba::hex(0xC792EA).with_alpha(0x1F).to_egui();
    }
    if visual.bold && visual.level == 0 {
        fmt.font_id = FontId::new(visual.size, family_of(true, style.code));
    }
    fmt
}

impl InlineVisual {
    fn bold_strong(&self, style: InlineStyle) -> bool {
        self.bold || style.strong
    }
}

/// The egui family for a (bold, mono) combination.
fn family_of(bold: bool, mono: bool) -> FontFamily {
    if mono {
        return FontFamily::Monospace;
    }
    if bold {
        FontFamily::Name("mdp-sans-bold".into())
    } else {
        FontFamily::Name("mdp-sans".into())
    }
}

/// A monospace job for code, with fence lines muted.
fn code_job(code: &str, theme: &Theme, _lang: &str) -> egui::text::LayoutJob {
    let size = theme.typo.code();
    let mut job = egui::text::LayoutJob::default();
    let normal = egui::TextFormat {
        font_id: FontId::new(size, FontFamily::Monospace),
        color: theme.palette.text.to_egui(),
        ..Default::default()
    };
    let fence = egui::TextFormat {
        font_id: FontId::new(size, FontFamily::Monospace),
        color: theme.palette.text_faint.to_egui(),
        ..Default::default()
    };
    for (i, line) in code.split('\n').enumerate() {
        if i > 0 {
            job.append("\n", 0.0, normal.clone());
        }
        let is_fence = {
            let t = line.trim();
            (t.starts_with("```") || t.starts_with("~~~")) && t.chars().filter(|c| *c == '`' || *c == '~').count() >= 3
        };
        job.append(
            line,
            0.0,
            if is_fence { fence.clone() } else { normal.clone() },
        );
    }
    job
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The stable widget id for a block path.
fn block_id(ui: &Ui, path: &[usize]) -> egui::Id {
    ui.make_persistent_id(("mdp-block", path.to_vec()))
}

/// The caret as a pair of *character* indices, if the widget has one.
fn caret_chars(ui: &Ui, id: egui::Id) -> Option<(usize, usize)> {
    let state = TextEditState::load(ui.ctx(), id)?;
    let range = state.cursor.char_range()?;
    let a = range.primary.index.0;
    let b = range.secondary.index.0;
    Some((a.min(b), a.max(b)))
}

/// Converts a character index into a byte offset for `text`.
pub fn char_to_byte(text: &str, char_index: usize) -> usize {
    if char_index == 0 {
        return 0;
    }
    match text.char_indices().nth(char_index) {
        Some((byte, _)) => byte,
        None => text.len(),
    }
}

/// Cache key for a diagram render.
///
/// `scale` is folded into the size so that changing the diagram font scale in the
/// settings re-renders instead of serving the previous drawing forever.
fn cache_key(engine: Engine, code: &str, dark: bool, size: f32, scale: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (engine == Engine::PlantUml).hash(&mut h);
    code.hash(&mut h);
    dark.hash(&mut h);
    size.to_bits().hash(&mut h);
    scale.to_bits().hash(&mut h);
    h.finish()
}

fn cache_key_math(tex: &str, size: f32, color: Rgba) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tex.hash(&mut h);
    size.to_bits().hash(&mut h);
    color.to_array().hash(&mut h);
    h.finish()
}

fn candidate_hint(syntax: CalloutSyntax) -> String {
    format!("{} 写法：{}", syntax.label(), syntax.sample())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::model::Document;

    fn editor_frame(
        context: &egui::Context,
        doc: &Document,
        state: &mut ViewState,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 900.0));
        context.run_pass(
            egui::RawInput { screen_rect: Some(screen), events, ..Default::default() },
            |context| {
                let mut ui = egui::Ui::new(
                    context.clone(),
                    egui::Id::unique("editor-regression"),
                    egui::UiBuilder::new()
                        .max_rect(screen)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                document_ui(
                    &mut ui,
                    state,
                    doc,
                    &Theme::dark(),
                    &RenderOpts { measure: 320.0, ..RenderOpts::default() },
                );
            },
        )
    }

    #[test]
    fn editing_chinese_after_an_ascii_paragraph_does_not_panic() {
        let context = egui::Context::default();
        crate::fonts::install(&context);
        let doc = Document::from_file("test.md".into(), "ab");
        let mut state = ViewState::default();
        state.begin_session(vec![0], &doc, &doc.blocks[0]);
        state.active.as_mut().unwrap().buffer = "中文".to_owned();
        editor_frame(&context, &doc, &mut state, Vec::new()).drop_without_applying_deltas();
        assert_eq!(state.active.as_ref().unwrap().buffer, "中文");
    }

    #[test]
    fn editable_math_keeps_character_offsets_equal_to_the_buffer() {
        let context = egui::Context::default();
        crate::fonts::install(&context);
        let source = "before $x^2$ after";
        let doc = Document::from_file("test.md".into(), source);
        let mut state = ViewState::default();
        state.begin_session(vec![0], &doc, &doc.blocks[0]);
        let output = editor_frame(&context, &doc, &mut state, Vec::new());
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == source)
        }));
        output.drop_without_applying_deltas();
    }

    #[test]
    fn table_and_markdoc_source_editors_accept_keyboard_input() {
        for source in ["| A | B |\n|---|---|\n| a | b |\n", "{% callout /%}\n"] {
            let context = egui::Context::default();
            crate::fonts::install(&context);
            let doc = Document::from_file("test.md".into(), source);
            let mut state = ViewState::default();
            state.begin_session(vec![0], &doc, &doc.blocks[0]);
            editor_frame(&context, &doc, &mut state, Vec::new()).drop_without_applying_deltas();
            editor_frame(
                &context,
                &doc,
                &mut state,
                vec![egui::Event::Text("新增".to_owned())],
            )
            .drop_without_applying_deltas();
            assert!(state.active.as_ref().unwrap().buffer.contains("新增"), "{source}");
        }
    }

    /// Paints a three-column table whose columns are left-, centre- and
    /// right-aligned, and returns where each cell's text landed plus the table's
    /// painted right edge.
    ///
    /// The right edge comes from the table's own background rectangle rather than
    /// from a formula: the vertical separators are clipped on a narrow sheet, so
    /// the background is the only trustworthy reference for "the column's edge".
    fn table_cell_positions() -> (Vec<(String, f32, f32)>, f32) {
        let source = "| AAAAAAAAAAAAA | B | C |\n|:--|:-:|--:|\n| a | b | c |\n";
        let context = egui::Context::default();
        crate::fonts::install(&context);
        let doc = Document::from_file("test.md".into(), source);
        let mut state = ViewState::default();
        let output = editor_frame(&context, &doc, &mut state, Vec::new());
        let painted = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((
                    text.galley.job.text.clone(),
                    text.pos.x,
                    text.galley.rect.width(),
                )),
                _ => None,
            })
            .collect();
        // The table's background is the widest rectangle on the sheet; the drop
        // shadows and card fills are narrower.
        let right_edge = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) => Some(rect.rect.right()),
                _ => None,
            })
            .fold(f32::NEG_INFINITY, f32::max);
        output.drop_without_applying_deltas();
        (painted, right_edge)
    }

    #[test]
    fn every_table_alignment_keeps_the_same_padding_on_both_sides() {
        let (painted, border) = table_cell_positions();
        let cell = |label: &str| {
            painted
                .iter()
                .find(|(text, _, _)| text == label)
                .map(|(_, x, width)| (*x, *width))
                .unwrap_or_else(|| panic!("the {label:?} cell was not painted"))
        };
        assert!(border.is_finite(), "the table background was not painted");
        let (_left_x, left_w) = cell("a");
        let (_center_x, center_w) = cell("b");
        let (right_x, right_w) = cell("c");
        // The right-aligned cell is the one under test: its ink must stop one
        // padding short of the table's right edge. Before the fix it stopped at the
        // edge itself, because the paint position forgot to subtract the padding
        // that the left column honours.
        let right_padding = border - (right_x + right_w);
        assert!(
            (right_padding - TABLE_PAD_X).abs() < 0.5,
            "right-aligned cell had {right_padding} padding, expected {TABLE_PAD_X} \
             (border {border}, ink {}..{}, other columns {left_w} / {center_w})",
            right_x,
            right_x + right_w
        );
    }

    #[test]
    fn long_paragraphs_wrap_in_both_rendered_and_editable_views() {
        let source = "a long paragraph with many words ".repeat(30);
        let doc = Document::from_file("test.md".into(), &source);
        for active in [false, true] {
            let context = egui::Context::default();
            crate::fonts::install(&context);
            let mut state = ViewState::default();
            if active {
                state.begin_session(vec![0], &doc, &doc.blocks[0]);
            }
            let output = editor_frame(&context, &doc, &mut state, Vec::new());
            let text = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == source => Some(text),
                _ => None,
            }).expect("paragraph was painted");
            assert!(text.galley.rows.len() > 1, "active={active}");
            assert!(text.galley.size().x <= 320.1, "active={active}: {:?}", text.galley.size());
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn focus_and_checkbox_actions_survive_a_simultaneous_blur_commit() {
        for next in [
            EditorAction::Focus { path: vec![0], caret: 0 },
            EditorAction::ToggleTask { span: 0..8, checked: true },
        ] {
            let mut action = Some(next.clone());
            merge_action(&mut action, EditorAction::Commit { span: 10..20, text: "edited".into() });
            assert!(!matches!(action, Some(EditorAction::Commit { .. })));
            let mut action = Some(EditorAction::Commit { span: 10..20, text: "edited".into() });
            merge_action(&mut action, next);
            assert!(!matches!(action, Some(EditorAction::Commit { .. })));
        }
    }

    #[test]
    fn char_to_byte_handles_cjk() {
        let text = "\u{4E2D}\u{6587}abc";
        assert_eq!(char_to_byte(text, 0), 0);
        assert_eq!(char_to_byte(text, 1), 3);
        assert_eq!(char_to_byte(text, 2), 6);
        assert_eq!(char_to_byte(text, 5), text.len());
        // Out of range must clamp, never panic.
        assert_eq!(char_to_byte(text, 99), text.len());
    }

    #[test]
    fn session_buffer_matches_the_block_slice() {
        let doc = Document::from_file("t.md".into(), "# Title\n\nbody text\n");
        let block = &doc.blocks[0];
        let mut state = ViewState::default();
        state.begin_session(vec![0], &doc, block);
        let session = state.active.expect("session");
        assert_eq!(session.buffer, &doc.text[block.span.clone()]);
    }

    #[test]
    fn stale_sessions_are_discarded() {
        let mut doc = Document::from_file("t.md".into(), "body\n");
        let mut state = ViewState::default();
        state.begin_session(vec![0], &doc, &doc.blocks[0]);
        assert!(state.active.is_some());
        doc.set_text("changed\n".to_owned());
        // The next frame must notice the revision bump.
        assert_ne!(
            state.active.as_ref().map(|s| s.revision),
            Some(doc.revision)
        );
    }

    #[test]
    fn hidden_delimiters_keep_the_text_identical() {
        // The job's text must equal the source, otherwise egui's cursor offsets
        // would drift out of sync with what the user sees.
        let theme = Theme::dark();
        let src = "## a **b** `c` $d$";
        let job = inline_job(
            src,
            3,
            src.len() - 3,
            InlineMode::Wysiwyg,
            &theme,
            InlineVisual {
                size: 16.0,
                bold: true,
                level: 2,
                italic: false,
            },
            None,
        );
        assert_eq!(job.text, src);
    }

    #[test]
    fn inline_math_is_typeset_rather_than_printed() {
        // Regression guard: `$E = mc^2$` used to be painted as literal source
        // text, because `tex::render` was never called on the inline path.
        let theme = Theme::dark();
        let src = "before $E = mc^2$ after";
        let mut math = Vec::new();
        let job = inline_job(
            src,
            0,
            src.len(),
            InlineMode::Wysiwyg,
            &theme,
            InlineVisual::body(&theme),
            Some(&mut math),
        );
        assert_eq!(math.len(), 1, "no formula was recognised in {src:?}");
        assert_eq!(math[0].source, "E = mc^2");
        // The formula must not also be printed as text.
        assert!(
            !job.text.contains("E = mc^2"),
            "formula leaked into the text job: {:?}",
            job.text
        );
        let ph = MATH_PLACEHOLDER.chars().next().unwrap();
        assert_eq!(
            job.text.chars().filter(|c| *c == ph).count(),
            1,
            "expected exactly one placeholder in {:?}",
            job.text
        );
    }

    #[test]
    fn inline_math_placeholders_are_indexed_correctly() {
        // Two formulas on one line: each placeholder index must point at its own
        // character, or the formulas get painted over each other.
        let theme = Theme::dark();
        let src = "a $x$ b $y$ c";
        let mut math = Vec::new();
        let job = inline_job(
            src,
            0,
            src.len(),
            InlineMode::Wysiwyg,
            &theme,
            InlineVisual::body(&theme),
            Some(&mut math),
        );
        assert_eq!(math.len(), 2, "expected two formulas");
        let chars: Vec<char> = job.text.chars().collect();
        let ph = MATH_PLACEHOLDER.chars().next().unwrap();
        for (i, span) in math.iter().enumerate() {
            assert_eq!(
                chars.get(span.char_index),
                Some(&ph),
                "span {i} does not point at its placeholder (job = {:?})",
                job.text
            );
        }
        assert_ne!(math[0].char_index, math[1].char_index);
    }

    #[test]
    fn source_mode_keeps_math_as_source() {
        // Source mode must show `$…$` verbatim; it is the raw text view.
        let theme = Theme::dark();
        let src = "E = $x^2$";
        let job = source_inline_job(src, &theme);
        assert!(
            job.text.contains("$x^2$"),
            "source mode lost the dollar signs: {:?}",
            job.text
        );
    }

    #[test]
    fn inline_math_keeps_surrounding_text_intact() {
        // The placeholder swap must not disturb the rest of the line.
        let theme = Theme::dark();
        let src = "**bold** and $x$";
        let mut math = Vec::new();
        let job = inline_job(
            src,
            0,
            src.len(),
            InlineMode::Wysiwyg,
            &theme,
            InlineVisual::body(&theme),
            Some(&mut math),
        );
        assert_eq!(math.len(), 1);
        assert!(
            job.text.contains("bold"),
            "emphasis text was lost: {:?}",
            job.text
        );
        assert!(job.text.contains(" and "), "connector lost: {:?}", job.text);
    }

    #[test]
    fn wysiwyg_hides_the_block_marker() {
        // `## ` must not be painted: the heading style already conveys the
        // level, and showing the hashes was the "still looks like Markdown"
        // complaint.
        let theme = Theme::dark();
        let src = "## Title";
        let job = inline_job(
            src,
            3,
            src.len() - 3,
            InlineMode::Wysiwyg,
            &theme,
            InlineVisual::body(&theme),
            None,
        );
        let marker: String = job
            .sections
            .iter()
            .filter(|s| s.format.color == Color32::TRANSPARENT)
            .filter_map(|s| job.text.get(s.byte_range.start.0..s.byte_range.end.0))
            .collect();
        assert_eq!(marker, "## ", "block marker was left visible");
    }

    #[test]
    fn wysiwyg_hides_emphasis_delimiters() {
        let theme = Theme::dark();
        let src = "plain **bold** more";
        let job = inline_job(
            src,
            0,
            src.len(),
            // Caret deliberately parked inside the bold run: WYSIWYG must still
            // hide the asterisks. This is the behaviour the user asked for.
            InlineMode::Wysiwyg,
            &theme,
            InlineVisual::body(&theme),
            None,
        );
        let mut hidden = String::new();
        for s in &job.sections {
            if s.format.color == Color32::TRANSPARENT {
                hidden.push_str(job.text.get(s.byte_range.start.0..s.byte_range.end.0).unwrap_or(""));
            }
        }
        assert!(
            hidden.contains("**"),
            "asterisks stayed visible with the caret inside; got {hidden:?}"
        );
    }

    #[test]
    fn content_is_never_hidden() {
        // Guard against over-correcting: real text must remain visible.
        let theme = Theme::dark();
        let src = "**bold**";
        let job = inline_job(
            src,
            0,
            src.len(),
            InlineMode::Wysiwyg,
            &theme,
            InlineVisual::body(&theme),
            None,
        );
        assert!(
            job.sections
                .iter()
                .any(|s| s.format.color != Color32::TRANSPARENT),
            "everything went invisible"
        );
    }

    #[test]
    fn source_mode_still_reveals_delimiters() {
        // The counterpart guarantee: source mode must show the real syntax, or
        // the "view source" button would be useless.
        let theme = Theme::dark();
        let src = "**bold**";
        let job = source_inline_job(src, &theme);
        let visible: String = job
            .sections
            .iter()
            .filter(|s| s.format.color != Color32::TRANSPARENT)
            .filter_map(|s| job.text.get(s.byte_range.start.0..s.byte_range.end.0))
            .collect();
        assert!(
            visible.contains("**"),
            "source mode hid the asterisks; got {visible:?}"
        );
    }

    #[test]
    fn block_markers_still_occupy_width() {
        // Hidden sections must still advance, otherwise the caret cannot be
        // positioned consistently against the buffer.
        let base = egui::TextFormat {
            font_id: FontId::new(16.0, FontFamily::Proportional),
            ..Default::default()
        };
        let f = hidden_format(&base);
        assert!(
            f.font_id.size < 2.0,
            "hidden text still takes up real width: {}pt",
            f.font_id.size
        );
        assert_eq!(f.color, Color32::TRANSPARENT);
    }

    #[test]
    fn quote_visual_italicises_plain_runs() {
        let theme = Theme::dark();
        let mut visual = InlineVisual::body(&theme);
        assert!(!visual.italic);
        visual.italic = true;
        let plain = InlineStyle::default();
        assert!(
            content_format(plain, &theme, visual).italics,
            "quote text was not italicised"
        );

        // Code spans keep their own look instead of inheriting italics.
        let code = InlineStyle {
            code: true,
            ..InlineStyle::default()
        };
        assert!(!content_format(code, &theme, visual).italics);
    }

    #[test]
    fn code_job_preserves_line_structure() {
        let theme = Theme::dark();
        let src = "```rust\nfn main() {}\n```";
        let job = code_job(src, &theme, "rust");
        assert_eq!(job.text, src);
    }

    #[test]
    fn cache_keys_distinguish_inputs() {
        let a = cache_key(Engine::Mermaid, "graph TD;A-->B", true, 15.0, 1.0);
        let b = cache_key(Engine::Mermaid, "graph TD;A-->B", false, 15.0, 1.0);
        let c = cache_key(Engine::PlantUml, "graph TD;A-->B", true, 15.0, 1.0);
        let d = cache_key(Engine::Mermaid, "graph TD;A-->C", true, 15.0, 1.0);
        for (i, x) in [b, c, d].iter().enumerate() {
            assert_ne!(a, *x, "collision at index {i}");
        }
        // The diagram font scale is an input too: changing it in the settings must
        // produce a different key, otherwise the old drawing is cached forever.
        assert_ne!(a, cache_key(Engine::Mermaid, "graph TD;A-->B", true, 15.0, 1.3));
        // Stability across calls matters for cache hits.
        assert_eq!(
            a,
            cache_key(Engine::Mermaid, "graph TD;A-->B", true, 15.0, 1.0)
        );
    }
}

#[cfg(test)]
mod repro_tests {
    use super::*;
    use egui::RawInput;

    const SAMPLE: &str = "# 标题\n\n这是一段正文，用来验证文字确实被画出来了。\n\n- 列表项一\n- 列表项二\n\n> 引用内容\n";

    /// Regression guard for the "text is invisible" bug.
    ///
    /// Delimiters are hidden with a 1pt transparent font. The negative letter
    /// spacing that once accompanied it was computed from the *body* font size,
    /// so every hidden character subtracted ~14pt and dragged whole lines off
    /// the left edge of the page. This asserts the rendered job stays a sane
    /// width: close to the visible text, and never absurdly wide or negative.
    #[test]
    fn hidden_delimiters_do_not_destroy_line_width() {
        let ctx = egui::Context::default();
        let out = ctx.run_pass(RawInput::default(), |ctx| {
            install_fonts_for_test(ctx);
            let theme = Theme::dark();
            let plain = "bold";
            let marked = "**bold**";
            let a = layout_width(ctx, plain, &theme);
            let b = layout_width(ctx, marked, &theme);
            assert!(a > 10.0, "plain text measured {a}");
            assert!(
                b > 0.0,
                "marked-up text measured {b} — it was laid out off-page"
            );
            // The four hidden asterisks may add a little width, but must not add
            // anything close to a body-size em per character.
            assert!(
                b < a + 24.0,
                "hidden delimiters added {}pt for four characters",
                b - a
            );
        });
        // Laying out glyphs grows the font atlas; a headless test has no GPU to
        // upload it to, so the deltas must be dropped explicitly.
        out.drop_without_applying_deltas();
    }

    fn layout_width(ctx: &egui::Context, text: &str, theme: &Theme) -> f32 {
        let job = inline_job(
            text,
            0,
            text.len(),
            InlineMode::Wysiwyg,
            theme,
            InlineVisual::body(theme),
            None,
        );
        ctx.fonts_mut(|f| f.layout_job(job)).size().x
    }

    fn install_fonts_for_test(ctx: &egui::Context) {
        let _ = crate::fonts::install(ctx);
    }

    /// Renders a whole document headlessly and returns the shapes drawn for
    /// text, so a test can assert the glyphs actually land on screen.
    #[test]
    fn document_text_lands_inside_the_viewport() {
        let ctx = egui::Context::default();
        let out = ctx.run_pass(
            RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1200.0, 900.0),
                )),
                ..Default::default()
            },
            |ctx| {
                install_fonts_for_test(ctx);
                let theme = Theme::dark();
                let doc = Document::from_file(
                    std::path::PathBuf::from("t.md"),
                    SAMPLE,
                );
                let mut view = ViewState::default();
                let opts = RenderOpts { measure: 600.0, ..RenderOpts::default() };
                // `CentralPanel::show` needs a parent `Ui`, which a headless pass
                // does not have; build one directly over the screen rect.
                let mut ui = egui::Ui::new(
                    ctx.clone(),
                    egui::Id::unique("repro"),
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(1200.0, 900.0),
                    )),
                );
                ui.set_width(1200.0);
                let _ = document_ui(&mut ui, &mut view, &doc, &theme, &opts);
            },
        );
        // Walk every tessellated vertex. Glyphs that landed outside the window
        // are exactly what "no text visible" looks like from the outside.
        let mut visible = 0usize;
        let mut outside = 0usize;
        for clipped in ctx.tessellate(out.shapes.clone(), out.pixels_per_point) {
            let mesh = match clipped.primitive {
                egui::epaint::Primitive::Mesh(m) => m,
                _ => continue,
            };
            for v in mesh.vertices {
                if !(v.pos.x.is_finite() && v.pos.y.is_finite()) {
                    continue;
                }
                if (-200.0..1400.0).contains(&v.pos.x) && (-200.0..1100.0).contains(&v.pos.y) {
                    visible += 1;
                } else {
                    outside += 1;
                }
            }
        }
        assert!(
            visible > 50,
            "almost nothing was drawn on screen (visible={visible})"
        );
        assert!(
            outside < visible,
            "most geometry landed off-screen: visible={visible} outside={outside}"
        );
        out.drop_without_applying_deltas();
    }
}
