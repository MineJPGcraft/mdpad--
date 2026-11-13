//! Layout of the diagram IR into final, positioned drawing primitives.
//!
//! The three diagram families are laid out by three independent paths that
//! share one small toolbox (text metrics, empty-diagram fallback, the final
//! "normalise" pass). Everything is measured against the injected [`Measure`],
//! so the same code runs against the live egui font atlas and against
//! [`crate::gfx::ApproxMeasure`] in tests.
//!
//! # Coordinate model
//!
//! Layout works in an abstract, unbounded space; nothing is clamped while
//! elements are placed. [`finish`] then measures the union bounding box of all
//! primitives, shifts everything so the top-left sits at `PADDING`, clamps the
//! canvas to at least `40 x 24` and finally emits the optional background rect.
//! Because the shift is derived from the real geometry, every emitted
//! coordinate is finite and non-negative for *every* input.
//!
//! # Never panics
//!
//! There is no `unsafe`, no `unwrap`/`expect` on input-derived data, no
//! division that is not guarded against a zero total, and no indexing that is
//! not bounds-checked. Degenerate IRs render as a small "空图表" card instead.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::f32::consts::PI;

use super::{draw, DiagramTheme};
use crate::diagram::model::{
    series_color, Arrow, Diagram, Direction, Edge, Graph, GroupKind, LineStyle, Node, NodeShape,
    NodeStyle, NotePosition, Participant, ParticipantKind, Pie, SeqStep, Sequence,
};
use crate::gfx::{Anchor, Font, Measure, Prim, Rect, Rgba, Slant, Stroke, Weight};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Padding between the content bounding box and the canvas edge.
const PADDING: f32 = 12.0;
const MIN_CANVAS: [f32; 2] = [40.0, 24.0];
/// Longest a single node label line is allowed to be before it wraps.
const LABEL_WRAP: f32 = 180.0;
/// Gap between two disconnected components stacked in the layer axis.
const COMPONENT_GAP: f32 = 40.0;

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Result of laying out one diagram: primitives whose top-left corner is (0,0),
/// plus the overall canvas size and any diagnostics.
#[derive(Clone, Debug, Default)]
pub struct DiagramRender {
    pub prims: Vec<crate::gfx::Prim>,
    pub size: [f32; 2],
    /// `Some` when the source could not be laid out at all; `prims` then holds a
    /// readable fallback (e.g. the raw source in a bordered box).
    pub error: Option<String>,
    /// Non-fatal problems merged from the parser warnings + layout notes.
    pub warnings: Vec<String>,
    /// Human readable summary for the status bar, e.g. "flowchart · 7 节点 · 9 连线".
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Lay out a parsed diagram. Never panics.
pub fn layout(
    diagram: &Diagram,
    theme: &DiagramTheme,
    base_size: f32,
    m: &dyn Measure,
) -> DiagramRender {
    let ctx = Ctx {
        m,
        theme,
        base: effective_base(theme, base_size),
    };
    if diagram.is_empty() {
        return empty_render(&ctx, diagram.summary(), diagram.warnings().to_vec());
    }
    match diagram {
        Diagram::Graph(g) => layout_graph(g, diagram, &ctx),
        Diagram::Sequence(s) => layout_sequence(s, diagram, &ctx),
        Diagram::Pie(p) => layout_pie(p, diagram, &ctx),
    }
}

/// Body font size: `theme.title_size` when usable, else the host's `base_size`,
/// else 14 pt, always multiplied by `font_scale`.
fn effective_base(theme: &DiagramTheme, base_size: f32) -> f32 {
    let raw = if theme.title_size.is_finite() && theme.title_size > 0.0 {
        theme.title_size
    } else if base_size.is_finite() && base_size > 0.0 {
        base_size
    } else {
        14.0
    };
    let scale = if theme.font_scale.is_finite() {
        theme.font_scale.max(0.05)
    } else {
        1.0
    };
    (raw.max(4.0) * scale).min(400.0)
}

// ---------------------------------------------------------------------------
// Shared context
// ---------------------------------------------------------------------------

/// Font metrics + theme, threaded through every layout path.
struct Ctx<'a> {
    m: &'a dyn Measure,
    theme: &'a DiagramTheme,
    base: f32,
}

impl Ctx<'_> {
    fn font(&self, size: f32, weight: Weight, slant: Slant) -> Font {
        Font {
            size: draw::fin(size, self.base).max(0.5),
            family: self.theme.font_family,
            weight,
            slant,
        }
    }

    /// Node / participant labels.
    fn body(&self) -> Font {
        self.font(self.base, Weight::Regular, Slant::Upright)
    }

    fn body_bold(&self) -> Font {
        self.font(self.base, Weight::Bold, Slant::Upright)
    }

    /// Diagram title.
    fn title(&self) -> Font {
        self.font(self.base * 1.25, Weight::Bold, Slant::Upright)
    }

    /// Edge labels, notes, legends, cluster captions.
    fn small(&self) -> Font {
        self.font(self.base * 0.9, Weight::Regular, Slant::Upright)
    }

    fn small_bold(&self) -> Font {
        self.font(self.base * 0.9, Weight::Bold, Slant::Upright)
    }

    fn small_italic(&self) -> Font {
        self.font(self.base * 0.9, Weight::Regular, Slant::Italic)
    }

    fn lh(&self, f: Font) -> f32 {
        draw::fin(self.m.line_height(f), f.size).max(0.0)
    }

    fn w(&self, text: &str, f: Font) -> f32 {
        draw::fin(self.m.measure(text, f)[0], 0.0).max(0.0)
    }

    fn h(&self, text: &str, f: Font) -> f32 {
        draw::fin(self.m.measure(text, f)[1], 0.0).max(0.0)
    }
}

// ---------------------------------------------------------------------------
// Shared finishing steps
// ---------------------------------------------------------------------------

/// Normalise a primitive list: sanitise, measure, shift to the padding origin,
/// then prepend the optional background rect.
fn finish(
    prims: Vec<Prim>,
    ctx: &Ctx,
    error: Option<String>,
    warnings: Vec<String>,
    summary: String,
) -> DiagramRender {
    let mut prims = prims;
    draw::sanitize(&mut prims);

    let mut size = MIN_CANVAS;
    if let Some(b) = draw::bounds(&prims, ctx.m) {
        draw::translate(&mut prims, [PADDING - b.min[0], PADDING - b.min[1]]);
        size = [
            (b.width() + PADDING * 2.0).max(MIN_CANVAS[0]),
            (b.height() + PADDING * 2.0).max(MIN_CANVAS[1]),
        ];
    }
    if !size[0].is_finite() || !size[1].is_finite() {
        size = MIN_CANVAS;
    }

    if !ctx.theme.transparent {
        let bg = Prim::filled_rect(
            Rect::from_min_size([0.0, 0.0], size),
            0.0,
            ctx.theme.background,
        );
        prims.insert(0, bg);
    }

    DiagramRender {
        prims,
        size,
        error,
        warnings,
        summary,
    }
}

/// Friendly card used whenever the IR carries nothing to draw.
fn empty_render(ctx: &Ctx, summary: String, warnings: Vec<String>) -> DiagramRender {
    let head = ctx.body_bold();
    let sub = ctx.small();
    let msg = "空图表";
    let head_h = ctx.lh(head);
    let sub_h = ctx.lh(sub);
    let width = ctx.w(msg, head).max(ctx.w(&summary, sub)).max(64.0);

    let text_w = width * 0.5;
    let box_rect = Rect::new(
        -text_w - 16.0,
        -10.0,
        text_w + 16.0,
        head_h + 6.0 + sub_h + 10.0,
    );

    let mut prims = Vec::new();
    if !ctx.theme.transparent {
        prims.push(Prim::filled_rect(box_rect, 6.0, ctx.theme.note_fill));
    }
    prims.push(Prim::stroked_rect(
        box_rect,
        6.0,
        draw::stroke(1.0, ctx.theme.node_stroke.fade(0.6)),
    ));
    prims.push(Prim::anchored_text(
        [0.0, -10.0 + head_h * 0.5 + 6.0],
        msg,
        head,
        ctx.theme.node_text,
        Anchor::Center,
    ));
    prims.push(Prim::anchored_text(
        [0.0, -10.0 + head_h + 6.0 + sub_h * 0.5 + 3.0],
        summary.clone(),
        sub,
        ctx.theme.node_text.fade(0.75),
        Anchor::Center,
    ));
    finish(prims, ctx, None, warnings, summary)
}

/// Append `msg` unless an identical note is already present.
fn push_warning(warnings: &mut Vec<String>, msg: String) {
    if !warnings.iter().any(|w| w == &msg) {
        warnings.push(msg);
    }
}

/// Pick a label colour that stays readable on `fill`.
///
/// A `classDef` / `skinparam` the user wrote can turn a node dark; keeping the
/// theme's (also dark) text would render it invisible, so the preferred colour
/// is only used when it contrasts enough with the fill.
fn readable_on(fill: Rgba, preferred: Rgba) -> Rgba {
    let lf = fill.luminance();
    if (preferred.luminance() - lf).abs() >= 0.35 {
        return preferred;
    }
    if lf > 0.5 { Rgba::BLACK } else { Rgba::WHITE }
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// Split an IR label on its explicit `\n` breaks, trimming each line.
fn split_lines(text: &str) -> Vec<String> {
    text.split('\n').map(|s| s.trim().to_owned()).collect()
}

/// Split into display lines and wrap anything longer than `max_w`.
fn wrap_lines(lines: &[String], font: Font, max_w: f32, ctx: &Ctx) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let wrapped = ctx.m.wrap(line, font, max_w);
        if wrapped.is_empty() {
            out.push(line.clone());
        } else {
            for (text, _) in wrapped {
                out.push(text);
            }
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn text_size(lines: &[String], font: Font, ctx: &Ctx) -> [f32; 2] {
    let mut w = 0.0f32;
    for line in lines {
        w = w.max(ctx.w(line, font));
    }
    [w, ctx.lh(font) * lines.len().max(1) as f32]
}

/// Emit one centred [`Prim::Text`] per line, the block starting at `top`.
fn draw_centered(
    out: &mut Vec<Prim>,
    lines: &[String],
    font: Font,
    color: Rgba,
    center_x: f32,
    top: f32,
    ctx: &Ctx,
) {
    let lh = ctx.lh(font);
    for (i, line) in lines.iter().enumerate() {
        out.push(Prim::anchored_text(
            [center_x, top + lh * (i as f32 + 0.5)],
            line.clone(),
            font,
            color,
            Anchor::Center,
        ));
    }
}

/// Upper-case the first letter of every word, leaving the rest untouched.
fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_word_start = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            at_word_start = true;
            out.push(ch);
        } else if at_word_start {
            out.extend(ch.to_uppercase());
            at_word_start = false;
        } else {
            out.push(ch);
        }
    }
    out
}

