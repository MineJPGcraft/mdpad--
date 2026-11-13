//! Tokenizer and abstract syntax tree for the LaTeX math subset.
//!
//! The front-end is deliberately *total*: every input produces some tree, and
//! every problem is pushed into [`ParseOutput::warnings`] instead of aborting
//! the parse. That mirrors the "collect, do not fail" policy of
//! [`crate::diagram::model`] and is what keeps arbitrary user text from ever
//! taking the UI down. Anything the parser does not understand becomes a
//! [`Node::Raw`], which the layout step paints as faded literal source.
//!
//! Geometry is *not* decided here. The AST stores only what is needed to pick
//! a glyph, a font face and a spacing class; sizes and positions belong to
//! [`super::layout`].

use crate::gfx::{Family, Rgba, Slant, Weight};

/// Maximum nesting depth for braces, groups and environments.
///
/// Recursive descent over adversarial input (`{{{{...`) would otherwise grow
/// the stack without bound; past this depth the parser stops descending and
/// records a warning.
pub const MAX_DEPTH: u32 = 64;

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

/// A single lexical unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// `\name`, without the leading backslash. `\\` (the row separator) and
    /// the escaped punctuation commands (`\{`, `\,`, ...) carry their single
    /// character as the name, so `\` + `_` yields `Command("_")`.
    Command(String),
    /// Raw braced content of a text-like command, with spaces preserved:
    /// `\text{...}`, `\mathrm`-free helpers, `\begin{...}`, `\hspace{...}`.
    Group(String),
    /// Any other single character: a letter, a digit, `.`, `+`, `(` ...
    Char(char),
    LBrace,
    RBrace,
    /// `^`
    Sup,
    /// `_`
    Sub,
    /// `&`
    Amp,
}

/// Commands whose `{...}` argument is *text*, not math.
///
/// Those groups are captured verbatim by the lexer so that `\text{if }` keeps
/// its spaces (which math mode would otherwise discard).
const RAW_GROUP_COMMANDS: [&str; 19] = [
    "text",
    "textrm",
    "textsf",
    "texttt",
    "textnormal",
    "textup",
    "textmd",
    "textbf",
    "textit",
    "textsl",
    "mbox",
    "operatorname",
    "hspace",
    "mkern",
    "mskip",
    "color",
    "textcolor",
    "begin",
    "end",
];

/// Split `src` into [`Token`]s. `%` starts a comment running to end of line.
///
/// Whitespace is discarded: math mode is whitespace-insensitive, so `a b` and
/// `ab` tokenize identically. Spaces inside a raw group survive.
pub fn tokenize(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '%' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    // A trailing backslash cannot name anything; keep it so the
                    // parser can report it instead of silently dropping it.
                    out.push(Token::Command(String::new()));
                    break;
                }
                let n = chars[i];
                if n.is_alphabetic() {
                    let start = i;
                    while i < chars.len() && chars[i].is_alphabetic() {
                        i += 1;
                    }
                    let mut name: String = chars[start..i].iter().collect();
                    // `\operatorname*{...}` asks for limit placement. The star
                    // is folded into the command name so the parser sees one
                    // token, and the text argument is still captured verbatim.
                    if name == "operatorname" && i < chars.len() && chars[i] == '*' {
                        i += 1;
                        name.push('*');
                    }
                    if name == "operatorname*" || RAW_GROUP_COMMANDS.contains(&name.as_str()) {
                        let mut j = i;
                        while j < chars.len() && chars[j].is_whitespace() {
                            j += 1;
                        }
                        if j < chars.len() && chars[j] == '{' {
                            let (content, next) = read_group(&chars, j);
                            i = next;
                            out.push(Token::Command(name));
                            out.push(Token::Group(content));
                            continue;
                        }
                    }
                    out.push(Token::Command(name));
                } else {
                    i += 1;
                    out.push(Token::Command(n.to_string()));
                }
            }
            '{' => {
                out.push(Token::LBrace);
                i += 1;
            }
            '}' => {
                out.push(Token::RBrace);
                i += 1;
            }
            '^' => {
                out.push(Token::Sup);
                i += 1;
            }
            '_' => {
                out.push(Token::Sub);
                i += 1;
            }
            '&' => {
                out.push(Token::Amp);
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            c => {
                out.push(Token::Char(c));
                i += 1;
            }
        }
    }

    out
}

/// Read the balanced group starting at `chars[start] == '{'`.
///
/// Returns the content without the outer braces and the index just past the
/// closing brace. A handful of escapes are resolved (`\%` -> `%`), any other
/// backslash sequence is preserved for the caller to deal with.
fn read_group(chars: &[char], start: usize) -> (String, usize) {
    let mut depth = 0i32;
    let mut i = start;
    let mut s = String::new();

    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => {
                depth += 1;
                if depth > 1 {
                    s.push('{');
                }
                i += 1;
            }
            '}' => {
                depth -= 1;
                i += 1;
                if depth <= 0 {
                    return (s, i);
                }
                s.push('}');
            }
            '\\' => {
                if i + 1 < chars.len() {
                    let n = chars[i + 1];
                    if matches!(n, '%' | '{' | '}' | '&' | '$' | '#' | '_' | '|' | ' ' | '\\' | ',' | ';' | ':') {
                        s.push(n);
                    } else {
                        s.push('\\');
                        s.push(n);
                    }
                    i += 2;
                } else {
                    s.push('\\');
                    i += 1;
                }
            }
            _ => {
                s.push(c);
                i += 1;
            }
        }
    }

    (s, i)
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

/// TeX atom class, used only to derive inter-atom spacing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// An ordinary symbol or a group.
    Ord,
    /// A big operator or a function name.
    Op,
    /// A binary operator such as `+` or `\times`.
    Bin,
    /// A relation such as `=` or `\leq`.
    Rel,
    /// An opening delimiter.
    Open,
    /// A closing delimiter.
    Close,
    /// Punctuation: `,` `;`.
    Punct,
    /// A delimited sub-formula (`\left(...\right)`).
    Inner,
}

/// Resolved font request for one symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SymStyle {
    pub family: Family,
    pub weight: Weight,
    pub slant: Slant,
}

impl Default for SymStyle {
    fn default() -> Self {
        Self {
            family: Family::Math,
            weight: Weight::Regular,
            slant: Slant::Upright,
        }
    }
}

/// Face used by [`Node::Text`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextStyle {
    Sans,
    Serif,
    Mono,
}

/// Mathematical alphabet selected by `\mathrm`, `\mathbf`, `\mathbb`, ...
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MathStyle {
    Roman,
    Bold,
    Italic,
    Sans,
    Mono,
    Blackboard,
    Script,
    Fraktur,
}

/// Size switch: `\displaystyle`, `\textstyle`, ...
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeStyle {
    Display,
    Text,
    Script,
    ScriptScript,
}

/// Sizing of a fraction's operands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FracStyle {
    /// `\frac`: text-style in display math, script-style otherwise.
    Auto,
    /// `\dfrac`: always text-style.
    Display,
    /// `\tfrac`: always script-style.
    Script,
}

/// Whether an operator accepts limits above/below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LimitMode {
    /// Limits when the surrounding style is display and the operator allows it.
    Auto,
    /// Forced by `\limits`.
    Limits,
    /// Forced by `\nolimits`.
    NoLimits,
}

/// Operator flavour, deciding both glyph size and limit eligibility.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    /// `\sum \bigcup ...`: drawn large in display style, limits allowed.
    Large,
    /// `\int \oint ...`: drawn large in display style, but limits sit beside
    /// the symbol unless `\limits` asks otherwise (TeX's `\nolimits` default
    /// for integrals).
    Integral,
    /// `\lim \sup \max`: an upright name that takes limits in display style.
    Limit,
    /// `\sin \log \det`: upright name, scripts always to the side.
    Func,
}

/// Fixed-size delimiter prefix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BigSize {
    One,
    Two,
    Three,
    Four,
}

impl BigSize {
    /// Nominal delimiter height as a multiple of the em.
    pub fn em(self) -> f32 {
        match self {
            Self::One => 1.2,
            Self::Two => 1.6,
            Self::Three => 2.2,
            Self::Four => 3.0,
        }
    }
}

/// A delimiter, with its side already resolved (`(` and `)` are distinct).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Delim {
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    /// `|`
    Pipe,
    /// `\|`
    DPipe,
    LAngle,
    RAngle,
    LFloor,
    RFloor,
    LCeil,
    RCeil,
    Slash,
    Backslash,
    /// `.`: renders nothing, used for one-sided `\left.` / `\right.`.
    Dot,
}

/// Accent placed above (or, for [`AccentKind::Underline`], below) its base.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccentKind {
    Hat,
    WideHat,
    Bar,
    Overline,
    Tilde,
    WideTilde,
    Dot,
    Ddot,
    Vec,
    Check,
    Breve,
    Acute,
    Grave,
    Ring,
    Underline,
}

/// Column alignment inside an `array` / `aligned`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColAlign {
    Left,
    Center,
    Right,
}

/// Default space between two columns of an `array`, in em.
pub const DEFAULT_COL_GAP: f32 = 0.6;

/// `array` column specification, e.g. `{c|lr}`.
#[derive(Clone, Debug, PartialEq)]
pub struct ArraySpec {
    pub aligns: Vec<ColAlign>,
    /// `vlines[i]` is `true` when a vertical rule sits before column `i`;
    /// index `aligns.len()` is the rule after the last column.
    pub vlines: Vec<bool>,
    /// `gaps[i]` overrides [`DEFAULT_COL_GAP`] for the space before column `i`
    /// (`@{}` sets it to zero). Index `aligns.len()` is the trailing space.
    pub gaps: Vec<Option<f32>>,
    /// Minimum column widths in points, from `p{..}` / `m{..}` / `b{..}`.
    pub min_widths: Vec<f32>,
}

impl Default for ArraySpec {
    fn default() -> Self {
        Self {
            aligns: Vec::new(),
            vlines: vec![false],
            gaps: Vec::new(),
            min_widths: Vec::new(),
        }
    }
}

impl ArraySpec {
    fn push_col(&mut self, align: ColAlign, min_width: f32) {
        self.aligns.push(align);
        self.vlines.push(false);
        if self.gaps.len() < self.aligns.len() {
            self.gaps.push(None);
        }
        self.min_widths.push(min_width);
    }
}

/// Supported `\begin{...}` families.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvKind {
    Matrix,
    ParenMatrix,
    BracketMatrix,
    BarMatrix,
    BraceMatrix,
    Cases,
    Aligned,
    Align,
    Gathered,
    Array,
}

