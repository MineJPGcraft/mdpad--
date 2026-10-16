//! Renderer-agnostic 2D primitives and text measurement.
//!
//! Every visual engine in MarkdownPad (Markdown blocks, the LaTeX typesetter,
//! the Mermaid and PlantUML engines) emits [`Prim`] values instead of talking
//! to `egui::Painter` directly. The single translation point into egui lives in
//! [`crate::gfx_egui`].
//!
//! Keeping this file free of egui types means the engines are plain data
//! transformations: they can be reasoned about, unit-tested and laid out
//! without a running GUI context. Text metrics are injected through the
//! [`Measure`] trait, so layout code never touches a font atlas.

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// Straight (non-premultiplied) 8-bit RGBA colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255, 255);
    pub const BLACK: Self = Self::new(0, 0, 0, 255);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Build from a `0xRRGGBB` literal at full opacity.
    pub const fn hex(rgb: u32) -> Self {
        Self::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255)
    }

    pub fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Multiply existing alpha by `factor` (0.0..=1.0).
    pub fn fade(self, factor: f32) -> Self {
        let a = (self.a as f32 * factor.clamp(0.0, 1.0)).round() as u8;
        self.with_alpha(a)
    }

    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Self::new(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
            mix(self.a, other.a),
        )
    }

    pub fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Perceived luminance in 0.0..=1.0, used to pick readable label colours.
    pub fn luminance(self) -> f32 {
        (0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32) / 255.0
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Axis-aligned rectangle in local (diagram/engine) coordinates.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl Rect {
    pub const ZERO: Self = Self {
        min: [0.0, 0.0],
        max: [0.0, 0.0],
    };

    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            min: [x0.min(x1), y0.min(y1)],
            max: [x0.max(x1), y0.max(y1)],
        }
    }

    pub fn from_min_size(min: [f32; 2], size: [f32; 2]) -> Self {
        Self {
            min,
            max: [min[0] + size[0].max(0.0), min[1] + size[1].max(0.0)],
        }
    }

    pub fn from_center_size(center: [f32; 2], size: [f32; 2]) -> Self {
        Self::from_min_size(
            [center[0] - size[0] * 0.5, center[1] - size[1] * 0.5],
            size,
        )
    }

    pub fn from_xywh(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self::from_min_size([x, y], [w, h])
    }

    pub fn width(&self) -> f32 {
        self.max[0] - self.min[0]
    }

    pub fn height(&self) -> f32 {
        self.max[1] - self.min[1]
    }

    pub fn size(&self) -> [f32; 2] {
        [self.width(), self.height()]
    }

    pub fn center(&self) -> [f32; 2] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }

    pub fn translated(&self, d: [f32; 2]) -> Self {
        Self {
            min: [self.min[0] + d[0], self.min[1] + d[1]],
            max: [self.max[0] + d[0], self.max[1] + d[1]],
        }
    }

    pub fn expand(&self, by: f32) -> Self {
        Self {
            min: [self.min[0] - by, self.min[1] - by],
            max: [self.max[0] + by, self.max[1] + by],
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        Self {
            min: [self.min[0].min(other.min[0]), self.min[1].min(other.min[1])],
            max: [self.max[0].max(other.max[0]), self.max[1].max(other.max[1])],
        }
    }

    pub fn contains(&self, p: [f32; 2]) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0] && p[1] >= self.min[1] && p[1] <= self.max[1]
    }

    /// Edge midpoints, in the order used by the graph layout for edge routing:
    /// `[top, right, bottom, left]`.
    pub fn edge_points(&self) -> [[f32; 2]; 4] {
        let c = self.center();
        [
            [c[0], self.min[1]],
            [self.max[0], c[1]],
            [c[0], self.max[1]],
            [self.min[0], c[1]],
        ]
    }
}

// ---------------------------------------------------------------------------
// Text styling
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Sans,
    Serif,
    Mono,
    /// Math font used by the LaTeX typesetter (upright symbols, radicals, ...).
    Math,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weight {
    Regular,
    Bold,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slant {
    Upright,
    Italic,
}

/// A fully resolved font request.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Font {
    pub size: f32,
    pub family: Family,
    pub weight: Weight,
    pub slant: Slant,
}

impl Font {
    pub fn new(size: f32) -> Self {
        Self {
            size,
            family: Family::Sans,
            weight: Weight::Regular,
            slant: Slant::Upright,
        }
    }

    pub fn with_family(mut self, family: Family) -> Self {
        self.family = family;
        self
    }

