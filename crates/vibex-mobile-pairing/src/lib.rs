//! The phone's pairing core: entries, claims, credentials and the on-device
//! store.
//!
//! This is platform-neutral: it holds no GPUI and no nsh types, so a host can
//! pair through the same rules. The mobile client's pairing used to read the
//! device name itself; here it is passed in, which is what lets the same rules
//! serve a host that names the phone differently.
//!
//! Layers:
//!
//! - [`entry`]: what a scanned or pasted pairing entry is — a link or a bare
//!   code — and the claim that follows from it.
//! - [`pairing`]: the credential schema, the claim functions (server code,
//!   code link, desktop offer, LAN and zero-config LAN), and the transport
//!   configuration a saved credential builds.
//! - [`store`]: the JSON file the phone keeps its credentials in.

pub mod entry;
pub mod pairing;
pub mod store;

pub use entry::{PairingEntry, claim_entry, classify_entry};
pub use pairing::*;
pub use store::{PAIRING_STORE_SCHEMA_VERSION, PairingStore, PairingStoreFile};