impl EnvKind {
    /// Map an environment name to a kind. `None` means "unknown".
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            // `smallmatrix` is typographically smaller, but the row/column
            // model is the same, so it shares the matrix path.
            "matrix" | "smallmatrix" | "matrix*" => Self::Matrix,
            "pmatrix" | "pmatrix*" => Self::ParenMatrix,
            "bmatrix" | "bmatrix*" => Self::BracketMatrix,
            "vmatrix" | "vmatrix*" => Self::BarMatrix,
            "Bmatrix" | "Bmatrix*" => Self::BraceMatrix,
            "cases" | "dcases" | "rcases" | "cases*" => Self::Cases,
            // `split`, `alignedat` and friends all alternate columns around the
            // alignment point, which is exactly the `aligned` model.
            "aligned" | "split" | "alignedat" | "alignat" | "alignat*" => Self::Aligned,
            "align" | "align*" | "eqnarray" | "eqnarray*" | "flalign" | "flalign*" => Self::Align,
            "gathered" | "gather" | "gather*" | "multline" | "multline*" => Self::Gathered,
            "array" => Self::Array,
            _ => return None,
        })
    }

    /// `true` for the kinds whose columns pair up on the `&` marks.
    pub fn alternates_columns(self) -> bool {
        matches!(self, Self::Aligned | Self::Align)
    }
}

/// One node of the math tree.
#[derive(Clone, Debug)]
pub enum Node {
    /// A horizontal sequence, baselines aligned.
    Row(Vec<Node>),
    /// A literal run of characters plus its resolved font and atom class.
    Sym {
        text: String,
        class: Class,
        style: SymStyle,
        /// Set by the `\mathbb` fallback: draw a rule under the glyph.
        underline: bool,
    },
    /// Horizontal space, in em of the current size. Negative values are legal
    /// (that is what `\!` is).
    Space(f32),
    /// Text-mode content.
    Text {
        body: String,
        style: TextStyle,
        weight: Weight,
        class: Class,
    },
    /// A style scope. Purely structural at layout time: the fonts of the
    /// contained [`Node::Sym`]s were already resolved while parsing.
    Style {
        kind: MathStyle,
        body: Box<Node>,
    },
    /// `\displaystyle` & friends; affects the rest of the enclosing row.
    StyleSwitch(SizeStyle),
    Frac {
        num: Box<Node>,
        den: Box<Node>,
        frac: FracStyle,
    },
    Binom {
        num: Box<Node>,
        den: Box<Node>,
    },
    Sqrt {
        index: Option<Box<Node>>,
        body: Box<Node>,
    },
    Accent {
        kind: AccentKind,
        base: Box<Node>,
    },
    Delimited {
        left: Delim,
        right: Delim,
        body: Box<Node>,
    },
    BigDelim {
        delim: Delim,
        size: BigSize,
        class: Class,
    },
    Scripts {
        base: Box<Node>,
        sup: Option<Box<Node>>,
        sub: Option<Box<Node>>,
        limits: LimitMode,
    },
    Op {
        text: String,
        kind: OpKind,
        limits: LimitMode,
    },
    Environ {
        kind: EnvKind,
        rows: Vec<Vec<Node>>,
        spec: Option<ArraySpec>,
        /// Extra vertical space (in points) after row `i`, from `\\[dim]`.
        gaps: Vec<f32>,
    },
    /// `\not`: a negation slash drawn across the base atom.
    Negate { base: Box<Node> },
    /// `\overset` / `\underset` / `\stackrel`.
    Stack {
        base: Box<Node>,
        top: Option<Box<Node>>,
        bottom: Option<Box<Node>>,
        class: Class,
    },
    /// `\overbrace` / `\underbrace`; scripts attach to the brace.
    Brace { over: bool, body: Box<Node> },
    /// `\boxed` / `\fbox`.
    Boxed { body: Box<Node> },
    /// `\phantom` and friends: occupy space, draw nothing.
    Phantom { body: Box<Node>, kind: PhantomKind },
    /// `\mathbin{...}` and friends: force an atom class.
    Classed { class: Class, body: Box<Node> },
    /// `\textcolor{...}{...}`: colour scope that ends with the node.
    Color { color: Rgba, body: Box<Node> },
    /// `\color{...}`: colour switch for the rest of the enclosing group.
    ColorSwitch(Rgba),
    /// `\hline` inside an environment.
    Rule,
    /// Text the parser could not interpret, kept verbatim as `\foo`.
    Raw(String),
    /// `\\` outside an environment; laid out as a space.
    LineBreak,
}

/// Which parts of a phantom to keep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhantomKind {
    /// `\phantom`: both size and depth.
    Full,
    /// `\hphantom`: width only.
    Horizontal,
    /// `\vphantom`: height and depth only.
    Vertical,
}

/// Result of [`parse`].
#[derive(Clone, Debug)]
pub struct ParseOutput {
    pub root: Node,
    /// Non-fatal problems, in source order.
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Which mathematical alphabet is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SpecialFont {
    None,
    DoubleStruck,
    Script,
    Fraktur,
}

/// The font state a newly created symbol inherits.
#[derive(Clone, Copy, Debug)]
struct FontState {
    family: Family,
    weight: Weight,
    slant: Option<Slant>,
    upright_letters: bool,
    special: SpecialFont,
}

impl Default for FontState {
    fn default() -> Self {
        Self {
            family: Family::Math,
            weight: Weight::Regular,
            slant: None,
            upright_letters: false,
            special: SpecialFont::None,
        }
    }
}

/// A symbol pulled from the built-in name tables.
struct NamedSym {
    text: &'static str,
    class: Class,
    italic: bool,
}

struct P {
    t: Vec<Token>,
    i: usize,
    warnings: Vec<String>,
    depth: u32,
    font: FontState,
}

/// Parse a token stream into an AST. Never fails.
pub fn parse(tokens: Vec<Token>) -> ParseOutput {
    let mut p = P {
        t: tokens,
        i: 0,
        warnings: Vec::new(),
        depth: 0,
        font: FontState::default(),
    };
    let root = p.parse_document();
    if p.i < p.t.len() {
        p.warn("末尾多余的记号已被忽略");
    }
    ParseOutput {
        root,
        warnings: p.warnings,
    }
}

impl P {
    // -- plumbing -----------------------------------------------------------

    fn next(&mut self) -> Option<Token> {
        let tok = self.t.get(self.i)?.clone();
        self.i += 1;
        Some(tok)
    }

    fn peek(&self) -> Option<&Token> {
        self.t.get(self.i)
    }

    fn warn(&mut self, msg: &str) {
        if self.warnings.iter().any(|w| w == msg) {
            return;
        }
        self.warnings.push(msg.to_owned());
    }

    /// Consume the next token when it is a raw [`Token::Group`].
    fn take_group(&mut self) -> Option<String> {
        if !matches!(self.peek(), Some(Token::Group(_))) {
            return None;
        }
        match self.next() {
            Some(Token::Group(g)) => Some(g),
            _ => None,
        }
    }

    // -- document -----------------------------------------------------------

    fn parse_document(&mut self) -> Node {
        let mut items: Vec<Node> = Vec::new();
        loop {
            items.push(self.parse_row());
            if matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "\\") {
                self.next();
                items.push(Node::LineBreak);
            } else {
                break;
            }
        }
        collapse(items)
    }

    /// Parse until a terminator (`}`, `&`, `\\`, `\end`, `\right`) or EOF.
    fn parse_row(&mut self) -> Node {
        if self.depth > MAX_DEPTH {
            self.warn("表达式嵌套层数过多，已截断");
            return Node::Row(Vec::new());
        }

        let mut items: Vec<Node> = Vec::new();
        loop {
            let terminated = matches!(
                self.peek(),
                None | Some(Token::RBrace) | Some(Token::Amp)
            );
            if terminated {
                break;
            }
            let stops_command = matches!(
                self.peek(),
                Some(Token::Command(c)) if c.as_str() == "\\" || c.as_str() == "end" || c.as_str() == "right"
            );
            if stops_command {
                break;
            }
            if matches!(self.peek(), Some(Token::Sup) | Some(Token::Sub)) {
                let is_sup = matches!(self.peek(), Some(Token::Sup));
                self.next();
                self.warn("上下标缺少主体");
                let arg = self.parse_script_arg();
                let (sup, sub) = if is_sup {
                    (Some(Box::new(arg)), None)
                } else {
                    (None, Some(Box::new(arg)))
                };
                items.push(Node::Scripts {
                    base: Box::new(Node::Row(Vec::new())),
                    sup,
                    sub,
                    limits: LimitMode::Auto,
                });
                continue;
            }
            let atom = self.parse_atom();
            items.push(self.parse_scripts(atom));
        }
        collapse(items)
    }

