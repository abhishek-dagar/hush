//! Table layout: anonymous box fixup, the slot grid, column and row sizing,
//! borders and backgrounds (spec 0009, child 0004, AC-14 to AC-19).
//!
//! Tables are built on the box tree produced by [`crate::box_tree`], then
//! laid out following CSS Tables 3, with Chrome as the tie breaker where the
//! draft is unclear.

mod fixup;
mod grid;

pub(crate) use fixup::fix_tables;