// ===========================================================================
// Graph diagrams: flowchart, class, state, ER
// ===========================================================================

fn layout_graph(g: &Graph, diagram: &Diagram, ctx: &Ctx) -> DiagramRender {
    let summary = diagram.summary();
    let mut warnings: Vec<String> = diagram.warnings().to_vec();
    if g.nodes.is_empty() {
        return empty_render(ctx, summary, warnings);
    }

    // id -> node index, first occurrence wins (the parser keeps ids unique).
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(g.nodes.len());
    for (i, node) in g.nodes.iter().enumerate() {
        index.entry(node.id.as_str()).or_insert(i);
    }

    // Only edges whose endpoints exist take part in layout.
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for e in &g.edges {
        match (index.get(e.from.as_str()), index.get(e.to.as_str())) {
            (Some(&a), Some(&b)) => edges.push((a, b)),
            _ => push_warning(
                &mut warnings,
                format!("连线引用了未知节点：{} → {}", e.from, e.to),
            ),
        }
    }

    let sizes: Vec<[f32; 2]> = g.nodes.iter().map(|n| node_size(n, ctx)).collect();
    let label_room = g
        .edges
        .iter()
        .filter_map(|e| e.label.as_deref())
        .map(|l| ctx.h(l, ctx.small()) + 6.0)
        .fold(0.0f32, f32::max);
    let positions = place_nodes(g, &edges, &sizes, 48.0 + label_room, 32.0);

    let mut prims: Vec<Prim> = Vec::new();

    // 1. clusters (behind everything else)
    prims.extend(cluster_prims(g, &positions, &index, ctx));

    // 2. edges
    for e in &g.edges {
        let (Some(&a), Some(&b)) = (index.get(e.from.as_str()), index.get(e.to.as_str())) else {
            continue;
        };
        let (Some(from), Some(to)) = (positions.get(a).copied(), positions.get(b).copied()) else {
            continue;
        };
        if a == b || from.center() == to.center() {
            prims.extend(self_loop(from, e, ctx));
        } else {
            prims.extend(edge_prims(e, from, to, g.direction, ctx));
        }
    }

    // 3. nodes, then 4. notes (drawn on top of the nodes they annotate)
    for (i, node) in g.nodes.iter().enumerate() {
        if node.shape == NodeShape::Note {
            continue;
        }
        if let Some(r) = positions.get(i).copied() {
            prims.extend(node_prims(node, r, ctx));
        }
    }
    for (i, node) in g.nodes.iter().enumerate() {
        if node.shape != NodeShape::Note {
            continue;
        }
        if let Some(r) = positions.get(i).copied() {
            prims.extend(node_prims(node, r, ctx));
        }
    }

    // 5. title, centred above the content
    if let Some(title) = g.title.as_deref()
        && !title.trim().is_empty()
    {
        let font = ctx.title();
        let lh = ctx.lh(font);
        let bbox = positions
            .iter()
            .fold(None::<Rect>, |acc, r| Some(match acc {
                Some(b) => b.union(r),
                None => *r,
            }));
        let cx = bbox.map(|b| b.center()[0]).unwrap_or(0.0);
        let top = bbox.map(|b| b.min[1]).unwrap_or(0.0) - 12.0 - lh;
        prims.push(Prim::anchored_text(
            [cx, top + lh * 0.5],
            title,
            font,
            ctx.theme.node_text,
            Anchor::Center,
        ));
    }

    finish(prims, ctx, None, warnings, summary)
}

// ---------------------------------------------------------------------------
// Layered placement
// ---------------------------------------------------------------------------

/// Undirected connected components, each sorted by node index (parser order).
fn components(n: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a != b && a < n && b < n {
            adj[a].push(b);
            adj[b].push(a);
        }
    }
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    for start in 0..n {
        if seen[start] {
            continue;
        }
        let mut comp = Vec::new();
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(u) = stack.pop() {
            comp.push(u);
            for &v in &adj[u] {
                if !seen[v] {
                    seen[v] = true;
                    stack.push(v);
                }
            }
        }
        comp.sort_unstable();
        out.push(comp);
    }
    out
}

/// Longest-path layer assignment over the DAG obtained by dropping back edges.
///
/// A depth-first pass marks edges that point at a node still on the search
/// stack; those are ignored, the rest are layered with a Kahn sweep, so no
/// cycle can stall the queue.
fn assign_layers(comp: &[usize], edges: &[(usize, usize)]) -> HashMap<usize, usize> {
    let members: BTreeSet<usize> = comp.iter().copied().collect();
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for &m in comp {
        adj.entry(m).or_default();
    }
    for &(a, b) in edges {
        if members.contains(&a) && members.contains(&b) {
            adj.entry(a).or_default().push(b);
        }
    }

    // 0 = white, 1 = grey (on stack), 2 = black.
    let mut color: HashMap<usize, u8> = comp.iter().map(|&m| (m, 0u8)).collect();
    let mut back: BTreeSet<(usize, usize)> = BTreeSet::new();
    for &start in comp {
        if color.get(&start).copied().unwrap_or(0) != 0 {
            continue;
        }
        color.insert(start, 1);
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&(node, idx)) = stack.last() {
            let neighbors = adj.get(&node).map(Vec::as_slice).unwrap_or(&[]);
            if idx < neighbors.len() {
                let next = neighbors[idx];
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                match color.get(&next).copied().unwrap_or(0) {
                    1 => {
                        back.insert((node, next));
                    }
                    0 => {
                        color.insert(next, 1);
                        stack.push((next, 0));
                    }
                    _ => {}
                }
            } else {
                color.insert(node, 2);
                stack.pop();
            }
        }
    }

    let mut fwd: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut indeg: HashMap<usize, usize> = comp.iter().map(|&m| (m, 0usize)).collect();
    for &(a, b) in edges {
        if members.contains(&a) && members.contains(&b) && !back.contains(&(a, b)) {
            fwd.entry(a).or_default().push(b);
            if let Some(d) = indeg.get_mut(&b) {
                *d += 1;
            }
        }
    }

    let mut layer: HashMap<usize, usize> = comp.iter().map(|&m| (m, 0usize)).collect();
    let mut queue: VecDeque<usize> = comp
        .iter()
        .filter(|m| indeg.get(m).copied().unwrap_or(0) == 0)
        .copied()
        .collect();
    while let Some(u) = queue.pop_front() {
        let lu = layer.get(&u).copied().unwrap_or(0);
        for &v in fwd.get(&u).map(Vec::as_slice).unwrap_or(&[]) {
            let candidate = lu.saturating_add(1);
            if let Some(slot) = layer.get_mut(&v) {
                *slot = (*slot).max(candidate);
            }
            if let Some(d) = indeg.get_mut(&v) {
                *d = d.saturating_sub(1);
                if *d == 0 {
                    queue.push_back(v);
                }
            }
        }
    }
    layer
}

/// Map `(layer, order)` onto `(x, y)` for one component.
///
/// Every direction shares this one function: `TopDown`/`LeftRight` place layers
/// forwards, `BottomTop`/`RightLeft` mirror the layer axis afterwards.
fn layout_component(
    comp: &[usize],
    edges: &[(usize, usize)],
    sizes: &[[f32; 2]],
    dir: Direction,
    layer_sep: f32,
    node_sep: f32,
) -> (Vec<(usize, Rect)>, Rect) {
    let layers = assign_layers(comp, edges);
    let mut max_layer = 0usize;
    for &m in comp {
        max_layer = max_layer.max(layers.get(&m).copied().unwrap_or(0));
    }
    let mut by_layer: Vec<Vec<usize>> = vec![Vec::new(); max_layer + 1];
    for &m in comp {
        let l = layers.get(&m).copied().unwrap_or(0);
        if let Some(slot) = by_layer.get_mut(l) {
            slot.push(m);
        }
    }

    let layer_axis = if dir.is_horizontal() { 0usize } else { 1usize };
    let cross_axis = 1 - layer_axis;
    let count = by_layer.len();

    let mut extent = vec![0.0f32; count];
    let mut cross = vec![0.0f32; count];
    for (l, nodes) in by_layer.iter().enumerate() {
        let mut e = 0.0f32;
        let mut c = 0.0f32;
        for (j, &ni) in nodes.iter().enumerate() {
            if let Some(s) = sizes.get(ni) {
                e = e.max(s[layer_axis]);
                if j > 0 {
                    c += node_sep;
                }
                c += s[cross_axis];
            }
        }
        extent[l] = e;
        cross[l] = c;
    }

    let mut layer_pos = vec![0.0f32; count];
    let mut acc = 0.0f32;
    for l in 0..count {
        layer_pos[l] = acc;
        acc += extent[l] + layer_sep;
    }
    let total = (acc - layer_sep).max(0.0);
    let max_cross = cross.iter().copied().fold(0.0f32, f32::max);

    let mut placed: Vec<(usize, Rect)> = Vec::with_capacity(comp.len());
    let mut bbox: Option<Rect> = None;
    for (l, nodes) in by_layer.iter().enumerate() {
        let mut c = (max_cross - cross[l]) * 0.5;
        for &ni in nodes {
            let Some(size) = sizes.get(ni).copied() else {
                continue;
            };
            let la = layer_pos[l];
            let (x, y) = if layer_axis == 1 {
                let y = if dir.reversed() {
                    total - (la + size[1])
                } else {
                    la
                };
                (c, y)
            } else {
                let x = if dir.reversed() {
                    total - (la + size[0])
                } else {
                    la
                };
                (x, c)
            };
            let r = Rect::from_min_size([x, y], size);
            bbox = Some(match bbox {
                Some(b) => b.union(&r),
                None => r,
            });
            placed.push((ni, r));
            c += size[cross_axis] + node_sep;
        }
    }
    (placed, bbox.unwrap_or(Rect::ZERO))
}

