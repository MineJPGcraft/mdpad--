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
    Gantt,
    Timeline,
    Journey,
    Quadrant,
    XYChart,
    Packet,
    Sankey,
    GitGraph,
    Activity,
    Wbs,
    Block,
    Radar,
    Treemap,
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
            Self::Gantt => "gantt",
            Self::Timeline => "timeline",
            Self::Journey => "journey",
            Self::Quadrant => "quadrant",
            Self::XYChart => "xychart",
            Self::Packet => "packet",
            Self::Sankey => "sankey",
            Self::GitGraph => "gitGraph",
            Self::Activity => "activity",
            Self::Wbs => "wbs",
            Self::Block => "block",
            Self::Radar => "radar",
            Self::Treemap => "treemap",
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
// Gantt charts
// ---------------------------------------------------------------------------

/// One bar of a `gantt` chart.
#[derive(Clone, Debug)]
pub struct Task {
    pub name: String,
    /// Start offset from the chart's day 0, in days.
    pub start: f32,
    /// Duration in days; `0.0` for a milestone.
    pub duration: f32,
    /// Index into [`Gantt::sections`].
    pub section: usize,
    pub milestone: bool,
    pub done: bool,
    pub active: bool,
    pub crit: bool,
}

impl Task {
    pub fn end(&self) -> f32 {
        self.start + self.duration.max(0.0)
    }
}

/// A parsed `gantt` block.
#[derive(Clone, Debug, Default)]
pub struct Gantt {
    pub title: Option<String>,
    pub sections: Vec<String>,
    pub tasks: Vec<Task>,
    /// `axisFormat` pattern; `%m-%d` when the source did not ask for one.
    pub axis_format: String,
    /// Calendar date the chart's day 0 falls on, when absolute dates were given.
    pub origin: Option<(i32, u32, u32)>,
    pub warnings: Vec<String>,
}

