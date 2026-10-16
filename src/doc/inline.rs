//! Inline Markdown scanner.
//!
//! Produces a *contiguous, byte-complete* description of a line of inline
//! Markdown: every byte of the input belongs to exactly one [`Span`], and every
//! syntactic construct additionally records where its delimiters live in a
//! [`Construct`].
//!
//! That split is what makes live rendering possible. The renderer walks the
//! spans and styles each one, but it *hides* the delimiter spans unless the caret
//! is currently inside that construct — so `**bold**` reads as **bold** until you
//! click into it, at which point the asterisks reappear for editing. Because the
//! span list never drops bytes, the widget's text stays character-for-character
//! identical to the source, which is a hard requirement of egui's text cursor.
//!
//! This is a deliberate CommonMark *subset*: nested emphasis, code spans, links,
//! images, math, MDX expressions and JSX are understood; reference links,
//! setext-style edge cases and HTML block escapes inside a paragraph are not.
//! Unrecognised input is simply reported as text, never dropped.

use std::ops::Range;

/// Which construct a span belongs to, or what style it carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InlineKind {
    #[default]
    Text,
    Strong,
    Em,
    Code,
    Strike,
    Highlight,
    Link,
    Image,
    Math,
    MdxExpr,
    MdxJsx,
    /// Backslash escape (`\*`).
    Escape,
    /// Raw inline HTML tag.
    Html,
}

/// Render-affecting flags accumulated by nesting.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Style {
    pub strong: bool,
    pub em: bool,
    pub code: bool,
    pub strike: bool,
    pub highlight: bool,
    pub link: bool,
    pub math: bool,
    /// MDX `{expr}` or JSX — rendered in monospace with a tinted background.
    pub mdx: bool,
    /// Inside a construct that is currently being edited (delimiters revealed).
    pub revealed: bool,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// A byte range of the scanned text with a resolved style.
#[derive(Clone, Debug)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
    /// Index into [`Inline::constructs`] when this span is inside a construct.
    pub construct: Option<usize>,
}

impl Span {
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        source.get(self.range.clone()).unwrap_or("")
    }
}

/// A syntactic construct and the location of its delimiters.
#[derive(Clone, Debug)]
pub struct Construct {
    pub kind: InlineKind,
    /// Opening delimiter, e.g. the `**` of `**bold**`.
    pub open: Range<usize>,
    /// Closing delimiter; `None` for escapes and unterminated constructs.
    pub close: Option<Range<usize>>,
    /// The content between the delimiters.
    pub inner: Range<usize>,
    /// Destination for links and images.
    pub href: Option<String>,
    /// Nesting depth, 0 for the outermost construct.
    pub depth: u8,
}

impl Construct {
    /// `true` when `span` is one of this construct's delimiter spans.
    pub fn is_delimiter(&self, range: &Range<usize>) -> bool {
        *range == self.open || self.close.as_ref() == Some(range)
    }

    /// `true` when the caret at byte `at` sits inside the construct, meaning its
    /// delimiters should be revealed for editing.
    pub fn contains(&self, at: usize) -> bool {
        let end = self.close.as_ref().map(|c| c.end).unwrap_or(self.inner.end);
        at >= self.open.start && at <= end
    }

    /// `true` when any byte of `range` touches the construct.
    pub fn intersects(&self, range: &Range<usize>) -> bool {
        let end = self.close.as_ref().map(|c| c.end).unwrap_or(self.inner.end);
        range.start < end && self.open.start < range.end.max(range.start + 1)
    }
}

/// The result of scanning one run of inline text.
#[derive(Clone, Debug, Default)]
pub struct Inline {
    pub spans: Vec<Span>,
    pub constructs: Vec<Construct>,
    /// Non-fatal problems, surfaced in the UI for MDX/Markdoc diagnostics.
    pub warnings: Vec<String>,
}

impl Inline {
    /// Scans `text`. Offsets in the result are relative to `text`.
    pub fn scan(text: &str) -> Self {
        let mut out = Self::default();
        let mut style = Style::default();
        scan_into(text, 0, &mut style, 0, &mut out);
        out.push_warnings();
        out
    }

