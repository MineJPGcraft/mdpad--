//! Mermaid front-end: Mermaid source text -> diagram IR.
//!
//! This is a hand-written, dependency-free and network-free parser. It never
//! executes JavaScript and never touches egui: like the PlantUML front-end it
//! is a pure text transformation whose only output is the renderer-agnostic IR
//! in [`crate::diagram::model`].
//!
//! # Pipeline
//!
//! 1. [`strip_noise`] removes YAML front matter, `%%` comments, `%%{ .. }%%`
//!    directives, blank lines and `title` statements, and captures the title.
//! 2. [`detect_header`] classifies the first meaningful line.
//! 3. Exactly one `parse_*` function per diagram family turns the remaining
//!    lines into IR. Unrecognised headers fall back to the flowchart body
//!    parser so the user still sees whatever `A --> B` edges exist.
//!
//! Nothing here may panic on arbitrary user input: there is no `unsafe`, no
//! `unwrap`/`expect` on parsed data, and no byte slicing that could split a
//! UTF-8 character. Unsupported constructs are collected in
//! [`Graph::warnings`] / [`Sequence::warnings`] instead of failing.

use std::collections::BTreeSet;

use crate::diagram::model::{
    Arrow, Cluster, Diagram, DiagramKind, Direction, Edge, Graph, GroupKind, LineStyle, Node,
    NodeShape, NodeStyle, NotePosition, ParticipantKind, Pie, SeqStep, Sequence, Slice,
    parse_color,
};
use crate::gfx::Rgba;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Parse Mermaid source into the renderer-agnostic IR. Never panics.
pub fn parse(src: &str) -> Diagram {
    let (lines, title) = strip_noise(src);

    let Some(first) = lines.first() else {
        return Diagram::Graph(Graph {
            title,
            ..Graph::default()
        });
    };

    match detect_header(first) {
        Header::Flowchart(dir) => Diagram::Graph(flowchart_graph(
            &lines[1..],
            dir,
            Some(DiagramKind::Flowchart),
            title,
        )),
        Header::Sequence => Diagram::Sequence(sequence_parse(&lines[1..], title)),
        Header::Pie {
            show_data,
            title: pie_title,
        } => {
            let mut pie = pie_parse(&lines[1..]);
            pie.show_data = show_data;
            pie.title = pie_title.or(title);
            Diagram::Pie(pie)
        }
        Header::Class => Diagram::Graph(class_parse(&lines[1..], title)),
        Header::State => Diagram::Graph(state_parse(&lines[1..], title)),
        Header::Entity => Diagram::Graph(er_parse(&lines[1..], title)),
        Header::Mindmap => Diagram::Graph(mindmap_parse(src, title)),
        Header::Requirement => Diagram::Graph(requirement_parse(&lines[1..], title)),
        Header::Kanban => Diagram::Graph(kanban_parse(src, title)),
        Header::Unknown(name) => {
            let mut graph = if looks_like_edge(first) {
                flowchart_graph(&lines, Direction::TopDown, None, title)
            } else {
                let mut graph = flowchart_graph(&lines[1..], Direction::TopDown, None, title);
                graph
                    .warnings
                    .insert(0, format!("unsupported diagram type: {name}"));
                graph
            };
            graph.kind_hint = None;
            Diagram::Graph(graph)
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers: text cleanup and cursors
// ---------------------------------------------------------------------------

/// Keeps warnings unique and in first-seen order.
#[derive(Default)]
struct Notes {
    seen: BTreeSet<String>,
    order: Vec<String>,
}

impl Notes {
    fn warn(&mut self, msg: String) {
        if self.seen.insert(msg.clone()) {
            self.order.push(msg);
        }
    }

    fn into_vec(self) -> Vec<String> {
        self.order
    }
}

/// Remove front matter, comments, directives, blank lines and title statements.
fn strip_noise(src: &str) -> (Vec<String>, Option<String>) {
    let raw: Vec<&str> = src.lines().collect();
    let mut start = 0;
    while raw.get(start).is_some_and(|l| l.trim().is_empty()) {
        start += 1;
    }
    // Leading `---` delimited YAML front matter is skipped wholesale.
    if raw.get(start).map(|l| l.trim()) == Some("---") {
        start += 1;
        while raw.get(start).is_some_and(|l| l.trim() != "---") {
            start += 1;
        }
        if start < raw.len() {
            start += 1;
        }
    }

    let mut lines = Vec::new();
    let mut title = None;
    for line in raw.iter().skip(start) {
        let cleaned = strip_comment(line);
        let t = cleaned.trim();
        if t.is_empty() || t.starts_with("%%") {
            continue;
        }
        if let Some(rest) = title_line(t) {
            title = Some(rest);
            continue;
        }
        lines.push(t.to_owned());
    }
    (lines, title)
}

/// Cut a `%%` comment, but only where it starts a token (so `a%%b` survives).
fn strip_comment(line: &str) -> &str {
    let mut search = 0;
    while let Some(rel) = line.get(search..).and_then(|s| s.find("%%")) {
        let idx = search + rel;
        let at_boundary = line
            .get(..idx)
            .and_then(|head| head.chars().next_back())
            .is_none_or(|c| c.is_whitespace());
        if at_boundary {
            return line.get(..idx).unwrap_or("");
        }
        search = idx + 2;
    }
    line
}

/// Recognise `title: X` and `title X` statements.
fn title_line(t: &str) -> Option<String> {
    if let Some(rest) = t.strip_prefix("title:") {
        let rest = rest.trim();
        if !rest.is_empty() {
            return Some(strip_quotes(rest));
        }
    }
    if let Some(rest) = t.strip_prefix("title") {
        if rest.starts_with(|c: char| c.is_whitespace()) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(strip_quotes(rest));
            }
        }
    }
    None
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or("")
}

fn after_first_word(s: &str) -> &str {
    match s.find(|c: char| c.is_whitespace()) {
        Some(i) => s.get(i..).map(str::trim).unwrap_or(""),
        None => "",
    }
}

fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.find(|c: char| c.is_whitespace()) {
        Some(i) => (
            s.get(..i).unwrap_or(""),
            s.get(i..).map(str::trim).unwrap_or(""),
        ),
        None => (s, ""),
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

/// Remove one matching pair of surrounding quotes, if present.
fn strip_quote_pair(s: &str) -> Option<String> {
    let t = s.trim();
    for q in ['"', '\''] {
        if let Some(inner) = t.strip_prefix(q).and_then(|x| x.strip_suffix(q)) {
            return Some(inner.to_owned());
        }
    }
    None
}

fn strip_quotes(s: &str) -> String {
    match strip_quote_pair(s) {
        Some(inner) => inner,
        None => s.trim().to_owned(),
    }
}

/// Decode `<br/>`-style breaks, `\n` escapes and common HTML entities.
fn html_to_text(raw: &str) -> String {
    let mut out = raw.to_owned();
    for br in ["<br/>", "<br />", "<br>", "<BR/>", "<BR>", "<Br/>"] {
        out = out.replace(br, "\n");
    }
    out = out
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    out.replace("\\n", "\n")
}

/// Full label cleanup: trim, unquote, then decode escapes and entities.
fn clean_label(raw: &str) -> String {
    html_to_text(&strip_quotes(raw)).trim().to_owned()
}

fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('~');
    }
    out
}

/// Lookahead cursor over a line's characters. Position indexes are always
/// character indices, so no byte boundary can ever be split.
struct Cur<'a> {
    s: &'a [char],
    i: usize,
}

impl<'a> Cur<'a> {
    fn new(s: &'a [char]) -> Self {
        Self { s, i: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.i += 1;
        }
    }

    fn rest(&self) -> &'a [char] {
        if self.i <= self.s.len() {
            &self.s[self.i..]
        } else {
            &[]
        }
    }

    fn starts_with(&self, pat: &str) -> bool {
        seq_starts_with(self.rest(), pat)
    }

    fn advance(&mut self, n: usize) {
        self.i = (self.i + n).min(self.s.len());
    }
}

fn seq_starts_with(hay: &[char], needle: &str) -> bool {
    let mut it = hay.iter();
    for want in needle.chars() {
        match it.next() {
            Some(&got) if got == want => {}
            _ => return false,
        }
    }
    true
}

fn find_char(hay: &[char], needle: char) -> Option<usize> {
    hay.iter().position(|c| *c == needle)
}

fn find_seq(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from);
    }
    let mut i = from;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == *needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Read a Mermaid identifier (`[A-Za-z0-9_]`, with `-`/`.` only when followed
/// by an identifier character so `A-->B` still terminates at the first dash).
fn read_id(cur: &mut Cur) -> String {
    let mut out = String::new();
    while let Some(c) = cur.peek() {
        let word_char = c.is_alphanumeric() || c == '_';
        // `-`/`.` only join when they are followed by a word character, so
        // `A-->B` terminates the id at the first dash.
        let joiner = (c == '-' || c == '.')
            && cur
                .at(1)
                .is_some_and(|n| n.is_alphanumeric() || n == '_');
        if !word_char && !joiner {
            break;
        }
        out.push(c);
        cur.i += 1;
    }
    out
}

/// Consume `open` .. `close`, returning the inner text. Consumes nothing when
/// the closing sequence is absent.
fn try_delimited(cur: &mut Cur, open: &str, close: &str) -> Option<String> {
    if !cur.starts_with(open) {
        return None;
    }
    let open_len = open.chars().count();
    let start = cur.i + open_len;
    let close_chars: Vec<char> = close.chars().collect();
    let rel = find_seq(cur.s.get(start..)?, &close_chars, 0)?;
    let inner: String = cur.s.get(start..start + rel)?.iter().collect();
    cur.i = start + rel + close_chars.len();
    Some(inner)
}

/// Split on `sep`, ignoring separators inside single or double quotes.
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    cur.push(c);
                } else if c == sep {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.push(c);
                }
            }
        }
    }
    out.push(cur);
    out
}

// ---------------------------------------------------------------------------
// Header detection
// ---------------------------------------------------------------------------

enum Header {
    Flowchart(Direction),
    Sequence,
    Pie {
        show_data: bool,
        title: Option<String>,
    },
    Class,
    State,
    Entity,
    Mindmap,
    Requirement,
    Kanban,
    Unknown(String),
}

fn detect_header(first: &str) -> Header {
    let word = first_word(first);
    let rest = after_first_word(first);
    match word.to_ascii_lowercase().as_str() {
        "flowchart" | "graph" => {
            let dir = rest
                .split_whitespace()
                .next()
                .and_then(Direction::parse)
                .unwrap_or_default();
            Header::Flowchart(dir)
        }
        "sequencediagram" => Header::Sequence,
        "pie" => Header::Pie {
            show_data: rest
                .split_whitespace()
                .any(|w| w.eq_ignore_ascii_case("showdata")),
            title: pie_title(rest),
        },
        "classdiagram" | "classdiagram-v2" => Header::Class,
        "statediagram" | "statediagram-v2" => Header::State,
        "erdiagram" => Header::Entity,
        "mindmap" => Header::Mindmap,
        "requirementdiagram" => Header::Requirement,
        "kanban" => Header::Kanban,
        _ => Header::Unknown(word.to_owned()),
    }
}

/// `pie showData title X` — title is whatever follows the `title` keyword.
fn pie_title(rest: &str) -> Option<String> {
    let lower = rest.to_ascii_lowercase();
    let idx = lower.find("title")?;
    let tail = rest.get(idx + 5..)?.trim();
    if tail.is_empty() {
        None
    } else {
        Some(strip_quotes(tail))
    }
}

/// `true` when a line already carries an edge, i.e. it is body rather than an
/// unknown header.
fn looks_like_edge(s: &str) -> bool {
    s.contains("--") || s.contains("->") || s.contains("==") || s.contains("..")
}

// ---------------------------------------------------------------------------
// Style properties (`classDef`, `style`, `linkStyle`)
// ---------------------------------------------------------------------------

/// `fill:#f9f,stroke:#333,stroke-width:2px,color:#fff` as an incremental patch.
#[derive(Clone, Debug, Default)]
pub(crate) struct StyleProps {
    fill: Option<Rgba>,
    stroke: Option<Rgba>,
    text: Option<Rgba>,
    width: Option<f32>,
    dashed: Option<bool>,
}

