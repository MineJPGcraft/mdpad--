//! Document model: what a parsed Markdown document looks like in memory.
//!
//! The model is deliberately *source-anchored*. [`Document::text`] is the single
//! source of truth; every [`Block`] merely records a byte range into it. The
//! editor only ever rewrites the range of the block the user actually edited, so
//! untouched Markdown survives a full edit session byte-for-byte — there is no
//! lossy "parse then pretty-print" round trip to worry about.
//!
//! Blocks therefore only carry *copies* of their text for rendering convenience;
//! [`Block::span`] is authoritative. After every commit the document reparses,
//! which is cheap enough to run on each keystroke (a few hundred microseconds for
//! a book-length file).

use std::ops::Range;
use std::path::{Path, PathBuf};

/// Which Markdown dialect a document is written in.
///
/// The flavour changes only the set of block constructs the parser recognises
/// and how MDX/Markdoc nodes are rendered — the document text is never rewritten
/// on load.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Flavor {
    #[default]
    Markdown,
    Mdx,
    Markdoc,
}

impl Flavor {
    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Mdx => "MDX",
            Self::Markdoc => "Markdoc",
        }
    }

    /// Guesses the flavour from a file extension.
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("mdx") => Self::Mdx,
            Some("markdoc") | Some("mdoc") => Self::Markdoc,
            _ => Self::Markdown,
        }
    }

    /// All flavours, in menu order.
    pub fn all() -> [Self; 3] {
        [Self::Markdown, Self::Mdx, Self::Markdoc]
    }
}

// ---------------------------------------------------------------------------
// Callouts / admonitions
// ---------------------------------------------------------------------------

/// The four callout syntaxes MarkdownPad reads and writes.
///
/// They are genuinely different formats, not just different styling: GitHub and
/// Obsidian both use a marked block quote, while VitePress and Docusaurus both
/// use `:::` containers with different keyword sets and defaults.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CalloutSyntax {
    /// `> [!NOTE]` — GitHub's alert syntax.
    #[default]
    GitHub,
    /// `> [!note] Title` — Obsidian's callout syntax (custom titles, folding).
    Obsidian,
    /// `::: tip Title` … `:::` — VitePress custom containers.
    VitePress,
    /// `:::note Title` … `:::` — Docusaurus admonitions.
    Docusaurus,
}

impl CalloutSyntax {
    pub fn label(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::Obsidian => "Obsidian",
            Self::VitePress => "VitePress",
            Self::Docusaurus => "Docusaurus",
        }
    }

    /// A one-line description of the syntax, shown in the UI.
    pub fn sample(self) -> &'static str {
        match self {
            Self::GitHub => "> [!NOTE]",
            Self::Obsidian => "> [!note] 自定义标题",
            Self::VitePress => "::: tip 标题",
            Self::Docusaurus => ":::note 标题",
        }
    }

    pub fn all() -> [Self; 4] {
        [
            Self::GitHub,
            Self::Obsidian,
            Self::VitePress,
            Self::Docusaurus,
        ]
    }

    /// `true` when the syntax is expressed as a marked block quote.
    pub fn is_quote_form(self) -> bool {
        matches!(self, Self::GitHub | Self::Obsidian)
    }

    /// Keywords this syntax natively recognises.
    pub fn keywords(self) -> &'static [CalloutVariant] {
        use CalloutVariant::*;
        match self {
            // GitHub documents exactly five alert types.
            Self::GitHub => &[Note, Tip, Important, Warning, Caution],
            // Obsidian ships thirteen, several with aliases.
            Self::Obsidian => &[
                Note, Abstract, Info, Todo, Tip, Success, Question, Warning, Failure, Danger, Bug,
                Example, Quote,
            ],
            // VitePress has four containers plus `details`.
            Self::VitePress => &[Tip, Info, Warning, Danger, Details],
            // Docusaurus admonitions.
            Self::Docusaurus => &[Note, Tip, Info, Warning, Caution, Danger],
        }
    }
}

