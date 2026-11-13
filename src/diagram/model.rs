//! Diagram intermediate representation.
//!
//! Both the Mermaid and PlantUML front-ends are pure text -> IR transformers.
//! Everything downstream (layout, painting, the optional external-renderer
//! fallback) only ever sees these types, so adding a third diagram language
//! means writing one more parser and nothing else.
//!
//! All rendering happens locally; the IR deliberately contains no notion of a
//! remote service.

use std::collections::BTreeMap;

use crate::gfx::Rgba;

/// Which diagram family a parsed document belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiagramKind {
    Flowchart,
    Sequence,
    Pie,
    Class,
    State,
    Entity,
    Mindmap,
    Requirement,
    Kanban,
}

impl DiagramKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Flowchart => "flowchart",
            Self::Sequence => "sequence",
            Self::Pie => "pie",
            Self::Class => "class",
            Self::State => "state",
            Self::Entity => "ER",
            Self::Mindmap => "mindmap",
            Self::Requirement => "requirement",
            Self::Kanban => "kanban",
        }
    }
}

/// Flow direction for graph-like diagrams.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Direction {
    /// Top to bottom (`TD` / `TB`).
    #[default]
    TopDown,
    BottomTop,
    /// Left to right (`LR`).
    LeftRight,
    RightLeft,
}

impl Direction {
    pub fn is_horizontal(self) -> bool {
        matches!(self, Self::LeftRight | Self::RightLeft)
    }

    /// Layer axis is `true` when layers stack horizontally.
    pub fn reversed(self) -> bool {
        matches!(self, Self::BottomTop | Self::RightLeft)
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "TD" | "TB" | "TOP-DOWN" | "TOPDOWN" => Some(Self::TopDown),
            "BT" | "BOTTOM-TOP" | "BOTTOMTOP" => Some(Self::BottomTop),
            "LR" | "LEFT-RIGHT" | "LEFTRIGHT" => Some(Self::LeftRight),
            "RL" | "RIGHT-LEFT" | "RIGHTLEFT" => Some(Self::RightLeft),
            _ => None,
        }
    }
}

/// Outline of a graph node.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NodeShape {
    /// `A[text]`
    #[default]
    Rect,
    /// `A(text)`
    Round,
    /// `A([text])`
    Stadium,
    /// `A((text))` — a plain circle, or a doubled ring when `double == true`.
    Circle,
    /// `A{text}`
    Diamond,
    /// `A{{text}}`
    Hexagon,
    /// `A[/text/]`
    Parallelogram,
    /// `A[(text)]` — database cylinder.
    Cylinder,
    /// `A>text]`
    Asymmetric,
    /// Classifier boxes (`class X { ... }`, `state Y { ... }`).
    Classifier,
    /// Sticky note (`note "..." as N1`, PlantUML notes).
    Note,
}

/// Semantic classification of a graph node, used for styling.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NodeStyle {
    #[default]
    Default,
    /// Start node (`[*]` in state diagrams, `start`/`end` shaped nodes).
    Terminal,
    Decision,
    /// Participant / actor in a use-case style diagram.
    Actor,
    /// Interface / abstract classifier.
    Abstract,
    /// Annotation attached to another element.
    Annotation,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    /// Primary label, may span multiple lines (split on `\n`).
    pub label: String,
    /// Optional second compartment, e.g. a class stereotype or a state name.
    pub stereotype: Option<String>,
    pub shape: NodeShape,
    pub style: NodeStyle,
    /// Attributes / methods for classifier boxes.
    pub compartments: Vec<Vec<String>>,
    /// Index into [`Graph::clusters`] when the node belongs to a subgraph.
    pub cluster: Option<usize>,
    /// Explicit colours from `classDef` / `style` / `#colour` suffixes.
    pub fill: Option<Rgba>,
    pub stroke: Option<Rgba>,
    pub text_color: Option<Rgba>,
    /// `stroke-width:2px`, in points.
    pub stroke_width: Option<f32>,
}

impl Node {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            stereotype: None,
            shape: NodeShape::Rect,
            style: NodeStyle::Default,
            compartments: Vec::new(),
            cluster: None,
            fill: None,
            stroke: None,
            text_color: None,
            stroke_width: None,
        }
    }

    /// Label split into display lines.
    pub fn label_lines(&self) -> Vec<String> {
        self.label
            .split('\n')
            .map(|s| s.trim().to_owned())
            .collect::<Vec<_>>()
    }

    pub fn total_label_line_count(&self) -> usize {
        let own = self.label_lines().len().max(1);
        own + self.compartments.iter().map(|c| c.len() + 1).sum::<usize>()
    }
}