/// Final node rectangles: each component is laid out on its own, then the
/// components are stacked along the layer axis so disconnected pieces never
/// overlap.
fn place_nodes(
    g: &Graph,
    edges: &[(usize, usize)],
    sizes: &[[f32; 2]],
    layer_sep: f32,
    node_sep: f32,
) -> Vec<Rect> {
    let n = g.nodes.len();
    let dir = g.direction;
    let mut out = vec![Rect::ZERO; n];
    let vertical = !dir.is_horizontal();
    let axis = if vertical { 1usize } else { 0usize };
    let mut cursor = 0.0f32;

    for comp in components(n, edges) {
        let (placed, bbox) = layout_component(&comp, edges, sizes, dir, layer_sep, node_sep);
        if placed.is_empty() {
            continue;
        }
        let delta = cursor - bbox.min[axis];
        let offset = if vertical {
            [0.0, delta]
        } else {
            [delta, 0.0]
        };
        for (i, r) in placed {
            if let Some(slot) = out.get_mut(i) {
                *slot = r.translated(offset);
            }
        }
        cursor += bbox.size()[axis] + COMPONENT_GAP;
    }
    out
}

// ---------------------------------------------------------------------------
// Node geometry
// ---------------------------------------------------------------------------

fn node_size(node: &Node, ctx: &Ctx) -> [f32; 2] {
    if node.style == NodeStyle::Terminal {
        return [14.0, 14.0];
    }
    if node.shape == NodeShape::Classifier {
        return classifier_size(node, ctx);
    }

    let body = ctx.body();
    let lines = wrap_lines(&node.label_lines(), body, LABEL_WRAP, ctx);
    let ls = text_size(&lines, body, ctx);
    let mut w = ls[0] + 24.0;
    let mut h = ls[1] + 16.0;

    if let Some(st) = node.stereotype.as_deref() {
        let font = ctx.small_italic();
        let sl = wrap_lines(&split_lines(st), font, LABEL_WRAP, ctx);
        let ss = text_size(&sl, font, ctx);
        w = w.max(ss[0] + 24.0);
        h += ss[1] + 4.0;
    }

    match node.shape {
        NodeShape::Circle => {
            let d = w.max(h).max(36.0) * 1.15;
            return [d, d];
        }
        NodeShape::Diamond => return [(w * 1.7).max(60.0), (h * 1.7).max(44.0)],
        NodeShape::Hexagon => w += h * 0.6,
        NodeShape::Parallelogram => w += h * 0.75,
        NodeShape::Asymmetric => w += h * 0.5,
        NodeShape::Cylinder => h += 14.0,
        NodeShape::Round | NodeShape::Stadium => w += 14.0,
        NodeShape::Note => {
            w += 10.0;
            h += 8.0;
        }
        _ => {}
    }
    [w.max(44.0), h.max(28.0)]
}

fn label_area(shape: NodeShape, rect: Rect) -> Rect {
    let w = rect.width();
    let h = rect.height();
    match shape {
        NodeShape::Diamond => Rect::new(
            rect.min[0] + w * 0.24,
            rect.min[1] + h * 0.22,
            rect.max[0] - w * 0.24,
            rect.max[1] - h * 0.22,
        ),
        NodeShape::Hexagon => Rect::new(
            rect.min[0] + h * 0.25,
            rect.min[1] + 4.0,
            rect.max[0] - h * 0.25,
            rect.max[1] - 4.0,
        ),
        NodeShape::Parallelogram => Rect::new(
            rect.min[0] + w * 0.17,
            rect.min[1] + 4.0,
            rect.max[0] - w * 0.17,
            rect.max[1] - 4.0,
        ),
        NodeShape::Asymmetric => Rect::new(
            rect.min[0] + 8.0,
            rect.min[1] + 4.0,
            rect.max[0] - h * 0.35,
            rect.max[1] - 4.0,
        ),
        NodeShape::Circle => Rect::new(
            rect.min[0] + w * 0.16,
            rect.min[1] + h * 0.16,
            rect.max[0] - w * 0.16,
            rect.max[1] - h * 0.16,
        ),
        NodeShape::Note => Rect::new(
            rect.min[0] + 8.0,
            rect.min[1] + 6.0,
            rect.max[0] - 14.0,
            rect.max[1] - 6.0,
        ),
        NodeShape::Cylinder => Rect::new(
            rect.min[0] + 8.0,
            rect.min[1] + 12.0,
            rect.max[0] - 8.0,
            rect.max[1] - 8.0,
        ),
        _ => Rect::new(
            rect.min[0] + 8.0,
            rect.min[1] + 4.0,
            rect.max[0] - 8.0,
            rect.max[1] - 4.0,
        ),
    }
}

/// Outline of `shape`, without its label.
fn shape_prims(shape: NodeShape, rect: Rect, fill: Option<Rgba>, stroke: Option<Stroke>) -> Vec<Prim> {
    let [x0, y0, x1, y1] = [rect.min[0], rect.min[1], rect.max[0], rect.max[1]];
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let (w, h) = (rect.width(), rect.height());
    let mut out = Vec::new();
    match shape {
        NodeShape::Rect => {
            out.extend(draw::round_rect(rect, 6.0, fill, stroke));
        }
        NodeShape::Round => {
            out.extend(draw::round_rect(rect, 8.0f32.min(h * 0.5), fill, stroke));
        }
        NodeShape::Stadium => {
            out.extend(draw::round_rect(rect, h * 0.5, fill, stroke));
        }
        NodeShape::Circle => {
            out.push(Prim::circle([cx, cy], (w.min(h) * 0.5).max(0.0), fill, stroke));
        }
        NodeShape::Diamond => {
            out.extend(draw::path(
                vec![[cx, y0], [x1, cy], [cx, y1], [x0, cy]],
                true,
                fill,
                stroke,
            ));
        }
        NodeShape::Hexagon => {
            let i = (h * 0.5).min(w * 0.3);
            out.extend(draw::path(
                vec![
                    [x0 + i, y0],
                    [x1 - i, y0],
                    [x1, cy],
                    [x1 - i, y1],
                    [x0 + i, y1],
                    [x0, cy],
                ],
                true,
                fill,
                stroke,
            ));
        }
        NodeShape::Parallelogram => {
            let s = (w * 0.18).min(h * 0.9);
            out.extend(draw::path(
                vec![[x0 + s, y0], [x1, y0], [x1 - s, y1], [x0, y1]],
                true,
                fill,
                stroke,
            ));
        }
        NodeShape::Asymmetric => {
            let n = (w * 0.3).min(h * 0.9);
            out.extend(draw::path(
                vec![[x0, y0], [x1 - n, y0], [x1, cy], [x1 - n, y1], [x0, y1]],
                true,
                fill,
                stroke,
            ));
        }
        NodeShape::Cylinder => {
            out.extend(draw::cylinder(rect, 14.0, fill, stroke));
        }
        NodeShape::Note => {
            out.extend(draw::note_shape(rect, 12.0, fill, stroke));
        }
        // Classifier boxes are painted by a dedicated helper; the plain
        // rectangle below is only the fallback if one ever reaches here.
        NodeShape::Classifier => {
            out.extend(draw::round_rect(rect, 4.0, fill, stroke));
        }
    }
    out
}

fn node_prims(node: &Node, rect: Rect, ctx: &Ctx) -> Vec<Prim> {
    if node.style == NodeStyle::Terminal {
        let is_end = node.id.contains("-end");
        return draw::terminal(rect.center(), is_end, ctx.theme.node_stroke);
    }
    if node.shape == NodeShape::Classifier {
        return classifier_prims(node, rect, ctx);
    }

    // A `classDef` / `style` / `#colour` override wins over the theme.
    let fill = node.fill.unwrap_or(ctx.theme.node_fill);
    let stroke_color = node.stroke.unwrap_or(ctx.theme.node_stroke);
    let text_color = node
        .text_color
        .unwrap_or_else(|| readable_on(fill, ctx.theme.node_text));

    let mut out = Vec::new();
    out.extend(shape_prims(
        node.shape,
        rect,
        Some(fill),
        Some(draw::stroke(node.stroke_width.unwrap_or(1.2), stroke_color)),
    ));

    let area = label_area(node.shape, rect);
    if area.is_empty() {
        return out;
    }
    let body = ctx.body();
    let lines = wrap_lines(&node.label_lines(), body, area.width().max(20.0), ctx);
    let small = ctx.small_italic();
    let stereo = node
        .stereotype
        .as_deref()
        .map(|s| wrap_lines(&split_lines(s), small, area.width().max(20.0), ctx))
        .unwrap_or_default();

    let total = if stereo.is_empty() {
        ctx.lh(body) * lines.len().max(1) as f32
    } else {
        ctx.lh(small) * stereo.len().max(1) as f32 + 2.0 + ctx.lh(body) * lines.len().max(1) as f32
    };
    let mut top = area.center()[1] - total * 0.5;
    if !stereo.is_empty() {
        draw_centered(
            &mut out,
            &stereo,
            small,
            text_color.fade(0.8),
            area.center()[0],
            top,
            ctx,
        );
        top += ctx.lh(small) * stereo.len().max(1) as f32 + 2.0;
    }
    draw_centered(&mut out, &lines, body, text_color, area.center()[0], top, ctx);
    out
}

fn classifier_size(node: &Node, ctx: &Ctx) -> [f32; 2] {
    let name_font = ctx.body_bold();
    let comp_font = ctx.small();
    let name = wrap_lines(&node.label_lines(), name_font, LABEL_WRAP, ctx);
    let mut w = text_size(&name, name_font, ctx)[0];
    let mut h = ctx.lh(name_font) * name.len().max(1) as f32;

    if let Some(st) = node.stereotype.as_deref() {
        let sl = split_lines(st);
        w = w.max(text_size(&sl, comp_font, ctx)[0]);
        h += ctx.lh(comp_font) * sl.len().max(1) as f32;
    }
    for comp in &node.compartments {
        if comp.is_empty() {
            continue;
        }
        let lines: Vec<String> = comp.iter().flat_map(|l| split_lines(l)).collect();
        w = w.max(text_size(&lines, comp_font, ctx)[0]);
        h += ctx.lh(comp_font) * lines.len() as f32 + 10.0;
    }
    [(w + 26.0).max(72.0), (h + 18.0).max(40.0)]
}