    /// Scans `text` whose first byte is at `base` in the document, so spans and
    /// constructs are expressed in document coordinates.
    pub fn scan_at(text: &str, base: usize) -> Self {
        let mut out = Self::default();
        let mut style = Style::default();
        scan_into(text, base, &mut style, 0, &mut out);
        out.push_warnings();
        out
    }

    /// The concatenated content of the text, with all delimiters removed.
    pub fn plain_text(&self, source: &str) -> String {
        let mut s = String::with_capacity(source.len());
        for span in &self.spans {
            let is_delim = span
                .construct
                .and_then(|id| self.constructs.get(id))
                .is_some_and(|c| c.is_delimiter(&span.range));
            if is_delim {
                continue;
            }
            s.push_str(span.text(source));
        }
        s
    }

    /// Construct ids whose delimiters should be revealed for a caret/selection
    /// covering `[lo, hi)`.
    pub fn revealed(&self, lo: usize, hi: usize) -> Vec<usize> {
        let selection = lo..hi.max(lo + 1);
        self.constructs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.intersects(&selection))
            .map(|(i, _)| i)
            .collect()
    }

    /// The innermost construct containing `at`, if any.
    pub fn construct_at(&self, at: usize) -> Option<&Construct> {
        self.constructs
            .iter()
            .filter(|c| c.contains(at))
            .max_by_key(|c| c.depth)
    }

    /// The link destination at `at`, used for ctrl-click to open.
    pub fn link_at(&self, at: usize) -> Option<&str> {
        self.constructs
            .iter()
            .find(|c| c.kind == InlineKind::Link && c.contains(at))
            .and_then(|c| c.href.as_deref())
    }

    /// Appends a warning for every unterminated construct.
    fn push_warnings(&mut self) {
        for c in &self.constructs {
            if c.close.is_none() && c.kind != InlineKind::Escape {
                let mut buf = [0u8; 4];
                let _ = &mut buf;
                self.warnings
                    .push(format!("未闭合的 {:?} 结构", c.kind));
            }
        }
        self.warnings.dedup();
    }

    /// Total number of spans that are delimiters.
    pub fn delimiter_count(&self) -> usize {
        self.spans
            .iter()
            .filter(|s| {
                s.construct
                    .and_then(|id| self.constructs.get(id))
                    .is_some_and(|c| c.is_delimiter(&s.range))
            })
            .count()
    }
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

const MAX_DEPTH: u8 = 16;

/// Pushes a span, merging with the previous one when the style is identical and
/// the ranges are adjacent. Merging keeps the span list small, which matters
/// because the renderer builds one `LayoutJob` section per span.
///
/// Only *plain* spans are merged. A span that belongs to a construct must stay
/// separate from its neighbours even when the style matches, otherwise a
/// delimiter would be absorbed into the surrounding content and could no longer
/// be recognised — and therefore never hidden.
fn push_span(out: &mut Inline, start: usize, end: usize, style: Style, construct: Option<usize>) {
    if end <= start {
        return;
    }
    if let Some(last) = out.spans.last_mut()
        && last.style == style
        && last.construct.is_none()
        && construct.is_none()
        && last.range.end == start
    {
        last.range.end = end;
        return;
    }
    out.spans.push(Span {
        range: start..end,
        style,
        construct,
    });
}

