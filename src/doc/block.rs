//! Block-level parser.
//!
//! Line-oriented, single pass, and deliberately permissive: anything it cannot
//! classify becomes a paragraph rather than being dropped, and every construct it
//! does recognise records the exact byte range it came from so the editor can
//! rewrite that range in place.
//!
//! Beyond CommonMark's block structure this understands the four callout syntaxes
//! MarkdownPad supports (GitHub alerts, Obsidian callouts, VitePress containers,
//! Docusaurus admonitions), MDX `import`/`export` and JSX blocks, Markdoc
//! `{% tag %}` pairs and references, `$…$` / `$$…$$` math, and GFM tables.

use std::ops::Range;

use super::model::{
    Align, Block, BlockKind, Callout, CalloutSyntax, CalloutVariant, Flavor, ListKind,
    MarkdocTagKind,
};

// ---------------------------------------------------------------------------
// Line table
// ---------------------------------------------------------------------------

/// A logical line: the text the parser examines, plus where it came from.
///
/// `content` is not always a slice of the document — a line inside a block quote
/// has its `> ` marker removed — so the origin is tracked explicitly instead of
/// being recomputed from offsets.
#[derive(Clone, Debug)]
struct Line {
    /// Absolute offset of the physical line's first byte.
    start: usize,
    /// Absolute offset just past the physical line, including its newline.
    end: usize,
    /// Absolute offset where `content` begins in the document.
    content_start: usize,
    /// The text to parse (may have container markers stripped).
    content: String,
}

impl Line {
    /// Absolute byte range of this line's parsed content.
    fn content_range(&self) -> Range<usize> {
        self.content_start..self.content_start + self.content.len()
    }

    fn is_blank(&self) -> bool {
        self.content.trim().is_empty()
    }

    fn trimmed(&self) -> &str {
        self.content.trim()
    }

    /// Number of leading spaces/tabs (tabs count as 4).
    fn indent(&self) -> usize {
        let mut n = 0;
        for c in self.content.chars() {
            match c {
                ' ' => n += 1,
                '\t' => n += 4,
                _ => break,
            }
        }
        n
    }
}