/// Semantic kind of a callout, normalised across the four syntaxes.
#[derive(Clone, PartialEq, Eq, Debug, Default, Hash)]
pub enum CalloutVariant {
    #[default]
    Note,
    Tip,
    Important,
    Warning,
    Caution,
    Info,
    Danger,
    Success,
    Question,
    Failure,
    Bug,
    Example,
    Abstract,
    Todo,
    Details,
    Quote,
    /// A `:::` container with a non-standard keyword, or a custom container.
    Custom(String),
}

impl CalloutVariant {
    /// The canonical keyword for this variant under `syntax`.
    ///
    /// Falls back to the closest supported variant so that, for example, a
    /// GitHub callout converted to VitePress becomes `::: warning` rather than
    /// an unparseable keyword.
    pub fn key_for(&self, syntax: CalloutSyntax) -> String {
        let v = self.nearest(syntax);
        match syntax {
            CalloutSyntax::GitHub => match v {
                Self::Note => "NOTE",
                Self::Tip => "TIP",
                Self::Important => "IMPORTANT",
                Self::Warning => "WARNING",
                Self::Caution => "CAUTION",
                _ => "NOTE",
            }
            .to_owned(),
            CalloutSyntax::Obsidian => match v {
                Self::Note => "note",
                Self::Abstract => "abstract",
                Self::Info => "info",
                Self::Todo => "todo",
                Self::Tip => "tip",
                Self::Success => "success",
                Self::Question => "question",
                Self::Warning => "warning",
                Self::Failure => "failure",
                Self::Danger => "danger",
                Self::Bug => "bug",
                Self::Example => "example",
                Self::Quote => "quote",
                Self::Custom(name) => return name.clone(),
                _ => "note",
            }
            .to_owned(),
            CalloutSyntax::VitePress => match v {
                Self::Tip => "tip",
                Self::Info => "info",
                Self::Warning => "warning",
                Self::Danger => "danger",
                Self::Details => "details",
                Self::Note => "info",
                Self::Caution => "warning",
                Self::Important => "danger",
                Self::Custom(name) => return name.clone(),
                _ => "tip",
            }
            .to_owned(),
            CalloutSyntax::Docusaurus => match v {
                Self::Note => "note",
                Self::Tip => "tip",
                Self::Info => "info",
                Self::Warning | Self::Caution => "caution",
                Self::Danger => "danger",
                Self::Custom(name) => return name.clone(),
                _ => "note",
            }
            .to_owned(),
        }
    }

    /// Whether this variant is natively supported by `syntax`.
    pub fn is_native_to(&self, syntax: CalloutSyntax) -> bool {
        match self {
            Self::Custom(_) => !syntax.is_quote_form(),
            other => syntax.keywords().iter().any(|k| k == other),
        }
    }

    /// The closest variant `syntax` actually supports.
    fn nearest(&self, syntax: CalloutSyntax) -> Self {
        if self.is_native_to(syntax) {
            return self.clone();
        }
        use CalloutVariant::*;
        match syntax {
            CalloutSyntax::GitHub => match self {
                Tip | Success | Example | Quote => Tip,
                Important | Danger | Bug | Failure => Important,
                Warning => Warning,
                Caution | Question | Todo | Details | Abstract => Note,
                _ => Note,
            },
            CalloutSyntax::VitePress => match self {
                Note | Info | Abstract | Todo | Quote => Info,
                Tip | Success | Example => Tip,
                Warning | Caution | Question | Bug => Warning,
                Danger | Failure => Danger,
                Details => Details,
                _ => Tip,
            },
            CalloutSyntax::Docusaurus => match self {
                Tip | Success | Example | Quote => Tip,
                Info | Abstract | Todo => Info,
                Warning | Caution | Question | Bug => Caution,
                Danger | Failure => Danger,
                Details => Note,
                _ => Note,
            },
            CalloutSyntax::Obsidian => match self {
                Details => Note,
                _ => Note,
            },
        }
    }

