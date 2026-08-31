//! Live mirror of the desktop shell's timeline display preferences.
//!
//! The GPUI shell owns the persisted UI state.  The runtime keeps this small
//! in-memory mirror so the RemoteGateway can serve the same values without
//! reading a file while the shell's throttled writer is still pending.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use vibex_core::{AgentTimelineDisplaySettings, VibexResult};
use vibex_remote::RemoteAgentTimelineDisplaySettingsSource;

#[derive(Clone)]
pub struct TimelineDisplaySettingsBridge {
    settings: Arc<RwLock<AgentTimelineDisplaySettings>>,
}

impl TimelineDisplaySettingsBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            settings: Arc::new(RwLock::new(AgentTimelineDisplaySettings::default())),
        })
    }

    pub fn get(&self) -> AgentTimelineDisplaySettings {
        *self
            .settings
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn set(&self, settings: AgentTimelineDisplaySettings) {
        *self
            .settings
            .write()
            .unwrap_or_else(|error| error.into_inner()) = settings;
    }
}

#[async_trait]
impl RemoteAgentTimelineDisplaySettingsSource for TimelineDisplaySettingsBridge {
    async fn timeline_display_settings(&self) -> VibexResult<AgentTimelineDisplaySettings> {
        Ok(self.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::AgentTimelineReasoningDisplayMode;

    #[tokio::test]
    async fn mirror_returns_the_latest_shell_value() {
        let bridge = TimelineDisplaySettingsBridge::new();
        bridge.set(AgentTimelineDisplaySettings {
            reasoning_display_mode: AgentTimelineReasoningDisplayMode::Timeline,
            ..Default::default()
        });
        assert_eq!(
            bridge
                .timeline_display_settings()
                .await
                .unwrap()
                .reasoning_display_mode,
            AgentTimelineReasoningDisplayMode::Timeline
        );
    }
}