    /// Attach any `^`, `_`, `\limits`, `\nolimits` or `'` following `base`.
    fn parse_scripts(&mut self, base: Node) -> Node {
        let mut sup: Option<Box<Node>> = None;
        let mut sub: Option<Box<Node>> = None;
        let mut limits = LimitMode::Auto;
        let mut touched = false;

        loop {
            if matches!(self.peek(), Some(Token::Sup)) {
                self.next();
                touched = true;
                let arg = self.parse_script_arg();
                if sup.is_some() {
                    self.warn("重复的上标，后一个覆盖前一个");
                }
                sup = Some(Box::new(arg));
            } else if matches!(self.peek(), Some(Token::Sub)) {
                self.next();
                touched = true;
                let arg = self.parse_script_arg();
                if sub.is_some() {
                    self.warn("重复的下标，后一个覆盖前一个");
                }
                sub = Some(Box::new(arg));
            } else if matches!(self.peek(), Some(Token::Char('\''))) {
                let mut primes = String::new();
                while matches!(self.peek(), Some(Token::Char('\''))) {
                    self.next();
                    primes.push('\u{2032}');
                }
                touched = true;
                if sup.is_some() {
                    self.warn("重复的上标，后一个覆盖前一个");
                }
                sup = Some(Box::new(self.math_sym(&primes, Class::Ord, false)));
            } else if matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "limits") {
                self.next();
                limits = LimitMode::Limits;
                touched = true;
            } else if matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "nolimits") {
                self.next();
                limits = LimitMode::NoLimits;
                touched = true;
            } else {
                break;
            }
        }

        if touched {
            Node::Scripts {
                base: Box::new(base),
                sup,
                sub,
                limits,
            }
        } else {
            base
        }
    }

    fn parse_script_arg(&mut self) -> Node {
        if matches!(self.peek(), Some(Token::LBrace)) {
            self.next();
            self.parse_braced_body()
        } else if self.peek().is_none() {
            self.warn("上下标缺少内容");
            Node::Row(Vec::new())
        } else {
            self.parse_atom()
        }
    }

    /// Same as [`P::parse_script_arg`], for commands that always want a body.
    fn parse_arg(&mut self) -> Node {
        if matches!(self.peek(), Some(Token::LBrace)) {
            self.next();
            self.parse_braced_body()
        } else if self.peek().is_none() {
            self.warn("命令缺少参数");
            Node::Row(Vec::new())
        } else {
            self.parse_atom()
        }
    }

    /// Parse the inside of a brace group; the opening `{` must already be
    /// consumed.
    fn parse_braced_body(&mut self) -> Node {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.warn("表达式嵌套层数过多，已忽略该层");
            self.skip_balanced();
            self.depth -= 1;
            return Node::Row(Vec::new());
        }
        let inner = self.parse_row();
        if matches!(self.peek(), Some(Token::RBrace)) {
            self.next();
        } else {
            self.warn("缺少匹配的 '}'");
        }
        self.depth -= 1;
        inner
    }

    /// Consume tokens until the brace group opened by the caller is balanced.
    fn skip_balanced(&mut self) {
        let mut depth = 1i32;
        while depth > 0 {
            match self.next() {
                None => break,
                Some(Token::LBrace) => depth += 1,
                Some(Token::RBrace) => depth -= 1,
                Some(_) => {}
            }
        }
    }

    // -- atoms --------------------------------------------------------------

    fn parse_atom(&mut self) -> Node {
        let tok = match self.next() {
            Some(t) => t,
            None => return Node::Row(Vec::new()),
        };
        match tok {
            Token::LBrace => self.parse_braced_body(),
            Token::Char(c) if c.is_ascii_alphabetic() => self.letter_run(c),
            Token::Char(c) if c.is_ascii_digit() || c == '.' => self.digit_run(c),
            Token::Char(c) => self.char_symbol(c),
            Token::Command(name) => self.parse_command(&name),
            Token::Group(g) => Node::Text {
                body: g,
                style: TextStyle::Sans,
                weight: Weight::Regular,
                class: Class::Ord,
            },
            Token::Sup | Token::Sub => {
                self.warn("上下标缺少主体");
                Node::Row(Vec::new())
            }
            Token::Amp => {
                self.warn("'&' 出现在环境之外");
                Node::Raw("&".to_owned())
            }
            Token::RBrace => {
                self.warn("多余的 '}'");
                Node::Raw("}".to_owned())
            }
        }
    }

    fn letter_run(&mut self, first: char) -> Node {
        let mut run = String::new();
        run.push(first);
        loop {
            if !matches!(self.peek(), Some(Token::Char(c)) if c.is_ascii_alphabetic()) {
                break;
            }
            if let Some(Token::Char(c)) = self.next() {
                run.push(c);
            }
        }

        if self.font.special != SpecialFont::None {
            let mut items: Vec<Node> = Vec::with_capacity(run.len());
            for ch in run.chars() {
                items.push(self.styled_letter(ch));
            }
            return collapse(items);
        }

        let italic = run.chars().count() == 1;
        self.math_sym(&run, Class::Ord, italic)
    }

    fn digit_run(&mut self, first: char) -> Node {
        let mut run = String::new();
        run.push(first);
        loop {
            let is_digit = matches!(self.peek(), Some(Token::Char(c)) if c.is_ascii_digit() || *c == '.');
            if !is_digit {
                break;
            }
            if let Some(Token::Char(c)) = self.next() {
                run.push(c);
            }
        }
        self.math_sym(&run, Class::Ord, false)
    }

    fn char_symbol(&mut self, c: char) -> Node {
        let (text, class): (&str, Class) = match c {
            '+' => ("+", Class::Bin),
            '-' => ("\u{2212}", Class::Bin),
            '=' => ("=", Class::Rel),
            '<' => ("<", Class::Rel),
            '>' => (">", Class::Rel),
            '(' => ("(", Class::Open),
            ')' => (")", Class::Close),
            '[' => ("[", Class::Open),
            ']' => ("]", Class::Close),
            '|' => ("|", Class::Ord),
            '/' => ("/", Class::Ord),
            '*' => ("\u{2217}", Class::Bin),
            ',' => (",", Class::Punct),
            ';' => (";", Class::Punct),
            ':' => (":", Class::Rel),
            '!' => ("!", Class::Close),
            '?' => ("?", Class::Close),
            '"' => ("\u{2033}", Class::Ord),
            '\'' => ("\u{2032}", Class::Ord),
            '~' => return Node::Space(0.3333),
            _ => {
                let owned = c.to_string();
                return self.math_sym(&owned, Class::Ord, false);
            }
        };
        self.math_sym(text, class, false)
    }

    /// Build a symbol with the current font state.
    fn math_sym(&self, text: &str, class: Class, italic: bool) -> Node {
        let slant = self.font.slant.unwrap_or(if italic {
            Slant::Italic
        } else {
            Slant::Upright
        });
        Node::Sym {
            text: text.to_owned(),
            class,
            style: SymStyle {
                family: self.font.family,
                weight: self.font.weight,
                slant,
            },
            underline: false,
        }
    }

    /// Letter inside `\mathbb` / `\mathcal` / `\mathfrak`, mapped to a Unicode
    /// mathematical alphanumeric where one exists.
    fn styled_letter(&mut self, c: char) -> Node {
        let (mapped, label) = match self.font.special {
            SpecialFont::DoubleStruck => (double_struck(c), "\\mathbb"),
            SpecialFont::Script => (script_letter(c), "\\mathcal"),
            SpecialFont::Fraktur => (fraktur_letter(c), "\\mathfrak"),
            SpecialFont::None => (Some(c), ""),
        };
        let (text, underline) = match mapped {
            Some(m) => (m.to_string(), false),
            None => {
                self.warn(&format!(
                    "{label} 对字符 '{c}' 没有对应字形，已改用下划线形式"
                ));
                (c.to_string(), true)
            }
        };
        Node::Sym {
            text,
            class: Class::Ord,
            style: SymStyle {
                family: Family::Math,
                weight: self.font.weight,
                slant: Slant::Upright,
            },
            underline,
        }
    }

    // -- commands -----------------------------------------------------------

    fn parse_command(&mut self, name: &str) -> Node {
        if let Some(node) = self.escape_or_space(name) {
            return node;
        }
        if let Some((text, kind)) = op_symbol(name) {
            let limits = if kind == OpKind::Func {
                LimitMode::NoLimits
            } else {
                LimitMode::Auto
            };
            return Node::Op {
                text: text.to_owned(),
                kind,
                limits,
            };
        }
        if let Some(ns) = named_symbol(name) {
            return self.math_sym(ns.text, ns.class, ns.italic);
        }
        if name == "mathbb" || name == "mathcal" || name == "mathscr" || name == "mathfrak" {
            let kind = match name {
                "mathbb" => MathStyle::Blackboard,
                "mathfrak" => MathStyle::Fraktur,
                _ => MathStyle::Script,
            };
            return self.style_wrap(kind);
        }
        if let Some(kind) = accent_kind(name) {
            let base = self.parse_arg();
            return Node::Accent {
                kind,
                base: Box::new(base),
            };
        }
        if let Some((size, class)) = big_prefix(name) {
            return self.big_delim(size, class);
        }

        match name {
            "frac" => self.frac_node(FracStyle::Auto),
            "dfrac" => self.frac_node(FracStyle::Display),
            "tfrac" => self.frac_node(FracStyle::Script),
            "binom" => {
                let num = self.parse_arg();
                let den = self.parse_arg();
                Node::Binom {
                    num: Box::new(num),
                    den: Box::new(den),
                }
            }
            "sqrt" => self.sqrt_node(),
            "left" => self.left_right(),
            "right" => {
                self.warn("\\right 没有匹配的 \\left");
                Node::Raw("\\right".to_owned())
            }
            "begin" => self.environment(),
            "end" => {
                self.warn("\\end 没有匹配的 \\begin");
                Node::Raw("\\end".to_owned())
            }
            "text" => self.text_node(TextStyle::Sans, Weight::Regular, Class::Ord),
            "textrm" => self.text_node(TextStyle::Serif, Weight::Regular, Class::Ord),
            "textsf" => self.text_node(TextStyle::Sans, Weight::Regular, Class::Ord),
            "texttt" => self.text_node(TextStyle::Mono, Weight::Regular, Class::Ord),
            "textnormal" | "textup" => self.text_node(TextStyle::Serif, Weight::Regular, Class::Ord),
            "textmd" => self.text_node(TextStyle::Serif, Weight::Regular, Class::Ord),
            "textbf" => self.text_node(TextStyle::Serif, Weight::Bold, Class::Ord),
            "textit" | "textsl" => self.text_node(TextStyle::Serif, Weight::Regular, Class::Ord),
            "mbox" => self.text_node(TextStyle::Serif, Weight::Regular, Class::Ord),
            "operatorname" => self.text_node(TextStyle::Serif, Weight::Regular, Class::Op),
            "operatorname*" => {
                // Limit-taking operator name: laid out as an upright op so that
                // scripts stack in display style, exactly like `\lim`.
                if let Some(body) = self.take_group() {
                    Node::Op {
                        text: body,
                        kind: OpKind::Limit,
                        limits: LimitMode::Auto,
                    }
                } else {
                    self.warn("\\operatorname* 缺少 {...} 参数");
                    Node::Raw("\\operatorname*".to_owned())
                }
            }
            "mathrm" => self.style_wrap(MathStyle::Roman),
            "mathbf" => self.style_wrap(MathStyle::Bold),
            "boldsymbol" | "bm" | "pmb" => self.style_wrap(MathStyle::Bold),
            "mathit" => self.style_wrap(MathStyle::Italic),
            "mathsf" => self.style_wrap(MathStyle::Sans),
            "mathtt" => self.style_wrap(MathStyle::Mono),
            "not" => {
                let base = self.parse_atom();
                Node::Negate {
                    base: Box::new(base),
                }
            }
            "overset" => {
                let top = self.parse_arg();
                let base = self.parse_arg();
                Node::Stack {
                    base: Box::new(base),
                    top: Some(Box::new(top)),
                    bottom: None,
                    class: Class::Ord,
                }
            }
            "underset" => {
                let bottom = self.parse_arg();
                let base = self.parse_arg();
                Node::Stack {
                    base: Box::new(base),
                    top: None,
                    bottom: Some(Box::new(bottom)),
                    class: Class::Ord,
                }
            }
            "stackrel" => {
                let top = self.parse_arg();
                let base = self.parse_arg();
                Node::Stack {
                    base: Box::new(base),
                    top: Some(Box::new(top)),
                    bottom: None,
                    class: Class::Rel,
                }
            }
            "overbrace" => Node::Brace {
                over: true,
                body: Box::new(self.parse_arg()),
            },
            "underbrace" => Node::Brace {
                over: false,
                body: Box::new(self.parse_arg()),
            },
            "boxed" | "fbox" => Node::Boxed {
                body: Box::new(self.parse_arg()),
            },
            "phantom" => Node::Phantom {
                body: Box::new(self.parse_arg()),
                kind: PhantomKind::Full,
            },
            "hphantom" => Node::Phantom {
                body: Box::new(self.parse_arg()),
                kind: PhantomKind::Horizontal,
            },
            "vphantom" => Node::Phantom {
                body: Box::new(self.parse_arg()),
                kind: PhantomKind::Vertical,
            },
            "substack" => self.substack(),
            "mathord" => self.classed(Class::Ord),
            "mathop" => self.classed(Class::Op),
            "mathbin" => self.classed(Class::Bin),
            "mathrel" => self.classed(Class::Rel),
            "mathopen" => self.classed(Class::Open),
            "mathclose" => self.classed(Class::Close),
            "mathpunct" => self.classed(Class::Punct),
            "mathinner" => self.classed(Class::Inner),
            "bmod" | "mod" => Node::Text {
                body: "mod".to_owned(),
                style: TextStyle::Serif,
                weight: Weight::Regular,
                class: Class::Bin,
            },
            "pmod" => {
                let body = self.parse_arg();
                let inner = Node::Row(vec![
                    Node::Text {
                        body: "mod".to_owned(),
                        style: TextStyle::Serif,
                        weight: Weight::Regular,
                        class: Class::Ord,
                    },
                    Node::Space(0.1667),
                    body,
                ]);
                Node::Delimited {
                    left: Delim::LParen,
                    right: Delim::RParen,
                    body: Box::new(inner),
                }
            }
            "color" => {
                let color = self.color_arg();
                Node::ColorSwitch(color)
            }
            "textcolor" => {
                let color = self.color_arg();
                let body = self.parse_arg();
                Node::Color {
                    color,
                    body: Box::new(body),
                }
            }
            "hspace" => self.hspace_node(),
            "mkern" | "mskip" => Node::Space(self.dimension_arg().unwrap_or(0.0)),
            "displaystyle" => Node::StyleSwitch(SizeStyle::Display),
            "textstyle" => Node::StyleSwitch(SizeStyle::Text),
            "scriptstyle" => Node::StyleSwitch(SizeStyle::Script),
            "scriptscriptstyle" => Node::StyleSwitch(SizeStyle::ScriptScript),
            "limits" | "nolimits" => {
                self.warn("\\limits/\\nolimits 之前没有运算符");
                Node::Row(Vec::new())
            }
            "hline" => {
                self.warn("\\hline 只能出现在环境里");
                Node::Row(Vec::new())
            }
            "label" | "tag" | "nonumber" | "notag" | "displaybreak" => {
                // Reconciliation-only markers: the editor renders neither
                // equation numbers nor cross-references, so they are dropped
                // instead of being echoed as unknown source.
                let _ = self.take_group();
                Node::Row(Vec::new())
            }
            other => {
                let rendered = if other.is_empty() {
                    "\\".to_owned()
                } else {
                    format!("\\{other}")
                };
                self.warn(&format!("未知命令 {rendered}，已按原文显示"));
                Node::Raw(rendered)
            }
        }
    }

    /// Escaped literals (`\{`), spacing commands (`\,`) and control space.
    fn escape_or_space(&mut self, name: &str) -> Option<Node> {
        let node = match name {
            "{" => self.math_sym("{", Class::Ord, false),
            "}" => self.math_sym("}", Class::Ord, false),
            "%" => self.math_sym("%", Class::Ord, false),
            "$" => self.math_sym("$", Class::Ord, false),
            "&" => self.math_sym("&", Class::Ord, false),
            "#" => self.math_sym("#", Class::Ord, false),
            "_" => self.math_sym("_", Class::Ord, false),
            "|" => self.math_sym("\u{2016}", Class::Ord, false),
            "backslash" => self.math_sym("\\", Class::Ord, false),
            "lbrace" => self.math_sym("{", Class::Ord, false),
            "rbrace" => self.math_sym("}", Class::Ord, false),
            "," | "thinspace" => Node::Space(0.1667),
            ":" | "medspace" => Node::Space(0.2222),
            ";" | "thickspace" => Node::Space(0.2778),
            " " | "\u{a0}" => Node::Space(0.3333),
            "!" | "negthinspace" => Node::Space(-0.1667),
            "negmedspace" => Node::Space(-0.2222),
            "negthickspace" => Node::Space(-0.2778),
            "quad" => Node::Space(1.0),
            "qquad" => Node::Space(2.0),
            "enspace" | "enskip" => Node::Space(0.5),
            // Nothing in this engine stretches, so `\hfill` and `\hfil` are
            // emulated with one quad of fixed space.
            "hfill" | "hfil" => Node::Space(1.0),
            "nobreakspace" => Node::Space(0.3333),
            "allowbreak" | "nobreak" | "relax" => Node::Space(0.0),
            _ => return None,
        };
        Some(node)
    }

    fn frac_node(&mut self, frac: FracStyle) -> Node {
        let num = self.parse_arg();
        let den = self.parse_arg();
        Node::Frac {
            num: Box::new(num),
            den: Box::new(den),
            frac,
        }
    }

    fn sqrt_node(&mut self) -> Node {
        let index = if matches!(self.peek(), Some(Token::Char('['))) {
            self.next();
            let mut items: Vec<Node> = Vec::new();
            loop {
                if self.peek().is_none() {
                    self.warn("\\sqrt 的根指数缺少 ']'");
                    break;
                }
                if matches!(self.peek(), Some(Token::Char(']'))) {
                    self.next();
                    break;
                }
                let atom = self.parse_atom();
                items.push(self.parse_scripts(atom));
            }
            Some(Box::new(collapse(items)))
        } else {
            None
        };
        let body = self.parse_arg();
        Node::Sqrt {
            index,
            body: Box::new(body),
        }
    }

    fn text_node(&mut self, style: TextStyle, weight: Weight, class: Class) -> Node {
        if let Some(body) = self.take_group() {
            return Node::Text {
                body,
                style,
                weight,
                class,
            };
        }
        self.warn("文本命令缺少 {...} 参数");
        Node::Raw("\\text".to_owned())
    }

    /// `\mathop{...}` and friends: wrap a body with a forced atom class.
    fn classed(&mut self, class: Class) -> Node {
        Node::Classed {
            class,
            body: Box::new(self.parse_arg()),
        }
    }

    /// `\substack{a \\ b}`: a `gathered` environment built from the token
    /// stream, since its rows are math rather than verbatim text.
    fn substack(&mut self) -> Node {
        if !matches!(self.peek(), Some(Token::LBrace)) {
            self.warn("\\substack 缺少 {...} 参数");
            return Node::Row(Vec::new());
        }
        self.next();
        self.depth += 1;
        let mut rows: Vec<Vec<Node>> = Vec::new();
        let mut gaps: Vec<f32> = Vec::new();
        loop {
            if self.peek().is_none() {
                self.warn("\\substack 缺少 '}'");
                break;
            }
            if matches!(self.peek(), Some(Token::RBrace)) {
                self.next();
                break;
            }
            rows.push(vec![self.parse_row()]);
            match self.peek() {
                Some(Token::Command(c)) if c.as_str() == "\\" => {
                    self.next();
                    gaps.push(self.read_row_gap());
                }
                Some(Token::Amp) => {
                    // `&` inside a substack is meaningless; treat it as a break.
                    self.next();
                    gaps.push(0.0);
                }
                Some(Token::RBrace) => {
                    self.next();
                    gaps.push(0.0);
                    break;
                }
                None => {
                    self.warn("\\substack 缺少 '}'");
                    gaps.push(0.0);
                    break;
                }
                Some(_) => {
                    // A stray `\end` or `\right` would otherwise spin forever.
                    self.next();
                    gaps.push(0.0);
                }
            }
        }
        self.depth -= 1;
        Node::Environ {
            kind: EnvKind::Gathered,
            rows,
            spec: None,
            gaps,
        }
    }

    /// Read the colour name/`#rrggbb` argument of `\color` / `\textcolor`.
    fn color_arg(&mut self) -> Rgba {
        match self.take_group() {
            Some(name) => match named_color(&name) {
                Some(c) => c,
                None => {
                    self.warn(&format!("未知颜色 '{name}'，已按当前文字颜色绘制"));
                    Rgba::BLACK
                }
            },
            None => {
                self.warn("\\color 缺少颜色参数");
                Rgba::BLACK
            }
        }
    }

    /// A braced or bare dimension, in em: `\mkern{2mu}`, `\mkern2mu`,
    /// `\hspace{1em}`, `\hspace*{1em}`.
    fn dimension_arg(&mut self) -> Option<f32> {
        if let Some(text) = self.take_group() {
            return Some(parse_dimension(&text));
        }
        // A group that the lexer did not capture verbatim (it only does that for
        // a fixed list of commands, and `\hspace*` puts a star in between).
        if matches!(self.peek(), Some(Token::LBrace)) {
            return self.read_raw_arg().map(|text| parse_dimension(&text));
        }
        // Bare form: a number followed by a unit, as TeX allows after `\mkern`.
        let mut number = String::new();
        while let Some(Token::Char(c)) = self.peek() {
            let c = *c;
            if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' {
                number.push(c);
                self.next();
            } else {
                break;
            }
        }
        let mut unit = String::new();
        while let Some(Token::Char(c)) = self.peek() {
            if c.is_ascii_alphabetic() && unit.len() < 2 {
                unit.push(*c);
                self.next();
            } else {
                break;
            }
        }
        if number.is_empty() {
            self.warn("长度命令缺少数值");
            return None;
        }
        if !matches!(
            unit.as_str(),
            "mu" | "em" | "ex" | "pt" | "cm" | "mm" | "in" | "pc" | "bp" | "sp" | "dd" | "cc"
        ) {
            // Put the letters back: `\mkern2x` is malformed, and eating the `x`
            // would silently drop a symbol from the formula.
            self.i -= unit.chars().count();
            self.warn(&format!("长度缺少单位（读到 '{unit}'），已按 em 处理"));
            return Some(parse_dimension(&number));
        }
        Some(parse_dimension(&format!("{number}{unit}")))
    }

    /// Collect raw characters up to the matching `}`, including the braces, in
    /// the same shape [`Token::Group`] would have had.
    fn read_raw_arg(&mut self) -> Option<String> {
        while let Some(Token::Char(c)) = self.peek() {
            if c.is_whitespace() {
                self.next();
            } else {
                break;
            }
        }
        if !matches!(self.peek(), Some(Token::LBrace)) {
            return None;
        }
        self.next();
        let mut depth = 1i32;
        let mut s = String::new();
        while let Some(tok) = self.next() {
            match tok {
                Token::LBrace => {
                    depth += 1;
                    s.push('{');
                }
                Token::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(s);
                    }
                    s.push('}');
                }
                Token::Char(c) => s.push(c),
                Token::Command(c) => {
                    s.push('\\');
                    s.push_str(&c);
                }
                Token::Group(g) => s.push_str(&g),
                Token::Amp => s.push('&'),
                Token::Sup => s.push('^'),
                Token::Sub => s.push('_'),
            }
        }
        self.warn("缺少匹配的 '}'");
        Some(s)
    }

    /// Optional `[dim]` after `\\`, giving the row extra vertical space. The
    /// value is in points so an absolute `\\[6pt]` means what it says.
    fn read_row_gap(&mut self) -> f32 {
        if !matches!(self.peek(), Some(Token::Char('['))) {
            return 0.0;
        }
        self.next();
        let mut dim = String::new();
        while let Some(tok) = self.peek() {
            match tok {
                Token::Char(']') => {
                    self.next();
                    return parse_length_pt(&dim);
                }
                Token::Char(c) => {
                    dim.push(*c);
                    self.next();
                }
                _ => break,
            }
        }
        self.warn("\\\\[dim] 缺少 ']'");
        0.0
    }

    fn style_wrap(&mut self, kind: MathStyle) -> Node {
        let saved = self.font;
        self.apply_style(kind);
        let body = self.parse_arg();
        self.font = saved;
        Node::Style {
            kind,
            body: Box::new(body),
        }
    }

    fn apply_style(&mut self, kind: MathStyle) {
        match kind {
            MathStyle::Roman => {
                self.font.family = Family::Serif;
                self.font.weight = Weight::Regular;
                self.font.slant = Some(Slant::Upright);
                self.font.upright_letters = true;
                self.font.special = SpecialFont::None;
            }
            MathStyle::Bold => {
                self.font.family = Family::Serif;
                self.font.weight = Weight::Bold;
                self.font.slant = Some(Slant::Upright);
                self.font.upright_letters = true;
                self.font.special = SpecialFont::None;
            }
            MathStyle::Italic => {
                self.font.family = Family::Serif;
                self.font.weight = Weight::Regular;
                self.font.slant = Some(Slant::Italic);
                self.font.upright_letters = false;
                self.font.special = SpecialFont::None;
            }
            MathStyle::Sans => {
                self.font.family = Family::Sans;
                self.font.weight = Weight::Regular;
                self.font.slant = Some(Slant::Upright);
                self.font.upright_letters = true;
                self.font.special = SpecialFont::None;
            }
            MathStyle::Mono => {
                self.font.family = Family::Mono;
                self.font.weight = Weight::Regular;
                self.font.slant = Some(Slant::Upright);
                self.font.upright_letters = true;
                self.font.special = SpecialFont::None;
            }
            MathStyle::Blackboard => {
                self.font.family = Family::Math;
                self.font.slant = Some(Slant::Upright);
                self.font.special = SpecialFont::DoubleStruck;
            }
            MathStyle::Script => {
                self.font.family = Family::Math;
                self.font.slant = Some(Slant::Upright);
                self.font.special = SpecialFont::Script;
            }
            MathStyle::Fraktur => {
                self.font.family = Family::Math;
                self.font.slant = Some(Slant::Upright);
                self.font.special = SpecialFont::Fraktur;
            }
        }
    }

    fn hspace_node(&mut self) -> Node {
        // `\hspace*{..}` behaves like `\hspace{..}` here: nothing is discarded.
        if matches!(self.peek(), Some(Token::Char('*'))) {
            self.next();
        }
        match self.dimension_arg() {
            Some(em) => Node::Space(em),
            None => {
                self.warn("\\hspace 缺少长度参数");
                Node::Space(0.0)
            }
        }
    }

    fn big_delim(&mut self, size: BigSize, class: Class) -> Node {
        match self.read_delim() {
            Some(delim) => Node::BigDelim {
                delim,
                size,
                class,
            },
            None => {
                self.warn("\\big 系列命令后缺少定界符");
                Node::Row(Vec::new())
            }
        }
    }

    fn left_right(&mut self) -> Node {
        let left = match self.read_delim() {
            Some(d) => d,
            None => {
                self.warn("\\left 后缺少定界符");
                return Node::Raw("\\left".to_owned());
            }
        };
        let body = self.parse_row();
        let right = if matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "right") {
            self.next();
            match self.read_delim() {
                Some(d) => d,
                None => {
                    self.warn("\\right 后缺少定界符");
                    Delim::Dot
                }
            }
        } else {
            self.warn("\\left 缺少匹配的 \\right");
            Delim::Dot
        };
        Node::Delimited {
            left,
            right,
            body: Box::new(body),
        }
    }

    /// Consume the next token if it names a delimiter.
    fn read_delim(&mut self) -> Option<Delim> {
        let delim = delim_of(self.peek()?)?;
        self.next();
        Some(delim)
    }

    // -- environments -------------------------------------------------------

    fn environment(&mut self) -> Node {
        let name = match self.next() {
            Some(Token::Group(g)) => g.trim().to_owned(),
            _ => {
                self.warn("\\begin 后缺少环境名");
                return Node::Raw("\\begin".to_owned());
            }
        };
        let kind = match EnvKind::from_name(&name) {
            Some(k) => k,
            None => {
                self.warn(&format!("未知环境 {{{name}}}，已按矩阵排版"));
                EnvKind::Matrix
            }
        };
        // `array` takes a column spec; `alignedat`/`alignat` take a column
        // *count* in braces, which is consumed so it does not print as text.
        let spec = if kind == EnvKind::Array {
            self.parse_array_spec()
        } else {
            if matches!(self.peek(), Some(Token::LBrace)) && name.contains("alignat") {
                let _ = self.read_raw_arg();
            }
            None
        };
        let (rows, gaps) = self.parse_env_body();
        Node::Environ {
            kind,
            rows,
            spec,
            gaps,
        }
    }

    fn parse_array_spec(&mut self) -> Option<ArraySpec> {
        if !matches!(self.peek(), Some(Token::LBrace)) {
            self.warn("array 缺少列格式说明，已按居中排版");
            return None;
        }
        self.next();
        let mut spec = ArraySpec::default();
        if !self.spec_columns(&mut spec) {
            self.warn("array 列格式说明缺少 '}'");
        }
        Some(spec)
    }

    /// Parse column letters up to the closing `}`. Returns `false` on EOF.
    ///
    /// Handles the LaTeX column syntax that shows up in practice: `l c r`,
    /// vertical rules `|`, `p{2cm}` / `m{..}` / `b{..}` minimum widths,
    /// `@{}` gap suppression, and `*{n}{...}` repetition.
    fn spec_columns(&mut self, spec: &mut ArraySpec) -> bool {
        loop {
            let tok = match self.next() {
                Some(t) => t,
                None => return false,
            };
            match tok {
                Token::RBrace => return true,
                Token::Char('l') => spec.push_col(ColAlign::Left, 0.0),
                Token::Char('c') => spec.push_col(ColAlign::Center, 0.0),
                Token::Char('r') => spec.push_col(ColAlign::Right, 0.0),
                // `C` / `L` / `R` are not standard; treat them as their
                // lowercase counterparts rather than dropping the column.
                Token::Char('L') => spec.push_col(ColAlign::Left, 0.0),
                Token::Char('C') => spec.push_col(ColAlign::Center, 0.0),
                Token::Char('R') => spec.push_col(ColAlign::Right, 0.0),
                Token::Char('|') => {
                    if let Some(last) = spec.vlines.last_mut() {
                        *last = true;
                    }
                }
                Token::Command(ref c) if c == "vert" => {
                    if let Some(last) = spec.vlines.last_mut() {
                        *last = true;
                    }
                }
                Token::Char('p') | Token::Char('m') | Token::Char('b') => {
                    let w = self
                        .read_raw_arg()
                        .map(|s| parse_length_pt(&s).max(0.0))
                        .unwrap_or(0.0);
                    spec.push_col(ColAlign::Left, w);
                }
                Token::Char('@') => {
                    // `@{}` removes the space at this boundary; `@{...}` is
                    // approximated by the length of its content when it names
                    // a spacing command, and by zero otherwise.
                    let body = self.read_raw_arg().unwrap_or_default();
                    let gap = parse_spec_gap(&body);
                    let idx = spec.aligns.len();
                    while spec.gaps.len() <= idx {
                        spec.gaps.push(None);
                    }
                    spec.gaps[idx] = Some(gap);
                }
                Token::Char('*') => {
                    let count = self
                        .read_raw_arg()
                        .and_then(|s| s.trim().parse::<usize>().ok())
                        .unwrap_or(0)
                        .min(64);
                    if !matches!(self.peek(), Some(Token::LBrace)) {
                        self.warn("array 的 *{n} 缺少列格式");
                        continue;
                    }
                    // The repeated part is a complete spec of its own, so it is
                    // parsed into a scratch spec and merged — sharing this loop
                    // would consume the enclosing `}` on its first iteration.
                    self.next();
                    let mut sub = ArraySpec::default();
                    if !self.spec_columns(&mut sub) {
                        self.warn("array 的 *{n}{...} 缺少 '}'");
                    }
                    for _ in 0..count {
                        for (i, align) in sub.aligns.iter().enumerate() {
                            let width = sub.min_widths.get(i).copied().unwrap_or(0.0);
                            spec.push_col(*align, width);
                            if let Some(rule) = sub.vlines.get(i).copied() {
                                if let Some(last) = spec.vlines.last_mut() {
                                    *last = rule;
                                }
                            }
                        }
                        if let Some(rule) = sub.vlines.get(sub.aligns.len()).copied() {
                            if rule
                                && let Some(last) = spec.vlines.last_mut()
                            {
                                *last = true;
                            }
                        }
                    }
                    if sub.aligns.is_empty() && count > 0 {
                        self.warn("array 的 *{n}{...} 里没有列格式");
                    }
                }
                Token::Char('>') | Token::Char('<') => {
                    // `>{...}` / `<{...}` inject declarations; only their
                    // presence is noted, the content is not applied.
                    let _ = self.read_raw_arg();
                    self.warn("array 列格式中的 >{...} / <{...} 已被忽略");
                }
                Token::Char('^') => {}
                other => {
                    self.warn(&format!("array 列格式中的 {other:?} 已被忽略"));
                }
            }
        }
    }

    fn parse_env_body(&mut self) -> (Vec<Vec<Node>>, Vec<f32>) {
        let mut rows: Vec<Vec<Node>> = Vec::new();
        let mut gaps: Vec<f32> = Vec::new();
        let mut row: Vec<Node> = Vec::new();

        loop {
            if self.peek().is_none() {
                if !row.is_empty() {
                    rows.push(row);
                    gaps.push(0.0);
                }
                self.warn("环境缺少 \\end");
                return (rows, gaps);
            }
            if matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "end") {
                self.next();
                if matches!(self.peek(), Some(Token::Group(_))) {
                    self.next();
                }
                if !row.is_empty() {
                    rows.push(row);
                    gaps.push(0.0);
                }
                return (rows, gaps);
            }

            // `\hline` / `\cline{..}` occupy a row of their own; a rule never
            // shares a row with cells, which keeps the column maths simple.
            let is_rule = matches!(
                self.peek(),
                Some(Token::Command(c)) if c.as_str() == "hline" || c.as_str() == "cline"
            );
            if is_rule {
                let is_cline = matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "cline");
                self.next();
                if is_cline {
                    let _ = self.read_raw_arg();
                }
                if !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                    gaps.push(0.0);
                }
                rows.push(vec![Node::Rule]);
                gaps.push(0.0);
                continue;
            }

            row.push(self.parse_row());

            if matches!(self.peek(), Some(Token::Amp)) {
                self.next();
                continue;
            }
            if matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "\\") {
                self.next();
                let extra = self.read_row_gap();
                rows.push(std::mem::take(&mut row));
                gaps.push(extra);
                continue;
            }
            rows.push(std::mem::take(&mut row));
            gaps.push(0.0);
            // Only `\end` or EOF may follow a completed cell; anything else
            // (a stray `\right`, say) is skipped so the loop always advances.
            let done = self.peek().is_none()
                || matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "end");
            if !done {
                self.warn("环境中出现无法识别的记号，已忽略");
                self.next();
            }
        }
    }
}

