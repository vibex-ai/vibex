//! WASM-safe remote backend for the shared GPUI contracts.
//!
//! The crate deliberately keeps the wire protocol below the backend adapters:
//! views/controllers only see the seven domain traits from
//! `vibex-backend`. Direct and future Relay transports share the same
//! request/event/synchronisation machinery.

#![forbid(unsafe_code)]

mod backend;
mod binary;
mod credentials;
mod pairing;
mod sync;
mod transport;

pub use backend::*;
pub use binary::*;
pub use credentials::*;
pub use pairing::*;
pub use sync::*;
pub use transport::*;
