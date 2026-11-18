//! The app's styled controls.
//!
//! `nsh_base` gives behavior and no look. Every control the screens use is
//! wrapped exactly once here, takes a variant or a size rather than a color or
//! a radius, and reads its colors from [`crate::theme`]. A screen imports from
//! this module, never from `nsh_base`'s controls directly, so a new look is a
//! change here and nowhere else.

pub mod action;
pub mod brand;
pub mod field;
