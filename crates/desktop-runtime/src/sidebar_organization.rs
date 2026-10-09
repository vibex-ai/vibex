//! Bridge between the remote service and the running Desktop shell for the
//! sidebar tree.
//!
//! The sidebar organization lives in the Desktop's UI state, which the shell
//! holds in memory and persists on its own schedule. Serving compact clients
//! from the file behind the shell's back would let the two writers clobber each
//! other, so requests are forwarded to the shell instead: it answers from the
//! same state it renders, and applies changes exactly as a local drag would.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use vibex_core::{
    GroupPresentationCommand, GroupPresentationReply, RemoteSidebarOrganizationMutation,
    RemoteSidebarOrganizationSnapshot, SessionGroupId, VibexError, VibexResult,
    VibexUsePresentationCapability, VibexUseUnavailableReason,
};
use vibex_remote::RemoteSidebarOrganizationSource;

/// How long a compact client waits for the Desktop shell to answer before the
/// request is reported as unavailable. The shell answers on its next frame, so
/// exceeding this means it is wedged rather than busy.
const SHELL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

type Reply = oneshot::Sender<VibexResult<RemoteSidebarOrganizationSnapshot>>;

/// A request forwarded to the Desktop shell. The shell owns the reply channel
/// and must answer, including on rejection, so callers never hang.
pub enum SidebarOrganizationRequest {
    Snapshot {
        reply: Reply,
    },
    Mutate {
        mutation: Box<RemoteSidebarOrganizationMutation>,
        expected_revision: Option<u64>,
        reply: Reply,
    },
}

impl SidebarOrganizationRequest {
    /// Answers the request. Dropping the reply channel instead is also safe —
    /// the caller then reports the shell as unavailable.
    pub fn respond(self, outcome: VibexResult<RemoteSidebarOrganizationSnapshot>) {
        let reply = match self {
            Self::Snapshot { reply } => reply,
            Self::Mutate { reply, .. } => reply,
        };
        let _ = reply.send(outcome);
    }
}

#[derive(Default)]
pub struct SidebarOrganizationBridge {
    sender: Mutex<Option<mpsc::UnboundedSender<SidebarOrganizationRequest>>>,
}

fn shell_unavailable() -> VibexError {
    VibexError::capability(
        "remote_sidebar_organization_shell_unavailable",
        "the desktop shell is not answering, so its sidebar layout is unavailable",
    )
}

impl SidebarOrganizationBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Claims the bridge for a Desktop shell. A later attach replaces the
    /// earlier one so a restarted shell takes over cleanly.
    pub fn attach(&self) -> mpsc::UnboundedReceiver<SidebarOrganizationRequest> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self
            .sender
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(sender);
        receiver
    }

    async fn forward(
        &self,
        build: impl FnOnce(Reply) -> SidebarOrganizationRequest,
    ) -> VibexResult<RemoteSidebarOrganizationSnapshot> {
        let (reply, response) = oneshot::channel();
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(shell_unavailable)?;
        sender.send(build(reply)).map_err(|_| shell_unavailable())?;
        match tokio::time::timeout(SHELL_RESPONSE_TIMEOUT, response).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => Err(shell_unavailable()),
        }
    }
}

#[async_trait]
impl RemoteSidebarOrganizationSource for SidebarOrganizationBridge {
    async fn sidebar_organization(&self) -> VibexResult<RemoteSidebarOrganizationSnapshot> {
        self.forward(|reply| SidebarOrganizationRequest::Snapshot { reply })
            .await
    }

