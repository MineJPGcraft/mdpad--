//! The single bridge from engine primitives to egui.
//!
//! Everything below this line is plain data; everything above it is egui. Keeping
//! the translation in one file means the LaTeX typesetter, the diagram engines and
//! the Markdown renderer can share one painter without any of them depending on
//! the GUI framework.

use egui::{Color32, Painter, Pos2, Rect as ERect, Stroke as EStroke, Vec2};

use crate::fonts;
use crate::gfx::{Anchor, Font, Measure, Prim, Rect as FRect, Rgba};

/// [`Measure`] backed by egui's font atlas and galley cache.
pub struct EguiMeasure<'a> {
    pub ctx: &'a egui::Context,
}

impl<'a> EguiMeasure<'a> {
    pub fn new(ctx: &'a egui::Context) -> Self {
        Self { ctx }
    }

    /// Lays out a single unwrapped line.
    fn galley(&self, text: &str, font: Font) -> std::sync::Arc<egui::Galley> {
        let font_id = fonts::font_id(font);
        self.ctx
            .fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font_id, Color32::WHITE))
    }
}

impl Measure for EguiMeasure<'_> {
    fn measure(&self, text: &str, font: Font) -> [f32; 2] {
        if text.is_empty() {
            // Still report a sensible line box so empty runs do not collapse
            // vertically and break baseline alignment.
            return [0.0, font.size * 1.25];
        }
        let g = self.galley(text, font);
        let s = g.size();
        [s.x, s.y.max(font.size * 1.05)]
    }

    fn ascent(&self, font: Font) -> f32 {
        // egui does not expose the glyph baseline for a laid-out row in a stable
        // way, so mirror the usual Latin ascent/decent ratio instead. Using one
        // consistent value keeps mixed-font rows aligned.
        font.size * 0.82
    }

    fn descent(&self, font: Font) -> f32 {
        (self.measure("Hg", font)[1] - self.ascent(font)).max(font.size * 0.2)
    }
}

// ---------------------------------------------------------------------------
// Coordinate helpers
// ---------------------------------------------------------------------------

/// Local point -> screen point.
pub fn to_pos(p: [f32; 2], origin: Pos2) -> Pos2 {
    Pos2::new(origin.x + p[0], origin.y + p[1])
}

/// Local rect -> screen rect.
pub fn to_rect(r: &FRect, origin: Pos2) -> ERect {
    ERect::from_min_max(
        Pos2::new(origin.x + r.min[0], origin.y + r.min[1]),
        Pos2::new(origin.x + r.max[0], origin.y + r.max[1]),
    )
}

fn stroke_of(s: &crate::gfx::Stroke) -> EStroke {
    EStroke::new(s.width, s.color.to_egui())
}

