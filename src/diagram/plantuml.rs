//! PlantUML front-end.
//!
//! Pure `&str -> IR` transformer: PlantUML source goes in, a
//! [`crate::diagram::model::Diagram`] comes out. Layout and painting happen
//! elsewhere, so nothing in this file knows about egui, `plantuml.jar`, the
//! network or the filesystem — diagrams are parsed and drawn entirely offline.
//!
//! The parser is deliberately forgiving. PlantUML is used as documentation
//! markup, so half-finished edits land here constantly; unrecognised
//! constructs are collected into [`Graph::warnings`] / [`Sequence::warnings`]
//! and parsing continues. [`parse`] never panics and never slices a `&str` at
//! a byte offset that could split a UTF-8 scalar (all scanning goes through
//! `char_indices` / `find` / `strip_prefix`).
//!
//! Pipeline:
//!
//! ```text
//! strip_comments  ->  collect_lines  ->  extract_preamble  ->  detect_type
//!     ->  parse_{sequence,class,state,flow,er}  |  parse_fallback
//! ```

use crate::diagram::model::{
    ActBranch, ActNode, Activity, Arrow, Cluster, Diagram, DiagramKind, Direction, Edge, Graph,
    GroupKind, LineStyle, LoopKind, Node, NodeShape, NodeStyle, NotePosition, ParticipantKind,
    SeqStep, Sequence, Terminal, parse_color,
};
use crate::gfx::Rgba;

/// Parse PlantUML source into the renderer-agnostic IR. Never panics.
///
/// Unknown or malformed input degrades to a [`Graph`] carrying an
/// `unsupported PlantUML diagram type` warning rather than an error, because a
/// partially-typed diagram is still useful to look at.
pub fn parse(src: &str) -> Diagram {
    let mut lines = collect_lines(src);
    let mut pre = extract_preamble(&mut lines);
    pre.skin.resolve();

    let mut diagram = match detect_type(&lines) {
        Some(DiagramKind::Sequence) => Diagram::Sequence(parse_sequence(&lines, &pre)),
        Some(DiagramKind::Class) => Diagram::Graph(parse_class(&lines, &pre)),
        Some(DiagramKind::State) => Diagram::Graph(parse_state(&lines, &pre)),
        Some(DiagramKind::Entity) => Diagram::Graph(parse_er(&lines, &pre)),
        Some(DiagramKind::Flowchart) => Diagram::Graph(parse_flow(&lines, &pre)),
        Some(DiagramKind::Activity) => Diagram::Activity(parse_activity(&lines, &pre)),
        // `Pie` and the Mermaid-only families never come out of detection, but
        // the match has to stay total.
        Some(DiagramKind::Pie)
        | Some(DiagramKind::Mindmap)
        | Some(DiagramKind::Requirement)
        | Some(DiagramKind::Kanban)
        | Some(DiagramKind::Gantt)
        | Some(DiagramKind::Timeline)
        | Some(DiagramKind::Journey)
        | Some(DiagramKind::Quadrant)
        | Some(DiagramKind::XYChart)
        | Some(DiagramKind::Packet)
        | Some(DiagramKind::Sankey)
        | Some(DiagramKind::GitGraph)
        | None => Diagram::Graph(parse_fallback(&lines, &pre)),
    };
    match &mut diagram {
        Diagram::Graph(g) => apply_skin(g, &pre.skin),
        Diagram::Sequence(s) => apply_skin_sequence(s, &pre.skin),
        Diagram::Pie(_)
        | Diagram::Gantt(_)
        | Diagram::Timeline(_)
        | Diagram::Journey(_)
        | Diagram::Quadrant(_)
        | Diagram::XYChart(_)
        | Diagram::Packet(_)
        | Diagram::Sankey(_)
        | Diagram::GitGraph(_)
        | Diagram::Activity(_) => {}
    }
    diagram
}

// ---------------------------------------------------------------------------
// Preprocessing
// ---------------------------------------------------------------------------

/// Remove `/' ... '/` block comments, preserving the line structure so that
/// indentation-based note bodies still line up afterwards.
fn strip_block_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    loop {
        match rest.find("/'") {
            Some(open) => {
                out.push_str(&rest[..open]);
                let after = &rest[open + 2..];
                match after.find("'/") {
                    Some(close) => {
                        for ch in after[..close].chars() {
                            if ch == '\n' {
                                out.push('\n');
                            }
                        }
                        rest = &after[close + 2..];
                    }
                    // Unterminated block comment: drop the remainder rather
                    // than pretending the rest of the file is code.
                    None => return out,
                }
            }
            None => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// Cut a single line at its first `'` that is not inside a `"..."` string.
///
/// Quote awareness matters: `participant A as "O'Neill"` must survive.
fn strip_line_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '\'' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

/// One meaningful (non-blank, comment-free) source line.
struct Line {
    /// Number of leading whitespace characters, used for note bodies.
    indent: usize,
    /// Content with surrounding whitespace removed.
    text: String,
}

/// Strip comments, drop blank lines and record indentation.
fn collect_lines(src: &str) -> Vec<Line> {
    let src = strip_block_comments(src);
    let mut out = Vec::new();
    for raw in src.split('\n') {
        let raw = raw.trim_end_matches('\r');
        let code = strip_line_comment(raw);
        let text = code.trim();
        if text.is_empty() {
            continue;
        }
        let indent = code.chars().take_while(|c| c.is_whitespace()).count();
        out.push(Line {
            indent,
            text: text.to_owned(),
        });
    }
    out
}

/// Document-level settings lifted out of the body.
#[derive(Default)]
struct Preamble {
    title: Option<String>,
    direction: Direction,
    skin: Skin,
}

/// Colours resolved from `skinparam` statements.
///
/// PlantUML's skin vocabulary is enormous and mostly about spacing and stroke
/// details this renderer does not model, so only the colour families are kept:
/// element background / border / font, package (cluster) background / border and
/// arrow colour. Statements are collected raw and resolved in one pass at the
/// end, which makes the result independent of where they appear.
#[derive(Default)]
struct Skin {
    fill: Option<Rgba>,
    stroke: Option<Rgba>,
    text: Option<Rgba>,
    cluster_fill: Option<Rgba>,
    cluster_stroke: Option<Rgba>,
    edge: Option<Rgba>,
    raw: Vec<(String, String)>,
}

impl Skin {
    fn add(&mut self, key: &str, value: &str) {
        let key = key.trim().to_ascii_lowercase().replace([' ', '-', '_'], "");
        let value = value.trim().to_owned();
        if key.is_empty() || value.is_empty() {
            return;
        }
        self.raw.push((key, value));
    }

    fn resolve(&mut self) {
        for (key, value) in std::mem::take(&mut self.raw) {
            let Some(color) = parse_color(&value) else {
                continue;
            };
            let cluster = key.contains("package")
                || key.contains("cluster")
                || key.contains("group")
                || key.contains("folder");
            if key.ends_with("backgroundcolor") {
                if cluster {
                    self.cluster_fill = Some(color);
                } else {
                    self.fill = Some(color);
                }
            } else if key.ends_with("bordercolor") || key.ends_with("linecolor") {
                if cluster {
                    self.cluster_stroke = Some(color);
                } else {
                    self.stroke = Some(color);
                }
            } else if key.ends_with("arrowcolor") || key.ends_with("arrowfontcolor") {
                self.edge = Some(color);
            } else if key.ends_with("fontcolor") || key.ends_with("textcolor") {
                self.text = Some(color);
            }
        }
    }

    /// Explicit per-element colours always win over the skin.
    fn paint_node(&self, node: &mut Node) {
        if node.fill.is_none() {
            node.fill = self.fill;
        }
        if node.stroke.is_none() {
            node.stroke = self.stroke;
        }
        if node.text_color.is_none() {
            node.text_color = self.text;
        }
    }
}

/// Apply resolved skin colours to a graph without overwriting element colours.
fn apply_skin(graph: &mut Graph, skin: &Skin) {
    for node in &mut graph.nodes {
        skin.paint_node(node);
    }
    for cluster in &mut graph.clusters {
        if cluster.fill.is_none() {
            cluster.fill = skin.cluster_fill;
        }
        if cluster.stroke.is_none() {
            cluster.stroke = skin.cluster_stroke;
        }
    }
    for edge in &mut graph.edges {
        if edge.color.is_none() {
            edge.color = skin.edge;
        }
    }
}

/// Apply resolved skin colours to a sequence diagram.
fn apply_skin_sequence(seq: &mut Sequence, skin: &Skin) {
    for p in &mut seq.participants {
        if p.fill.is_none() {
            p.fill = skin.fill;
        }
        if p.stroke.is_none() {
            p.stroke = skin.stroke;
        }
    }
}

/// Remove optional `@startuml` / `@enduml` delimiters, styling directives and
/// document furniture, capturing `title` and `direction` along the way.
fn extract_preamble(lines: &mut Vec<Line>) -> Preamble {
    let mut pre = Preamble::default();

    if let Some(i) = lines.iter().position(|l| l.text.starts_with("@startuml")) {
        lines.drain(..=i);
    }
    if let Some(i) = lines.iter().position(|l| l.text.starts_with("@enduml")) {
        lines.truncate(i);
    }

    let mut out = Vec::with_capacity(lines.len());
    let mut in_legend = false;
    // `skinparam class { BackgroundColor #red }` block form.
    let mut skin_block: Option<String> = None;

    for line in lines.drain(..) {
        let t = line.text.trim();

        if let Some(prefix) = skin_block.clone() {
            if t == "}" {
                skin_block = None;
                continue;
            }
            let (key, value) = split_first_word(t);
            pre.skin.add(&format!("{prefix}{key}"), value);
            continue;
        }
        if in_legend {
            if t.starts_with("endlegend") || t == "end legend" {
                in_legend = false;
            }
            continue;
        }
        if t.starts_with("@startuml") || t.starts_with("@enduml") {
            continue;
        }
        if let Some(rest) = after_keyword(t, "skinparam") {
            let rest = rest.trim();
            if let Some(prefix) = rest.strip_suffix('{') {
                skin_block = Some(prefix.trim().to_ascii_lowercase());
            } else {
                let (key, value) = split_first_word(rest);
                pre.skin.add(key, value);
            }
            continue;
        }
        if t == "left to right direction" {
            pre.direction = Direction::LeftRight;
            continue;
        }
        if t == "top to bottom direction" {
            pre.direction = Direction::TopDown;
            continue;
        }
        if let Some(rest) = after_keyword(t, "title") {
            if !rest.is_empty() {
                pre.title = Some(rest.to_owned());
            }
            continue;
        }
        if t.starts_with("legend") {
            in_legend = true;
            continue;
        }
        if is_dropped_directive(t) {
            continue;
        }
        out.push(line);
    }

    *lines = out;
    pre
}

/// Directives that describe *how* to draw rather than *what* to draw. They are
/// dropped silently: the local painter picks its own palette.
fn is_dropped_directive(t: &str) -> bool {
    if t.starts_with('!') {
        // !define / !include / !theme / !pragma / ...
        return true;
    }
    if t.starts_with("<style") || t.starts_with("</style") || t.starts_with("<color") {
        return true;
    }
    const KWS: &[&str] = &[
        "skinparam",
        "scale",
        "hide",
        "show",
        "header",
        "footer",
        "caption",
        "legend",
        "endlegend",
        "endheader",
        "endfooter",
        "allowmixing",
        "allow_mixing",
        "skin",
        "style",
    ];
    KWS.iter().any(|kw| after_keyword(t, kw).is_some())
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Pick the diagram family. Sequence is the primary target, so any
/// sequence-only construct beats the "unknown" fallback; declarations that are
/// unambiguous for another family win over that.
fn detect_type(lines: &[Line]) -> Option<DiagramKind> {
    // PlantUML names the diagram family on its first meaningful line. When that
    // line names a family this front-end does not model (gantt, pie, mindmap,
    // ...), say so up front instead of mis-reading its arrows as a sequence.
    const UNSUPPORTED: &[&str] = &[
        "gantt",
        "pie",
        "mindmap",
        "wbs",
        "json",
        "salt",
        "ditaa",
        "timing",
        "archimate",
        "gitgraph",
        "nwdiag",
        "packetdiag",
        "blockdiag",
        "math",
        "latex",
        "regex",
        "network",
    ];
    if let Some(l) = lines.first() {
        let (word, _) = split_first_word(&l.text);
        if UNSUPPORTED.contains(&word) {
            return None;
        }
    }

    let mut activity_kw = false;
    let mut class_kw = false;
    let mut pkg_kw = false;
    let mut state_kw = false;
    let mut usecase_kw = false;
    let mut entity_block = false;
    let mut entity_kw = false;
    let mut seq_kw = false;

    for l in lines {
        let t = l.text.as_str();
        let (first, _) = split_first_word(t);

        match first {
            "class" | "abstract" | "interface" | "enum" | "annotation" | "struct"
            | "exception" | "metaclass" | "stereotype" => class_kw = true,
            "state" => state_kw = true,
            "usecase" | "component" | "node" | "rectangle" => usecase_kw = true,
            "package" | "namespace" | "together" => pkg_kw = true,
            "entity" => {
                entity_kw = true;
                if t.contains('{') {
                    entity_block = true;
                }
            }
            "participant" | "activate" | "deactivate" | "autonumber" | "destroy" | "actor"
            | "boundary" | "control" | "database" | "collections" | "queue" | "alt" | "opt"
            | "loop" | "par" | "break" | "critical" | "group" | "rect" | "else" | "end"
            | "note" => seq_kw = true,
            _ => {}
        }

        if t.contains("[*]") {
            state_kw = true;
        }
        // `<|` / `|>` only appear on class inheritance arrows; `|--` would also
        // match the ER crow's foot `||--o{`, so it is deliberately not used.
        if t.contains("<|") || t.contains("|>") {
            class_kw = true;
        }
        if first.starts_with('(') || first.starts_with('[') {
            usecase_kw = true;
        }
        if find_message_arrow(&strip_colors(&split_label(t).0)).is_some() {
            seq_kw = true;
        }
        // Activity statements: `:action;`, `start`, `if (…) then (…)`, `fork`, …
        if t.starts_with(':')
            || first == "fork"
            || first == "endfork"
            || first == "endwhile"
            || first == "endif"
            || first == "partition"
            || first == "repeat"
            || (first == "start" && t.len() <= 6)
            || (first == "stop" && t.len() <= 6)
            || (first == "if" && t.contains("then"))
        {
            activity_kw = true;
        }
    }

    if activity_kw {
        Some(DiagramKind::Activity)
    } else if class_kw {
        Some(DiagramKind::Class)
    } else if state_kw {
        Some(DiagramKind::State)
    } else if usecase_kw {
        Some(DiagramKind::Flowchart)
    } else if pkg_kw {
        Some(DiagramKind::Class)
    } else if entity_block {
        Some(DiagramKind::Entity)
    } else if seq_kw {
        Some(DiagramKind::Sequence)
    } else if entity_kw {
        // A bare `entity X` is a class keyword when nothing else fits.
        Some(DiagramKind::Class)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Small text helpers
// ---------------------------------------------------------------------------

/// Split off the first whitespace-delimited word.
fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

/// `Some(rest)` when `text` is exactly `kw` or starts with `kw` + whitespace.
///
/// Word-boundary aware, so `title` never matches `titlecase` and `over` never
/// matches `overall`.
fn after_keyword<'a>(text: &'a str, kw: &str) -> Option<&'a str> {
    let text = text.trim();
    if text == kw {
        return Some("");
    }
    let rest = text.strip_prefix(kw)?;
    if rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

/// Strip one layer of surrounding double quotes, if present.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        // `1` and `len()-1` are both char boundaries: the delimiters are ASCII.
        return s[1..s.len() - 1].to_owned();
    }
    s.to_owned()
}

/// Whitespace split that keeps `"quoted runs"` together.
fn split_quoted(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                cur.push(ch);
                quoted = !quoted;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Split `head : label` on the first `:` outside quotes.
fn split_label(s: &str) -> (String, Option<String>) {
    let mut quoted = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                return (s[..i].trim().to_owned(), Some(s[i + 1..].trim().to_owned()));
            }
            _ => {}
        }
    }
    (s.trim().to_owned(), None)
}