fn scan_into(text: &str, base: usize, style: &mut Style, depth: u8, out: &mut Inline) {
    if depth > MAX_DEPTH {
        let end = base + text.len();
        push_span(out, base, end, *style, None);
        return;
    }

    let bytes = text.as_bytes();
    let mut i = 0usize;
    // Start of the current run of plain text.
    let mut plain_start = 0usize;

    while i < text.len() {
        let b = bytes[i];

        // --- backslash escape -------------------------------------------------
        if b == b'\\' && i + 1 < text.len() {
            let next_len = char_len_at(text, i + 1);
            if let Some(nc) = text[i + 1..].chars().next()
                && is_escapable(nc)
            {
                let open = base + i..base + i + 1;
                let inner = base + i + 1..base + i + 1 + next_len;
                let id = out.constructs.len();
                out.constructs.push(Construct {
                    kind: InlineKind::Escape,
                    open: open.clone(),
                    close: None,
                    inner: inner.clone(),
                    href: None,
                    depth,
                });
                push_span(out, base + plain_start, base + i, *style, None);
                let mut inner_style = *style;
                inner_style.revealed |= style.revealed;
                // The backslash itself is the construct's opening delimiter and
                // must still occupy a span, otherwise the span list would leave
                // a hole and the widget text would stop matching the source.
                push_span(out, open.start, open.end, *style, Some(id));
                push_span(out, inner.start, inner.end, inner_style, Some(id));
                i += 1 + next_len;
                plain_start = i;
                continue;
            }
        }

        // --- code span --------------------------------------------------------
        if b == b'`' {
            let run = count_run(bytes, i, b'`');
            if let Some(close_start) = find_backtick_close(text, i + run, run) {
                let open = base + i..base + i + run;
                let close = base + close_start..base + close_start + run;
                let inner = base + i + run..base + close_start;
                let id = out.constructs.len();
                out.constructs.push(Construct {
                    kind: InlineKind::Code,
                    open: open.clone(),
                    close: Some(close.clone()),
                    inner: inner.clone(),
                    href: None,
                    depth,
                });
                push_span(out, base + plain_start, base + i, *style, None);
                let mut s = *style;
                s.code = true;
                // Code spans are verbatim: no further parsing inside.
                push_span(out, open.start, open.end, *style, Some(id));
                push_span(out, inner.start, inner.end, s, Some(id));
                push_span(out, close.start, close.end, *style, Some(id));
                i = close_start + run;
                plain_start = i;
                continue;
            }
        }

        // --- emphasis / strike / highlight -----------------------------------
        if matches!(b, b'*' | b'_' | b'~' | b'=') {
            if let Some((open_len, kind)) = emphasis_open(text, i) {
                if let Some(close_start) = find_emphasis_close(text, i + open_len, open_len, b) {
                    let open = base + i..base + i + open_len;
                    let close = base + close_start..base + close_start + open_len;
                    let inner_start = i + open_len;
                    let inner_text = &text[inner_start..close_start];
                    let id = out.constructs.len();
                    out.constructs.push(Construct {
                        kind,
                        open: open.clone(),
                        close: Some(close.clone()),
                        inner: base + inner_start..base + close_start,
                        href: None,
                        depth,
                    });
                    push_span(out, base + plain_start, base + i, *style, None);
                    push_span(out, open.start, open.end, *style, Some(id));

                    let mut inner_style = *style;
                    apply_kind(&mut inner_style, kind);
                    // The inner text inherits the delimiter visibility of the whole
                    // construct, resolved later by the renderer via `revealed`.
                    scan_into(inner_text, base + inner_start, &mut inner_style, depth + 1, out);

                    push_span(out, close.start, close.end, *style, Some(id));
                    i = close_start + open_len;
                    plain_start = i;
                    continue;
                }
            }
        }

        // --- link / image -----------------------------------------------------
        if b == b'[' || (b == b'!' && bytes.get(i + 1) == Some(&b'[')) {
            let is_image = b == b'!';
            let bracket = if is_image { i + 1 } else { i };
            if let Some(parsed) = parse_link(text, bracket) {
                let kind = if is_image {
                    InlineKind::Image
                } else {
                    InlineKind::Link
                };
                let id = out.constructs.len();
                out.constructs.push(Construct {
                    kind,
                    open: base + i..base + bracket + 1,
                    // The closing delimiter is the whole `](dest "title")` tail,
                    // so it can be hidden and revealed as one unit.
                    close: Some(base + parsed.text_end..base + parsed.close_end),
                    inner: base + bracket + 1..base + parsed.text_end,
                    href: Some(parsed.href.clone()),
                    depth,
                });
                push_span(out, base + plain_start, base + i, *style, None);
                push_span(out, base + i, base + bracket + 1, *style, Some(id));

                let inner_text = &text[bracket + 1..parsed.text_end];
                let mut inner_style = *style;
                inner_style.link = !is_image;
                scan_into(
                    inner_text,
                    base + bracket + 1,
                    &mut inner_style,
                    depth + 1,
                    out,
                );

                push_span(
                    out,
                    base + parsed.text_end,
                    base + parsed.close_end,
                    *style,
                    Some(id),
                );
                i = parsed.close_end;
                plain_start = i;
                continue;
            }
        }

        // --- math -------------------------------------------------------------
        if b == b'$' {
            let run = count_run(bytes, i, b'$');
            let run = run.min(2);
            if let Some(close_start) = find_math_close(text, i + run, run) {
                let open = base + i..base + i + run;
                let close = base + close_start..base + close_start + run;
                let inner = base + i + run..base + close_start;
                let id = out.constructs.len();
                out.constructs.push(Construct {
                    kind: InlineKind::Math,
                    open: open.clone(),
                    close: Some(close.clone()),
                    inner: inner.clone(),
                    href: None,
                    depth,
                });
                push_span(out, base + plain_start, base + i, *style, None);
                let mut s = *style;
                s.math = true;
                push_span(out, open.start, open.end, *style, Some(id));
                push_span(out, inner.start, inner.end, s, Some(id));
                push_span(out, close.start, close.end, *style, Some(id));
                i = close_start + run;
                plain_start = i;
                continue;
            }
        }

        // --- MDX expression ---------------------------------------------------
        if b == b'{' {
            if let Some(close) = find_balanced(text, i, b'{', b'}') {
                let id = out.constructs.len();
                out.constructs.push(Construct {
                    kind: InlineKind::MdxExpr,
                    open: base + i..base + i + 1,
                    close: Some(base + close..base + close + 1),
                    inner: base + i + 1..base + close,
                    href: None,
                    depth,
                });
                push_span(out, base + plain_start, base + i, *style, None);
                let mut s = *style;
                s.mdx = true;
                push_span(out, base + i, base + i + 1, *style, Some(id));
                push_span(out, base + i + 1, base + close, s, Some(id));
                push_span(out, base + close, base + close + 1, *style, Some(id));
                i = close + 1;
                plain_start = i;
                continue;
            }
        }

        // --- JSX / HTML tag / autolink ----------------------------------------
        if b == b'<' {
            if let Some(end) = find_tag_end(text, i) {
                let tag = &text[i..end];
                let id = out.constructs.len();
                let (kind, href, style_update) = classify_angle(tag);
                out.constructs.push(Construct {
                    kind,
                    open: base + i..base + i + 1,
                    close: Some(base + end - 1..base + end),
                    inner: base + i + 1..base + end - 1,
                    href,
                    depth,
                });
                push_span(out, base + plain_start, base + i, *style, None);
                let mut s = *style;
                style_update(&mut s);
                push_span(out, base + i, base + i + 1, *style, Some(id));
                push_span(out, base + i + 1, base + end - 1, s, Some(id));
                push_span(out, base + end - 1, base + end, *style, Some(id));
                i = end;
                plain_start = i;
                continue;
            }
        }

        // Plain character.
        i += char_len_at(text, i);
    }

    push_span(out, base + plain_start, base + text.len(), *style, None);
}

