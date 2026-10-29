//! Visual design tokens.
//!
//! Everything the editor draws is expressed as a [`Rgba`] or a size taken from
//! [`Typography`], so a theme change is a single struct swap rather than a hunt
//! through call sites. Callouts get special treatment: the same semantic variant
//! is deliberately rendered with four *different* visual languages, because the
//! four supported syntaxes really do look different in their home ecosystems and
//! matching that is the point of supporting them.

use crate::diagram::DiagramTheme;
use crate::doc::model::{CalloutSyntax, CalloutVariant};
use crate::icon::IconId;
use crate::gfx::{Family, Rgba, Stroke};

/// The colour set for one brightness mode.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Window chrome behind everything.
    pub bg: Rgba,
    /// The page the document is typeset on.
    pub canvas: Rgba,
    pub surface: Rgba,
    pub surface_alt: Rgba,
    pub border: Rgba,
    pub border_soft: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_faint: Rgba,
    pub accent: Rgba,
    pub accent_soft: Rgba,
    pub link: Rgba,
    pub code_bg: Rgba,
    pub code_text: Rgba,
    pub code_border: Rgba,
    pub selection: Rgba,
    pub caret: Rgba,
    pub quote_bar: Rgba,
    pub table_head: Rgba,
    pub table_border: Rgba,
    pub rule: Rgba,
    pub todo_done: Rgba,
    /// The area behind the page in page view. Word shows a neutral grey
    /// desktop so the white page reads as a physical sheet.
    pub desk: Rgba,
    /// The page itself, plus its drop shadow.
    pub paper: Rgba,
    pub paper_shadow: Rgba,
}

impl Palette {
    pub const fn dark() -> Self {
        Self {
            bg: Rgba::hex(0x14161A),
            canvas: Rgba::hex(0x1A1D22),
            surface: Rgba::hex(0x22262D),
            surface_alt: Rgba::hex(0x2A2F38),
            border: Rgba::hex(0x3A414C),
            border_soft: Rgba::hex(0x2E343D),
            text: Rgba::hex(0xE4E7EB),
            text_muted: Rgba::hex(0x9AA3AF),
            text_faint: Rgba::hex(0x6B7280),
            accent: Rgba::hex(0x6EA8FE),
            accent_soft: Rgba::new(0x6E, 0xA8, 0xFE, 0x33),
            link: Rgba::hex(0x7CC0FF),
            code_bg: Rgba::hex(0x23272E),
            code_text: Rgba::hex(0xE5C07B),
            code_border: Rgba::hex(0x33394a),
            selection: Rgba::new(0x6E, 0xA8, 0xFE, 0x4D),
            caret: Rgba::hex(0x7CC0FF),
            quote_bar: Rgba::hex(0x4B5563),
            table_head: Rgba::hex(0x23272E),
            table_border: Rgba::hex(0x3A414C),
            rule: Rgba::hex(0x3A414C),
            todo_done: Rgba::hex(0x4ADE80),
            desk: Rgba::hex(0x0D0F12),
            paper: Rgba::hex(0x1B1E24),
            paper_shadow: Rgba::new(0, 0, 0, 0x66),
        }
    }