/// Remove `[#rrggbb]` / `[#color]` arrow colouring (`-[#blue]->`).
fn strip_colors(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find("[#") {
            Some(open) => {
                out.push_str(&rest[..open]);
                match rest[open..].find(']') {
                    Some(close) => rest = &rest[open + close + 1..],
                    None => return out,
                }
            }
            None => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// Split `head { body }`, returning the part before `{` and the part after.
fn split_brace(s: &str) -> (&str, Option<&str>) {
    match s.find('{') {
        Some(b) => (s[..b].trim(), Some(&s[b + 1..])),
        None => (s.trim(), None),
    }
}

/// Pull `<<stereotype>>` out of a declaration, returning the cleaned text.
fn extract_stereotype(s: &str) -> (String, Option<String>) {
    if let Some(open) = s.find("<<") {
        if let Some(rel) = s[open + 2..].find(">>") {
            let inner = &s[open + 2..open + 2 + rel];
            let mut out = String::new();
            out.push_str(s[..open].trim_end());
            let after = s[open + 2 + rel + 2..].trim_start();
            if !after.is_empty() {
                out.push(' ');
                out.push_str(after);
            }
            return (out, Some(inner.trim().to_owned()));
        }
    }
    (s.to_owned(), None)
}

/// Parse an `id`, `id as "Label"` or `"Label" as id` declaration.
fn parse_decl_text(s: &str) -> (String, String) {
    let (cleaned, _) = extract_stereotype(s);
    let cleaned = cleaned.trim();
    let tokens = split_quoted(cleaned);
    let Some(first) = tokens.first() else {
        return (String::new(), String::new());
    };

    if first.starts_with('"') {
        // Label-first form: `"Long name" as X`.
        let label = unquote(first);
        if tokens.len() >= 3 && tokens[1] == "as" {
            let id = unquote(&tokens[2..].join(" "));
            return (id, label);
        }
        return (label.clone(), label);
    }

    let id = unquote(first);
    if tokens.len() >= 3 && tokens[1] == "as" {
        let label = unquote(&tokens[2..].join(" "));
        return (id, label);
    }
    (id.clone(), id)
}

/// A class body line made only of separators (`--`, `..`, `==`) is a
/// compartment divider, not a member.
fn is_separator(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| matches!(c, '-' | '.' | '=' | '_' | '/' | '\\'))
}

/// Does this first word open a classifier declaration?
fn is_class_keyword(w: &str) -> bool {
    matches!(
        w,
        "class"
            | "abstract"
            | "interface"
            | "enum"
            | "annotation"
            | "entity"
            | "struct"
            | "exception"
            | "metaclass"
            | "stereotype"
    )
}

// ---------------------------------------------------------------------------
// Graph helpers
// ---------------------------------------------------------------------------

/// Ensure `id` exists, attaching it to `cluster` on first sight.
///
/// Kept separate from [`Graph::node_mut`] because the caller often needs the
/// index again straight after, and because cluster membership must be recorded
/// in exactly one place.
fn graph_node(g: &mut Graph, id: &str, cluster: Option<usize>) -> usize {
    g.node_mut(id);
    let idx = g.index_of(id).unwrap_or(0);

    if let Some(c) = cluster {
        if g.nodes[idx].cluster.is_none() {
            g.nodes[idx].cluster = Some(c);
        }
    }
    if let Some(c) = g.nodes[idx].cluster {
        if !g.clusters[c].members.iter().any(|m| m.as_str() == id) {
            g.clusters[c].members.push(id.to_owned());
        }
    }
    idx
}

// ---------------------------------------------------------------------------
// Relations
// ---------------------------------------------------------------------------

/// Arrow decoration found on a class / state / component relation.
#[derive(Clone, Copy)]
struct RelSpec {
    /// Decoration at the left endpoint.
    left: Arrow,
    /// Decoration at the right endpoint.
    right: Arrow,
    line: LineStyle,
    len: usize,
}

/// How relation endpoints should be interpreted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EndpointMode {
    /// Plain names, honouring `"1"` multiplicities (class diagrams).
    Plain,
    /// `(Label)` / `[Label]` shorthand shapes (use case / component).
    Flow,
}

/// Try to match a relation token starting at byte offset `i` (a char boundary).
///
/// Order matters: longer patterns are tested first so `-->` never degrades into
/// `--` and `<|--` never into `<-`.
fn match_relation_at(s: &str, i: usize) -> Option<RelSpec> {
    const TABLE: &[(&str, Arrow, Arrow, LineStyle)] = &[
        ("<|--", Arrow::Hollow, Arrow::None, LineStyle::Solid),
        ("<|..", Arrow::Hollow, Arrow::None, LineStyle::Dashed),
        ("--|>", Arrow::None, Arrow::Hollow, LineStyle::Solid),
        ("..|>", Arrow::None, Arrow::Hollow, LineStyle::Dashed),
        ("<--", Arrow::Filled, Arrow::None, LineStyle::Solid),
        ("-->", Arrow::None, Arrow::Filled, LineStyle::Solid),
        ("..>", Arrow::None, Arrow::Filled, LineStyle::Dashed),
        ("<..", Arrow::Filled, Arrow::None, LineStyle::Dashed),
        ("*--", Arrow::FilledDiamond, Arrow::None, LineStyle::Solid),
        ("--*", Arrow::None, Arrow::FilledDiamond, LineStyle::Solid),
        ("o--", Arrow::Diamond, Arrow::None, LineStyle::Solid),
        ("--o", Arrow::None, Arrow::Diamond, LineStyle::Solid),
        ("<->", Arrow::Filled, Arrow::Filled, LineStyle::Solid),
        ("<-", Arrow::Filled, Arrow::None, LineStyle::Solid),
        ("->", Arrow::None, Arrow::Filled, LineStyle::Solid),
        ("--", Arrow::None, Arrow::None, LineStyle::Solid),
        ("..", Arrow::None, Arrow::None, LineStyle::Dashed),
    ];

    let rest = &s[i..];
    for (pat, left, right, line) in TABLE {
        if rest.starts_with(pat) {
            return Some(RelSpec {
                left: *left,
                right: *right,
                line: *line,
                len: pat.len(),
            });
        }
    }
    None
}

