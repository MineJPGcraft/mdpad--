//! LaTeX math typesetting for MarkdownPad.
//!
//! The engine is a two-stage pure data transformation, in the same spirit as
//! the diagram front-ends: [`parse`] turns the source text into an AST that
//! carries no geometry, and [`layout`] turns that AST into renderer-agnostic
//! [`crate::gfx::Prim`]s.
//!
//! Nothing here mentions egui, and all glyph metrics arrive through
//! [`crate::gfx::Measure`], so the typesetter can be exercised headlessly and
//! cannot take the editor down: arbitrary user text is expected to produce
//! warnings, fallbacks, or an empty box — never a panic.
//!
//! Rendering an expression is a single call:
//!
//! ```text
//! let out = crate::tex::render(r"\frac{\pi}{2}", 16.0, colour, &measure);
//! // out.prims are laid out with the box origin at (0, 0)
//! // out.baseline is the distance from that origin down to the baseline
//! // out.warnings lists anything that had to be approximated
//! ```

pub mod layout;
pub mod parse;

// `MathRender` is the return type of [`render`]. Nothing inside the binary
// names it directly (callers just use the value), but re-exporting it is what
// makes this module's API coherent.
#[allow(unused_imports)]
pub use layout::{render, MathRender};
