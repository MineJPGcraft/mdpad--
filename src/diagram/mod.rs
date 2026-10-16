//! Diagram engines: Markdown fence in, positioned primitives out.
//!
//! MarkdownPad renders Mermaid and PlantUML diagrams entirely offline. Source
//! text is turned into the renderer-agnostic IR in [`model`] by one of the two
//! front-ends ([`mermaid`], [`plantuml`]); [`layout`] then turns that IR into
//! final, positioned [`crate::gfx::Prim`] values via the helpers in [`draw`].
//! No SVG, no HTML, no JavaScript, no network and no external renderer: every
//! coordinate is produced by this crate.
//!
//! ```text
//! fence info ──Engine::from_fence_info──▶ Engine
//! source     ──mermaid::parse / plantuml::parse──▶ Diagram (IR)
//! IR         ──layout──▶ DiagramRender { prims, size, warnings, summary }
//! ```
//!
//! The pipeline is deliberately pure: no function here can panic, allocate
//! without bound, or emit a non-finite coordinate, so a half-typed diagram is
//! always safe to repaint on every keystroke.

pub mod draw;
pub mod layout;
pub mod mermaid;
pub mod model;
pub mod plantuml;

pub use layout::{layout, DiagramRender};
pub use model::Diagram;

use crate::gfx::{Family, Measure, Rgba};

// ---------------------------------------------------------------------------
// Engine selection
// ---------------------------------------------------------------------------

/// Which front-end to use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Mermaid,
    PlantUml,
}

impl Engine {
    /// Pick an engine from a Markdown fence info string
    /// (`mermaid`, `mmd`, `plantuml`, `puml`, `uml`, `plantuml-svg`, ...).
    ///
    /// Only the first whitespace-delimited token is inspected, so Mermaid's
    /// own `mermaid { theme: dark }` argument block is tolerated. Returns
    /// `None` for non-diagram languages and for empty input.
    pub fn from_fence_info(info: &str) -> Option<Self> {
        let token = info.split_whitespace().next()?;
        // Trailing arguments written without a space (`mermaid{...}`).
        let token = token.split('{').next().unwrap_or("").trim();
        if token.is_empty() {
            return None;
        }
        match token.to_ascii_lowercase().as_str() {
            "mermaid" | "mmd" | "mermaid-js" => Some(Self::Mermaid),
            "plantuml" | "plantuml-svg" | "plantuml-png" | "puml" | "pu" | "uml" => {
                Some(Self::PlantUml)
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Theme inputs so the diagram matches the host editor.
///
/// Colours are plain [`Rgba`] values, so the caller can build the theme from
/// the live egui visuals (or from a hard-coded light/dark palette) without this
/// module ever depending on egui.
#[derive(Clone, Copy, Debug)]
pub struct DiagramTheme {
    pub node_fill: Rgba,
    pub node_stroke: Rgba,
    pub node_text: Rgba,
    pub edge: Rgba,
    pub edge_text: Rgba,
    pub cluster_fill: Rgba,
    pub cluster_stroke: Rgba,
    pub cluster_text: Rgba,
    pub note_fill: Rgba,
    pub note_stroke: Rgba,
    pub note_text: Rgba,
    pub accent: Rgba,
    /// When `false`, a background rect covering the whole canvas is emitted
    /// first so the diagram is opaque.
    pub transparent: bool,
    pub background: Rgba,
    pub font_family: Family,
    /// Base font size in points; every other size derives from it.
    pub title_size: f32,
    /// Multiplies every font size (node labels, edge labels, notes, titles).
    pub font_scale: f32,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Parse + lay out + paint. Never panics.
///
/// `src` is the raw fence body, `engine` comes from
/// [`Engine::from_fence_info`], `base_size` is the host's base font size (used
/// only when `theme.title_size` is unusable) and `m` supplies text metrics.
/// The returned primitives have their top-left corner at `(0, 0)`.
pub fn render(
    src: &str,
    engine: Engine,
    theme: &DiagramTheme,
    base_size: f32,
    m: &dyn Measure,
) -> DiagramRender {
    let diagram = match engine {
        Engine::Mermaid => mermaid::parse(src),
        Engine::PlantUml => plantuml::parse(src),
    };
    layout(&diagram, theme, base_size, m)
}