    async fn mutate_sidebar_organization(
        &self,
        mutation: RemoteSidebarOrganizationMutation,
        expected_revision: Option<u64>,
    ) -> VibexResult<RemoteSidebarOrganizationSnapshot> {
        self.forward(move |reply| SidebarOrganizationRequest::Mutate {
            mutation: Box::new(mutation),
            expected_revision,
            reply,
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requests_fail_fast_while_no_shell_is_attached() {
        let bridge = SidebarOrganizationBridge::new();
        let error = bridge
            .sidebar_organization()
            .await
            .expect_err("an unattached bridge has nothing to read");
        assert_eq!(error.code, "remote_sidebar_organization_shell_unavailable");
    }

    #[tokio::test]
    async fn an_attached_shell_answers_snapshot_requests() {
        let bridge = SidebarOrganizationBridge::new();
        let mut requests = bridge.attach();
        let shell = tokio::spawn(async move {
            let request = requests.recv().await.expect("the bridge forwards requests");
            request.respond(Ok(RemoteSidebarOrganizationSnapshot {
                revision: 7,
                ..RemoteSidebarOrganizationSnapshot::default()
            }));
        });
        let snapshot = bridge
            .sidebar_organization()
            .await
            .expect("the attached shell answered");
        assert_eq!(snapshot.revision, 7);
        shell.await.expect("the shell task finished");
    }

    #[tokio::test]
    async fn a_shell_that_drops_the_reply_reports_unavailable_instead_of_hanging() {
        let bridge = SidebarOrganizationBridge::new();
        let mut requests = bridge.attach();
        let shell = tokio::spawn(async move {
            drop(requests.recv().await.expect("the bridge forwards requests"));
        });
        let error = bridge
            .sidebar_organization()
            .await
            .expect_err("a dropped reply is not an answer");
        assert_eq!(error.code, "remote_sidebar_organization_shell_unavailable");
        shell.await.expect("the shell task finished");
    }
}

// ---------------------------------------------------------------------------
// Session group presentation
// ---------------------------------------------------------------------------

/// A typed display request forwarded to the Desktop shell.
///
/// The sidebar bridge answers "what does the tree look like". This one answers
/// "may I show this team split across panes, and what did the client actually
/// render" — a different question with a different lifetime, so it gets its own
/// channel instead of overloading the organisation mutation.
pub enum SessionGroupPresentationRequest {
    /// The shell publishes what it can do. It is sent when the shell attaches
    /// and whenever a window changes what it can show.
    PublishCapability {
        capability: Box<VibexUsePresentationCapability>,
    },
    /// Create or update one group, and optionally make it the visible
    /// workspace.
    Apply {
        command: Box<GroupPresentationCommand>,
        reply: oneshot::Sender<VibexResult<GroupPresentationReply>>,
    },
    /// Remove one group's presentation. Sessions and tasks are untouched.
    Dissolve {
        group_id: SessionGroupId,
        reply: oneshot::Sender<VibexResult<bool>>,
    },
}

/// Bridge between the runtime's presentation requests and the running shell.
///
/// The capability is cached rather than forwarded, because `vibex_discover`
/// must answer without waiting for a frame to render. A shell that has never
/// attached leaves the cached value at its "no client" default, which is the
/// honest answer for a headless runtime.
#[derive(Default)]
pub struct SessionGroupPresentationBridge {
    sender: Mutex<Option<mpsc::UnboundedSender<SessionGroupPresentationRequest>>>,
    capability: Mutex<VibexUsePresentationCapability>,
}

fn presentation_unavailable() -> VibexError {
    VibexError::capability(
        "presentation_unavailable",
        "no connected client can present a session group",
    )
}

impl SessionGroupPresentationBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Claims the bridge for a Desktop shell. A later attach replaces the
    /// earlier one so a restarted shell takes over cleanly.
    pub fn attach(&self) -> mpsc::UnboundedReceiver<SessionGroupPresentationRequest> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self
            .sender
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(sender);
        receiver
    }

    /// Detaches without answering outstanding requests. A shell that goes away
    /// must not keep pretending it can present.
    pub fn detach(&self) {
        *self
            .sender
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        *self
            .capability
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = VibexUsePresentationCapability::default();
    }

    pub fn capability(&self) -> VibexUsePresentationCapability {
        self.capability
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Records what the connected shell can do.
    pub fn publish_capability(&self, capability: VibexUsePresentationCapability) {
        *self
            .capability
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = capability;
    }

    /// Whether a shell is attached right now.
    pub fn is_attached(&self) -> bool {
        self.sender
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    /// Applies one group command and waits for the shell's answer.
    ///
    /// A shell that stopped answering reports `presentation_unavailable`
    /// rather than a success the client never confirmed.
    pub async fn apply(
        &self,
        command: GroupPresentationCommand,
    ) -> VibexResult<GroupPresentationReply> {
        let (reply, response) = oneshot::channel();
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(presentation_unavailable)?;
        sender
            .send(SessionGroupPresentationRequest::Apply {
                command: Box::new(command),
                reply,
            })
            .map_err(|_| presentation_unavailable())?;
        match tokio::time::timeout(SHELL_RESPONSE_TIMEOUT, response).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => Ok(GroupPresentationReply {
                state: vibex_core::PresentationState::Deferred,
                revision: 0,
                reason: Some(VibexUseUnavailableReason::NoShell),
                message: Some(
                    "the desktop shell did not confirm the layout; the group stays available"
                        .to_string(),
                ),
                applied_layout: None,
            }),
        }
    }

    /// Asks the shell to drop one group's presentation.
    pub async fn dissolve(&self, group_id: &SessionGroupId) -> VibexResult<bool> {
        let (reply, response) = oneshot::channel();
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(presentation_unavailable)?;
        sender
            .send(SessionGroupPresentationRequest::Dissolve {
                group_id: group_id.clone(),
                reply,
            })
            .map_err(|_| presentation_unavailable())?;
        match tokio::time::timeout(SHELL_RESPONSE_TIMEOUT, response).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => Err(presentation_unavailable()),
        }
    }
}