    pub const fn light() -> Self {
        Self {
            bg: Rgba::hex(0xF1F3F5),
            canvas: Rgba::hex(0xFFFFFF),
            surface: Rgba::hex(0xF7F8FA),
            surface_alt: Rgba::hex(0xECEFF3),
            border: Rgba::hex(0xD3D8E0),
            border_soft: Rgba::hex(0xE4E8EE),
            text: Rgba::hex(0x1F2328),
            text_muted: Rgba::hex(0x5B6470),
            text_faint: Rgba::hex(0x8A929E),
            accent: Rgba::hex(0x2F6FEB),
            accent_soft: Rgba::new(0x2F, 0x6F, 0xEB, 0x22),
            link: Rgba::hex(0x0A66C2),
            code_bg: Rgba::hex(0xF3F4F6),
            code_text: Rgba::hex(0x953800),
            code_border: Rgba::hex(0xE0E4EA),
            selection: Rgba::new(0x2F, 0x6F, 0xEB, 0x33),
            caret: Rgba::hex(0x1F2328),
            quote_bar: Rgba::hex(0xC4CBD5),
            table_head: Rgba::hex(0xF3F4F6),
            table_border: Rgba::hex(0xD8DDE5),
            rule: Rgba::hex(0xD8DDE5),
            todo_done: Rgba::hex(0x1A7F37),
            desk: Rgba::hex(0xD6D9DE),
            paper: Rgba::hex(0xFFFFFF),
            paper_shadow: Rgba::new(0, 0, 0, 0x28),
        }
    }
}

/// Type scale. All sizes are in points at zoom 1.0.
#[derive(Clone, Copy, Debug)]
pub struct Typography {
    pub base: f32,
    /// Multiplies every size; driven by the UI zoom setting.
    pub scale: f32,
    /// Line spacing as a multiple of the font size.
    pub line_height: f32,
    /// Reading column width in points. `f32::INFINITY` means full width.
    pub measure: f32,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            base: 15.5,
            scale: 1.0,
            line_height: 1.62,
            measure: 760.0,
        }
    }
}

impl Typography {
    pub fn size(&self, points: f32) -> f32 {
        points * self.scale
    }

    /// Shorthand for a body-sized font.
    pub fn body(&self) -> f32 {
        self.size(self.base)
    }

    pub fn code(&self) -> f32 {
        self.size(self.base * 0.9)
    }

    /// Heading size for levels 1..=6.
    pub fn heading(&self, level: u8) -> f32 {
        let factor = match level {
            1 => 1.95,
            2 => 1.58,
            3 => 1.34,
            4 => 1.15,
            5 => 1.04,
            _ => 0.95,
        };
        self.body() * factor
    }

    pub fn small(&self) -> f32 {
        self.size(self.base * 0.84)
    }
}

