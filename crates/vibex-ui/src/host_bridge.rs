use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use vibex_backend::{BackendBound, BackendFuture, BackendResult, MutationRequest};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKeyboardSource {
    Capacitor,
    VisualViewport,
    #[default]
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostViewportSnapshot {
    pub width: f32,
    pub height: f32,
    pub safe_area: HostInsets,
    pub keyboard_visible: bool,
    pub keyboard_inset: f32,
    pub keyboard_source: HostKeyboardSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapability {
    SafeArea,
    Keyboard,
    Storage,
    Push,
    DeepLink,
    Camera,
    FilePicker,
    Share,
    SystemUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostCapabilitySnapshot {
    pub capabilities: BTreeSet<HostCapability>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStorageWrite {
    pub key: String,
    pub value: String,
}

impl fmt::Debug for HostStorageWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostStorageWrite")
            .field("key", &self.key)
            .field("has_value", &!self.value.is_empty())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFileSelection {
    pub name: String,
    pub mime_type: Option<String>,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for HostFileSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostFileSelection")
            .field("has_name", &!self.name.is_empty())
            .field("mime_type", &self.mime_type)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostShareRequest {
    pub title: Option<String>,
    pub text: Option<String>,
    pub url: Option<String>,
}

impl fmt::Debug for HostShareRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostShareRequest")
            .field("has_title", &self.title.is_some())
            .field("has_text", &self.text.is_some())
            .field("has_url", &self.url.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HostEvent {
    Viewport(HostViewportSnapshot),
    PushToken(String),
    DeepLink(String),
}

impl fmt::Debug for HostEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Viewport(viewport) => formatter.debug_tuple("Viewport").field(viewport).finish(),
            Self::PushToken(value) => formatter
                .debug_struct("PushToken")
                .field("has_value", &!value.is_empty())
                .finish(),
            Self::DeepLink(value) => formatter
                .debug_struct("DeepLink")
                .field("has_value", &!value.is_empty())
                .finish(),
        }
    }
}

pub trait HostEventSubscription: BackendBound {
    fn next(&mut self) -> BackendFuture<'_, Option<HostEvent>>;
}

/// Host-only bridge. Product navigation and page state intentionally do not
/// appear in this interface.
pub trait PlatformBridge: BackendBound {
    fn capabilities(&self) -> HostCapabilitySnapshot;
    fn viewport(&self) -> HostViewportSnapshot;
    fn subscribe(&self) -> BackendResult<Box<dyn HostEventSubscription>>;
    fn read_storage(&self, key: String) -> BackendFuture<'_, Option<String>>;
    fn write_storage(&self, request: MutationRequest<HostStorageWrite>) -> BackendFuture<'_, ()>;
    fn pick_file(&self, accept: Vec<String>) -> BackendFuture<'_, Option<HostFileSelection>>;
    fn capture_image(&self) -> BackendFuture<'_, Option<HostFileSelection>>;
    fn share(&self, request: HostShareRequest) -> BackendFuture<'_, ()>;
    fn open_system_url(&self, url: String) -> BackendFuture<'_, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_capability_contract_is_bounded_to_platform_services() {
        let capabilities = HostCapabilitySnapshot {
            capabilities: [
                HostCapability::SafeArea,
                HostCapability::Keyboard,
                HostCapability::Storage,
                HostCapability::Push,
                HostCapability::DeepLink,
                HostCapability::Camera,
                HostCapability::FilePicker,
                HostCapability::Share,
                HostCapability::SystemUrl,
            ]
            .into_iter()
            .collect(),
        };
        let encoded = serde_json::to_string(&capabilities).unwrap();
        assert!(!encoded.contains("navigation"));
        assert!(!encoded.contains("session"));
        assert_eq!(capabilities.capabilities.len(), 9);
    }

    #[test]
    fn host_bridge_debug_redacts_tokens_storage_file_bytes_and_share_payloads() {
        let values = [
            format!(
                "{:?}",
                HostEvent::PushToken("push-token-secret".to_string())
            ),
            format!(
                "{:?}",
                HostEvent::DeepLink("vibex://pair?code=secret".to_string())
            ),
            format!(
                "{:?}",
                HostStorageWrite {
                    key: "session".to_string(),
                    value: "storage-secret".to_string(),
                }
            ),
            format!(
                "{:?}",
                HostFileSelection {
                    name: "private.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    bytes: b"file-secret".to_vec(),
                }
            ),
            format!(
                "{:?}",
                HostShareRequest {
                    title: Some("private title".to_string()),
                    text: Some("share-secret".to_string()),
                    url: Some("https://example.test/secret".to_string()),
                }
            ),
        ];
        let debug = values.join(" ");

        for secret in [
            "push-token-secret",
            "code=secret",
            "storage-secret",
            "private.txt",
            "file-secret",
            "private title",
            "share-secret",
            "example.test/secret",
        ] {
            assert!(!debug.contains(secret));
        }
    }
}