impl Gantt {
    /// Day span covering every task, always at least one day wide.
    pub fn span(&self) -> (f32, f32) {
        let start = self
            .tasks
            .iter()
            .map(|t| t.start)
            .fold(f32::INFINITY, f32::min);
        let end = self
            .tasks
            .iter()
            .map(|t| t.end())
            .fold(f32::NEG_INFINITY, f32::max);
        if !start.is_finite() || !end.is_finite() || end <= start {
            (0.0, 1.0)
        } else {
            (start, end)
        }
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`). `None` for an impossible month or day.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> Option<i32> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp as i32 + 2) / 5 + d as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Inverse of [`days_from_civil`], as `(year, month, day)`.
pub fn civil_from_days(days: i32) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Format a day number the way `axisFormat` asks, defaulting to `mm-dd`.
pub fn format_day(days: i32, pattern: &str) -> String {
    let (y, m, d) = civil_from_days(days);
    if pattern.trim().is_empty() {
        return format!("{m:02}-{d:02}");
    }
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&y.to_string()),
            Some('y') => out.push_str(&format!("{:02}", y % 100)),
            Some('m') => out.push_str(&format!("{m:02}")),
            Some('d') => out.push_str(&format!("{d:02}")),
            Some('e') => out.push_str(&d.to_string()),
            Some('b') | Some('B') => out.push_str(month_name(m)),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

fn month_name(m: u32) -> &'static str {
    match m {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    }
}

// ---------------------------------------------------------------------------
// Timelines
// ---------------------------------------------------------------------------

/// One column of a `timeline`: a period and the events it holds.
#[derive(Clone, Debug)]
pub struct Period {
    pub label: String,
    pub events: Vec<String>,
    /// Index into [`Timeline::sections`].
    pub section: usize,
}

/// A parsed `timeline` block.
#[derive(Clone, Debug, Default)]
pub struct Timeline {
    pub title: Option<String>,
    pub sections: Vec<String>,
    pub periods: Vec<Period>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// User journeys
// ---------------------------------------------------------------------------

/// One step of a `journey` diagram.
#[derive(Clone, Debug)]
pub struct JourneyStep {
    pub name: String,
    /// Satisfaction score, 1..=5 in Mermaid.
    pub score: u8,
    pub actors: Vec<String>,
    /// Index into [`Journey::sections`].
    pub section: usize,
}

/// A parsed `journey` block.
#[derive(Clone, Debug, Default)]
pub struct Journey {
    pub title: Option<String>,
    pub sections: Vec<String>,
    pub steps: Vec<JourneyStep>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Quadrant charts
// ---------------------------------------------------------------------------

/// One plotted item, in normalised 0..1 chart coordinates.
#[derive(Clone, Debug)]
pub struct QuadrantPoint {
    pub name: String,
    pub x: f32,
    pub y: f32,
}

/// A parsed `quadrantChart` block.
#[derive(Clone, Debug)]
pub struct Quadrant {
    pub title: Option<String>,
    /// Axis captions: `low --> high` for x and y.
    pub x_low: String,
    pub x_high: String,
    pub y_low: String,
    pub y_high: String,
    /// `quadrant-1` … `quadrant-4`: top-right, top-left, bottom-left,
    /// bottom-right, in Mermaid's order.
    pub quadrant_labels: [String; 4],
    pub points: Vec<QuadrantPoint>,
    pub warnings: Vec<String>,
}

impl Default for Quadrant {
    fn default() -> Self {
        Self {
            title: None,
            x_low: String::new(),
            x_high: String::new(),
            y_low: String::new(),
            y_high: String::new(),
            quadrant_labels: Default::default(),
            points: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// XY charts (`xychart-beta`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XYSeriesKind {
    Bar,
    Line,
}

/// One `bar [...]` or `line [...]` series.
#[derive(Clone, Debug)]
pub struct XYSeries {
    pub kind: XYSeriesKind,
    pub name: String,
    pub values: Vec<f32>,
}

/// A parsed `xychart-beta` block.
#[derive(Clone, Debug, Default)]
pub struct XYChart {
    pub title: Option<String>,
    pub x_label: String,
    /// Category ticks; empty when the x axis is numeric.
    pub x_categories: Vec<String>,
    pub x_min: Option<f32>,
    pub x_max: Option<f32>,
    pub y_label: String,
    pub y_min: Option<f32>,
    pub y_max: Option<f32>,
    pub series: Vec<XYSeries>,
    /// `xychart-beta horizontal` was asked for; drawn vertically with a warning.
    pub horizontal: bool,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Packet diagrams (`packet-beta`)
// ---------------------------------------------------------------------------

/// One labelled bit range.
#[derive(Clone, Debug)]
pub struct PacketField {
    /// First bit, inclusive.
    pub start: u32,
    /// Last bit, inclusive.
    pub end: u32,
    pub label: String,
}

/// A parsed `packet-beta` block.
#[derive(Clone, Debug, Default)]
pub struct Packet {
    pub title: Option<String>,
    pub fields: Vec<PacketField>,
    pub warnings: Vec<String>,
}

impl Packet {
    /// Highest bit used by any field, at least 31 so a single header still reads
    /// as one 32-bit word.
    pub fn max_bit(&self) -> u32 {
        self.fields.iter().map(|f| f.end).max().unwrap_or(31).max(31)
    }
}

// ---------------------------------------------------------------------------
// Sankey diagrams (`sankey-beta`)
// ---------------------------------------------------------------------------

/// One `source,target,value` flow.
#[derive(Clone, Debug)]
pub struct SankeyLink {
    pub source: String,
    pub target: String,
    pub value: f32,
}

/// A parsed `sankey-beta` block.
#[derive(Clone, Debug, Default)]
pub struct Sankey {
    pub title: Option<String>,
    pub links: Vec<SankeyLink>,
    pub warnings: Vec<String>,
}

impl Sankey {
    /// Node names in first-seen order.
    pub fn nodes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for link in &self.links {
            for name in [&link.source, &link.target] {
                if !out.iter().any(|n| n == name) {
                    out.push(name.clone());
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Git graphs
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CommitKind {
    #[default]
    Normal,
    /// `commit type: REVERSE` — a revert.
    Reverse,
    /// `commit type: HIGHLIGHT` — emphasised commit.
    Highlight,
}

/// One commit node.
#[derive(Clone, Debug)]
pub struct GitCommit {
    pub id: String,
    pub kind: CommitKind,
    pub tag: Option<String>,
    /// Index into [`GitGraph::branches`].
    pub branch: usize,
    /// Column the commit is drawn in; one per branch.
    pub lane: usize,
    /// Parent commit indices (0, 1 or 2 for merges).
    pub parents: Vec<usize>,
}

/// A parsed `gitGraph` block.
#[derive(Clone, Debug, Default)]
pub struct GitGraph {
    pub title: Option<String>,
    pub branches: Vec<String>,
    pub commits: Vec<GitCommit>,
    pub warnings: Vec<String>,
}

impl GitGraph {
    /// Deepest lane in use, at least 0.
    pub fn max_lane(&self) -> usize {
        self.commits.iter().map(|c| c.lane).max().unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Activity diagrams (PlantUML)
// ---------------------------------------------------------------------------

/// One branch of an `if`: a condition (empty for `else`) plus its body.
#[derive(Clone, Debug)]
pub struct ActBranch {
    pub label: String,
    pub body: Vec<ActNode>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoopKind {
    /// `while (cond) … endwhile` — tested before the body.
    While,
    /// `repeat … repeat while (cond)` — tested after the body.
    Repeat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminal {
    Start,
    Stop,
}

/// A statement in an activity diagram.
#[derive(Clone, Debug)]
pub enum ActNode {
    Action {
        label: String,
        /// `#colour` prefix on the action.
        fill: Option<Rgba>,
    },
    If {
        branches: Vec<ActBranch>,
    },
    Loop {
        kind: LoopKind,
        test: String,
        yes: String,
        no: String,
        body: Vec<ActNode>,
    },
    Fork {
        lanes: Vec<Vec<ActNode>>,
    },
    Partition {
        label: String,
        body: Vec<ActNode>,
    },
    Terminal(Terminal),
}