/// The active theme.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    pub palette: Palette,
    pub typo: Typography,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            dark: true,
            palette: Palette::dark(),
            typo: Typography::default(),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            palette: Palette::light(),
            typo: Typography::default(),
        }
    }

    pub fn for_mode(dark: bool) -> Self {
        if dark {
            Self::dark()
        } else {
            Self::light()
        }
    }

    /// Body font size in points, after zoom.
    pub fn body_size(&self) -> f32 {
        self.typo.body()
    }

    /// Line height in points for `size`.
    pub fn line_height(&self, size: f32) -> f32 {
        size * self.typo.line_height
    }

    /// Colour for a callout variant's accent, independent of syntax.
    pub fn variant_accent(&self, variant: &CalloutVariant) -> Rgba {
        use CalloutVariant::*;
        let dark = self.dark;
        let pick = |dark_hex: u32, light_hex: u32| {
            Rgba::hex(if dark { dark_hex } else { light_hex })
        };
        match variant {
            Note | Info => pick(0x6EA8FE, 0x0969DA),
            Tip | Success | Example => pick(0x4ADE80, 0x1A7F37),
            Important => pick(0xC792EA, 0x8250DF),
            Warning | Caution | Question => pick(0xFBBF24, 0x9A6700),
            Danger | Failure | Bug => pick(0xF87171, 0xCF222E),
            Abstract | Todo => pick(0x67E8F9, 0x1B7C83),
            Quote => self.palette.text_muted,
            Details => pick(0x94A3B8, 0x57606A),
            Custom(_) => self.palette.accent,
        }
    }

    /// The colour each ecosystem actually paints its callouts with.
    ///
    /// GitHub and Obsidian publish theirs; the other two route everything through
    /// theme variables, so the editor's own per-variant accents stand in there.
    fn syntax_accent(&self, syntax: CalloutSyntax, variant: &CalloutVariant) -> Rgba {
        use CalloutVariant::*;
        let dark = self.dark;
        match syntax {
            // github.com: --fgColor-accent / success / done / attention / danger.
            CalloutSyntax::GitHub => {
                let (light, dark_hex) = match variant {
                    Note | Info | Abstract | Todo => (0x0969DA, 0x4493F8),
                    Tip | Success | Example => (0x1A7F37, 0x3FB950),
                    Important => (0x8250DF, 0xAB7DF8),
                    Warning | Question => (0x9A6700, 0xD29922),
                    Danger | Caution | Failure | Bug => (0xCF222E, 0xF85149),
                    Quote | Details | Custom(_) => (0x57606A, 0x9198A1),
                };
                Rgba::hex(if dark { dark_hex } else { light })
            }
            // Obsidian's default palette (`--color-*-rgb` in app.css). The hue is
            // the same in both themes; only the tint alpha changes.
            CalloutSyntax::Obsidian => Rgba::hex(match variant {
                Note | Info | Abstract | Todo => 0x086DDD,
                Important => 0x7852EE,
                Tip | Success | Example => 0x00BFBC,
                Warning | Question | Caution => 0xEC7500,
                Danger | Failure | Bug => 0xE93147,
                Details | Quote => 0x9E9E9E,
                Custom(_) => 0x7852EE,
            }),
            CalloutSyntax::VitePress | CalloutSyntax::Docusaurus => {
                self.variant_accent(variant)
            }
        }
    }

    /// The look of a callout, which depends on both the syntax it was written in
    /// and its semantic variant.
    ///
    /// Each arm mirrors the structure that syntax's own stylesheet produces, not
    /// an impression of it: GitHub rules the left edge only, Obsidian outlines
    /// the whole box, VitePress tints without a visible border, Docusaurus rules
    /// the left edge and prints its heading in caps.
    pub fn callout_look(
        &self,
        syntax: CalloutSyntax,
        variant: &CalloutVariant,
    ) -> CalloutLook {
        let accent = self.syntax_accent(syntax, variant);
        // Callouts sit on the sheet, not on the desk: blending against `canvas`
        // gave every panel a tint of the wrong colour (and made GitHub's
        // "no background" case paint a faintly darker box over the paper).
        let canvas = self.palette.paper;
        match syntax {
            // github.com `.markdown-alert`: `border-left: .25em solid`, radius
            // 6px, no background tint of its own, octicon next to the keyword,
            // keyword in Title Case.
            CalloutSyntax::GitHub => CalloutLook {
                tint: canvas,
                accent,
                title: accent,
                radius: 6.0,
                bar: Some(4.0),
                border: None,
                icon: Some(github_icon(variant)),
                icon_scale: 1.0,
                uppercase_title: false,
            },
            // Obsidian `.callout`: 1px border at ~25% of the colour over a 10%
            // tint, radius 4px, lucide icon, title in the accent colour.
            CalloutSyntax::Obsidian => CalloutLook {
                tint: accent.with_alpha(if self.dark { 0x1F } else { 0x17 }).blend_over(canvas),
                accent,
                title: accent,
                radius: 4.0,
                bar: None,
                border: Some(Stroke::new(1.0, accent.with_alpha(0x45))),
                icon: Some(obsidian_icon(variant)),
                icon_scale: 1.0,
                uppercase_title: false,
            },
            // VitePress `.custom-block`: radius 0.5rem, a soft per-type
            // background, and a border that defaults to `transparent`. No icon in
            // the default theme.
            CalloutSyntax::VitePress => CalloutLook {
                tint: accent.with_alpha(if self.dark { 0x1C } else { 0x14 }).blend_over(canvas),
                accent,
                title: accent,
                radius: 8.0,
                bar: None,
                border: None,
                icon: None,
                icon_scale: 1.0,
                uppercase_title: false,
            },
            // Docusaurus admonitions: left rule, faint tint, radius 4px, its own
            // icon at `1.6em`, and `text-transform: uppercase` on the heading.
            CalloutSyntax::Docusaurus => CalloutLook {
                tint: accent.with_alpha(if self.dark { 0x17 } else { 0x10 }).blend_over(canvas),
                accent,
                title: accent,
                radius: 4.0,
                bar: Some(4.0),
                border: None,
                icon: Some(docusaurus_icon(variant)),
                icon_scale: 1.6,
                uppercase_title: true,
            },
        }
    }

    /// Theme handed to the diagram engines so diagrams match the editor.
    pub fn diagram_theme(&self) -> DiagramTheme {
        let p = &self.palette;
        DiagramTheme {
            node_fill: if self.dark {
                Rgba::hex(0x242A33)
            } else {
                Rgba::hex(0xFFFFFF)
            },
            node_stroke: p.border,
            node_text: p.text,
            edge: p.text_muted,
            edge_text: p.text_muted,
            cluster_fill: if self.dark {
                Rgba::hex(0x1E222A)
            } else {
                Rgba::hex(0xF6F8FA)
            },
            cluster_stroke: p.border_soft,
            cluster_text: p.text_muted,
            note_fill: if self.dark {
                Rgba::hex(0x3A3524)
            } else {
                Rgba::hex(0xFFF8C5)
            },
            note_stroke: if self.dark {
                Rgba::hex(0x5A5233)
            } else {
                Rgba::hex(0xE0D08A)
            },
            note_text: p.text,
            accent: p.accent,
            // Diagrams draw their own rounded panel, so the editor's canvas shows
            // through instead of a hard white box.
            transparent: true,
            background: p.canvas,
            font_family: Family::Sans,
            title_size: self.typo.body(),
            font_scale: 1.0,
        }
    }

    /// Maps the palette onto egui's own widget styling so panels, scrollbars and
    /// text selection agree with the editor surface.
    pub fn apply_egui_visuals(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let p = &self.palette;
        // The central panel is the desk the page sits on, not the page itself —
        // using the darker `bg` here would flatten the paper against it.
        visuals.panel_fill = to_egui_color(p.desk);
        visuals.window_fill = to_egui_color(p.surface);
        visuals.extreme_bg_color = to_egui_color(if self.dark {
            Rgba::hex(0x101216)
        } else {
            Rgba::hex(0xFFFFFF)
        });
        visuals.faint_bg_color = to_egui_color(p.surface_alt);
        visuals.override_text_color = Some(to_egui_color(p.text));
        visuals.window_stroke = egui::Stroke::new(1.0, to_egui_color(p.border));
        visuals.selection.bg_fill = to_egui_color(p.selection);
        visuals.selection.stroke = egui::Stroke::new(1.0, to_egui_color(p.accent));
        visuals.hyperlink_color = to_egui_color(p.link);
        visuals.widgets.noninteractive.bg_fill = to_egui_color(p.surface);
        visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0, to_egui_color(p.border_soft));
        visuals.widgets.inactive.bg_fill = to_egui_color(p.surface_alt);
        visuals.widgets.inactive.weak_bg_fill = to_egui_color(p.surface);
        visuals.widgets.hovered.bg_fill = to_egui_color(p.border_soft);
        visuals.widgets.hovered.weak_bg_fill = to_egui_color(p.surface_alt);
        visuals.widgets.active.bg_fill = to_egui_color(p.accent_soft);
        visuals.widgets.active.weak_bg_fill = to_egui_color(p.accent_soft);
        visuals.window_shadow.color = Rgba::BLACK.with_alpha(0x50).to_egui();
        visuals.popup_shadow.color = Rgba::BLACK.with_alpha(0x40).to_egui();
        ctx.set_visuals(visuals);
    }
}