impl StyleProps {
    fn parse(body: &str) -> Self {
        let mut p = Self::default();
        for part in split_top_level(body, ',') {
            let Some((key, value)) = part.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "fill" | "background" | "background-color" | "bgcolor" => {
                    p.fill = parse_color(value)
                }
                "stroke" | "border-color" | "bordercolor" => p.stroke = parse_color(value),
                "color" | "text-color" | "font-color" => p.text = parse_color(value),
                "stroke-width" | "line-width" | "border-width" => p.width = parse_px(value),
                "stroke-dasharray" => {
                    p.dashed = Some(!(value.is_empty() || value == "0" || value == "none"));
                }
                // Everything else (`font-weight`, `rx`, `ry`, `opacity`) has no
                // counterpart in the flat IR and is deliberately ignored.
                _ => {}
            }
        }
        p
    }

    /// Later declarations win, field by field.
    fn merge(&mut self, other: &Self) {
        if other.fill.is_some() {
            self.fill = other.fill;
        }
        if other.stroke.is_some() {
            self.stroke = other.stroke;
        }
        if other.text.is_some() {
            self.text = other.text;
        }
        if other.width.is_some() {
            self.width = other.width;
        }
        if other.dashed.is_some() {
            self.dashed = other.dashed;
        }
    }

    fn apply_node(&self, node: &mut Node) {
        if self.fill.is_some() {
            node.fill = self.fill;
        }
        if self.stroke.is_some() {
            node.stroke = self.stroke;
        }
        if self.text.is_some() {
            node.text_color = self.text;
        }
        if self.width.is_some() {
            node.stroke_width = self.width;
        }
    }

    fn apply_cluster(&self, cluster: &mut Cluster) {
        if self.fill.is_some() {
            cluster.fill = self.fill;
        }
        if self.stroke.is_some() {
            cluster.stroke = self.stroke;
        }
        if self.text.is_some() {
            cluster.text_color = self.text;
        }
    }

    fn apply_edge(&self, edge: &mut Edge) {
        if self.fill.is_some() {
            edge.color = self.fill;
        }
        if self.stroke.is_some() {
            edge.color = self.stroke;
        }
        if self.text.is_some() {
            // A coloured edge label is painted with the label pill's own colour
            // in this renderer, so the text colour rides along with the stroke.
            edge.color = edge.color.or(self.text);
        }
        if self.width.is_some() {
            edge.width = self.width;
        }
        if self.dashed == Some(true) {
            edge.line = LineStyle::Dashed;
        }
    }
}

