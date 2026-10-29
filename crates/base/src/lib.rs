//! Shared engine basics: the `Au` length type, geometry, the generational
//! arena and interned names (spec 0002, Crates).
//!
//! Every engine crate depends on this one; it depends on nothing in the engine.

#![forbid(unsafe_code)]

mod arena;
pub mod atoms;
mod au;
mod geometry;
mod mat4;

pub use arena::{Arena, Id};
pub use au::{AU_PER_PX, Au};
pub use geometry::{Point, Rect, SideOffsets, Size};
pub use mat4::{mat4_identity, mat4_inverse, mat4_mul, mat4_translate};

/// Marks ids into a document's box tree.
///
/// The box type itself lives in `layout`, but `dom` stores a node's box id
/// and cannot depend on `layout`, so the marker lives here (spec 0002, Crates
/// and who depends on whom).
#[derive(Debug)]
pub enum BoxMarker {}

/// A layout box in one document's box tree. Same 8 byte shape as a node id.
pub type BoxId = Id<BoxMarker>;