/// Leftmost relation token, skipping quoted spans so that the `..` inside a
/// `"0..*"` multiplicity is not mistaken for a dotted line.
fn find_relation_token(s: &str) -> Option<(usize, RelSpec)> {
    let mut quoted = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '-' | '<' | '.' | '*' | 'o' if !quoted => {
                if let Some(spec) = match_relation_at(s, i) {
                    return Some((i, spec));
                }
            }
            _ => {}
        }
    }
    None
}

/// Build an edge from a relation line. Returns `false` when the line is not a
/// usable relation (no token, or an empty endpoint).
fn parse_relation(
    g: &mut Graph,
    text: &str,
    cluster: Option<usize>,
    mode: EndpointMode,
) -> bool {
    let text = strip_colors(text);
    let (head, label) = split_label(&text);

    let Some((pos, spec)) = find_relation_token(&head) else {
        return false;
    };
    let left_raw = head[..pos].trim();
    let right_raw = head[pos + spec.len..].trim();
    if left_raw.is_empty() || right_raw.is_empty() {
        return false;
    }

    let (from, to, edge_label) = match mode {
        EndpointMode::Flow => {
            let from = flow_endpoint(g, left_raw, cluster);
            let to = flow_endpoint(g, right_raw, cluster);
            (from, to, label)
        }
        EndpointMode::Plain => {
            let (left_id, left_mult) = split_mult(left_raw, true);
            let (right_id, right_mult) = split_mult(right_raw, false);
            if left_id.is_empty() || right_id.is_empty() {
                return false;
            }
            graph_node(g, &left_id, cluster);
            graph_node(g, &right_id, cluster);
            let combined = combine_label(left_mult, right_mult, label);
            (left_id, right_id, combined)
        }
    };

    if from.is_empty() || to.is_empty() {
        return false;
    }

    let mut edge = Edge::new(&from, &to);
    edge.head = spec.right;
    edge.tail = spec.left;
    edge.line = spec.line;
    edge.label = edge_label;
    g.edges.push(edge);
    true
}

/// Split `"0..*" B` / `A "1"` into the class id and its multiplicity.
fn split_mult(raw: &str, left_side: bool) -> (String, Option<String>) {
    let tokens = split_quoted(raw);
    if tokens.is_empty() {
        return (String::new(), None);
    }
    if left_side && tokens.len() >= 2 {
        let last = tokens.last().cloned().unwrap_or_default();
        if last.starts_with('"') && looks_like_multiplicity(&unquote(&last)) {
            let name = tokens[..tokens.len() - 1].join(" ");
            return (unquote(&name), Some(unquote(&last)));
        }
    }
    if !left_side && tokens.len() >= 2 {
        let first = tokens[0].clone();
        if first.starts_with('"') && looks_like_multiplicity(&unquote(&first)) {
            let name = tokens[1..].join(" ");
            return (unquote(&name), Some(unquote(&first)));
        }
    }
    (unquote(raw), None)
}

/// Multiplicities are `1`, `0..*`, `1..n`, `*`, ... — anything else quoted is a
/// quoted class name, not a multiplicity.
fn looks_like_multiplicity(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '*' | 'n' | 'm'))
}

/// Fold multiplicities and the trailing `: label` into one edge label.
fn combine_label(
    left: Option<String>,
    right: Option<String>,
    text: Option<String>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(l) = left {
        parts.push(l);
    }
    if let Some(r) = right {
        parts.push(r);
    }
    if let Some(t) = text {
        if !t.is_empty() {
            parts.push(t);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

// ---------------------------------------------------------------------------
// Sequence diagrams
// ---------------------------------------------------------------------------

/// Message arrow found in a sequence line.
#[derive(Clone, Copy)]
struct MsgArrow {
    start: usize,
    len: usize,
    /// `true` for `<-` family: the head sits on the left, so source and target
    /// swap relative to the written order.
    points_left: bool,
    line: LineStyle,
    head: Arrow,
}

/// Match a sequence message arrow at byte offset `i`.
fn match_msg_arrow(s: &str, i: usize) -> Option<MsgArrow> {
    let rest = &s[i..];

    if rest.starts_with("<-->") {
        return Some(MsgArrow { start: i, len: 4, points_left: true, line: LineStyle::Dashed, head: Arrow::Filled });
    }
    if rest.starts_with("<->") {
        return Some(MsgArrow { start: i, len: 3, points_left: true, line: LineStyle::Solid, head: Arrow::Filled });
    }
    if rest.starts_with("<--") {
        return Some(MsgArrow { start: i, len: 3, points_left: true, line: LineStyle::Dashed, head: Arrow::Filled });
    }
    if rest.starts_with("<-") {
        return Some(MsgArrow { start: i, len: 2, points_left: true, line: LineStyle::Solid, head: Arrow::Filled });
    }

    if rest.starts_with('-') {
        let dashes = rest.chars().take_while(|c| *c == '-').count();
        let after = &rest[dashes..];
        let line = if dashes >= 2 {
            LineStyle::Dashed
        } else {
            LineStyle::Solid
        };
        let mk = |len: usize, head: Arrow| MsgArrow {
            start: i,
            len,
            points_left: false,
            line,
            head,
        };
        // The head decorations are matched longest-first: `->x` is a single
        // token (line `-`, head `>x`), not `->` followed by a stray `x`, which
        // would otherwise be swallowed into the participant name.
        if after.starts_with(">x") {
            return Some(mk(dashes + 2, Arrow::Cross));
        }
        if after.starts_with(">o") {
            return Some(mk(dashes + 2, Arrow::Circle));
        }
        if after.starts_with(">>") {
            return Some(mk(dashes + 2, Arrow::Filled));
        }
        if after.starts_with('>') {
            return Some(mk(dashes + 1, Arrow::Filled));
        }
        if after.starts_with('x') {
            return Some(mk(dashes + 1, Arrow::Cross));
        }
        if after.starts_with('o') || after.starts_with('\\') || after.starts_with('/') {
            return Some(mk(dashes + 1, Arrow::Circle));
        }
    }
    None
}

/// Leftmost message arrow (quote-aware).
fn find_message_arrow(s: &str) -> Option<MsgArrow> {
    let mut quoted = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '<' | '-' if !quoted => {
                if let Some(a) = match_msg_arrow(s, i) {
                    return Some(a);
                }
            }
            _ => {}
        }
    }
    None
}

/// `autonumber` bookkeeping.
struct AutoNumber {
    next: u32,
    step: u32,
}

/// Mutable state threaded through the sequence line loop.
#[derive(Default)]
struct SeqState {
    /// Number of currently open `alt` / `loop` / ... blocks.
    depth: usize,
    /// Monotonic message counter, in source order.
    next_index: u32,
    auto: Option<AutoNumber>,
}

/// Register `id`, creating an undeclared participant on first sight.
fn seq_participant(seq: &mut Sequence, id: &str) -> usize {
    seq.participant_mut(id);
    seq.index_of(id).unwrap_or(0)
}

fn group_kind_of(w: &str) -> Option<GroupKind> {
    Some(match w {
        "alt" => GroupKind::Alt,
        "opt" => GroupKind::Opt,
        "loop" => GroupKind::Loop,
        "par" => GroupKind::Par,
        "break" => GroupKind::Break,
        "critical" => GroupKind::Critical,
        "rect" => GroupKind::Rect,
        "group" => GroupKind::Group,
        _ => return None,
    })
}

fn is_end_note(t: &str) -> bool {
    t == "end note" || t == "endnote"
}

fn parse_sequence(lines: &[Line], pre: &Preamble) -> Sequence {
    let mut seq = Sequence {
        title: pre.title.clone(),
        ..Default::default()
    };
    let mut st = SeqState::default();

    let mut i = 0;
    while i < lines.len() {
        let next = handle_seq_line(&mut seq, &mut st, lines, i);
        i = if next > i { next } else { i + 1 };
    }
    seq
}

/// Consume one sequence statement, returning the next line index.
fn handle_seq_line(seq: &mut Sequence, st: &mut SeqState, lines: &[Line], i: usize) -> usize {
    let text = lines[i].text.clone();
    let t = text.trim();
    let (first, rest) = split_first_word(t);

    if let Some(kind) = group_kind_of(first) {
        seq.steps.push(SeqStep::GroupOpen {
            kind,
            label: rest.to_owned(),
            depth: st.depth,
        });
        st.depth += 1;
        return i + 1;
    }
    if first == "else" {
        seq.steps.push(SeqStep::GroupElse {
            label: rest.to_owned(),
            depth: st.depth.saturating_sub(1),
        });
        return i + 1;
    }
    if first == "end" {
        // A stray `end note` (its block was consumed elsewhere) is not a close.
        if rest == "note" {
            return i + 1;
        }
        st.depth = st.depth.saturating_sub(1);
        seq.steps.push(SeqStep::GroupClose { depth: st.depth });
        return i + 1;
    }
    if first == "activate" || first == "deactivate" || first == "destroy" {
        let name = unquote(rest.trim());
        if !name.is_empty() {
            let idx = seq_participant(seq, &name);
            // `destroy` maps onto deactivation: the IR has no separate notion.
            seq.steps.push(SeqStep::Activate {
                participant: idx,
                on: first == "activate",
            });
        }
        return i + 1;
    }
    if first == "autonumber" {
        apply_autonumber(st, rest);
        return i + 1;
    }
    if matches!(
        first,
        "participant" | "actor" | "boundary" | "control" | "entity" | "database" | "collections"
            | "queue"
    ) {
        declare_participant(seq, first, rest);
        return i + 1;
    }
    if first == "note" {
        return parse_note(seq, lines, i);
    }
    if t.starts_with("==") {
        seq.steps.push(SeqStep::Divider {
            label: t.trim_matches('=').trim().to_owned(),
        });
        return i + 1;
    }
    if t.starts_with("...") {
        seq.steps.push(SeqStep::Delay {
            text: t.trim_matches('.').trim().to_owned(),
        });
        return i + 1;
    }
    if try_message(seq, st, t) {
        return i + 1;
    }
    if !t.is_empty() {
        seq.warnings
            .push(format!("unsupported statement in sequence diagram: {t}"));
    }
    i + 1
}

/// `participant A as "Alice"` and friends.
fn declare_participant(seq: &mut Sequence, keyword: &str, rest: &str) {
    let kind = match keyword {
        "actor" => ParticipantKind::Actor,
        "boundary" => ParticipantKind::Boundary,
        "control" => ParticipantKind::Control,
        "entity" => ParticipantKind::Entity,
        "database" => ParticipantKind::Store,
        "collections" => ParticipantKind::Collections,
        "queue" => ParticipantKind::Queue,
        _ => ParticipantKind::Default,
    };
    let (id, label) = parse_decl_text(rest);
    if id.is_empty() {
        return;
    }
    let p = seq.participant_mut(&id);
    p.kind = kind;
    p.label = label;
    p.declared = true;
}

/// `autonumber`, `autonumber 10`, `autonumber 10 5`, `autonumber stop`.
fn apply_autonumber(st: &mut SeqState, rest: &str) {
    let rest = rest.trim();
    if rest == "stop" {
        st.auto = None;
        return;
    }
    let mut it = rest.split_whitespace();
    let start = it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
    let step = it
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
        .max(1);
    st.auto = Some(AutoNumber { next: start, step });
}

/// Strip a trailing `++` / `--` activation marker from a participant reference.
fn strip_activation(s: &str) -> (String, bool, bool) {
    let mut name = s.trim();
    let mut activate = false;
    let mut deactivate = false;
    if let Some(r) = name.strip_suffix("++") {
        activate = true;
        name = r.trim();
    } else if let Some(r) = name.strip_suffix("--") {
        deactivate = true;
        name = r.trim();
    }
    (name.to_owned(), activate, deactivate)
}

/// Parse a message line, pushing a [`SeqStep::Message`] on success.
fn try_message(seq: &mut Sequence, st: &mut SeqState, line: &str) -> bool {
    let (head, label) = split_label(line);
    let head = strip_colors(&head);

    let Some(arrow) = find_message_arrow(&head) else {
        return false;
    };
    let left_raw = head[..arrow.start].trim();
    let right_raw = head[arrow.start + arrow.len..].trim();
    if left_raw.is_empty() || right_raw.is_empty() {
        return false;
    }

    let (left_name, left_act, left_deact) = strip_activation(left_raw);
    let (right_name, right_act, right_deact) = strip_activation(right_raw);
    let activate = left_act || right_act;
    let deactivate = left_deact || right_deact;

    let (from_id, to_id) = if arrow.points_left {
        (right_name, left_name)
    } else {
        (left_name, right_name)
    };
    let from_id = unquote(&from_id);
    let to_id = unquote(&to_id);
    if from_id.is_empty() || to_id.is_empty() {
        return false;
    }

    let from = seq_participant(seq, &from_id);
    let to = seq_participant(seq, &to_id);

    st.next_index += 1;
    let index = st.next_index;

    let mut text = label.unwrap_or_default();
    if let Some(auto) = st.auto.as_mut() {
        text = if text.is_empty() {
            format!("{}.", auto.next)
        } else {
            format!("{}. {}", auto.next, text)
        };
        auto.next += auto.step;
    }

    seq.steps.push(SeqStep::Message {
        index,
        from,
        to,
        label: text,
        line: arrow.line,
        head: arrow.head,
        activate,
        deactivate,
        self_call: from == to,
    });
    true
}

/// `note ...` — resolves position + targets, then either reads the inline text
/// or a following block terminated by `end note` / dedent.
fn parse_note(seq: &mut Sequence, lines: &[Line], i: usize) -> usize {
    let base = lines[i].indent;
    let (head, inline) = split_label(&lines[i].text);
    let after = after_keyword(head.trim(), "note").unwrap_or("").trim();

    let (position, targets_part) = if let Some(r) = after_keyword(after, "over") {
        (NotePosition::Over, r)
    } else if let Some(r) = after_keyword(after, "left") {
        (NotePosition::Left, r)
    } else if let Some(r) = after_keyword(after, "right") {
        (NotePosition::Right, r)
    } else {
        (NotePosition::Over, after)
    };
    let targets_part = after_keyword(targets_part.trim(), "of").unwrap_or(targets_part);

    let mut over = Vec::new();
    for part in targets_part.split(',') {
        let name = unquote(part.trim());
        if !name.is_empty() {
            over.push(seq_participant(seq, &name));
        }
    }

    if let Some(text) = inline {
        seq.steps.push(SeqStep::Note {
            over,
            position,
            text,
        });
        return i + 1;
    }

    // Multi-line body: prefer the `end note` terminator, fall back to the
    // indentation rule ("a line continues while the next is more indented").
    let mut j = i + 1;
    let mut content: Vec<String> = Vec::new();
    let first_dedented = j >= lines.len() || lines[j].indent <= base;

    if first_dedented {
        if let Some(end) = (j..lines.len()).find(|&k| is_end_note(&lines[k].text)) {
            for k in j..end {
                content.push(lines[k].text.clone());
            }
            j = end + 1;
        }
    } else {
        while j < lines.len() {
            if is_end_note(&lines[j].text) {
                j += 1;
                break;
            }
            if lines[j].indent <= base {
                break;
            }
            content.push(lines[j].text.clone());
            j += 1;
        }
    }

    seq.steps.push(SeqStep::Note {
        over,
        position,
        text: content.join("\n"),
    });
    j
}

// ---------------------------------------------------------------------------
// Class diagrams
// ---------------------------------------------------------------------------

fn parse_class(lines: &[Line], pre: &Preamble) -> Graph {
    let mut g = Graph {
        kind_hint: Some(DiagramKind::Class),
        direction: pre.direction,
        title: pre.title.clone(),
        ..Default::default()
    };
    let mut stack: Vec<usize> = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let text = lines[i].text.clone();
        let next = handle_class_line(&mut g, &mut stack, lines, i, &text);
        i = if next > i { next } else { i + 1 };
    }
    g
}

