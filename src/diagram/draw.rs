//! Painting helpers shared by the graph, sequence and pie layouts.
//!
//! Layout decides *where* an element lives; this module knows *how* one element
//! is painted. Every entry point is a pure `… -> Vec<Prim>` transformation over
//! the renderer-agnostic primitives in [`crate::gfx`], so the same code drives
//! the live egui painter and the headless unit tests.
//!
//! [`Prim`] carries neither a dash pattern nor an arc, so both are synthesised
//! here: dashed and dotted strokes become a run of short [`Prim::Line`]s whose
//! pattern is anchored at the *source* end, and arcs (cylinder caps, note
//! folds, pie sectors) become [`Prim::Path`] polylines.
//!
//! Nothing in this file can panic: degenerate inputs yield empty vectors, and
//! every scalar that reaches a [`Prim`] is checked for finiteness.

use std::f32::consts::PI;

use crate::diagram::model::{Arrow, LineStyle};
use crate::gfx::{Anchor, Measure, Prim, Rect, Rgba, Stroke};

/// Longest arrow head we ever emit, in points.
pub(crate) const ARROW_SIZE: f32 = 9.0;

/// `LineStyle::Dashed`: 6 on, 4 off.
const DASH_ON: f32 = 6.0;
const DASH_OFF: f32 = 4.0;
/// `LineStyle::Dotted`: 2 on, 3 off.
const DOT_ON: f32 = 2.0;
const DOT_OFF: f32 = 3.0;
/// Hard cap so a pathological length can never spin the dash loop.
const MAX_DASHES: usize = 4096;

// ---------------------------------------------------------------------------
// Scalar guards
// ---------------------------------------------------------------------------