/// Collapse a one-element row; empty input stays an empty row.
fn collapse(mut items: Vec<Node>) -> Node {
    if items.len() == 1 {
        match items.pop() {
            Some(node) => node,
            None => Node::Row(Vec::new()),
        }
    } else {
        Node::Row(items)
    }
}

/// Map a token to a delimiter, if it is one.
fn delim_of(tok: &Token) -> Option<Delim> {
    Some(match tok {
        Token::Char('(') => Delim::LParen,
        Token::Char(')') => Delim::RParen,
        Token::Char('[') => Delim::LBracket,
        Token::Char(']') => Delim::RBracket,
        Token::Char('|') => Delim::Pipe,
        Token::Char('.') => Delim::Dot,
        Token::Char('/') => Delim::Slash,
        Token::Char('<') => Delim::LAngle,
        Token::Char('>') => Delim::RAngle,
        Token::LBrace => Delim::LBrace,
        Token::RBrace => Delim::RBrace,
        Token::Command(n) => match n.as_str() {
            "{" => Delim::LBrace,
            "}" => Delim::RBrace,
            "|" => Delim::DPipe,
            "langle" => Delim::LAngle,
            "rangle" => Delim::RAngle,
            "lfloor" => Delim::LFloor,
            "rfloor" => Delim::RFloor,
            "lceil" => Delim::LCeil,
            "rceil" => Delim::RCeil,
            "lvert" => Delim::Pipe,
            "rvert" => Delim::Pipe,
            "lVert" => Delim::DPipe,
            "rVert" => Delim::DPipe,
            "vert" => Delim::Pipe,
            "Vert" => Delim::DPipe,
            "backslash" => Delim::Backslash,
            "slash" => Delim::Slash,
            "." => Delim::Dot,
            _ => return None,
        },
        _ => return None,
    })
}