/// A parsed PlantUML activity diagram.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    pub title: Option<String>,
    pub nodes: Vec<ActNode>,
    pub warnings: Vec<String>,
}

impl Activity {
    /// `true` when nothing at all was parsed.
    pub fn is_blank(&self) -> bool {
        self.nodes.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Block diagrams (`block-beta`)
// ---------------------------------------------------------------------------

/// One cell of a block grid.
#[derive(Clone, Debug)]
pub enum BlockCell {
    Item { label: String },
    /// `space` / `space:2` — empty columns.
    Space { span: usize },
}

/// A parsed `block-beta` block.
#[derive(Clone, Debug)]
pub struct BlockDiagram {
    pub title: Option<String>,
    /// `columns N`, defaulting to 3 like Mermaid.
    pub columns: usize,
    pub cells: Vec<BlockCell>,
    /// Cell indices joined by an arrow.
    pub edges: Vec<(usize, usize)>,
    pub warnings: Vec<String>,
}

impl Default for BlockDiagram {
    fn default() -> Self {
        Self {
            title: None,
            columns: 3,
            cells: Vec::new(),
            edges: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Radar charts (`radar-beta`)
// ---------------------------------------------------------------------------

/// A parsed `radar-beta` block.
#[derive(Clone, Debug, Default)]
pub struct Radar {
    pub title: Option<String>,
    pub axes: Vec<String>,
    /// One series: a name and one value per axis.
    pub curves: Vec<(String, Vec<f32>)>,
    pub max: f32,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Treemaps (`treemap-beta`)
// ---------------------------------------------------------------------------

/// One rectangle of a treemap: a leaf has a value, a section has children.
#[derive(Clone, Debug)]
pub struct TreeItem {
    pub label: String,
    /// Leaf weight; a section's weight is the sum of its children.
    pub value: f32,
    pub children: Vec<TreeItem>,
}

impl TreeItem {
    pub fn weight(&self) -> f32 {
        if self.children.is_empty() {
            self.value.max(0.0)
        } else {
            self.children.iter().map(|c| c.weight()).sum::<f32>().max(0.0)
        }
    }
}

/// A parsed `treemap-beta` block.
#[derive(Clone, Debug, Default)]
pub struct Treemap {
    pub title: Option<String>,
    pub items: Vec<TreeItem>,
    pub warnings: Vec<String>,
}

/// Total weight of a tree, used for the layout's proportional split.
pub fn tree_weight(items: &[TreeItem]) -> f32 {
    items.iter().map(|i| i.weight()).sum()
}

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Diagram {
    Graph(Graph),
    Sequence(Sequence),
    Pie(Pie),
    Gantt(Gantt),
    Timeline(Timeline),
    Journey(Journey),
    Quadrant(Quadrant),
    XYChart(XYChart),
    Packet(Packet),
    Sankey(Sankey),
    GitGraph(GitGraph),
    Activity(Activity),
    Block(BlockDiagram),
    Radar(Radar),
    Treemap(Treemap),
}

impl Diagram {
    pub fn kind(&self) -> DiagramKind {
        match self {
            Self::Graph(g) => g
                .kind_hint
                .unwrap_or(DiagramKind::Flowchart),
            Self::Sequence(_) => DiagramKind::Sequence,
            Self::Pie(_) => DiagramKind::Pie,
            Self::Gantt(_) => DiagramKind::Gantt,
            Self::Timeline(_) => DiagramKind::Timeline,
            Self::Journey(_) => DiagramKind::Journey,
            Self::Quadrant(_) => DiagramKind::Quadrant,
            Self::XYChart(_) => DiagramKind::XYChart,
            Self::Packet(_) => DiagramKind::Packet,
            Self::Sankey(_) => DiagramKind::Sankey,
            Self::GitGraph(_) => DiagramKind::GitGraph,
            Self::Activity(_) => DiagramKind::Activity,
            Self::Block(_) => DiagramKind::Block,
            Self::Radar(_) => DiagramKind::Radar,
            Self::Treemap(_) => DiagramKind::Treemap,
        }
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            Self::Graph(g) => g.title.as_deref(),
            Self::Sequence(s) => s.title.as_deref(),
            Self::Pie(p) => p.title.as_deref(),
            Self::Gantt(g) => g.title.as_deref(),
            Self::Timeline(t) => t.title.as_deref(),
            Self::Journey(j) => j.title.as_deref(),
            Self::Quadrant(q) => q.title.as_deref(),
            Self::XYChart(c) => c.title.as_deref(),
            Self::Packet(p) => p.title.as_deref(),
            Self::Sankey(k) => k.title.as_deref(),
            Self::GitGraph(g) => g.title.as_deref(),
            Self::Activity(a) => a.title.as_deref(),
            Self::Block(b) => b.title.as_deref(),
            Self::Radar(r) => r.title.as_deref(),
            Self::Treemap(t) => t.title.as_deref(),
        }
    }

    /// Non-fatal problems found while parsing, shown under the diagram.
    pub fn warnings(&self) -> &[String] {
        match self {
            Self::Graph(g) => &g.warnings,
            Self::Sequence(s) => &s.warnings,
            Self::Pie(_) => &[],
            Self::Gantt(g) => &g.warnings,
            Self::Timeline(t) => &t.warnings,
            Self::Journey(j) => &j.warnings,
            Self::Quadrant(q) => &q.warnings,
            Self::XYChart(c) => &c.warnings,
            Self::Packet(p) => &p.warnings,
            Self::Sankey(k) => &k.warnings,
            Self::GitGraph(g) => &g.warnings,
            Self::Activity(a) => &a.warnings,
            Self::Block(b) => &b.warnings,
            Self::Radar(r) => &r.warnings,
            Self::Treemap(t) => &t.warnings,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Graph(g) => g.nodes.is_empty(),
            Self::Sequence(s) => s.participants.is_empty() && s.steps.is_empty(),
            Self::Pie(p) => p.slices.is_empty(),
            Self::Gantt(g) => g.tasks.is_empty(),
            Self::Timeline(t) => t.periods.is_empty(),
            Self::Journey(j) => j.steps.is_empty(),
            Self::Quadrant(q) => q.points.is_empty(),
            Self::XYChart(c) => c.series.iter().all(|s| s.values.is_empty()),
            Self::Packet(p) => p.fields.is_empty(),
            Self::Sankey(k) => k.links.is_empty(),
            Self::GitGraph(g) => g.commits.is_empty(),
            Self::Activity(a) => a.is_blank(),
            Self::Block(b) => b.cells.is_empty(),
            Self::Radar(r) => r.curves.is_empty(),
            Self::Treemap(t) => t.items.is_empty(),
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
            Self::Gantt(g) => format!(
                "gantt · {} 个任务 · {} 个分组",
                g.tasks.len(),
                g.sections.len()
            ),
            Self::Timeline(t) => format!(
                "timeline · {} 个时期 · {} 个事件",
                t.periods.len(),
                t.periods.iter().map(|p| p.events.len()).sum::<usize>()
            ),
            Self::Journey(j) => format!(
                "journey · {} 个步骤 · {} 个分组",
                j.steps.len(),
                j.sections.len()
            ),
            Self::Quadrant(q) => format!("quadrant · {} 个数据点", q.points.len()),
            Self::XYChart(c) => format!(
                "xychart · {} 条序列 · {} 个数据点",
                c.series.len(),
                c.series.iter().map(|s| s.values.len()).sum::<usize>()
            ),
            Self::Packet(p) => format!("packet · {} 个字段", p.fields.len()),
            Self::Sankey(k) => format!(
                "sankey · {} 条流量 · {} 个节点",
                k.links.len(),
                k.nodes().len()
            ),
            Self::GitGraph(g) => format!(
                "gitGraph · {} 个提交 · {} 个分支",
                g.commits.len(),
                g.branches.len()
            ),
            Self::Activity(a) => format!("activity · {} 个顶层节点", a.nodes.len()),
            Self::Block(b) => format!("block · {} 个格子", b.cells.len()),
            Self::Radar(r) => format!("radar · {} 条曲线 · {} 个轴", r.curves.len(), r.axes.len()),
            Self::Treemap(t) => format!("treemap · {} 个顶层区块", t.items.len()),
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
