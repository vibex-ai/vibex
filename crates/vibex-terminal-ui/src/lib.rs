//! WASM-safe terminal presentation primitives.
//!
//! This crate owns the portable ANSI emulator and frame projection. PTY
//! creation, sockets, and process lifecycle remain in `vibex-terminal` and
//! the Backend implementations.
#![forbid(unsafe_code)]

#[cfg(not(target_family = "wasm"))]
mod emulator;
#[cfg(target_family = "wasm")]
mod emulator_wasm;

#[cfg(not(target_family = "wasm"))]
pub use emulator::*;
#[cfg(target_family = "wasm")]
pub use emulator_wasm::*;