/// Arrow head / tail decoration.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Arrow {
    #[default]
    None,
    /// Filled triangle arrow.
    Filled,
    /// Open (V shaped) arrow.
    Open,
    /// Hollow triangle, as used for inheritance.
    Hollow,
    /// Diamond, composition.
    Diamond,
    /// Filled diamond.
    FilledDiamond,
    Cross,
    Circle,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LineStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub head: Arrow,
    pub tail: Arrow,
    pub line: LineStyle,
    /// Extra margin for edge labels, in points.
    pub label_pad: f32,
    /// `linkStyle ... stroke:#f00`.
    pub color: Option<Rgba>,
    /// `linkStyle ... stroke-width:3px`, in points.
    pub width: Option<f32>,
    /// `A ~~~ B`: still shapes the layout, but is not drawn.
    pub hidden: bool,
}

impl Edge {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            label: None,
            head: Arrow::Filled,
            tail: Arrow::None,
            line: LineStyle::Solid,
            label_pad: 0.0,
            color: None,
            width: None,
            hidden: false,
        }
    }
}

/// A `subgraph` / `package` / `namespace` grouping.
#[derive(Clone, Debug, Default)]
pub struct Cluster {
    pub id: String,
    pub label: String,
    pub members: Vec<String>,
    pub parent: Option<usize>,
    /// `style subgraphId fill:#f9f`.
    pub fill: Option<Rgba>,
    pub stroke: Option<Rgba>,
    pub text_color: Option<Rgba>,
}

impl Cluster {
    pub fn new(id: impl Into<String>, label: impl Into<String>, parent: Option<usize>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            members: Vec::new(),
            parent,
            fill: None,
            stroke: None,
            text_color: None,
        }
    }
}

/// Flowchart / class / state / ER documents.
#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub kind_hint: Option<DiagramKind>,
    pub direction: Direction,
    pub title: Option<String>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub clusters: Vec<Cluster>,
    /// Parser notes surfaced in the UI when a construct is not understood.
    pub warnings: Vec<String>,
}

impl Graph {
    /// Insert or fetch a node, preserving first-seen order.
    pub fn node_mut(&mut self, id: &str) -> &mut Node {
        if let Some(idx) = self.nodes.iter().position(|n| n.id == id) {
            &mut self.nodes[idx]
        } else {
            self.nodes.push(Node::new(id, id));
            self.nodes.last_mut().expect("just pushed")
        }
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }
}

// ---------------------------------------------------------------------------
// Sequence diagrams
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ParticipantKind {
    /// Plain box with a name.
    #[default]
    Default,
    /// Stick-figure `actor`.
    Actor,
    /// Database / queue / boundary store.
    Store,
    /// `boundary` / `control` / `entity` analysis stereotypes.
    Boundary,
    Control,
    Entity,
    Collections,
    Queue,
}

#[derive(Clone, Debug, Default)]
pub struct Participant {
    pub id: String,
    pub label: String,
    pub kind: ParticipantKind,
    /// `true` when declared with an explicit `participant` statement.
    pub declared: bool,
    /// Explicit colours (`participant A #red`, `skinparam`).
    pub fill: Option<Rgba>,
    pub stroke: Option<Rgba>,
}

