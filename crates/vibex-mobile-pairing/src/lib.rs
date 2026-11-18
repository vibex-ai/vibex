//! The phone's pairing core: entries, claims, credentials and the on-device
//! store.
//!
//! This is platform-neutral: it holds no GPUI and no nsh types, so both mobile
//! clients can pair through the same rules. It is the pairing module the GPUI
//! client keeps in `apps/mobile/src/pairing.rs`, with the device name it used
//! to read from the host passed in instead; the GPUI client still carries its
//! copy until its own build is green again, at which point that module becomes
//! a re-export of this crate.
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
