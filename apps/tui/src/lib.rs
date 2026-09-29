#![forbid(unsafe_code)]
//! Seat resolution shared by every Vibex character-grid entry point.
//!
//! The `vibex` binary, `vibex-server tui`, and `vibex-desktop tui` all need to
//! answer the same question — *which runtime does this client talk to, and
//! how* — so the answer lives here rather than being re-derived three times.
//! `crates/vibex-tui` itself stays ignorant of all of it.

pub mod seat;