/// `2px`, `1.5` and `3` all mean points in this renderer.
fn parse_px(raw: &str) -> Option<f32> {
    let t = raw.trim().trim_end_matches("px").trim();
    t.parse::<f32>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

/// One deferred styling statement, applied after the body is parsed.
enum StyleDirective {
    /// `class A,B name` and `A:::name`: node ids plus the class names to use.
    Classes(Vec<String>, Vec<String>),
    /// `style target fill:...`, where `target` may be a node *or* a subgraph id.
    Inline(String, StyleProps),
    /// `linkStyle 0,2 ...`; `None` means the `default` form.
    Links(Option<Vec<usize>>, StyleProps),
}

// ---------------------------------------------------------------------------
// Flowchart
// ---------------------------------------------------------------------------

/// A node occurrence found while scanning a statement.
struct PendingNode {
    id: String,
    shape: Option<NodeShape>,
    label: Option<String>,
    warn: Option<String>,
    /// `A:::className` class suffixes.
    classes: Vec<String>,
}

/// A parsed link token together with how many characters it spans.
struct Link {
    len: usize,
    head: Arrow,
    tail: Arrow,
    line: LineStyle,
    label: Option<String>,
    /// `~~~`: laid out but never drawn.
    hidden: bool,
}

struct FlowParser {
    graph: Graph,
    notes: Notes,
    /// Indices of currently open subgraphs, innermost last.
    stack: Vec<usize>,
    auto: usize,
    /// `classDef name prop:value,...` definitions, in source order.
    class_defs: Vec<(String, StyleProps)>,
    /// Styling statements deferred until the body (and its nodes) exists.
    directives: Vec<StyleDirective>,
}

/// Build a flowchart-style graph. `kind` is `None` for the unrecognised-header
/// fallback.
fn flowchart_graph(
    lines: &[String],
    direction: Direction,
    kind: Option<DiagramKind>,
    title: Option<String>,
) -> Graph {
    let mut parser = FlowParser {
        graph: Graph {
            kind_hint: kind,
            direction,
            title,
            ..Graph::default()
        },
        notes: Notes::default(),
        stack: Vec::new(),
        auto: 0,
        class_defs: Vec::new(),
        directives: Vec::new(),
    };
    for line in lines {
        parser.stmt(line);
    }
    parser.apply_directives();
    let warnings = parser.notes.into_vec();
    parser.graph.warnings = warnings;
    parser.graph
}

impl FlowParser {
    fn stmt(&mut self, line: &str) {
        let t = line.trim();
        if t.is_empty() {
            return;
        }
        let word = first_word(t).to_ascii_lowercase();
        match word.as_str() {
            "end" => {
                self.stack.pop();
                return;
            }
            "subgraph" => {
                self.push_subgraph(after_first_word(t));
                return;
            }
            "direction" => {
                // Only meaningful at the top level for our IR.
                if self.stack.is_empty() {
                    if let Some(d) = Direction::parse(after_first_word(t)) {
                        self.graph.direction = d;
                    }
                }
                return;
            }
            "classdef" => {
                self.class_def(after_first_word(t));
                return;
            }
            "class" => {
                self.class_assign(after_first_word(t));
                return;
            }
            "style" => {
                self.style_stmt(after_first_word(t));
                return;
            }
            "linkstyle" => {
                self.link_style(after_first_word(t));
                return;
            }
            "click" | "acctitle" | "accdescr" | "acc_title" | "acc_descr" => {
                // Accessibility text and click handlers have no visual effect in
                // this renderer; nothing to warn about.
                return;
            }
            _ => {}
        }
        self.edge_stmt(t);
    }

    // -- styling directives -------------------------------------------------

    /// `classDef important fill:#f9f,stroke:#333,stroke-width:4px`
    fn class_def(&mut self, rest: &str) {
        let (names, props) = split_word(rest);
        if names.is_empty() {
            self.notes.warn("classDef 缺少类名".to_owned());
            return;
        }
        let props = StyleProps::parse(props);
        for name in names.split(',') {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            // A later definition of the same class replaces the earlier one.
            match self.class_defs.iter_mut().find(|(n, _)| n == name) {
                Some(slot) => slot.1 = props.clone(),
                None => self.class_defs.push((name.to_owned(), props.clone())),
            }
        }
    }

    /// `class A,B important` — attach one or more classes to existing nodes.
    fn class_assign(&mut self, rest: &str) {
        let rest = rest.trim();
        let Some((ids, names)) = rest.rsplit_once(char::is_whitespace) else {
            self.notes.warn(format!("class 语句缺少类名: {rest}"));
            return;
        };
        let ids: Vec<String> = ids
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let names: Vec<String> = names
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if ids.is_empty() || names.is_empty() {
            self.notes.warn(format!("class 语句不完整: {rest}"));
            return;
        }
        self.directives.push(StyleDirective::Classes(ids, names));
    }

    /// `style A fill:#f9f,stroke:#333` (`A` may also be a subgraph id).
    fn style_stmt(&mut self, rest: &str) {
        let (target, props) = split_word(rest);
        let target = strip_quotes(target);
        if target.is_empty() {
            self.notes.warn("style 缺少目标".to_owned());
            return;
        }
        self.directives
            .push(StyleDirective::Inline(target, StyleProps::parse(props)));
    }

    /// `linkStyle 0,1 stroke:#f00` or `linkStyle default stroke:#f00`.
    fn link_style(&mut self, rest: &str) {
        let (head, props) = split_word(rest);
        let props = StyleProps::parse(props);
        let indices = if head.eq_ignore_ascii_case("default") {
            None
        } else {
            let list: Vec<usize> = head
                .split(',')
                .filter_map(|s| s.trim().parse::<usize>().ok())
                .collect();
            if list.is_empty() {
                self.notes.warn(format!("linkStyle 缺少连线下标: {head}"));
                return;
            }
            Some(list)
        };
        self.directives.push(StyleDirective::Links(indices, props));
    }

    /// Apply every deferred styling statement now that the nodes exist.
    fn apply_directives(&mut self) {
        let directives = std::mem::take(&mut self.directives);
        for directive in directives {
            match directive {
                StyleDirective::Classes(ids, names) => {
                    let mut props = StyleProps::default();
                    let mut known = false;
                    for name in &names {
                        match self.class_defs.iter().find(|(n, _)| n == name) {
                            Some((_, p)) => {
                                props.merge(p);
                                known = true;
                            }
                            None => self.notes.warn(format!("未定义的 classDef: {name}")),
                        }
                    }
                    if !known {
                        continue;
                    }
                    for id in ids {
                        match self.graph.index_of(&id) {
                            Some(idx) => props.apply_node(&mut self.graph.nodes[idx]),
                            None => self.notes.warn(format!("class 目标不存在: {id}")),
                        }
                    }
                }
                StyleDirective::Inline(target, props) => {
                    if let Some(idx) = self.graph.index_of(&target) {
                        props.apply_node(&mut self.graph.nodes[idx]);
                    } else if let Some(ci) =
                        self.graph.clusters.iter().position(|c| c.id == target)
                    {
                        props.apply_cluster(&mut self.graph.clusters[ci]);
                    } else {
                        self.notes.warn(format!("style 目标不存在: {target}"));
                    }
                }
                StyleDirective::Links(indices, props) => match indices {
                    None => {
                        for edge in &mut self.graph.edges {
                            props.apply_edge(edge);
                        }
                    }
                    Some(list) => {
                        for i in list {
                            match self.graph.edges.get_mut(i) {
                                Some(edge) => props.apply_edge(edge),
                                None => self
                                    .notes
                                    .warn(format!("linkStyle 下标越界: {i}")),
                            }
                        }
                    }
                },
            }
        }

        // `classDef default` styles everything that no class claimed.
        if let Some((_, props)) = self.class_defs.iter().find(|(n, _)| n == "default") {
            let props = props.clone();
            for node in &mut self.graph.nodes {
                if node.fill.is_none() && node.stroke.is_none() && node.text_color.is_none() {
                    props.apply_node(node);
                }
            }
        }
    }

    fn push_subgraph(&mut self, spec: &str) {
        let (mut id, label) = self.subgraph_names(spec);
        if self.graph.clusters.iter().any(|c| c.id == id) {
            id = format!("{id}#{}", self.graph.clusters.len());
        }
        let parent = self.stack.last().copied();
        let idx = self.graph.clusters.len();
        self.graph.clusters.push(Cluster {
            id,
            label,
            members: Vec::new(),
            parent,
                ..Default::default()
        });
        self.stack.push(idx);
    }

    fn subgraph_names(&mut self, spec: &str) -> (String, String) {
        let spec = spec.trim();
        if spec.is_empty() {
            self.auto += 1;
            return (format!("subgraph-{}", self.auto), "subgraph".to_owned());
        }
        if let Some(open) = spec.find('[') {
            if let Some(close) = spec.get(open..).and_then(|s| s.find(']')) {
                let id = spec.get(..open).unwrap_or("").trim();
                let title = strip_quotes(spec.get(open + 1..open + close).unwrap_or(""));
                if !id.is_empty() {
                    let label = if title.is_empty() {
                        id.to_owned()
                    } else {
                        title
                    };
                    return (id.to_owned(), label);
                }
                if !title.is_empty() {
                    return (title.clone(), title);
                }
            }
        }
        if let Some(inner) = strip_quote_pair(spec) {
            return (inner.clone(), inner);
        }
        let (id, label) = split_word(spec);
        if id.is_empty() {
            self.auto += 1;
            return (format!("subgraph-{}", self.auto), "subgraph".to_owned());
        }
        let label = if label.is_empty() {
            id.to_owned()
        } else {
            strip_quotes(label)
        };
        (id.to_owned(), label)
    }

    /// Parse `A --> B --> C`, `A & B --> C & D`, lone ids and shaped nodes.
    fn edge_stmt(&mut self, t: &str) {
        let chars: Vec<char> = t.chars().collect();
        let mut cur = Cur::new(&chars);
        cur.skip_ws();
        let Some(group) = parse_node_group(&mut cur) else {
            return;
        };
        let mut prev: Vec<String> = Vec::new();
        for p in group {
            prev.push(p.id.clone());
            self.register(p);
        }
        loop {
            cur.skip_ws();
            if cur.eof() {
                break;
            }
            let Some(link) = read_link(cur.rest()) else {
                break;
            };
            if link.len == 0 {
                break;
            }
            cur.advance(link.len);
            cur.skip_ws();
            let Some(next) = parse_node_group(&mut cur) else {
                break;
            };
            let mut ids: Vec<String> = Vec::new();
            for p in next {
                ids.push(p.id.clone());
                self.register(p);
            }
            for from in &prev {
                for to in &ids {
                    self.add_edge(from, to, &link);
                }
            }
            prev = ids;
        }
    }

    fn register(&mut self, p: PendingNode) {
        if let Some(w) = &p.warn {
            self.notes.warn(w.clone());
        }
        if !p.classes.is_empty() {
            self.directives.push(StyleDirective::Classes(
                vec![p.id.clone()],
                p.classes.clone(),
            ));
        }
        let idx = match self.graph.index_of(&p.id) {
            Some(i) => i,
            None => {
                let mut node = Node::new(p.id.clone(), p.id.clone());
                if let Some(&c) = self.stack.last() {
                    node.cluster = Some(c);
                }
                self.graph.nodes.push(node);
                self.graph.nodes.len() - 1
            }
        };
        if let Some(shape) = p.shape {
            self.graph.nodes[idx].shape = shape;
        }
        if let Some(label) = p.label {
            self.graph.nodes[idx].label = label;
        }
        // Every enclosing subgraph lists the node; the innermost owns it.
        let stack = self.stack.clone();
        for c in stack {
            let Some(cluster) = self.graph.clusters.get_mut(c) else {
                continue;
            };
            if !cluster.members.iter().any(|m| m == &p.id) {
                cluster.members.push(p.id.clone());
            }
        }
    }

    fn add_edge(&mut self, from: &str, to: &str, link: &Link) {
        let mut edge = Edge::new(from, to);
        edge.head = link.head;
        edge.tail = link.tail;
        edge.line = link.line;
        edge.hidden = link.hidden;
        if let Some(label) = &link.label {
            edge.label = Some(label.clone());
            edge.label_pad = 4.0;
        }
        self.graph.edges.push(edge);
    }
}

/// Parse one or more nodes separated by `&`.
fn parse_node_group(cur: &mut Cur) -> Option<Vec<PendingNode>> {
    let mut out = Vec::new();
    loop {
        cur.skip_ws();
        match parse_node(cur) {
            Some(node) => out.push(node),
            None => break,
        }
        cur.skip_ws();
        if cur.peek() == Some('&') {
            cur.bump_amp();
            continue;
        }
        break;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Parse a single `id[shape]:::class` occurrence.
fn parse_node(cur: &mut Cur) -> Option<PendingNode> {
    cur.skip_ws();
    let id = read_id(cur);
    if id.is_empty() {
        return None;
    }
    let mut pending = PendingNode {
        id,
        shape: None,
        label: None,
        warn: None,
        classes: Vec::new(),
    };

    if cur.starts_with("@{") {
        let rest = cur.rest();
        match find_char(rest, '}') {
            Some(close) => {
                let body: String = rest.get(2..close).unwrap_or(&[]).iter().collect();
                let (shape, label, unknown) = parse_palette(&body);
                if shape.is_some() {
                    pending.shape = shape;
                }
                if let Some(label) = label {
                    pending.label = Some(clean_label(&label));
                }
                if let Some(name) = unknown {
                    pending.warn = Some(format!("unknown node shape in @{{}}: {name}"));
                }
                cur.advance(close + 1);
            }
            None => {
                let len = rest.len();
                cur.advance(len);
            }
        }
    } else if let Some((shape, inner)) = read_shape(cur) {
        pending.shape = Some(shape);
        pending.label = Some(clean_label(&inner));
    }

    // `:::class` attaches a `classDef` style to this node.
    while cur.starts_with(":::") || cur.starts_with("::") {
        let skip = if cur.starts_with(":::") { 3 } else { 2 };
        cur.advance(skip);
        let name = read_id(cur);
        let name = name.trim().to_owned();
        if !name.is_empty() {
            pending.classes.push(name);
        }
    }
    Some(pending)
}

impl<'a> Cur<'a> {
    fn bump_amp(&mut self) {
        if self.peek() == Some('&') {
            self.i += 1;
        }
    }
}

/// Map a shape's opening/closing delimiter pair onto [`NodeShape`].
fn read_shape(cur: &mut Cur) -> Option<(NodeShape, String)> {
    let forms: [(&str, &str, NodeShape); 12] = [
        ("(((", ")))", NodeShape::Circle),
        ("((", "))", NodeShape::Circle),
        ("([", "])", NodeShape::Stadium),
        ("[(", ")]", NodeShape::Cylinder),
        ("{{", "}}", NodeShape::Hexagon),
        ("[/", "/]", NodeShape::Parallelogram),
        ("[/", "\\]", NodeShape::Parallelogram),
        ("[\\", "/]", NodeShape::Parallelogram),
        ("[", "]", NodeShape::Rect),
        ("(", ")", NodeShape::Round),
        ("{", "}", NodeShape::Diamond),
        (">", "]", NodeShape::Asymmetric),
    ];
    for (open, close, shape) in forms {
        if let Some(inner) = try_delimited(cur, open, close) {
            return Some((shape, inner));
        }
    }
    None
}

fn map_shape_name(name: &str) -> Option<NodeShape> {
    match name.trim().to_ascii_lowercase().as_str() {
        "rect" | "rectangle" | "proc" | "process" => Some(NodeShape::Rect),
        "rounded" | "round" => Some(NodeShape::Round),
        "stadium" | "pill" => Some(NodeShape::Stadium),
        "circle" | "circ" => Some(NodeShape::Circle),
        "diam" | "diamond" | "decision" | "rhombus" => Some(NodeShape::Diamond),
        "hex" | "hexagon" => Some(NodeShape::Hexagon),
        "lean-r" | "lean-l" | "para" | "parallelogram" => Some(NodeShape::Parallelogram),
        "cyl" | "cylinder" | "database" | "db" => Some(NodeShape::Cylinder),
        "tri" | "asymmetric" => Some(NodeShape::Asymmetric),
        "classifier" => Some(NodeShape::Classifier),
        "note" => Some(NodeShape::Note),
        _ => None,
    }
}

/// Parse the body of an `A@{ shape: rect, label: "x" }` palette node.
fn parse_palette(body: &str) -> (Option<NodeShape>, Option<String>, Option<String>) {
    let mut shape = None;
    let mut label = None;
    let mut unknown = None;
    for part in split_top_level(body, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "shape" => {
                let name = strip_quotes(value);
                match map_shape_name(&name) {
                    Some(s) => shape = Some(s),
                    None => unknown = Some(name),
                }
            }
            "label" => label = Some(strip_quotes(value)),
            _ => {}
        }
    }
    (shape, label, unknown)
}

/// Read a link operator starting at the head of `s`, including inline labels
/// (`-- text -->`), pipe labels (`-->|text|`) and `<`/`o`/`x` decorations.
fn read_link(s: &[char]) -> Option<Link> {
    if s.is_empty() {
        return None;
    }
    // `~~~` is an invisible link: it only shapes the layout.
    if s.first() == Some(&'~') {
        let mut i = 0;
        while s.get(i) == Some(&'~') {
            i += 1;
        }
        let mut label = None;
        // `~~~ text ~~~` carries a label that is never painted.
        if s.get(i).is_some_and(|c| c.is_whitespace()) {
            if let Some(close) = find_seq(s, &['~', '~', '~'], i) {
                let text: String = s.get(i..close).unwrap_or(&[]).iter().collect();
                let text = text.trim();
                if !text.is_empty() {
                    label = Some(clean_label(text));
                }
                i = close + 3;
            }
        }
        return Some(Link {
            len: i,
            head: Arrow::None,
            tail: Arrow::None,
            line: LineStyle::Solid,
            label,
            hidden: true,
        });
    }
    let mut i = 0;
    let mut tail = Arrow::None;
    if s.first() == Some(&'<') {
        tail = Arrow::Filled;
        i = 1;
    } else if let Some(c) = s.first().copied() {
        if (c == 'o' || c == 'x') && s.get(1).is_some_and(|n| *n == '-' || *n == '.') {
            tail = if c == 'o' { Arrow::Circle } else { Arrow::Cross };
            i = 1;
        }
    }

    let body_start = i;
    let mut saw_dot = false;
    while let Some(&c) = s.get(i) {
        if c == '-' || c == '.' || c == '=' {
            if c == '.' {
                saw_dot = true;
            }
            i += 1;
        } else {
            break;
        }
    }
    if i == body_start {
        return None;
    }

    let mut head = Arrow::None;
    if let Some(&c) = s.get(i) {
        match c {
            '>' => {
                head = Arrow::Filled;
                i += 1;
            }
            'o' => {
                head = Arrow::Circle;
                i += 1;
            }
            'x' => {
                head = Arrow::Cross;
                i += 1;
            }
            _ => {}
        }
    }

    // `-->|label|`
    let mut label = None;
    if head != Arrow::None && s.get(i) == Some(&'|') {
        if let Some(rel) = find_char(s.get(i + 1..).unwrap_or(&[]), '|') {
            let inner: String = s.get(i + 1..i + 1 + rel).unwrap_or(&[]).iter().collect();
            label = Some(clean_label(&inner));
            i = i + rel + 2;
        }
    }

    // `-- text -->`: a body, whitespace, the label, then another link token.
    if head == Arrow::None && label.is_none() && s.get(i).is_some_and(|c| c.is_whitespace()) {
        if let Some((start, rest)) = find_trailing_link(s, i) {
            let inner: String = s.get(i..start).unwrap_or(&[]).iter().collect();
            let text = inner.trim();
            if !text.is_empty() {
                let dashed = saw_dot || rest.line == LineStyle::Dashed;
                return Some(Link {
                    len: start + rest.len,
                    head: rest.head,
                    tail,
                    line: if dashed {
                        LineStyle::Dashed
                    } else {
                        LineStyle::Solid
                    },
                    label: Some(clean_label(text)),
                    hidden: false,
                });
            }
        }
    }

    Some(Link {
        len: i,
        head,
        tail,
        line: if saw_dot {
            LineStyle::Dashed
        } else {
            LineStyle::Solid
        },
        label,
        hidden: false,
    })
}

/// Find the terminating link of an inline label form, starting at `from`.
fn find_trailing_link(s: &[char], from: usize) -> Option<(usize, Link)> {
    let mut k = from;
    while k < s.len() {
        let c = s[k];
        let after_space = k > from
            && s.get(k.wrapping_sub(1))
                .is_some_and(|p| p.is_whitespace());
        if (c == '-' || c == '.' || c == '=') && after_space {
            if let Some(link) = read_link(s.get(k..).unwrap_or(&[])) {
                if link.len > 0 {
                    return Some((k, link));
                }
            }
        }
        k += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// sequenceDiagram
// ---------------------------------------------------------------------------

struct SeqParser {
    seq: Sequence,
    notes: Notes,
    depth: usize,
    autonumber: bool,
    next_index: u32,
}

fn sequence_parse(lines: &[String], title: Option<String>) -> Sequence {
    let mut parser = SeqParser {
        seq: Sequence {
            title,
            ..Sequence::default()
        },
        notes: Notes::default(),
        depth: 0,
        autonumber: false,
        next_index: 1,
    };
    for line in lines {
        parser.stmt(line.trim());
    }
    let warnings = parser.notes.into_vec();
    parser.seq.warnings = warnings;
    parser.seq
}

impl SeqParser {
    fn stmt(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        let lower = first_word(t).to_ascii_lowercase();
        match lower.as_str() {
            "autonumber" => {
                // `autonumber off` stops numbering; a bare `autonumber` starts it.
                let rest = after_first_word(t).trim().to_ascii_lowercase();
                self.autonumber = rest != "off";
                return;
            }
            "end" => {
                self.group_close();
                return;
            }
            "activate" => {
                self.set_activation(after_first_word(t), true);
                return;
            }
            "deactivate" | "destroy" => {
                self.set_activation(after_first_word(t), false);
                return;
            }
            "create" => {
                // `create participant D` brings the participant in at this
                // point; this renderer draws lifelines for the whole diagram, so
                // the declaration is enough.
                let rest = after_first_word(t);
                let kind = first_word(rest).to_ascii_lowercase();
                if matches!(
                    kind.as_str(),
                    "participant" | "actor" | "database" | "queue" | "collections" | "boundary"
                        | "control" | "entity"
                ) {
                    self.declare(&kind, after_first_word(rest));
                }
                return;
            }
            "loop" | "alt" | "opt" | "par" | "break" | "critical" | "rect" => {
                let kind = group_kind(&lower);
                self.group_open(kind, after_first_word(t));
                return;
            }
            "else" | "and" | "option" => {
                self.group_else(after_first_word(t));
                return;
            }
            "participant" | "actor" | "database" | "queue" | "collections" | "boundary"
            | "control" | "entity" => {
                self.declare(&lower, after_first_word(t));
                return;
            }
            "note" => {
                self.note(t);
                return;
            }
            "box" | "links" | "link" | "acctitle" | "accdescr" | "properties" | "details"
            | "linkstyle" | "style" => {
                return;
            }
            _ => {}
        }

        if let Some(inner) = divider_text(t) {
            self.seq.steps.push(SeqStep::Divider {
                label: strip_quotes(inner),
            });
            return;
        }
        if let Some(inner) = delay_text(t) {
            self.seq.steps.push(SeqStep::Delay {
                text: strip_quotes(inner),
            });
            return;
        }
        if self.message(t) {
            return;
        }
        self.notes
            .warn(format!("ignored sequence statement: {}", truncate(t, 40)));
    }

    /// Insert (or fetch) a participant and return its index.
    fn participant(&mut self, id: &str) -> usize {
        let _ = self.seq.participant_mut(id);
        self.seq.index_of(id).unwrap_or(0)
    }

    fn declare(&mut self, keyword: &str, rest: &str) {
        let (id, label) = split_as(rest);
        if id.is_empty() {
            return;
        }
        let kind = match keyword {
            "actor" => ParticipantKind::Actor,
            "database" => ParticipantKind::Store,
            "queue" => ParticipantKind::Queue,
            "collections" => ParticipantKind::Collections,
            "boundary" => ParticipantKind::Boundary,
            "control" => ParticipantKind::Control,
            "entity" => ParticipantKind::Entity,
            _ => ParticipantKind::Default,
        };
        let idx = self.participant(&id);
        if let Some(p) = self.seq.participants.get_mut(idx) {
            p.kind = kind;
            p.label = if label.is_empty() { id } else { label };
            p.declared = true;
        }
    }

    fn set_activation(&mut self, id: &str, on: bool) {
        let id = id.trim();
        if id.is_empty() {
            return;
        }
        let participant = self.participant(id);
        self.seq.steps.push(SeqStep::Activate { participant, on });
    }

    fn group_open(&mut self, kind: GroupKind, label: &str) {
        self.seq.steps.push(SeqStep::GroupOpen {
            kind,
            label: strip_quotes(label),
            depth: self.depth,
        });
        self.depth += 1;
    }

    fn group_else(&mut self, label: &str) {
        if self.depth == 0 {
            return;
        }
        self.seq.steps.push(SeqStep::GroupElse {
            label: strip_quotes(label),
            depth: self.depth - 1,
        });
    }

    fn group_close(&mut self) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        self.seq.steps.push(SeqStep::GroupClose { depth: self.depth });
    }

    fn note(&mut self, t: &str) {
        let rest = after_first_word(t);
        let (word, after) = split_word(rest);
        let position = match word.to_ascii_lowercase().as_str() {
            "left" => NotePosition::Left,
            "right" => NotePosition::Right,
            "over" => NotePosition::Over,
            _ => return,
        };
        let mut after = after.trim();
        if let Some(r) = strip_prefix_ci(after, "of") {
            if r.is_empty() || r.starts_with(|c: char| c.is_whitespace()) {
                after = r.trim();
            }
        }
        let (ids_part, text) = match after.find(':') {
            Some(i) => (
                after.get(..i).unwrap_or("").trim(),
                after.get(i + 1..).unwrap_or("").trim(),
            ),
            None => (after, ""),
        };
        let mut over = Vec::new();
        for id in ids_part
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            over.push(self.participant(id));
        }
        if over.is_empty() {
            return;
        }
        self.seq.steps.push(SeqStep::Note {
            over,
            position,
            text: strip_quotes(text),
        });
    }

    fn message(&mut self, t: &str) -> bool {
        let Some((dash, token, head, line)) = find_arrow(t) else {
            return false;
        };
        let from = t.get(..dash).unwrap_or("").trim().to_owned();
        let rest = t.get(dash + token.len()..).unwrap_or("");
        let (to_raw, text) = match rest.find(':') {
            Some(i) => (
                rest.get(..i).unwrap_or("").trim(),
                rest.get(i + 1..).unwrap_or("").trim(),
            ),
            None => (rest.trim(), ""),
        };
        let mut to = to_raw.to_owned();
        let mut activate = false;
        let mut deactivate = false;
        if let Some(r) = to.strip_prefix('+') {
            activate = true;
            to = r.trim().to_owned();
        } else if let Some(r) = to.strip_prefix('-') {
            deactivate = true;
            to = r.trim().to_owned();
        }
        if from.is_empty() || to.is_empty() {
            return false;
        }

        let index = self.next_index;
        self.next_index += 1;
        let label = if self.autonumber {
            let text = strip_quotes(text);
            if text.is_empty() {
                format!("{index}.")
            } else {
                format!("{index}. {text}")
            }
        } else {
            strip_quotes(text)
        };

        let from_idx = self.participant(&from);
        let to_idx = self.participant(&to);
        self.seq.steps.push(SeqStep::Message {
            index,
            from: from_idx,
            to: to_idx,
            label,
            line,
            head,
            activate,
            deactivate,
            self_call: from_idx == to_idx,
        });
        true
    }
}

fn group_kind(word: &str) -> GroupKind {
    match word {
        "loop" => GroupKind::Loop,
        "alt" => GroupKind::Alt,
        "opt" => GroupKind::Opt,
        "par" => GroupKind::Par,
        "break" => GroupKind::Break,
        "critical" => GroupKind::Critical,
        "rect" => GroupKind::Rect,
        _ => GroupKind::Group,
    }
}

const ARROWS: &[(&str, Arrow, LineStyle)] = &[
    ("-->>", Arrow::Open, LineStyle::Dashed),
    ("->>", Arrow::Filled, LineStyle::Solid),
    ("-->", Arrow::Open, LineStyle::Dashed),
    ("--x", Arrow::Cross, LineStyle::Dashed),
    ("--)", Arrow::Open, LineStyle::Dashed),
    ("--", Arrow::None, LineStyle::Solid),
    ("-x", Arrow::Cross, LineStyle::Solid),
    ("-)", Arrow::Open, LineStyle::Solid),
    ("->", Arrow::Filled, LineStyle::Solid),
    ("-", Arrow::None, LineStyle::Solid),
];

fn find_arrow(t: &str) -> Option<(usize, &'static str, Arrow, LineStyle)> {
    let mut search = 0;
    while let Some(rel) = t.get(search..).and_then(|s| s.find('-')) {
        let idx = search + rel;
        let tail = t.get(idx..)?;
        for &(token, head, line) in ARROWS {
            if tail.starts_with(token) {
                return Some((idx, token, head, line));
            }
        }
        search = idx + 1;
    }
    None
}

fn divider_text(t: &str) -> Option<&str> {
    let long_enough = t.chars().count() >= 4;
    let (starts, ends) = (t.starts_with("=="), t.ends_with("=="));
    if long_enough && starts && ends {
        return t.get(2..t.len() - 2);
    }
    if long_enough && t.starts_with("--") && t.ends_with("--") {
        return t.get(2..t.len() - 2);
    }
    None
}

fn delay_text(t: &str) -> Option<&str> {
    if t.chars().count() >= 6 && t.starts_with("...") && t.ends_with("...") {
        return t.get(3..t.len() - 3);
    }
    None
}

fn split_as(rest: &str) -> (String, String) {
    let lower = rest.to_ascii_lowercase();
    if let Some(i) = lower.find(" as ") {
        let id = rest.get(..i).unwrap_or("").trim();
        let label = strip_quotes(rest.get(i + 4..).unwrap_or(""));
        return (id.to_owned(), label);
    }
    (strip_quotes(rest.trim()), String::new())
}

// ---------------------------------------------------------------------------
// pie
// ---------------------------------------------------------------------------

fn pie_parse(lines: &[String]) -> Pie {
    let mut pie = Pie::default();
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lower = first_word(t).to_ascii_lowercase();
        if lower == "title" {
            let rest = after_first_word(t);
            if !rest.is_empty() {
                pie.title = Some(strip_quotes(rest));
            }
            continue;
        }
        if lower == "showdata" {
            pie.show_data = true;
            continue;
        }
        for part in split_top_level(t, ',') {
            let Some(idx) = part.rfind(':') else {
                continue;
            };
            let label = part.get(..idx).unwrap_or("").trim();
            let value = part.get(idx + 1..).unwrap_or("").trim();
            if label.is_empty() {
                continue;
            }
            if let Ok(v) = value.parse::<f32>() {
                pie.slices.push(Slice {
                    label: strip_quotes(label),
                    value: v,
                });
            }
        }
    }
    pie
}

// ---------------------------------------------------------------------------
// classDiagram
// ---------------------------------------------------------------------------

fn class_parse(lines: &[String], title: Option<String>) -> Graph {
    let mut graph = Graph {
        kind_hint: Some(DiagramKind::Class),
        title,
        ..Graph::default()
    };
    let mut notes = Notes::default();
    let mut stack: Vec<usize> = Vec::new();
    let mut body: Option<usize> = None;

    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        i += 1;
        if t.is_empty() {
            continue;
        }
        if t == "}" {
            if body.take().is_none() {
                stack.pop();
            }
            continue;
        }
        if let Some(idx) = body {
            match t.find('}') {
                Some(close) => {
                    // `add_member` ignores blank input, so no emptiness guard.
                    let member = t.get(..close).unwrap_or("").trim();
                    if let Some(node) = graph.nodes.get_mut(idx) {
                        add_member(node, member);
                    }
                    body = None;
                }
                None => {
                    if let Some(node) = graph.nodes.get_mut(idx) {
                        add_member(node, t);
                    }
                }
            }
            continue;
        }

        let lower = first_word(t).to_ascii_lowercase();
        match lower.as_str() {
            "direction" => {
                if let Some(d) = Direction::parse(after_first_word(t)) {
                    graph.direction = d;
                }
                continue;
            }
            "namespace" | "package" => {
                push_cluster(&mut graph, &mut stack, after_first_word(t), after_first_word(t));
                continue;
            }
            "class" => {
                class_decl(&mut graph, &mut stack, &mut body, after_first_word(t), &mut notes);
                continue;
            }
            "note" | "style" | "click" | "callback" => {
                notes.warn(format!("ignored class directive: {lower}"));
                continue;
            }
            _ => {}
        }

        if t.starts_with("<<") {
            apply_annotation(&mut graph, t, &stack);
            continue;
        }
        if let Some((left, right, op, label)) = parse_class_relation(t) {
            ensure_node(
                &mut graph,
                &left,
                NodeShape::Classifier,
                NodeStyle::Default,
                &stack,
            );
            ensure_node(
                &mut graph,
                &right,
                NodeShape::Classifier,
                NodeStyle::Default,
                &stack,
            );
            graph.edges.push(class_edge(&left, &right, &op, label));
            continue;
        }
        if let Some(colon) = t.find(':') {
            let left = t.get(..colon).unwrap_or("").trim();
            let right = t.get(colon + 1..).unwrap_or("").trim();
            if !left.is_empty() && !right.is_empty() && !left.contains(|c: char| c.is_whitespace()) {
                let idx = ensure_node(
                    &mut graph,
                    left,
                    NodeShape::Classifier,
                    NodeStyle::Default,
                    &stack,
                );
                if let Some(node) = graph.nodes.get_mut(idx) {
                    add_member(node, right);
                }
                continue;
            }
        }
        notes.warn(format!("ignored class statement: {}", truncate(t, 40)));
    }

    graph.warnings = notes.into_vec();
    graph
}

