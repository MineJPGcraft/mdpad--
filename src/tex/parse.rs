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

use crate::gfx::{Family, Slant, Weight};

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
const RAW_GROUP_COMMANDS: [&str; 9] = [
    "text",
    "textrm",
    "textsf",
    "texttt",
    "mbox",
    "operatorname",
    "hspace",
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
                    let name: String = chars[start..i].iter().collect();
                    if RAW_GROUP_COMMANDS.contains(&name.as_str()) {
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
    /// `\sum \int \bigcup ...`: drawn large in display style, limits allowed.
    Large,
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

/// `array` column specification, e.g. `{c|lr}`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArraySpec {
    pub aligns: Vec<ColAlign>,
    /// `vlines[i]` is `true` when a vertical rule sits before column `i`;
    /// index `aligns.len()` is the rule after the last column.
    pub vlines: Vec<bool>,
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
            "matrix" | "smallmatrix" => Self::Matrix,
            "pmatrix" => Self::ParenMatrix,
            "bmatrix" => Self::BracketMatrix,
            "vmatrix" => Self::BarMatrix,
            "Bmatrix" => Self::BraceMatrix,
            "cases" => Self::Cases,
            "aligned" => Self::Aligned,
            "align" | "align*" => Self::Align,
            "gathered" | "gather" => Self::Gathered,
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
    },
    /// Text the parser could not interpret, kept verbatim as `\foo`.
    Raw(String),
    /// `\\` outside an environment; laid out as a space.
    LineBreak,
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
            "text" => self.text_node(TextStyle::Sans, Class::Ord),
            "textrm" => self.text_node(TextStyle::Serif, Class::Ord),
            "textsf" => self.text_node(TextStyle::Sans, Class::Ord),
            "texttt" => self.text_node(TextStyle::Mono, Class::Ord),
            "mbox" => self.text_node(TextStyle::Serif, Class::Ord),
            "operatorname" => self.text_node(TextStyle::Serif, Class::Op),
            "mathrm" => self.style_wrap(MathStyle::Roman),
            "mathbf" => self.style_wrap(MathStyle::Bold),
            "mathit" => self.style_wrap(MathStyle::Italic),
            "mathsf" => self.style_wrap(MathStyle::Sans),
            "mathtt" => self.style_wrap(MathStyle::Mono),
            "bmod" | "mod" => Node::Text {
                body: "mod".to_owned(),
                style: TextStyle::Serif,
                class: Class::Bin,
            },
            "pmod" => {
                let body = self.parse_arg();
                let inner = Node::Row(vec![
                    Node::Text {
                        body: "mod".to_owned(),
                        style: TextStyle::Serif,
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
            "hspace" => self.hspace_node(),
            "displaystyle" => Node::StyleSwitch(SizeStyle::Display),
            "textstyle" => Node::StyleSwitch(SizeStyle::Text),
            "scriptstyle" => Node::StyleSwitch(SizeStyle::Script),
            "scriptscriptstyle" => Node::StyleSwitch(SizeStyle::ScriptScript),
            "limits" | "nolimits" => {
                self.warn("\\limits/\\nolimits 之前没有运算符");
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
            "," => Node::Space(0.1667),
            ":" => Node::Space(0.2222),
            ";" => Node::Space(0.2778),
            " " => Node::Space(0.3333),
            "!" => Node::Space(-0.1667),
            "quad" => Node::Space(1.0),
            "qquad" => Node::Space(2.0),
            "enspace" => Node::Space(0.5),
            "thinspace" => Node::Space(0.1667),
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

    fn text_node(&mut self, style: TextStyle, class: Class) -> Node {
        if let Some(body) = self.take_group() {
            return Node::Text { body, style, class };
        }
        self.warn("文本命令缺少 {...} 参数");
        Node::Raw("\\text".to_owned())
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
        if let Some(dim) = self.take_group() {
            return Node::Space(parse_dimension(&dim));
        }
        self.warn("\\hspace 缺少长度参数");
        Node::Space(0.0)
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
        let spec = if kind == EnvKind::Array {
            self.parse_array_spec()
        } else {
            None
        };
        let rows = self.parse_env_body();
        Node::Environ { kind, rows, spec }
    }

    fn parse_array_spec(&mut self) -> Option<ArraySpec> {
        if !matches!(self.peek(), Some(Token::LBrace)) {
            self.warn("array 缺少列格式说明，已按居中排版");
            return None;
        }
        self.next();

        let mut spec = ArraySpec {
            aligns: Vec::new(),
            vlines: vec![false],
        };
        loop {
            if self.peek().is_none() {
                self.warn("array 列格式说明缺少 '}'");
                break;
            }
            if matches!(self.peek(), Some(Token::RBrace)) {
                self.next();
                break;
            }
            let tok = match self.next() {
                Some(t) => t,
                None => break,
            };
            match tok {
                Token::Char('^') => {}
                Token::Char('|') => {
                    if let Some(last) = spec.vlines.last_mut() {
                        *last = true;
                    }
                }
                Token::Char('l') => {
                    spec.aligns.push(ColAlign::Left);
                    spec.vlines.push(false);
                }
                Token::Char('c') => {
                    spec.aligns.push(ColAlign::Center);
                    spec.vlines.push(false);
                }
                Token::Char('r') => {
                    spec.aligns.push(ColAlign::Right);
                    spec.vlines.push(false);
                }
                Token::Command(c) if c == "vert" => {
                    if let Some(last) = spec.vlines.last_mut() {
                        *last = true;
                    }
                }
                other => {
                    self.warn(&format!("array 列格式中的 {other:?} 已被忽略"));
                }
            }
        }
        Some(spec)
    }

    fn parse_env_body(&mut self) -> Vec<Vec<Node>> {
        let mut rows: Vec<Vec<Node>> = Vec::new();
        let mut row: Vec<Node> = Vec::new();

        loop {
            if self.peek().is_none() {
                if !row.is_empty() {
                    rows.push(row);
                }
                self.warn("环境缺少 \\end");
                return rows;
            }
            if matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "end") {
                self.next();
                if matches!(self.peek(), Some(Token::Group(_))) {
                    self.next();
                }
                if !row.is_empty() {
                    rows.push(row);
                }
                return rows;
            }

            row.push(self.parse_row());

            if matches!(self.peek(), Some(Token::Amp)) {
                self.next();
                continue;
            }
            if matches!(self.peek(), Some(Token::Command(c)) if c.as_str() == "\\") {
                self.next();
                rows.push(std::mem::take(&mut row));
                continue;
            }
            rows.push(std::mem::take(&mut row));
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

/// Parse an `\hspace` dimension into em units.
fn parse_dimension(s: &str) -> f32 {
    let t = s.trim();
    let num: String = t
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    let value: f32 = num.parse().unwrap_or(0.0);
    let unit = &t[num.len().min(t.len())..];
    if unit.starts_with("ex") {
        value * 0.5
    } else if unit.starts_with("mu") {
        value / 18.0
    } else if unit.starts_with("pt") {
        value * 0.1
    } else if unit.starts_with("cm") {
        value * 2.845
    } else if unit.starts_with("mm") {
        value * 0.2845
    } else if unit.starts_with("in") {
        value * 7.227
    } else {
        value
    }
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
        "int" => ("\u{222B}", OpKind::Large),
        "iint" => ("\u{222C}", OpKind::Large),
        "iiint" => ("\u{222D}", OpKind::Large),
        "oint" => ("\u{222E}", OpKind::Large),
        "bigcup" => ("\u{22C3}", OpKind::Large),
        "bigcap" => ("\u{22C2}", OpKind::Large),
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
        // Function names: scripts always to the side.
        "det" => ("det", OpKind::Func),
        "gcd" => ("gcd", OpKind::Func),
        "deg" => ("deg", OpKind::Func),
        "dim" => ("dim", OpKind::Func),
        "ker" => ("ker", OpKind::Func),
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
}
