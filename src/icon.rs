//! Callout icons drawn from the **official** path data of each ecosystem.
//!
//! The previous approach picked a Unicode character per variant out of the
//! symbol blocks (`ℹ`, `⚠`, `✔`, …). Those glyphs come from whatever font
//! happens to have them, so their weight, optical size and baseline never match
//! the surrounding text, and they look nothing like the icon GitHub, Obsidian or
//! Docusaurus actually put next to the title. In a WYSIWYG editor that is a
//! fidelity failure, not a cosmetic one: the whole point is that what you see
//! here is what the site will render.
//!
//! So the icons are the real thing: the path data is copied verbatim from
//! octicons (MIT, GitHub), lucide (ISC, Obsidian) and the Docusaurus theme
//! (MIT), stored in [`crate::icon_data`], and turned into geometry here. Nothing
//! is downloaded at runtime — the assets live in the binary.
//!
//! This module stays egui-free like the other engines: it emits triangles,
//! polylines and circles in the icon's own coordinate space, and
//! [`crate::gfx_egui::icon`] paints them.

use std::sync::OnceLock;

/// How overlapping subpaths are filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    /// Winding number != 0. What SVG uses by default, and what octicons rely on:
    /// a ring is drawn as two circles wound in opposite directions.
    NonZero,
    /// Crossing-count parity. Docusaurus sets `fill-rule="evenodd"` explicitly.
    EvenOdd,
}

/// One element of an icon, in the icon's own viewBox units.
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    /// A path. `d` is the official `d` attribute, unmodified.
    Path {
        d: &'static str,
        fill: FillRule,
        /// `true` for outline-only icons (lucide draws with a stroke and no fill).
        stroked: bool,
    },
    /// A `<circle>` element, which lucide uses instead of a path for round shapes.
    Circle {
        c: [f32; 2],
        r: f32,
        stroked: bool,
    },
}

/// A complete icon: its coordinate space plus the shapes it is made of.
pub struct IconDef {
    /// The `viewBox` extent, used to fit the icon into its box without distortion.
    pub view: [f32; 2],
    /// Stroke width in viewBox units (lucide: 2 on a 24-unit grid).
    pub stroke: f32,
    pub shapes: &'static [Shape],
}

/// Which icon to draw. Grouped by the syntax whose icons they are, because the
/// four ecosystems do not use the same set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconId {
    GitHubInfo,
    GitHubBulb,
    GitHubReport,
    GitHubAlert,
    GitHubStop,
    ObsidianPencil,
    ObsidianInfo,
    ObsidianFlame,
    ObsidianAlert,
    ObsidianZap,
    ObsidianOctagon,
    ObsidianChevron,
    DocusaurusInfo,
    DocusaurusTip,
    DocusaurusNote,
    DocusaurusDanger,
}

impl IconId {
    fn index(self) -> usize {
        self as usize
    }