/// `class X`, `class X {`, `class X <<interface>>`.
fn class_decl(
    graph: &mut Graph,
    stack: &mut Vec<usize>,
    body: &mut Option<usize>,
    rest: &str,
    notes: &mut Notes,
) {
    let chars: Vec<char> = rest.chars().collect();
    let mut cur = Cur::new(&chars);
    cur.skip_ws();
    let id = read_id(&mut cur);
    if id.is_empty() {
        notes.warn("ignored empty class declaration".to_owned());
        return;
    }
    cur.skip_ws();
    let tail: String = cur.rest().iter().collect();
    let tail = tail.trim();

    let annotation = tail
        .strip_prefix("<<")
        .and_then(|s| s.split_once(">>").map(|(a, _)| a.trim().to_owned()));

    let idx = ensure_node(
        graph,
        &id,
        NodeShape::Classifier,
        NodeStyle::Default,
        stack,
    );
    if let Some(annotation) = annotation {
        if let Some(node) = graph.nodes.get_mut(idx) {
            apply_style(node, &annotation);
        }
    }
    if let Some(open) = tail.find('{') {
        match tail.get(open + 1..).and_then(|s| s.find('}')) {
            Some(close) => {
                let inner = tail.get(open + 1..open + 1 + close).unwrap_or("").trim();
                if let Some(node) = graph.nodes.get_mut(idx) {
                    add_member(node, inner);
                }
            }
            None => *body = Some(idx),
        }
    }
}