/// Parse a length into points.
///
/// Font-relative units (`em`, `ex`, `mu`) need the current em size, which the
/// parser does not have; they are resolved against the 10 pt reference size the
/// rest of this file uses, so `2em` is 20 pt here and gets scaled by the caller.
/// Absolute units (`cm`, `mm`, `in`, `pt`, `bp`, `pc`) are exact.
fn parse_length_pt(s: &str) -> f32 {
    let t = s.trim();
    let num: String = t
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    let value: f32 = num.parse().unwrap_or(0.0);
    let unit = &t[num.len().min(t.len())..];
    if unit.starts_with("cm") {
        value * 28.45
    } else if unit.starts_with("mm") {
        value * 2.845
    } else if unit.starts_with("in") {
        value * 72.27
    } else if unit.starts_with("bp") {
        value * 1.003_75
    } else if unit.starts_with("pt") {
        value
    } else if unit.starts_with("pc") {
        value * 12.0
    } else if unit.starts_with("ex") {
        value * 5.0
    } else if unit.starts_with("mu") {
        value * 10.0 / 18.0
    } else {
        // `em` and a missing unit both mean "em of the reference size".
        value * 10.0
    }
}

/// Parse a length into em, against the same 10 pt reference.
fn parse_dimension(s: &str) -> f32 {
    parse_length_pt(s) * 0.1
}