/// Consume one class-diagram statement, returning the next line index.
fn handle_class_line(
    g: &mut Graph,
    stack: &mut Vec<usize>,
    lines: &[Line],
    i: usize,
    text: &str,
) -> usize {
    let (first, rest) = split_first_word(text);
    let cur = stack.last().copied();

    if first == "}" {
        stack.pop();
        return i;
    }

    if matches!(first, "package" | "namespace" | "together") {
        let (head, after) = split_brace(rest);
        let (id, label) = if first == "together" {
            (
                format!("__together_{}__", g.clusters.len()),
                "together".to_owned(),
            )
        } else {
            parse_decl_text(head)
        };
        if id.is_empty() {
            g.warnings
                .push(format!("cluster without a name: {}", text.trim()));
            return i;
        }
        let idx = g.clusters.len();
        g.clusters.push(Cluster {
            id,
            label,
            members: Vec::new(),
            parent: cur,
                ..Default::default()
        });
        let body_follows = !matches!(after, Some(a) if a.contains('}'));
        if body_follows {
            stack.push(idx);
        }
        return i;
    }

    if is_class_keyword(first) {
        let (head, after) = split_brace(rest);
        let (id, label, stereotype, style) = parse_class_head(first, head);
        if id.is_empty() {
            g.warnings
                .push(format!("class declaration without a name: {}", text.trim()));
            return i;
        }

        let idx = graph_node(g, &id, cur);
        {
            let node = &mut g.nodes[idx];
            node.label = label;
            node.shape = NodeShape::Classifier;
            node.style = style;
            if let Some(st) = stereotype {
                node.stereotype = Some(st);
            }
        }

        let mut body: Vec<String> = Vec::new();
        let mut next = i + 1;
        if let Some(after) = after {
            let inline = after.trim();
            if let Some(close) = inline.find('}') {
                for m in inline[..close].split(';') {
                    let m = m.trim();
                    if !m.is_empty() {
                        body.push(m.to_owned());
                    }
                }
            } else {
                if !inline.is_empty() {
                    body.push(inline.to_owned());
                }
                let mut j = i + 1;
                while j < lines.len() {
                    let bt = lines[j].text.as_str();
                    if bt == "}" || bt.starts_with('}') {
                        j += 1;
                        break;
                    }
                    body.push(bt.to_owned());
                    j += 1;
                }
                next = j;
            }
        }
        apply_class_body(g, idx, &body);
        return next;
    }

    if first == "note" {
        g.warnings
            .push("note attached to a class ignored (unsupported)".to_owned());
        return i;
    }

    if parse_relation(g, text, cur, EndpointMode::Plain) {
        return i;
    }

    g.warnings
        .push(format!("unsupported statement in class diagram: {}", text.trim()));
    i
}

/// Decode `class X`, `abstract class X`, `interface X <<st>>` into node parts.
fn parse_class_head(first: &str, head: &str) -> (String, String, Option<String>, NodeStyle) {
    let mut style = NodeStyle::Default;
    let mut body = head.trim();

    if first == "abstract" {
        style = NodeStyle::Abstract;
        // `abstract class X` — drop the redundant `class` keyword.
        let (_, r) = split_first_word(body);
        body = r;
    }
    if first == "interface" {
        style = NodeStyle::Abstract;
    }

    let (stripped, stereotype) = extract_stereotype(body);
    let (id, label) = parse_decl_text(stripped.trim());

    if let Some(st) = &stereotype {
        let low = st.to_ascii_lowercase();
        if low == "interface" || low == "abstract" {
            style = NodeStyle::Abstract;
        }
    }
    (id, label, stereotype, style)
}

/// Sort body members into `compartments[0]` (fields) and `compartments[1]`
/// (methods) — any line containing `(` is a method.
fn apply_class_body(g: &mut Graph, idx: usize, body: &[String]) {
    let mut fields = Vec::new();
    let mut methods = Vec::new();
    let mut stereotype: Option<String> = None;

    for m in body {
        let m = m.trim();
        if m.is_empty() || is_separator(m) {
            continue;
        }
        if let Some(st) = m.strip_prefix("<<").and_then(|r| r.strip_suffix(">>")) {
            stereotype = Some(st.trim().to_owned());
            continue;
        }
        if m.contains('(') {
            methods.push(m.to_owned());
        } else {
            fields.push(m.to_owned());
        }
    }

    if !fields.is_empty() || !methods.is_empty() {
        g.nodes[idx].compartments = vec![fields, methods];
    }
    if let Some(st) = stereotype {
        let low = st.to_ascii_lowercase();
        g.nodes[idx].stereotype = Some(st);
        if low == "interface" || low == "abstract" {
            g.nodes[idx].style = NodeStyle::Abstract;
        }
    }
}

// ---------------------------------------------------------------------------
// State diagrams
// ---------------------------------------------------------------------------

/// Synthetic terminal bookkeeping for `[*]`.
struct StateCtx {
    stack: Vec<usize>,
    start_n: usize,
    end_n: usize,
}

fn parse_state(lines: &[Line], pre: &Preamble) -> Graph {
    let mut g = Graph {
        kind_hint: Some(DiagramKind::State),
        direction: pre.direction,
        title: pre.title.clone(),
        ..Default::default()
    };
    let mut ctx = StateCtx {
        stack: Vec::new(),
        start_n: 0,
        end_n: 0,
    };

    let mut i = 0;
    while i < lines.len() {
        let text = lines[i].text.clone();
        let next = handle_state_line(&mut g, &mut ctx, lines, i, &text);
        i = if next > i { next } else { i + 1 };
    }
    g
}