/// Splits `text[range]` into lines, preserving absolute offsets.
fn lines_of(text: &str, range: Range<usize>) -> Vec<Line> {
    let start_bound = range.start.min(text.len());
    let end_bound = range.end.min(text.len()).max(start_bound);
    let slice = text.get(start_bound..end_bound).unwrap_or("");
    let mut out = Vec::new();
    let mut line_start = 0usize;
    for (i, ch) in slice.char_indices() {
        if ch == '\n' {
            let content = slice[line_start..i].strip_suffix('\r').unwrap_or(&slice[line_start..i]);
            out.push(Line {
                start: start_bound + line_start,
                end: start_bound + i + 1,
                content_start: start_bound + line_start,
                content: content.to_owned(),
            });
            line_start = i + 1;
        }
    }
    if line_start < slice.len() {
        out.push(Line {
            start: start_bound + line_start,
            end: start_bound + slice.len(),
            content_start: start_bound + line_start,
            content: slice[line_start..].to_owned(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Parses a whole document into blocks.
pub fn parse_document(text: &str, flavor: Flavor) -> Vec<Block> {
    let lines = lines_of(text, 0..text.len());
    let parser = Parser { text, flavor };
    parser.parse_lines(&lines, 0, lines.len(), 0)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser<'a> {
    text: &'a str,
    flavor: Flavor,
}

/// Guard against pathological nesting.
const MAX_NEST: u8 = 12;

impl Parser<'_> {
    fn parse_lines(&self, lines: &[Line], from: usize, to: usize, depth: u8) -> Vec<Block> {
        let mut out = Vec::new();
        if depth > MAX_NEST {
            return out;
        }
        let mut i = from;
        while i < to {
            let line = &lines[i];
            if line.is_blank() {
                i += 1;
                continue;
            }
            let t = line.content.trim_start();

            // Front matter, only at the very top of the document.
            if i == 0 && depth == 0
                && let Some((close, delim)) = front_matter_close(lines, i, to)
            {
                let raw = self.slice(lines[i].start..lines[close].end).to_owned();
                out.push(Block::new(
                    BlockKind::FrontMatter {
                        delimiter: delim,
                        raw,
                    },
                    lines[i].start..lines[close].end,
                    0..0,
                ));
                i = close + 1;
                continue;
            }

            // Fenced code (and therefore Mermaid / PlantUML diagrams).
            if let Some((fence_char, fence_len, info)) = fence_open(t) {
                let (close, next) = fence_close(lines, i, to, fence_char, fence_len);
                let body_start = if i + 1 < to {
                    lines[i + 1].start
                } else {
                    lines[i].end
                };
                let body_end = if close < next {
                    lines[close].start
                } else {
                    lines[to - 1].end
                };
                let body_end = body_end.max(body_start);
                let code = self.slice(body_start..body_end).to_owned();
                let lang = info
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                out.push(Block::new(
                    BlockKind::Code {
                        lang,
                        info,
                        code,
                        backticks: fence_char == '`',
                        fence_len,
                    },
                    lines[i].start..lines[next - 1].end,
                    body_start..body_end,
                ));
                i = next;
                continue;
            }

            // Display math.
            if let Some((close, next, tex)) = math_block(lines, i, to) {
                out.push(Block::new(
                    BlockKind::Math { tex },
                    lines[i].start..lines[next - 1].end,
                    0..0,
                ));
                let _ = close;
                i = next;
                continue;
            }

            // ATX heading.
            if let Some((level, text, span)) = atx_heading(line) {
                let inline_span = span.clone();
                out.push(Block::new(
                    BlockKind::Heading { level, text },
                    lines[i].content_start..lines[i].content_range().end.max(line.start),
                    inline_span,
                ));
                i += 1;
                continue;
            }

            // Thematic break. Checked after headings so `# ---` still works.
            if is_thematic_break(t) && !self.starts_setext(lines, i, to) {
                out.push(Block::new(BlockKind::Rule, lines[i].start..lines[i].end, 0..0));
                i += 1;
                continue;
            }

            // GFM table.
            if is_table_start(lines, i, to)
                && let Some(next) = self.parse_table(lines, i, to, &mut out)
            {
                i = next;
                continue;
            }

            // VitePress / Docusaurus container.
            if let Some((colons, name, title, spaced)) = container_open(t)
                && name.is_some()
                && let Some(next) =
                    self.parse_container(lines, i, to, depth, colons, name, title, spaced, &mut out)
            {
                i = next;
                continue;
            }

            // Markdoc tag.
            if t.starts_with("{%")
                && let Some(next) = self.parse_markdoc(lines, i, to, depth, &mut out)
            {
                i = next;
                continue;
            }

            // MDX import / export statement.
            if is_esm_start(t)
                && let Some(next) = self.parse_esm(lines, i, to, &mut out)
            {
                i = next;
                continue;
            }

            // JSX block element.
            if let Some(tag) = jsx_open(t)
                && let Some(next) = self.parse_jsx(lines, i, to, depth, &tag, &mut out)
            {
                i = next;
                continue;
            }

            // Block-level raw HTML.
            if let Some(tag) = html_open(t)
                && is_block_html_tag(&tag)
                && let Some(next) = self.parse_html(lines, i, to, &mut out)
            {
                i = next;
                continue;
            }

            // Block quote, possibly a callout.
            if t.starts_with('>')
                && let Some(next) = self.parse_quote(lines, i, to, depth, &mut out)
            {
                i = next;
                continue;
            }

            // List item.
            if let Some(item) = list_item(line) {
                out.push(item);
                i += 1;
                continue;
            }

            // Paragraph (the fallback).
            let next = self.parse_paragraph(lines, i, to, &mut out);
            i = next.max(i + 1);
        }
        out
    }

    fn slice(&self, range: Range<usize>) -> &str {
        self.text.get(range).unwrap_or("")
    }

    /// `true` when line `i` is a paragraph followed by a `===`/`---` underline.
    fn starts_setext(&self, lines: &[Line], i: usize, to: usize) -> bool {
        let _ = (lines, i, to);
        false
    }

    // -- paragraphs --------------------------------------------------------

    fn parse_paragraph(
        &self,
        lines: &[Line],
        i: usize,
        to: usize,
        out: &mut Vec<Block>,
    ) -> usize {
        let mut j = i + 1;
        while j < to {
            let l = &lines[j];
            if l.is_blank() {
                break;
            }
            let t = l.content.trim_start();
            // Setext underline upgrades the paragraph to a heading.
            if j == i + 1 && is_setext_underline(t, '=') {
                let text = lines[i].trimmed().to_owned();
                let span = lines[i].content_range();
                out.push(Block::new(
                    BlockKind::Heading { level: 1, text },
                    lines[i].content_start..lines[j].end,
                    span,
                ));
                return j + 1;
            }
            if j == i + 1 && is_setext_underline(t, '-') {
                let text = lines[i].trimmed().to_owned();
                let span = lines[i].content_range();
                out.push(Block::new(
                    BlockKind::Heading { level: 2, text },
                    lines[i].content_start..lines[j].end,
                    span,
                ));
                return j + 1;
            }
            if self.starts_new_block(lines, j, to) {
                break;
            }
            j += 1;
        }
        let text = (i..j)
            .map(|k| lines[k].content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let span = lines[i].content_start..lines[j - 1].content_range().end;
        out.push(Block::new(
            BlockKind::Paragraph { text },
            span.clone(),
            span,
        ));
        j
    }

    /// `true` when line `j` begins a construct other than a paragraph.
    fn starts_new_block(&self, lines: &[Line], j: usize, to: usize) -> bool {
        let line = &lines[j];
        let t = line.content.trim_start();
        if t.is_empty() {
            return true;
        }
        if fence_open(t).is_some()
            || atx_heading(line).is_some()
            || is_thematic_break(t)
            || t.starts_with('>')
            || list_item(line).is_some()
            || is_esm_start(t)
            || t.starts_with("{%")
            || jsx_open(t).is_some()
        {
            return true;
        }
        if let Some((_, name, _, _)) = container_open(t)
            && name.is_some()
        {
            return true;
        }
        if is_table_start(lines, j, to) {
            return true;
        }
        if math_block(lines, j, to).is_some() {
            return true;
        }
        false
    }

    // -- tables ------------------------------------------------------------

    fn parse_table(
        &self,
        lines: &[Line],
        i: usize,
        to: usize,
        out: &mut Vec<Block>,
    ) -> Option<usize> {
        let header = split_row(&lines[i].content);
        let align = parse_align_row(&lines[i + 1].content)?;
        let mut rows = Vec::new();
        let mut j = i + 2;
        while j < to {
            let l = &lines[j];
            if l.is_blank() || !l.content.contains('|') {
                break;
            }
            rows.push(split_row(&l.content));
            j += 1;
        }
        out.push(Block::new(
            BlockKind::Table {
                align,
                header,
                rows,
            },
            lines[i].start..lines[j - 1].end,
            0..0,
        ));
        Some(j)
    }

    // -- containers --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn parse_container(
        &self,
        lines: &[Line],
        i: usize,
        to: usize,
        depth: u8,
        colons: usize,
        name: Option<String>,
        title: String,
        spaced: bool,
        out: &mut Vec<Block>,
    ) -> Option<usize> {
        let name = name?;
        // Find the closing line: the same number of colons and nothing else.
        let mut close = None;
        for j in (i + 1)..to {
            let t = lines[j].trimmed();
            if t.len() >= colons && t.chars().all(|c| c == ':') && t.len() == colons {
                close = Some(j);
                break;
            }
        }
        let body_start = i + 1;
        let body_end = close.unwrap_or(to);
        let syntax = container_syntax(&name, spaced);
        let variant = CalloutVariant::from_key(syntax, &name);
        let title = if title.trim().is_empty() {
            None
        } else {
            Some(title)
        };
        let body = self.parse_lines(lines, body_start, body_end, depth + 1);
        let body_span = if body_start < body_end {
            lines[body_start].content_start..lines[body_end - 1].content_range().end
        } else {
            0..0
        };
        let end = close.map(|c| lines[c].end).unwrap_or_else(|| lines[to - 1].end);
        out.push(Block::new(
            BlockKind::Callout(Callout {
                syntax,
                variant,
                title,
                foldable: None,
                body,
                body_span,
            }),
            lines[i].start..end,
            0..0,
        ));
        Some(close.map(|c| c + 1).unwrap_or(to))
    }

    // -- block quotes and callouts -----------------------------------------

    fn parse_quote(
        &self,
        lines: &[Line],
        i: usize,
        to: usize,
        depth: u8,
        out: &mut Vec<Block>,
    ) -> Option<usize> {
        // Collect the run of `>`-prefixed lines, stripping the marker.
        let mut inner: Vec<Line> = Vec::new();
        let mut j = i;
        while j < to {
            let l = &lines[j];
            let t = l.content.trim_start();
            if let Some(after) = t.strip_prefix('>') {
                let skipped = after.len() - after.trim_start_matches([' ', '\t']).len();
                let lead_ws = l.content.len() - t.len();
                let stripped = &after[skipped.min(after.len())..];
                let content_start = l.content_start + lead_ws + 1 + skipped;
                inner.push(Line {
                    start: l.start,
                    end: l.end,
                    content_start,
                    content: stripped.to_owned(),
                });
                j += 1;
            } else {
                // A blank line ends the quote. Continuing across it (which this
                // used to do whenever the next line still started with `>`) glued
                // two separate blockquotes into one block — so a document with
                // several callouts rendered only the first, with the rest shown as
                // literal `[!TIP]` lines inside it. To continue a quote across a
                // paragraph break, CommonMark requires a `>` on the blank line,
                // which the branch above already handles.
                break;
            }
        }
        if inner.is_empty() {
            return None;
        }

        // Is the first non-blank inner line a callout marker?
        let marker_idx = inner.iter().position(|l| !l.is_blank());
        if let Some(mi) = marker_idx
            && let Some((variant, foldable, title)) = callout_marker(&inner[mi].content)
        {
            let syntax = classify_callout_syntax(&inner[mi].content, &variant, title.as_deref());
            let body = self.parse_lines(&inner, mi + 1, inner.len(), depth + 1);
            let body_span = if mi + 1 < inner.len() {
                inner[mi + 1].content_start..inner[inner.len() - 1].content_range().end
            } else {
                0..0
            };
            out.push(Block::new(
                BlockKind::Callout(Callout {
                    syntax,
                    variant,
                    title,
                    foldable,
                    body,
                    body_span,
                }),
                lines[i].start..lines[j - 1].end,
                0..0,
            ));
            return Some(j);
        }

        // Plain block quote.
        let text = inner
            .iter()
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned();
        let inline_span = if inner.is_empty() {
            0..0
        } else {
            inner[0].content_start..inner[inner.len() - 1].content_range().end
        };
        out.push(Block::new(
            BlockKind::Quote { text },
            lines[i].start..lines[j - 1].end,
            inline_span,
        ));
        Some(j)
    }

    // -- Markdoc -----------------------------------------------------------

    fn parse_markdoc(
        &self,
        lines: &[Line],
        i: usize,
        to: usize,
        depth: u8,
        out: &mut Vec<Block>,
    ) -> Option<usize> {
        let line = &lines[i];
        let t = line.content.trim_start();
        let lead = line.content.len() - t.len();
        let close_rel = t.find("%}")?;
        let inner = t.get(2..close_rel)?.trim();
        if inner.is_empty() {
            return None;
        }

        let (kind, name, attrs) = classify_markdoc(inner);
        let raw = line.content.trim().to_owned();

        // A container tag consumes everything up to its closing tag.
        // Comments and loop bodies own a body too, so they must be grouped
        // before the runtime can drop or repeat them.
        let is_pair = matches!(
            kind,
            MarkdocTagKind::Pair | MarkdocTagKind::Conditional | MarkdocTagKind::Comment
        )
            && !inner.ends_with('/');
        if is_pair && let Some(close) = markdoc_close(lines, i + 1, to, &name) {
            let body = self.parse_lines(lines, i + 1, close, depth + 1);
            let body_span = if i + 1 < close {
                lines[i + 1].content_start..lines[close - 1].content_range().end
            } else {
                0..0
            };
            out.push(Block::new(
                BlockKind::MarkdocTag {
                    kind,
                    name,
                    attrs,
                    body_span,
                    raw,
                    body,
                },
                line.start..lines[close].end,
                0..0,
            ));
            return Some(close + 1);
        }

        // Leaf tag.
        let _ = lead;
        out.push(Block::new(
            BlockKind::MarkdocTag {
                kind,
                name,
                attrs,
                body_span: 0..0,
                raw,
                body: Vec::new(),
            },
            line.start..line.end,
            0..0,
        ));
        Some(i + 1)
    }

    // -- MDX ---------------------------------------------------------------

    fn parse_esm(
        &self,
        lines: &[Line],
        i: usize,
        to: usize,
        out: &mut Vec<Block>,
    ) -> Option<usize> {
        let mut j = i;
        let mut acc = String::new();
        while j < to {
            let l = &lines[j];
            if j > i && l.is_blank() {
                break;
            }
            if !acc.is_empty() {
                acc.push('\n');
            }
            acc.push_str(&l.content);
            j += 1;
            if bracket_balance(&acc) <= 0 && statement_complete(&acc) {
                break;
            }
        }
        if acc.trim().is_empty() {
            return None;
        }
        out.push(Block::new(
            BlockKind::MdxEsm { raw: acc },
            lines[i].start..lines[j - 1].end,
            0..0,
        ));
        Some(j)
    }

    fn parse_jsx(
        &self,
        lines: &[Line],
        i: usize,
        to: usize,
        depth: u8,
        tag: &str,
        out: &mut Vec<Block>,
    ) -> Option<usize> {
        let line = &lines[i];
        let t = line.content.trim_start();
        let lead = line.content.len() - t.len();
        let name_end = 1 + tag.len();
        if name_end > t.len() {
            return None;
        }
        // Start the search after the tag name: `find_tag_close` treats a leading
        // `<` as an abort, so it must not be handed the opening bracket.
        let open_end = find_tag_close(t, name_end)?;

        // Attributes are everything between the tag name and the closing bracket.
        let before_bracket = t.get(name_end..open_end).unwrap_or("");
        let self_closing = before_bracket.trim_end().ends_with('/');
        let props = before_bracket
            .trim_end()
            .trim_end_matches('/')
            .trim()
            .to_owned();

        if self_closing {
            out.push(Block::new(
                BlockKind::MdxJsx {
                    name: tag.to_owned(),
                    props,
                    self_closing: true,
                    children_raw: String::new(),
                    children_span: 0..0,
                    children: Vec::new(),
                },
                line.start..line.end,
                0..0,
            ));
            return Some(i + 1);
        }

        // Scan forward for the matching close tag, tracking nesting of the same
        // element name.
        let mut depth_count = 1usize;
        let mut close_line = None;
        let mut close_offset = 0usize;
        for j in i..to {
            let content = &lines[j].content;
            let from = if j == i { open_end + 1 } else { 0 };
            if let Some((off, kind)) = find_tag_event(content, from, tag) {
                match kind {
                    TagEvent::Open => depth_count += 1,
                    TagEvent::Close => {
                        depth_count -= 1;
                        if depth_count == 0 {
                            close_line = Some(j);
                            close_offset = off;
                            break;
                        }
                    }
                }
            }
        }

        let (children_start, children_end, next) = match close_line {
            Some(cl) => {
                let children_start = lines[i].content_start + open_end + 1;
                let children_end = lines[cl].content_start + close_offset;
                (children_start, children_end, cl + 1)
            }
            None => {
                // Unterminated: take the rest of the run as children.
                let children_start = lines[i].content_start + open_end + 1;
                let children_end = lines[to - 1].content_range().end;
                (children_start, children_end, to)
            }
        };
        let children_end = children_end.max(children_start);
        let children_raw = self.slice(children_start..children_end).to_owned();
        let child_lines = lines_of(self.text, children_start..children_end);
        let children = self.parse_lines(&child_lines, 0, child_lines.len(), depth + 1);

        let end = match close_line {
            Some(cl) => lines[cl].end,
            None => lines[to - 1].end,
        };
        let _ = lead;
        out.push(Block::new(
            BlockKind::MdxJsx {
                name: tag.to_owned(),
                props,
                self_closing: false,
                children_raw,
                children_span: children_start..children_end,
                children,
            },
            line.start..end,
            0..0,
        ));
        Some(next)
    }

    fn parse_html(
        &self,
        lines: &[Line],
        i: usize,
        to: usize,
        out: &mut Vec<Block>,
    ) -> Option<usize> {
        let mut j = i + 1;
        while j < to && !lines[j].is_blank() {
            j += 1;
        }
        let raw = (i..j)
            .map(|k| lines[k].content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        out.push(Block::new(
            BlockKind::Html { raw },
            lines[i].start..lines[j - 1].end,
            0..0,
        ));
        Some(j)
    }
}

// ---------------------------------------------------------------------------
// Construct detectors
// ---------------------------------------------------------------------------

/// Opening fence, if `t` starts one.
fn fence_open(t: &str) -> Option<(char, usize, String)> {
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = t.chars().take_while(|&x| x == c).count();
    if n < 3 {
        return None;
    }
    let info = t.get(n..)?.trim();
    // A backtick fence's info string may not contain backticks.
    if c == '`' && info.contains('`') {
        return None;
    }
    Some((c, n, info.to_owned()))
}

/// Finds the closing fence. Returns `(close_index, next_index)`; when the fence
/// is unterminated, `close_index == to` and `next_index == to`.
fn fence_close(
    lines: &[Line],
    i: usize,
    to: usize,
    fence_char: char,
    fence_len: usize,
) -> (usize, usize) {
    for j in (i + 1)..to {
        let t = lines[j].trimmed();
        if t.len() >= fence_len
            && t.chars().all(|c| c == fence_char)
        {
            return (j, j + 1);
        }
    }
    (to, to)
}

/// Front matter closing delimiter, if line `i` opens YAML (`---`) or TOML (`+++`).
fn front_matter_close(lines: &[Line], i: usize, to: usize) -> Option<(usize, String)> {
    let t = lines[i].trimmed();
    let delim = if t == "---" {
        "---"
    } else if t == "+++" {
        "+++"
    } else {
        return None;
    };
    for j in (i + 1)..to {
        if lines[j].trimmed() == delim {
            return Some((j, delim.to_owned()));
        }
    }
    None
}

/// ATX heading: returns `(level, text, inline_span)`.
fn atx_heading(line: &Line) -> Option<(u8, String, Range<usize>)> {
    let t = line.content.trim_start();
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = t.get(hashes..)?;
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let ws = rest.len() - rest.trim_start().len();
    let body = rest.trim();
    // Strip an optional closing run of `#` (CommonMark only allows it after a space).
    let body = if body.ends_with('#') {
        let without = body.trim_end_matches('#');
        if without.is_empty() || without.ends_with([' ', '\t']) {
            without.trim_end()
        } else {
            body
        }
    } else {
        body
    };
    let lead = line.content.len() - t.len();
    let start = line.content_start + lead + hashes + ws;
    Some((
        hashes as u8,
        body.to_owned(),
        start..start + body.len(),
    ))
}

fn is_thematic_break(t: &str) -> bool {
    let t = t.trim();
    if t.is_empty() {
        return false;
    }
    for c in ['-', '*', '_'] {
        if t.chars().all(|x| x == c || x == ' ' || x == '\t')
            && t.chars().filter(|&x| x == c).count() >= 3
        {
            return true;
        }
    }
    false
}

fn is_setext_underline(t: &str, ch: char) -> bool {
    let t = t.trim();
    !t.is_empty() && t.chars().all(|c| c == ch)
}

fn is_table_start(lines: &[Line], i: usize, to: usize) -> bool {
    if i + 1 >= to {
        return false;
    }
    if !lines[i].content.contains('|') {
        return false;
    }
    parse_align_row(&lines[i + 1].content).is_some()
}

fn parse_align_row(s: &str) -> Option<Vec<Align>> {
    let t = s.trim();
    if t.is_empty() || !t.contains('-') {
        return None;
    }
    let t = t.trim_start_matches('|').trim_end_matches('|');
    let mut out = Vec::new();
    for cell in split_cells(t) {
        let c = cell.trim();
        if c.is_empty() || !c.chars().all(|ch| ch == '-' || ch == ':') || !c.contains('-') {
            return None;
        }
        out.push(match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => Align::Center,
            (true, false) => Align::Left,
            (false, true) => Align::Right,
            (false, false) => Align::None,
        });
    }
    (!out.is_empty()).then_some(out)
}

fn split_row(s: &str) -> Vec<String> {
    let t = s.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    split_cells(t)
}

/// Splits on unescaped `|`.
fn split_cells(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for ch in s.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out.into_iter().map(|c| c.trim().to_owned()).collect()
}

/// `::: name title` / `:::`.
///
/// Returns `(colon count, name, title, spaced)` where `spaced` records whether a
/// space separates the colons from the keyword. That single bit is what tells the
/// two container ecosystems apart for the keywords they share.
fn container_open(t: &str) -> Option<(usize, Option<String>, String, bool)> {
    if !t.starts_with(":::") {
        return None;
    }
    let colons = t.chars().take_while(|&c| c == ':').count();
    if colons < 3 {
        return None;
    }
    let after = t.get(colons..)?;
    let spaced = after.starts_with([' ', '\t']);
    let rest = after.trim();
    if rest.is_empty() {
        return Some((colons, None, String::new(), spaced));
    }
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let title = parts.next().unwrap_or("").trim();
    if name.is_empty() {
        return Some((colons, None, String::new(), spaced));
    }
    let title = title
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
        .to_owned();
    Some((colons, Some(name.to_owned()), title, spaced))
}

/// Picks the callout syntax for a `:::` container.
///
/// The decision is made per block, not per document, so one file can legitimately
/// mix the two styles — which is exactly what the "support all four" requirement
/// demands.
fn container_syntax(name: &str, spaced: bool) -> CalloutSyntax {
    match name.to_ascii_lowercase().as_str() {
        // Keywords unique to one ecosystem settle it outright.
        "note" | "caution" | "admonition" => CalloutSyntax::Docusaurus,
        "details" => CalloutSyntax::VitePress,
        // `tip` / `info` / `warning` / `danger` exist in both, but their
        // canonical spelling does not: VitePress writes `::: tip`, Docusaurus
        // writes `:::tip`.
        _ if spaced => CalloutSyntax::VitePress,
        _ => CalloutSyntax::Docusaurus,
    }
}

/// Parses a block-quote callout marker such as `[!NOTE]` or `[!tip]- Title`.
fn callout_marker(s: &str) -> Option<(CalloutVariant, Option<bool>, Option<String>)> {
    let t = s.trim();
    let rest = t.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let key = rest.get(..close)?;
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let mut after = rest.get(close + 1..)?.trim_start();
    let mut foldable = None;
    if let Some(stripped) = after.strip_prefix('+') {
        foldable = Some(true);
        after = stripped.trim_start();
    } else if let Some(stripped) = after.strip_prefix('-') {
        foldable = Some(false);
        after = stripped.trim_start();
    }
    let variant = CalloutVariant::from_key(CalloutSyntax::Obsidian, key);
    let title = if after.trim().is_empty() {
        None
    } else {
        Some(after.trim().to_owned())
    };
    Some((variant, foldable, title))
}

/// Decides whether a `> [!X]` marker is GitHub or Obsidian.
///
/// Both reserve the same syntax, so the discriminators are the features only
/// Obsidian has: a folding marker, a title on the marker line, or a keyword
/// outside GitHub's five.
fn classify_callout_syntax(
    line: &str,
    variant: &CalloutVariant,
    title: Option<&str>,
) -> CalloutSyntax {
    // A custom title or a fold marker only exists in Obsidian's grammar; GitHub's
    // alert has to be the bare keyword. Those signals win over everything else.
    let has_fold = line.contains("[!") && {
        let after = line.split(']').nth(1).unwrap_or("");
        after.trim_start().starts_with(['+', '-'])
    };
    if has_fold || title.is_some_and(|t| !t.trim().is_empty()) {
        return CalloutSyntax::Obsidian;
    }
    // Otherwise the keyword's case decides: GitHub's documentation always writes
    // `[!NOTE]` in caps, Obsidian's always writes `[!note]` in lower case. Judging
    // per block is what keeps a document that mixes both from painting a stray
    // GitHub-styled box in the middle of an Obsidian run.
    match keyword_case(line) {
        Some(KeywordCase::Upper) => CalloutSyntax::GitHub,
        Some(KeywordCase::Lower) => CalloutSyntax::Obsidian,
        // No ASCII letters (`[!注意]`) or mixed case: fall back to whether the
        // variant can even be expressed in GitHub's five keywords.
        None if variant.is_native_to(CalloutSyntax::GitHub) => CalloutSyntax::GitHub,
        None => CalloutSyntax::Obsidian,
    }
}

/// How the alert keyword was typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeywordCase {
    Upper,
    Lower,
}

/// The case the keyword between `[!` and `]` was written in.
///
/// `None` when it is mixed (`Note`), carries no ASCII letters (a Chinese custom
/// key), or the marker is malformed.
fn keyword_case(line: &str) -> Option<KeywordCase> {
    let start = line.find("[!")? + 2;
    let end = start + line[start..].find(']')?;
    let key = line[start..end].trim();
    let upper = key.chars().any(|c| c.is_ascii_uppercase());
    let lower = key.chars().any(|c| c.is_ascii_lowercase());
    match (upper, lower) {
        (true, false) => Some(KeywordCase::Upper),
        (false, true) => Some(KeywordCase::Lower),
        _ => None,
    }
}

fn classify_markdoc(inner: &str) -> (MarkdocTagKind, String, String) {
    let inner = inner.trim_end_matches('/').trim();
    let mut parts = inner.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let attrs = parts.next().unwrap_or("").trim().to_owned();
    // A reference keeps its sigil: `$x` is a value and `#partial` is an
    // include, and the runtime has to tell them apart.
    let kind = if head.starts_with('#') {
        return (MarkdocTagKind::Reference, head.to_owned(), attrs);
    } else if head.starts_with('$') {
        return (MarkdocTagKind::Reference, head.to_owned(), attrs);
    } else if let Some(name) = head.strip_prefix('/') {
        return (MarkdocTagKind::Pair, name.to_owned(), attrs);
    } else {
        match head {
            "if" | "else" | "elsif" | "unless" => MarkdocTagKind::Conditional,
            "comment" => MarkdocTagKind::Comment,
            // `{% for %} … {% /for %}` owns a body, so it is a container. The
            // runtime repeats it; the parser only has to group it.
            "for" => MarkdocTagKind::Pair,
            _ => MarkdocTagKind::Leaf,
        }
    };
    let name = head.trim_start_matches('/').to_owned();
    (kind, name, attrs)
}

/// Finds the `{% /name %}` line that closes a Markdoc container.
fn markdoc_close(lines: &[Line], from: usize, to: usize, name: &str) -> Option<usize> {
    for j in from..to {
        let t = lines[j].trimmed();
        if let Some(rest) = t.strip_prefix("{%")
            && let Some(end) = rest.find("%}")
        {
            let inner = rest.get(..end).unwrap_or("").trim();
            if let Some(close_name) = inner.strip_prefix('/')
                && close_name.trim() == name
            {
                return Some(j);
            }
        }
    }
    None
}

fn is_esm_start(t: &str) -> bool {
    t.starts_with("import ")
        || t.starts_with("import{")
        || t.starts_with("import(")
        || t.starts_with("export ")
        || t.starts_with("export{")
        || t == "export"
        || t.starts_with("export default")
}

/// Net bracket depth of `s`, ignoring string literals.
fn bracket_balance(s: &str) -> i32 {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in s.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => quote = Some(ch),
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// Heuristic: has this ESM statement finished?
fn statement_complete(s: &str) -> bool {
    let t = s.trim_end();
    t.ends_with(';')
        || t.ends_with('}')
        || t.ends_with(')')
        || t.ends_with('\'')
        || t.ends_with('"')
        || t.ends_with('`')
}

/// JSX opening tag name at the start of `t`, if any.
fn jsx_open(t: &str) -> Option<String> {
    let rest = t.strip_prefix('<')?;
    if rest.starts_with('>') {
        // Fragment `<>`.
        return None;
    }
    if rest.starts_with('/') {
        return None;
    }
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ':'))
        .collect();
    if name.is_empty() {
        return None;
    }
    let first = name.chars().next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    // Must be followed by whitespace, `>`, or `/`.
    let after = rest.get(name.len()..)?;
    if !(after.is_empty() || after.starts_with([' ', '\t', '>', '/', '\n'])) {
        return None;
    }
    Some(name)
}

/// HTML tag name at the start of `t`, if any.
fn html_open(t: &str) -> Option<String> {
    let rest = t.strip_prefix('<')?;
    if rest.starts_with('/') || rest.starts_with('!') || rest.starts_with('?') {
        return None;
    }
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    if name.is_empty() {
        return None;
    }
    let after = rest.get(name.len()..)?;
    if !(after.is_empty() || after.starts_with([' ', '\t', '>', '/', '\n'])) {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

fn is_block_html_tag(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "details"
            | "dialog"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "iframe"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "script"
            | "section"
            | "style"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
            | "video"
            | "audio"
            | "canvas"
            | "svg"
            | "center"
            | "marquee"
    )
}

/// Offset of the `>` that ends the opening tag starting at `from`.
fn find_tag_close(t: &str, from: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, ch) in t.char_indices().skip_while(|(i, _)| *i < from) {
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '>' => return Some(i),
            '<' => return None,
            _ => {}
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
enum TagEvent {
    Open,
    Close,
}

/// Finds the next `<tag` or `</tag>` after `from` in `content`.
fn find_tag_event(content: &str, from: usize, tag: &str) -> Option<(usize, TagEvent)> {
    let mut search = from;
    while let Some(rel) = content.get(search..)?.find('<') {
        let at = search + rel;
        let after = content.get(at..)?;
        if let Some(rest) = after.strip_prefix("</") {
            if rest.strip_prefix(tag).is_some_and(|r| {
                r.starts_with('>') || r.starts_with([' ', '\t', '\n'])
            }) {
                return Some((at, TagEvent::Close));
            }
        } else if let Some(rest) = after.strip_prefix('<')
            && rest.strip_prefix(tag).is_some_and(|r| {
                r.starts_with('>')
                    || r.starts_with('/')
                    || r.starts_with([' ', '\t', '\n'])
            })
        {
            return Some((at, TagEvent::Open));
        }
        search = at + 1;
    }
    None
}

/// A block-level math delimited by `$$` or `\[ ... \]`.
fn math_block(lines: &[Line], i: usize, to: usize) -> Option<(Option<usize>, usize, String)> {
    let t = lines[i].trimmed();
    let (open, close_tok) = if t == "$$" {
        ("$$", "$$")
    } else if t == "\\[" {
        ("\\[", "\\]")
    } else if t.starts_with("$$") && t.ends_with("$$") && t.len() > 4 {
        // Single-line display math.
        let tex = t.trim_start_matches('$').trim_end_matches('$').trim().to_owned();
        return Some((None, i + 1, tex));
    } else {
        return None;
    };
    let _ = open;
    for j in (i + 1)..to {
        let lt = lines[j].trimmed();
        if lt == close_tok || lt.ends_with(close_tok) && lt.len() <= close_tok.len() + 2 {
            let tex = (i + 1..j)
                .map(|k| lines[k].content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            return Some((Some(j), j + 1, tex));
        }
    }
    None
}

/// Parses a list item.
fn list_item(line: &Line) -> Option<Block> {
    let t = line.content.trim_start();
    let lead = line.content.len() - t.len();
    let (kind, index, marker_len) = if let Some(rest) = t.strip_prefix("- ") {
        (ListKind::Unordered, 0, t.len() - rest.len())
    } else if let Some(rest) = t.strip_prefix("* ") {
        (ListKind::Unordered, 0, t.len() - rest.len())
    } else if let Some(rest) = t.strip_prefix("+ ") {
        (ListKind::Unordered, 0, t.len() - rest.len())
    } else {
        // Ordered: `12.` or `12)`.
        let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        let rest = t.get(digits.len()..)?;
        let sep = rest.chars().next()?;
        if sep != '.' && sep != ')' {
            return None;
        }
        let after = rest.get(1..)?;
        if !after.starts_with(' ') && !after.is_empty() {
            return None;
        }
        let idx = digits.parse::<u64>().unwrap_or(1);
        (ListKind::Ordered, idx, digits.len() + 1)
    };

    // Consume the marker plus following spaces.
    let after_marker = t.get(marker_len..)?;
    let ws = after_marker.len() - after_marker.trim_start().len();
    let body = after_marker.trim_start();

    // Task list?
    let (checked, body) = if let Some(rest) = body.strip_prefix("[ ] ") {
        (Some(false), rest)
    } else if let Some(rest) = body.strip_prefix("[x] ").or_else(|| body.strip_prefix("[X] ")) {
        (Some(true), rest)
    } else {
        (None, body)
    };

    let content_start = line.content_start + lead + marker_len + ws
        + if checked.is_some() { 4 } else { 0 };
    let level = (line.indent() / 2).min(7) as u8;
    let text = body.trim_end().to_owned();
    let inline_span = content_start..content_start + text.len();
    let span = line.content_range();
    Some(Block::new(
        BlockKind::ListItem {
            kind,
            index,
            level,
            checked,
            text,
        },
        span,
        inline_span,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<&'static str> {
        parse_document(text, Flavor::Markdown)
            .iter()
            .map(Block::type_label)
            .collect()
    }

    fn parse(text: &str) -> Vec<Block> {
        parse_document(text, Flavor::Markdown)
    }

    /// Block spans must be ordered, non-overlapping and inside the source.
    fn assert_spans_sane(text: &str, blocks: &[Block]) {
        let mut last = 0usize;
        for b in blocks {
            assert!(b.span.start <= b.span.end, "inverted span: {:?}", b.span);
            assert!(b.span.end <= text.len(), "span past EOF: {:?}", b.span);
            assert!(b.span.start >= last, "overlapping spans at {:?}", b.span);
            last = b.span.end;
            if !b.inline_span.is_empty() {
                assert!(b.inline_span.end <= text.len());
                assert!(text.is_char_boundary(b.inline_span.start));
                assert!(text.is_char_boundary(b.inline_span.end));
            }
        }
    }

    #[test]
    fn headings_and_paragraphs() {
        let src = "# One\n\nsome text\n\n### Three ###\n";
        let blocks = parse(src);
        assert_spans_sane(src, &blocks);
        assert_eq!(blocks.len(), 3);
        match &blocks[0].kind {
            BlockKind::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text, "One");
            }
            other => panic!("expected heading, got {other:?}"),
        }
        match &blocks[2].kind {
            BlockKind::Heading { level, text } => {
                assert_eq!(*level, 3);
                assert_eq!(text, "Three");
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn heading_inline_span_points_at_the_text() {
        let src = "# Hello **world**\n";
        let blocks = parse(src);
        let b = &blocks[0];
        assert_eq!(&src[b.inline_span.clone()], "Hello **world**");
    }

    #[test]
    fn fenced_code_with_language() {
        let src = "```rust\nfn main() {}\n```\n\nafter\n";
        let blocks = parse(src);
        assert_spans_sane(src, &blocks);
        match &blocks[0].kind {
            BlockKind::Code { lang, code, .. } => {
                assert_eq!(lang, "rust");
                assert!(code.contains("fn main"));
            }
            other => panic!("expected code, got {other:?}"),
        }
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn unterminated_fence_still_terminates() {
        let src = "```rs\nlet x = 1;\n";
        let blocks = parse(src);
        assert_spans_sane(src, &blocks);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn thematic_break_and_quote() {
        let blocks = parse("---\n\n> quoted\n> lines\n");
        assert_eq!(kinds("---\n"), vec!["―"]);
        match &blocks[1].kind {
            BlockKind::Quote { text } => assert!(text.contains("quoted") && text.contains("lines")),
            other => panic!("expected quote, got {other:?}"),
        }
    }

    #[test]
    fn lists_with_tasks_and_ordering() {
        let src = "- one\n- [x] done\n- [ ] todo\n\n1. first\n2. second\n";
        let blocks = parse(src);
        assert_spans_sane(src, &blocks);
        let checks: Vec<Option<bool>> = blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::ListItem { checked, .. } => Some(*checked),
                _ => None,
            })
            .collect();
        assert_eq!(checks, vec![None, Some(true), Some(false), None, None]);
        match &blocks[3].kind {
            BlockKind::ListItem { kind, index, .. } => {
                assert_eq!(*kind, ListKind::Ordered);
                assert_eq!(*index, 1);
            }
            other => panic!("expected ordered item, got {other:?}"),
        }
    }

    #[test]
    fn table_with_alignment() {
        let src = "| a | b |\n|:--|--:|\n| 1 | 2 |\n| 3 | 4 |\n";
        let blocks = parse(src);
        assert_spans_sane(src, &blocks);
        match &blocks[0].kind {
            BlockKind::Table { align, header, rows } => {
                assert_eq!(header, &["a".to_owned(), "b".to_owned()]);
                assert_eq!(align, &[Align::Left, Align::Right]);
                assert_eq!(rows.len(), 2);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn github_callout() {
        let src = "> [!NOTE]\n> Body text here.\n";
        let blocks = parse(src);
        match &blocks[0].kind {
            BlockKind::Callout(c) => {
                assert_eq!(c.syntax, CalloutSyntax::GitHub);
                assert_eq!(c.variant, CalloutVariant::Note);
                assert_eq!(c.body.len(), 1);
            }
            other => panic!("expected callout, got {other:?}"),
        }
    }

    #[test]
    fn obsidian_callout_with_title_and_fold() {
        let src = "> [!tip]- Folded title\n> content\n";
        let blocks = parse(src);
        match &blocks[0].kind {
            BlockKind::Callout(c) => {
                assert_eq!(c.syntax, CalloutSyntax::Obsidian);
                assert_eq!(c.variant, CalloutVariant::Tip);
                assert_eq!(c.foldable, Some(false));
                assert_eq!(c.title.as_deref(), Some("Folded title"));
            }
            other => panic!("expected callout, got {other:?}"),
        }
    }

    #[test]
    fn obsidian_only_keyword_is_detected() {
        let blocks = parse("> [!bug]\n> it broke\n");
        match &blocks[0].kind {
            BlockKind::Callout(c) => assert_eq!(c.syntax, CalloutSyntax::Obsidian),
            other => panic!("expected callout, got {other:?}"),
        }
    }

    #[test]
    fn plain_quote_is_not_a_callout() {
        let blocks = parse("> just a quote\n");
        assert!(matches!(blocks[0].kind, BlockKind::Quote { .. }));
    }

    #[test]
    fn vitepress_container() {
        let src = "::: tip 提示\n正文\n:::\n";
        let blocks = parse(src);
        assert_spans_sane(src, &blocks);
        match &blocks[0].kind {
            BlockKind::Callout(c) => {
                assert_eq!(c.syntax, CalloutSyntax::VitePress);
                assert_eq!(c.variant, CalloutVariant::Tip);
                assert_eq!(c.title.as_deref(), Some("提示"));
                assert_eq!(c.body.len(), 1);
            }
            other => panic!("expected callout, got {other:?}"),
        }
    }

    #[test]
    fn docusaurus_container_detected_by_keyword() {
        let src = ":::note Title\nbody\n:::\n";
        let blocks = parse(src);
        match &blocks[0].kind {
            BlockKind::Callout(c) => {
                assert_eq!(c.syntax, CalloutSyntax::Docusaurus);
                assert_eq!(c.variant, CalloutVariant::Note);
            }
            other => panic!("expected callout, got {other:?}"),
        }
    }

    #[test]
    fn docusaurus_wins_when_document_mixes_styles() {
        // `:::caution` is Docusaurus-only, so the ambiguous `:::tip` follows it.
        let src = ":::caution\nA\n:::\n\n:::tip\nB\n:::\n";
        let blocks = parse(src);
        let syntaxes: Vec<CalloutSyntax> = blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::Callout(c) => Some(c.syntax),
                _ => None,
            })
            .collect();
        assert_eq!(
            syntaxes,
            vec![CalloutSyntax::Docusaurus, CalloutSyntax::Docusaurus]
        );
    }

    #[test]
    fn container_body_contains_rich_blocks() {
        let src = "::: warning\n## Head\n\n- a\n- b\n\n```js\nlet x = 1;\n```\n:::\n";
        let blocks = parse(src);
        match &blocks[0].kind {
            BlockKind::Callout(c) => {
                assert_eq!(c.body.len(), 4, "{:?}", c.body);
                assert!(matches!(c.body[0].kind, BlockKind::Heading { .. }));
                assert!(matches!(c.body[3].kind, BlockKind::Code { .. }));
            }
            other => panic!("expected callout, got {other:?}"),
        }
    }

    #[test]
    fn display_math_forms() {
        for src in ["$$\n\\int_0^1 x\\,dx\n$$\n", "\\[\nE = mc^2\n\\]\n"] {
            let blocks = parse(src);
            assert_eq!(blocks.len(), 1, "{src}");
            match &blocks[0].kind {
                BlockKind::Math { tex } => assert!(!tex.trim().is_empty()),
                other => panic!("expected math, got {other:?}"),
            }
        }
        let single = parse("$$a+b$$\n");
        assert!(matches!(single[0].kind, BlockKind::Math { .. }));
    }

    #[test]
    fn mdx_esm_is_captured() {
        let src = "import Tabs from '@theme/Tabs';\nimport TabItem from '@theme/TabItem';\n\nText.\n";
        let blocks = parse(src);
        assert_spans_sane(src, &blocks);
        match &blocks[0].kind {
            BlockKind::MdxEsm { raw } => assert!(raw.contains("Tabs")),
            other => panic!("expected esm, got {other:?}"),
        }
    }

    #[test]
    fn multi_line_export_is_captured() {
        let src = "export const meta = {\n  title: 'x',\n};\n\nBody.\n";
        let blocks = parse(src);
        match &blocks[0].kind {
            BlockKind::MdxEsm { raw } => assert!(raw.contains("title")),
            other => panic!("expected esm, got {other:?}"),
        }
    }

    #[test]
    fn jsx_block_with_children() {
        let src = "<Callout type=\"warn\">\n\nBody **bold**\n\n</Callout>\n";
        let blocks = parse(src);
        assert_spans_sane(src, &blocks);
        match &blocks[0].kind {
            BlockKind::MdxJsx {
                name,
                props,
                self_closing,
                children,
                ..
            } => {
                assert_eq!(name, "Callout");
                assert!(props.contains("type"));
                assert!(!self_closing);
                assert_eq!(children.len(), 1);
                assert!(matches!(children[0].kind, BlockKind::Paragraph { .. }));
            }
            other => panic!("expected jsx, got {other:?}"),
        }
    }

    #[test]
    fn self_closing_jsx() {
        let blocks = parse("<Tabs items={['a','b']} />\n");
        match &blocks[0].kind {
            BlockKind::MdxJsx { self_closing, props, .. } => {
                assert!(self_closing);
                assert!(props.contains("items"));
            }
            other => panic!("expected jsx, got {other:?}"),
        }
    }

    #[test]
    fn markdoc_leaf_and_pair() {
        let leaf = parse("{% callout type=\"note\" /%}\n");
        match &leaf[0].kind {
            BlockKind::MarkdocTag { kind, name, attrs, .. } => {
                assert_eq!(*kind, MarkdocTagKind::Leaf);
                assert_eq!(name, "callout");
                assert!(attrs.contains("type"));
            }
            other => panic!("expected markdoc, got {other:?}"),
        }

        let pair = parse("{% if $a %}\nBody\n{% /if %}\n");
        match &pair[0].kind {
            BlockKind::MarkdocTag { kind, name, body, .. } => {
                assert_eq!(*kind, MarkdocTagKind::Conditional);
                assert_eq!(name, "if");
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected markdoc, got {other:?}"),
        }
    }

    #[test]
    fn markdoc_reference() {
        let blocks = parse("{% #partial /%}\n");
        match &blocks[0].kind {
            BlockKind::MarkdocTag { kind, name, .. } => {
                assert_eq!(*kind, MarkdocTagKind::Reference);
                // The sigil is kept: `#partial` is an include, `$x` is a value.
                assert_eq!(name, "#partial");
            }
            other => panic!("expected markdoc, got {other:?}"),
        }
    }

    #[test]
    fn front_matter_only_at_the_top() {
        let with = parse("---\ntitle: x\n---\n\n# H\n");
        assert!(matches!(with[0].kind, BlockKind::FrontMatter { .. }));
        // A later `---` is a thematic break, not front matter.
        let later = parse("# H\n\n---\n");
        assert!(matches!(later[1].kind, BlockKind::Rule));
    }

    #[test]
    fn html_block() {
        let blocks = parse("<div class=\"x\">\n  <span>hi</span>\n</div>\n");
        assert!(matches!(blocks[0].kind, BlockKind::Html { .. }));
    }

    #[test]
    fn unsupported_constructs_fall_back_to_paragraphs() {
        for src in ["plain", "a * b", "1)", "> >", ":::"] {
            let blocks = parse(src);
            assert_eq!(blocks.len(), 1, "{src} -> {blocks:?}");
        }
    }

    #[test]
    fn parser_never_panics_on_odd_input() {
        let samples: [&str; 24] = [
            "",
            "\n",
            "\r\n\r\n",
            "#",
            "######",
            "# ",
            "```",
            "~~~~",
            "|",
            "|---|",
            ":::",
            "::::",
            "::: tip",
            "{% ",
            "{%",
            "import",
            "export",
            "<A",
            "<A>",
            "</A>",
            "<<>>",
            "> [!",
            "$$",
            "\\[",
        ];
        for src in samples {
            let blocks = parse_document(src, Flavor::Markdown);
            assert_spans_sane(src, &blocks);
        }
    }

    #[test]
    fn deeply_nested_quotes_terminate() {
        let src = (0..40).map(|i| format!("{}deep {i}\n", "> ".repeat(20))).collect::<String>();
        let blocks = parse_document(&src, Flavor::Markdown);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn utf8_spans_stay_on_char_boundaries() {
        let src = "**粗体** 与 `代码`\n\n> [!note] 标题\n> 内容\n\n| 表 | 头 |\n|---|---|\n| 值 | 1 |\n";
        let blocks = parse_document(src, Flavor::Markdown);
        assert_spans_sane(src, &blocks);
    }

    #[test]
    fn keyword_case_decides_the_syntax_per_block() {
        // The document mixes both dialects, exactly as it does when someone keeps
        // an Obsidian section under a GitHub section. A single per-document guess
        // painted `[!tip]` as a GitHub alert — green left rule and a light bulb in
        // the middle of the Obsidian run. GitHub's docs write capitals, Obsidian's
        // write lower case, so the keyword's own case is the reliable signal.
        let src = "> [!NOTE]\n> a\n\n> [!tip]\n> b\n\n> [!DANGER]\n> c\n\n> [!danger]\n> d\n\n> [!note] 自定义标题\n> e\n";
        let blocks = parse_document(src, Flavor::Markdown);
        let syntaxes: Vec<CalloutSyntax> = blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::Callout(c) => Some(c.syntax),
                _ => None,
            })
            .collect();
        assert_eq!(
            syntaxes,
            vec![
                CalloutSyntax::GitHub,   // [!NOTE]
                CalloutSyntax::Obsidian, // [!tip]
                CalloutSyntax::GitHub,   // [!DANGER] — caps, even though GitHub has no such keyword
                CalloutSyntax::Obsidian, // [!danger]
                CalloutSyntax::Obsidian, // [!note] 自定义标题 — a title is Obsidian-only
            ]
        );
    }

    #[test]
    fn a_mixed_case_keyword_falls_back_to_the_variant() {
        // `[!Note]` matches neither convention; the variant then decides whether
        // the keyword is even expressible as a GitHub alert.
        let blocks = parse_document("> [!Note]\n> a\n", Flavor::Markdown);
        match &blocks[0].kind {
            BlockKind::Callout(c) => assert_eq!(c.syntax, CalloutSyntax::GitHub),
            other => panic!("expected a callout, got {}", crate::doc::model::kind_label(other)),
        }
        let blocks = parse_document("> [!Bug]\n> a\n", Flavor::Markdown);
        match &blocks[0].kind {
            // `bug` is not one of GitHub's five, so mixed case lands on Obsidian.
            BlockKind::Callout(c) => assert_eq!(c.syntax, CalloutSyntax::Obsidian),
            other => panic!("expected a callout, got {}", crate::doc::model::kind_label(other)),
        }
    }
}