/// Resolved visual parameters for one callout.
#[derive(Clone, Copy, Debug)]
pub struct CalloutLook {
    /// Panel background.
    pub tint: Rgba,
    /// Accent used for the bar, icon and title.
    pub accent: Rgba,
    /// Title text colour.
    pub title: Rgba,
    pub radius: f32,
    /// Width of the left rule, when the syntax uses one.
    pub bar: Option<f32>,
    pub border: Option<Stroke>,
    /// The variant's real icon, from the set its ecosystem ships.
    pub icon: Option<IconId>,
    /// Icon box size as a multiple of the body font size. Docusaurus asks for
    /// `1.6em`; the others sit at about one line of text.
    pub icon_scale: f32,
    /// Whether the title is rendered in caps, which is what the syntax's CSS does.
    pub uppercase_title: bool,
}

/// The icons GitHub puts on its five alerts, from octicons.
fn github_icon(variant: &CalloutVariant) -> IconId {
    use CalloutVariant::*;
    match variant {
        Tip | Success | Example => IconId::GitHubBulb,
        Important => IconId::GitHubReport,
        Warning | Question => IconId::GitHubAlert,
        Danger | Caution | Failure | Bug => IconId::GitHubStop,
        Note | Info | Abstract | Todo | Quote | Details | Custom(_) => IconId::GitHubInfo,
    }
}

