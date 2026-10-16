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

/// Which diagram family a parsed document belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiagramKind {
    Flowchart,
    Sequence,
    Pie,
    Class,
    State,
    Entity,
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
        }
    }
}

/// A `subgraph` / `package` / `namespace` grouping.
#[derive(Clone, Debug)]
pub struct Cluster {
    pub id: String,
    pub label: String,
    pub members: Vec<String>,
    pub parent: Option<usize>,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParticipantKind {
    /// Plain box with a name.
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

#[derive(Clone, Debug)]
pub struct Participant {
    pub id: String,
    pub label: String,
    pub kind: ParticipantKind,
    /// `true` when declared with an explicit `participant` statement.
    pub declared: bool,
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
            self.participants.push(Participant {
                id: id.to_owned(),
                label: id.to_owned(),
                kind: ParticipantKind::Default,
                declared: false,
            });
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

/// Ordered map used by parsers that need insertion-ordered unique keys.
pub type OrderedSet = BTreeMap<String, usize>;