impl Participant {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            label: label.into(),
            id,
            kind: ParticipantKind::Default,
            declared: false,
            fill: None,
            stroke: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotePosition {
    Left,
    Right,
    Over,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupKind {
    Alt,
    Opt,
    Loop,
    Par,
    Break,
    Critical,
    Rect,
    Group,
}

#[derive(Clone, Debug)]
pub enum SeqStep {
    Message {
        index: u32,
        from: usize,
        to: usize,
        label: String,
        line: LineStyle,
        head: Arrow,
        /// Activation bars opened / closed at the destination by this message.
        activate: bool,
        deactivate: bool,
        /// `true` when the message is a self-call (from == to).
        self_call: bool,
    },
    Note {
        over: Vec<usize>,
        position: NotePosition,
        text: String,
    },
    Divider {
        label: String,
    },
    GroupOpen {
        kind: GroupKind,
        label: String,
        depth: usize,
    },
    GroupElse {
        label: String,
        depth: usize,
    },
    GroupClose {
        depth: usize,
    },
    Activate {
        participant: usize,
        on: bool,
    },
    Delay {
        text: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Sequence {
    pub title: Option<String>,
    pub participants: Vec<Participant>,
    pub steps: Vec<SeqStep>,
    pub warnings: Vec<String>,
}

impl Sequence {
    pub fn participant_mut(&mut self, id: &str) -> &mut Participant {
        if let Some(idx) = self.participants.iter().position(|p| p.id == id) {
            &mut self.participants[idx]
        } else {
            self.participants.push(Participant::new(id, id));
            self.participants.last_mut().expect("just pushed")
        }
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.participants.iter().position(|p| p.id == id)
    }
}

// ---------------------------------------------------------------------------
// Pie charts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Slice {
    pub label: String,
    pub value: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Pie {
    pub title: Option<String>,
    pub show_data: bool,
    pub slices: Vec<Slice>,
}

impl Pie {
    pub fn total(&self) -> f32 {
        self.slices.iter().map(|s| s.value).sum()
    }
}

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Diagram {
    Graph(Graph),
    Sequence(Sequence),
    Pie(Pie),
}

impl Diagram {
    pub fn kind(&self) -> DiagramKind {
        match self {
            Self::Graph(g) => g
                .kind_hint
                .unwrap_or(DiagramKind::Flowchart),
            Self::Sequence(_) => DiagramKind::Sequence,
            Self::Pie(_) => DiagramKind::Pie,
        }
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            Self::Graph(g) => g.title.as_deref(),
            Self::Sequence(s) => s.title.as_deref(),
            Self::Pie(p) => p.title.as_deref(),
        }
    }

    /// Non-fatal problems found while parsing, shown under the diagram.
    pub fn warnings(&self) -> &[String] {
        match self {
            Self::Graph(g) => &g.warnings,
            Self::Sequence(s) => &s.warnings,
            Self::Pie(_) => &[],
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Graph(g) => g.nodes.is_empty(),
            Self::Sequence(s) => s.participants.is_empty() && s.steps.is_empty(),
            Self::Pie(p) => p.slices.is_empty(),
        }
    }

    /// Short human-readable summary, e.g. `flowchart · 7 nodes · 9 edges`.
    pub fn summary(&self) -> String {
        match self {
            Self::Graph(g) => format!(
                "{} · {} 个节点 · {} 条连线",
                g.kind_hint.unwrap_or(DiagramKind::Flowchart).label(),
                g.nodes.len(),
                g.edges.len()
            ),
            Self::Sequence(s) => format!(
                "sequence · {} 个参与者 · {} 步",
                s.participants.len(),
                s.steps.len()
            ),
            Self::Pie(p) => format!("pie · {} 个扇区", p.slices.len()),
        }
    }
}

/// Deterministic per-series palette shared by pie charts and cluster fills.
pub const SERIES_PALETTE: [u32; 10] = [
    0x4C8DFF, 0x34C759, 0xFF9F0A, 0xFF375F, 0xBF5AF2, 0x64D2FF, 0xFFD60A, 0x30D158, 0xFF6482,
    0x5E5CE6,
];

pub fn series_color(index: usize) -> crate::gfx::Rgba {
    crate::gfx::Rgba::hex(SERIES_PALETTE[index % SERIES_PALETTE.len()])
}

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// Parse a colour literal written in a diagram source.
///
/// Both front-ends share this: Mermaid writes CSS-ish values (`fill:#f9f`,
/// `stroke:rgb(20,30,40)`), PlantUML writes `#lightblue` or a bare colour name.
/// Anything unrecognised returns `None` so the theme default stays in charge.
pub fn parse_color(raw: &str) -> Option<Rgba> {
    let s = raw.trim().trim_matches('"').trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if lower == "transparent" {
        return Some(Rgba::TRANSPARENT);
    }
    if let Some(hex) = s.strip_prefix('#') {
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
            4 => Some(Rgba::new(
                digits[0] * 17,
                digits[1] * 17,
                digits[2] * 17,
                digits[3] * 17,
            )),
            6 => Some(Rgba::new(
                digits[0] * 16 + digits[1],
                digits[2] * 16 + digits[3],
                digits[4] * 16 + digits[5],
                255,
            )),
            8 => Some(Rgba::new(
                digits[0] * 16 + digits[1],
                digits[2] * 16 + digits[3],
                digits[4] * 16 + digits[5],
                digits[6] * 16 + digits[7],
            )),
            _ => None,
        };
    }
    for prefix in ["rgba(", "rgb("] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let body = rest.trim_end_matches(')');
            let parts: Vec<f32> = body
                .split(',')
                .filter_map(|p| p.trim().parse::<f32>().ok())
                .collect();
            if parts.len() >= 3 {
                let ch = |v: f32| v.clamp(0.0, 255.0).round() as u8;
                let a = parts
                    .get(3)
                    .map(|v| {
                        // `rgba(0,0,0,0.5)` writes alpha as a fraction.
                        let v = if *v <= 1.0 { *v * 255.0 } else { *v };
                        ch(v)
                    })
                    .unwrap_or(255);
                return Some(Rgba::new(ch(parts[0]), ch(parts[1]), ch(parts[2]), a));
            }
        }
    }
    let rgb = match lower.as_str() {
        "black" => 0x000000,
        "white" => 0xFFFF_FF,
        "red" => 0xFF_0000,
        "green" => 0x00_8000,
        "lime" => 0x00_FF00,
        "blue" => 0x00_00FF,
        "yellow" => 0xFF_FF00,
        "orange" => 0xFF_A5_00,
        "purple" => 0x80_0080,
        "violet" => 0xEE_82EE,
        "indigo" => 0x4B_0082,
        "brown" => 0xA5_2A2A,
        "pink" => 0xFF_C0CB,
        "cyan" | "aqua" => 0x00_FFFF,
        "magenta" | "fuchsia" => 0xFF_00FF,
        "olive" => 0x80_8000,
        "navy" => 0x00_0080,
        "teal" => 0x00_8080,
        "silver" => 0xC0_C0C0,
        "maroon" => 0x80_0000,
        "gray" | "grey" => 0x80_8080,
        "lightgray" | "lightgrey" => 0xD3_D3D3,
        "darkgray" | "darkgrey" => 0xA9_A9A9,
        "lightblue" => 0xAD_D8E6,
        "darkblue" => 0x00_008B,
        "lightgreen" => 0x90_EE90,
        "darkgreen" => 0x00_6400,
        "lightyellow" => 0xFF_FFE0,
        "lightcyan" => 0xE0_FFFF,
        "lightpink" => 0xFF_B6C1,
        "lightsalmon" => 0xFF_A07A,
        "lightsteelblue" => 0xB0_C4DE,
        "lightseagreen" => 0x20_B2AA,
        "lightcoral" => 0xF0_8080,
        "skyblue" => 0x87_CEEB,
        "steelblue" => 0x46_82B4,
        "royalblue" => 0x41_69E1,
        "dodgerblue" => 0x1E_90FF,
        "cornflowerblue" => 0x64_95ED,
        "palegreen" => 0x98_FB98,
        "paleturquoise" => 0xAF_EEEE,
        "powderblue" => 0xB0_E0E6,
        "gold" => 0xFF_D700,
        "goldenrod" => 0xDA_A520,
        "coral" => 0xFF_7F50,
        "salmon" => 0xFA_8072,
        "tomato" => 0xFF_6347,
        "khaki" => 0xF0_E68C,
        "wheat" => 0xF5_DEB3,
        "beige" => 0xF5_F5DC,
        "ivory" => 0xFF_FFF0,
        "lavender" => 0xE6_E6FA,
        "plum" => 0xDD_A0DD,
        "orchid" => 0xDA_70D6,
        "thistle" => 0xD8_BFD8,
        "turquoise" => 0x40_E0D0,
        "tan" => 0xD2_B48C,
        "crimson" => 0xDC_143C,
        "firebrick" => 0xB2_2222,
        "darkred" => 0x8B_0000,
        "darkorange" => 0xFF_8C00,
        "darkkhaki" => 0xBD_B76B,
        "darkseagreen" => 0x8F_BC8F,
        "midnightblue" => 0x19_1970,
        "slategray" | "slategrey" => 0x70_8090,
        "seagreen" => 0x2E_8B57,
        "forestgreen" => 0x22_8B22,
        "yellowgreen" => 0x9A_CD32,
        "chartreuse" => 0x7F_FF00,
        "springgreen" => 0x00_FF7F,
        "azure" => 0xF0_FFFF,
        "aliceblue" => 0xF0_F8FF,
        "ghostwhite" => 0xF8_F8FF,
        "snow" => 0xFF_FAFA,
        "linen" => 0xFA_F0E6,
        "bisque" => 0xFF_E4C4,
        "moccasin" => 0xFF_E4B5,
        "peachpuff" => 0xFF_DAB9,
        "mistyrose" => 0xFF_E4E1,
        "honeydew" => 0xF0_FFF0,
        "mintcream" => 0xF5_FFFA,
        "oldlace" => 0xFD_F5E6,
        _ => return None,
    };
    Some(Rgba::hex(rgb))
}

/// Ordered map used by parsers that need insertion-ordered unique keys.
pub type OrderedSet = BTreeMap<String, usize>;
