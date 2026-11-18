//! Events the platform hosts send into the running app.
//!
//! The Kotlin and Swift shells own the camera and the local-network browser,
//! so their results arrive here: a scanned pairing link, a discovered runtime
//! or a permission answer. The channel is process-wide because one phone runs
//! one surface; the app starts the reader once, on the engine thread, so the
//! pairing service's signals are written where they are owned.
//!
//! Discovery payloads are a small JSON contract until the host bridge is
//! aligned with the desktop's advertisements:
//!
//! - `{"kind":"candidate","key":"…","name":"…"}`
//! - `{"kind":"removed","key":"…"}`
//! - `{"kind":"permission_denied"}`
//! - `{"kind":"failed","message":"…"}`

use std::sync::{Mutex, OnceLock};

use futures_channel::mpsc::{UnboundedSender, unbounded};
use futures_util::StreamExt as _;
use nsh_core::prelude::*;
use serde::Deserialize;

use crate::pairing::PairingService;

/// Something the host observed and the app should act on.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "android", target_os = "ios")),
    allow(dead_code, reason = "built by the platform entry points only")
)]
pub enum HostEvent {
    /// The camera read a pairing link or a server connection string.
    ScanResult(String),
    /// Discovery found a runtime.
    DiscoveryCandidate { key: String, name: String },
    /// A runtime left the network.
    DiscoveryRemoved { key: String },
    /// The user refused local-network access.
    DiscoveryPermissionDenied,
    /// Discovery failed.
    DiscoveryFailed(String),
}

static SENDER: OnceLock<Mutex<Option<UnboundedSender<HostEvent>>>> = OnceLock::new();

/// Starts the host-event reader for `service`.
///
/// Called once from the root component. A second call is a no-op: the phone
/// runs one surface, and a second reader would split the events.
pub fn start(service: PairingService) {
    let (sender, mut receiver) = unbounded();
    let slot = SENDER.get_or_init(|| Mutex::new(None));
    let mut slot = slot.lock().unwrap_or_else(|error| error.into_inner());
    if slot.is_some() {
        return;
    }
    *slot = Some(sender);
    drop(slot);
    spawn(async move {
        while let Some(event) = receiver.next().await {
            service.apply_host_event(event);
        }
    });
}

/// Sends an event to the running app; false when it has not started.
pub fn send(event: HostEvent) -> bool {
    let Some(slot) = SENDER.get() else {
        return false;
    };
    let sender = slot
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    sender.is_some_and(|sender| sender.unbounded_send(event).is_ok())
}

/// A discovery message in the host's JSON contract.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum DiscoveryMessage {
    Candidate { key: String, name: String },
    Removed { key: String },
    PermissionDenied,
    Failed { message: String },
}

/// Parses a discovery message from the host.
///
/// A message that does not parse becomes a failure rather than being dropped:
/// a bridge sending the wrong shape should be visible on the page.
pub fn parse_discovery(value: &str) -> HostEvent {
    match serde_json::from_str::<DiscoveryMessage>(value) {
        Ok(DiscoveryMessage::Candidate { key, name }) => {
            HostEvent::DiscoveryCandidate { key, name }
        }
        Ok(DiscoveryMessage::Removed { key }) => HostEvent::DiscoveryRemoved { key },
        Ok(DiscoveryMessage::PermissionDenied) => HostEvent::DiscoveryPermissionDenied,
        Ok(DiscoveryMessage::Failed { message }) => HostEvent::DiscoveryFailed(message),
        Err(error) => HostEvent::DiscoveryFailed(format!(
            "the discovery bridge sent an invalid message: {error}"
        )),
    }
}

/// Parses a discovery message and sends it to the running app.
///
/// Only the platform modules call this; on the host target it has no caller.
#[cfg_attr(
    not(any(target_os = "android", target_os = "ios")),
    allow(dead_code, reason = "called by the platform entry points only")
)]
pub fn send_discovery_json(value: &str) -> bool {
    send(parse_discovery(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_candidate_carries_its_key_and_name() {
        assert_eq!(
            parse_discovery(r#"{"kind":"candidate","key":"desktop-1","name":"Studio"}"#),
            HostEvent::DiscoveryCandidate {
                key: "desktop-1".to_owned(),
                name: "Studio".to_owned(),
            }
        );
    }

    #[test]
    fn a_removed_runtime_is_reported() {
        assert_eq!(
            parse_discovery(r#"{"kind":"removed","key":"desktop-1"}"#),
            HostEvent::DiscoveryRemoved {
                key: "desktop-1".to_owned()
            }
        );
    }

    #[test]
    fn an_unknown_message_is_a_failure() {
        match parse_discovery("not json") {
            HostEvent::DiscoveryFailed(message) => assert!(message.contains("invalid message")),
            other => panic!("expected a failure, got {other:?}"),
        }
    }
}
