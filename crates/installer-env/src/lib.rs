//! The Windows installer owns exactly one user environment value: the per-user
//! `Path`. It never reads or writes any other value, and it never replaces a
//! `Path` it could not read.
//!
//! The editing rule is split from the registry layer so it can be tested
//! everywhere. The reason this crate exists is a limit in NSIS: a `$variable`
//! holds at most 1023 characters, so `ReadRegStr` on a longer `Path` yields an
//! empty string and sets the error flag. The installer used to interpret that
//! empty string as "the user has no Path" and wrote only its own directory,
//! destroying every entry the user had. Here the value is read through
//! `RegQueryValueExW` with a buffer sized from the registry itself, so length
//! is not a failure mode.

pub mod path;

#[cfg(windows)]
pub mod registry;

/// The registry key that holds per-user environment variables.
pub const USER_ENVIRONMENT_KEY: &str = "Environment";

/// The value the installer edits. Other environment values are never touched.
pub const PATH_VALUE_NAME: &str = "Path";