fn handle_state_line(
    g: &mut Graph,
    ctx: &mut StateCtx,
    _lines: &[Line],
    i: usize,
    text: &str,
) -> usize {
    // `S1 -[#blue]-> S2`: colour is a rendering hint the local painter ignores.
    let t = strip_colors(text);
    let (first, rest) = split_first_word(&t);
    let cur = ctx.stack.last().copied();

    if first == "}" {
        ctx.stack.pop();
        return i;
    }

    if first == "state" {
        let (head, after) = split_brace(rest);
        let (stripped, _) = extract_stereotype(head);
        let (id, label) = parse_decl_text(stripped.trim());
        if id.is_empty() {
            g.warnings
                .push(format!("state declaration without a name: {}", text.trim()));
            return i;
        }
        let idx = graph_node(g, &id, cur);
        g.nodes[idx].shape = NodeShape::Classifier;
        g.nodes[idx].label = label.clone();

        // A composite state is a cluster; the state itself stays addressable.
        if let Some(after) = after {
            let cidx = g.clusters.len();
            g.clusters.push(Cluster {
                id: id.clone(),
                label,
                members: Vec::new(),
                parent: cur,
                    ..Default::default()
            });
            if !after.trim().contains('}') {
                ctx.stack.push(cidx);
            }
        }
        return i;
    }

    let (head, label) = split_label(&t);
    if let Some((pos, spec)) = find_relation_token(&head) {
        let left_raw = head[..pos].trim();
        let right_raw = head[pos + spec.len..].trim();
        if !left_raw.is_empty() && !right_raw.is_empty() {
            let from = state_endpoint(g, left_raw, true, ctx, cur);
            let to = state_endpoint(g, right_raw, false, ctx, cur);
            if !from.is_empty() && !to.is_empty() {
                let mut edge = Edge::new(&from, &to);
                edge.head = spec.right;
                edge.tail = spec.left;
                edge.line = spec.line;
                edge.label = label;
                g.edges.push(edge);
                return i;
            }
        }
    }

    if !t.is_empty() {
        g.warnings
            .push(format!("unsupported statement in state diagram: {}", t.trim()));
    }
    i
}

/// Synthetic id for the `n`-th `[*]` terminal.
///
/// PlantUML reuses the literal `[*]` for both the start and the end marker, so
/// mapping it straight to a node id would collapse the two into one. Mirroring
/// the Mermaid front-end (`mermaid::terminal_or_node`), the first start keeps
/// `[*]` and the first end becomes `[*-end]`; any further occurrence gets a
/// numbered suffix so repeated markers stay distinct.
fn term_id(is_start: bool, n: usize) -> String {
    match (is_start, n) {
        (true, 0) => "[*]".to_owned(),
        (true, k) => format!("[*-start-{}]", k + 1),
        (false, 0) => "[*-end]".to_owned(),
        (false, k) => format!("[*-end-{}]", k + 1),
    }
}

/// Resolve one endpoint of a state relation, handling `[*]`.
fn state_endpoint(
    g: &mut Graph,
    raw: &str,
    is_source: bool,
    ctx: &mut StateCtx,
    cluster: Option<usize>,
) -> String {
    let raw = raw.trim();
    if raw == "[*]" {
        let id = if is_source {
            let id = term_id(true, ctx.start_n);
            ctx.start_n += 1;
            id
        } else {
            let id = term_id(false, ctx.end_n);
            ctx.end_n += 1;
            id
        };
        let idx = graph_node(g, &id, cluster);
        let node = &mut g.nodes[idx];
        node.label = String::new();
        node.shape = NodeShape::Circle;
        node.style = NodeStyle::Terminal;
        return id;
    }

    let (id, label) = parse_decl_text(raw);
    if id.is_empty() {
        return String::new();
    }
    let idx = graph_node(g, &id, cluster);
    if label != id {
        g.nodes[idx].label = label;
    }
    id
}

// ---------------------------------------------------------------------------
// Use case / component diagrams
// ---------------------------------------------------------------------------

fn parse_flow(lines: &[Line], pre: &Preamble) -> Graph {
    let mut g = Graph {
        kind_hint: Some(DiagramKind::Flowchart),
        direction: pre.direction,
        title: pre.title.clone(),
        ..Default::default()
    };
    let mut stack: Vec<usize> = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let text = lines[i].text.clone();
        let next = handle_flow_line(&mut g, &mut stack, lines, i, &text);
        i = if next > i { next } else { i + 1 };
    }
    g
}

fn handle_flow_line(
    g: &mut Graph,
    stack: &mut Vec<usize>,
    _lines: &[Line],
    i: usize,
    text: &str,
) -> usize {
    let (first, rest) = split_first_word(text);
    let cur = stack.last().copied();

    if first == "}" {
        stack.pop();
        return i;
    }

    if matches!(first, "package" | "rectangle") || (first == "node" && rest.contains('{')) {
        let (head, after) = split_brace(rest);
        let (id, label) = parse_decl_text(head);
        if id.is_empty() {
            g.warnings
                .push(format!("cluster without a name: {}", text.trim()));
            return i;
        }
        let idx = g.clusters.len();
        g.clusters.push(Cluster {
            id,
            label,
            members: Vec::new(),
            parent: cur,
                ..Default::default()
        });
        if !matches!(after, Some(a) if a.contains('}')) {
            stack.push(idx);
        }
        return i;
    }

    if matches!(first, "usecase" | "component" | "node" | "actor") {
        let (head, _) = split_brace(rest);
        let (id, label) = parse_decl_text(head);
        if id.is_empty() {
            g.warnings
                .push(format!("declaration without a name: {}", text.trim()));
            return i;
        }
        let idx = graph_node(g, &id, cur);
        g.nodes[idx].label = label;
        if first == "actor" {
            g.nodes[idx].style = NodeStyle::Actor;
        }
        return i;
    }

    // `(Label) as U` / `[Label] as X`
    if first.starts_with('(') || first.starts_with('[') {
        let (open, close) = if first.starts_with('(') {
            ('(', ')')
        } else {
            ('[', ']')
        };
        if let Some(body) = text.strip_prefix(open) {
            if let Some(end) = body.find(close) {
                let label = unquote(body[..end].trim());
                let tail = body[end + close.len_utf8()..].trim();
                let id = match after_keyword(tail, "as") {
                    Some(r) => unquote(r),
                    None => label.clone(),
                };
                if !id.is_empty() {
                    let idx = graph_node(g, &id, cur);
                    g.nodes[idx].label = label;
                    // `(Label)` is the rounded use-case bubble; `[Label]` a plain box.
                    g.nodes[idx].shape = if open == '(' {
                        NodeShape::Round
                    } else {
                        NodeShape::Rect
                    };
                    return i;
                }
            }
        }
    }

    if parse_relation(g, text, cur, EndpointMode::Flow) {
        return i;
    }

    g.warnings
        .push(format!("unsupported statement in component diagram: {}", text.trim()));
    i
}

/// Resolve one endpoint of a component relation, honouring `(Label)` / `[Label]`.
fn flow_endpoint(g: &mut Graph, raw: &str, cluster: Option<usize>) -> String {
    let raw = raw.trim();

    if let Some(inner) = raw.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        let name = unquote(inner.trim());
        if name.is_empty() {
            return String::new();
        }
        let idx = graph_node(g, &name, cluster);
        g.nodes[idx].label = name.clone();
        g.nodes[idx].shape = NodeShape::Round;
        return name;
    }
    if let Some(inner) = raw.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        let name = unquote(inner.trim());
        if name.is_empty() {
            return String::new();
        }
        let idx = graph_node(g, &name, cluster);
        g.nodes[idx].label = name.clone();
        g.nodes[idx].shape = NodeShape::Rect;
        return name;
    }

    let (id, label) = parse_decl_text(raw);
    if id.is_empty() {
        return String::new();
    }
    let idx = graph_node(g, &id, cluster);
    if label != id {
        g.nodes[idx].label = label;
    }
    id
}

// ---------------------------------------------------------------------------
// ER / IE diagrams
// ---------------------------------------------------------------------------

fn parse_er(lines: &[Line], pre: &Preamble) -> Graph {
    let mut g = Graph {
        kind_hint: Some(DiagramKind::Entity),
        direction: pre.direction,
        title: pre.title.clone(),
        ..Default::default()
    };

    let mut i = 0;
    while i < lines.len() {
        let text = lines[i].text.clone();
        let next = handle_er_line(&mut g, lines, i, &text);
        i = if next > i { next } else { i + 1 };
    }
    g
}

fn handle_er_line(g: &mut Graph, lines: &[Line], i: usize, text: &str) -> usize {
    let (first, rest) = split_first_word(text);

    if first == "entity" {
        let (head, after) = split_brace(rest);
        let (stripped, _) = extract_stereotype(head);
        let (id, label) = parse_decl_text(stripped.trim());
        if id.is_empty() {
            g.warnings
                .push(format!("entity without a name: {}", text.trim()));
            return i;
        }
        let idx = graph_node(g, &id, None);
        g.nodes[idx].label = label;
        g.nodes[idx].shape = NodeShape::Classifier;

        let mut body: Vec<String> = Vec::new();
        let mut next = i + 1;
        if let Some(after) = after {
            let inline = after.trim();
            if let Some(close) = inline.find('}') {
                for m in inline[..close].split(';') {
                    let m = m.trim();
                    if !m.is_empty() {
                        body.push(m.to_owned());
                    }
                }
            } else {
                if !inline.is_empty() {
                    body.push(inline.to_owned());
                }
                let mut j = i + 1;
                while j < lines.len() {
                    let bt = lines[j].text.as_str();
                    if bt == "}" || bt.starts_with('}') {
                        j += 1;
                        break;
                    }
                    if !bt.trim().is_empty() {
                        body.push(bt.to_owned());
                    }
                    j += 1;
                }
                next = j;
            }
        }
        if !body.is_empty() {
            g.nodes[idx].compartments = vec![body];
        }
        return next;
    }

    if let Some((pos, spec)) = find_relation_token(text) {
        let left = clean_er_endpoint(&text[..pos]);
        let right = clean_er_endpoint(&text[pos + spec.len..]);
        if !left.is_empty() && !right.is_empty() {
            graph_node(g, &left, None);
            graph_node(g, &right, None);
            let mut edge = Edge::new(&left, &right);
            edge.head = Arrow::None;
            edge.tail = Arrow::None;
            edge.line = LineStyle::Solid;
            g.edges.push(edge);
            return i;
        }
    }

    g.warnings
        .push(format!("unsupported statement in ER diagram: {}", text.trim()));
    i
}

/// Strip crow's-foot decoration (`||`, `o{`, `}|`) from an ER endpoint.
fn clean_er_endpoint(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .trim_matches(|c: char| matches!(c, '|' | '{' | '}' | ' '))
        .trim()
        .to_owned()
}

// ---------------------------------------------------------------------------
// Fallback
// ---------------------------------------------------------------------------

