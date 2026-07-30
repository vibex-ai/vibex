use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use tokio::sync::broadcast;
use vibex_core::{AgentSession, FetchTimelineRequest, TimelinePage, VibexError, VibexSessionId};

use crate::{DesktopEvent, DesktopEventReceiver, DesktopRuntimeFacade};

pub struct FixtureDesktopRuntime {
    sessions: RwLock<Vec<AgentSession>>,
    timelines: RwLock<BTreeMap<VibexSessionId, TimelinePage>>,
    events: broadcast::Sender<DesktopEvent>,
    shutdown: AtomicBool,
}

impl FixtureDesktopRuntime {
    pub fn new(sessions: Vec<AgentSession>, timelines: Vec<TimelinePage>) -> Arc<Self> {
        let (events, _) = broadcast::channel(128);
        Arc::new(Self {
            sessions: RwLock::new(sessions),
            timelines: RwLock::new(
                timelines
                    .into_iter()
                    .map(|timeline| (timeline.session_id.clone(), timeline))
                    .collect(),
            ),
            events,
            shutdown: AtomicBool::new(false),
        })
    }

    pub fn publish(&self, event: DesktopEvent) -> bool {
        self.events.send(event).is_ok()
    }
}

#[async_trait]
impl DesktopRuntimeFacade for FixtureDesktopRuntime {
    fn subscribe(&self) -> DesktopEventReceiver {
        DesktopEventReceiver::new(self.events.subscribe())
    }

    async fn list_sessions(&self, include_archived: bool) -> Result<Vec<AgentSession>, VibexError> {
        let sessions = self.sessions.read().map_err(|_| {
            VibexError::process(
                "fixture_runtime_poisoned",
                "fixture session state is unavailable",
            )
        })?;
        Ok(sessions
            .iter()
            .filter(|session| include_archived || session.archived_at_ms.is_none())
            .cloned()
            .collect())
    }

    async fn fetch_timeline(
        &self,
        request: FetchTimelineRequest,
    ) -> Result<TimelinePage, VibexError> {
        self.timelines
            .read()
            .map_err(|_| {
                VibexError::process(
                    "fixture_runtime_poisoned",
                    "fixture timeline state is unavailable",
                )
            })?
            .get(&request.session_id)
            .cloned()
            .ok_or_else(|| {
                VibexError::validation("fixture_timeline_missing", "fixture timeline was not found")
            })
    }

    async fn shutdown(&self) -> Result<(), VibexError> {
        if !self.shutdown.swap(true, Ordering::SeqCst) {
            let _ = self.events.send(DesktopEvent::Shutdown);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixture_uses_the_product_facade_and_shutdown_event() {
        let runtime: Arc<dyn DesktopRuntimeFacade> =
            FixtureDesktopRuntime::new(Vec::new(), Vec::new());
        let mut events = runtime.subscribe();
        assert!(runtime.list_sessions(false).await.unwrap().is_empty());
        runtime.shutdown().await.unwrap();
        assert_eq!(events.recv().await.unwrap(), DesktopEvent::Shutdown);
    }
}