    /// Human-readable label used in menus and rendered titles.
    pub fn label(&self) -> String {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
            Self::Info => "Info",
            Self::Danger => "Danger",
            Self::Success => "Success",
            Self::Question => "Question",
            Self::Failure => "Failure",
            Self::Bug => "Bug",
            Self::Example => "Example",
            Self::Abstract => "Abstract",
            Self::Todo => "Todo",
            Self::Details => "Details",
            Self::Quote => "Quote",
            Self::Custom(name) => return name.clone(),
        }
        .to_owned()
    }

    /// Parses a source keyword, honouring each syntax's aliases.
    pub fn from_key(syntax: CalloutSyntax, key: &str) -> Self {
        let k = key.trim();
        let lower = k.to_ascii_lowercase();
        if syntax == CalloutSyntax::GitHub {
            // GitHub is case-insensitive but conventionally uppercase.
            let upper = k.to_ascii_uppercase();
            return match upper.as_str() {
                "NOTE" => Self::Note,
                "TIP" => Self::Tip,
                "IMPORTANT" => Self::Important,
                "WARNING" => Self::Warning,
                "CAUTION" => Self::Caution,
                _ => Self::Custom(lower),
            };
        }
        match lower.as_str() {
            "note" => Self::Note,
            "tip" | "hint" => Self::Tip,
            "important" => Self::Important,
            "warning" | "attention" => Self::Warning,
            "caution" => Self::Caution,
            "info" => Self::Info,
            "danger" | "error" => Self::Danger,
            "success" | "check" | "done" => Self::Success,
            "question" | "help" | "faq" => Self::Question,
            "failure" | "fail" | "missing" => Self::Failure,
            "bug" => Self::Bug,
            "example" => Self::Example,
            "abstract" | "summary" | "tldr" => Self::Abstract,
            "todo" => Self::Todo,
            "details" => Self::Details,
            "quote" | "cite" => Self::Quote,
            _ => Self::Custom(lower),
        }
    }
}

/// A parsed callout / admonition block.
#[derive(Clone, Debug)]
pub struct Callout {
    /// The syntax the block was written in; serialisation reuses it verbatim.
    pub syntax: CalloutSyntax,
    pub variant: CalloutVariant,
    /// Custom title, when the source provided one.
    pub title: Option<String>,
    /// `Some(true)` = `> [!note]+` (open), `Some(false)` = `> [!note]-` (folded).
    /// Only meaningful for [`CalloutSyntax::Obsidian`].
    pub foldable: Option<bool>,
    /// Body, parsed recursively so callouts can contain lists, code, math, ...
    pub body: Vec<Block>,
    /// Byte range of the body inside the original source, used for in-place editing.
    pub body_span: Range<usize>,
}

impl Callout {
    pub fn new(syntax: CalloutSyntax, variant: CalloutVariant) -> Self {
        Self {
            syntax,
            variant,
            title: None,
            foldable: None,
            body: Vec::new(),
            body_span: 0..0,
        }
    }

    /// Title to display: the explicit one, or the variant's default label when
    /// the syntax renders a default heading.
    pub fn display_title(&self) -> Option<String> {
        match (&self.title, self.syntax) {
            (Some(t), _) if !t.trim().is_empty() => Some(t.clone()),
            // GitHub always shows the alert keyword as a heading.
            (_, CalloutSyntax::GitHub) => Some(self.variant.label()),
            // VitePress/Docusaurus show the title only when written.
            (_, CalloutSyntax::VitePress) | (_, CalloutSyntax::Docusaurus) => None,
            // Obsidian shows the variant name as the default title.
            (_, CalloutSyntax::Obsidian) => Some(self.variant.label()),
        }
    }
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// Ordered / unordered list marker kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ListKind {
    #[default]
    Unordered,
    Ordered,
}