fn classifier_prims(node: &Node, rect: Rect, ctx: &Ctx) -> Vec<Prim> {
    let fill = node.fill.unwrap_or(ctx.theme.node_fill);
    let text_color = node
        .text_color
        .unwrap_or_else(|| readable_on(fill, ctx.theme.node_text));
    let stroke = draw::stroke(
        node.stroke_width.unwrap_or(1.2),
        node.stroke.unwrap_or(ctx.theme.node_stroke),
    );
    let mut out = Vec::new();
    out.extend(draw::round_rect(rect, 4.0, Some(fill), Some(stroke)));

    let pad_x = 10.0;
    let name_font = ctx.body_bold();
    let comp_font = ctx.small();
    let mut y = rect.min[1] + 8.0;

    if let Some(st) = node.stereotype.as_deref() {
        let lines = split_lines(st);
        draw_centered(
            &mut out,
            &lines,
            comp_font,
            text_color.fade(0.8),
            rect.center()[0],
            y,
            ctx,
        );
        y += ctx.lh(comp_font) * lines.len().max(1) as f32;
    }

    let name = wrap_lines(&node.label_lines(), name_font, rect.width() - pad_x * 2.0, ctx);
    draw_centered(&mut out, &name, name_font, text_color, rect.center()[0], y, ctx);
    y += ctx.lh(name_font) * name.len().max(1) as f32 + 5.0;

    for comp in &node.compartments {
        if comp.is_empty() {
            continue;
        }
        out.push(Prim::line([rect.min[0], y], [rect.max[0], y], stroke));
        y += 6.0;
        let lines: Vec<String> = comp.iter().flat_map(|l| split_lines(l)).collect();
        let lh = ctx.lh(comp_font);
        for line in &lines {
            out.push(Prim::anchored_text(
                [rect.min[0] + pad_x, y + lh * 0.5],
                line.clone(),
                comp_font,
                text_color,
                Anchor::LeftCenter,
            ));
            y += lh;
        }
        y += 4.0;
    }
    out
}

// ---------------------------------------------------------------------------
// Clusters
// ---------------------------------------------------------------------------

/// How deeply a cluster is nested inside other clusters. Cycles are impossible
/// but a self-referential parent is nevertheless guarded against.
fn cluster_depth(g: &Graph, i: usize) -> usize {
    let mut depth = 0usize;
    let mut cur = g.clusters.get(i).and_then(|c| c.parent);
    let mut guard = 0usize;
    while let Some(p) = cur {
        if p == i {
            break;
        }
        depth += 1;
        guard += 1;
        if guard > 64 {
            break;
        }
        cur = g.clusters.get(p).and_then(|c| c.parent);
    }
    depth
}

