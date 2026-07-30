//! Platform-neutral backend contracts consumed by shared GPUI controllers.
//!
//! The default feature set is intentionally WASM-safe. Native composition is
//! available only through the opt-in `native` feature.

#![forbid(unsafe_code)]

use std::future::Future;
use std::pin::Pin;

mod agent;
mod capability;
mod device;
mod disconnected;
mod error;
mod facade;
mod file;
mod git;
mod management;
mod mutation;
mod terminal;
mod workspace;

#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;

pub use agent::*;
pub use capability::*;
pub use device::*;
pub use disconnected::*;
pub use error::*;
pub use facade::*;
pub use file::*;
pub use git::*;
pub use management::*;
pub use mutation::*;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
pub use native::*;
pub use terminal::*;
pub use workspace::*;

#[cfg(not(target_family = "wasm"))]
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = BackendResult<T>> + Send + 'a>>;

#[cfg(target_family = "wasm")]
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = BackendResult<T>> + 'a>>;

/// Native backends can cross executor threads; WASM backends remain local to
/// the browser thread without inheriting an artificial `Send + Sync` bound.
#[cfg(not(target_family = "wasm"))]
pub trait BackendBound: Send + Sync {}

#[cfg(not(target_family = "wasm"))]
impl<T: Send + Sync + ?Sized> BackendBound for T {}

#[cfg(target_family = "wasm")]
pub trait BackendBound {}

#[cfg(target_family = "wasm")]
impl<T: ?Sized> BackendBound for T {}