/// Obsidian's callout icons are lucide glyphs; these are the ones it assigns to
/// the types we can express.
fn obsidian_icon(variant: &CalloutVariant) -> IconId {
    use CalloutVariant::*;
    match variant {
        Note => IconId::ObsidianPencil,
        Info | Abstract | Todo => IconId::ObsidianInfo,
        // Obsidian has no "important" callout; the purple accent carries it and
        // the info glyph is the closest of its own set.
        Important => IconId::ObsidianInfo,
        Tip | Success | Example => IconId::ObsidianFlame,
        Warning | Question => IconId::ObsidianAlert,
        Danger | Failure | Bug => IconId::ObsidianZap,
        Caution => IconId::ObsidianOctagon,
        Details | Quote => IconId::ObsidianChevron,
        Custom(_) => IconId::ObsidianInfo,
    }
}

/// Docusaurus ships one icon per admonition type.
fn docusaurus_icon(variant: &CalloutVariant) -> IconId {
    use CalloutVariant::*;
    match variant {
        Note | Abstract | Todo => IconId::DocusaurusNote,
        Tip | Success | Example => IconId::DocusaurusTip,
        Info | Quote | Details | Important => IconId::DocusaurusInfo,
        Warning | Danger | Caution | Failure | Bug | Question | Custom(_) => IconId::DocusaurusDanger,
    }
}

impl Rgba {
    pub fn to_egui(self) -> egui::Color32 {
        egui::Color32::from_rgba_unmultiplied(self.r, self.g, self.b, self.a)
    }

    /// Alpha-composites `self` over an opaque backdrop.
    pub fn blend_over(self, backdrop: Rgba) -> Rgba {
        let a = self.a as f32 / 255.0;
        let mix = |fg: u8, bg: u8| (fg as f32 * a + bg as f32 * (1.0 - a)).round() as u8;
        Rgba::new(
            mix(self.r, backdrop.r),
            mix(self.g, backdrop.g),
            mix(self.b, backdrop.b),
            255,
        )
    }
}