    /// The icon definition, from the vendored data table.
    pub fn def(self) -> &'static IconDef {
        use IconId::*;
        let (view, stroke, shapes) = match self {
            GitHubInfo => ([16.0, 16.0], 0.0, crate::icon_data::GITHUB_INFO),
            GitHubBulb => ([16.0, 16.0], 0.0, crate::icon_data::GITHUB_BULB),
            GitHubReport => ([16.0, 16.0], 0.0, crate::icon_data::GITHUB_REPORT),
            GitHubAlert => ([16.0, 16.0], 0.0, crate::icon_data::GITHUB_ALERT),
            GitHubStop => ([16.0, 16.0], 0.0, crate::icon_data::GITHUB_STOP),
            ObsidianPencil => ([24.0, 24.0], 2.0, crate::icon_data::OBSIDIAN_PENCIL),
            ObsidianInfo => ([24.0, 24.0], 2.0, crate::icon_data::OBSIDIAN_INFO),
            ObsidianFlame => ([24.0, 24.0], 2.0, crate::icon_data::OBSIDIAN_FLAME),
            ObsidianAlert => ([24.0, 24.0], 2.0, crate::icon_data::OBSIDIAN_ALERT),
            ObsidianZap => ([24.0, 24.0], 2.0, crate::icon_data::OBSIDIAN_ZAP),
            ObsidianOctagon => ([24.0, 24.0], 2.0, crate::icon_data::OBSIDIAN_OCTAGON),
            ObsidianChevron => ([24.0, 24.0], 2.0, crate::icon_data::OBSIDIAN_CHEVRON),
            DocusaurusInfo => ([14.0, 16.0], 0.0, crate::icon_data::DOCUSAURUS_INFO),
            DocusaurusTip => ([12.0, 16.0], 0.0, crate::icon_data::DOCUSAURUS_TIP),
            DocusaurusNote => ([14.0, 16.0], 0.0, crate::icon_data::DOCUSAURUS_NOTE),
            DocusaurusDanger => ([12.0, 16.0], 0.0, crate::icon_data::DOCUSAURUS_DANGER),
        };
        // Leak-free: the definition is built once per id and lives for the process.
        let slots = DEFINITIONS.get_or_init(|| {
            let mut v: Vec<OnceLock<IconDef>> = Vec::new();
            for _ in 0..COUNT {
                v.push(OnceLock::new());
            }
            v
        });
        slots[self.index()].get_or_init(|| IconDef {
            view,
            stroke,
            shapes,
        })
    }
}

const COUNT: usize = 16;

static DEFINITIONS: OnceLock<Vec<OnceLock<IconDef>>> = OnceLock::new();

/// Icon geometry, flattened once and reused every frame.
///
/// Filled icons become triangles (a concave silhouette cannot be a convex
/// polygon, which is all egui offers), outline icons become polylines and
/// circles. All coordinates stay in viewBox units; the painter scales them.
pub struct Geometry {
    pub view: [f32; 2],
    pub stroke: f32,
    /// Flat list of triangle corners, three per triangle.
    pub triangles: Vec<[f32; 2]>,
    pub polylines: Vec<Vec<[f32; 2]>>,
    /// Stubby subpaths (a dot in "i") cannot be stroked into existence: egg's
    /// line rendering has no caps, so they are collected as dots instead.
    pub dots: Vec<[f32; 2]>,
    pub circles: Vec<([f32; 2], f32)>,
}

static GEOMETRY: OnceLock<Vec<Geometry>> = OnceLock::new();

/// The flattened geometry for `id`, built on first use.
pub fn geometry(id: IconId) -> &'static Geometry {
    let all = GEOMETRY.get_or_init(|| {
        let ids = [
            IconId::GitHubInfo,
            IconId::GitHubBulb,
            IconId::GitHubReport,
            IconId::GitHubAlert,
            IconId::GitHubStop,
            IconId::ObsidianPencil,
            IconId::ObsidianInfo,
            IconId::ObsidianFlame,
            IconId::ObsidianAlert,
            IconId::ObsidianZap,
            IconId::ObsidianOctagon,
            IconId::ObsidianChevron,
            IconId::DocusaurusInfo,
            IconId::DocusaurusTip,
            IconId::DocusaurusNote,
            IconId::DocusaurusDanger,
        ];
        let mut out: Vec<Geometry> = Vec::new();
        for _ in 0..COUNT {
            out.push(Geometry {
                view: [1.0, 1.0],
                stroke: 0.0,
                triangles: Vec::new(),
                polylines: Vec::new(),
                dots: Vec::new(),
                circles: Vec::new(),
            });
        }
        for id in ids {
            out[id.index()] = build(id.def());
        }
        out
    });
    &all[id.index()]
}