fn apply_kind(style: &mut Style, kind: InlineKind) {
    match kind {
        InlineKind::Strong => style.strong = true,
        InlineKind::Em => style.em = true,
        InlineKind::Strike => style.strike = true,
        InlineKind::Highlight => style.highlight = true,
        _ => {}
    }
}

/// Byte length of the UTF-8 character starting at `i` (at least 1).
fn char_len_at(text: &str, i: usize) -> usize {
    text.get(i..)
        .and_then(|s| s.chars().next())
        .map(char::len_utf8)
        .unwrap_or(1)
}

/// Number of consecutive `byte` characters starting at `i`.
fn count_run(bytes: &[u8], i: usize, byte: u8) -> usize {
    let mut n = 0;
    while i + n < bytes.len() && bytes[i + n] == byte {
        n += 1;
    }
    n
}

/// Characters that may be escaped with a backslash.
fn is_escapable(c: char) -> bool {
    matches!(
        c,
        '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '~' | '=' | '>' | '<' | '$' | '%' | '&' | ':' | '"' | '\'' | '^'
            | '/' | '@' | ';' | ','
    )
}

/// Classifies an emphasis opener at `i`.
///
/// Returns the delimiter length and the construct kind. CommonMark's
/// flanking rules are approximated: `_` must not appear inside a word, and an
/// opener may not be followed by whitespace.
fn emphasis_open(text: &str, i: usize) -> Option<(usize, InlineKind)> {
    let bytes = text.as_bytes();
    let ch = bytes[i];
    let run = count_run(bytes, i, ch);
    if ch == b'=' {
        // `==highlight==` needs an even pair; `=` is otherwise literal.
        return (run >= 2).then_some((2, InlineKind::Highlight));
    }
    if ch == b'~' {
        return (run >= 2).then_some((2, InlineKind::Strike));
    }
    if run == 0 {
        return None;
    }
    let next = text.get(i + run..)?.chars().next()?;
    if next.is_whitespace() {
        return None;
    }
    if ch == b'_' {
        // Intraword underscores are literal.
        if i > 0 {
            if let Some(prev) = text.get(..i).and_then(|s| s.chars().next_back())
                && (prev.is_alphanumeric() || prev == '_')
            {
                return None;
            }
        }
    }
    let len = run.min(3);
    let kind = match len {
        1 => InlineKind::Em,
        2 => InlineKind::Strong,
        _ => InlineKind::Strong,
    };
    Some((len, kind))
}

