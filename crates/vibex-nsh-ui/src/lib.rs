//! The shared UI layer of Vibex's native mobile client, drawn with Native
//! Shell (`nsh`).
//!
//! The crate replaces `apps/mobile`'s GPUI view layer screen by screen. It
//! keeps what the product already decided — the desktop token palette, the
//! compact phone spacing ladder, the copy, the information architecture —
//! while the rendering, text, scrolling and input are the platform's own
//! through `nsh`.
//!
//! Layers, from the bottom:
//!
//! - [`theme`]: the phone's token names over `nsh_base::theme`, plus the
//!   spacing, radius and type ladder the old GPUI client used, so a screen
//!   reads the same numbers on both stacks while the migration runs.
//! - [`ui`]: the styled controls. `nsh_base` has behavior and no look; the
//!   look of a Vibex button, action row, field or page lives here, once.
//! - [`screens`]: the pages themselves, composed only from [`ui`] and
//!   [`theme`].
//! - [`copy`]: the three languages the phone ships (English, Simplified
//!   Chinese, Traditional Chinese), resolved from the platform locale.
//!
//! Design rules this layer keeps:
//!
//! - A screen never names a raw color; it names a token through [`theme`].
//! - A control never styles itself per screen; it takes a variant from
//!   [`ui`].
//! - Native behavior wins over pixel parity: the page structure, spacing,
//!   palette and copy match the GPUI client, while text metrics, scrolling,
//!   keyboards and selection are the platform's.

pub mod copy;
pub mod screens;
pub mod theme;
pub mod ui;