/// Unknown diagram family: keep whatever simple `A --> B` edges can be salvaged
/// so the user still sees something meaningful.
fn parse_fallback(lines: &[Line], pre: &Preamble) -> Graph {
    let mut g = Graph {
        kind_hint: Some(DiagramKind::Flowchart),
        direction: pre.direction,
        title: pre.title.clone(),
        ..Default::default()
    };
    g.warnings
        .push("unsupported PlantUML diagram type".to_owned());

    for l in lines {
        if find_relation_token(&l.text).is_some() {
            let _ = parse_relation(&mut g, &l.text, None, EndpointMode::Plain);
        }
    }
    g
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Activity diagrams (PlantUML "beta" syntax)
// ---------------------------------------------------------------------------

/// Cursor over the activity statements.
struct ActParser<'a> {
    lines: &'a [Line],
    i: usize,
    warnings: Vec<String>,
}

impl<'a> ActParser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.i).map(|l| l.text.trim())
    }

    fn bump(&mut self) {
        self.i += 1;
    }

    fn warn(&mut self, msg: String) {
        if !self.warnings.iter().any(|w| *w == msg) {
            self.warnings.push(msg);
        }
    }

    /// Parse statements until one of `stops`. The stop keyword is left for the
    /// caller so `if`/`while`/`fork` can decide how to close themselves.
    fn block(&mut self, stops: &[&str]) -> Vec<ActNode> {
        let mut out = Vec::new();
        while let Some(text) = self.peek() {
            if text.is_empty() {
                self.bump();
                continue;
            }
            let (word, rest) = split_first_word(text);
            let lower = word.to_ascii_lowercase();
            if text == "}" || stops.iter().any(|s| lower == *s) {
                break;
            }
            match lower.as_str() {
                "start" => {
                    out.push(ActNode::Terminal(Terminal::Start));
                    self.bump();
                }
                "stop" | "end" | "kill" | "detach" => {
                    out.push(ActNode::Terminal(Terminal::Stop));
                    self.bump();
                }
                "if" => {
                    if let Some(node) = self.parse_if(rest) {
                        out.push(node);
                    }
                }
                "while" => {
                    if let Some(node) = self.parse_while(rest) {
                        out.push(node);
                    }
                }
                "repeat" => {
                    if let Some(node) = self.parse_repeat() {
                        out.push(node);
                    }
                }
                "fork" => {
                    if let Some(node) = self.parse_fork() {
                        out.push(node);
                    }
                }
                "partition" => {
                    if let Some(node) = self.parse_partition(rest) {
                        out.push(node);
                    }
                }
                "note" | "floating" => {
                    self.warn(format!("activity note 已忽略: {}", truncate_text(text, 40)));
                    self.bump();
                }
                "switch" | "case" | "label" | "goto" | "backward" => {
                    self.warn(format!("activity 暂不支持: {}", truncate_text(text, 40)));
                    self.bump();
                }
                _ => {
                    // Arrow-only connectors (`-> label;`) carry nothing to draw.
                    if text.starts_with("->") || text.starts_with(';') {
                        self.bump();
                        continue;
                    }
                    if let Some(node) = self.parse_action(text) {
                        out.push(node);
                    } else {
                        self.warn(format!(
                            "无法识别的 activity 语句: {}",
                            truncate_text(text, 40)
                        ));
                        self.bump();
                    }
                }
            }
        }
        out
    }

    /// `:label;`, optionally preceded by `#colour`.
    fn parse_action(&mut self, text: &str) -> Option<ActNode> {
        let body = text.strip_prefix(':')?;
        let body = body.strip_suffix(';').unwrap_or(body);
        let (fill, label) = match body.trim().strip_prefix('#') {
            Some(rest) => {
                let (color, label) = split_first_word(rest);
                (parse_color(color), label.trim().to_owned())
            }
            None => (None, body.trim().to_owned()),
        };
        self.bump();
        if label.is_empty() {
            return None;
        }
        Some(ActNode::Action {
            label: label.replace("\\n", "\n"),
            fill,
        })
    }

    fn parse_if(&mut self, rest: &str) -> Option<ActNode> {
        let cond = between(rest, '(', ')').unwrap_or_else(|| rest.trim().to_owned());
        let then_label = rest
            .split("then")
            .nth(1)
            .map(|tail| between(tail, '(', ')').unwrap_or_default())
            .unwrap_or_default();
        let cond = if cond.is_empty() { then_label.clone() } else { cond };
        self.bump();

        let mut branches = vec![ActBranch {
            label: if then_label.is_empty() {
                "yes".to_owned()
            } else {
                then_label
            },
            body: self.block(&["else", "elseif", "endif"]),
        }];
        while let Some(text) = self.peek() {
            let (word, tail) = split_first_word(text);
            match word.to_ascii_lowercase().as_str() {
                "elseif" => {
                    let label = between(tail, '(', ')').unwrap_or_else(|| tail.trim().to_owned());
                    self.bump();
                    branches.push(ActBranch {
                        label,
                        body: self.block(&["else", "elseif", "endif"]),
                    });
                }
                "else" => {
                    let label = between(tail, '(', ')').unwrap_or_default();
                    self.bump();
                    branches.push(ActBranch {
                        label: if label.is_empty() {
                            "else".to_owned()
                        } else {
                            label
                        },
                        body: self.block(&["endif"]),
                    });
                }
                "endif" => {
                    self.bump();
                    break;
                }
                _ => break,
            }
        }
        let _ = cond;
        Some(ActNode::If { branches })
    }

    fn parse_while(&mut self, rest: &str) -> Option<ActNode> {
        let test = between(rest, '(', ')').unwrap_or_else(|| rest.trim().to_owned());
        let yes = rest
            .split(" is ")
            .nth(1)
            .map(|tail| between(tail, '(', ')').unwrap_or_default())
            .unwrap_or_default();
        let no = rest
            .split(" not ")
            .nth(1)
            .map(|tail| between(tail, '(', ')').unwrap_or_default())
            .unwrap_or_default();
        self.bump();
        let body = self.block(&["endwhile"]);
        if self
            .peek()
            .is_some_and(|t| t.trim().eq_ignore_ascii_case("endwhile"))
        {
            self.bump();
        }
        Some(ActNode::Loop {
            kind: LoopKind::While,
            test,
            yes: if yes.is_empty() { "yes".to_owned() } else { yes },
            no: if no.is_empty() { "no".to_owned() } else { no },
            body,
        })
    }

    fn parse_repeat(&mut self) -> Option<ActNode> {
        self.bump();
        let body = self.block(&["repeat"]);
        let mut test = String::new();
        let mut yes = "yes".to_owned();
        let mut no = "no".to_owned();
        if let Some(text) = self.peek() {
            let lower = text.to_ascii_lowercase();
            if lower.starts_with("repeat while") || lower.starts_with("repeatwhile") {
                let rest = text.split_once("while").map(|(_, r)| r).unwrap_or(text);
                test = between(rest, '(', ')').unwrap_or_else(|| rest.trim().to_owned());
                if let Some(tail) = rest.split(" is ").nth(1) {
                    let label = between(tail, '(', ')').unwrap_or_default();
                    if !label.is_empty() {
                        yes = label;
                    }
                }
                if let Some(tail) = rest.split(" not ").nth(1) {
                    let label = between(tail, '(', ')').unwrap_or_default();
                    if !label.is_empty() {
                        no = label;
                    }
                }
            }
            self.bump();
        }
        Some(ActNode::Loop {
            kind: LoopKind::Repeat,
            test,
            yes,
            no,
            body,
        })
    }

    fn parse_fork(&mut self) -> Option<ActNode> {
        self.bump();
        let mut lanes = vec![self.block(&["fork", "end", "endfork"])];
        while let Some(text) = self.peek() {
            let lower = text.to_ascii_lowercase();
            if lower == "fork again" || lower == "forkagain" {
                self.bump();
                lanes.push(self.block(&["fork", "end", "endfork"]));
            } else {
                break;
            }
        }
        if self
            .peek()
            .is_some_and(|t| t.eq_ignore_ascii_case("end fork") || t.eq_ignore_ascii_case("endfork") || t == "end")
        {
            self.bump();
        }
        Some(ActNode::Fork { lanes })
    }

    fn parse_partition(&mut self, rest: &str) -> Option<ActNode> {
        let label = unquote(rest.trim().trim_end_matches('{'))
            .trim()
            .to_owned();
        self.bump();
        let body = self.block(&[]);
        if self.peek() == Some("}") {
            self.bump();
        }
        Some(ActNode::Partition {
            label: if label.is_empty() {
                "分区".to_owned()
            } else {
                label
            },
            body,
        })
    }
}

/// Text inside the first `open`…`close` pair.
fn between(text: &str, open: char, close: char) -> Option<String> {
    let start = text.find(open)?;
    let rest = &text[start + 1..];
    let end = rest.find(close)?;
    Some(rest[..end].trim().to_owned())
}