/// Space inserted at an `@{...}` boundary, in em.
///
/// `@{}` (the common case) collapses the inter-column space entirely; a spacing
/// command or a dimension inside the braces is honoured; anything else
/// degenerates to zero rather than to a guess.
fn parse_spec_gap(body: &str) -> f32 {
    let t = body.trim();
    if t.is_empty() {
        return 0.0;
    }
    let name = t.strip_prefix('\\').unwrap_or(t);
    match name {
        "," | "thinspace" => 0.1667,
        ":" | "medspace" => 0.2222,
        ";" | "thickspace" => 0.2778,
        "quad" | "hfill" => 1.0,
        "qquad" => 2.0,
        "enspace" | "enskip" => 0.5,
        " " => 0.3333,
        "!" | "negthinspace" => 0.0,
        _ => {
            if t.starts_with('\\') {
                0.0
            } else {
                parse_dimension(t).max(0.0)
            }
        }
    }
}

/// xcolor-ish colour names plus `#rgb` / `#rrggbb` literals.
fn named_color(name: &str) -> Option<Rgba> {
    let t = name.trim();
    if let Some(hex) = t.strip_prefix('#') {
        let digits: Vec<u8> = hex
            .chars()
            .filter_map(|c| c.to_digit(16).map(|d| d as u8))
            .collect();
        return match digits.len() {
            3 => Some(Rgba::new(
                digits[0] * 17,
                digits[1] * 17,
                digits[2] * 17,
                255,
            )),
            6 => Some(Rgba::new(
                digits[0] * 16 + digits[1],
                digits[2] * 16 + digits[3],
                digits[4] * 16 + digits[5],
                255,
            )),
            _ => None,
        };
    }
    let lower = t.to_ascii_lowercase();
    let rgb = match lower.as_str() {
        "black" => 0x000000,
        "white" => 0xFF_FFFF,
        "gray" | "grey" => 0x80_8080,
        "darkgray" | "darkgrey" => 0xA9_A9A9,
        "lightgray" | "lightgrey" => 0xD3_D3D3,
        "red" => 0xFF_0000,
        "orange" => 0xFF_A5_00,
        "yellow" => 0xFF_FF00,
        "lime" => 0x00_FF00,
        "green" => 0x00_80_00,
        "teal" => 0x00_80_80,
        "cyan" | "aqua" => 0x00_FF_FF,
        "blue" => 0x00_00FF,
        "navy" => 0x00_00_80,
        "violet" | "purple" => 0x80_00_80,
        "magenta" | "fuchsia" | "pink" => 0xFF_00FF,
        "brown" => 0xA5_2A_2A,
        "olive" => 0x80_80_00,
        "maroon" => 0x80_0000,
        "silver" => 0xC0_C0C0,
        _ => return None,
    };
    Some(Rgba::hex(rgb))
}

/// `\big`, `\Bigl`, ... prefixes: fixed delimiter heights plus an atom class.
fn big_prefix(name: &str) -> Option<(BigSize, Class)> {
    let (stem, class) = match name.strip_suffix('l') {
        Some(stem) if name.len() > 1 => (stem, Class::Open),
        _ => match name.strip_suffix('r') {
            Some(stem) if name.len() > 1 => (stem, Class::Close),
            _ => match name.strip_suffix('m') {
                Some(stem) if name.len() > 1 => (stem, Class::Rel),
                _ => (name, Class::Ord),
            },
        },
    };
    let size = match stem {
        "big" => BigSize::One,
        "Big" => BigSize::Two,
        "bigg" => BigSize::Three,
        "Bigg" => BigSize::Four,
        _ => return None,
    };
    Some((size, class))
}

/// Accent commands.
fn accent_kind(name: &str) -> Option<AccentKind> {
    Some(match name {
        "hat" => AccentKind::Hat,
        "widehat" => AccentKind::WideHat,
        "bar" => AccentKind::Bar,
        "overline" => AccentKind::Overline,
        "tilde" => AccentKind::Tilde,
        "widetilde" => AccentKind::WideTilde,
        "dot" => AccentKind::Dot,
        "ddot" => AccentKind::Ddot,
        "vec" => AccentKind::Vec,
        "check" => AccentKind::Check,
        "breve" => AccentKind::Breve,
        "acute" => AccentKind::Acute,
        "grave" => AccentKind::Grave,
        "mathring" => AccentKind::Ring,
        "underline" => AccentKind::Underline,
        _ => return None,
    })
}

/// Large operators, limit-taking names and function names.
fn op_symbol(name: &str) -> Option<(&'static str, OpKind)> {
    Some(match name {
        // Large operators.
        "sum" => ("\u{2211}", OpKind::Large),
        "prod" => ("\u{220F}", OpKind::Large),
        "coprod" => ("\u{2210}", OpKind::Large),
        // Integrals: large, but `\sum`-style limits are opt-in.
        "int" => ("\u{222B}", OpKind::Integral),
        "iint" => ("\u{222C}", OpKind::Integral),
        "iiint" => ("\u{222D}", OpKind::Integral),
        "iiiint" => ("\u{2A0C}", OpKind::Integral),
        "oint" => ("\u{222E}", OpKind::Integral),
        "oiint" => ("\u{222F}", OpKind::Integral),
        "oiiint" => ("\u{2230}", OpKind::Integral),
        "smallint" => ("\u{222B}", OpKind::Large),
        "bigcup" => ("\u{22C3}", OpKind::Large),
        "bigcap" => ("\u{22C2}", OpKind::Large),
        "bigsqcup" => ("\u{2A06}", OpKind::Large),
        "biguplus" => ("\u{2A04}", OpKind::Large),
        "bigoplus" => ("\u{2A01}", OpKind::Large),
        "bigotimes" => ("\u{2A02}", OpKind::Large),
        "bigodot" => ("\u{2A00}", OpKind::Large),
        "bigvee" => ("\u{22C1}", OpKind::Large),
        "bigwedge" => ("\u{22C0}", OpKind::Large),
        // Upright names that accept limits in display style.
        "lim" => ("lim", OpKind::Limit),
        "limsup" => ("lim sup", OpKind::Limit),
        "liminf" => ("lim inf", OpKind::Limit),
        "sup" => ("sup", OpKind::Limit),
        "inf" => ("inf", OpKind::Limit),
        "max" => ("max", OpKind::Limit),
        "min" => ("min", OpKind::Limit),
        "argmax" => ("arg max", OpKind::Limit),
        "argmin" => ("arg min", OpKind::Limit),
        "Pr" => ("Pr", OpKind::Limit),
        // Function names: scripts always to the side.
        "det" => ("det", OpKind::Func),
        "gcd" => ("gcd", OpKind::Func),
        "deg" => ("deg", OpKind::Func),
        "dim" => ("dim", OpKind::Func),
        "ker" => ("ker", OpKind::Func),
        "hom" => ("hom", OpKind::Func),
        "arg" => ("arg", OpKind::Func),
        "log" => ("log", OpKind::Func),
        "ln" => ("ln", OpKind::Func),
        "lg" => ("lg", OpKind::Func),
        "exp" => ("exp", OpKind::Func),
        "sin" => ("sin", OpKind::Func),
        "cos" => ("cos", OpKind::Func),
        "tan" => ("tan", OpKind::Func),
        "cot" => ("cot", OpKind::Func),
        "sec" => ("sec", OpKind::Func),
        "csc" => ("csc", OpKind::Func),
        "arcsin" => ("arcsin", OpKind::Func),
        "arccos" => ("arccos", OpKind::Func),
        "arctan" => ("arctan", OpKind::Func),
        "sinh" => ("sinh", OpKind::Func),
        "cosh" => ("cosh", OpKind::Func),
        "tanh" => ("tanh", OpKind::Func),
        "coth" => ("coth", OpKind::Func),
        "sgn" => ("sgn", OpKind::Func),
        _ => return None,
    })
}