    pub fn bold(mut self) -> Self {
        self.weight = Weight::Bold;
        self
    }

    pub fn italic(mut self) -> Self {
        self.slant = Slant::Italic;
        self
    }

    pub fn mono(size: f32) -> Self {
        Self::new(size).with_family(Family::Mono)
    }

    pub fn math(size: f32) -> Self {
        Self::new(size).with_family(Family::Math)
    }

    pub fn scaled(mut self, factor: f32) -> Self {
        self.size *= factor;
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Anchor {
    LeftTop,
    CenterTop,
    RightTop,
    LeftCenter,
    Center,
    RightCenter,
    LeftBottom,
    CenterBottom,
    RightBottom,
}

impl Anchor {
    /// Where the anchor point sits inside the text box, as a 0..=1 factor.
    /// `[0, 0]` is the text box's top-left corner.
    pub fn factor(self) -> [f32; 2] {
        match self {
            Self::LeftTop => [0.0, 0.0],
            Self::CenterTop => [0.5, 0.0],
            Self::RightTop => [1.0, 0.0],
            Self::LeftCenter => [0.0, 0.5],
            Self::Center => [0.5, 0.5],
            Self::RightCenter => [1.0, 0.5],
            Self::LeftBottom => [0.0, 1.0],
            Self::CenterBottom => [0.5, 1.0],
            Self::RightBottom => [1.0, 1.0],
        }
    }
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stroke {
    pub width: f32,
    pub color: Rgba,
}

impl Stroke {
    pub fn new(width: f32, color: Rgba) -> Self {
        Self { width, color }
    }
}

/// A single drawing command in local coordinates.
#[derive(Clone, Debug)]
pub enum Prim {
    Rect {
        rect: Rect,
        radius: f32,
        fill: Option<Rgba>,
        stroke: Option<Stroke>,
    },
    Circle {
        center: [f32; 2],
        radius: f32,
        fill: Option<Rgba>,
        stroke: Option<Stroke>,
    },
    Ellipse {
        center: [f32; 2],
        radii: [f32; 2],
        fill: Option<Rgba>,
        stroke: Option<Stroke>,
    },
    Line {
        from: [f32; 2],
        to: [f32; 2],
        stroke: Stroke,
    },
    Path {
        points: Vec<[f32; 2]>,
        closed: bool,
        fill: Option<Rgba>,
        stroke: Option<Stroke>,
    },
    Text {
        /// Anchor position: where the text's [`Anchor`] point is placed.
        pos: [f32; 2],
        text: String,
        font: Font,
        color: Rgba,
        anchor: Anchor,
    },
}

impl Prim {
    pub fn rect(rect: Rect, radius: f32, fill: Option<Rgba>, stroke: Option<Stroke>) -> Self {
        Self::Rect {
            rect,
            radius,
            fill,
            stroke,
        }
    }

    pub fn filled_rect(rect: Rect, radius: f32, fill: Rgba) -> Self {
        Self::rect(rect, radius, Some(fill), None)
    }

    pub fn stroked_rect(rect: Rect, radius: f32, stroke: Stroke) -> Self {
        Self::rect(rect, radius, None, Some(stroke))
    }

    pub fn circle(center: [f32; 2], radius: f32, fill: Option<Rgba>, stroke: Option<Stroke>) -> Self {
        Self::Circle {
            center,
            radius,
            fill,
            stroke,
        }
    }

    pub fn ellipse(
        center: [f32; 2],
        radii: [f32; 2],
        fill: Option<Rgba>,
        stroke: Option<Stroke>,
    ) -> Self {
        Self::Ellipse {
            center,
            radii,
            fill,
            stroke,
        }
    }

    pub fn line(from: [f32; 2], to: [f32; 2], stroke: Stroke) -> Self {
        Self::Line { from, to, stroke }
    }

    pub fn path(points: Vec<[f32; 2]>, closed: bool, fill: Option<Rgba>, stroke: Option<Stroke>) -> Self {
        Self::Path {
            points,
            closed,
            fill,
            stroke,
        }
    }

    pub fn text(pos: [f32; 2], text: impl Into<String>, font: Font, color: Rgba) -> Self {
        Self::Text {
            pos,
            text: text.into(),
            font,
            color,
            anchor: Anchor::LeftCenter,
        }
    }

    /// Convenience: horizontally centred label at `pos`.
    pub fn label_center(pos: [f32; 2], text: impl Into<String>, font: Font, color: Rgba) -> Self {
        Self::Text {
            pos,
            text: text.into(),
            font,
            color,
            anchor: Anchor::Center,
        }
    }

    pub fn anchored_text(
        pos: [f32; 2],
        text: impl Into<String>,
        font: Font,
        color: Rgba,
        anchor: Anchor,
    ) -> Self {
        Self::Text {
            pos,
            text: text.into(),
            font,
            color,
            anchor,
        }
    }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// Text metrics provider.
///
/// Implemented by [`crate::gfx_egui::EguiMeasure`] against the live egui font
/// atlas, and by [`ApproxMeasure`] for headless use and unit tests.
pub trait Measure {
    /// Advance width and line height of `text` when laid out on a single line.
    fn measure(&self, text: &str, font: Font) -> [f32; 2];

    /// Distance from the top of a line box down to the baseline.
    fn ascent(&self, font: Font) -> f32 {
        font.size * 0.80
    }

    /// Distance from the baseline to the bottom of a line box.
    fn descent(&self, font: Font) -> f32 {
        font.size * 0.22
    }

    /// Full line height for a single line of `font`.
    fn line_height(&self, font: Font) -> f32 {
        self.ascent(font) + self.descent(font)
    }

    /// Greedy word wrap. Returns the lines together with their widths.
    /// `max_width <= 0` disables wrapping.
    fn wrap(&self, text: &str, font: Font, max_width: f32) -> Vec<(String, f32)> {
        if max_width <= 0.0 || text.is_empty() {
            let w = self.measure(text, font)[0];
            return vec![(text.to_owned(), w)];
        }

        let mut out: Vec<(String, f32)> = Vec::new();
        let mut line = String::new();
        for word in text.split_inclusive(' ') {
            let candidate_len = line.len() + word.len();
            let candidate = if line.is_empty() {
                word.trim_end().to_owned()
            } else {
                format!("{}{}", line, word).trim_end().to_owned()
            };
            let _ = candidate_len;
            if !line.is_empty() && self.measure(&candidate, font)[0] > max_width {
                let w = self.measure(&line, font)[0];
                out.push((std::mem::take(&mut line).trim_end().to_owned(), w));
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            let trimmed = line.trim_end().to_owned();
            let w = self.measure(&trimmed, font)[0];
            out.push((trimmed, w));
        }
        if out.is_empty() {
            out.push((String::new(), 0.0));
        }
        out
    }
}

/// Font-metric estimator with no font atlas.
///
/// Widths are approximated from average glyph advance per script class, which
/// is accurate enough for diagram layout and keeps this file dependency-free.
pub struct ApproxMeasure;

impl Measure for ApproxMeasure {
    fn measure(&self, text: &str, font: Font) -> [f32; 2] {
        let adv = match font.family {
            Family::Mono => 0.60,
            Family::Serif => 0.50,
            Family::Math => 0.54,
            Family::Sans => 0.52,
        };
        let mut units = 0.0f32;
        for ch in text.chars() {
            units += if ch.is_ascii() {
                adv
            } else if is_wide(ch) {
                1.0
            } else {
                0.62
            };
        }
        let weight = if font.weight == Weight::Bold { 1.04 } else { 1.0 };
        [
            (units * font.size * weight).max(0.0),
            font.size * 1.25,
        ]
    }

    fn ascent(&self, font: Font) -> f32 {
        font.size * 0.80
    }

    fn descent(&self, font: Font) -> f32 {
        font.size * 0.22
    }
}

/// Coarse test for CJK / full-width glyphs, which occupy roughly one em.
pub fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_helpers_are_normalised() {
        let r = Rect::new(10.0, 10.0, 0.0, 4.0);
        assert_eq!(r.min, [0.0, 4.0]);
        assert_eq!(r.max, [10.0, 10.0]);
        assert_eq!(r.size(), [10.0, 6.0]);
        assert_eq!(r.center(), [5.0, 7.0]);
    }

    #[test]
    fn approx_wrap_respects_width() {
        let m = ApproxMeasure;
        let font = Font::new(10.0);
        let lines = m.wrap("alpha beta gamma delta", font, 40.0);
        assert!(lines.len() > 1, "expected wrapping, got {lines:?}");
        for (_, w) in &lines {
            assert!(*w <= 40.0 + 0.001, "line too wide: {w}");
        }
    }

    #[test]
    fn wide_glyphs_are_wider_than_ascii() {
        let m = ApproxMeasure;
        let font = Font::new(10.0);
        let latin = m.measure("ab", font)[0];
        let cjk = m.measure("\u{4E2D}\u{6587}", font)[0];
        assert!(cjk > latin);
    }
}
