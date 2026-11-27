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
    /// Sessions a committed deletion removed.
    ///
    /// A deletion deletes the row, so unlike an archive there is no updated
    /// snapshot to broadcast: the ids are what a client drops from the list it
    /// is drawing. A read in flight still describes the row, so the ids are the
    /// only account of the removal that arrives in time to answer it.
    SessionsRemoved(Vec<VibexSessionId>),
    /// A session whose running turn was explicitly interrupted.
    ///
    /// Any client — desktop, mobile, TUI, or IM — can stop a running turn. The
    /// resulting `SessionUpdated` only reports `Idle`, so the shell needs this
    /// signal to pause auto-continue for that stopped turn without disabling
    /// the session's auto-continue preference.
    SessionInterrupted(VibexSessionId),
    Runtime(RuntimeSessionEvent),
    RuntimeSelection(AgentSessionRuntimeSelectionEvent),
    ProviderConfigChanged(ProviderConfigChangedEvent),
    /// One gateway request began, produced its first token, or ended.
    ///
    /// The graph draws a request while it runs, so this is the only event that
    /// carries in-progress state rather than a completed fact.
    ModelGateway(vibex_core::GatewayEvent),
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