/// Approximates an axis-aligned ellipse with a closed polygon.
///
/// Sampling avoids depending on egui's ellipse shape and keeps filled ellipses
/// (cylinder caps, pie charts) visually identical to stroked ones.
fn ellipse_points(center: [f32; 2], radii: [f32; 2], segments: usize) -> Vec<Pos2> {
    let segments = segments.clamp(12, 128);
    let (rx, ry) = (radii[0].abs(), radii[1].abs());
    (0..segments)
        .map(|i| {
            let t = std::f32::consts::TAU * (i as f32) / (segments as f32);
            let (s, c) = t.sin_cos();
            Pos2::new(center[0] + rx * c, center[1] + ry * s)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// Paints a primitive list, translated so local `(0, 0)` sits at `origin`.
pub fn paint(ctx: &egui::Context, painter: &Painter, origin: Pos2, prims: &[Prim]) {
    for prim in prims {
        paint_one(ctx, painter, origin, prim);
    }
}

/// Paints primitives using a caller-supplied clip rectangle.
///
/// Used by the editor so a diagram or a long code block cannot paint outside its
/// own block while the surrounding document scrolls.
pub fn paint_clipped(
    ctx: &egui::Context,
    painter: &Painter,
    origin: Pos2,
    clip: ERect,
    prims: &[Prim],
) {
    let child = painter.with_clip_rect(clip);
    for prim in prims {
        let _ = ctx;
        paint_one(ctx, &child, origin, prim);
    }
}

fn paint_one(ctx: &egui::Context, painter: &Painter, origin: Pos2, prim: &Prim) {
    // Guard against anything non-finite reaching the tessellator: a single NaN
    // would blank the whole frame.
    match prim {
        Prim::Rect {
            rect,
            radius,
            fill,
            stroke,
        } => {
            if !finite_rect(rect) {
                return;
            }
            let r = to_rect(rect, origin);
            if let Some(f) = fill {
                painter.rect_filled(r, *radius, f.to_egui());
            }
            if let Some(s) = stroke {
                painter.rect_stroke(
                    r,
                    *radius,
                    stroke_of(s),
                    egui::StrokeKind::Inside,
                );
            }
        }
        Prim::Circle {
            center,
            radius,
            fill,
            stroke,
        } => {
            if !finite_point(*center) || !radius.is_finite() {
                return;
            }
            let c = to_pos(*center, origin);
            if let Some(f) = fill {
                painter.circle_filled(c, *radius, f.to_egui());
            }
            if let Some(s) = stroke {
                painter.circle_stroke(c, *radius, stroke_of(s));
            }
        }
        Prim::Ellipse {
            center,
            radii,
            fill,
            stroke,
        } => {
            if !finite_point(*center) || !finite_point(*radii) {
                return;
            }
            let points = ellipse_points(*center, *radii, 48)
                .into_iter()
                .map(|p| p + origin.to_vec2())
                .collect::<Vec<_>>();
            let es = stroke
                .as_ref()
                .map(stroke_of)
                .unwrap_or(EStroke::NONE);
            let fill_c = fill.map(Rgba::to_egui).unwrap_or(Color32::TRANSPARENT);
            if points.len() >= 3 && (fill.is_some() || stroke.is_some()) {
                painter.add(egui::Shape::convex_polygon(points, fill_c, es));
            }
        }
        Prim::Line { from, to, stroke } => {
            if !finite_point(*from) || !finite_point(*to) {
                return;
            }
            painter.line_segment(
                [to_pos(*from, origin), to_pos(*to, origin)],
                stroke_of(stroke),
            );
        }
        Prim::Path {
            points,
            closed,
            fill,
            stroke,
        } => {
            if points.len() < 2 || points.iter().any(|p| !finite_point(*p)) {
                return;
            }
            let pts: Vec<Pos2> = points.iter().map(|p| to_pos(*p, origin)).collect();
            let es = stroke
                .as_ref()
                .map(stroke_of)
                .unwrap_or(EStroke::NONE);
            match (closed, fill) {
                (true, Some(f)) if pts.iter().all(|p| p.x.is_finite()) => {
                    painter.add(egui::Shape::convex_polygon(
                        pts,
                        f.to_egui(),
                        es,
                    ));
                }
                (true, _) => {
                    painter.add(egui::Shape::closed_line(pts, es));
                }
                (false, _) => {
                    painter.add(egui::Shape::line(pts, es));
                }
            }
        }
        Prim::Text {
            pos,
            text,
            font,
            color,
            anchor,
        } => {
            if text.is_empty() || !finite_point(*pos) {
                return;
            }
            let fmt = fonts::text_format(*font, color.to_egui());
            let mut job = egui::text::LayoutJob::default();
            job.append(text, 0.0, fmt);
            let galley = ctx.fonts_mut(|f| f.layout_job(job));
            let size = galley.size();
            let factor = anchor.factor();
            let top_left = origin + Vec2::new(pos[0], pos[1])
                - Vec2::new(size.x * factor[0], size.y * factor[1]);
            painter.galley(top_left, galley, color.to_egui());
        }
    }
}

fn finite_point(p: [f32; 2]) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

fn finite_rect(r: &FRect) -> bool {
    finite_point(r.min) && finite_point(r.max)
}

/// An anchor's 0..=1 factor, re-exported for layout code that needs it without
/// depending on `gfx` directly.
pub fn anchor_factor(anchor: Anchor) -> [f32; 2] {
    anchor.factor()
}

/// Paints an icon into `rect`, keeping its aspect ratio and centring it.
///
/// Icons are the one thing that reaches egui as a mesh rather than a primitive:
/// their silhouettes are concave (the "i" of `info`, the lightning of `zap`) and
/// some punch holes (the octicon ring), which no convex polygon can express.
/// Triangulation happens in [`crate::icon`], which knows nothing about egui.
pub fn icon(
    painter: &Painter,
    geo: &crate::icon::Geometry,
    rect: ERect,
    color: Color32,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || geo.view[0] <= 0.0 || geo.view[1] <= 0.0 {
        return;
    }
    let scale = (rect.width() / geo.view[0]).min(rect.height() / geo.view[1]);
    let drawn = Vec2::new(geo.view[0] * scale, geo.view[1] * scale);
    let origin = rect.center() - drawn * 0.5;
    let map = |p: [f32; 2]| Pos2::new(origin.x + p[0] * scale, origin.y + p[1] * scale);

    if geo.triangles.len() >= 3 {
        let mut mesh = egui::Mesh::default();
        for p in &geo.triangles {
            mesh.colored_vertex(map(*p), color);
        }
        for index in 0..(geo.triangles.len() / 3) as u32 {
            let base = index * 3;
            mesh.add_triangle(base, base + 1, base + 2);
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    // A hair of minimum width keeps outline icons visible at small sizes.
    let stroke = EStroke::new((geo.stroke * scale).max(0.7), color);
    for line in &geo.polylines {
        if line.len() >= 2 {
            let pts: Vec<Pos2> = line.iter().map(|p| map(*p)).collect();
            painter.add(egui::Shape::line(pts, stroke));
        }
    }
    for (centre, radius) in &geo.circles {
        painter.circle_stroke(map(*centre), radius * scale, stroke);
    }
    let dot = (geo.stroke * scale * 0.5).max(0.7);
    for centre in &geo.dots {
        painter.circle_filled(map(*centre), dot, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_to_screen_translation() {
        let origin = Pos2::new(10.0, 20.0);
        assert_eq!(to_pos([1.0, 2.0], origin), Pos2::new(11.0, 22.0));
        let r = FRect::from_xywh(0.0, 0.0, 4.0, 6.0);
        let sr = to_rect(&r, origin);
        assert_eq!(sr.min, Pos2::new(10.0, 20.0));
        assert_eq!(sr.max, Pos2::new(14.0, 26.0));
    }

    #[test]
    fn ellipse_sampling_is_bounded_and_closed_enough() {
        let pts = ellipse_points([0.0, 0.0], [10.0, 5.0], 8);
        assert_eq!(pts.len(), 12, "segment count is clamped to a minimum of 12");
        for p in &pts {
            assert!(p.x.is_finite() && p.y.is_finite());
            assert!(p.x.abs() <= 10.001 && p.y.abs() <= 5.001);
        }
    }

    /// Runs a headless pass and discards the texture deltas it produced.
    fn pass(ctx: &egui::Context, body: impl FnMut(&egui::Context)) {
        let output = ctx.run_pass(egui::RawInput::default(), body);
        output.drop_without_applying_deltas();
    }

    #[test]
    fn measuring_within_a_pass_works_and_scales_with_size() {
        let ctx = egui::Context::default();
        pass(&ctx, |ctx| {
            let m = EguiMeasure::new(ctx);
            let small = m.measure("hello", Font::new(10.0))[0];
            let large = m.measure("hello", Font::new(20.0))[0];
            assert!(small > 0.0);
            assert!(large > small * 1.8, "width did not scale with the font size");
            assert!(m.ascent(Font::new(16.0)) > 0.0);
            assert!(m.descent(Font::new(16.0)) > 0.0);
        });
    }

    #[test]
    fn painting_never_panics_on_hostile_input() {
        let ctx = egui::Context::default();
        pass(&ctx, |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::unique("test"),
            ));
            let prims = vec![
                Prim::line(
                    [0.0, 0.0],
                    [f32::NAN, 1.0],
                    crate::gfx::Stroke::new(1.0, Rgba::WHITE),
                ),
                Prim::circle([f32::INFINITY, 0.0], 4.0, Some(Rgba::WHITE), None),
                Prim::path(vec![[0.0, 0.0]], true, Some(Rgba::WHITE), None),
                Prim::text([0.0, 0.0], "", Font::new(12.0), Rgba::WHITE),
            ];
            paint(ctx, &painter, Pos2::ZERO, &prims);
        });
    }
}