/// Greek letters and standalone symbols.
fn named_symbol(name: &str) -> Option<NamedSym> {
    fn it(text: &'static str) -> NamedSym {
        NamedSym {
            text,
            class: Class::Ord,
            italic: true,
        }
    }
    fn ord(text: &'static str) -> NamedSym {
        NamedSym {
            text,
            class: Class::Ord,
            italic: false,
        }
    }
    fn rel(text: &'static str) -> NamedSym {
        NamedSym {
            text,
            class: Class::Rel,
            italic: false,
        }
    }
    fn bin(text: &'static str) -> NamedSym {
        NamedSym {
            text,
            class: Class::Bin,
            italic: false,
        }
    }

    Some(match name {
        // ---- lowercase Greek (italic variables) ----
        "alpha" => it("\u{03B1}"),
        "beta" => it("\u{03B2}"),
        "gamma" => it("\u{03B3}"),
        "delta" => it("\u{03B4}"),
        "epsilon" => it("\u{03F5}"),
        "varepsilon" => it("\u{03B5}"),
        "zeta" => it("\u{03B6}"),
        "eta" => it("\u{03B7}"),
        "theta" => it("\u{03B8}"),
        "vartheta" => it("\u{03D1}"),
        "iota" => it("\u{03B9}"),
        "kappa" => it("\u{03BA}"),
        "varkappa" => it("\u{03F0}"),
        "lambda" => it("\u{03BB}"),
        "mu" => it("\u{03BC}"),
        "nu" => it("\u{03BD}"),
        "xi" => it("\u{03BE}"),
        "omicron" => it("\u{03BF}"),
        "pi" => it("\u{03C0}"),
        "varpi" => it("\u{03D6}"),
        "rho" => it("\u{03C1}"),
        "varrho" => it("\u{03F1}"),
        "sigma" => it("\u{03C3}"),
        "varsigma" => it("\u{03C2}"),
        "tau" => it("\u{03C4}"),
        "upsilon" => it("\u{03C5}"),
        "phi" => it("\u{03D5}"),
        "varphi" => it("\u{03C6}"),
        "chi" => it("\u{03C7}"),
        "psi" => it("\u{03C8}"),
        "omega" => it("\u{03C9}"),
        // ---- uppercase Greek (upright) ----
        "Alpha" => ord("\u{0391}"),
        "Beta" => ord("\u{0392}"),
        "Gamma" => ord("\u{0393}"),
        "Delta" => ord("\u{0394}"),
        "Epsilon" => ord("\u{0395}"),
        "Zeta" => ord("\u{0396}"),
        "Eta" => ord("\u{0397}"),
        "Theta" => ord("\u{0398}"),
        "Iota" => ord("\u{0399}"),
        "Kappa" => ord("\u{039A}"),
        "Lambda" => ord("\u{039B}"),
        "Mu" => ord("\u{039C}"),
        "Nu" => ord("\u{039D}"),
        "Xi" => ord("\u{039E}"),
        "Omicron" => ord("\u{039F}"),
        "Pi" => ord("\u{03A0}"),
        "Rho" => ord("\u{03A1}"),
        "Sigma" => ord("\u{03A3}"),
        "Tau" => ord("\u{03A4}"),
        "Upsilon" => ord("\u{03A5}"),
        "Phi" => ord("\u{03A6}"),
        "Chi" => ord("\u{03A7}"),
        "Psi" => ord("\u{03A8}"),
        "Omega" => ord("\u{03A9}"),
        // ---- standalone symbols ----
        "infty" => ord("\u{221E}"),
        "partial" => ord("\u{2202}"),
        "nabla" => ord("\u{2207}"),
        "emptyset" => ord("\u{2205}"),
        "varnothing" => ord("\u{2205}"),
        "forall" => ord("\u{2200}"),
        "exists" => ord("\u{2203}"),
        "nexists" => ord("\u{2204}"),
        "neg" => ord("\u{00AC}"),
        "top" => ord("\u{22A4}"),
        "bot" => ord("\u{22A5}"),
        "angle" => ord("\u{2220}"),
        "triangle" => ord("\u{25B3}"),
        "square" => ord("\u{25A1}"),
        "star" => bin("\u{22C6}"),
        "ast" => bin("\u{2217}"),
        "circ" => bin("\u{2218}"),
        "bullet" => bin("\u{2219}"),
        "oplus" => bin("\u{2295}"),
        "otimes" => bin("\u{2297}"),
        "odot" => bin("\u{2299}"),
        "dagger" => ord("\u{2020}"),
        "ddagger" => ord("\u{2021}"),
        "prime" => ord("\u{2032}"),
        "ldots" => ord("\u{2026}"),
        "dots" => ord("\u{2026}"),
        "cdots" => ord("\u{22EF}"),
        "vdots" => ord("\u{22EE}"),
        "ddots" => ord("\u{22F1}"),
        "S" => ord("\u{00A7}"),
        "P" => ord("\u{00B6}"),
        "aleph" => ord("\u{2135}"),
        "hbar" => ord("\u{210F}"),
        "ell" => ord("\u{2113}"),
        "Re" => ord("\u{211C}"),
        "Im" => ord("\u{2111}"),
        "wp" => ord("\u{2118}"),
        "imath" => ord("\u{0131}"),
        "jmath" => ord("\u{0237}"),
        "surd" => ord("\u{221A}"),
        "flat" => ord("\u{266D}"),
        "natural" => ord("\u{266E}"),
        "sharp" => ord("\u{266F}"),
        // ---- binary operators ----
        "pm" => bin("\u{00B1}"),
        "mp" => bin("\u{2213}"),
        "times" => bin("\u{00D7}"),
        "div" => bin("\u{00F7}"),
        "cdot" => bin("\u{22C5}"),
        // ---- relations ----
        "leq" | "le" => rel("\u{2264}"),
        "geq" | "ge" => rel("\u{2265}"),
        "neq" | "ne" => rel("\u{2260}"),
        "equiv" => rel("\u{2261}"),
        "approx" => rel("\u{2248}"),
        "sim" => rel("\u{223C}"),
        "simeq" => rel("\u{2243}"),
        "cong" => rel("\u{2245}"),
        "propto" => rel("\u{221D}"),
        "ll" => rel("\u{226A}"),
        "gg" => rel("\u{226B}"),
        "prec" => rel("\u{227A}"),
        "succ" => rel("\u{227B}"),
        "preceq" => rel("\u{2AAF}"),
        "succeq" => rel("\u{2AB0}"),
        "subset" => rel("\u{2282}"),
        "supset" => rel("\u{2283}"),
        "subseteq" => rel("\u{2286}"),
        "supseteq" => rel("\u{2287}"),
        "in" => rel("\u{2208}"),
        "notin" => rel("\u{2209}"),
        "ni" => rel("\u{220B}"),
        "parallel" => rel("\u{2225}"),
        "perp" => rel("\u{22A5}"),
        "mid" => rel("\u{2223}"),
        "nmid" => rel("\u{2224}"),
        "vdash" => rel("\u{22A2}"),
        "dashv" => rel("\u{22A3}"),
        "models" => rel("\u{22A8}"),
        "implies" => rel("\u{27F9}"),
        "iff" => rel("\u{27FA}"),
        "to" | "rightarrow" => rel("\u{2192}"),
        "leftarrow" => rel("\u{2190}"),
        "leftrightarrow" => rel("\u{2194}"),
        "Longrightarrow" => rel("\u{27F9}"),
        "Longleftarrow" => rel("\u{27F8}"),
        "Longleftrightarrow" => rel("\u{27FA}"),
        "mapsto" => rel("\u{21A6}"),
        "longrightarrow" => rel("\u{27F6}"),
        "longleftarrow" => rel("\u{27F5}"),
        "nearrow" => rel("\u{2197}"),
        "searrow" => rel("\u{2198}"),
        "swarrow" => rel("\u{2199}"),
        "nwarrow" => rel("\u{2196}"),
        "uparrow" => rel("\u{2191}"),
        "downarrow" => rel("\u{2193}"),
        "updownarrow" => rel("\u{2195}"),
        "gets" => rel("\u{2190}"),
        "hookrightarrow" => rel("\u{21AA}"),
        "hookleftarrow" => rel("\u{21A9}"),
        "longmapsto" => rel("\u{27FC}"),
        "rightsquigarrow" | "leadsto" => rel("\u{21DD}"),
        "twoheadrightarrow" => rel("\u{21A0}"),
        "nrightarrow" => rel("\u{219B}"),
        "nleftarrow" => rel("\u{219A}"),
        "upharpoonright" => rel("\u{21BE}"),
        "downharpoonright" => rel("\u{21C2}"),
        "doteq" => rel("\u{2250}"),
        "triangleq" => rel("\u{225C}"),
        "coloneqq" => rel("\u{2254}"),
        "fallingdotseq" => rel("\u{2252}"),
        "risingdotseq" => rel("\u{2253}"),
        "lesssim" => rel("\u{2272}"),
        "gtrsim" => rel("\u{2273}"),
        "leqslant" => rel("\u{2A7D}"),
        "geqslant" => rel("\u{2A7E}"),
        "nleq" => rel("\u{2270}"),
        "ngeq" => rel("\u{2271}"),
        "subsetneq" => rel("\u{228A}"),
        "supsetneq" => rel("\u{228B}"),
        "sqsubseteq" => rel("\u{2291}"),
        "sqsupseteq" => rel("\u{2292}"),
        "asymp" => rel("\u{224D}"),
        "bowtie" => rel("\u{22C8}"),
        "smile" => rel("\u{2323}"),
        "frown" => rel("\u{2322}"),
        "Vdash" => rel("\u{22A9}"),
        "vDash" => rel("\u{22A8}"),
        "nvdash" => rel("\u{22AC}"),
        "therefore" => rel("\u{2234}"),
        "because" => rel("\u{2235}"),
        // ---- more binary operators ----
        "ominus" => bin("\u{2296}"),
        "oslash" => bin("\u{2298}"),
        "uplus" => bin("\u{228E}"),
        "sqcup" => bin("\u{2294}"),
        "sqcap" => bin("\u{2293}"),
        "wr" => bin("\u{2240}"),
        "amalg" => bin("\u{2A3F}"),
        "diamond" => bin("\u{22C4}"),
        "bigcirc" => bin("\u{25EF}"),
        "triangleleft" => bin("\u{25C1}"),
        "triangleright" => bin("\u{25B7}"),
        "setminus" | "smallsetminus" => bin("\u{2216}"),
        "circledast" => bin("\u{229B}"),
        "circledcirc" => bin("\u{229A}"),
        "circleddash" => bin("\u{229D}"),
        "divideontimes" => bin("\u{22C7}"),
        "ltimes" => bin("\u{22C9}"),
        "rtimes" => bin("\u{22CA}"),
        // ---- more ordinary symbols ----
        "complement" => ord("\u{2201}"),
        "Box" => ord("\u{25A1}"),
        "Diamond" => ord("\u{25C7}"),
        "lozenge" => ord("\u{25CA}"),
        "blacksquare" => ord("\u{25A0}"),
        "blacktriangle" => ord("\u{25B2}"),
        "vartriangle" => ord("\u{25B3}"),
        "blacktriangledown" => ord("\u{25BC}"),
        "clubsuit" => ord("\u{2663}"),
        "diamondsuit" => ord("\u{2662}"),
        "heartsuit" => ord("\u{2661}"),
        "spadesuit" => ord("\u{2660}"),
        "mho" => ord("\u{2127}"),
        "eth" => ord("\u{00F0}"),
        "beth" => ord("\u{2136}"),
        "gimel" => ord("\u{2137}"),
        "daleth" => ord("\u{2138}"),
        "backprime" => ord("\u{2035}"),
        "digamma" => it("\u{03DD}"),
        "checkmark" => ord("\u{2713}"),
        "circledR" => ord("\u{00AE}"),
        "circledS" => ord("\u{24C8}"),
        "yen" => ord("\u{00A5}"),
        "pounds" => ord("\u{00A3}"),
        "degree" => ord("\u{00B0}"),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Mathematical alphanumeric symbol tables
// ---------------------------------------------------------------------------

/// Double-struck capitals: most live in the astral plane, but `C H N P Q R Z`
/// keep their legacy Letterlike Symbols codepoints, hence the explicit table.
const DOUBLE_STRUCK_UPPER: [(char, u32); 26] = [
    ('A', 0x1D538),
    ('B', 0x1D539),
    ('C', 0x2102),
    ('D', 0x1D53B),
    ('E', 0x1D53C),
    ('F', 0x1D53D),
    ('G', 0x1D53E),
    ('H', 0x210D),
    ('I', 0x1D540),
    ('J', 0x1D541),
    ('K', 0x1D542),
    ('L', 0x1D543),
    ('M', 0x1D544),
    ('N', 0x2115),
    ('O', 0x1D546),
    ('P', 0x2119),
    ('Q', 0x211A),
    ('R', 0x211D),
    ('S', 0x1D54A),
    ('T', 0x1D54B),
    ('U', 0x1D54C),
    ('V', 0x1D54D),
    ('W', 0x1D54E),
    ('X', 0x1D54F),
    ('Y', 0x1D550),
    ('Z', 0x2124),
];

/// Script capitals, again with legacy codepoints for `B E F H I L M R`.
const SCRIPT_UPPER: [(char, u32); 26] = [
    ('A', 0x1D49C),
    ('B', 0x212C),
    ('C', 0x1D49E),
    ('D', 0x1D49F),
    ('E', 0x2130),
    ('F', 0x2131),
    ('G', 0x1D4A2),
    ('H', 0x210B),
    ('I', 0x2110),
    ('J', 0x1D4A5),
    ('K', 0x1D4A6),
    ('L', 0x2112),
    ('M', 0x2133),
    ('N', 0x1D4A9),
    ('O', 0x1D4AA),
    ('P', 0x1D4AB),
    ('Q', 0x1D4AC),
    ('R', 0x211B),
    ('S', 0x1D4AE),
    ('T', 0x1D4AF),
    ('U', 0x1D4B0),
    ('V', 0x1D4B1),
    ('W', 0x1D4B2),
    ('X', 0x1D4B3),
    ('Y', 0x1D4B4),
    ('Z', 0x1D4B5),
];

/// Fraktur capitals, with the five letters that live in Letterlike Symbols.
const FRAKTUR_UPPER: [(char, u32); 26] = [
    ('A', 0x1D504),
    ('B', 0x1D505),
    ('C', 0x212D),
    ('D', 0x1D507),
    ('E', 0x1D508),
    ('F', 0x1D509),
    ('G', 0x1D50A),
    ('H', 0x210C),
    ('I', 0x2111),
    ('J', 0x1D50D),
    ('K', 0x1D50E),
    ('L', 0x1D50F),
    ('M', 0x1D510),
    ('N', 0x1D511),
    ('O', 0x1D512),
    ('P', 0x1D513),
    ('Q', 0x1D514),
    ('R', 0x211C),
    ('S', 0x1D516),
    ('T', 0x1D517),
    ('U', 0x1D518),
    ('V', 0x1D519),
    ('W', 0x1D51A),
    ('X', 0x1D51B),
    ('Y', 0x1D51C),
    ('Z', 0x2128),
];

fn lookup_upper(table: &[(char, u32)], c: char) -> Option<char> {
    table
        .iter()
        .find(|(k, _)| *k == c)
        .and_then(|(_, v)| char::from_u32(*v))
}

fn double_struck(c: char) -> Option<char> {
    if c.is_ascii_uppercase() {
        lookup_upper(&DOUBLE_STRUCK_UPPER, c)
    } else if c.is_ascii_lowercase() {
        char::from_u32(0x1D552 + (c as u32 - 'a' as u32))
    } else if c.is_ascii_digit() {
        char::from_u32(0x1D7D8 + (c as u32 - '0' as u32))
    } else {
        None
    }
}

fn script_letter(c: char) -> Option<char> {
    if c.is_ascii_uppercase() {
        lookup_upper(&SCRIPT_UPPER, c)
    } else if c.is_ascii_lowercase() {
        char::from_u32(0x1D4B6 + (c as u32 - 'a' as u32))
    } else {
        None
    }
}

fn fraktur_letter(c: char) -> Option<char> {
    if c.is_ascii_uppercase() {
        lookup_upper(&FRAKTUR_UPPER, c)
    } else if c.is_ascii_lowercase() {
        char::from_u32(0x1D51E + (c as u32 - 'a' as u32))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_handles_commands_and_escapes() {
        let t = tokenize(r"\frac{1}{2}+\alpha\\");
        assert!(matches!(t.first(), Some(Token::Command(c)) if c == "frac"));
        assert!(t.contains(&Token::Char('2')));
        assert!(t.contains(&Token::Command("alpha".to_owned())));
        assert!(t.contains(&Token::Command("\\".to_owned())));
    }

    #[test]
    fn comments_and_whitespace_are_dropped() {
        let t = tokenize("a % comment\nb");
        let chars: Vec<char> = t
            .iter()
            .filter_map(|tok| match tok {
                Token::Char(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(chars, vec!['a', 'b']);
    }

    #[test]
    fn text_group_keeps_spaces() {
        let t = tokenize(r"\text{if } x");
        assert!(t.contains(&Token::Group("if ".to_owned())));
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let src = format!("x{}{}", "{".repeat(500), "}".repeat(500));
        let out = parse(tokenize(&src));
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn operatorname_star_is_folded_into_the_command_name() {
        let t = tokenize(r"\operatorname*{argmax}_{x}");
        assert!(
            t.iter()
                .any(|tok| matches!(tok, Token::Command(c) if c == "operatorname*"))
        );
        assert!(t.contains(&Token::Group("argmax".to_owned())));
    }

    #[test]
    fn text_commands_keep_their_argument_verbatim() {
        let t = tokenize(r"\textbf{加粗 if } and \color{red}");
        assert!(t.contains(&Token::Group("加粗 if ".to_owned())));
        assert!(t.contains(&Token::Group("red".to_owned())));
    }

    #[test]
    fn array_spec_understands_stars_widths_and_gaps() {
        let out = parse(tokenize(
            r"\begin{array}{@{}*{2}{c|}p{2cm}@{}}a\end{array}",
        ));
        let Node::Environ {
            spec: Some(spec), ..
        } = out.root
        else {
            panic!("没有解析成带列格式的环境：{:?}", out.root);
        };
        assert_eq!(
            spec.aligns,
            vec![ColAlign::Center, ColAlign::Center, ColAlign::Left]
        );
        // `*{2}{c|}` repeats the rule after each repeated column.
        assert_eq!(spec.vlines, vec![false, true, true, false]);
        assert_eq!(spec.gaps.first().copied(), Some(Some(0.0)));
        assert!(
            (spec.min_widths[2] - 56.9).abs() < 0.2,
            "p{{2cm}} 应约为 56.9pt，实际 {}",
            spec.min_widths[2]
        );
    }

    #[test]
    fn hline_becomes_a_row_of_its_own() {
        let out = parse(tokenize(r"\begin{array}{c}a\\\hline b\end{array}"));
        let Node::Environ { rows, gaps, .. } = out.root else {
            panic!("不是环境：{:?}", out.root);
        };
        assert_eq!(rows.len(), 3, "a / \\hline / b 应是三行");
        assert!(matches!(rows[1].as_slice(), [Node::Rule]));
        assert_eq!(gaps.len(), rows.len());
    }

    #[test]
    fn bracket_row_gap_is_recorded_in_points() {
        let out = parse(tokenize(r"\begin{matrix}a\\[6pt]b\end{matrix}"));
        let Node::Environ { gaps, .. } = out.root else {
            panic!("不是环境：{:?}", out.root);
        };
        assert_eq!(gaps.len(), 2);
        assert!((gaps[0] - 6.0).abs() < 0.01, "实际 {}", gaps[0]);
        // The `[6pt]` must not leak into the content as literal text.
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn color_and_negation_build_their_nodes() {
        let out = parse(tokenize(r"\not\in + \textcolor{red}{x}"));
        let Node::Row(items) = out.root else {
            panic!("不是行：{:?}", out.root);
        };
        assert!(matches!(items[0], Node::Negate { .. }));
        assert!(items.iter().any(|n| matches!(n, Node::Color { .. })));
    }

    #[test]
    fn new_commands_do_not_warn() {
        for src in [
            r"\overset{a}{b}",
            r"\underset{a}{b}",
            r"\stackrel{a}{b}",
            r"\overbrace{x}^{n}",
            r"\underbrace{x}_{n}",
            r"\boxed{x}",
            r"\phantom{x}",
            r"\hphantom{x}",
            r"\vphantom{x}",
            r"\mathrel{=}",
            r"\operatorname*{argmax}",
            r"\boldsymbol{\alpha}",
            r"\mkern2mu",
            r"\negthinspace",
            r"\hspace*{1em}",
            r"\color{#00ff00}x",
            r"\substack{a\\b}",
        ] {
            let out = parse(tokenize(src));
            assert!(
                out.warnings.is_empty(),
                "{src:?} 产生了警告：{:?}",
                out.warnings
            );
        }
    }
}