/// One parsed block, anchored to a byte range of the document source.
#[derive(Clone, Debug)]
pub struct Block {
    pub kind: BlockKind,
    /// Byte range in [`Document::text`], including the trailing newline.
    pub span: Range<usize>,
    /// Byte range of this block's *inline* content inside
    /// [`Document::text`].
    ///
    /// This is what the renderer feeds to [`crate::doc::inline::Inline::scan_at`],
    /// so inline delimiters, links and formulas are reported in document
    /// coordinates and the caret can be mapped straight onto them. It is empty
    /// for blocks that have no inline content (rules, front matter, ...).
    pub inline_span: Range<usize>,
}

impl Block {
    pub fn new(kind: BlockKind, span: Range<usize>, inline_span: Range<usize>) -> Self {
        Self {
            kind,
            span,
            inline_span,
        }
    }

    /// The block's inline source, when it has any.
    pub fn inline_source<'a>(&self, doc: &'a str) -> &'a str {
        doc.get(self.inline_span.clone()).unwrap_or("")
    }

    /// `true` when the caret should be able to enter this block as text.
    pub fn is_editable(&self) -> bool {
        !matches!(self.kind, BlockKind::Rule)
    }

    /// A short label for the block, shown in the block gutter.
    pub fn type_label(&self) -> &'static str {
        match &self.kind {
            BlockKind::Heading { level, .. } => match level {
                1 => "H1",
                2 => "H2",
                3 => "H3",
                4 => "H4",
                5 => "H5",
                _ => "H6",
            },
            BlockKind::Paragraph { .. } => "¶",
            BlockKind::ListItem { .. } => "•",
            BlockKind::Quote { .. } => "❝",
            BlockKind::Callout(_) => "▍",
            BlockKind::Code { .. } => "</>",
            BlockKind::Math { .. } => "∑",
            BlockKind::Table { .. } => "▦",
            BlockKind::Rule => "―",
            BlockKind::Html { .. } => "html",
            BlockKind::FrontMatter { .. } => "yaml",
            BlockKind::MdxEsm { .. } => "esm",
            BlockKind::MdxJsx { .. } => "jsx",
            BlockKind::MarkdocTag { .. } => "{% %}",
        }
    }

    /// Plain text of the block's own content (no nested blocks, no markup).
    pub fn plain_text(&self) -> String {
        match &self.kind {
            BlockKind::Heading { text, .. }
            | BlockKind::Paragraph { text }
            | BlockKind::Quote { text }
            | BlockKind::ListItem { text, .. } => text.clone(),
            BlockKind::Callout(c) => c
                .body
                .iter()
                .map(Block::plain_text)
                .collect::<Vec<_>>()
                .join("\n"),
            BlockKind::Code { code, .. } | BlockKind::Math { tex: code } => code.clone(),
            BlockKind::Table { header, rows, .. } => {
                let mut s = header.join(" | ");
                for r in rows {
                    s.push('\n');
                    s.push_str(&r.join(" | "));
                }
                s
            }
            _ => String::new(),
        }
    }

    /// Word- and character-count contribution, used by the status bar.
    pub fn word_count(&self) -> usize {
        match &self.kind {
            // Count CJK glyphs individually and Latin runs as words.
            BlockKind::Code { .. } | BlockKind::Math { .. } => 0,
            other => count_words(&other_plain_text(other)),
        }
    }

    /// The diagram engine for fenced code blocks, when the info string names one.
    pub fn diagram_engine(&self) -> Option<crate::diagram::Engine> {
        match &self.kind {
            BlockKind::Code { lang, .. } => crate::diagram::Engine::from_fence_info(lang),
            _ => None,
        }
    }
}