/// Replaces a non-finite scalar with `fallback`.
pub(crate) fn fin(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

pub(crate) fn finite_point(p: [f32; 2]) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

pub(crate) fn finite_rect(r: &Rect) -> bool {
    finite_point(r.min) && finite_point(r.max)
}

/// A stroke with a clamped, finite width.
pub(crate) fn stroke(width: f32, color: Rgba) -> Stroke {
    Stroke::new(fin(width, 1.0).max(0.0), color)
}

fn unit(v: [f32; 2]) -> [f32; 2] {
    let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if len.is_finite() && len > 1e-6 {
        [v[0] / len, v[1] / len]
    } else {
        [1.0, 0.0]
    }
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

// ---------------------------------------------------------------------------
// Basic shapes
// ---------------------------------------------------------------------------

/// Rounded rectangle, with the radius clamped to half the shorter side.
pub(crate) fn round_rect(
    rect: Rect,
    radius: f32,
    fill: Option<Rgba>,
    stroke: Option<Stroke>,
) -> Option<Prim> {
    if !finite_rect(&rect) {
        return None;
    }
    let limit = (rect.width().min(rect.height()) * 0.5).max(0.0);
    Some(Prim::rect(
        rect,
        fin(radius, 0.0).clamp(0.0, limit),
        fill,
        stroke,
    ))
}

/// Closed/open polyline, dropped when degenerate.
pub(crate) fn path(
    points: Vec<[f32; 2]>,
    closed: bool,
    fill: Option<Rgba>,
    stroke: Option<Stroke>,
) -> Option<Prim> {
    if points.len() < 2 || points.iter().any(|p| !finite_point(*p)) {
        return None;
    }
    Some(Prim::path(points, closed, fill, stroke))
}

// ---------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------

/// A straight, dashed or dotted run from `from` to `to`.
///
/// The dash phase is anchored at `from`, so the pattern starts at the source of
/// an edge and does not crawl when the target node moves.
pub(crate) fn segment(from: [f32; 2], to: [f32; 2], stroke: Stroke, style: LineStyle) -> Vec<Prim> {
    if !finite_point(from) || !finite_point(to) {
        return Vec::new();
    }
    match style {
        LineStyle::Solid => vec![Prim::line(from, to, stroke)],
        LineStyle::Dashed => dashed(from, to, stroke, DASH_ON, DASH_OFF),
        LineStyle::Dotted => dashed(from, to, stroke, DOT_ON, DOT_OFF),
    }
}

/// Synthesise a dash pattern as a run of short [`Prim::Line`]s.
pub(crate) fn dashed(from: [f32; 2], to: [f32; 2], stroke: Stroke, on: f32, off: f32) -> Vec<Prim> {
    let dx = to[0] - from[0];
    let dy = to[1] - from[1];
    let len = (dx * dx + dy * dy).sqrt();
    if !len.is_finite() || len <= 1e-4 {
        return vec![Prim::line(from, to, stroke)];
    }
    let on = fin(on, 1.0).max(0.4);
    let off = fin(off, 0.0).max(0.0);
    let step = on + off;
    if !step.is_finite() || step <= 0.0 {
        return vec![Prim::line(from, to, stroke)];
    }
    let u = [dx / len, dy / len];
    let mut out = Vec::new();
    let mut t = 0.0f32;
    while t < len && out.len() < MAX_DASHES {
        let end = (t + on).min(len);
        out.push(Prim::line(
            [from[0] + u[0] * t, from[1] + u[1] * t],
            [from[0] + u[0] * end, from[1] + u[1] * end],
            stroke,
        ));
        t += step;
    }
    if out.is_empty() {
        out.push(Prim::line(from, to, stroke));
    }
    out
}

// ---------------------------------------------------------------------------
// Arrow heads
// ---------------------------------------------------------------------------

/// Arrow head (or tail) decoration whose tip sits exactly at `tip`.
///
/// `dir` is the direction of travel *into* the tip; it does not need to be a
/// unit vector. `size` is clamped to a sane range so a hostile label cannot
/// produce a monstrous head.
pub(crate) fn arrow_head(
    tip: [f32; 2],
    dir: [f32; 2],
    kind: Arrow,
    color: Rgba,
    size: f32,
) -> Vec<Prim> {
    if !finite_point(tip) || kind == Arrow::None {
        return Vec::new();
    }
    let size = fin(size, ARROW_SIZE).clamp(1.0, 24.0);
    let d = unit(dir);
    let perp = [-d[1], d[0]];
    let st = stroke(1.2, color);
    let back = [tip[0] - d[0] * size, tip[1] - d[1] * size];
    let wing = |k: f32| {
        [
            back[0] + perp[0] * size * k,
            back[1] + perp[1] * size * k,
        ]
    };
    match kind {
        Arrow::None => Vec::new(),
        Arrow::Filled => {
            path(vec![tip, wing(0.5), wing(-0.5)], true, Some(color), None)
                .into_iter()
                .collect()
        }
        Arrow::Open => vec![
            Prim::line(tip, wing(0.6), st),
            Prim::line(tip, wing(-0.6), st),
        ],
        Arrow::Hollow => path(vec![tip, wing(0.5), wing(-0.5)], true, None, Some(st))
            .into_iter()
            .collect(),
        Arrow::Diamond | Arrow::FilledDiamond => {
            let half = size * 0.62;
            let c = [tip[0] - d[0] * half, tip[1] - d[1] * half];
            let a = [c[0] + perp[0] * size * 0.42, c[1] + perp[1] * size * 0.42];
            let b = [c[0] - perp[0] * size * 0.42, c[1] - perp[1] * size * 0.42];
            let tail = [tip[0] - d[0] * half * 2.0, tip[1] - d[1] * half * 2.0];
            let fill = if kind == Arrow::FilledDiamond {
                Some(color)
            } else {
                None
            };
            path(vec![tip, a, tail, b], true, fill, Some(st))
                .into_iter()
                .collect()
        }
        Arrow::Cross => {
            let s = size * 0.5;
            vec![
                Prim::line(add(tip, [-perp[0] * s + d[0] * s, -perp[1] * s + d[1] * s]), add(tip, [perp[0] * s - d[0] * s, perp[1] * s - d[1] * s]), st),
                Prim::line(add(tip, [perp[0] * s + d[0] * s, perp[1] * s + d[1] * s]), add(tip, [-perp[0] * s - d[0] * s, -perp[1] * s - d[1] * s]), st),
            ]
        }
        Arrow::Circle => {
            let c = [tip[0] - d[0] * size * 0.4, tip[1] - d[1] * size * 0.4];
            vec![Prim::circle(c, size * 0.38, None, Some(st))]
        }
    }
}

// ---------------------------------------------------------------------------
// Arcs, sectors and composite shapes
// ---------------------------------------------------------------------------

/// Points along an elliptical arc, inclusive of both ends.
pub(crate) fn arc_points(
    center: [f32; 2],
    radii: [f32; 2],
    a0: f32,
    a1: f32,
    steps: usize,
) -> Vec<[f32; 2]> {
    let rx = fin(radii[0], 0.0).max(0.0);
    let ry = fin(radii[1], 0.0).max(0.0);
    let steps = steps.clamp(2, 512);
    let mut out = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
        out.push([center[0] + rx * t.cos(), center[1] + ry * t.sin()]);
    }
    out
}

/// Closed pie sector: the centre plus an arc sampled at ≈2° per segment,
/// with a floor of 8 segments so tiny slices still look round.
pub(crate) fn sector_points(center: [f32; 2], r: f32, start: f32, end: f32) -> Vec<[f32; 2]> {
    let r = fin(r, 0.0).max(0.0);
    let sweep = end - start;
    let steps = ((sweep.abs() / (2.0 * PI / 180.0)).ceil() as usize).clamp(8, 720);
    let mut out = Vec::with_capacity(steps + 2);
    out.push(center);
    for i in 0..=steps {
        let t = start + sweep * (i as f32 / steps as f32);
        out.push([center[0] + r * t.cos(), center[1] + r * t.sin()]);
    }
    out
}

/// Database cylinder: a filled body, two straight sides, an ellipse cap on top
/// and a matching lower arc.
pub(crate) fn cylinder(
    rect: Rect,
    cap: f32,
    fill: Option<Rgba>,
    stroke: Option<Stroke>,
) -> Vec<Prim> {
    if !finite_rect(&rect) {
        return Vec::new();
    }
    let c = fin(cap, 8.0).clamp(0.0, rect.height() * 0.4);
    let cx = rect.center()[0];
    let rx = (rect.width() * 0.5).max(0.0);
    let ry = (c * 0.5).max(0.0);
    let top_c = [cx, rect.min[1] + ry];
    let bot_c = [cx, rect.max[1] - ry];

    let mut out = Vec::new();
    out.push(Prim::filled_rect(
        Rect::new(rect.min[0], top_c[1], rect.max[0], bot_c[1]),
        0.0,
        fill.unwrap_or(Rgba::TRANSPARENT),
    ));
    if let Some(s) = stroke {
        out.push(Prim::line([rect.min[0], top_c[1]], [rect.min[0], bot_c[1]], s));
        out.push(Prim::line([rect.max[0], top_c[1]], [rect.max[0], bot_c[1]], s));
    }
    out.push(Prim::ellipse(top_c, [rx, ry], fill, stroke));
    // Lower half of the bottom ellipse, closed so it can be filled.
    let arc = arc_points(bot_c, [rx, ry], 0.0, PI, 16);
    if let Some(p) = path(arc, true, fill, stroke) {
        out.push(p);
    }
    out
}

/// Sticky note: a quadrilateral with the top-right corner folded back.
pub(crate) fn note_shape(
    rect: Rect,
    fold: f32,
    fill: Option<Rgba>,
    stroke: Option<Stroke>,
) -> Vec<Prim> {
    if !finite_rect(&rect) {
        return Vec::new();
    }
    let limit = rect.width().min(rect.height()).max(0.0);
    let f = fin(fold, 10.0).clamp(0.0, limit);
    if f <= 0.5 {
        return round_rect(rect, 4.0, fill, stroke).into_iter().collect();
    }
    let [x0, y0, x1, y1] = [rect.min[0], rect.min[1], rect.max[0], rect.max[1]];
    let body = vec![[x0, y0], [x1 - f, y0], [x1, y0 + f], [x1, y1], [x0, y1]];
    let mut out = Vec::new();
    if let Some(p) = path(body, true, fill, stroke) {
        out.push(p);
    }
    if let Some(s) = stroke {
        out.push(Prim::line([x1 - f, y0], [x1 - f, y0 + f], s));
        out.push(Prim::line([x1 - f, y0 + f], [x1, y0 + f], s));
    }
    out
}

/// Start/end terminal of a state diagram: a solid dot for the start, a ring
/// with an inner dot for the end.
pub(crate) fn terminal(center: [f32; 2], is_end: bool, color: Rgba) -> Vec<Prim> {
    let r = 7.0;
    if is_end {
        vec![
            Prim::circle(center, r, None, Some(stroke(1.4, color))),
            Prim::circle(center, 2.4, Some(color), None),
        ]
    } else {
        vec![Prim::circle(center, r, Some(color), None)]
    }
}

// ---------------------------------------------------------------------------
// Bounds, translation and sanitising
// ---------------------------------------------------------------------------

/// Axis-aligned box actually covered by a text primitive.
pub(crate) fn text_bounds(
    pos: [f32; 2],
    text: &str,
    font: crate::gfx::Font,
    anchor: Anchor,
    m: &dyn Measure,
) -> Rect {
    let size = m.measure(text, font);
    let size = [fin(size[0], 0.0).max(0.0), fin(size[1], 0.0).max(0.0)];
    let f = anchor.factor();
    Rect::from_min_size(
        [pos[0] - f[0] * size[0], pos[1] - f[1] * size[1]],
        size,
    )
}

/// Union of every primitive's coverage, or `None` for an empty list.
///
/// Strokes are included via their width so a translated result never clips.
pub(crate) fn bounds(prims: &[Prim], m: &dyn Measure) -> Option<Rect> {
    let mut acc: Option<Rect> = None;
    for p in prims {
        let r = match p {
            Prim::Rect { rect, .. } => Some(*rect),
            Prim::Circle { center, radius, .. } => {
                Some(Rect::from_center_size(*center, [radius * 2.0, radius * 2.0]))
            }
            Prim::Ellipse { center, radii, .. } => {
                Some(Rect::from_center_size(*center, [radii[0] * 2.0, radii[1] * 2.0]))
            }
            Prim::Line { from, to, stroke } => Some(Rect::new(
                from[0] - stroke.width,
                from[1] - stroke.width,
                to[0] + stroke.width,
                to[1] + stroke.width,
            )),
            Prim::Path { points, .. } => {
                let mut it = points.iter();
                match it.next() {
                    None => None,
                    Some(first) => {
                        let (mut mn, mut mx) = (*first, *first);
                        for pt in it {
                            mn = [mn[0].min(pt[0]), mn[1].min(pt[1])];
                            mx = [mx[0].max(pt[0]), mx[1].max(pt[1])];
                        }
                        Some(Rect::new(mn[0], mn[1], mx[0], mx[1]))
                    }
                }
            }
            Prim::Text {
                pos,
                text,
                font,
                anchor,
                ..
            } => Some(text_bounds(*pos, text, *font, *anchor, m)),
        };
        if let Some(r) = r && finite_rect(&r) {
            acc = Some(match acc {
                Some(prev) => prev.union(&r),
                None => r,
            });
        }
    }
    acc
}

/// Shift every primitive by `d`.
pub(crate) fn translate(prims: &mut [Prim], d: [f32; 2]) {
    let d = [fin(d[0], 0.0), fin(d[1], 0.0)];
    if d == [0.0, 0.0] {
        return;
    }
    for p in prims.iter_mut() {
        match p {
            Prim::Rect { rect, .. } => {
                rect.min = add(rect.min, d);
                rect.max = add(rect.max, d);
            }
            Prim::Circle { center, .. } | Prim::Ellipse { center, .. } => *center = add(*center, d),
            Prim::Line { from, to, .. } => {
                *from = add(*from, d);
                *to = add(*to, d);
            }
            Prim::Path { points, .. } => {
                for pt in points.iter_mut() {
                    *pt = add(*pt, d);
                }
            }
            Prim::Text { pos, .. } => *pos = add(*pos, d),
        }
    }
}

/// Drop non-finite primitives and clamp every radius/width to a sane value.
///
/// Called once, right before the result leaves [`crate::diagram::layout`], so
/// the guarantee "no NaN/Inf reaches the painter" holds for every input.
pub(crate) fn sanitize(prims: &mut Vec<Prim>) {
    prims.retain_mut(clean);
}

fn clean(p: &mut Prim) -> bool {
    match p {
        Prim::Rect {
            rect,
            radius,
            stroke,
            ..
        } => {
            if !finite_rect(rect) {
                return false;
            }
            *radius = fin(*radius, 0.0).max(0.0);
            clean_stroke(stroke);
        }
        Prim::Circle {
            center,
            radius,
            stroke,
            ..
        } => {
            if !finite_point(*center) {
                return false;
            }
            *radius = fin(*radius, 0.0).max(0.0);
            clean_stroke(stroke);
        }
        Prim::Ellipse {
            center,
            radii,
            stroke,
            ..
        } => {
            if !finite_point(*center) {
                return false;
            }
            radii[0] = fin(radii[0], 0.0).max(0.0);
            radii[1] = fin(radii[1], 0.0).max(0.0);
            clean_stroke(stroke);
        }
        Prim::Line { from, to, stroke } => {
            if !finite_point(*from) || !finite_point(*to) {
                return false;
            }
            stroke.width = fin(stroke.width, 1.0).max(0.0);
        }
        Prim::Path {
            points, stroke, ..
        } => {
            if points.iter().any(|p| !finite_point(*p)) {
                return false;
            }
            clean_stroke(stroke);
        }
        Prim::Text { pos, font, .. } => {
            if !finite_point(*pos) {
                return false;
            }
            font.size = fin(font.size, 1.0).max(0.0);
        }
    }
    true
}

fn clean_stroke(st: &mut Option<Stroke>) {
    if let Some(s) = st {
        s.width = fin(s.width, 1.0).max(0.0);
    }
}
