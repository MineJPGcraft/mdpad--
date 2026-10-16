//! Document model and parsers.
//!
//! ```text
//! text ──block::parse_document──▶ Vec<Block>   (block structure, byte-anchored)
//! block.inline_span ──inline::Inline::scan_at──▶ spans + constructs
//! ```
//!
//! The two layers are independent on purpose: block structure is line-oriented
//! and cheap to recompute, while inline scanning is only ever run for the blocks
//! that are actually on screen.

// This module is pure re-exports; the names below are the crate's `doc` API.
#![allow(unused_imports)]

pub mod block;
pub mod inline;
pub mod model;

pub use block::parse_document;
pub use inline::{Construct, Inline, InlineKind, Style as InlineStyle};
pub use model::{
    Align, Block, BlockKind, Callout, CalloutSyntax, CalloutVariant, DocStats, Document, Flavor,
    ListKind, MarkdocTagKind,
};