fn other_plain_text(kind: &BlockKind) -> String {
    match kind {
        BlockKind::Heading { text, .. }
        | BlockKind::Paragraph { text }
        | BlockKind::Quote { text }
        | BlockKind::ListItem { text, .. } => text.clone(),
        BlockKind::Callout(c) => c
            .body
            .iter()
            .map(Block::plain_text)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Counts words, treating each CJK ideograph as one word.
pub fn count_words(text: &str) -> usize {
    let mut count = 0;
    let mut in_latin = false;
    for ch in text.chars() {
        let is_cjk = crate::gfx::is_wide(ch);
        if is_cjk {
            count += 1;
            in_latin = false;
        } else if ch.is_alphanumeric() || ch == '\'' || ch == '-' {
            if !in_latin {
                count += 1;
                in_latin = true;
            }
        } else {
            in_latin = false;
        }
    }
    count
}

/// The payload of a [`Block`].
#[derive(Clone, Debug)]
pub enum BlockKind {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    ListItem {
        kind: ListKind,
        /// 1-based ordinal for ordered lists.
        index: u64,
        /// Nesting depth, 0-based.
        level: u8,
        /// `Some` for task list items (`- [x] done`).
        checked: Option<bool>,
        text: String,
    },
    /// A plain block quote that is not a callout.
    Quote {
        text: String,
    },
    Callout(Callout),
    Code {
        /// Fence info string's first word, e.g. `mermaid`, `rust`.
        lang: String,
        /// Full info string after the fence, e.g. `mermaid title="x"`.
        info: String,
        code: String,
        /// `true` when written with backticks (```` ``` ````) rather than tildes.
        backticks: bool,
        /// Number of fence characters in the opening fence.
        fence_len: usize,
    },
    Math {
        tex: String,
    },
    Table {
        align: Vec<Align>,
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Rule,
    Html {
        raw: String,
    },
    FrontMatter {
        delimiter: String,
        raw: String,
    },
    /// MDX `import` / `export` statement.
    MdxEsm {
        raw: String,
    },
    /// MDX JSX element occupying its own block.
    MdxJsx {
        name: String,
        /// Raw attribute text between the tag name and `>`.
        props: String,
        self_closing: bool,
        /// Raw inner source for non-self-closing elements.
        children_raw: String,
        /// Byte range of `children_raw` inside the document source.
        children_span: Range<usize>,
        /// Children parsed as Markdown so `<Callout>` bodies render properly.
        children: Vec<Block>,
    },
    /// Markdoc tag: `{% ... %}`.
    MarkdocTag {
        kind: MarkdocTagKind,
        /// Tag name, or the opening tag's name for a block pair.
        name: String,
        /// Raw attribute text.
        attrs: String,
        /// Byte range of the body for block pairs.
        body_span: Range<usize>,
        raw: String,
        body: Vec<Block>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkdocTagKind {
    /// `{% tag attr="v" %}` — leaf tag.
    Leaf,
    /// `{% tag %} ... {% /tag %}` — container tag.
    Pair,
    /// `{% if ... %}` … `{% /if %}` and friends.
    Conditional,
    /// `{% #partial %}`, `{% $variable %}`.
    Reference,
    /// `{% comment %}` … `{% /comment %}`.
    Comment,
}

impl MarkdocTagKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Leaf => "tag",
            Self::Pair => "container",
            Self::Conditional => "conditional",
            Self::Reference => "reference",
            Self::Comment => "comment",
        }
    }
}

/// Short machine-readable name for a block kind, for diagnostics.
pub fn kind_label(kind: &BlockKind) -> &'static str {
    match kind {
        BlockKind::Heading { .. } => "heading",
        BlockKind::Paragraph { .. } => "paragraph",
        BlockKind::ListItem { .. } => "list-item",
        BlockKind::Quote { .. } => "quote",
        BlockKind::Callout(_) => "callout",
        BlockKind::Code { .. } => "code",
        BlockKind::Math { .. } => "math",
        BlockKind::Table { .. } => "table",
        BlockKind::Rule => "rule",
        BlockKind::Html { .. } => "html",
        BlockKind::FrontMatter { .. } => "front-matter",
        BlockKind::MdxEsm { .. } => "mdx-esm",
        BlockKind::MdxJsx { .. } => "mdx-jsx",
        BlockKind::MarkdocTag { .. } => "markdoc",
    }
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// A parsed Markdown / MDX / Markdoc document.
#[derive(Clone, Debug)]
pub struct Document {
    pub path: Option<PathBuf>,
    pub flavor: Flavor,
    /// The source of truth. Never rewritten except by an explicit block edit.
    pub text: String,
    pub blocks: Vec<Block>,
    /// Bumped on every mutation; caches compare against it.
    pub revision: u64,
    pub dirty: bool,
    /// End-of-line convention of the file, preserved on save.
    pub crlf: bool,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// An empty, untitled document.
    pub fn new() -> Self {
        let mut doc = Self {
            path: None,
            flavor: Flavor::Markdown,
            text: String::new(),
            blocks: Vec::new(),
            revision: 1,
            dirty: false,
            crlf: false,
        };
        doc.reparse();
        doc
    }

    /// Loads a document from disk.
    pub fn from_file(path: PathBuf, raw: &str) -> Self {
        let crlf = raw.contains("\r\n");
        let text = normalise_newlines(raw);
        let mut doc = Self {
            flavor: Flavor::from_path(&path),
            path: Some(path),
            text,
            blocks: Vec::new(),
            revision: 1,
            dirty: false,
            crlf,
        };
        doc.reparse();
        doc
    }

    /// Replaces the whole document text (used by the source-mode editor).
    pub fn set_text(&mut self, text: String) {
        if text == self.text {
            return;
        }
        self.text = text;
        self.revision += 1;
        self.dirty = true;
        self.reparse();
    }

    /// Replaces a byte range with new source text and reparses.
    ///
    /// `span` must come from a [`Block`] of the current revision; the parser
    /// clamps out-of-range indices rather than panicking.
    pub fn replace_span(&mut self, span: Range<usize>, new_text: &str) {
        let start = clamp_boundary(&self.text, span.start);
        let end = clamp_boundary(&self.text, span.end.max(start));
        self.text.replace_range(start..end, new_text);
        self.revision += 1;
        self.dirty = true;
        self.reparse();
    }

    /// Rebuilds [`Self::blocks`] from [`Self::text`].
    pub fn reparse(&mut self) {
        self.blocks = crate::doc::block::parse_document(&self.text, self.flavor);
    }

    /// File name for display, or `未命名` for an unsaved document.
    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| match self.flavor {
                Flavor::Markdown => "未命名.md".to_owned(),
                Flavor::Mdx => "未命名.mdx".to_owned(),
                Flavor::Markdoc => "未命名.markdoc".to_owned(),
            })
    }

    /// Full path for display, falling back to the display name.
    pub fn display_path(&self) -> String {
        self.path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| self.display_name())
    }

    /// The block containing `byte`, if any.
    pub fn block_at(&self, byte: usize) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| b.span.start <= byte && byte < b.span.end)
    }

    /// Whether the document contains a block of the given flavour-specific kind.
    pub fn has_mdx(&self) -> bool {
        self.blocks
            .iter()
            .any(|b| matches!(b.kind, BlockKind::MdxEsm { .. } | BlockKind::MdxJsx { .. }))
    }

    /// Whether the document uses any Markdoc tag.
    pub fn has_markdoc(&self) -> bool {
        self.blocks
            .iter()
            .any(|b| matches!(b.kind, BlockKind::MarkdocTag { .. }))
    }

    /// The callout syntaxes actually used in the document, in first-use order.
    pub fn callout_syntaxes(&self) -> Vec<CalloutSyntax> {
        let mut out = Vec::new();
        collect_callout_syntaxes(&self.blocks, &mut out);
        out
    }

    /// Character and word counts for the status bar.
    pub fn stats(&self) -> DocStats {
        let chars = self.text.chars().count();
        let words: usize = self.blocks.iter().map(Block::word_count).sum();
        let lines = self.text.lines().count();
        let reading_minutes = if words == 0 { 0 } else { (words + 299) / 300 };
        DocStats {
            chars,
            words,
            lines,
            reading_minutes,
        }
    }

    /// Rendered length of the document in bytes.
    pub fn byte_len(&self) -> usize {
        self.text.len()
    }
}