/// Shorten a statement for a warning message.
fn truncate_text(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// Parse a PlantUML activity diagram. Never panics.
fn parse_activity(lines: &[Line], pre: &Preamble) -> Activity {
    let mut parser = ActParser {
        lines,
        i: 0,
        warnings: Vec::new(),
    };
    let nodes = parser.block(&[]);
    Activity {
        title: pre.title.clone(),
        nodes,
        warnings: parser.warnings,
    }
}


    // -- activity -----------------------------------------------------------

    #[test]
    fn activity_parses_the_beta_syntax() {
        let d = parse(
            "@startuml\n\
             start\n\
             :准备;\n\
             if (有权限?) then (是)\n\
             \x20 :执行;\n\
             else (否)\n\
             \x20 :拒绝;\n\
             endif\n\
             while (还有任务?) is (是)\n\
             \x20 :处理一个;\n\
             endwhile\n\
             fork\n\
             \x20 :甲;\n\
             fork again\n\
             \x20 :乙;\n\
             end fork\n\
             partition \"阶段二\" {\n\
             \x20 :收尾;\n\
             }\n\
             stop\n\
             @enduml",
        );
        let Diagram::Activity(a) = d else {
            panic!("expected an activity diagram")
        };
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
        // start, action, if, while, fork, partition, stop
        assert_eq!(a.nodes.len(), 7);

        match &a.nodes[1] {
            ActNode::Action { label, .. } => assert_eq!(label, "准备"),
            other => panic!("expected an action, got {other:?}"),
        }
        match &a.nodes[2] {
            ActNode::If { branches } => {
                assert_eq!(branches.len(), 2);
                assert_eq!(branches[0].label, "是");
                assert_eq!(branches[1].label, "否");
                assert_eq!(branches[0].body.len(), 1);
            }
            other => panic!("expected an if, got {other:?}"),
        }
        match &a.nodes[3] {
            ActNode::Loop { kind, yes, body, .. } => {
                assert_eq!(*kind, LoopKind::While);
                assert_eq!(yes, "是");
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected a loop, got {other:?}"),
        }
        match &a.nodes[4] {
            ActNode::Fork { lanes } => {
                assert_eq!(lanes.len(), 2);
                assert_eq!(lanes[0].len(), 1);
                assert_eq!(lanes[1].len(), 1);
            }
            other => panic!("expected a fork, got {other:?}"),
        }
        match &a.nodes[5] {
            ActNode::Partition { label, body } => {
                assert_eq!(label, "阶段二");
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected a partition, got {other:?}"),
        }
        assert!(matches!(a.nodes[6], ActNode::Terminal(Terminal::Stop)));
    }

    #[test]
    fn activity_parses_repeat_and_coloured_actions() {
        let d = parse(
            "start\nrepeat\n :#lightblue 重试;\nrepeat while (成功?) is (否) not (是)\nstop",
        );
        let Diagram::Activity(a) = d else {
            panic!("expected an activity diagram")
        };
        let ActNode::Loop { kind, test, yes, no, body } = &a.nodes[1] else {
            panic!("expected a repeat loop: {:?}", a.nodes)
        };
        assert_eq!(*kind, LoopKind::Repeat);
        assert_eq!(test, "成功?");
        assert_eq!(yes, "否");
        assert_eq!(no, "是");
        match &body[0] {
            ActNode::Action { label, fill } => {
                assert_eq!(label, "重试");
                assert!(fill.is_some(), "#lightblue 应被解析成填充色");
            }
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn activity_warns_about_unsupported_statements() {
        let d = parse("start\n:ok;\nnote right: 说明\nswitch (x)\nstop");
        let Diagram::Activity(a) = d else {
            panic!("expected an activity diagram")
        };
        assert!(a.warnings.iter().any(|w| w.contains("note")));
        assert!(a.warnings.iter().any(|w| w.contains("switch")));
        // The supported statements still parse.
        assert!(a.nodes.len() >= 3);
    }

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::diagram::model::parse_color;
    use crate::diagram::model::{
        Arrow, Diagram, DiagramKind, Direction, Graph, GroupKind, LineStyle, NodeShape, NodeStyle,
        ParticipantKind, SeqStep, Sequence,
    };

    fn as_seq(d: &Diagram) -> &Sequence {
        match d {
            Diagram::Sequence(s) => s,
            other => panic!("expected a sequence diagram, got {other:?}"),
        }
    }

    fn as_graph(d: &Diagram) -> &Graph {
        match d {
            Diagram::Graph(g) => g,
            other => panic!("expected a graph diagram, got {other:?}"),
        }
    }

    fn messages(s: &Sequence) -> Vec<&SeqStep> {
        s.steps
            .iter()
            .filter(|st| matches!(st, SeqStep::Message { .. }))
            .collect()
    }

    fn node<'a>(g: &'a Graph, id: &str) -> &'a crate::diagram::model::Node {
        g.node(id).unwrap_or_else(|| panic!("missing node {id}"))
    }

    // -- sequence ----------------------------------------------------------

    #[test]
    fn full_sequence_diagram() {
        let src = "\
@startuml
autonumber
participant A as \"Alice\"
actor Bob
database DB
A -> A : self call
A ->> Bob : hello
Bob --> A : ok
A ->x DB : fail
activate DB
DB -->> A : data
deactivate DB
note over A, Bob
  first line
  second line
end note
alt success
  A -> Bob : done
else failure
  Bob --> A : nope
end
== phase two ==
...waiting...
@enduml";

        let d = parse(src);
        let s = as_seq(&d);

        assert_eq!(s.participants.len(), 3);
        assert_eq!(s.participants[0].label, "Alice");
        assert_eq!(s.participants[0].kind, ParticipantKind::Default);
        assert_eq!(s.participants[1].kind, ParticipantKind::Actor);
        assert_eq!(s.participants[2].kind, ParticipantKind::Store);
        assert!(s.participants.iter().all(|p| p.declared));

        // self message
        assert!(s.steps.iter().any(|st| matches!(
            st,
            SeqStep::Message { self_call: true, .. }
        )));

        // autonumber prefixes, in order
        let msgs = messages(&s);
        assert_eq!(msgs.len(), 7);
        match msgs[0] {
            SeqStep::Message { label, .. } => assert_eq!(label, "1. self call"),
            _ => unreachable!(),
        }
        match msgs[1] {
            SeqStep::Message {
                label,
                head,
                line,
                ..
            } => {
                assert_eq!(label, "2. hello");
                assert_eq!(*head, Arrow::Filled);
                assert_eq!(*line, LineStyle::Solid);
            }
            _ => unreachable!(),
        }
        match msgs[2] {
            SeqStep::Message { line, .. } => assert_eq!(*line, LineStyle::Dashed),
            _ => unreachable!(),
        }
        match msgs[3] {
            SeqStep::Message { head, .. } => assert_eq!(*head, Arrow::Cross),
            _ => unreachable!(),
        }

        // standalone activate / deactivate
        assert!(s
            .steps
            .iter()
            .any(|st| matches!(st, SeqStep::Activate { on: true, .. })));
        assert!(s
            .steps
            .iter()
            .any(|st| matches!(st, SeqStep::Activate { on: false, .. })));

        // note over two participants, multi-line
        let note = s.steps.iter().find_map(|st| match st {
            SeqStep::Note { over, text, .. } if over.len() == 2 => Some(text),
            _ => None,
        });
        assert_eq!(note.map(String::as_str), Some("first line\nsecond line"));

        // groups
        assert!(s.steps.iter().any(|st| matches!(
            st,
            SeqStep::GroupOpen {
                kind: GroupKind::Alt,
                depth: 0,
                ..
            }
        )));
        assert!(s.steps.iter().any(|st| matches!(
            st,
            SeqStep::GroupElse { label, depth: 0 } if label == "failure"
        )));
        assert!(s
            .steps
            .iter()
            .any(|st| matches!(st, SeqStep::GroupClose { depth: 0 })));

        // divider + delay
        assert!(s.steps.iter().any(|st| matches!(
            st,
            SeqStep::Divider { label } if label == "phase two"
        )));
        assert!(s.steps.iter().any(|st| matches!(
            st,
            SeqStep::Delay { text } if text == "waiting"
        )));
    }

    #[test]
    fn reverse_arrow_swaps_source_and_target() {
        let src = "participant A\nparticipant B\nA <-- B : reply";
        let d = parse(src);
        let s = as_seq(&d);
        let (a, b) = (s.index_of("A"), s.index_of("B"));
        let msg = s.steps.iter().find_map(|st| match st {
            SeqStep::Message { from, to, line, .. } => Some((*from, *to, *line)),
            _ => None,
        });
        assert_eq!(msg, Some((b.unwrap(), a.unwrap(), LineStyle::Dashed)));
    }

    #[test]
    fn label_first_participant_and_kinds() {
        let src = "participant \"Long name\" as X\nboundary W\nqueue Q\ncontrol C\nentity E\ncollections K";
        let d = parse(src);
        let s = as_seq(&d);
        assert_eq!(s.participants[0].id, "X");
        assert_eq!(s.participants[0].label, "Long name");
        assert_eq!(s.participants[0].kind, ParticipantKind::Default);
        assert_eq!(s.participants[1].kind, ParticipantKind::Boundary);
        assert_eq!(s.participants[2].kind, ParticipantKind::Queue);
        assert_eq!(s.participants[3].kind, ParticipantKind::Control);
        assert_eq!(s.participants[4].kind, ParticipantKind::Entity);
        assert_eq!(s.participants[5].kind, ParticipantKind::Collections);
    }

    #[test]
    fn autonumber_custom_start_and_step() {
        let d = parse("autonumber 10 5\nA -> B : first\nA -> B : second");
        let s = as_seq(&d);
        let labels: Vec<&str> = s
            .steps
            .iter()
            .filter_map(|st| match st {
                SeqStep::Message { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["10. first", "15. second"]);
    }

    #[test]
    fn undeclared_participants_are_created_in_order() {
        let d = parse("X -> Y : hi\nZ -> X : yo");
        let s = as_seq(&d);
        let ids: Vec<&str> = s.participants.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["X", "Y", "Z"]);
        assert!(s.participants.iter().all(|p| !p.declared));
    }

    #[test]
    fn note_over_single_target_inline_and_block() {
        let d = parse("participant A\nnote over A : inline\nnote left of A\n  block\nend note");
        let s = as_seq(&d);
        let notes: Vec<&SeqStep> = s
            .steps
            .iter()
            .filter(|st| matches!(st, SeqStep::Note { .. }))
            .collect();
        assert_eq!(notes.len(), 2);
        match notes[0] {
            SeqStep::Note { text, over, .. } => {
                assert_eq!(text, "inline");
                assert_eq!(over.len(), 1);
            }
            _ => unreachable!(),
        }
        match notes[1] {
            SeqStep::Note { text, position, .. } => {
                assert_eq!(text, "block");
                assert_eq!(*position, crate::diagram::model::NotePosition::Left);
            }
            _ => unreachable!(),
        }
    }

    // -- class -------------------------------------------------------------

    #[test]
    fn class_fields_methods_and_stereotype() {
        let src = "\
class Foo <<interface>> {
  +name: String
  -count: int
  +run(): void
}";
        let d = parse(src);
        let g = as_graph(&d);
        assert_eq!(g.kind_hint, Some(DiagramKind::Class));
        let foo = node(g, "Foo");
        assert_eq!(foo.shape, NodeShape::Classifier);
        assert_eq!(foo.stereotype.as_deref(), Some("interface"));
        assert_eq!(foo.style, NodeStyle::Abstract);
        assert_eq!(foo.compartments.len(), 2);
        assert_eq!(foo.compartments[0], vec!["+name: String", "-count: int"]);
        assert_eq!(foo.compartments[1], vec!["+run(): void"]);
    }

    #[test]
    fn class_stereotype_on_body_line() {
        let src = "class Bar {\n  <<abstract>>\n  -x: int\n}";
        let d = parse(src);
        let g = as_graph(&d);
        let bar = node(g, "Bar");
        assert_eq!(bar.stereotype.as_deref(), Some("abstract"));
        assert_eq!(bar.style, NodeStyle::Abstract);
        assert_eq!(bar.compartments[0], vec!["-x: int"]);
    }

    #[test]
    fn class_inheritance_and_multiplicity() {
        let src = "\
class Base
class Derived
Base <|-- Derived
Foo \"1\" *-- \"0..*\" Derived : owns";
        let d = parse(src);
        let g = as_graph(&d);

        let inherit = g
            .edges
            .iter()
            .find(|e| e.from == "Base" && e.to == "Derived")
            .expect("inheritance edge");
        assert_eq!(inherit.tail, Arrow::Hollow);
        assert_eq!(inherit.head, Arrow::None);
        assert_eq!(inherit.line, LineStyle::Solid);

        let compose = g
            .edges
            .iter()
            .find(|e| e.from == "Foo" && e.to == "Derived")
            .expect("composition edge");
        assert_eq!(compose.tail, Arrow::FilledDiamond);
        assert_eq!(compose.label.as_deref(), Some("1 0..* owns"));
    }

    #[test]
    fn class_dashed_dependency_and_dotted_realization() {
        let src = "A ..> B\nC <|.. D";
        let d = parse(src);
        let g = as_graph(&d);
        let dep = g.edges.iter().find(|e| e.from == "A").expect("dependency");
        assert_eq!(dep.line, LineStyle::Dashed);
        assert_eq!(dep.head, Arrow::Filled);

        let real = g.edges.iter().find(|e| e.from == "C").expect("realization");
        assert_eq!(real.tail, Arrow::Hollow);
        assert_eq!(real.line, LineStyle::Dashed);
    }

    #[test]
    fn nested_packages_become_nested_clusters() {
        let src = "\
package Outer {
  package Inner {
    class X
  }
  class Y
}";
        let d = parse(src);
        let g = as_graph(&d);
        assert_eq!(g.clusters.len(), 2);
        assert_eq!(g.clusters[0].label, "Outer");
        assert_eq!(g.clusters[1].label, "Inner");
        assert_eq!(g.clusters[1].parent, Some(0));
        assert_eq!(g.clusters[1].members, vec!["X"]);
        assert_eq!(g.clusters[0].members, vec!["Y"]);
        assert_eq!(node(g, "X").cluster, Some(1));
        assert_eq!(node(g, "Y").cluster, Some(0));
    }

    #[test]
    fn note_on_class_is_a_warning_not_a_failure() {
        let d = parse("class A\nnote left of A");
        let g = as_graph(&d);
        assert!(!g.warnings.is_empty());
        assert!(g.node("A").is_some());
    }

    // -- state -------------------------------------------------------------

    #[test]
    fn state_diagram_with_both_terminals() {
        let src = "\
[*] --> Idle
Idle --> Running : start
Running --> [*]
state Running {
  state Paused
}";
        let d = parse(src);
        let g = as_graph(&d);
        assert_eq!(g.kind_hint, Some(DiagramKind::State));

        let start = node(g, "[*]");
        let end = node(g, "[*-end]");
        assert_eq!(start.style, NodeStyle::Terminal);
        assert_eq!(start.shape, NodeShape::Circle);
        assert_eq!(end.style, NodeStyle::Terminal);
        assert_eq!(end.shape, NodeShape::Circle);

        // start and end are distinct nodes
        assert_ne!(start.id, end.id);

        // composite state became a cluster containing Paused
        let running_cluster = g
            .clusters
            .iter()
            .find(|c| c.id == "Running")
            .expect("Running cluster");
        assert_eq!(running_cluster.members, vec!["Paused"]);
        assert_eq!(node(g, "Idle").style, NodeStyle::Default);
    }

    #[test]
    fn state_relation_colour_is_stripped() {
        let src = "state A\nstate B\nA -[#blue]-> B : go";
        let d = parse(src);
        let g = as_graph(&d);
        let e = g.edges.iter().find(|e| e.from == "A").expect("edge");
        assert_eq!(e.to, "B");
        assert_eq!(e.label.as_deref(), Some("go"));
    }

    // -- use case / component ---------------------------------------------

    #[test]
    fn usecase_and_component_diagram() {
        let src = "\
usecase UC as \"Login\"
actor U as \"User\"
component C as \"Backend\"
U --> UC
C ..> UC";
        let d = parse(src);
        let g = as_graph(&d);
        assert_eq!(g.kind_hint, Some(DiagramKind::Flowchart));

        assert_eq!(node(g, "UC").label, "Login");
        assert_eq!(node(g, "UC").shape, NodeShape::Rect);
        assert_eq!(node(g, "U").style, NodeStyle::Actor);
        assert_eq!(node(g, "C").label, "Backend");

        let u = g.edges.iter().find(|e| e.from == "U").expect("U edge");
        assert_eq!(u.to, "UC");
        let c = g.edges.iter().find(|e| e.from == "C").expect("C edge");
        assert_eq!(c.to, "UC");
        assert_eq!(c.line, LineStyle::Dashed);
    }

    #[test]
    fn shorthand_usecase_shapes() {
        let src = "(Login) as L\n[Backend] as B\nL --> B";
        let d = parse(src);
        let g = as_graph(&d);
        assert_eq!(node(g, "L").label, "Login");
        assert_eq!(node(g, "L").shape, NodeShape::Round);
        assert_eq!(node(g, "B").label, "Backend");
        assert_eq!(node(g, "B").shape, NodeShape::Rect);
    }

    // -- ER ----------------------------------------------------------------

    #[test]
    fn er_entities_and_relations() {
        let src = "\
entity CUSTOMER {
  string id
  string name
}
entity ORDER {
  string id
}
CUSTOMER ||--o{ ORDER";
        let d = parse(src);
        let g = as_graph(&d);
        assert_eq!(g.kind_hint, Some(DiagramKind::Entity));
        assert_eq!(
            node(g, "CUSTOMER").compartments[0],
            vec!["string id", "string name"]
        );
        let e = g
            .edges
            .iter()
            .find(|e| e.from == "CUSTOMER")
            .expect("relation");
        assert_eq!(e.to, "ORDER");
    }

    // -- preprocessing -----------------------------------------------------

    #[test]
    fn delimiters_are_optional() {
        let with = parse("@startuml\nA -> B : hi\n@enduml");
        let without = parse("A -> B : hi");
        assert_eq!(with.summary(), without.summary());
        assert_eq!(messages(as_seq(&with)).len(), 1);
    }

    #[test]
    fn comments_are_stripped() {
        let src = "\
@startuml
' a full line comment
/' a block
comment spanning lines '/
participant C ' trailing comment
A -> B : hi ' and here
@enduml";
        let d = parse(src);
        let s = as_seq(&d);
        let ids: Vec<&str> = s.participants.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["C", "A", "B"]);
        match messages(&s)[0] {
            SeqStep::Message { label, .. } => assert_eq!(label, "hi"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn left_to_right_direction_is_honoured() {
        let src = "left to right direction\nclass A\nA --> B";
        let d = parse(src);
        let g = as_graph(&d);
        assert_eq!(g.direction, Direction::LeftRight);

        let d2 = parse("top to bottom direction\nclass A\nA --> B");
        let td = as_graph(&d2);
        assert_eq!(td.direction, Direction::TopDown);
    }

    #[test]
    fn title_and_skinparam_are_handled() {
        let src = "\
@startuml
skinparam monochrome true
hide members
title My Diagram
class A
@enduml";
        let d = parse(src);
        let g = as_graph(&d);
        assert_eq!(g.title.as_deref(), Some("My Diagram"));
        assert!(g.node("A").is_some());
        assert!(g.warnings.is_empty());
    }

    #[test]
    fn unknown_input_warns_but_returns_a_graph() {
        let d = parse("pie\n\"a\": 1\n\"b\": 2");
        let g = as_graph(&d);
        assert_eq!(g.kind_hint, Some(DiagramKind::Flowchart));
        assert!(g
            .warnings
            .iter()
            .any(|w| w == "unsupported PlantUML diagram type"));
    }

    #[test]
    fn fallback_still_extracts_edges() {
        let d = parse("gantt\nA --> B\ntitle x");
        let g = as_graph(&d);
        assert!(g.edges.iter().any(|e| e.from == "A" && e.to == "B"));
        assert!(g.warnings.iter().any(|w| w.contains("unsupported")));
    }

    // -- robustness --------------------------------------------------------

    #[test]
    fn malformed_and_truncated_inputs_never_panic() {
        const FUZZ: &[&str] = &[
            "@startuml\nparticipant",
            "A ->",
            "class X {",
            "note over A",
            "alt",
            "@startuml\n@enduml",
            "",
            "{",
            "}",
            "[[[",
            ">>>",
            "[[[ ))) ",
            "--",
            "-> ->",
            "participant A as",
            "participant \"",
            "note",
            "note left of",
            "note over A, B, ",
            "state {",
            "state \"desc\" as {",
            "entity",
            "package",
            "A <|",
            "A \"1\" *--",
            "activate",
            "autonumber abc def",
            "class \" \" { <<",
        ];
        for src in FUZZ {
            let _ = parse(src);
        }

        // Deep, unmatched nesting.
        let deep = "alt\n".repeat(200);
        let d = parse(&deep);
        let s = as_seq(&d);
        assert!(matches!(
            s.steps.first(),
            Some(SeqStep::GroupOpen { depth: 0, .. })
        ));

        // A single very long line.
        let long = "A -> B : ".repeat(1500);
        let _ = parse(&long);

        // Unterminated block comment swallows the rest without panicking.
        let _ = parse("A -> B : hi\n/' never closed\nC -> D : x");
    }

    #[test]
    fn multibyte_input_is_not_split() {
        // Emoji / CJK / accented text in labels and ids must survive intact.
        let src = "participant 用户 as \"用户 🚀\"\n用户 -> café : 你好";
        let d = parse(src);
        let s = as_seq(&d);
        assert_eq!(s.participants[0].label, "用户 🚀");
        match messages(&s)[0] {
            SeqStep::Message { label, .. } => assert_eq!(label, "你好"),
            _ => unreachable!(),
        }
    }
    // -- skinparam ----------------------------------------------------------

    #[test]
    fn skinparam_colours_reach_the_ir() {
        let d = parse(
            "@startuml\n\
             skinparam classBackgroundColor #ffcc00\n\
             skinparam classBorderColor #333333\n\
             class A\n\
             class B\n\
             @enduml",
        );
        let g = as_graph(&d);
        assert_eq!(g.nodes.len(), 2);
        for n in &g.nodes {
            assert_eq!(n.fill, parse_color("#ffcc00"), "{}", n.id);
            assert_eq!(n.stroke, parse_color("#333333"), "{}", n.id);
        }
    }

    #[test]
    fn skinparam_block_form_is_understood() {
        let d = parse(
            "@startuml\n\
             skinparam class {\n\
             \x20 BackgroundColor lightblue\n\
             \x20 BorderColor black\n\
             }\n\
             class A\n\
             @enduml",
        );
        let g = as_graph(&d);
        assert_eq!(g.nodes[0].fill, parse_color("lightblue"));
        assert_eq!(g.nodes[0].stroke, parse_color("black"));
    }

    #[test]
    fn skinparam_paints_sequence_participants() {
        let d = parse(
            "@startuml\n\
             skinparam participantBackgroundColor #ffddaa\n\
             A -> B : hi\n\
             @enduml",
        );
        let s = as_seq(&d);
        assert_eq!(s.participants.len(), 2);
        assert!(s.participants.iter().all(|p| p.fill == parse_color("#ffddaa")));
    }

    #[test]
    fn skinparam_paints_packages_and_arrows() {
        let d = parse(
            "@startuml\n\
             skinparam packageBackgroundColor #eef\n\
             skinparam arrowColor #0a0\n\
             package P {\n\
             class A\n\
             }\n\
             A --> B\n\
             @enduml",
        );
        let g = as_graph(&d);
        assert_eq!(g.clusters.len(), 1);
        assert_eq!(g.clusters[0].fill, parse_color("#eef"));
        assert!(g.edges.iter().all(|e| e.color == parse_color("#0a0")));
    }

    #[test]
    fn unknown_skinparam_values_are_ignored() {
        let d = parse("@startuml\nskinparam classBackgroundColor notacolour\nclass A\n@enduml");
        let g = as_graph(&d);
        assert!(g.nodes[0].fill.is_none());
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);
    }
}

