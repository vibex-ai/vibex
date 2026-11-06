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

mod capability;
mod program_status;
mod signals;

pub use capability::{
    COLOR_TERM, ChildEnvironment, DRAWS_PICTURES, IMAGE_OVERRIDE, KITTY_KEYBOARD_PROTOCOL,
    OUTER_TERMINAL_VARIABLES, TERM_NAME, child_environment,
};
#[cfg(not(target_family = "wasm"))]
pub use emulator::*;
#[cfg(target_family = "wasm")]
pub use emulator_wasm::*;
pub use program_status::{
    PROGRAM_STATUS_PROBE, PROGRAM_STATUS_PROBE_REPLY, ProgramState, ProgramStatus,
    ProgramStatusScanner, is_probe_prefix, probe_length,
};
pub use signals::{
    TerminalColorQuery, TerminalHostSignals, TerminalMouseModes, TerminalSignalScanner,
};