/// `<<interface>> X` or `<<abstract>> X, Y`.
fn apply_annotation(graph: &mut Graph, t: &str, stack: &[usize]) {
    let Some(end) = t.find(">>") else {
        return;
    };
    let annotation = t.get(2..end).unwrap_or("").trim();
    let ids = t.get(end + 2..).unwrap_or("").trim();
    for id in ids.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let idx = ensure_node(
            graph,
            id,
            NodeShape::Classifier,
            NodeStyle::Default,
            stack,
        );
        if let Some(node) = graph.nodes.get_mut(idx) {
            apply_style(node, annotation);
        }
    }
}

fn apply_style(node: &mut Node, annotation: &str) {
    let annotation = annotation.trim();
    node.stereotype = Some(annotation.to_owned());
    node.shape = NodeShape::Classifier;
    let lower = annotation.to_ascii_lowercase();
    if lower.contains("interface") || lower.contains("abstract") {
        node.style = NodeStyle::Abstract;
    }
}

fn add_member(node: &mut Node, text: &str) {
    let text = text.trim().trim_end_matches(';').trim();
    if text.is_empty() {
        return;
    }
    let idx = if text.contains('(') { 1 } else { 0 };
    while node.compartments.len() <= idx {
        node.compartments.push(Vec::new());
    }
    node.compartments[idx].push(text.to_owned());
}

const CLASS_OPS: &[&str] = &[
    "<|--", "--|>", "<|..", "..|>", "*--", "--*", "o--", "--o", "-->", "<--", "..>", "<..", "--",
    "..", "<|", "|>",
];

fn parse_class_relation(t: &str) -> Option<(String, String, String, Option<String>)> {
    let chars: Vec<char> = t.chars().collect();
    let mut cur = Cur::new(&chars);
    cur.skip_ws();
    let left = read_id(&mut cur);
    if left.is_empty() {
        return None;
    }
    cur.skip_ws();
    let op = CLASS_OPS.iter().find(|o| cur.starts_with(o)).copied()?;
    cur.advance(op.chars().count());
    cur.skip_ws();
    let right = read_id(&mut cur);
    if right.is_empty() {
        return None;
    }
    cur.skip_ws();
    let label = relation_label(&mut cur);
    Some((left, right, op.to_owned(), label))
}

/// Text after a `:` at the cursor, cleaned.
fn relation_label(cur: &mut Cur) -> Option<String> {
    if cur.peek() != Some(':') {
        return None;
    }
    cur.i += 1;
    let rest: String = cur.rest().iter().collect();
    let rest = rest.trim();
    if rest.is_empty() {
        None
    } else {
        Some(clean_label(rest))
    }
}

fn class_edge(left: &str, right: &str, op: &str, label: Option<String>) -> Edge {
    use Arrow::{Diamond, Filled, FilledDiamond, Hollow, None as NoHead, Open};
    let (from, to, head, tail, line) = match op {
        "<|--" => (right, left, Hollow, NoHead, LineStyle::Solid),
        "--|>" => (left, right, Hollow, NoHead, LineStyle::Solid),
        "<|.." => (right, left, Hollow, NoHead, LineStyle::Dotted),
        "..|>" => (left, right, Hollow, NoHead, LineStyle::Dotted),
        "--> " | "-->" => (left, right, Open, NoHead, LineStyle::Solid),
        "<--" => (right, left, Open, NoHead, LineStyle::Solid),
        "..>" => (left, right, Open, NoHead, LineStyle::Dotted),
        "<.." => (right, left, Open, NoHead, LineStyle::Dotted),
        "*--" => (left, right, NoHead, FilledDiamond, LineStyle::Solid),
        "--*" => (left, right, FilledDiamond, NoHead, LineStyle::Solid),
        "o--" => (left, right, NoHead, Diamond, LineStyle::Solid),
        "--o" => (left, right, Diamond, NoHead, LineStyle::Solid),
        "--" => (left, right, NoHead, NoHead, LineStyle::Solid),
        ".." => (left, right, NoHead, NoHead, LineStyle::Dotted),
        "<|" => (right, left, Hollow, NoHead, LineStyle::Solid),
        "|>" => (left, right, Hollow, NoHead, LineStyle::Solid),
        _ => (left, right, Filled, NoHead, LineStyle::Solid),
    };
    let mut edge = Edge::new(from, to);
    edge.head = head;
    edge.tail = tail;
    edge.line = line;
    if let Some(label) = label {
        edge.label = Some(label);
        edge.label_pad = 4.0;
    }
    edge
}

// ---------------------------------------------------------------------------
// stateDiagram
// ---------------------------------------------------------------------------

fn state_parse(lines: &[String], title: Option<String>) -> Graph {
    let mut graph = Graph {
        kind_hint: Some(DiagramKind::State),
        title,
        ..Graph::default()
    };
    let mut notes = Notes::default();
    let mut stack: Vec<usize> = Vec::new();
    let mut start_used = false;
    let mut end_warned = false;

    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t == "}" {
            stack.pop();
            continue;
        }
        let lower = first_word(t).to_ascii_lowercase();
        match lower.as_str() {
            "direction" => {
                if let Some(d) = Direction::parse(after_first_word(t)) {
                    graph.direction = d;
                }
                continue;
            }
            "state" => {
                state_decl(&mut graph, &mut stack, after_first_word(t));
                continue;
            }
            "note" | "click" | "classdef" | "class" => {
                notes.warn(format!("ignored state directive: {lower}"));
                continue;
            }
            _ => {}
        }
        if let Some((left, right, label)) = split_transition(t) {
            let from = terminal_or_node(
                &mut graph,
                &left,
                true,
                &stack,
                &mut start_used,
                &mut end_warned,
                &mut notes,
            );
            let to = terminal_or_node(
                &mut graph,
                &right,
                false,
                &stack,
                &mut start_used,
                &mut end_warned,
                &mut notes,
            );
            let mut edge = Edge::new(&from, &to);
            if let Some(label) = label {
                edge.label = Some(label);
                edge.label_pad = 4.0;
            }
            graph.edges.push(edge);
            continue;
        }
        notes.warn(format!("ignored state statement: {}", truncate(t, 40)));
    }

    graph.warnings = notes.into_vec();
    graph
}

/// `state S`, `state S { .. }`, `state "desc" as S`.
fn state_decl(graph: &mut Graph, stack: &mut Vec<usize>, rest: &str) {
    let rest = rest.trim();
    if rest.is_empty() {
        return;
    }
    if let Some(stripped) = rest.strip_prefix('"') {
        if let Some(close) = stripped.find('"') {
            let desc = stripped.get(..close).unwrap_or("").trim();
            let tail = stripped.get(close + 1..).unwrap_or("").trim();
            let id = tail
                .strip_prefix("as")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(desc);
            let idx = ensure_node(graph, id, NodeShape::Round, NodeStyle::Default, stack);
            if let Some(node) = graph.nodes.get_mut(idx) {
                node.label = desc.to_owned();
            }
            return;
        }
    }
    let chars: Vec<char> = rest.chars().collect();
    let mut cur = Cur::new(&chars);
    let id = read_id(&mut cur);
    if id.is_empty() {
        return;
    }
    cur.skip_ws();
    if cur.peek() == Some('{') {
        push_cluster(graph, stack, &id, &id);
    } else {
        ensure_node(graph, &id, NodeShape::Round, NodeStyle::Default, stack);
    }
}

/// Resolve a transition endpoint, uniquifying repeated `[*]` terminals.
#[allow(clippy::too_many_arguments)]
fn terminal_or_node(
    graph: &mut Graph,
    token: &str,
    is_source: bool,
    stack: &[usize],
    start_used: &mut bool,
    end_warned: &mut bool,
    notes: &mut Notes,
) -> String {
    if token != "[*]" {
        ensure_node(graph, token, NodeShape::Round, NodeStyle::Default, stack);
        return token.to_owned();
    }
    if !*start_used {
        *start_used = true;
        ensure_node(graph, "[*]", NodeShape::Circle, NodeStyle::Terminal, stack);
        return "[*]".to_owned();
    }
    if !is_source {
        if !*end_warned {
            notes.warn(
                "state diagram: `[*]` reused as an end state; renamed to `[*-end]`".to_owned(),
            );
            *end_warned = true;
        }
        ensure_node(
            graph,
            "[*-end]",
            NodeShape::Circle,
            NodeStyle::Terminal,
            stack,
        );
        return "[*-end]".to_owned();
    }
    "[*]".to_owned()
}

fn split_transition(t: &str) -> Option<(String, String, Option<String>)> {
    let (idx, len) = t
        .find("-->")
        .map(|i| (i, 3))
        .or_else(|| t.find("->").map(|i| (i, 2)))?;
    let left = t.get(..idx)?.trim();
    let after = t.get(idx + len..)?;
    let (to, label) = match after.find(':') {
        Some(i) => (
            after.get(..i)?.trim(),
            Some(after.get(i + 1..)?.trim().to_owned()),
        ),
        None => (after.trim(), None),
    };
    if left.is_empty() || to.is_empty() {
        return None;
    }
    let label = label
        .filter(|s| !s.is_empty())
        .map(|s| clean_label(&s));
    Some((left.to_owned(), to.to_owned(), label))
}

// ---------------------------------------------------------------------------
// erDiagram
// ---------------------------------------------------------------------------

fn er_parse(lines: &[String], title: Option<String>) -> Graph {
    let mut graph = Graph {
        kind_hint: Some(DiagramKind::Entity),
        title,
        ..Graph::default()
    };
    let mut notes = Notes::default();
    let mut open: Option<usize> = None;

    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t == "}" {
            open = None;
            continue;
        }
        if let Some((left, right, marker, label)) = parse_er_relation(t) {
            ensure_node(
                &mut graph,
                &left,
                NodeShape::Classifier,
                NodeStyle::Default,
                &[],
            );
            ensure_node(
                &mut graph,
                &right,
                NodeShape::Classifier,
                NodeStyle::Default,
                &[],
            );
            graph.edges.push(er_edge(&left, &right, &marker, label));
            continue;
        }
        if let Some(head) = t.strip_suffix('{') {
            let id = head.trim();
            if !id.is_empty() && !id.contains(|c: char| c.is_whitespace()) {
                let idx = ensure_node(
                    &mut graph,
                    id,
                    NodeShape::Classifier,
                    NodeStyle::Default,
                    &[],
                );
                open = Some(idx);
                continue;
            }
        }
        if let Some(idx) = open {
            let text = t.trim_end_matches('}').trim();
            if let Some(node) = graph.nodes.get_mut(idx) {
                add_member(node, text);
            }
            continue;
        }
        notes.warn(format!("ignored ER statement: {}", truncate(t, 40)));
    }

    graph.warnings = notes.into_vec();
    graph
}

fn parse_er_relation(t: &str) -> Option<(String, String, String, Option<String>)> {
    let chars: Vec<char> = t.chars().collect();
    let mut cur = Cur::new(&chars);
    cur.skip_ws();
    let left = read_id(&mut cur);
    if left.is_empty() {
        return None;
    }
    cur.skip_ws();
    let mut marker = String::new();
    while let Some(c) = cur.peek() {
        if "|o{}-.".contains(c) {
            marker.push(c);
            cur.i += 1;
        } else {
            break;
        }
    }
    if !marker.contains('-') && !marker.contains('.') {
        return None;
    }
    cur.skip_ws();
    let right = read_id(&mut cur);
    if right.is_empty() {
        return None;
    }
    cur.skip_ws();
    let label = relation_label(&mut cur);
    Some((left, right, marker, label))
}