fn build(def: &IconDef) -> Geometry {
    let mut g = Geometry {
        view: def.view,
        stroke: def.stroke,
        triangles: Vec::new(),
        polylines: Vec::new(),
        dots: Vec::new(),
        circles: Vec::new(),
    };
    // Curves are flattened to this fraction of the icon's largest dimension, so
    // a 16px icon and a 40px one both look round.
    let tolerance = def.view[0].max(def.view[1]) * 0.004;
    for shape in def.shapes {
        match *shape {
            Shape::Path {
                d,
                fill,
                stroked: false,
            } => {
                let subpaths = flatten(d, tolerance);
                g.triangles.extend(fill_triangles(&subpaths, fill));
            }
            Shape::Path {
                d,
                stroked: true,
                ..
            } => {
                for subpath in flatten(d, tolerance) {
                    if polyline_length(&subpath) <= def.stroke.max(0.1) * 0.6 {
                        // A degenerate stroke is a dot, not a line.
                        if let Some(p) = subpath.first() {
                            g.dots.push(*p);
                        }
                    } else {
                        g.polylines.push(subpath);
                    }
                }
            }
            Shape::Circle {
                c,
                r,
                stroked: false,
            } => {
                // A filled circle is just a polygon; reusing the filler keeps one
                // code path for coverage.
                let ring = circle_points(c, r, 24);
                g.triangles
                    .extend(fill_triangles(&[ring], FillRule::NonZero));
            }
            Shape::Circle { c, r, .. } => g.circles.push((c, r)),
        }
    }
    g
}

fn polyline_length(points: &[[f32; 2]]) -> f32 {
    points
        .windows(2)
        .map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt())
        .sum()
}

