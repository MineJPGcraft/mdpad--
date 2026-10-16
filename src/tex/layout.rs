//! Baseline-driven layout of the math AST into renderer-agnostic primitives.
//!
//! Every sub-formula is laid out as a *box* of `(width, ascent, descent)`
//! measured against the expression baseline, exactly like TeX's horizontal and
//! vertical lists. Boxes are stacked horizontally on a shared baseline and
//! centred vertically where the construct demands it (fractions, limits,
//! delimiters). Primitives are produced in a local frame whose origin is the
//! *baseline* of the outermost row; [`render`] then translates everything so
//! the union bounding box starts at `(0, 0)` and reports the baseline offset.
//!
//! All glyph metrics come from the injected [`Measure`], so the same code runs
//! against the live egui font atlas and against [`ApproxMeasure`] in tests.

use super::parse::{
    AccentKind, ArraySpec, Class, ColAlign, Delim, EnvKind, FracStyle, LimitMode, Node, OpKind,
    SizeStyle, SymStyle, TextStyle,
};
use crate::gfx::{Anchor, Family, Font, Measure, Prim, Rect, Rgba, Slant, Stroke, Weight};

// ---------------------------------------------------------------------------
// Public result
// ---------------------------------------------------------------------------

/// Result of laying out one math expression.
#[derive(Clone, Debug, Default)]
pub struct MathRender {
    /// Drawing primitives. The math box's top-left corner is at (0, 0).
    pub prims: Vec<Prim>,
    /// Bounding box of the whole expression, in points.
    pub size: [f32; 2],
    /// Distance from the top of the box down to the baseline of the outermost row.
    pub baseline: f32,
    /// Non-fatal problems (unknown commands, etc.).
    pub warnings: Vec<String>,
    /// Set when the input could not be interpreted at all; `prims` then contains
    /// a plain fallback rendering of the raw source.
    pub error: Option<String>,
}

/// Lay out a LaTeX math expression. Never panics.
///
/// `base_size` is the em size in points for the outermost level; a non-finite
/// or non-positive value falls back to 12 pt. Surrounding `$...$`, `$$...$$`,
/// `\(...\)` and `\[...\]` delimiters are stripped, and the `$$` / `\[` forms
/// select display style (limits above operators instead of beside them).
pub fn render(src: &str, base_size: f32, color: Rgba, m: &dyn Measure) -> MathRender {
    let base = if base_size.is_finite() && base_size > 0.0 {
        base_size
    } else {
        12.0
    };

    let (body, display) = strip_delims(src);
    let parsed = super::parse::parse(super::parse::tokenize(body));

    let mut ctx = Ctx {
        m,
        color,
        warnings: parsed.warnings,
        display,
    };

    let mut error: Option<String> = None;
    let has_content = !body.trim().is_empty();
    let sb = if is_blank(&parsed.root) && has_content {
        error = Some("无法解析该数学表达式，已按原样显示".to_owned());
        let font = Font {
            size: base,
            family: Family::Math,
            weight: Weight::Regular,
            slant: Slant::Upright,
        };
        text_box(body.trim(), font, color, m)
    } else {
        layout(&parsed.root, base, 0, &mut ctx)
    };
    let warnings = ctx.warnings;

    let mut prims = sb.prims;
    prims.retain(prim_is_finite);

    let (size, baseline) = match prims_bounds(&prims, m) {
        Some(r) => {
            let d = [-r.min[0], -r.min[1]];
            for p in prims.iter_mut() {
                translate_prim(p, d);
            }
            let w = san((r.max[0] - r.min[0]).max(0.0));
            let h = san((r.max[1] - r.min[1]).max(0.0));
            ([w, h], san(d[1]).clamp(0.0, h))
        }
        None => {
            let w = san(sb.w.max(0.0));
            let h = san((sb.ascent + sb.descent).max(0.0));
            ([w, h], san(sb.ascent).clamp(0.0, h))
        }
    };

    MathRender {
        prims,
        size,
        baseline,
        warnings,
        error,
    }
}

// ---------------------------------------------------------------------------
// Layout context
// ---------------------------------------------------------------------------

/// TeX's thin / medium / thick math spaces, in em.
const THIN: f32 = 0.1667;
const MED: f32 = 0.2222;
const THICK: f32 = 0.2778;

/// Height of the math axis above the baseline, in em.
const AXIS: f32 = 0.25;

/// Script size factors for level 1 and level 2+.
const SCRIPT_L1: f32 = 0.72;
const SCRIPT_L2: f32 = 0.55;

struct Ctx<'a> {
    m: &'a dyn Measure,
    color: Rgba,
    warnings: Vec<String>,
    /// `true` while display style is active; drives limit placement, fraction
    /// sizing and the size of large operators.
    display: bool,
}

impl Ctx<'_> {
    fn warn(&mut self, msg: String) {
        if self.warnings.contains(&msg) {
            return;
        }
        self.warnings.push(msg);
    }
}

/// A laid-out sub-formula: horizontal extent plus the two vertical extents
/// measured from the shared baseline. `prims` use local coordinates with the
/// baseline at `y = 0` and the left edge at `x = 0`.
#[derive(Clone, Debug, Default)]
struct Sb {
    w: f32,
    ascent: f32,
    descent: f32,
    prims: Vec<Prim>,
}

impl Sb {
    fn empty() -> Self {
        Self::default()
    }

    fn pure(w: f32) -> Self {
        Self {
            w: w.max(0.0),
            ..Self::default()
        }
    }
}

/// Append `child` to `target`, offset by `(dx, dy)`.
fn place(target: &mut Sb, child: &Sb, dx: f32, dy: f32) {
    for p in &child.prims {
        let mut p = p.clone();
        translate_prim(&mut p, [dx, dy]);
        target.prims.push(p);
    }
}