fn er_edge(left: &str, right: &str, marker: &str, label: Option<String>) -> Edge {
    let (line, left_marker, right_marker) = if let Some(i) = marker.find("--") {
        (
            LineStyle::Solid,
            marker.get(..i).unwrap_or("").to_owned(),
            marker.get(i + 2..).unwrap_or("").to_owned(),
        )
    } else if let Some(i) = marker.find("..") {
        (
            LineStyle::Dashed,
            marker.get(..i).unwrap_or("").to_owned(),
            marker.get(i + 2..).unwrap_or("").to_owned(),
        )
    } else {
        (LineStyle::Solid, marker.to_owned(), String::new())
    };

    let mut edge = Edge::new(left, right);
    edge.head = card_arrow(&right_marker);
    edge.tail = card_arrow(&left_marker);
    edge.line = line;

    let text = match label {
        Some(text) if !text.is_empty() => format!("{marker} {text}"),
        _ => marker.to_owned(),
    };
    if !text.is_empty() {
        edge.label = Some(text);
        edge.label_pad = 4.0;
    }
    edge
}

/// Best-effort crow's-foot mapping: `o` (optional) becomes a circle, everything
/// else a filled head.
fn card_arrow(marker: &str) -> Arrow {
    if marker.is_empty() {
        Arrow::None
    } else if marker.contains('o') {
        Arrow::Circle
    } else {
        Arrow::Filled
    }
}

// ---------------------------------------------------------------------------
// Shared graph construction
// ---------------------------------------------------------------------------

/// Create a node if missing (remembering the enclosing cluster) and register it
/// as a member of every open cluster.
fn ensure_node(
    graph: &mut Graph,
    id: &str,
    shape: NodeShape,
    style: NodeStyle,
    stack: &[usize],
) -> usize {
    let idx = match graph.index_of(id) {
        Some(i) => i,
        None => {
            let mut node = Node::new(id, id);
            node.shape = shape;
            node.style = style;
            if let Some(&c) = stack.last() {
                node.cluster = Some(c);
            }
            graph.nodes.push(node);
            graph.nodes.len() - 1
        }
    };
    for &c in stack {
        let Some(cluster) = graph.clusters.get_mut(c) else {
            continue;
        };
        if !cluster.members.iter().any(|m| m == id) {
            cluster.members.push(id.to_owned());
        }
    }
    idx
}