fn circle_points(c: [f32; 2], r: f32, segments: usize) -> Vec<[f32; 2]> {
    (0..segments)
        .map(|i| {
            let a = i as f32 / segments as f32 * std::f32::consts::TAU;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Filling
// ---------------------------------------------------------------------------

/// Scanline fill with a winding rule, emitted as trapezoid pairs.
///
/// A convex polygon would have been enough for a circle; the real icons are not
/// convex (the "i" of `info`, the speech bubble of `report`, the lightning of
/// `zap`), and octicons punch holes with opposite-wound subpaths. A trapezoid
/// sweep handles all of that exactly, and stays vector so icons stay sharp at
/// every zoom level.
fn fill_triangles(subpaths: &[Vec<[f32; 2]>], rule: FillRule) -> Vec<[f32; 2]> {
    // (y_low, y_high, x_low, x_high, direction)
    let mut edges: Vec<(f32, f32, f32, f32, i32)> = Vec::new();
    let mut ys: Vec<f32> = Vec::new();
    for subpath in subpaths {
        if subpath.len() < 3 {
            continue;
        }
        for i in 0..subpath.len() {
            let a = subpath[i];
            let b = subpath[(i + 1) % subpath.len()];
            if !a.iter().chain(b.iter()).all(|v| v.is_finite()) {
                continue;
            }
            // Horizontal edges contribute no crossings.
            if (a[1] - b[1]).abs() < 1e-9 {
                continue;
            }
            let (low, high, dir) = if a[1] < b[1] { (a, b, 1) } else { (b, a, -1) };
            edges.push((low[1], high[1], low[0], high[0], dir));
            ys.push(low[1]);
            ys.push(high[1]);
        }
    }
    if edges.is_empty() {
        return Vec::new();
    }
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ys.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    let mut tris: Vec<[f32; 2]> = Vec::new();
    for band in ys.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        if y1 - y0 < 1e-7 {
            continue;
        }
        let mid = (y0 + y1) * 0.5;
        // x at the band's middle (for ordering) plus x at both band edges (so the
        // trapezoid follows the slanted edge instead of stair-stepping).
        let mut crossings: Vec<(f32, f32, f32, i32)> = Vec::new();
        for &(ey0, ey1, ex0, ex1, dir) in &edges {
            if ey0 <= mid && mid < ey1 {
                let span = ey1 - ey0;
                let x_at = |y: f32| ex0 + (ex1 - ex0) * ((y - ey0) / span);
                crossings.push((x_at(mid), x_at(y0), x_at(y1), dir));
            }
        }
        if crossings.len() < 2 {
            continue;
        }
        crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut winding = 0;
        let mut count = 0usize;
        for pair in crossings.windows(2) {
            let (left, right) = (&pair[0], &pair[1]);
            winding += left.3;
            count += 1;
            let inside = match rule {
                FillRule::NonZero => winding != 0,
                FillRule::EvenOdd => count % 2 == 1,
            };
            if !inside {
                continue;
            }
            let p0 = [left.1, y0];
            let p1 = [right.1, y0];
            let p2 = [right.2, y1];
            let p3 = [left.2, y1];
            tris.extend_from_slice(&[p0, p1, p2, p0, p2, p3]);
        }
    }
    tris
}

// ---------------------------------------------------------------------------
// SVG path parsing
// ---------------------------------------------------------------------------

/// Flattens an SVG `d` string into closed-capable polylines.
///
/// Supports the full command set the vendored icons use — `M L H V C S Q T A Z`
/// in both cases — because the official data is not written in a subset. Unknown
/// commands stop the parse rather than producing garbage, and every caller is
/// covered by a test that asserts the shape count it expects.
pub fn flatten(d: &str, tolerance: f32) -> Vec<Vec<[f32; 2]>> {
    let mut p = Parser::new(d, tolerance.max(0.01));
    p.run();
    p.subpaths
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    tolerance: f32,
    subpaths: Vec<Vec<[f32; 2]>>,
    current: Vec<[f32; 2]>,
    cursor: [f32; 2],
    start: [f32; 2],
    /// Reflection point for `S` / `T`: the last cubic/quadratic control point.
    last_ctrl: Option<[f32; 2]>,
    command: u8,
}

impl<'a> Parser<'a> {
    fn new(d: &'a str, tolerance: f32) -> Self {
        Self {
            bytes: d.as_bytes(),
            at: 0,
            tolerance,
            subpaths: Vec::new(),
            current: Vec::new(),
            cursor: [0.0, 0.0],
            start: [0.0, 0.0],
            last_ctrl: None,
            command: b'M',
        }
    }

    fn run(&mut self) {
        self.skip_separators();
        while self.at < self.bytes.len() {
            let b = self.bytes[self.at];
            if b.is_ascii_alphabetic() {
                self.command = b;
                self.at += 1;
            }
            // Implicit repetition: a command keeps repeating until a new letter.
            let ok = self.one(self.command);
            if !ok {
                break;
            }
            self.skip_separators();
        }
        self.finish_subpath(false);
    }

    fn one(&mut self, command: u8) -> bool {
        let rel = command.is_ascii_lowercase();
        let base = if rel { self.cursor } else { [0.0, 0.0] };
        match command.to_ascii_uppercase() {
            b'M' => {
                let Some(p) = self.point(base) else {
                    return false;
                };
                // A new subpath: close the previous one without a closing edge.
                self.finish_subpath(false);
                self.cursor = p;
                self.start = p;
                self.current.push(p);
                self.last_ctrl = None;
                // Following pairs are implicit line-tos.
                self.command = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let Some(p) = self.point(base) else {
                    return false;
                };
                self.line_to(p);
            }
            b'H' => {
                let Some(x) = self.number() else {
                    return false;
                };
                let x = if rel { self.cursor[0] + x } else { x };
                self.line_to([x, self.cursor[1]]);
            }
            b'V' => {
                let Some(y) = self.number() else {
                    return false;
                };
                let y = if rel { self.cursor[1] + y } else { y };
                self.line_to([self.cursor[0], y]);
            }
            b'C' => {
                let (Some(c1), Some(c2), Some(p)) =
                    (self.point(base), self.point(base), self.point(base))
                else {
                    return false;
                };
                self.cubic_to(c1, c2, p);
            }
            b'S' => {
                let (Some(c2), Some(p)) = (self.point(base), self.point(base)) else {
                    return false;
                };
                let c1 = self
                    .last_ctrl
                    .map(|c| {
                        [
                            self.cursor[0] * 2.0 - c[0],
                            self.cursor[1] * 2.0 - c[1],
                        ]
                    })
                    .unwrap_or(self.cursor);
                self.cubic_to(c1, c2, p);
            }
            b'Q' => {
                let (Some(c), Some(p)) = (self.point(base), self.point(base)) else {
                    return false;
                };
                self.quad_to(c, p);
            }
            b'T' => {
                let Some(p) = self.point(base) else {
                    return false;
                };
                let c = self
                    .last_ctrl
                    .map(|c| {
                        [
                            self.cursor[0] * 2.0 - c[0],
                            self.cursor[1] * 2.0 - c[1],
                        ]
                    })
                    .unwrap_or(self.cursor);
                self.quad_to(c, p);
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some(p)) = (
                    self.number(),
                    self.number(),
                    self.number(),
                    self.flag(),
                    self.flag(),
                    self.point(base),
                ) else {
                    return false;
                };
                self.arc_to(rx, ry, rot, large, sweep, p);
            }
            b'Z' => {
                self.finish_subpath(true);
                self.cursor = self.start;
                self.last_ctrl = None;
            }
            _ => return false,
        }
        true
    }

    fn line_to(&mut self, p: [f32; 2]) {
        if self.current.is_empty() {
            self.current.push(self.cursor);
        }
        self.current.push(p);
        self.cursor = p;
        self.last_ctrl = None;
    }

    fn cubic_to(&mut self, c1: [f32; 2], c2: [f32; 2], p: [f32; 2]) {
        if self.current.is_empty() {
            self.current.push(self.cursor);
        }
        // Segment count from the control polygon's length: short curves on a
        // 16-unit grid need very few points.
        let rough = dist(self.cursor, c1) + dist(c1, c2) + dist(c2, p);
        let steps = ((rough / self.tolerance).sqrt().ceil() as usize).clamp(4, 40);
        let from = self.cursor;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            self.current.push(cubic_at(from, c1, c2, p, t));
        }
        self.cursor = p;
        self.last_ctrl = Some(c2);
    }

    fn quad_to(&mut self, c: [f32; 2], p: [f32; 2]) {
        if self.current.is_empty() {
            self.current.push(self.cursor);
        }
        let rough = dist(self.cursor, c) + dist(c, p);
        let steps = ((rough / self.tolerance).sqrt().ceil() as usize).clamp(4, 40);
        let from = self.cursor;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let mt = 1.0 - t;
            self.current.push([
                mt * mt * from[0] + 2.0 * mt * t * c[0] + t * t * p[0],
                mt * mt * from[1] + 2.0 * mt * t * c[1] + t * t * p[1],
            ]);
        }
        self.cursor = p;
        self.last_ctrl = Some(c);
    }

    /// Endpoint-parameterised arc, converted to cubics (SVG 1.1 F.6).
    fn arc_to(&mut self, rx: f32, ry: f32, rot_deg: f32, large: bool, sweep: bool, to: [f32; 2]) {
        let from = self.cursor;
        let (mut rx, mut ry) = (rx.abs(), ry.abs());
        if (from[0] - to[0]).abs() < 1e-9 && (from[1] - to[1]).abs() < 1e-9 {
            return;
        }
        if rx < 1e-9 || ry < 1e-9 {
            self.line_to(to);
            return;
        }
        let phi = rot_deg.to_radians();
        let (sin_p, cos_p) = phi.sin_cos();
        let dx = (from[0] - to[0]) * 0.5;
        let dy = (from[1] - to[1]) * 0.5;
        let x1 = cos_p * dx + sin_p * dy;
        let y1 = -sin_p * dx + cos_p * dy;
        // Scale up radii that are too small to span the endpoints.
        let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
        if lambda > 1.0 {
            let s = lambda.sqrt();
            rx *= s;
            ry *= s;
        }
        let sign = if large != sweep { 1.0 } else { -1.0 };
        let num = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
        let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
        let coef = if den <= 1e-12 {
            0.0
        } else {
            sign * (num / den).sqrt()
        };
        let cx1 = coef * rx * y1 / ry;
        let cy1 = -coef * ry * x1 / rx;
        let cx = cos_p * cx1 - sin_p * cy1 + (from[0] + to[0]) * 0.5;
        let cy = sin_p * cx1 + cos_p * cy1 + (from[1] + to[1]) * 0.5;
        let angle = |x: f32, y: f32| (y.atan2(x)).to_degrees();
        let theta1 = angle((x1 - cx1) / rx, (y1 - cy1) / ry).to_radians();
        let theta2 = angle((-x1 - cx1) / rx, (-y1 - cy1) / ry).to_radians();
        let mut delta = theta2 - theta1;
        if !sweep && delta > 0.0 {
            delta -= std::f32::consts::TAU;
        } else if sweep && delta < 0.0 {
            delta += std::f32::consts::TAU;
        }

        if self.current.is_empty() {
            self.current.push(from);
        }
        let segments = (delta.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
        let step = delta / segments as f32;
        // Round the step so the arc lands exactly on its endpoint.
        let mut t0 = theta1;
        for i in 0..segments {
            let t1 = if i + 1 == segments {
                theta1 + delta
            } else {
                t0 + step
            };
            let k = (4.0 / 3.0) * ((t1 - t0) / 4.0).tan();
            let p0 = [t0.cos(), t0.sin()];
            let p1 = [t1.cos(), t1.sin()];
            let c1 = [p0[0] - k * p0[1], p0[1] + k * p0[0]];
            let c2 = [p1[0] + k * p1[1], p1[1] - k * p1[0]];
            let map = |q: [f32; 2]| {
                [
                    cx + rx * (cos_p * q[0] - sin_p * q[1]),
                    cy + ry * (sin_p * q[0] + cos_p * q[1]),
                ]
            };
            self.cubic_to(map(c1), map(c2), map(p1));
            t0 = t1;
        }
        self.cursor = to;
        self.last_ctrl = None;
    }

    fn finish_subpath(&mut self, close: bool) {
        if self.current.len() >= 2 {
            let mut path = std::mem::take(&mut self.current);
            if close
                && let (Some(first), Some(last)) = (path.first().copied(), path.last().copied())
                && (first[0] - last[0]).abs() < 1e-6
                && (first[1] - last[1]).abs() < 1e-6
            {
                path.pop();
            }
            if path.len() >= 2 {
                self.subpaths.push(path);
            }
        } else {
            self.current.clear();
        }
    }

    // -- lexing ------------------------------------------------------------

    fn skip_separators(&mut self) {
        while self.at < self.bytes.len() {
            let b = self.bytes[self.at];
            if b == b',' || b.is_ascii_whitespace() {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    fn point(&mut self, base: [f32; 2]) -> Option<[f32; 2]> {
        let x = self.number()?;
        let y = self.number()?;
        Some([base[0] + x, base[1] + y])
    }

    fn flag(&mut self) -> Option<bool> {
        self.skip_separators();
        match self.bytes.get(self.at) {
            // Arc flags are written without separators: "a1 1 0 011 1".
            Some(b'0') => {
                self.at += 1;
                Some(false)
            }
            Some(b'1') => {
                self.at += 1;
                Some(true)
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let start = self.at;
        if matches!(self.bytes.get(self.at), Some(b'+') | Some(b'-')) {
            self.at += 1;
        }
        let mut digits = false;
        while matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
            self.at += 1;
            digits = true;
        }
        if matches!(self.bytes.get(self.at), Some(b'.')) {
            self.at += 1;
            while matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
                self.at += 1;
                digits = true;
            }
        }
        if !digits {
            return None;
        }
        if matches!(self.bytes.get(self.at), Some(b'e') | Some(b'E')) {
            let save = self.at;
            self.at += 1;
            if matches!(self.bytes.get(self.at), Some(b'+') | Some(b'-')) {
                self.at += 1;
            }
            let mut exp_digits = false;
            while matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
                self.at += 1;
                exp_digits = true;
            }
            if !exp_digits {
                self.at = save;
            }
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()?
            .parse()
            .ok()
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
}

fn cubic_at(p0: [f32; 2], c1: [f32; 2], c2: [f32; 2], p1: [f32; 2], t: f32) -> [f32; 2] {
    let mt = 1.0 - t;
    let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
    [
        a * p0[0] + b * c1[0] + c * c2[0] + d * p1[0],
        a * p0[1] + b * c1[1] + c * c2[1] + d * p1[1],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_ids() -> Vec<IconId> {
        use IconId::*;
        vec![
            GitHubInfo,
            GitHubBulb,
            GitHubReport,
            GitHubAlert,
            GitHubStop,
            ObsidianPencil,
            ObsidianInfo,
            ObsidianFlame,
            ObsidianAlert,
            ObsidianZap,
            ObsidianOctagon,
            ObsidianChevron,
            DocusaurusInfo,
            DocusaurusTip,
            DocusaurusNote,
            DocusaurusDanger,
        ]
    }

    #[test]
    fn every_vendored_icon_produces_drawable_geometry() {
        for id in all_ids() {
            let g = geometry(id);
            let prims = g.triangles.len() / 3 + g.polylines.len() + g.circles.len() + g.dots.len();
            assert!(prims > 0, "{id:?} produced nothing to draw");
            assert!(
                g.triangles.iter().chain(g.polylines.iter().flatten()).all(|p| {
                    p[0].is_finite()
                        && p[1].is_finite()
                        && p[0] >= -1.0
                        && p[1] >= -1.0
                        && p[0] <= g.view[0] + 1.0
                        && p[1] <= g.view[1] + 1.0
                }),
                "{id:?} has points outside its viewBox"
            );
        }
    }

    #[test]
    fn filled_icons_are_triangles_and_outline_icons_are_strokes() {
        // Octicons are filled paths; lucide is stroke-only. Getting this backwards
        // would render a hollow GitHub icon or a filled blob for Obsidian.
        let github = geometry(IconId::GitHubInfo);
        assert!(!github.triangles.is_empty());
        assert!(github.polylines.is_empty());

        let obsidian = geometry(IconId::ObsidianInfo);
        assert!(obsidian.triangles.is_empty());
        assert!(!obsidian.polylines.is_empty());
        assert_eq!(obsidian.circles.len(), 1, "the info circle");
    }

    #[test]
    fn a_ring_keeps_its_hole() {
        // The octicon `info` is a ring: outer circle one way, inner circle the
        // other. If the winding rule were ignored the centre would fill in, which
        // is exactly the "homemade icon" look this work is fixing. The probe is
        // off-centre because the lower-case "i" sits in the middle of the glyph.
        let g = geometry(IconId::GitHubInfo);
        assert!(
            !point_coverage(&g.triangles, [5.5, 5.5]),
            "the ring's centre must stay empty"
        );
        assert!(
            point_coverage(&g.triangles, [8.0, 1.2]),
            "the ring itself must be filled"
        );
    }

    #[test]
    fn even_odd_and_nonzero_disagree_on_overlapping_same_wound_shapes() {
        // Two concentric squares wound the same way: even-odd leaves a hole,
        // nonzero does not. Docusaurus paths depend on this distinction.
        let outer = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let inner = vec![[3.0, 3.0], [7.0, 3.0], [7.0, 7.0], [3.0, 7.0]];
        let nonzero = fill_triangles(&[outer.clone(), inner.clone()], FillRule::NonZero);
        let even_odd = fill_triangles(&[outer, inner], FillRule::EvenOdd);
        assert!(point_coverage(&nonzero, [5.0, 5.0]));
        assert!(!point_coverage(&even_odd, [5.0, 5.0]));
    }

    #[test]
    fn a_curve_is_flattened_into_enough_points_to_look_round() {
        let subpaths = flatten("M0 0 C0 10 10 10 10 0 Z", 0.05);
        assert_eq!(subpaths.len(), 1);
        assert!(
            subpaths[0].len() >= 8,
            "a cubic got only {} points",
            subpaths[0].len()
        );
    }

    #[test]
    fn close_path_returns_to_where_it_started() {
        let subpaths = flatten("M2 2 L8 2 L8 8 L2 8 Z", 0.05);
        let first = subpaths[0].first().copied().unwrap();
        assert_eq!(first, [2.0, 2.0]);
        // `Z` closes the loop without duplicating the start point.
        assert!(subpaths[0].len() == 4, "{:?}", subpaths[0]);
    }

    #[test]
    fn relative_commands_are_accumulated_from_the_cursor() {
        let subpaths = flatten("m1 1 h4 v4 h-4 z", 0.05);
        let xs: Vec<f32> = subpaths[0].iter().map(|p| p[0]).collect();
        let ys: Vec<f32> = subpaths[0].iter().map(|p| p[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::MIN, f32::max), 5.0);
        assert_eq!(ys.iter().cloned().fold(f32::MIN, f32::max), 5.0);
    }

    #[test]
    fn packed_arc_flags_are_read_the_svg_way() {
        // "a1 1 0 011 1" — the two flags are single digits with no separators.
        let subpaths = flatten("M0 5 a5 5 0 0110 0 z", 0.05);
        assert_eq!(subpaths.len(), 1);
        let max_x = subpaths[0].iter().map(|p| p[0]).fold(f32::MIN, f32::max);
        assert!((max_x - 10.0).abs() < 0.2, "arc ended at x={max_x}");
    }

    #[test]
    fn a_degenerate_stroke_becomes_a_dot() {
        // lucide's "i" cap is `h.01`: a zero-length line. Drawn as a line it
        // disappears; it has to become a dot.
        let g = geometry(IconId::ObsidianInfo);
        assert!(!g.dots.is_empty(), "the dot on the i went missing");
    }

    #[test]
    fn malformed_paths_do_not_panic_or_hang() {
        for d in [
            "",
            "M",
            "M1",
            "Z Z Z",
            "L1 2",
            "M0 0 C",
            "M0 0 A1 1 0 9 9 5 5",
            "@@@",
            "M0 0 LNaN 2",
        ] {
            let _ = flatten(d, 0.05);
        }
    }

    /// Writes every icon's flattened geometry to `%TEMP%\mdpad-icons.txt`.
    ///
    /// Ignored by default: it produces no assertion, it exists so the icons can
    /// be rasterised outside the build and looked at. Run with
    /// `cargo test dump_icon_geometry -- --ignored`.
    #[test]
    #[ignore]
    fn dump_icon_geometry() {
        let mut out = String::new();
        for id in all_ids() {
            let g = geometry(id);
            out.push_str(&format!(
                "ICON {:?} {} {}\n",
                id, g.view[0], g.view[1]
            ));
            for tri in g.triangles.chunks_exact(3) {
                out.push_str(&format!(
                    "T {} {} {} {} {} {}\n",
                    tri[0][0], tri[0][1], tri[1][0], tri[1][1], tri[2][0], tri[2][1]
                ));
            }
            for line in &g.polylines {
                out.push('L');
                for p in line {
                    out.push_str(&format!(" {} {}", p[0], p[1]));
                }
                out.push('\n');
            }
            for (c, r) in &g.circles {
                out.push_str(&format!("C {} {} {}\n", c[0], c[1], r));
            }
            for d in &g.dots {
                out.push_str(&format!("D {} {}\n", d[0], d[1]));
            }
            out.push_str(&format!("STROKE {}\nEND\n", g.stroke));
        }
        let path = std::env::temp_dir().join("mdpad-icons.txt");
        std::fs::write(&path, out).expect("write icon dump");
        eprintln!("wrote {}", path.display());
    }

    /// True when `p` is inside any of the triangles.
    fn point_coverage(tris: &[[f32; 2]], p: [f32; 2]) -> bool {
        tris.chunks_exact(3).any(|t| inside(t, p))
    }

    fn inside(t: &[[f32; 2]], p: [f32; 2]) -> bool {
        let d = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
            (a[0] - c[0]) * (b[1] - c[1]) - (b[0] - c[0]) * (a[1] - c[1])
        };
        let d1 = d(p, t[0], t[1]);
        let d2 = d(p, t[1], t[2]);
        let d3 = d(p, t[2], t[0]);
        let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(neg && pos)
    }
}