fn cluster_prims(
    g: &Graph,
    positions: &[Rect],
    index: &HashMap<&str, usize>,
    ctx: &Ctx,
) -> Vec<Prim> {
    if g.clusters.is_empty() {
        return Vec::new();
    }
    // Outer clusters first so nested boxes land on top of their parents.
    let mut order: Vec<(usize, usize)> = (0..g.clusters.len())
        .map(|i| (cluster_depth(g, i), i))
        .collect();
    order.sort_by_key(|(d, i)| (*d, *i));

    let font = ctx.small_bold();
    let label_h = ctx.lh(font);
    let mut out = Vec::new();
    for (_, ci) in order {
        let Some(cluster) = g.clusters.get(ci) else {
            continue;
        };
        let mut bbox: Option<Rect> = None;
        for member in &cluster.members {
            if let Some(&ni) = index.get(member.as_str())
                && let Some(r) = positions.get(ni)
            {
                bbox = Some(match bbox {
                    Some(b) => b.union(r),
                    None => *r,
                });
            }
        }
        let Some(b) = bbox else {
            // A cluster with no laid-out members is simply not drawn.
            continue;
        };

        let mut r = b.expand(14.0);
        r.min[1] -= label_h + 4.0;
        let accent = series_color(ci);
        let fill = cluster
            .fill
            .unwrap_or_else(|| ctx.theme.cluster_fill.lerp(accent, 0.10));
        let stroke_color = cluster
            .stroke
            .unwrap_or_else(|| ctx.theme.cluster_stroke.lerp(accent, 0.35));
        let text_color = cluster
            .text_color
            .unwrap_or_else(|| readable_on(fill, ctx.theme.cluster_text));
        let stroke = draw::stroke(1.2, stroke_color);
        out.extend(draw::round_rect(r, 10.0, Some(fill), Some(stroke)));
        out.push(Prim::anchored_text(
            [r.min[0] + 10.0, r.min[1] + label_h * 0.5 + 4.0],
            cluster.label.clone(),
            font,
            text_color,
            Anchor::LeftCenter,
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Edges
// ---------------------------------------------------------------------------

/// Exit / entry sides as indices into [`Rect::edge_points`]
/// (`0 = top, 1 = right, 2 = bottom, 3 = left`).
fn sides(dir: Direction, from: Rect, to: Rect) -> (usize, usize) {
    let fc = from.center();
    let tc = to.center();
    let eps = 1.0;
    if dir.is_horizontal() {
        if tc[0] > fc[0] + eps {
            (1, 3)
        } else if tc[0] < fc[0] - eps {
            (3, 1)
        } else if tc[1] >= fc[1] {
            (2, 0)
        } else {
            (0, 2)
        }
    } else if tc[1] > fc[1] + eps {
        (2, 0)
    } else if tc[1] < fc[1] - eps {
        (0, 2)
    } else if tc[0] >= fc[0] {
        (1, 3)
    } else {
        (3, 1)
    }
}

fn distinct(p: [f32; 2], q: [f32; 2]) -> bool {
    let dx = p[0] - q[0];
    let dy = p[1] - q[1];
    dx * dx + dy * dy > 1e-4
}

/// Orthogonal three-segment route between two border points.
fn route_points(start: [f32; 2], end: [f32; 2], exit_side: usize, enter_side: usize) -> Vec<[f32; 2]> {
    let vertical_exit = exit_side.is_multiple_of(2);
    let vertical_enter = enter_side.is_multiple_of(2);
    let mut pts = vec![start];
    if vertical_exit && vertical_enter {
        let mid = (start[1] + end[1]) * 0.5;
        pts.push([start[0], mid]);
        pts.push([end[0], mid]);
    } else if !vertical_exit && !vertical_enter {
        let mid = (start[0] + end[0]) * 0.5;
        pts.push([mid, start[1]]);
        pts.push([mid, end[1]]);
    } else if vertical_exit {
        pts.push([start[0], end[1]]);
    } else {
        pts.push([end[0], start[1]]);
    }
    pts.push(end);

    let mut out: Vec<[f32; 2]> = Vec::with_capacity(pts.len());
    for p in pts {
        if let Some(last) = out.last()
            && !distinct(*last, p)
        {
            continue;
        }
        out.push(p);
    }
    out
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt()
}

/// Point halfway along a polyline, measured by arc length.
fn polyline_mid(pts: &[[f32; 2]]) -> [f32; 2] {
    if pts.is_empty() {
        return [0.0, 0.0];
    }
    if pts.len() == 1 {
        return pts[0];
    }
    let mut total = 0.0f32;
    for w in pts.windows(2) {
        total += dist(w[0], w[1]);
    }
    if total <= 0.0 || !total.is_finite() {
        return pts[0];
    }
    let half = total * 0.5;
    let mut acc = 0.0f32;
    for w in pts.windows(2) {
        let d = dist(w[0], w[1]);
        if d > 0.0 && acc + d >= half {
            let t = ((half - acc) / d).clamp(0.0, 1.0);
            return [
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ];
        }
        acc += d;
    }
    pts.last().copied().unwrap_or(pts[0])
}

/// Label plus an opaque pill so the line does not strike through the text.
fn label_pill(text: &str, center: [f32; 2], ctx: &Ctx) -> Vec<Prim> {
    let font = ctx.small();
    let w = ctx.w(text, font);
    let h = ctx.lh(font);
    let rect = Rect::from_center_size(center, [w + 8.0, h + 3.0]);
    vec![
        Prim::rect(rect, 3.0, Some(ctx.theme.background), None),
        Prim::anchored_text(center, text.to_owned(), font, ctx.theme.edge_text, Anchor::Center),
    ]
}

fn edge_prims(e: &Edge, from: Rect, to: Rect, dir: Direction, ctx: &Ctx) -> Vec<Prim> {
    let (exit, enter) = sides(dir, from, to);
    let fpts = from.edge_points();
    let tpts = to.edge_points();
    let start = fpts[exit.min(3)];
    let end = tpts[enter.min(3)];
    let pts = route_points(start, end, exit, enter);
    if pts.len() < 2 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let color = e.color.unwrap_or(ctx.theme.edge);
    let stroke = draw::stroke(e.width.unwrap_or(1.3), color);
    for w in pts.windows(2) {
        out.extend(draw::segment(w[0], w[1], stroke, e.line));
    }

    if let (Some(last), Some(prev)) = (pts.last(), pts.get(pts.len().saturating_sub(2))) {
        out.extend(draw::arrow_head(
            *last,
            [last[0] - prev[0], last[1] - prev[1]],
            e.head,
            color,
            draw::ARROW_SIZE,
        ));
    }
    if e.tail != Arrow::None
        && let (Some(first), Some(next)) = (pts.first(), pts.get(1))
    {
        out.extend(draw::arrow_head(
            *first,
            [first[0] - next[0], first[1] - next[1]],
            e.tail,
            color,
            draw::ARROW_SIZE,
        ));
    }
    if let Some(label) = e.label.as_deref()
        && !label.trim().is_empty()
    {
        out.extend(label_pill(label, polyline_mid(&pts), ctx));
    }
    out
}

/// A small right-angle loop on the right-hand side of a node.
fn self_loop(node: Rect, e: &Edge, ctx: &Ctx) -> Vec<Prim> {
    let w = 22.0;
    let x = node.max[0];
    let y0 = node.center()[1] - 9.0;
    let y1 = node.center()[1] + 9.0;
    let pts = [[x, y0], [x + w, y0], [x + w, y1], [x, y1]];

    let mut out = Vec::new();
    let color = e.color.unwrap_or(ctx.theme.edge);
    let stroke = draw::stroke(e.width.unwrap_or(1.3), color);
    for p in pts.windows(2) {
        out.extend(draw::segment(p[0], p[1], stroke, e.line));
    }
    out.extend(draw::arrow_head(
        [x, y1],
        [-1.0, 0.0],
        e.head,
        color,
        draw::ARROW_SIZE,
    ));
    if let Some(label) = e.label.as_deref()
        && !label.trim().is_empty()
    {
        let font = ctx.small();
        let center = [x + w + 8.0 + ctx.w(label, font) * 0.5, (y0 + y1) * 0.5];
        out.extend(label_pill(label, center, ctx));
    }
    out
}

// ===========================================================================
// Sequence diagrams
// ===========================================================================

/// One message resolved to its final y coordinate.
struct Msg {
    from: usize,
    to: usize,
    y: f32,
    label: String,
    line: LineStyle,
    head: Arrow,
    self_call: bool,
}

/// A resolved note rectangle.
struct NoteBox {
    rect: Rect,
    lines: Vec<String>,
}

/// A resolved alt/opt/loop frame.
struct Frame {
    depth: usize,
    kind: String,
    label: String,
    start: f32,
    end: f32,
    else_y: Option<f32>,
    else_label: Option<String>,
}

fn group_label(kind: GroupKind) -> &'static str {
    match kind {
        GroupKind::Alt => "alt",
        GroupKind::Opt => "opt",
        GroupKind::Loop => "loop",
        GroupKind::Par => "par",
        GroupKind::Break => "break",
        GroupKind::Critical => "critical",
        GroupKind::Rect => "rect",
        GroupKind::Group => "group",
    }
}

fn glyph_height(kind: ParticipantKind) -> f32 {
    match kind {
        ParticipantKind::Actor => 34.0,
        ParticipantKind::Store | ParticipantKind::Collections | ParticipantKind::Queue => 22.0,
        ParticipantKind::Boundary | ParticipantKind::Control | ParticipantKind::Entity => 18.0,
        ParticipantKind::Default => 0.0,
    }
}

fn layout_sequence(seq: &Sequence, diagram: &Diagram, ctx: &Ctx) -> DiagramRender {
    let summary = diagram.summary();
    let warnings: Vec<String> = diagram.warnings().to_vec();
    let n = seq.participants.len();
    if n == 0 {
        return empty_render(ctx, summary, warnings);
    }

    let body = ctx.body();
    let lh = ctx.lh(body);
    let pad = 9.0;

    // --- header row ---------------------------------------------------------
    let mut widths = vec![60.0f32; n];
    for (i, p) in seq.participants.iter().enumerate() {
        let label = title_case(&p.label);
        let w = (ctx.w(&label, body) + pad * 2.0).max(60.0);
        if let Some(slot) = widths.get_mut(i) {
            *slot = w;
        }
    }
    let globe = seq
        .participants
        .iter()
        .map(|p| glyph_height(p.kind))
        .fold(0.0f32, f32::max);
    let header_h = globe + lh + pad * 2.0;

    let gap = 40.0f32;
    let mut cx = vec![0.0f32; n];
    let mut cursor_x = 0.0f32;
    for (i, x) in cx.iter_mut().enumerate() {
        let w = widths.get(i).copied().unwrap_or(60.0);
        *x = cursor_x + w * 0.5;
        cursor_x += w + gap;
    }

    let title_font = ctx.title();
    let title_h = if seq.title.is_some() {
        ctx.lh(title_font) + 10.0
    } else {
        0.0
    };
    let header_top = title_h;
    let header_bottom = header_top + header_h;

    // --- step pass ----------------------------------------------------------
    let mut msgs: Vec<Msg> = Vec::new();
    let mut notes: Vec<NoteBox> = Vec::new();
    let mut dividers: Vec<(f32, String)> = Vec::new();
    let mut delays: Vec<(f32, String)> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut open_frames: Vec<usize> = Vec::new();
    let mut bars: Vec<(usize, f32, f32)> = Vec::new();
    let mut open_bars: Vec<Vec<f32>> = vec![Vec::new(); n];

    let mut cursor = header_bottom + 36.0;
    let note_font = ctx.small();
    let note_lh = ctx.lh(note_font);

    for step in &seq.steps {
        match step {
            SeqStep::Activate { participant, on } => {
                if let Some(stack) = open_bars.get_mut(*participant) {
                    if *on {
                        stack.push(cursor);
                    } else if let Some(top) = stack.pop() {
                        bars.push((*participant, top, cursor));
                    }
                }
            }
            SeqStep::Message {
                from,
                to,
                label,
                line,
                head,
                activate,
                deactivate,
                self_call,
                ..
            } => {
                msgs.push(Msg {
                    from: *from,
                    to: *to,
                    y: cursor,
                    label: label.clone(),
                    line: *line,
                    head: *head,
                    self_call: *self_call || from == to,
                });
                if *activate
                    && let Some(stack) = open_bars.get_mut(*to)
                {
                    stack.push(cursor);
                }
                if *deactivate
                    && let Some(stack) = open_bars.get_mut(*to)
                    && let Some(top) = stack.pop()
                {
                    bars.push((*to, top, cursor));
                }
                cursor += 34.0;
            }
            SeqStep::Note {
                over,
                position,
                text,
            } => {
                let lines = split_lines(text);
                let text_w = lines.iter().map(|l| ctx.w(l, note_font)).fold(0.0f32, f32::max);
                let h = note_lh * lines.len().max(1) as f32 + 14.0;
                let rect = note_rect(over, *position, &cx, &widths, cursor, h, text_w);
                notes.push(NoteBox { rect, lines });
                cursor += h + 12.0;
            }
            SeqStep::Divider { label } => {
                dividers.push((cursor, label.clone()));
                cursor += 24.0;
            }
            SeqStep::Delay { text } => {
                delays.push((cursor, text.clone()));
                cursor += 26.0;
            }
            SeqStep::GroupOpen {
                kind,
                label,
                depth,
            } => {
                frames.push(Frame {
                    depth: *depth,
                    kind: group_label(*kind).to_owned(),
                    label: label.clone(),
                    start: cursor,
                    end: cursor,
                    else_y: None,
                    else_label: None,
                });
                if let Some(idx) = frames.len().checked_sub(1) {
                    open_frames.push(idx);
                }
                cursor += 20.0;
            }
            SeqStep::GroupElse { label, .. } => {
                if let Some(&idx) = open_frames.last()
                    && let Some(frame) = frames.get_mut(idx)
                {
                    frame.else_y = Some(cursor);
                    frame.else_label = Some(label.clone());
                }
                cursor += 10.0;
            }
            SeqStep::GroupClose { .. } => {
                if let Some(idx) = open_frames.pop()
                    && let Some(frame) = frames.get_mut(idx)
                {
                    frame.end = cursor;
                }
                cursor += 6.0;
            }
        }
    }
    // Unbalanced input: close whatever is still open, and every open bar.
    for idx in open_frames {
        if let Some(frame) = frames.get_mut(idx) {
            frame.end = cursor;
        }
    }
    for (p, stack) in open_bars.iter_mut().enumerate() {
        for top in stack.drain(..) {
            bars.push((p, top, cursor));
        }
    }
    let content_bottom = cursor.max(header_bottom + 36.0);

    // --- emit ---------------------------------------------------------------
    let mut prims: Vec<Prim> = Vec::new();

    // lifelines ("edges")
    for i in 0..n {
        let x = cx.get(i).copied().unwrap_or(0.0);
        prims.extend(draw::segment(
            [x, header_bottom],
            [x, content_bottom],
            draw::stroke(1.0, ctx.theme.edge.fade(0.5)),
            LineStyle::Dotted,
        ));
    }
    for m in &msgs {
        prims.extend(message_prims(m, &cx, ctx));
    }
    for (p, top, bot) in &bars {
        let x = cx.get(*p).copied().unwrap_or(0.0);
        let h = (bot - top).max(2.0);
        prims.extend(draw::round_rect(
            Rect::from_min_size([x - 5.0, *top], [10.0, h]),
            2.0,
            Some(ctx.theme.node_fill),
            Some(draw::stroke(1.0, ctx.theme.node_stroke)),
        ));
    }

    // participant headers ("nodes")
    for (i, p) in seq.participants.iter().enumerate() {
        let w = widths.get(i).copied().unwrap_or(60.0);
        let x = cx.get(i).copied().unwrap_or(0.0);
        let rect = Rect::new(x - w * 0.5, header_top, x + w * 0.5, header_bottom);
        prims.extend(participant_prims(p, rect, ctx));
    }

    // notes
    for nb in &notes {
        prims.extend(draw::note_shape(
            nb.rect,
            12.0,
            Some(ctx.theme.note_fill),
            Some(draw::stroke(1.0, ctx.theme.note_stroke)),
        ));
        let inner = Rect::new(
            nb.rect.min[0] + 6.0,
            nb.rect.min[1] + 5.0,
            nb.rect.max[0] - 6.0,
            nb.rect.max[1] - 5.0,
        );
        let lines = wrap_lines(&nb.lines, note_font, inner.width().max(20.0), ctx);
        let total = ctx.lh(note_font) * lines.len().max(1) as f32;
        draw_centered(
            &mut prims,
            &lines,
            note_font,
            ctx.theme.note_text,
            inner.center()[0],
            inner.center()[1] - total * 0.5,
            ctx,
        );
    }

    // group frames
    if n > 0 {
        let min_x = cx.iter().copied().fold(f32::MAX, f32::min);
        let max_x = cx.iter().copied().fold(f32::MIN, f32::max);
        for frame in &frames {
            prims.extend(frame_prims(frame, min_x, max_x, ctx));
        }
    }

    // dividers
    if n > 0 {
        let min_x = cx.iter().copied().fold(f32::MAX, f32::min) - 20.0;
        let max_x = cx.iter().copied().fold(f32::MIN, f32::max) + 20.0;
        for (y, label) in &dividers {
            prims.extend(draw::segment(
                [min_x, *y],
                [max_x, *y],
                draw::stroke(1.0, ctx.theme.edge.fade(0.7)),
                LineStyle::Dashed,
            ));
            prims.extend(label_pill(label, [(min_x + max_x) * 0.5, *y], ctx));
        }
        for (y, text) in &delays {
            let mid = (min_x + max_x) * 0.5;
            for k in -1..=1 {
                prims.push(Prim::circle(
                    [mid + k as f32 * 9.0, *y],
                    1.8,
                    Some(ctx.theme.edge),
                    None,
                ));
            }
            prims.push(Prim::anchored_text(
                [mid, *y + 14.0],
                text.clone(),
                note_font,
                ctx.theme.edge_text,
                Anchor::Center,
            ));
        }
    }

    // title
    if let Some(title) = seq.title.as_deref()
        && !title.trim().is_empty()
    {
        let mid = (cx.first().copied().unwrap_or(0.0) + cx.last().copied().unwrap_or(0.0)) * 0.5;
        prims.push(Prim::anchored_text(
            [mid, title_h - 10.0 + ctx.lh(title_font) * 0.5],
            title,
            title_font,
            ctx.theme.node_text,
            Anchor::Center,
        ));
    }

    finish(prims, ctx, None, warnings, summary)
}

fn note_rect(
    over: &[usize],
    position: NotePosition,
    cx: &[f32],
    widths: &[f32],
    y: f32,
    h: f32,
    text_w: f32,
) -> Rect {
    let valid: Vec<usize> = over.iter().copied().filter(|i| *i < cx.len()).collect();
    match position {
        NotePosition::Over => {
            if valid.is_empty() {
                let w = (text_w + 20.0).max(90.0);
                return Rect::from_min_size([-w * 0.5, y], [w, h]);
            }
            let mut min_x = f32::MAX;
            let mut max_x = f32::MIN;
            for &i in &valid {
                let x = cx.get(i).copied().unwrap_or(0.0);
                let w = widths.get(i).copied().unwrap_or(60.0);
                min_x = min_x.min(x - w * 0.5 - 8.0);
                max_x = max_x.max(x + w * 0.5 + 8.0);
            }
            let w = (max_x - min_x).max(text_w + 20.0).max(90.0);
            let center = (min_x + max_x) * 0.5;
            Rect::from_min_size([center - w * 0.5, y], [w, h])
        }
        NotePosition::Right | NotePosition::Left => {
            let i = valid.first().copied().unwrap_or(0);
            let x = cx.get(i).copied().unwrap_or(0.0);
            let w = (text_w + 20.0).max(90.0);
            if position == NotePosition::Right {
                Rect::from_min_size([x + 26.0, y], [w, h])
            } else {
                Rect::from_min_size([x - 26.0 - w, y], [w, h])
            }
        }
    }
}

fn participant_prims(p: &Participant, rect: Rect, ctx: &Ctx) -> Vec<Prim> {
    let mut out = Vec::new();
    let fill = p.fill.unwrap_or(ctx.theme.node_fill);
    let stroke_color = p.stroke.unwrap_or(ctx.theme.node_stroke);
    let text_color = readable_on(fill, ctx.theme.node_text);
    out.extend(draw::round_rect(
        rect,
        6.0,
        Some(fill),
        Some(draw::stroke(1.2, stroke_color)),
    ));

    let font = ctx.body();
    let lh = ctx.lh(font);
    let pad = 9.0;
    let gh = glyph_height(p.kind);
    if gh > 0.0 {
        out.extend(glyph_prims(
            p.kind,
            rect.center()[0],
            rect.min[1] + pad,
            gh,
            ctx,
        ));
    }
    out.push(Prim::anchored_text(
        [rect.center()[0], rect.min[1] + pad + gh + lh * 0.5],
        title_case(&p.label),
        font,
        text_color,
        Anchor::Center,
    ));
    out
}

fn glyph_prims(kind: ParticipantKind, cx: f32, top: f32, h: f32, ctx: &Ctx) -> Vec<Prim> {
    let color = ctx.theme.node_stroke;
    let stroke = draw::stroke(1.2, color);
    let mut out = Vec::new();
    match kind {
        ParticipantKind::Actor => {
            out.push(Prim::circle([cx, top + h * 0.22], h * 0.18, None, Some(stroke)));
            out.push(Prim::line(
                [cx, top + h * 0.40],
                [cx, top + h * 0.70],
                stroke,
            ));
            out.push(Prim::line(
                [cx - h * 0.24, top + h * 0.50],
                [cx + h * 0.24, top + h * 0.50],
                stroke,
            ));
            out.push(Prim::line(
                [cx, top + h * 0.70],
                [cx - h * 0.20, top + h * 0.98],
                stroke,
            ));
            out.push(Prim::line(
                [cx, top + h * 0.70],
                [cx + h * 0.20, top + h * 0.98],
                stroke,
            ));
        }
        ParticipantKind::Store => {
            out.extend(draw::cylinder(
                Rect::new(cx - h * 0.45, top, cx + h * 0.45, top + h),
                8.0,
                None,
                Some(stroke),
            ));
        }
        ParticipantKind::Collections | ParticipantKind::Queue => {
            let w = h * 0.9;
            for k in 0..3 {
                let off = k as f32 * 3.0;
                out.extend(draw::round_rect(
                    Rect::new(
                        cx - w * 0.5 + off,
                        top + off,
                        cx + w * 0.5 + off,
                        top + h * 0.55 + off,
                    ),
                    2.0,
                    None,
                    Some(stroke),
                ));
            }
        }
        ParticipantKind::Boundary | ParticipantKind::Control | ParticipantKind::Entity => {
            let rect = Rect::new(cx - h * 0.6, top, cx + h * 0.6, top + h * 0.75);
            out.extend(draw::round_rect(rect, 3.0, None, Some(stroke)));
            out.push(Prim::circle(
                [rect.min[0], rect.min[1] + rect.height() * 0.35],
                h * 0.12,
                None,
                Some(stroke),
            ));
            out.push(Prim::line(
                [rect.min[0] + h * 0.12, rect.min[1] + rect.height() * 0.35],
                [rect.max[0], rect.min[1] + rect.height() * 0.35],
                stroke,
            ));
        }
        ParticipantKind::Default => {}
    }
    out
}

fn message_prims(m: &Msg, cx: &[f32], ctx: &Ctx) -> Vec<Prim> {
    let mut out = Vec::new();
    let from_x = cx.get(m.from).copied().unwrap_or(0.0);
    let to_x = cx.get(m.to).copied().unwrap_or(0.0);
    let stroke = draw::stroke(1.2, ctx.theme.edge);
    let font = ctx.small();
    let lh = ctx.lh(font);

    if m.self_call || (from_x - to_x).abs() < 1.0 {
        let w = 34.0;
        let top = m.y - 8.0;
        let bot = m.y + 8.0;
        let pts = [[from_x, top], [from_x + w, top], [from_x + w, bot], [from_x, bot]];
        for p in pts.windows(2) {
            out.extend(draw::segment(p[0], p[1], stroke, m.line));
        }
        out.extend(draw::arrow_head(
            [from_x, bot],
            [-1.0, 0.0],
            m.head,
            ctx.theme.edge,
            draw::ARROW_SIZE,
        ));
        if !m.label.trim().is_empty() {
            let center = [from_x + w + 10.0 + ctx.w(&m.label, font) * 0.5, m.y];
            out.extend(label_pill(&m.label, center, ctx));
        }
    } else {
        out.extend(draw::segment([from_x, m.y], [to_x, m.y], stroke, m.line));
        let dir = if to_x >= from_x { 1.0 } else { -1.0 };
        out.extend(draw::arrow_head(
            [to_x, m.y],
            [dir, 0.0],
            m.head,
            ctx.theme.edge,
            draw::ARROW_SIZE,
        ));
        if !m.label.trim().is_empty() {
            let center = [(from_x + to_x) * 0.5, m.y - lh * 0.6 - 2.0];
            out.extend(label_pill(&m.label, center, ctx));
        }
    }
    out
}

fn frame_prims(frame: &Frame, min_x: f32, max_x: f32, ctx: &Ctx) -> Vec<Prim> {
    let indent = frame.depth as f32 * 12.0;
    let left = min_x - 26.0 + indent;
    let right = max_x + 26.0;
    let top = frame.start - 18.0;
    let bottom = frame.end + 6.0;
    if bottom <= top || right <= left {
        return Vec::new();
    }

    let font = ctx.small_bold();
    let lh = ctx.lh(font);
    let tab_text = format!("{} {}", frame.kind, frame.label);
    let tab_w = (ctx.w(&tab_text, font) + 14.0).max(30.0);
    let tab_h = lh + 6.0;

    let mut out = Vec::new();
    out.extend(draw::round_rect(
        Rect::new(left, top, right, bottom),
        6.0,
        None,
        Some(draw::stroke(1.0, ctx.theme.cluster_stroke)),
    ));
    out.extend(draw::round_rect(
        Rect::new(left, top, left + tab_w, top + tab_h),
        4.0,
        Some(ctx.theme.cluster_fill),
        Some(draw::stroke(1.0, ctx.theme.cluster_stroke)),
    ));
    out.push(Prim::anchored_text(
        [left + 7.0, top + tab_h * 0.5],
        tab_text,
        font,
        ctx.theme.cluster_text,
        Anchor::LeftCenter,
    ));

    if let (Some(y), Some(label)) = (frame.else_y, frame.else_label.as_deref()) {
        out.extend(draw::segment(
            [left, y],
            [right, y],
            draw::stroke(1.0, ctx.theme.cluster_stroke),
            LineStyle::Dashed,
        ));
        out.extend(label_pill(label, [(left + right) * 0.5, y], ctx));
    }
    out
}

// ===========================================================================
// Pie charts
// ===========================================================================

fn layout_pie(pie: &Pie, diagram: &Diagram, ctx: &Ctx) -> DiagramRender {
    let summary = diagram.summary();
    let mut warnings: Vec<String> = Vec::new();
    let n = pie.slices.len();
    let total = pie.total();

    let title_font = ctx.title();
    let legend_font = ctx.small();
    let r = (80.0 + (n as f32 - 1.0).max(0.0) * 5.0).min(170.0);
    let legend_x = r + 30.0;
    let row_h = ctx.lh(legend_font) + 6.0;
    let legend_top = -(row_h * n as f32) * 0.5;

    let mut legend_texts: Vec<String> = Vec::with_capacity(n);
    for s in &pie.slices {
        let pct = if total > 0.0 {
            s.value / total * 100.0
        } else {
            0.0
        };
        let text = if pie.show_data {
            format!("{}  {} ({:.1}%)", s.label, s.value, pct)
        } else {
            format!("{}  {:.1}%", s.label, pct)
        };
        legend_texts.push(text);
    }

    let mut prims: Vec<Prim> = Vec::new();

    if !total.is_finite() || total <= 0.0 {
        warnings.push("饼图数值合计为 0：已绘制空圆".to_owned());
        prims.push(Prim::circle(
            [0.0, 0.0],
            r,
            Some(ctx.theme.node_fill),
            Some(draw::stroke(1.2, ctx.theme.node_stroke)),
        ));
    } else {
        let sep = draw::stroke(1.0, ctx.theme.background);
        let mut angle = -PI / 2.0;
        for (i, s) in pie.slices.iter().enumerate() {
            let frac = (s.value / total).clamp(0.0, 1.0);
            let sweep = frac * 2.0 * PI;
            let a0 = angle;
            let a1 = angle + sweep;
            angle = a1;

            let color = series_color(i);
            out_sector(&mut prims, 0.0, r, a0, a1, color, sep);

            // An inside percentage only when the arc is wide enough to hold it.
            if sweep * r * 0.62 >= 34.0 {
                let mid = (a0 + a1) * 0.5;
                let pos = [mid.cos() * r * 0.6, mid.sin() * r * 0.6];
                let text_color = if color.luminance() > 0.55 {
                    Rgba::BLACK
                } else {
                    Rgba::WHITE
                };
                prims.push(Prim::anchored_text(
                    pos,
                    format!("{:.0}%", frac * 100.0),
                    legend_font,
                    text_color,
                    Anchor::Center,
                ));
            }
        }
        prims.push(Prim::circle(
            [0.0, 0.0],
            r,
            None,
            Some(draw::stroke(1.0, ctx.theme.node_stroke.fade(0.5))),
        ));
    }

    for (i, text) in legend_texts.iter().enumerate() {
        let y = legend_top + row_h * (i as f32 + 0.5);
        prims.extend(draw::round_rect(
            Rect::from_min_size([legend_x, y - 6.0], [12.0, 12.0]),
            2.0,
            Some(series_color(i)),
            None,
        ));
        prims.push(Prim::anchored_text(
            [legend_x + 18.0, y],
            text.clone(),
            legend_font,
            ctx.theme.node_text,
            Anchor::LeftCenter,
        ));
    }

    if let Some(title) = pie.title.as_deref()
        && !title.trim().is_empty()
    {
        let lh = ctx.lh(title_font);
        prims.push(Prim::anchored_text(
            [0.0, -r - 16.0 - lh * 0.5],
            title,
            title_font,
            ctx.theme.node_text,
            Anchor::Center,
        ));
    }

    finish(prims, ctx, None, warnings, summary)
}

fn out_sector(out: &mut Vec<Prim>, cx: f32, r: f32, a0: f32, a1: f32, color: Rgba, edge: Stroke) {
    let center = [cx, 0.0];
    let pts = draw::sector_points(center, r, a0, a1);
    if let Some(p) = draw::path(pts, true, Some(color), Some(edge)) {
        out.push(p);
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagram::model::{
        Cluster, Edge, Graph, GroupKind, LineStyle, Node, NodeShape, NotePosition, Participant,
        ParticipantKind, Pie, SeqStep, Sequence, Slice,
    };
    use crate::diagram::{Diagram, Engine};
    use crate::gfx::{ApproxMeasure, Family, Prim, Rgba};

    fn theme() -> DiagramTheme {
        DiagramTheme {
            node_fill: Rgba::hex(0xFFFFFF),
            node_stroke: Rgba::hex(0x333333),
            node_text: Rgba::hex(0x111111),
            edge: Rgba::hex(0x555555),
            edge_text: Rgba::hex(0x333333),
            cluster_fill: Rgba::hex(0xEEF3FF),
            cluster_stroke: Rgba::hex(0x9BB4E8),
            cluster_text: Rgba::hex(0x2A3B5C),
            note_fill: Rgba::hex(0xFFF6D8),
            note_stroke: Rgba::hex(0xE0B84C),
            note_text: Rgba::hex(0x5A4300),
            accent: Rgba::hex(0x4C8DFF),
            transparent: true,
            background: Rgba::hex(0xFFFFFF),
            font_family: Family::Sans,
            title_size: 14.0,
            font_scale: 1.0,
        }
    }

    fn run(d: &Diagram) -> DiagramRender {
        layout(d, &theme(), 14.0, &ApproxMeasure)
    }

    fn chain(dir: Direction, count: usize) -> Diagram {
        let mut g = Graph {
            direction: dir,
            ..Graph::default()
        };
        for i in 0..count {
            g.nodes.push(Node::new(format!("n{i}"), format!("node{i}")));
        }
        for i in 1..count {
            g.edges.push(Edge::new(format!("n{}", i - 1), format!("n{i}")));
        }
        Diagram::Graph(g)
    }

    fn sequence_with_messages(count: usize) -> Diagram {
        let mut s = Sequence::default();
        s.participants.push(Participant {
            id: "A".into(),
            label: "alice".into(),
            kind: ParticipantKind::Default,
            declared: true,
                ..Default::default()
        });
        s.participants.push(Participant {
            id: "B".into(),
            label: "bob".into(),
            kind: ParticipantKind::Default,
            declared: true,
                ..Default::default()
        });
        for i in 0..count {
            s.steps.push(SeqStep::Message {
                index: i as u32 + 1,
                from: 0,
                to: 1,
                label: format!("message {i}"),
                line: LineStyle::Solid,
                head: Arrow::Filled,
                activate: false,
                deactivate: false,
                self_call: false,
            });
        }
        Diagram::Sequence(s)
    }

    fn pie(count: usize) -> Diagram {
        let mut p = Pie::default();
        for i in 0..count {
            p.slices.push(Slice {
                label: format!("slice {i}"),
                value: (i + 1) as f32,
            });
        }
        Diagram::Pie(p)
    }

    fn coords(p: &Prim) -> Vec<[f32; 2]> {
        match p {
            Prim::Rect { rect, .. } => vec![rect.min, rect.max],
            Prim::Circle { center, radius, .. } => vec![
                *center,
                [center[0] - radius, center[1] - radius],
                [center[0] + radius, center[1] + radius],
            ],
            Prim::Ellipse { center, radii, .. } => vec![
                *center,
                [center[0] - radii[0], center[1] - radii[1]],
                [center[0] + radii[0], center[1] + radii[1]],
            ],
            Prim::Line { from, to, .. } => vec![*from, *to],
            Prim::Path { points, .. } => points.clone(),
            Prim::Text { pos, .. } => vec![*pos],
        }
    }

    fn assert_clean(r: &DiagramRender) {
        assert!(r.size[0].is_finite() && r.size[1].is_finite(), "size {:?}", r.size);
        assert!(r.size[0] > 0.0 && r.size[1] > 0.0, "size {:?}", r.size);
        for p in &r.prims {
            for c in coords(p) {
                assert!(c[0].is_finite() && c[1].is_finite(), "non-finite {c:?}");
                assert!(c[0] >= 0.0 && c[1] >= 0.0, "negative {c:?}");
            }
        }
    }

    #[test]
    fn flowchart_two_nodes_has_size() {
        let mut g = Graph::default();
        g.nodes.push(Node::new("A", "开始"));
        g.nodes.push(Node::new("B", "结束"));
        g.edges.push(Edge::new("A", "B"));
        let r = run(&Diagram::Graph(g));
        assert!(r.error.is_none());
        assert!(r.size[0] > 0.0 && r.size[1] > 0.0);
        assert!(!r.prims.is_empty());
        assert_eq!(r.summary, "flowchart · 2 个节点 · 1 条连线");
    }

    #[test]
    fn direction_controls_aspect() {
        let lr = run(&chain(Direction::LeftRight, 3));
        let td = run(&chain(Direction::TopDown, 3));
        assert!(
            lr.size[0] > lr.size[1],
            "LeftRight should be wide: {:?}",
            lr.size
        );
        assert!(
            td.size[1] > td.size[0],
            "TopDown should be tall: {:?}",
            td.size
        );
    }

    #[test]
    fn mirrored_directions_are_laid_out_too() {
        let bt = run(&chain(Direction::BottomTop, 3));
        let rl = run(&chain(Direction::RightLeft, 3));
        assert_clean(&bt);
        assert_clean(&rl);
        assert!(bt.size[1] > bt.size[0]);
        assert!(rl.size[0] > rl.size[1]);
    }

    #[test]
    fn all_coordinates_finite_and_non_negative() {
        assert_clean(&run(&chain(Direction::TopDown, 5)));
        assert_clean(&run(&chain(Direction::LeftRight, 4)));
        assert_clean(&run(&sequence_with_messages(3)));
        assert_clean(&run(&pie(4)));
        assert_clean(&run(&Diagram::Graph(Graph::default())));
    }

    #[test]
    fn sequence_grows_with_messages() {
        let one = run(&sequence_with_messages(1));
        let three = run(&sequence_with_messages(3));
        assert!(three.size[1] > one.size[1], "{:?} vs {:?}", three.size, one.size);
    }

    #[test]
    fn pie_one_and_four_slices_render() {
        for n in [1usize, 4] {
            let r = run(&pie(n));
            assert!(r.error.is_none());
            assert!(r.size[0] > 0.0 && r.size[1] > 0.0);
            assert_clean(&r);
        }
    }

    #[test]
    fn unknown_edge_reference_terminates() {
        let mut g = Graph::default();
        g.nodes.push(Node::new("A", "A"));
        g.edges.push(Edge::new("A", "ghost"));
        let r = run(&Diagram::Graph(g));
        assert!(r.warnings.iter().any(|w| w.contains("ghost")));
        assert_clean(&r);
    }

    #[test]
    fn empty_graph_shows_message() {
        let r = run(&Diagram::Graph(Graph::default()));
        assert!(r.error.is_none());
        assert!(r.size[0] > 0.0 && r.size[1] > 0.0);
        assert!(r
            .prims
            .iter()
            .any(|p| matches!(p, Prim::Text { text, .. } if text.contains("空图表"))));
    }

    #[test]
    fn filled_arrow_is_emitted() {
        let mut g = Graph::default();
        g.nodes.push(Node::new("A", "A"));
        g.nodes.push(Node::new("B", "B"));
        let mut e = Edge::new("A", "B");
        e.head = Arrow::Filled;
        g.edges.push(e);
        let r = run(&Diagram::Graph(g));
        let has_triangle = r.prims.iter().any(|p| {
            matches!(p, Prim::Path { points, closed: true, fill: Some(_), .. } if points.len() == 3)
        });
        assert!(has_triangle, "no filled arrow head emitted");
    }

    #[test]
    fn dashed_edge_has_more_lines_than_solid() {
        let count = |style: LineStyle| {
            let mut g = Graph::default();
            g.nodes.push(Node::new("A", "alpha"));
            g.nodes.push(Node::new("B", "beta"));
            let mut e = Edge::new("A", "B");
            e.line = style;
            e.head = Arrow::None;
            g.edges.push(e);
            run(&Diagram::Graph(g))
                .prims
                .iter()
                .filter(|p| matches!(p, Prim::Line { .. }))
                .count()
        };
        assert!(count(LineStyle::Dashed) > count(LineStyle::Solid));
        assert!(count(LineStyle::Dotted) > count(LineStyle::Solid));
    }

    #[test]
    fn fence_info_maps_known_engines() {
        assert_eq!(Engine::from_fence_info("mermaid"), Some(Engine::Mermaid));
        assert_eq!(Engine::from_fence_info("mmd"), Some(Engine::Mermaid));
        assert_eq!(Engine::from_fence_info("mermaid { theme: dark }"), Some(Engine::Mermaid));
        assert_eq!(Engine::from_fence_info("plantuml"), Some(Engine::PlantUml));
        assert_eq!(Engine::from_fence_info("puml"), Some(Engine::PlantUml));
        assert_eq!(Engine::from_fence_info("uml"), Some(Engine::PlantUml));
        assert_eq!(Engine::from_fence_info("plantuml-svg"), Some(Engine::PlantUml));
        assert_eq!(Engine::from_fence_info("rust"), None);
        assert_eq!(Engine::from_fence_info(""), None);
    }

    #[test]
    fn title_shifts_content_down() {
        let mut with = Graph::default();
        with.nodes.push(Node::new("A", "A"));
        with.nodes.push(Node::new("B", "B"));
        with.edges.push(Edge::new("A", "B"));
        with.title = Some("流程".to_owned());

        let mut without = with.clone();
        without.title = None;

        let a = run(&Diagram::Graph(with));
        let b = run(&Diagram::Graph(without));
        assert!(a.size[1] > b.size[1]);
        assert_clean(&a);
    }

    #[test]
    fn sequence_notes_groups_and_activation_terminate() {
        let mut s = Sequence::default();
        s.participants.push(Participant {
            id: "A".into(),
            label: "alice".into(),
            kind: ParticipantKind::Actor,
            declared: true,
                ..Default::default()
        });
        s.participants.push(Participant {
            id: "B".into(),
            label: "bob".into(),
            kind: ParticipantKind::Store,
            declared: true,
                ..Default::default()
        });
        s.steps.push(SeqStep::GroupOpen {
            kind: GroupKind::Alt,
            label: "ok".into(),
            depth: 0,
        });
        s.steps.push(SeqStep::Message {
            index: 1,
            from: 0,
            to: 1,
            label: "hello\nthere".into(),
            line: LineStyle::Solid,
            head: Arrow::Filled,
            activate: true,
            deactivate: false,
            self_call: false,
        });
        s.steps.push(SeqStep::Note {
            over: vec![0, 1],
            position: NotePosition::Over,
            text: "a note".into(),
        });
        s.steps.push(SeqStep::GroupElse {
            label: "ko".into(),
            depth: 0,
        });
        s.steps.push(SeqStep::Divider {
            label: "phase".into(),
        });
        s.steps.push(SeqStep::Delay {
            text: "waiting".into(),
        });
        s.steps.push(SeqStep::GroupClose { depth: 0 });
        let r = run(&Diagram::Sequence(s));
        assert_clean(&r);
    }

    #[test]
    fn pie_zero_total_warns_and_draws() {
        let mut p = Pie::default();
        p.slices.push(Slice {
            label: "none".into(),
            value: 0.0,
        });
        let r = run(&Diagram::Pie(p));
        assert!(r.error.is_none());
        assert!(!r.warnings.is_empty());
        assert_clean(&r);
    }

    #[test]
    fn degenerate_inputs_never_panic() {
        let mut cases: Vec<Diagram> = Vec::new();

        // Empty label and a 40-character label.
        let mut g = Graph::default();
        g.nodes.push(Node::new("empty", ""));
        g.nodes.push(Node::new("long", "x".repeat(40)));
        g.edges.push(Edge::new("empty", "long"));
        cases.push(Diagram::Graph(g));

        // Self loop.
        let mut g = Graph::default();
        g.nodes.push(Node::new("A", "A"));
        let mut e = Edge::new("A", "A");
        e.label = Some("loop".into());
        g.edges.push(e);
        cases.push(Diagram::Graph(g));

        // A -> B -> A cycle.
        let mut g = Graph::default();
        g.nodes.push(Node::new("A", "A"));
        g.nodes.push(Node::new("B", "B"));
        g.edges.push(Edge::new("A", "B"));
        g.edges.push(Edge::new("B", "A"));
        cases.push(Diagram::Graph(g));

        // 60-node chain.
        cases.push(chain(Direction::TopDown, 60));

        // Cluster with no members.
        let mut g = Graph::default();
        g.nodes.push(Node::new("A", "A"));
        g.clusters.push(Cluster {
            id: "c".into(),
            label: "empty".into(),
            members: Vec::new(),
            parent: None,
                ..Default::default()
        });
        cases.push(Diagram::Graph(g));

        // Cluster whose member list references a missing node.
        let mut g = Graph::default();
        g.nodes.push(Node::new("A", "A"));
        g.clusters.push(Cluster {
            id: "c".into(),
            label: "ghostly".into(),
            members: vec!["missing".into()],
            parent: Some(0),
                ..Default::default()
        });
        cases.push(Diagram::Graph(g));

        // All node shapes, to exercise every painting branch.
        for shape in [
            NodeShape::Rect,
            NodeShape::Round,
            NodeShape::Stadium,
            NodeShape::Circle,
            NodeShape::Diamond,
            NodeShape::Hexagon,
            NodeShape::Parallelogram,
            NodeShape::Cylinder,
            NodeShape::Asymmetric,
            NodeShape::Note,
            NodeShape::Classifier,
        ] {
            let mut g = Graph::default();
            let mut n = Node::new("n", "shape");
            n.shape = shape;
            n.stereotype = Some("stereo".into());
            if shape == NodeShape::Classifier {
                n.compartments.push(vec!["field: int".into()]);
                n.compartments.push(vec!["do()".into()]);
            }
            g.nodes.push(n);
            cases.push(Diagram::Graph(g));
        }

        // Terminals.
        let mut g = Graph::default();
        let mut start = Node::new("[*]", "");
        start.shape = NodeShape::Circle;
        start.style = NodeStyle::Terminal;
        let mut end = Node::new("[*-end]", "");
        end.shape = NodeShape::Circle;
        end.style = NodeStyle::Terminal;
        g.nodes.push(start);
        g.nodes.push(end);
        g.edges.push(Edge::new("[*]", "[*-end]"));
        cases.push(Diagram::Graph(g));

        // Sequence with an out-of-range participant index.
        let mut s = Sequence::default();
        s.participants.push(Participant {
            id: "A".into(),
            label: "A".into(),
            kind: ParticipantKind::Default,
            declared: false,
                ..Default::default()
        });
        s.steps.push(SeqStep::Message {
            index: 1,
            from: 0,
            to: 9,
            label: "oops".into(),
            line: LineStyle::Solid,
            head: Arrow::Filled,
            activate: false,
            deactivate: false,
            self_call: false,
        });
        s.steps.push(SeqStep::Note {
            over: vec![7],
            position: NotePosition::Left,
            text: "dangling".into(),
        });
        cases.push(Diagram::Sequence(s));

        // Group close with no open.
        let mut s = Sequence::default();
        s.participants.push(Participant {
            id: "A".into(),
            label: "A".into(),
            kind: ParticipantKind::Default,
            declared: false,
                ..Default::default()
        });
        s.steps.push(SeqStep::GroupClose { depth: 0 });
        s.steps.push(SeqStep::GroupElse {
            label: "stray".into(),
            depth: 0,
        });
        cases.push(Diagram::Sequence(s));

        // Self call.
        let mut s = Sequence::default();
        s.participants.push(Participant {
            id: "A".into(),
            label: "A".into(),
            kind: ParticipantKind::Default,
            declared: false,
                ..Default::default()
        });
        s.steps.push(SeqStep::Message {
            index: 1,
            from: 0,
            to: 0,
            label: "self".into(),
            line: LineStyle::Dashed,
            head: Arrow::Open,
            activate: true,
            deactivate: true,
            self_call: true,
        });
        cases.push(Diagram::Sequence(s));

        // Pie with a negative value and a zero slice.
        let mut p = Pie::default();
        p.slices.push(Slice {
            label: "neg".into(),
            value: -3.0,
        });
        p.slices.push(Slice {
            label: "zero".into(),
            value: 0.0,
        });
        cases.push(Diagram::Pie(p));

        // Pie with a zero total but several slices.
        let mut p = Pie::default();
        for i in 0..3 {
            p.slices.push(Slice {
                label: format!("s{i}"),
                value: 0.0,
            });
        }
        cases.push(Diagram::Pie(p));

        // Pie with one huge slice.
        let mut p = Pie::default();
        p.slices.push(Slice {
            label: "all".into(),
            value: 1e9,
        });
        cases.push(Diagram::Pie(p));

        // Empty sequence / empty graph sources.
        cases.push(Diagram::Sequence(Sequence::default()));
        cases.push(Diagram::Graph(Graph::default()));
        cases.push(Diagram::Pie(Pie::default()));

        // Cascade: cluster referencing an unknown parent plus a title.
        let mut g = Graph::default();
        g.nodes.push(Node::new("A", "A"));
        g.title = Some("degenerate".into());
        g.clusters.push(Cluster {
            id: "c".into(),
            label: "orphan".into(),
            members: vec!["A".into()],
            parent: Some(9),
                ..Default::default()
        });
        cases.push(Diagram::Graph(g));

        // Warnings-only graph (parsed but nothing usable).
        let mut g = Graph::default();
        g.warnings.push("ignored directive: style".into());
        cases.push(Diagram::Graph(g));

        assert!(cases.len() >= 20, "only {} degenerate cases", cases.len());
        for (i, case) in cases.iter().enumerate() {
            let r = run(case);
            assert_clean(&r);
            assert!(r.size[0] <= 200_000.0 && r.size[1] <= 200_000.0, "case {i} exploded");
        }
    }
}