/// Push a new cluster named `id` labelled `label` (used by namespaces and
/// composite states).
fn push_cluster(graph: &mut Graph, stack: &mut Vec<usize>, id: &str, label: &str) {
    let mut id = id.trim().trim_end_matches('{').trim().to_owned();
    if id.is_empty() {
        id = format!("cluster-{}", graph.clusters.len());
    }
    if graph.clusters.iter().any(|c| c.id == id) {
        id = format!("{id}#{}", graph.clusters.len());
    }
    let label = label.trim().trim_end_matches('{').trim();
    let parent = stack.last().copied();
    let idx = graph.clusters.len();
    graph.clusters.push(Cluster {
        id,
        label: label.to_owned(),
        members: Vec::new(),
        parent,
            ..Default::default()
    });
    stack.push(idx);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Indentation-based diagrams: mindmap and kanban
// ---------------------------------------------------------------------------

/// Source lines with their indentation preserved.
///
/// [`strip_noise`] trims every line, which is fine for the statement-oriented
/// parsers but destroys the nesting that mindmaps and kanban boards express.
fn indented_lines(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_front_matter = false;
    let mut seen_content = false;
    for raw in src.lines() {
        let code = strip_comment(raw);
        let trimmed = code.trim();
        if trimmed.is_empty() || trimmed.starts_with("%%") {
            continue;
        }
        if !seen_content && trimmed == "---" {
            in_front_matter = !in_front_matter;
            continue;
        }
        if in_front_matter {
            continue;
        }
        if trimmed.starts_with("@") {
            continue;
        }
        seen_content = true;
        let indent = code.chars().take_while(|c| c.is_whitespace()).count();
        out.push((indent, trimmed.to_owned()));
    }
    out
}

/// `((circle))`, `(round)`, `[rect]`, `{{hex}}`, `)cloud(` or a bare label.
fn mindmap_node(text: &str) -> (Option<NodeShape>, String) {
    let t = text.trim();
    for (open, close, shape) in [
        ("((", "))", NodeShape::Circle),
        ("))", "((", NodeShape::Circle),
        ("{{", "}}", NodeShape::Hexagon),
        ("(", ")", NodeShape::Round),
        ("[", "]", NodeShape::Rect),
        (")", "(", NodeShape::Asymmetric),
    ] {
        // The id may sit in front of the delimiters: `root((中心))`.
        let Some(pos) = t.find(open) else { continue };
        let Some(inner) = t[pos..]
            .strip_prefix(open)
            .and_then(|r| r.strip_suffix(close))
        else {
            continue;
        };
        return (Some(shape), inner.trim().to_owned());
    }
    (None, t.to_owned())
}

/// `mindmap` — an indentation tree that is laid out as a left-to-right graph.
fn mindmap_parse(src: &str, title: Option<String>) -> Graph {
    let mut graph = Graph {
        kind_hint: Some(DiagramKind::Mindmap),
        direction: Direction::LeftRight,
        title,
        ..Graph::default()
    };
    // (indent, node id) of the current ancestor chain.
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut auto = 0usize;

    for (indent, text) in indented_lines(src) {
        if text.eq_ignore_ascii_case("mindmap") {
            continue;
        }
        while stack.last().is_some_and(|(outer, _)| *outer >= indent) {
            stack.pop();
        }
        let parent = stack.last().map(|(_, id)| id.clone());

        let (shape, label) = mindmap_node(&text);
        auto += 1;
        let id = format!("mm{auto}");
        let mut node = Node::new(&id, if label.is_empty() { text.clone() } else { label });
        if let Some(shape) = shape {
            node.shape = shape;
        }
        graph.nodes.push(node);

        if let Some(parent) = parent {
            let mut edge = Edge::new(parent, id.clone());
            // Mindmap branches are plain lines, not arrows.
            edge.head = Arrow::None;
            graph.edges.push(edge);
        }
        stack.push((indent, id));
    }

    if graph.nodes.is_empty() {
        graph.warnings.push("mindmap 没有节点".to_owned());
    }
    graph
}

// ---------------------------------------------------------------------------
// requirementDiagram
// ---------------------------------------------------------------------------

/// `requirementDiagram` — boxes with their fields, joined by dashed relations.
fn requirement_parse(lines: &[String], title: Option<String>) -> Graph {
    let mut graph = Graph {
        kind_hint: Some(DiagramKind::Requirement),
        direction: Direction::LeftRight,
        title,
        ..Graph::default()
    };
    let mut i = 0usize;
    while i < lines.len() {
        let text = lines[i].trim();
        if text.is_empty()
            || text.eq_ignore_ascii_case("requirementdiagram")
            || text.eq_ignore_ascii_case("requirementDiagram")
        {
            i += 1;
            continue;
        }

        if let Some(head) = text.strip_suffix('{') {
            // `requirement name {` … `}`
            let head = head.trim();
            let (kind, name) = split_word(head);
            let kind = if kind.is_empty() { "requirement" } else { kind };
            let name = name.trim();
            let id = if name.is_empty() { kind } else { name };
            let mut fields: Vec<String> = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim() != "}" {
                let line = lines[i].trim();
                if !line.is_empty() {
                    fields.push(line.to_owned());
                }
                i += 1;
            }
            i += 1; // the closing brace

            let mut node = Node::new(id, id);
            node.shape = NodeShape::Classifier;
            node.stereotype = Some(kind.to_owned());
            if !fields.is_empty() {
                node.compartments.push(fields);
            }
            // Re-declaring the same id keeps the first box but upgrades it.
            if let Some(idx) = graph.index_of(id) {
                graph.nodes[idx] = node;
            } else {
                graph.nodes.push(node);
            }
            continue;
        }

        if let Some((from, to, label)) = parse_requirement_relation(text) {
            let mut edge = Edge::new(from, to);
            edge.line = LineStyle::Dashed;
            edge.label = label;
            if edge.label.is_some() {
                edge.label_pad = 4.0;
            }
            graph.edges.push(edge);
        } else {
            graph
                .warnings
                .push(format!("无法识别的 requirement 语句: {text}"));
        }
        i += 1;
    }
    graph
}

/// `A - satisfies -> B` and `B <- satisfies - A` both point at the relation head.
///
/// The forward form reads `tail [- label] -> head`, the reversed one reads
/// `head <- label - tail`, so the two sides are split differently.
fn parse_requirement_relation(text: &str) -> Option<(String, String, Option<String>)> {
    if let Some(idx) = text.find("->") {
        let (from, label) = split_name_label(text[..idx].trim());
        let (to, _) = split_name_label(text[idx + 2..].trim());
        if from.is_empty() || to.is_empty() {
            return None;
        }
        return Some((from, to, label));
    }
    let idx = text.find("<-")?;
    let (to, _) = split_name_label(text[..idx].trim());
    let (label, from) = split_label_name(text[idx + 2..].trim());
    if from.is_empty() || to.is_empty() {
        return None;
    }
    Some((from, to, label))
}

/// `name - label` → `(name, Some(label))`; `name -` → `(name, None)`.
fn split_name_label(s: &str) -> (String, Option<String>) {
    let s = s.trim().trim_end_matches('-').trim();
    match s.split_once(" - ") {
        Some((name, label)) => {
            let label = label.trim().trim_end_matches('-').trim();
            (
                name.trim().to_owned(),
                if label.is_empty() {
                    None
                } else {
                    Some(label.to_owned())
                },
            )
        }
        None => (s.to_owned(), None),
    }
}

/// `label - name` → `(Some(label), name)`.
fn split_label_name(s: &str) -> (Option<String>, String) {
    let s = s.trim().trim_start_matches('-').trim();
    match s.rsplit_once(" - ") {
        Some((label, name)) => {
            let label = label.trim().trim_start_matches('-').trim();
            (
                if label.is_empty() {
                    None
                } else {
                    Some(label.to_owned())
                },
                name.trim().to_owned(),
            )
        }
        None => (None, s.to_owned()),
    }
}

// ---------------------------------------------------------------------------
// kanban
// ---------------------------------------------------------------------------

/// `id[Label]`, `id(Label)` or a bare label.
fn kanban_entry(text: &str) -> (String, String) {
    let t = text.trim();
    let t = match t.find("@{") {
        Some(idx) => t[..idx].trim(),
        None => t,
    };
    for (open, close) in [('[', ']'), ('(', ')'), ('{', '}')] {
        if let Some(pos) = t.find(open) {
            if let Some(end) = t.rfind(close) {
                if end > pos {
                    let id = t[..pos].trim().to_owned();
                    let label = t[pos + 1..end].trim().to_owned();
                    if !label.is_empty() {
                        return (id, strip_quotes(&label));
                    }
                }
            }
        }
    }
    (String::new(), strip_quotes(t))
}

/// `kanban` — columns become clusters (laid out left to right), cards are nodes.
fn kanban_parse(src: &str, title: Option<String>) -> Graph {
    let mut graph = Graph {
        kind_hint: Some(DiagramKind::Kanban),
        direction: Direction::LeftRight,
        title,
        ..Graph::default()
    };
    let mut column: Option<usize> = None;
    let mut column_indent: Option<usize> = None;
    let mut auto = 0usize;

    for (indent, text) in indented_lines(src) {
        if text.eq_ignore_ascii_case("kanban") {
            continue;
        }
        let (id, label) = kanban_entry(&text);
        let is_column = match column_indent {
            None => true,
            Some(base) => indent <= base,
        };
        if is_column {
            let cid = if id.is_empty() {
                format!("column-{}", graph.clusters.len() + 1)
            } else {
                id.clone()
            };
            let idx = graph.clusters.len();
            graph.clusters.push(Cluster::new(cid, label, None));
            column = Some(idx);
            column_indent = Some(indent);
            continue;
        }
        let Some(ci) = column else { continue };
        auto += 1;
        let node_id = if id.is_empty() {
            format!("card-{auto}")
        } else {
            id
        };
        let mut node = Node::new(node_id.clone(), label);
        node.cluster = Some(ci);
        if let Some(c) = graph.clusters.get_mut(ci) {
            c.members.push(node_id);
        }
        graph.nodes.push(node);
    }

    if graph.clusters.is_empty() {
        graph.warnings.push("kanban 没有列".to_owned());
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagram::model::{
        Arrow, Diagram, DiagramKind, Direction, GroupKind, LineStyle, NodeShape, NodeStyle,
        NotePosition, ParticipantKind, SeqStep,
    };

    fn graph(src: &str) -> Graph {
        match parse(src) {
            Diagram::Graph(g) => g,
            other => panic!("expected a graph, got {other:?}"),
        }
    }
    fn sequence(src: &str) -> Sequence {
        match parse(src) {
            Diagram::Sequence(s) => s,
            other => panic!("expected a sequence, got {other:?}"),
        }
    }

    fn messages(seq: &Sequence) -> Vec<(Arrow, LineStyle, bool, bool, String)> {
        seq.steps
            .iter()
            .filter_map(|step| match step {
                SeqStep::Message {
                    head,
                    line,
                    activate,
                    deactivate,
                    label,
                    ..
                } => Some((*head, *line, *activate, *deactivate, label.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn flowchart_node_shapes() {
        let src = "flowchart TD\nA[Rect]\nB(Round)\nC([Stadium])\nD((Circle))\nE{Diamond}\nF{{Hexagon}}\nG[/Para/]\nH[(Cyl)]\nI>Asym]\nJ";
        let g = graph(src);
        assert_eq!(g.kind_hint, Some(DiagramKind::Flowchart));
        assert_eq!(g.direction, Direction::TopDown);
        let shape = |id: &str| g.node(id).map(|n| n.shape);
        assert_eq!(shape("A"), Some(NodeShape::Rect));
        assert_eq!(shape("B"), Some(NodeShape::Round));
        assert_eq!(shape("C"), Some(NodeShape::Stadium));
        assert_eq!(shape("D"), Some(NodeShape::Circle));
        assert_eq!(shape("E"), Some(NodeShape::Diamond));
        assert_eq!(shape("F"), Some(NodeShape::Hexagon));
        assert_eq!(shape("G"), Some(NodeShape::Parallelogram));
        assert_eq!(shape("H"), Some(NodeShape::Cylinder));
        assert_eq!(shape("I"), Some(NodeShape::Asymmetric));
        assert_eq!(shape("J"), Some(NodeShape::Rect));
        assert_eq!(g.node("A").map(|n| n.label.as_str()), Some("Rect"));
    }

    #[test]
    fn edge_label_both_syntaxes() {
        let g = graph("flowchart LR\nA -- text --> B\nC -->|quoted| D\nE -. dotted .-> F");
        assert_eq!(g.direction, Direction::LeftRight);
        let find = |f: &str, t: &str| g.edges.iter().find(|e| e.from == f && e.to == t);
        let e1 = find("A", "B").expect("A->B");
        assert_eq!(e1.label.as_deref(), Some("text"));
        assert_eq!(e1.head, Arrow::Filled);
        assert_eq!(e1.line, LineStyle::Solid);
        let e2 = find("C", "D").expect("C->D");
        assert_eq!(e2.label.as_deref(), Some("quoted"));
        let e3 = find("E", "F").expect("E->F");
        assert_eq!(e3.label.as_deref(), Some("dotted"));
        assert_eq!(e3.line, LineStyle::Dashed);
    }

    #[test]
    fn chained_edges_produce_every_link() {
        let g = graph("flowchart TD\nA --> B --> C");
        assert_eq!(g.edges.len(), 2);
        assert!(g.edges.iter().any(|e| e.from == "A" && e.to == "B"));
        assert!(g.edges.iter().any(|e| e.from == "B" && e.to == "C"));
    }

    #[test]
    fn ampersand_is_a_cross_product() {
        let g = graph("flowchart TD\nA & B --> C & D");
        assert_eq!(g.nodes.len(), 4);
        assert_eq!(g.edges.len(), 4);
        for e in &g.edges {
            assert!(matches!(e.from.as_str(), "A" | "B"));
            assert!(matches!(e.to.as_str(), "C" | "D"));
        }
    }

    #[test]
    fn subgraph_nesting_tracks_parents() {
        let src =
            "flowchart TD\nsubgraph one [Outer]\nA --> B\nsubgraph two [Inner]\nC --> D\nend\nend";
        let g = graph(src);
        assert_eq!(g.clusters.len(), 2);
        assert_eq!(g.clusters[0].label, "Outer");
        assert_eq!(g.clusters[0].parent, None);
        assert_eq!(g.clusters[1].label, "Inner");
        assert_eq!(g.clusters[1].parent, Some(0));
        assert_eq!(g.node("A").and_then(|n| n.cluster), Some(0));
        assert_eq!(g.node("C").and_then(|n| n.cluster), Some(1));
        assert!(g.clusters[1].members.iter().any(|m| m == "C"));
        assert!(g.clusters[1].members.iter().any(|m| m == "D"));
    }

    #[test]
    fn sequence_arrows_and_activation() {
        let seq = sequence(
            "sequenceDiagram\nparticipant A\nactor B as Bee\nA->>B: hello\nB-->>A: hi\nA-xB: cross\nA-)B: async\nA->>+B: on\nB-->>-A: off",
        );
        assert_eq!(seq.participants.len(), 2);
        assert_eq!(seq.participants[0].kind, ParticipantKind::Default);
        assert_eq!(seq.participants[1].kind, ParticipantKind::Actor);
        assert_eq!(seq.participants[1].label, "Bee");

        let msgs = messages(&seq);
        assert_eq!(msgs.len(), 6);
        assert_eq!(msgs[0].0, Arrow::Filled);
        assert_eq!(msgs[0].1, LineStyle::Solid);
        assert_eq!(msgs[1].0, Arrow::Open);
        assert_eq!(msgs[1].1, LineStyle::Dashed);
        assert_eq!(msgs[2].0, Arrow::Cross);
        assert_eq!(msgs[3].0, Arrow::Open);
        assert!(msgs[4].2, "expected activate on `A->>+B`");
        assert!(msgs[5].3, "expected deactivate on `B-->>-A`");
    }

    #[test]
    fn sequence_notes_and_alt_depth() {
        let seq = sequence(
            "sequenceDiagram\nA->>B: m\nNote left of A: l\nNote right of B: r\nNote over A: o\nNote over A,B: ab\nalt yes\nA->>B: y\nelse no\nA->>B: n\nend",
        );
        let notes: Vec<(NotePosition, usize, String)> = seq
            .steps
            .iter()
            .filter_map(|s| match s {
                SeqStep::Note {
                    over,
                    position,
                    text,
                } => Some((*position, over.len(), text.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(notes.len(), 4);
        assert_eq!(notes[0], (NotePosition::Left, 1, "l".to_owned()));
        assert_eq!(notes[1].0, NotePosition::Right);
        assert_eq!(notes[2].0, NotePosition::Over);
        assert_eq!(notes[3], (NotePosition::Over, 2, "ab".to_owned()));

        let opens: Vec<(GroupKind, usize, String)> = seq
            .steps
            .iter()
            .filter_map(|s| match s {
                SeqStep::GroupOpen { kind, label, depth } => Some((*kind, *depth, label.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(opens, vec![(GroupKind::Alt, 0, "yes".to_owned())]);

        let elses: Vec<usize> = seq
            .steps
            .iter()
            .filter_map(|s| match s {
                SeqStep::GroupElse { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(elses, vec![0]);

        let closes: Vec<usize> = seq
            .steps
            .iter()
            .filter_map(|s| match s {
                SeqStep::GroupClose { depth } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(closes, vec![0]);
    }

    #[test]
    fn sequence_autonumber_prefixes_labels() {
        let seq = sequence("sequenceDiagram\nautonumber\nA->>B: hi\nB-->>A: yo");
        let msgs = messages(&seq);
        assert_eq!(msgs[0].4, "1. hi");
        assert_eq!(msgs[1].4, "2. yo");
    }

    #[test]
    fn sequence_dividers_and_delays() {
        let seq = sequence("sequenceDiagram\n== section ==\n... waiting ...\nA->>B: x");
        assert!(matches!(seq.steps.first(), Some(SeqStep::Divider { .. })));
        assert!(matches!(seq.steps.get(1), Some(SeqStep::Delay { .. })));
    }

    #[test]
    fn pie_with_quoted_labels() {
        let d = parse("pie title Pets\n\"Dog\" : 10\n\"Cat\" : 5.5\nBird : 2");
        let p = match d {
            Diagram::Pie(p) => p,
            other => panic!("expected a pie, got {other:?}"),
        };
        assert_eq!(p.title.as_deref(), Some("Pets"));
        assert_eq!(p.slices.len(), 3);
        assert_eq!(p.slices[0].label, "Dog");
        assert!((p.slices[1].value - 5.5).abs() < 1e-6);
        assert!((p.total() - 17.5).abs() < 1e-6);
    }

    #[test]
    fn class_body_splits_fields_and_methods() {
        let src = "classDiagram\nclass Animal {\n+String name\n+int age\n+speak() void\n}\nAnimal <|-- Dog";
        let g = graph(src);
        assert_eq!(g.kind_hint, Some(DiagramKind::Class));
        let animal = g.node("Animal").expect("Animal");
        assert_eq!(animal.shape, NodeShape::Classifier);
        assert!(
            animal
                .compartments
                .first()
                .is_some_and(|c| c.iter().any(|m| m.contains("+String name"))),
            "field expected in compartment 0"
        );
        assert!(
            animal
                .compartments
                .get(1)
                .is_some_and(|c| c.iter().any(|m| m.contains("+speak()"))),
            "method expected in compartment 1"
        );
        let inherit = g
            .edges
            .iter()
            .find(|e| e.from == "Dog" && e.to == "Animal")
            .expect("inheritance edge");
        assert_eq!(inherit.head, Arrow::Hollow);
    }

    #[test]
    fn class_annotation_and_namespace() {
        let src = "classDiagram\nnamespace ns {\nclass Shape\n}\n<<interface>> Shape";
        let g = graph(src);
        assert_eq!(g.clusters.len(), 1);
        assert_eq!(g.clusters[0].label, "ns");
        let shape = g.node("Shape").expect("Shape");
        assert_eq!(shape.style, NodeStyle::Abstract);
        assert_eq!(shape.stereotype.as_deref(), Some("interface"));
        assert_eq!(shape.cluster, Some(0));
    }

    #[test]
    fn state_diagram_two_terminal_uses() {
        let g = graph("stateDiagram-v2\n[*] --> Still\nStill --> Moving\nMoving --> [*]");
        assert_eq!(g.kind_hint, Some(DiagramKind::State));
        let start = g.node("[*]").expect("start terminal");
        assert_eq!(start.style, NodeStyle::Terminal);
        assert_eq!(start.shape, NodeShape::Circle);
        let end = g.node("[*-end]").expect("end terminal");
        assert_eq!(end.style, NodeStyle::Terminal);
        assert!(g.edges.iter().any(|e| e.from == "Moving" && e.to == "[*-end]"));
        assert!(g.warnings.iter().any(|w| w.contains("[*-end]")));
    }

    #[test]
    fn state_composite_and_description() {
        let g = graph("stateDiagram\nstate \"Long Name\" as S\nstate S {\nA --> B\n}");
        assert_eq!(g.node("S").map(|n| n.label.as_str()), Some("Long Name"));
        assert_eq!(g.clusters.len(), 1);
        assert_eq!(g.clusters[0].id, "S");
        assert_eq!(g.node("A").and_then(|n| n.cluster), Some(0));
    }

    #[test]
    fn er_entity_body_and_relation() {
        let src = "erDiagram\nCUSTOMER {\nstring name\nint id\n}\nCUSTOMER ||--o{ ORDER : places";
        let g = graph(src);
        assert_eq!(g.kind_hint, Some(DiagramKind::Entity));
        let customer = g.node("CUSTOMER").expect("CUSTOMER");
        assert_eq!(customer.shape, NodeShape::Classifier);
        assert_eq!(customer.compartments.first().map(|c| c.len()), Some(2));
        let rel = g
            .edges
            .iter()
            .find(|e| e.from == "CUSTOMER" && e.to == "ORDER")
            .expect("relation");
        assert_eq!(rel.tail, Arrow::Filled);
        assert_eq!(rel.head, Arrow::Circle);
        assert!(rel.label.as_deref().unwrap_or("").contains("||--o{"));
    }

    #[test]
    fn node_palette_shape_syntax() {
        let g = graph("flowchart TD\nA@{ shape: hexagon, label: \"N\" }\nB@{ shape: bogus }");
        assert_eq!(g.node("A").map(|n| n.shape), Some(NodeShape::Hexagon));
        assert_eq!(g.node("A").map(|n| n.label.as_str()), Some("N"));
        assert!(g.warnings.iter().any(|w| w.contains("bogus")));
    }

    #[test]
    fn html_entities_and_breaks_in_labels() {
        let g = graph("flowchart TD\nA[\"a &amp; b\"]\nB[line1<br/>line2 &#39;q&#39;]");
        assert_eq!(g.node("A").map(|n| n.label.as_str()), Some("a & b"));
        let b_lines = g.node("B").map(|n| n.label_lines()).unwrap_or_default();
        assert_eq!(b_lines, vec!["line1".to_owned(), "line2 'q'".to_owned()]);
    }

    #[test]
    fn preliminaries_front_matter_comments_and_title() {
        let src = "---\nfoo: bar\n---\n%% a comment\nflowchart LR %%{init: {}}%%\ntitle My Chart\nA --> B %% trailing";
        let g = graph(src);
        assert_eq!(g.title.as_deref(), Some("My Chart"));
        assert_eq!(g.direction, Direction::LeftRight);
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn unsupported_header_warns_and_still_parses_edges() {
        // `C4Context` is deliberately out of scope: C4 describes a deployment
        // topology that this renderer has no layout for.
        let g = graph("C4Context\n  Person(p)\nA --> B");
        assert!(g.kind_hint.is_none());
        assert!(
            g.warnings
                .iter()
                .any(|w| w.contains("unsupported diagram type: C4Context")),
            "expected an unsupported-type warning, got {:?}",
            g.warnings
        );
        // The fallback still harvested the edge in the body.
        assert!(g.edges.iter().any(|e| e.from == "A" && e.to == "B"));
    }

    #[test]
    fn edges_without_a_header_are_treated_as_a_flowchart() {
        let g = graph("A --> B\nB --- C");
        assert!(g.warnings.is_empty());
        assert_eq!(g.edges.len(), 2);
    }

    #[test]
    fn fuzz_malformed_inputs_do_not_panic() {
        let cases: &[&str] = &[
            "",
            "%%
%%\n",
            "%% only a comment",
            "graph TD\nA[",
            "graph TD\nA[]",
            "graph TD\nA[---->",
            "flowchart\n& --> ",
            "flowchart TD\n-->",
            "flowchart TD\nA & & B",
            "flowchart LR\nA[(]",
            "flowchart LR\nA{{{x}}}",
            "flowchart TD\nsubgraph\nend\nend\nend",
            "sequenceDiagram\nA->",
            "sequenceDiagram\nA->>",
            "sequenceDiagram\n->>B: x",
            "sequenceDiagram\nNote over :\nend",
            "sequenceDiagram\nalt\nelse\nend\nend\nend",
            "sequenceDiagram\nA->>B: \u{1F600}\nB->>A: \u{4E2D}\u{6587}",
            "pie\n\"x\" :",
            "pie\n\"x\" : abc",
            "pie\n: 1",
            "classDiagram\nclass",
            "classDiagram\nclass X {\n",
            "classDiagram\n<|--",
            "classDiagram\nA <|--",
            "stateDiagram-v2\n[*] --> ",
            "stateDiagram-v2\n--> [*]",
            "stateDiagram-v2\nstate \"x\" as",
            "erDiagram\nA ||--o{",
            "erDiagram\n{\n}\n",
            "A --",
            "A -->",
            "-->\n",
            "mindmap\n root\n  a\n   b\n    c\n     d\n      e",
            "---\n---\n",
            "---",
            "flowchart TD\nA[\u{4E2D}\u{6587}] --> B",
        ];
        for case in cases {
            let diagram = parse(case);
            // Touching the whole public surface keeps the test honest.
            let _ = diagram.summary();
            let _ = diagram.warnings().len();
            let _ = diagram.is_empty();
            let _ = diagram.title();
            let _ = diagram.kind();
        }
    }
    // -- styling directives -------------------------------------------------

    #[test]
    fn classdef_class_style_and_linkstyle_paint_the_graph() {
        let g = graph(
            "flowchart TD\n\
             \x20 A[开始]:::hot --> B[中间]\n\
             \x20 B --> C[结束]\n\
             \x20 classDef hot fill:#f9f,stroke:#333,stroke-width:3px,color:#fff\n\
             \x20 class B hot\n\
             \x20 style C fill:#0f0\n\
             \x20 linkStyle 0 stroke:#f00,stroke-width:2px",
        );
        let a = g.node("A").expect("A");
        assert_eq!(a.fill, parse_color("#f9f"));
        assert_eq!(a.stroke, parse_color("#333"));
        assert_eq!(a.text_color, parse_color("#fff"));
        assert_eq!(a.stroke_width, Some(3.0));

        let b = g.node("B").expect("B");
        assert_eq!(b.fill, parse_color("#f9f"), "class B hot 应生效");

        let c = g.node("C").expect("C");
        assert_eq!(c.fill, parse_color("#0f0"), "style 应覆盖表主题");

        assert_eq!(g.edges[0].color, parse_color("#f00"));
        assert_eq!(g.edges[0].width, Some(2.0));
        assert!(g.edges[1].color.is_none(), "未指定的连线保持主题色");
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);
    }

    #[test]
    fn linkstyle_default_paints_every_edge() {
        let g = graph("flowchart LR\n a --> b\n b --> c\n linkStyle default stroke:#123456");
        assert!(g.edges.iter().all(|e| e.color == parse_color("#123456")));
    }

    #[test]
    fn classdef_default_fills_unstyled_nodes() {
        let g = graph("flowchart TD\n a --> b\n classDef default fill:#abc");
        assert!(g.nodes.iter().all(|n| n.fill == parse_color("#abc")));
    }

    #[test]
    fn subgraph_style_colours_the_cluster() {
        let g = graph(
            "flowchart TD\n\
             \x20 subgraph S1[组一]\n\
             \x20 a --> b\n\
             \x20 end\n\
             \x20 style S1 fill:#ff0,stroke:#333",
        );
        let cluster = g.clusters.first().expect("cluster");
        assert_eq!(cluster.fill, parse_color("#ff0"));
        assert_eq!(cluster.stroke, parse_color("#333"));
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);
    }

    #[test]
    fn unknown_style_target_warns_instead_of_failing() {
        let g = graph("flowchart TD\n a --> b\n style ghost fill:#f00\n class a nope");
        assert!(g.warnings.iter().any(|w| w.contains("ghost")));
        assert!(g.warnings.iter().any(|w| w.contains("nope")));
    }

    #[test]
    fn stroke_dasharray_makes_the_edge_dashed() {
        let g = graph("flowchart LR\n a --> b\n linkStyle 0 stroke-dasharray: 5 5");
        assert_eq!(g.edges[0].line, LineStyle::Dashed);
    }
    // -- sequence / flowchart gaps -----------------------------------------

    #[test]
    fn invisible_links_are_kept_for_layout_only() {
        let g = graph("flowchart TD\n a ~~~ b\n a --> c\n c --> b");
        let hidden = g.edges.iter().find(|e| e.hidden).expect("invisible edge");
        assert_eq!(hidden.from, "a");
        assert_eq!(hidden.to, "b");
        assert_eq!(hidden.head, Arrow::None);
        assert_eq!(g.edges.iter().filter(|e| !e.hidden).count(), 2);
    }

    #[test]
    fn create_destroy_and_option_are_understood() {
        let s = sequence(
            "sequenceDiagram\n\
             \x20 create participant D\n\
             \x20 A->>D: hi\n\
             \x20 destroy D\n\
             \x20 critical 关键步骤\n\
             \x20   A->>B: x\n\
             \x20 option 备选\n\
             \x20   A->>B: y\n\
             \x20 end",
        );
        assert!(s.participants.iter().any(|p| p.id == "D"), "create 应声明参与者");
        assert!(
            s.steps.iter().any(|st| matches!(st, SeqStep::GroupElse { .. })),
            "option 应变成分块的 else"
        );
        assert!(
            s.steps
                .iter()
                .any(|st| matches!(st, SeqStep::Activate { on: false, .. })),
            "destroy 应结束生命线"
        );
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    }

    #[test]
    fn autonumber_off_stops_numbering() {
        let s = sequence(
            "sequenceDiagram\n\
             \x20 autonumber\n\
             \x20 A->>B: one\n\
             \x20 autonumber off\n\
             \x20 A->>B: two",
        );
        let labels: Vec<String> = s
            .steps
            .iter()
            .filter_map(|st| match st {
                SeqStep::Message { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert!(labels[0].starts_with('1'), "{labels:?}");
        assert_eq!(labels[1], "two", "autonumber off 之后不再编号");
    }
    // -- mindmap / requirement / kanban ------------------------------------

    #[test]
    fn mindmap_nests_by_indentation() {
        let g = graph(
            "mindmap\n\
             \x20 root((中心))\n\
             \x20   A[分支一]\n\
             \x20     A1(叶子)\n\
             \x20   B[分支二]",
        );
        assert_eq!(g.kind_hint, Some(DiagramKind::Mindmap));
        assert_eq!(g.direction, Direction::LeftRight);
        assert_eq!(g.nodes.len(), 4);
        assert_eq!(g.edges.len(), 3, "每个子节点一条边");
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);

        // Root is the circle, the leaf hangs off its parent, not off the root.
        let root = g.nodes.iter().find(|n| n.label == "中心").expect("root");
        assert_eq!(root.shape, NodeShape::Circle);
        let leaf = g.nodes.iter().find(|n| n.label == "叶子").expect("leaf");
        let parent = g
            .nodes
            .iter()
            .find(|n| n.label == "分支一")
            .expect("branch");
        assert!(
            g.edges
                .iter()
                .any(|e| e.from == parent.id && e.to == leaf.id),
            "叶子应挂在分支一下"
        );
    }

    #[test]
    fn requirement_diagram_builds_boxes_and_relations() {
        let g = graph(
            "requirementDiagram\n\
             \x20 requirement test_req {\n\
             \x20   id: 1\n\
             \x20   text: 需求文本\n\
             \x20   risk: high\n\
             \x20 }\n\
             \x20 element test_entity {\n\
             \x20   type: simulation\n\
             \x20 }\n\
             \x20 test_entity - satisfies -> test_req",
        );
        assert_eq!(g.kind_hint, Some(DiagramKind::Requirement));
        assert_eq!(g.nodes.len(), 2);
        let req = g.nodes.iter().find(|n| n.id == "test_req").expect("req");
        assert_eq!(req.shape, NodeShape::Classifier);
        assert_eq!(req.stereotype.as_deref(), Some("requirement"));
        assert_eq!(req.compartments.len(), 1);
        assert_eq!(req.compartments[0].len(), 3, "三个字段各一行");

        assert_eq!(g.edges.len(), 1);
        let edge = &g.edges[0];
        assert_eq!(edge.from, "test_entity");
        assert_eq!(edge.to, "test_req");
        assert_eq!(edge.label.as_deref(), Some("satisfies"));
        assert_eq!(edge.line, LineStyle::Dashed);
    }

    #[test]
    fn requirement_relation_can_point_backwards() {
        let g = graph(
            "requirementDiagram\n\
             \x20 requirement a {\n id: 1\n }\n\
             \x20 element b {\n }\n\
             \x20 a <- derives - b",
        );
        let edge = g.edges.first().expect("edge");
        assert_eq!(edge.from, "b");
        assert_eq!(edge.to, "a");
        assert_eq!(edge.label.as_deref(), Some("derives"));
    }

    #[test]
    fn kanban_columns_become_clusters() {
        let g = graph(
            "kanban\n\
             \x20 todo[待办]\n\
             \x20   t1[写解析器]\n\
             \x20   t2[补测试]\n\
             \x20 doing[进行中]\n\
             \x20   d1[修布局]",
        );
        assert_eq!(g.kind_hint, Some(DiagramKind::Kanban));
        assert_eq!(g.direction, Direction::LeftRight);
        assert_eq!(g.clusters.len(), 2);
        assert_eq!(g.clusters[0].label, "待办");
        assert_eq!(g.clusters[0].members.len(), 2);
        assert_eq!(g.clusters[1].members.len(), 1);
        assert_eq!(g.nodes.len(), 3);
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);
    }
}