fn translate_prim(p: &mut Prim, d: [f32; 2]) {
    match p {
        Prim::Rect { rect, .. } => {
            rect.min[0] += d[0];
            rect.min[1] += d[1];
            rect.max[0] += d[0];
            rect.max[1] += d[1];
        }
        Prim::Circle { center, .. } => {
            center[0] += d[0];
            center[1] += d[1];
        }
        Prim::Ellipse { center, .. } => {
            center[0] += d[0];
            center[1] += d[1];
        }
        Prim::Line { from, to, .. } => {
            from[0] += d[0];
            from[1] += d[1];
            to[0] += d[0];
            to[1] += d[1];
        }
        Prim::Path { points, .. } => {
            for q in points.iter_mut() {
                q[0] += d[0];
                q[1] += d[1];
            }
        }
        Prim::Text { pos, .. } => {
            pos[0] += d[0];
            pos[1] += d[1];
        }
    }
}

// ---------------------------------------------------------------------------
// Text metrics
// ---------------------------------------------------------------------------

fn san(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

fn finite(x: f32) -> bool {
    x.is_finite()
}

/// `(width, ascent, descent)` of a single-line text run.
///
/// The descent is widened to cover the measured line height so the reported box
/// really contains the glyphs; renderers generally return a line height a little
/// taller than `ascent + descent`.
fn text_metrics(text: &str, font: Font, m: &dyn Measure) -> (f32, f32, f32) {
    let measured = m.measure(text, font);
    let w = san(measured[0]).max(0.0);
    let asc = san(m.ascent(font)).max(0.0);
    let lh = san(measured[1]).max(asc);
    (w, asc, (lh - asc).max(0.0))
}

/// A text run as an [`Sb`], anchored by its top-left corner so the shared
/// baseline lands exactly on `y = 0`.
fn text_box(text: &str, font: Font, color: Rgba, m: &dyn Measure) -> Sb {
    let (w, asc, desc) = text_metrics(text, font, m);
    Sb {
        w,
        ascent: asc,
        descent: desc,
        prims: vec![Prim::anchored_text(
            [0.0, -asc],
            text,
            font,
            color,
            Anchor::LeftTop,
        )],
    }
}

fn upright_math(size: f32) -> Font {
    Font {
        size,
        family: Family::Math,
        weight: Weight::Regular,
        slant: Slant::Upright,
    }
}

// ---------------------------------------------------------------------------
// Atom classes and spacing
// ---------------------------------------------------------------------------

fn class_of(node: &Node) -> Class {
    match node {
        Node::Sym { class, .. } | Node::Text { class, .. } => *class,
        Node::Op { .. } => Class::Op,
        Node::BigDelim { class, .. } => *class,
        Node::Scripts { base, .. } => class_of(base),
        Node::Style { body, .. } => class_of(body),
        Node::Delimited { .. } | Node::Environ { .. } | Node::Row(_) => Class::Inner,
        _ => Class::Ord,
    }
}

/// Convert a binary operator into an ordinary atom when it is really unary:
/// nothing, an operator, a relation or an opening delimiter precedes it.
fn effective_class(prev: Option<Class>, class: Class) -> Class {
    if class == Class::Bin
        && !matches!(prev, Some(Class::Ord) | Some(Class::Inner) | Some(Class::Close))
    {
        Class::Ord
    } else {
        class
    }
}

/// TeXbook table 18, in em. Illegal combinations take the ordinary-atom value.
fn space_between(left: Option<Class>, right: Class) -> f32 {
    let l = match left {
        Some(c) => c,
        None => return 0.0,
    };
    match l {
        Class::Ord => match right {
            Class::Op => THIN,
            Class::Bin => MED,
            Class::Rel => THICK,
            Class::Inner => THIN,
            _ => 0.0,
        },
        Class::Op => match right {
            Class::Ord | Class::Op | Class::Inner => THIN,
            Class::Rel => THICK,
            _ => 0.0,
        },
        Class::Bin => match right {
            Class::Ord | Class::Op | Class::Bin | Class::Open | Class::Inner => MED,
            Class::Rel => THICK,
            _ => 0.0,
        },
        Class::Rel => match right {
            Class::Ord | Class::Op | Class::Open | Class::Inner => THICK,
            Class::Bin => MED,
            _ => 0.0,
        },
        Class::Open => 0.0,
        Class::Close => match right {
            Class::Op | Class::Inner => THIN,
            Class::Bin => MED,
            _ => 0.0,
        },
        Class::Punct => match right {
            Class::Bin => MED,
            _ => THIN,
        },
        Class::Inner => match right {
            Class::Ord | Class::Op | Class::Open | Class::Punct | Class::Inner => THIN,
            Class::Bin => MED,
            Class::Rel => THICK,
            Class::Close => 0.0,
        },
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn layout(node: &Node, size: f32, level: u32, ctx: &mut Ctx) -> Sb {
    match node {
        Node::Row(items) => layout_row(items, size, level, ctx),
        Node::Sym {
            text,
            style,
            underline,
            ..
        } => layout_sym(text, *style, *underline, size, ctx),
        Node::Space(em) => Sb::pure(em * size),
        Node::Text { body, style, .. } => {
            let family = match style {
                TextStyle::Sans => Family::Sans,
                TextStyle::Serif => Family::Serif,
                TextStyle::Mono => Family::Mono,
            };
            let font = Font {
                size,
                family,
                weight: Weight::Regular,
                slant: Slant::Upright,
            };
            text_box(body, font, ctx.color, ctx.m)
        }
        Node::Style { body, .. } => layout(body, size, level, ctx),
        Node::StyleSwitch(_) => Sb::empty(),
        Node::Frac { num, den, frac } => layout_frac(num, den, *frac, size, level, ctx),
        Node::Binom { num, den } => layout_binom(num, den, size, level, ctx),
        Node::Sqrt { index, body } => layout_sqrt(index.as_deref(), body, size, level, ctx),
        Node::Accent { kind, base } => {
            let b = layout(base, size, level, ctx);
            layout_accent(*kind, b, size, ctx)
        }
        Node::Delimited { left, right, body } => {
            let b = layout(body, size, level, ctx);
            wrap_delims(b, *left, *right, size, ctx)
        }
        Node::BigDelim {
            delim,
            size: big,
            ..
        } => {
            let h = big.em() * size;
            let axis = AXIS * size;
            let need_ascent = (h * 0.5 + axis).max(0.0);
            let need_descent = (h * 0.5 - axis).max(0.0);
            delim_box(*delim, need_ascent, need_descent, size, true, ctx)
        }
        Node::Scripts {
            base,
            sup,
            sub,
            limits,
        } => layout_scripts(base, sup, sub, *limits, size, level, ctx),
        Node::Op { text, kind, .. } => {
            let op_size = if *kind == OpKind::Large && ctx.display {
                size * 1.4
            } else {
                size
            };
            text_box(text, upright_math(op_size), ctx.color, ctx.m)
        }
        Node::Environ { kind, rows, spec } => {
            layout_environment(*kind, rows, spec.as_ref(), size, level, ctx)
        }
        Node::Raw(text) => {
            let faded = ctx.color.fade(0.75);
            text_box(text, upright_math(size), faded, ctx.m)
        }
        Node::LineBreak => Sb::pure(0.5 * size),
    }
}

fn layout_row(items: &[Node], size: f32, level: u32, ctx: &mut Ctx) -> Sb {
    let saved_display = ctx.display;
    let mut out = Sb::empty();
    let mut cursor = 0.0f32;
    let mut prev: Option<Class> = None;
    let mut pending = 0.0f32;

    for item in items {
        match item {
            Node::Space(em) => {
                pending += em * size;
                prev = None;
                continue;
            }
            Node::StyleSwitch(style) => {
                ctx.display = matches!(style, SizeStyle::Display);
                continue;
            }
            Node::LineBreak => {
                ctx.warn("顶层的 \\\\ 已按空格处理".to_owned());
                pending += 0.5 * size;
                prev = None;
                continue;
            }
            _ => {}
        }

        let class = effective_class(prev, class_of(item));
        let gap = pending + space_between(prev, class) * size;
        pending = 0.0;

        let child = layout(item, size, level, ctx);
        let x = (cursor + gap).max(0.0);
        place(&mut out, &child, x, 0.0);
        out.ascent = out.ascent.max(child.ascent);
        out.descent = out.descent.max(child.descent);
        cursor = x + child.w;
        prev = Some(class);
    }

    out.w = cursor.max(0.0);
    ctx.display = saved_display;
    out
}

fn layout_sym(text: &str, style: SymStyle, underline: bool, size: f32, ctx: &mut Ctx) -> Sb {
    let font = Font {
        size,
        family: style.family,
        weight: style.weight,
        slant: style.slant,
    };
    let mut sb = text_box(text, font, ctx.color, ctx.m);
    if underline {
        let thickness = (0.06 * size).max(0.6);
        let y = sb.descent + 0.06 * size;
        let rect = Rect::from_xywh(0.0, y, sb.w, thickness);
        sb.prims.push(Prim::filled_rect(rect, 0.0, ctx.color));
        sb.descent = y + thickness;
    }
    sb
}

// ---------------------------------------------------------------------------
// Fractions, binomials
// ---------------------------------------------------------------------------

fn layout_frac(
    num: &Node,
    den: &Node,
    frac: FracStyle,
    size: f32,
    level: u32,
    ctx: &mut Ctx,
) -> Sb {
    let display_sized = match frac {
        FracStyle::Display => true,
        FracStyle::Script => false,
        FracStyle::Auto => ctx.display,
    };
    let part = if display_sized { size } else { size * SCRIPT_L1 };
    let nb = layout(num, part, level + 1, ctx);
    let db = layout(den, part, level + 1, ctx);

    let rule_h = (0.06 * size).max(0.6);
    let gap = 0.12 * size;
    let axis = AXIS * size;

    let content_w = nb.w.max(db.w);
    let w = content_w + 0.2 * size;
    let rule_top = -axis - rule_h * 0.5;
    let rule_bottom = -axis + rule_h * 0.5;
    let num_baseline = rule_top - gap - nb.descent;
    let den_baseline = rule_bottom + gap + db.ascent;

    let mut out = Sb::empty();
    place(&mut out, &nb, (w - nb.w) * 0.5, num_baseline);
    place(&mut out, &db, (w - db.w) * 0.5, den_baseline);
    out.prims
        .push(Prim::filled_rect(Rect::from_xywh(0.0, rule_top, w, rule_h), 0.0, ctx.color));
    out.w = w;
    out.ascent = nb.ascent - num_baseline;
    out.descent = den_baseline + db.descent;
    out
}

fn layout_binom(num: &Node, den: &Node, size: f32, level: u32, ctx: &mut Ctx) -> Sb {
    let part = if ctx.display { size } else { size * SCRIPT_L1 };
    let nb = layout(num, part, level + 1, ctx);
    let db = layout(den, part, level + 1, ctx);

    let gap = 0.15 * size;
    let axis = AXIS * size;
    let w = nb.w.max(db.w);
    let num_baseline = -axis - gap * 0.5 - nb.descent;
    let den_baseline = -axis + gap * 0.5 + db.ascent;

    let mut inner = Sb::empty();
    place(&mut inner, &nb, (w - nb.w) * 0.5, num_baseline);
    place(&mut inner, &db, (w - db.w) * 0.5, den_baseline);
    inner.w = w;
    inner.ascent = nb.ascent - num_baseline;
    inner.descent = den_baseline + db.descent;

    wrap_delims(inner, Delim::LParen, Delim::RParen, size, ctx)
}

// ---------------------------------------------------------------------------
// Radicals
// ---------------------------------------------------------------------------

fn layout_sqrt(
    index: Option<&Node>,
    body: &Node,
    size: f32,
    level: u32,
    ctx: &mut Ctx,
) -> Sb {
    let b = layout(body, size, level, ctx);
    let stroke_w = (0.05 * size).max(0.6);
    let radical_w = 0.52 * size;
    let gap = 0.08 * size;

    let idx = index.map(|n| layout(n, size * SCRIPT_L1, level + 1, ctx));
    let idx_w = idx.as_ref().map(|s| s.w).unwrap_or(0.0);
    let shift = if idx.is_some() { idx_w + 0.06 * size } else { 0.0 };

    let over_y = -(b.ascent + gap);
    let bottom_y = b.descent.max(0.18 * size);
    let stem_top = over_y + stroke_w * 0.5;

    let points = vec![
        [shift, -0.06 * size],
        [shift + 0.22 * size, bottom_y],
        [shift + 0.40 * size, stem_top],
        [shift + radical_w, stem_top],
    ];

    let mut out = Sb::empty();
    out.prims.push(Prim::path(
        points,
        false,
        None,
        Some(Stroke::new(stroke_w, ctx.color)),
    ));

    let over_w = b.w.max(0.2 * size);
    out.prims.push(Prim::filled_rect(
        Rect::from_xywh(shift + radical_w - 0.02 * size, over_y, over_w, stroke_w),
        0.0,
        ctx.color,
    ));
    place(&mut out, &b, shift + radical_w, 0.0);

    out.w = (shift + radical_w + b.w).max(shift + radical_w + over_w);
    out.ascent = (b.ascent + gap + stroke_w * 0.5).max(0.0);
    out.descent = b.descent.max(bottom_y + stroke_w * 0.5);

    if let Some(ix) = idx {
        let iy = over_y + (ix.ascent - ix.descent) * 0.5;
        place(&mut out, &ix, 0.0, iy);
        out.ascent = out.ascent.max(-(iy - ix.ascent));
        out.descent = out.descent.max(iy + ix.descent);
    }

    out
}

// ---------------------------------------------------------------------------
// Accents
// ---------------------------------------------------------------------------

fn mark_glyph(kind: AccentKind) -> &'static str {
    match kind {
        AccentKind::Hat => "\u{02C6}",
        AccentKind::Tilde => "\u{02DC}",
        AccentKind::Dot => "\u{02D9}",
        AccentKind::Ddot => "\u{00A8}",
        AccentKind::Vec => "\u{2192}",
        AccentKind::Check => "\u{02C7}",
        AccentKind::Breve => "\u{02D8}",
        AccentKind::Acute => "\u{00B4}",
        AccentKind::Grave => "\u{0060}",
        AccentKind::Ring => "\u{02DA}",
        _ => "\u{02C6}",
    }
}

fn layout_accent(kind: AccentKind, base: Sb, size: f32, ctx: &mut Ctx) -> Sb {
    let stroke_w = (0.055 * size).max(0.6);
    let gap = 0.08 * size;
    let stroke = Stroke::new(stroke_w, ctx.color);

    match kind {
        AccentKind::Overline | AccentKind::Bar => {
            let w = base.w.max(0.3 * size);
            let y = -(base.ascent + gap);
            let mut out = Sb::empty();
            place(&mut out, &base, (w - base.w) * 0.5, 0.0);
            out.prims
                .push(Prim::filled_rect(Rect::from_xywh(0.0, y, w, stroke_w), 0.0, ctx.color));
            out.w = w;
            out.ascent = base.ascent + gap + stroke_w;
            out.descent = base.descent;
            out
        }
        AccentKind::Underline => {
            let w = base.w.max(0.3 * size);
            let y = base.descent + gap;
            let mut out = Sb::empty();
            place(&mut out, &base, (w - base.w) * 0.5, 0.0);
            out.prims
                .push(Prim::filled_rect(Rect::from_xywh(0.0, y, w, stroke_w), 0.0, ctx.color));
            out.w = w;
            out.ascent = base.ascent;
            out.descent = y + stroke_w;
            out
        }
        AccentKind::WideHat | AccentKind::WideTilde => {
            let w = base.w.max(0.5 * size);
            let y = -(base.ascent + gap);
            let hh = 0.18 * size;
            let points = if kind == AccentKind::WideHat {
                vec![[0.0, y + hh], [w * 0.5, y], [w, y + hh]]
            } else {
                vec![
                    [0.0, y + hh],
                    [w * 0.25, y],
                    [w * 0.5, y + hh],
                    [w * 0.75, y],
                    [w, y + hh],
                ]
            };
            let mut out = Sb::empty();
            place(&mut out, &base, (w - base.w) * 0.5, 0.0);
            out.prims.push(Prim::path(points, false, None, Some(stroke)));
            out.w = w;
            out.ascent = base.ascent + gap + hh + stroke_w * 0.5;
            out.descent = base.descent;
            out
        }
        _ => {
            // Marks are glyphs like U+02C6 whose ink sits in the upper part of
            // their own line box, so the mark is anchored by its baseline just
            // above the base instead of by its box bottom. That keeps the accent
            // close to the base while never letting the boxes collide.
            let font = Font {
                size: size * 0.85,
                family: Family::Math,
                weight: Weight::Regular,
                slant: Slant::Upright,
            };
            let mark = text_box(mark_glyph(kind), font, ctx.color, ctx.m);
            let w = base.w.max(mark.w);
            let mark_baseline = -(base.ascent + gap);
            let mut out = Sb::empty();
            place(&mut out, &base, (w - base.w) * 0.5, 0.0);
            place(&mut out, &mark, (w - mark.w) * 0.5, mark_baseline);
            out.w = w;
            out.ascent = base.ascent + gap + mark.ascent;
            out.descent = base.descent;
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Delimiters
// ---------------------------------------------------------------------------

impl Delim {
    fn glyph(self) -> &'static str {
        match self {
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBracket => "[",
            Self::RBracket => "]",
            Self::LBrace => "{",
            Self::RBrace => "}",
            Self::Pipe => "|",
            Self::DPipe => "\u{2016}",
            Self::LAngle => "\u{27E8}",
            Self::RAngle => "\u{27E9}",
            Self::LFloor => "\u{230A}",
            Self::RFloor => "\u{230B}",
            Self::LCeil => "\u{2308}",
            Self::RCeil => "\u{2309}",
            Self::Slash => "/",
            Self::Backslash => "\\",
            Self::Dot => "",
        }
    }
}

/// Height below which the plain delimiter glyph is used instead of a polyline.
const NATURAL_DELIM: f32 = 1.3;

/// Build one delimiter sized to a content box of `need_ascent` / `need_descent`.
fn delim_box(
    d: Delim,
    need_ascent: f32,
    need_descent: f32,
    size: f32,
    force_stretch: bool,
    ctx: &mut Ctx,
) -> Sb {
    if matches!(d, Delim::Dot) {
        return Sb::empty();
    }
    let simple = matches!(d, Delim::Slash | Delim::Backslash);
    if !force_stretch && (simple || need_ascent + need_descent <= NATURAL_DELIM * size) {
        return text_box(d.glyph(), upright_math(size), ctx.color, ctx.m);
    }

    let pad = 0.10 * size;
    let top = -need_ascent - pad;
    let bottom = need_descent + pad;
    let (prims, w) = stretch_delim(d, top, bottom, size, ctx.color);
    Sb {
        w,
        ascent: -top,
        descent: bottom,
        prims,
    }
}

/// Piecewise-polyline substitution for a stretched delimiter.
fn stretch_delim(d: Delim, top: f32, bottom: f32, size: f32, color: Rgba) -> (Vec<Prim>, f32) {
    let stroke = Stroke::new((0.05 * size).max(0.6), color);
    let h = (bottom - top).max(0.001);
    let centre = (top + bottom) * 0.5;
    let half = (h * 0.5).max(0.001);

    match d {
        Delim::LParen | Delim::RParen => {
            let w = 0.38 * size;
            let mut points = Vec::with_capacity(9);
            for i in 0..=8 {
                let t = i as f32 / 8.0;
                let y = top + h * t;
                let u = ((y - centre) / half).clamp(-1.0, 1.0);
                let x = if d == Delim::LParen {
                    w * u * u
                } else {
                    w * (1.0 - u * u)
                };
                points.push([x, y]);
            }
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::LBracket => {
            let w = 0.30 * size;
            let points = vec![[w, top], [0.0, top], [0.0, bottom], [w, bottom]];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::RBracket => {
            let w = 0.30 * size;
            let points = vec![[0.0, top], [w, top], [w, bottom], [0.0, bottom]];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::LBrace | Delim::RBrace => {
            let w = 0.40 * size;
            let q = h * 0.03;
            let x_out = |v: f32| {
                if d == Delim::LBrace {
                    w * v
                } else {
                    w * (1.0 - v)
                }
            };
            let points = vec![
                [x_out(1.0), top],
                [x_out(0.55), top + q],
                [x_out(0.55), centre - q],
                [x_out(0.0), centre],
                [x_out(0.55), centre + q],
                [x_out(0.55), bottom - q],
                [x_out(1.0), bottom],
            ];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::Pipe => {
            let x = 0.075 * size;
            (vec![Prim::line([x, top], [x, bottom], stroke)], 0.15 * size)
        }
        Delim::DPipe => {
            let a = 0.06 * size;
            let b = 0.19 * size;
            (
                vec![
                    Prim::line([a, top], [a, bottom], stroke),
                    Prim::line([b, top], [b, bottom], stroke),
                ],
                0.25 * size,
            )
        }
        Delim::LAngle => {
            let w = 0.35 * size;
            let points = vec![[w, top], [0.0, centre], [w, bottom]];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::RAngle => {
            let w = 0.35 * size;
            let points = vec![[0.0, top], [w, centre], [0.0, bottom]];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::LFloor => {
            let w = 0.30 * size;
            let points = vec![[w, bottom], [0.0, bottom], [0.0, top]];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::RFloor => {
            let w = 0.30 * size;
            let points = vec![[0.0, bottom], [w, bottom], [w, top]];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::LCeil => {
            let w = 0.30 * size;
            let points = vec![[0.0, top], [w, top], [w, bottom]];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::RCeil => {
            let w = 0.30 * size;
            let points = vec![[w, top], [0.0, top], [0.0, bottom]];
            (vec![Prim::path(points, false, None, Some(stroke))], w)
        }
        Delim::Slash => (
            vec![Prim::line([0.0, bottom], [0.28 * size, top], stroke)],
            0.28 * size,
        ),
        Delim::Backslash => (
            vec![Prim::line([0.0, top], [0.28 * size, bottom], stroke)],
            0.28 * size,
        ),
        Delim::Dot => (Vec::new(), 0.0),
    }
}

/// Surround `inner` with a delimiter pair sized to its height.
fn wrap_delims(inner: Sb, left: Delim, right: Delim, size: f32, ctx: &mut Ctx) -> Sb {
    let l = delim_box(left, inner.ascent, inner.descent, size, false, ctx);
    let r = delim_box(right, inner.ascent, inner.descent, size, false, ctx);

    let mut out = Sb::empty();
    let mut x = 0.0f32;
    let parts: [&Sb; 3] = [&l, &inner, &r];
    for part in parts {
        place(&mut out, part, x, 0.0);
        out.ascent = out.ascent.max(part.ascent);
        out.descent = out.descent.max(part.descent);
        x += part.w;
    }
    out.w = x.max(0.0);
    out
}

// ---------------------------------------------------------------------------
// Scripts and limits
// ---------------------------------------------------------------------------

fn limits_allowed(base: &Node) -> bool {
    match base {
        Node::Op { kind, limits, .. } => match limits {
            LimitMode::NoLimits => false,
            LimitMode::Limits => true,
            LimitMode::Auto => matches!(kind, OpKind::Large | OpKind::Limit),
        },
        Node::Style { body, .. } => limits_allowed(body),
        _ => false,
    }
}

fn layout_scripts(
    base: &Node,
    sup: &Option<Box<Node>>,
    sub: &Option<Box<Node>>,
    limits: LimitMode,
    size: f32,
    level: u32,
    ctx: &mut Ctx,
) -> Sb {
    let base_box = layout(base, size, level, ctx);
    let base_w = base_box.w;
    let base_ascent = base_box.ascent;
    let base_descent = base_box.descent;

    let script_size = size * if level == 0 { SCRIPT_L1 } else { SCRIPT_L2 };
    let sup_box = sup.as_deref().map(|n| layout(n, script_size, level + 1, ctx));
    let sub_box = sub.as_deref().map(|n| layout(n, script_size, level + 1, ctx));

    if sup_box.is_none() && sub_box.is_none() {
        return base_box;
    }

    let use_limits = match limits {
        LimitMode::Limits => true,
        LimitMode::NoLimits => false,
        LimitMode::Auto => ctx.display && limits_allowed(base),
    };

    if use_limits {
        let gap = 0.18 * size;
        let sup_w = sup_box.as_ref().map(|s| s.w).unwrap_or(0.0);
        let sub_w = sub_box.as_ref().map(|s| s.w).unwrap_or(0.0);
        let w = base_w.max(sup_w).max(sub_w);

        let mut out = Sb::empty();
        place(&mut out, &base_box, (w - base_w) * 0.5, 0.0);
        out.ascent = base_ascent;
        out.descent = base_descent;

        if let Some(s) = sup_box {
            let dy = -(base_ascent + gap) - s.descent;
            place(&mut out, &s, (w - s.w) * 0.5, dy);
            out.ascent = out.ascent.max(base_ascent + gap + s.descent + s.ascent);
        }
        if let Some(s) = sub_box {
            let dy = base_descent + gap + s.ascent;
            place(&mut out, &s, (w - s.w) * 0.5, dy);
            out.descent = out.descent.max(base_descent + gap + s.ascent + s.descent);
        }
        out.w = w;
        out
    } else {
        let gap = 0.06 * size;
        let script_x = base_w + gap;

        let mut out = Sb::empty();
        place(&mut out, &base_box, 0.0, 0.0);
        out.ascent = base_ascent;
        out.descent = base_descent;

        let mut script_w = 0.0f32;
        if let Some(s) = sup_box {
            // Raise enough that the script's bottom clears the baseline.
            let raise = (0.45 * size).max(s.descent + 0.12 * size);
            place(&mut out, &s, script_x, -raise);
            out.ascent = out.ascent.max(raise + s.ascent);
            script_w = script_w.max(s.w);
        }
        if let Some(s) = sub_box {
            let drop = 0.22 * size;
            place(&mut out, &s, script_x, drop);
            out.descent = out.descent.max(drop + s.descent);
            script_w = script_w.max(s.w);
        }
        out.w = base_w + gap + script_w;
        out
    }
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

fn layout_environment(
    kind: EnvKind,
    rows: &[Vec<Node>],
    spec: Option<&ArraySpec>,
    size: f32,
    level: u32,
    ctx: &mut Ctx,
) -> Sb {
    let mut cells: Vec<Vec<Sb>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut laid = Vec::with_capacity(row.len());
        for cell in row {
            laid.push(layout(cell, size, level, ctx));
        }
        cells.push(laid);
    }

    let ncols = cells.iter().map(|r| r.len()).max().unwrap_or(0);
    if ncols == 0 {
        return Sb::empty();
    }

    let mut widths = vec![0.0f32; ncols];
    for row in &cells {
        for (c, cell) in row.iter().enumerate() {
            if c < ncols && cell.w > widths[c] {
                widths[c] = cell.w;
            }
        }
    }

    let aligns: Vec<ColAlign> = if kind == EnvKind::Cases {
        vec![ColAlign::Left; ncols]
    } else if kind.alternates_columns() {
        (0..ncols)
            .map(|i| {
                if i % 2 == 0 {
                    ColAlign::Right
                } else {
                    ColAlign::Left
                }
            })
            .collect()
    } else if kind == EnvKind::Array {
        match spec {
            Some(s) => (0..ncols)
                .map(|i| s.aligns.get(i).copied().unwrap_or(ColAlign::Center))
                .collect(),
            None => vec![ColAlign::Center; ncols],
        }
    } else {
        vec![ColAlign::Center; ncols]
    };

    let vlines: Vec<bool> = match spec {
        Some(s) if kind == EnvKind::Array => (0..=ncols)
            .map(|i| s.vlines.get(i).copied().unwrap_or(false))
            .collect(),
        _ => vec![false; ncols + 1],
    };
    let has_rules = vlines.iter().any(|b| *b);

    let col_gap = 0.6 * size;
    let pad = if has_rules { 0.3 * size } else { 0.0 };
    let mut col_x = vec![0.0f32; ncols];
    let mut x = pad;
    for (slot, w) in col_x.iter_mut().zip(widths.iter()) {
        *slot = x;
        x += w + col_gap;
    }
    let content_right = (x - col_gap).max(0.0);
    let total_w = content_right + pad;

    let row_gap = 0.35 * size;
    let mut baselines = vec![0.0f32; cells.len()];
    let mut y = 0.0f32;
    let mut ascent = 0.0f32;
    let mut descent = 0.0f32;
    for (ri, row) in cells.iter().enumerate() {
        let a = row.iter().map(|c| c.ascent).fold(0.0f32, f32::max);
        let d = row.iter().map(|c| c.descent).fold(0.0f32, f32::max);
        let baseline = y + a;
        baselines[ri] = baseline;
        if ri == 0 {
            ascent = a;
        }
        descent = baseline + d;
        y = baseline + d + row_gap;
    }

    let mut out = Sb::empty();
    for (ri, row) in cells.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if c >= ncols {
                continue;
            }
            let offset = match aligns[c] {
                ColAlign::Left => 0.0,
                ColAlign::Center => ((widths[c] - cell.w) * 0.5).max(0.0),
                ColAlign::Right => (widths[c] - cell.w).max(0.0),
            };
            place(&mut out, cell, col_x[c] + offset, baselines[ri]);
        }
    }

    if has_rules {
        let stroke = Stroke::new((0.04 * size).max(0.5), ctx.color);
        // Boundaries: 0, one between each pair of columns, then the right edge.
        let mut rule_x: Vec<f32> = Vec::with_capacity(ncols + 2);
        rule_x.push(0.0);
        for cx in &col_x {
            rule_x.push(cx - col_gap * 0.5);
        }
        rule_x.push(total_w);
        for (c, rx) in rule_x.iter().enumerate() {
            if !vlines.get(c).copied().unwrap_or(false) {
                continue;
            }
            out.prims
                .push(Prim::line([*rx, -ascent], [*rx, descent], stroke));
        }
    }

    out.w = total_w;
    out.ascent = ascent.max(0.0);
    out.descent = descent.max(0.0);

    match kind {
        EnvKind::ParenMatrix => wrap_delims(out, Delim::LParen, Delim::RParen, size, ctx),
        EnvKind::BracketMatrix => wrap_delims(out, Delim::LBracket, Delim::RBracket, size, ctx),
        EnvKind::BarMatrix => wrap_delims(out, Delim::Pipe, Delim::Pipe, size, ctx),
        EnvKind::BraceMatrix => wrap_delims(out, Delim::LBrace, Delim::RBrace, size, ctx),
        EnvKind::Cases => wrap_delims(out, Delim::LBrace, Delim::Dot, size, ctx),
        _ => out,
    }
}

// ---------------------------------------------------------------------------
// Source helpers
// ---------------------------------------------------------------------------

/// Strip one layer of math delimiters, reporting whether display style was
/// requested. A bare expression with no delimiters is text style, matching
/// TeX's inline default.
fn strip_delims(src: &str) -> (&str, bool) {
    let s = src.trim();
    let len = s.len();

    let display = (s.starts_with("$$") && s.ends_with("$$"))
        || (s.starts_with("\\[") && s.ends_with("\\]"));
    if display && len >= 4 {
        return (&s[2..len - 2], true);
    }

    if s.starts_with("\\(") && s.ends_with("\\)") && len >= 4 {
        return (&s[2..len - 2], false);
    }
    if s.starts_with('$') && s.ends_with('$') && len >= 2 {
        return (&s[1..len - 1], false);
    }

    (s, false)
}

/// `true` when the tree carries no visible content at all.
fn is_blank(node: &Node) -> bool {
    match node {
        Node::Row(items) => items.iter().all(is_blank),
        Node::Space(_) | Node::StyleSwitch(_) => true,
        Node::Scripts {
            base, sup, sub, ..
        } => {
            is_blank(base)
                && sup.as_deref().is_none_or(is_blank)
                && sub.as_deref().is_none_or(is_blank)
        }
        Node::Style { body, .. } => is_blank(body),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Bounding boxes
// ---------------------------------------------------------------------------

fn rect_is_finite(r: &Rect) -> bool {
    finite(r.min[0]) && finite(r.min[1]) && finite(r.max[0]) && finite(r.max[1])
}

fn prim_is_finite(p: &Prim) -> bool {
    match p {
        Prim::Rect { rect, .. } => rect_is_finite(rect),
        Prim::Circle { center, radius, .. } => {
            finite(center[0]) && finite(center[1]) && finite(*radius)
        }
        Prim::Ellipse { center, radii, .. } => {
            finite(center[0]) && finite(center[1]) && finite(radii[0]) && finite(radii[1])
        }
        Prim::Line { from, to, .. } => {
            finite(from[0]) && finite(from[1]) && finite(to[0]) && finite(to[1])
        }
        Prim::Path { points, .. } => points
            .iter()
            .all(|q| finite(q[0]) && finite(q[1])),
        Prim::Text { pos, .. } => finite(pos[0]) && finite(pos[1]),
    }
}

fn prim_bounds(p: &Prim, m: &dyn Measure) -> Option<Rect> {
    let r = match p {
        Prim::Rect { rect, .. } => *rect,
        Prim::Circle { center, radius, .. } => {
            Rect::from_center_size(*center, [radius * 2.0, radius * 2.0])
        }
        Prim::Ellipse { center, radii, .. } => {
            Rect::from_center_size(*center, [radii[0] * 2.0, radii[1] * 2.0])
        }
        Prim::Line { from, to, .. } => Rect::new(from[0], from[1], to[0], to[1]),
        Prim::Path { points, .. } => {
            let first = points.first()?;
            let mut min = *first;
            let mut max = *first;
            for q in points {
                min[0] = min[0].min(q[0]);
                min[1] = min[1].min(q[1]);
                max[0] = max[0].max(q[0]);
                max[1] = max[1].max(q[1]);
            }
            Rect { min, max }
        }
        Prim::Text {
            pos,
            text,
            font,
            anchor,
            ..
        } => {
            let measured = m.measure(text, *font);
            let size = [san(measured[0]).max(0.0), san(measured[1]).max(0.0)];
            let f = anchor.factor();
            Rect::from_min_size(
                [pos[0] - f[0] * size[0], pos[1] - f[1] * size[1]],
                size,
            )
        }
    };
    if rect_is_finite(&r) {
        Some(r)
    } else {
        None
    }
}

fn prims_bounds(prims: &[Prim], m: &dyn Measure) -> Option<Rect> {
    let mut acc: Option<Rect> = None;
    for p in prims {
        if let Some(r) = prim_bounds(p, m) {
            acc = Some(match acc {
                Some(a) => a.union(&r),
                None => r,
            });
        }
    }
    acc
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::{ApproxMeasure, Measure, Prim, Rgba};

    fn r(src: &str) -> MathRender {
        render(src, 16.0, Rgba::BLACK, &ApproxMeasure)
    }

    fn texts(m: &MathRender) -> Vec<String> {
        m.prims
            .iter()
            .filter_map(|p| match p {
                Prim::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn all_finite(m: &MathRender) -> bool {
        m.size[0].is_finite()
            && m.size[1].is_finite()
            && m.size[0] >= 0.0
            && m.size[1] >= 0.0
            && m.baseline.is_finite()
            && m.baseline >= 0.0
            && m.baseline <= m.size[1] + 0.001
    }

    #[test]
    fn fraction_is_taller_than_a_letter() {
        let frac = r("\\frac{1}{2}");
        let x = r("x");
        assert!(frac.size[1] > x.size[1], "{} <= {}", frac.size[1], x.size[1]);
        assert!(all_finite(&frac));
    }

    #[test]
    fn superscript_sits_above_the_baseline() {
        let m = r("x^2");
        assert!(m.size[0] > 0.0 && m.size[1] > 0.0);
        let measure = ApproxMeasure;
        let mut seen = false;
        for p in &m.prims {
            let Prim::Text { pos, text, font, .. } = p else {
                continue;
            };
            if text != "2" {
                continue;
            }
            let line = measure.measure(text.as_str(), *font)[1];
            assert!(
                pos[1] + line <= m.baseline + 0.001,
                "上标底边 {} 未高于基线 {}",
                pos[1] + line,
                m.baseline
            );
            seen = true;
        }
        assert!(seen, "未找到上标字形");
    }

    #[test]
    fn subscript_sits_below_the_baseline() {
        let m = r("x_i");
        let measure = ApproxMeasure;
        let mut seen = false;
        for p in &m.prims {
            let Prim::Text { pos, text, font, .. } = p else {
                continue;
            };
            if text != "i" {
                continue;
            }
            let line = measure.measure(text.as_str(), *font)[1];
            assert!(pos[1] > m.baseline - line * 0.5);
            assert!(font.size < 16.0);
            seen = true;
        }
        assert!(seen);
    }

    #[test]
    fn radical_is_wider_than_its_radicand() {
        let root = r("\\sqrt{x}");
        let x = r("x");
        assert!(root.size[0] > x.size[0]);
        assert!(root.size[1] >= x.size[1]);
        assert!(all_finite(&root));
    }

    #[test]
    fn limits_widen_a_large_operator() {
        let plain = r("\\sum");
        let with_limits = r("\\sum_{i=1}^{n}");
        assert!(with_limits.size[0] > plain.size[0]);
        assert!(all_finite(&with_limits));
    }

    #[test]
    fn mathbb_renders_a_different_glyph_than_plain() {
        let plain = r("R");
        let bb = r("\\mathbb{R}");
        let a = texts(&plain);
        let b = texts(&bb);
        assert_ne!(a, b);
        assert_eq!(a, vec!["R".to_owned()]);
        assert_eq!(b, vec!["\u{211D}".to_owned()]);
    }

    #[test]
    fn malformed_input_never_panics() {
        let samples = [
            "{", "}", "\\", "\\frac{1}", "\\frac", "^", "_", "x^_", "&", "\\\\", "\\left(",
            "\\left(\\right", "\\begin{matrix}", "\\begin{matrix}a & b", "\\sqrt[", "\\hspace",
            "\\text", "\\unknowncmd", "$$", "\\right)", "{{{",
        ];
        for s in samples {
            let m = r(s);
            assert!(all_finite(&m), "{s:?} 产生了非法尺寸 {:?}", m.size);
            assert!(
                m.error.is_none() || !m.prims.is_empty(),
                "{s:?} 报告错误却没有回退绘制"
            );
        }
    }

    #[test]
    fn assorted_expressions_have_finite_sizes() {
        let samples = [
            "\\alpha+\\beta^2",
            "\\frac{\\sqrt{x}}{2}",
            "\\int_0^1 x\\,dx",
            "\\left(\\frac{a}{b}\\right)",
            "\\begin{pmatrix}a&b\\\\c&d\\end{pmatrix}",
            "\\mathbb{R}^n",
            "\\hat{x}+\\vec{v}",
            "\\lim_{n\\to\\infty}",
            "\\binom{n}{k}",
            "\\text{if } x>0",
            "\\sum_{i=1}^{n}i^2",
            "\\sqrt[3]{x}",
        ];
        for s in samples {
            let m = r(s);
            assert!(all_finite(&m), "{s:?} -> {:?}", m.size);
        }
    }

    #[test]
    fn nested_scripts_terminate() {
        let m = r("x^{y^{z^{w}}}");
        assert!(all_finite(&m));
        assert!(m.size[1] > 0.0);
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let src = format!("{}{}", "\\frac{1}{".repeat(200), "x");
        let m = r(&src);
        assert!(all_finite(&m));
        assert!(!m.warnings.is_empty());
    }

    #[test]
    fn unknown_command_falls_back_to_literal_text() {
        let m = r("\\foo");
        assert!(m.warnings.iter().any(|w| w.contains("\\foo")));
        assert!(texts(&m).iter().any(|t| t == "\\foo"));
        assert!(m.error.is_none());
    }

    #[test]
    fn prims_start_at_the_origin() {
        let m = r("\\frac{a}{b}+\\sqrt{c}");
        let measure = ApproxMeasure;
        let bounds = prims_bounds(&m.prims, &measure).expect("prims exist");
        assert!(bounds.min[0].abs() < 0.01, "左边距 {}", bounds.min[0]);
        assert!(bounds.min[1].abs() < 0.01, "上边距 {}", bounds.min[1]);
        assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
    }

    #[test]
    fn display_style_stacks_limits() {
        let inline = r("x");
        let display = r("$$\\sum_{i=0}^{n}a_i$$");
        assert!(inline.size[1] > 0.0);
        assert!(all_finite(&display));
        // Limits stack, so the display form is notably taller than inline `\sum`.
        let inline_sum = r("\\sum_{i=0}^{n}");
        assert!(display.size[1] > inline_sum.size[1]);
    }

    #[test]
    fn array_with_rules_produces_lines() {
        let m = r("\\begin{array}{c|r}a&b\\\\c&d\\end{array}");
        assert!(all_finite(&m));
        assert!(m
            .prims
            .iter()
            .any(|p| matches!(p, Prim::Line { .. })));
    }
}