fn collect_callout_syntaxes(blocks: &[Block], out: &mut Vec<CalloutSyntax>) {
    for b in blocks {
        match &b.kind {
            BlockKind::Callout(c) => {
                if !out.contains(&c.syntax) {
                    out.push(c.syntax);
                }
                collect_callout_syntaxes(&c.body, out);
            }
            BlockKind::MdxJsx { children, .. } => collect_callout_syntaxes(children, out),
            _ => {}
        }
    }
}

/// Aggregated document statistics.
#[derive(Clone, Copy, Debug, Default)]
pub struct DocStats {
    pub chars: usize,
    pub words: usize,
    pub lines: usize,
    pub reading_minutes: usize,
}

/// Clamps `index` into `text` and snaps it to a UTF-8 character boundary.
pub fn clamp_boundary(text: &str, index: usize) -> usize {
    let mut i = index.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Converts CRLF and lone CR line endings to LF.
pub fn normalise_newlines(raw: &str) -> String {
    if !raw.contains('\r') {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// Converts LF line endings to CRLF.
pub fn to_crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavour_from_extension() {
        assert_eq!(Flavor::from_path(Path::new("a.md")), Flavor::Markdown);
        assert_eq!(Flavor::from_path(Path::new("a.MDX")), Flavor::Mdx);
        assert_eq!(Flavor::from_path(Path::new("a.markdoc")), Flavor::Markdoc);
        assert_eq!(Flavor::from_path(Path::new("noext")), Flavor::Markdown);
    }

    #[test]
    fn github_keywords_round_trip() {
        for v in CalloutSyntax::GitHub.keywords() {
            let key = v.key_for(CalloutSyntax::GitHub);
            assert_eq!(
                CalloutVariant::from_key(CalloutSyntax::GitHub, &key),
                v.clone(),
                "round trip failed for {key}"
            );
        }
    }

    #[test]
    fn obsidian_aliases_map_to_canonical() {
        let cases = [
            ("hint", CalloutVariant::Tip),
            ("check", CalloutVariant::Success),
            ("tldr", CalloutVariant::Abstract),
            ("error", CalloutVariant::Danger),
        ];
        for (key, want) in cases {
            assert_eq!(
                CalloutVariant::from_key(CalloutSyntax::Obsidian, key),
                want
            );
        }
    }

    #[test]
    fn unsupported_variants_map_to_a_native_one() {
        // `Danger` does not exist in GitHub's syntax.
        let key = CalloutVariant::Danger.key_for(CalloutSyntax::GitHub);
        assert!(CalloutSyntax::GitHub.keywords().iter().any(|k| k.key_for(CalloutSyntax::GitHub) == key));
        // `Details` is VitePress-only and must degrade in Docusaurus.
        let key = CalloutVariant::Details.key_for(CalloutSyntax::Docusaurus);
        assert_eq!(key, "note");
    }

    #[test]
    fn custom_variant_keeps_its_name() {
        let v = CalloutVariant::Custom("spoiler".into());
        assert_eq!(v.key_for(CalloutSyntax::VitePress), "spoiler");
        assert_eq!(v.key_for(CalloutSyntax::VitePress), "spoiler");
        assert!(!v.is_native_to(CalloutSyntax::GitHub));
    }

    #[test]
    fn consecutive_alerts_stay_separate_blocks() {
        // A blank line ends a blockquote. Gluing them together made the editor
        // render only the first alert and show every later one as literal
        // `[!TIP]` text wrapped inside it.
        let doc = Document::from_file(
            "t.md".into(),
            "> [!NOTE]\n> a\n\n> [!TIP]\n> b\n\n> [!WARNING]\n> c\n\n> [!CAUTION]\n> d\n",
        );
        let variants: Vec<CalloutVariant> = doc
            .blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::Callout(c) => Some(c.variant.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            variants,
            vec![
                CalloutVariant::Note,
                CalloutVariant::Tip,
                CalloutVariant::Warning,
                CalloutVariant::Caution
            ]
        );
        assert_eq!(doc.blocks.len(), 4, "one block per alert");
        // Each block must own only its own two lines.
        for block in &doc.blocks {
            assert_eq!(
                doc.text[block.span.clone()].lines().count(),
                2,
                "block span leaked into the next alert: {:?}",
                &doc.text[block.span.clone()]
            );
        }
    }

    #[test]
    fn a_quote_can_still_span_a_paragraph_break_with_a_marker() {
        // The rule that replaced the old behaviour: to continue across a blank
        // line the author writes `>` on it, exactly as CommonMark requires.
        let doc = Document::from_file("t.md".into(), "> first\n>\n> second\n");
        assert_eq!(doc.blocks.len(), 1, "still one blockquote");
        match &doc.blocks[0].kind {
            BlockKind::Quote { text } => {
                assert!(text.contains("first") && text.contains("second"), "{text:?}");
            }
            other => panic!("expected a quote, got {}", kind_label(other)),
        }
    }

    #[test]
    fn replace_span_edits_only_that_range() {
        let mut doc = Document::new();
        doc.set_text("# Title\n\nfirst\n\nsecond\n".to_owned());
        let block = doc
            .blocks
            .iter()
            .find(|b| matches!(&b.kind, BlockKind::Paragraph { text } if text == "first"))
            .cloned()
            .expect("paragraph block");
        doc.replace_span(block.span.clone(), "FIRST\n");
        assert!(doc.text.contains("FIRST"));
        assert!(doc.text.contains("second"));
        assert!(!doc.text.contains("first"));
        assert!(doc.dirty);
    }

    #[test]
    fn replace_span_tolerates_bad_ranges() {
        let mut doc = Document::new();
        doc.set_text("中文段落\n".to_owned());
        // Mid-character index must not panic.
        doc.replace_span(1..2, "x");
        assert!(doc.text.contains('x'));
    }

    #[test]
    fn word_count_handles_cjk_and_latin() {
        assert_eq!(count_words("hello world"), 2);
        assert_eq!(count_words("中文测试"), 4);
        // Two ideographs + one Latin word + two ideographs.
        assert_eq!(count_words("混合 mixed 文本"), 5);
        assert_eq!(count_words(""), 0);
    }

    #[test]
    fn stats_are_computed() {
        let mut doc = Document::new();
        doc.set_text("# 标题\n\nhello world\n".to_owned());
        let s = doc.stats();
        assert_eq!(s.lines, 3);
        assert!(s.chars > 10);
        assert_eq!(s.words, 4);
    }

    #[test]
    fn newline_normalisation() {
        assert_eq!(normalise_newlines("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(to_crlf("a\nb"), "a\r\nb");
    }

    #[test]
    fn block_at_finds_containing_block() {
        let mut doc = Document::new();
        doc.set_text("# A\n\npara\n".to_owned());
        let para = doc
            .blocks
            .iter()
            .position(|b| matches!(b.kind, BlockKind::Paragraph { .. }))
            .expect("paragraph");
        let start = doc.blocks[para].span.start;
        assert_eq!(doc.block_at(start), Some(para));
    }
}
