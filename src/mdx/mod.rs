//! The MDX / Markdoc runtime.
//!
//! MDX and Markdoc are *template* formats: the document text carries JavaScript
//! expressions (MDX) or Markdoc's expression language, and the real renderer
//! evaluates them against a data scope. This module supplies that evaluation
//! layer in pure Rust, so a document can be previewed offline without a
//! JavaScript engine:
//!
//! ```text
//! document text ──doc::block──▶ Block IR ──runtime::expand──▶ resolved Blocks
//!                                   ▲                              │
//!                                   └──── scope (front matter, ────┘
//!                                          export const, props)
//! ```
//!
//! What it deliberately does **not** do: run arbitrary JavaScript. There is no
//! `eval` of user code, no imports resolution and no function values — only
//! expressions over data. See [`expr`] for the supported subset.

pub mod expr;
pub mod runtime;
pub mod scope;

// The runtime surface is wider than the current UI consumes — the block
// expander lands next and uses all of it. Re-exporting keeps callers from
// needing to know which submodule a type lives in.
#[allow(unused_imports)]
pub use runtime::{Resolved, expand, expand_document};
#[allow(unused_imports)]
pub use scope::scope_from_document;
#[allow(unused_imports)]
pub use expr::{EvalError, Scope, Value, eval, eval_or_null, interpolate};
