use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use vibex_core::{
    AgentSession, AgentSessionRuntimeSelectionEvent, ProviderProfileId, RuntimeSessionEvent,
    TimelineLiveEvent, VibexSessionId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderConfigChangePhase {
    ProfilesChanged,
    RuntimeOptionsChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfigChangedEvent {
    pub provider_profile_ids: Vec<ProviderProfileId>,
    pub phase: ProviderConfigChangePhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEventStream {
    Timeline,
    Runtime,
    RuntimeSelection,
    Usage,
    Fanout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthoritativeRefetch {
    pub session_id: Option<VibexSessionId>,
    pub timeline: bool,
    pub runtime: bool,
    pub runtime_selection: bool,
    pub usage: bool,
}

impl AuthoritativeRefetch {
    pub fn for_stream(stream: DesktopEventStream) -> Self {
        Self {
            session_id: None,
            timeline: matches!(
                stream,
                DesktopEventStream::Timeline | DesktopEventStream::Fanout
            ),
            runtime: matches!(
                stream,
                DesktopEventStream::Runtime | DesktopEventStream::Fanout
            ),
            runtime_selection: matches!(
                stream,
                DesktopEventStream::RuntimeSelection | DesktopEventStream::Fanout
            ),
            usage: matches!(
                stream,
                DesktopEventStream::Usage | DesktopEventStream::Fanout
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DesktopEvent {
    Timeline(TimelineLiveEvent),
    SessionUpdated(AgentSession),
    Runtime(RuntimeSessionEvent),
    RuntimeSelection(AgentSessionRuntimeSelectionEvent),
    ProviderConfigChanged(ProviderConfigChangedEvent),
    UsageInvalidated,
    Lagged {
        stream: DesktopEventStream,
        skipped: u64,
        refetch: AuthoritativeRefetch,
    },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopEventReceiverClosed;

pub struct DesktopEventReceiver {
    inner: broadcast::Receiver<DesktopEvent>,
}

impl DesktopEventReceiver {
    pub(crate) fn new(inner: broadcast::Receiver<DesktopEvent>) -> Self {
        Self { inner }
    }

    pub async fn recv(&mut self) -> Result<DesktopEvent, DesktopEventReceiverClosed> {
        match self.inner.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(skipped)) => Ok(DesktopEvent::Lagged {
                stream: DesktopEventStream::Fanout,
                skipped,
                refetch: AuthoritativeRefetch::for_stream(DesktopEventStream::Fanout),
            }),
            Err(broadcast::error::RecvError::Closed) => Err(DesktopEventReceiverClosed),
        }
    }
}