/// Finds the matching closer for an emphasis run opened at `from`.
fn find_emphasis_close(text: &str, from: usize, len: usize, ch: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == ch {
            let run = count_run(bytes, i, ch);
            if run >= len {
                // The character before the closer must not be whitespace.
                let prev_ok = text
                    .get(..i)
                    .and_then(|s| s.chars().next_back())
                    .is_some_and(|c| !c.is_whitespace());
                if prev_ok {
                    return Some(i);
                }
            }
            i += run;
            continue;
        }
        if bytes[i] == b'\\' {
            i += 1 + char_len_at(text, i + 1);
            continue;
        }
        i += char_len_at(text, i);
    }
    None
}

/// Finds a closing backtick run of exactly `len`, starting the search at `from`.
fn find_backtick_close(text: &str, from: usize, len: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let run = count_run(bytes, i, b'`');
            if run == len {
                return Some(i);
            }
            i += run;
            continue;
        }
        i += char_len_at(text, i);
    }
    None
}

/// Finds a valid `$` closer for a math span opened with `len` dollars.
///
/// Guards against currency-like prose: the closer may not be preceded by
/// whitespace and may not be followed by a digit.
fn find_math_close(text: &str, from: usize, len: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i + len <= bytes.len() {
        if bytes[i] == b'$' && count_run(bytes, i, b'$') >= len {
            let prev_ok = text
                .get(..i)
                .and_then(|s| s.chars().next_back())
                .is_some_and(|c| !c.is_whitespace());
            let next = text.get(i + len..).and_then(|s| s.chars().next());
            let next_ok = !next.is_some_and(|c| c.is_ascii_digit());
            if prev_ok && next_ok {
                return Some(i);
            }
            i += len;
            continue;
        }
        i += char_len_at(text, i);
    }
    None
}

/// Balanced-delimiter search used for MDX `{ ... }` expressions.
fn find_balanced(text: &str, start: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = start;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_string {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_string = None;
            }
            i += char_len_at(text, i);
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => in_string = Some(b),
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += char_len_at(text, i);
    }
    None
}

