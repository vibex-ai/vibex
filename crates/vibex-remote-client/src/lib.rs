//! Platform-neutral remote backend for the shared GPUI contracts.
//!
//! The crate deliberately keeps the wire protocol below the backend adapters:
//! views/controllers only see the seven domain traits from
//! `vibex-backend`. Direct and Relay transports share the same request/event/
//! synchronisation machinery; native mobile and desktop clients remain the
//! product hosts.

#![forbid(unsafe_code)]

mod backend;
mod binary;
mod credentials;
mod lan_pairing;
mod pairing;
mod sync;
mod transport;
#[cfg(not(target_family = "wasm"))]
mod zero_config_pairing;

pub use backend::*;
pub use binary::*;
pub use credentials::*;
pub use lan_pairing::*;
pub use pairing::*;
pub use sync::*;
pub use transport::*;
#[cfg(not(target_family = "wasm"))]
pub use zero_config_pairing::*;