/// Free-function form of [`Rgba::to_egui`], for call sites that read better this way.
pub fn to_egui_color(c: Rgba) -> egui::Color32 {
    c.to_egui()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_sizes_decrease_with_level() {
        let t = Theme::dark();
        let mut prev = f32::INFINITY;
        for level in 1..=6u8 {
            let size = t.typo.heading(level);
            assert!(size < prev, "level {level} not smaller than the previous");
            prev = size;
        }
        assert!(t.typo.heading(1) > t.body_size());
    }

    #[test]
    fn blend_over_yields_opaque_colour() {
        let blended = Rgba::new(255, 0, 0, 128).blend_over(Rgba::WHITE);
        assert_eq!(blended.a, 255);
        assert!(blended.r > 200 && blended.g > 100);
    }

    #[test]
    fn the_four_syntaxes_are_visually_distinct() {
        let t = Theme::dark();
        let v = CalloutVariant::Warning;
        let looks: Vec<CalloutLook> = CalloutSyntax::all()
            .iter()
            .map(|s| t.callout_look(*s, &v))
            .collect();
        // GitHub: left rule, octicon, Title Case, and no background of its own.
        assert_eq!(looks[0].bar, Some(4.0));
        assert!(looks[0].icon.is_some());
        assert!(!looks[0].uppercase_title, "github.com shows 'Warning', not 'WARNING'");
        assert_eq!(
            looks[0].tint, t.palette.paper,
            "the panel must be the page colour, not a tint"
        );
        // Obsidian outlines the whole box and has no left rule.
        assert!(looks[1].border.is_some());
        assert!(looks[1].bar.is_none());
        assert!(looks[1].icon.is_some());
        // VitePress is a tinted panel with a transparent border and no icon.
        assert!(looks[2].border.is_none());
        assert!(looks[2].bar.is_none());
        assert!(looks[2].icon.is_none());
        assert_ne!(looks[2].tint, t.palette.paper, "it is a tinted panel");
        // Docusaurus: left rule, its own icon, uppercase heading.
        assert!(looks[3].bar.is_some());
        assert!(looks[3].icon.is_some());
        assert!(looks[3].uppercase_title);
        // Every tint must be opaque, otherwise callouts read as translucent.
        for look in &looks {
            assert_eq!(look.tint.a, 255);
        }
    }

    #[test]
    fn github_alerts_use_githubs_own_colours() {
        // From the primer palette: accent / success / done / attention / danger.
        for (dark, note, tip, important, warning, caution) in [
            (false, 0x0969DA, 0x1A7F37, 0x8250DF, 0x9A6700, 0xCF222E),
            (true, 0x4493F8, 0x3FB950, 0xAB7DF8, 0xD29922, 0xF85149),
        ] {
            let t = if dark { Theme::dark() } else { Theme::light() };
            let accent = |v: CalloutVariant| t.callout_look(CalloutSyntax::GitHub, &v).accent;
            assert_eq!(accent(CalloutVariant::Note), Rgba::hex(note), "note");
            assert_eq!(accent(CalloutVariant::Tip), Rgba::hex(tip), "tip");
            assert_eq!(
                accent(CalloutVariant::Important),
                Rgba::hex(important),
                "important"
            );
            assert_eq!(accent(CalloutVariant::Warning), Rgba::hex(warning), "warning");
            assert_eq!(accent(CalloutVariant::Caution), Rgba::hex(caution), "caution");
        }
    }

    #[test]
    fn obsidian_callouts_use_its_palette_and_its_icon_set() {
        let t = Theme::dark();
        let look = |v: CalloutVariant| t.callout_look(CalloutSyntax::Obsidian, &v);
        // `--color-blue-rgb` / `--color-cyan-rgb` / `--color-red-rgb`.
        assert_eq!(look(CalloutVariant::Note).accent, Rgba::hex(0x086DDD));
        assert_eq!(look(CalloutVariant::Tip).accent, Rgba::hex(0x00BFBC));
        assert_eq!(look(CalloutVariant::Danger).accent, Rgba::hex(0xE93147));
        // Lucide glyphs, not the octicons.
        assert_eq!(look(CalloutVariant::Note).icon, Some(IconId::ObsidianPencil));
        assert_eq!(look(CalloutVariant::Tip).icon, Some(IconId::ObsidianFlame));
        assert_eq!(look(CalloutVariant::Warning).icon, Some(IconId::ObsidianAlert));
    }

    #[test]
    fn diagram_theme_is_transparent_so_the_canvas_shows_through() {
        let t = Theme::dark();
        let d = t.diagram_theme();
        assert!(d.transparent);
    }
}