/// Locates the end of an inline `<...>` tag or autolink.
fn find_tag_end(text: &str, start: usize) -> Option<usize> {
    let after = text.get(start + 1..)?;
    let mut in_string: Option<char> = None;
    for (offset, ch) in after.char_indices() {
        if let Some(q) = in_string {
            if ch == q {
                in_string = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => in_string = Some(ch),
            '>' => {
                let end = start + 1 + offset + 1;
                return (end - start > 2).then_some(end);
            }
            '<' | '\n' => return None,
            _ => {}
        }
    }
    None
}

/// Decides whether `<...>` is a JSX tag, an HTML tag or an autolink.
#[allow(clippy::type_complexity)]
fn classify_angle(
    tag: &str,
) -> (
    InlineKind,
    Option<String>,
    fn(&mut Style),
) {
    let inner = tag.trim_start_matches('<').trim_end_matches('>').trim();
    let is_url = inner.starts_with("http://")
        || inner.starts_with("https://")
        || inner.starts_with("mailto:")
        || (inner.contains('@') && !inner.contains(' '));
    if is_url {
        return (InlineKind::Link, Some(inner.to_owned()), |s: &mut Style| {
            s.link = true;
        });
    }
    let first = inner.chars().next().unwrap_or(' ');
    let is_component = first.is_ascii_uppercase();
    if is_component || inner.starts_with("Fragment") {
        return (
            InlineKind::MdxJsx,
            None,
            |s: &mut Style| {
                s.mdx = true;
            },
        );
    }
    (
        InlineKind::Html,
        None,
        |s: &mut Style| {
            s.mdx = true;
        },
    )
}

struct ParsedLink {
    text_end: usize,
    close_start: usize,
    close_end: usize,
    href: String,
}

/// Parses `[text](href "title")` starting at the `[` index.
fn parse_link(text: &str, bracket: usize) -> Option<ParsedLink> {
    let bytes = text.as_bytes();
    if bytes.get(bracket) != Some(&b'[') {
        return None;
    }
    // Find the matching `]`, allowing one level of nesting.
    let mut depth = 0usize;
    let mut i = bracket;
    let mut text_end = None;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 1 + char_len_at(text, i + 1);
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    text_end = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += char_len_at(text, i);
    }
    let text_end = text_end?;
    if bytes.get(text_end + 1) != Some(&b'(') {
        return None;
    }
    let mut depth = 0usize;
    let mut j = text_end + 1;
    let mut close = None;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => {
                j += 1 + char_len_at(text, j + 1);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            b'\n' => return None,
            _ => {}
        }
        j += char_len_at(text, j);
    }
    let close_start = close?;
    let dest = text.get(text_end + 2..close_start).unwrap_or("").trim();
    // Strip an optional title: `url "title"` or `url 'title'`.
    let href = match dest.find(['"', '\'']) {
        Some(idx) => dest[..idx].trim().to_owned(),
        None => dest.to_owned(),
    };
    Some(ParsedLink {
        text_end,
        close_start,
        close_end: close_start + 1,
        href,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every byte must belong to exactly one span, in order.
    fn assert_contiguous(inline: &Inline, text: &str) {
        let mut at = 0usize;
        for span in &inline.spans {
            assert_eq!(span.range.start, at, "gap or overlap at {at} in {text:?}");
            assert!(span.range.end > span.range.start, "empty span");
            at = span.range.end;
        }
        assert_eq!(at, text.len(), "spans do not cover all of {text:?}");
    }

    #[test]
    fn plain_text_is_one_span() {
        let i = Inline::scan("just words");
        assert_eq!(i.spans.len(), 1);
        assert!(i.constructs.is_empty());
        assert_contiguous(&i, "just words");
    }

    #[test]
    fn strong_hides_delimiters() {
        let src = "a **b** c";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        assert_eq!(i.plain_text(src), "a b c");
        assert_eq!(i.delimiter_count(), 2);
        assert_eq!(i.constructs.len(), 1);
        assert_eq!(i.constructs[0].kind, InlineKind::Strong);
    }

    #[test]
    fn emphasis_kinds() {
        for (src, kind) in [
            ("*x*", InlineKind::Em),
            ("**x**", InlineKind::Strong),
            ("~~x~~", InlineKind::Strike),
            ("==x==", InlineKind::Highlight),
        ] {
            let i = Inline::scan(src);
            assert_contiguous(&i, src);
            assert_eq!(i.constructs.len(), 1, "{src}");
            assert_eq!(i.constructs[0].kind, kind, "{src}");
            assert_eq!(i.plain_text(src), "x", "{src}");
        }
    }

    #[test]
    fn nested_emphasis_accumulates_style() {
        let src = "**bold _and italic_**";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        assert_eq!(i.constructs.len(), 2);
        let styled = i
            .spans
            .iter()
            .find(|s| s.text(src).contains("and"))
            .expect("inner span");
        assert!(styled.style.strong && styled.style.em);
    }

    #[test]
    fn code_spans_are_verbatim() {
        let src = "`a *b* c`";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        assert_eq!(i.constructs.len(), 1);
        assert_eq!(i.constructs[0].kind, InlineKind::Code);
        assert_eq!(i.plain_text(src), "a *b* c");
        // The `*` inside must not become a construct.
        assert!(!i.constructs.iter().any(|c| c.kind == InlineKind::Em));
    }

    #[test]
    fn links_capture_href() {
        let src = "see [docs](https://example.com \"title\") now";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        let link = i
            .constructs
            .iter()
            .find(|c| c.kind == InlineKind::Link)
            .expect("link");
        assert_eq!(link.href.as_deref(), Some("https://example.com"));
        assert_eq!(i.plain_text(src), "see docs now");
        assert_eq!(i.link_at(src.find("docs").unwrap()), Some("https://example.com"));
    }

    #[test]
    fn images_are_distinguished_from_links() {
        let src = "![alt](pic.png)";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        assert_eq!(i.constructs[0].kind, InlineKind::Image);
        assert_eq!(i.constructs[0].href.as_deref(), Some("pic.png"));
        assert!(!i.spans.iter().any(|s| s.style.link));
    }

    #[test]
    fn math_requires_tight_delimiters() {
        let i = Inline::scan("$E=mc^2$");
        assert_eq!(i.constructs.len(), 1);
        assert_eq!(i.plain_text("$E=mc^2$"), "E=mc^2");

        // Currency-looking text must not become math.
        let money = Inline::scan("costs $5 and $10 today");
        assert!(money.constructs.is_empty());
    }

    #[test]
    fn display_math_is_recognised() {
        let src = "$$\\int_0^1 x\\,dx$$";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        assert_eq!(i.constructs.len(), 1);
        assert_eq!(i.constructs[0].kind, InlineKind::Math);
    }

    #[test]
    fn escapes_hide_the_backslash() {
        let src = "a \\*not emphasis\\* b";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        assert!(i.constructs.iter().all(|c| c.kind == InlineKind::Escape));
        assert_eq!(i.plain_text(src), "a *not emphasis* b");
    }

    #[test]
    fn mdx_expression_handles_nesting_and_strings() {
        let src = "value {fn({ a: 1 }, \"}\"))} end";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        let e = i
            .constructs
            .iter()
            .find(|c| c.kind == InlineKind::MdxExpr)
            .expect("mdx expr");
        assert!(e.close.is_some());
    }

    #[test]
    fn jsx_and_autolinks_are_classified() {
        let jsx = Inline::scan("hi <Badge count={1} /> there");
        assert!(jsx.constructs.iter().any(|c| c.kind == InlineKind::MdxJsx));

        let auto = Inline::scan("see <https://example.com> now");
        let l = auto
            .constructs
            .iter()
            .find(|c| c.kind == InlineKind::Link)
            .expect("autolink");
        assert_eq!(l.href.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn revealed_tracks_the_caret() {
        let src = "a **b** c";
        let i = Inline::scan(src);
        // Caret inside `b`.
        assert_eq!(i.revealed(5, 5), vec![0]);
        // Caret far away.
        assert!(i.revealed(0, 0).is_empty());
    }

    #[test]
    fn unterminated_constructs_degrade_to_text() {
        for src in ["**bold", "`code", "[text](", "$math", "a * b * c"] {
            let i = Inline::scan(src);
            assert_contiguous(&i, src);
        }
        let i = Inline::scan("**bold");
        assert!(i.constructs.is_empty() || i.constructs[0].close.is_none());
    }

    #[test]
    fn underscore_inside_words_stays_literal() {
        let src = "snake_case_name";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        assert!(i.constructs.is_empty(), "{:?}", i.constructs);
    }

    #[test]
    fn cjk_text_is_handled() {
        let src = "**粗体**与 `代码` 和 $x$ 混排";
        let i = Inline::scan(src);
        assert_contiguous(&i, src);
        assert_eq!(i.plain_text(src), "粗体与 代码 和 x 混排");
    }

    #[test]
    fn fuzz_never_panics_and_stays_contiguous() {
        let samples = [
            "",
            "\\",
            "`",
            "***",
            "[]()",
            "![](",
            "{[",
            "<",
            "$",
            "==",
            "~~~~",
            "a**b*c**d",
            "**a *b* c**",
            "\u{4E2D}**\u{6587}**",
            "[a](b(c))",
            "<a b='>'>",
            "$$$$",
            "\\*\\*",
        ];
        for src in samples {
            let i = Inline::scan(src);
            assert_contiguous(&i, src);
        }
    }
}
