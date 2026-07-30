use serde::{Deserialize, Serialize};

pub const TIMELINE_FALLBACK_POLL_MS: u64 = 300;
pub const RUNTIME_EVENT_POLL_MS: u64 = 2_000;
pub const ATTACH_HEARTBEAT_MS: u64 = 30_000;
pub const TERMINAL_SNAPSHOT_POLL_MS: u64 = 120;
pub const TERMINAL_LIST_POLL_MS: u64 = 3_000;
pub const FILE_TREE_POLL_MS: u64 = 1_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPollingPolicy {
    pub timeline_fallback_ms: u64,
    pub runtime_events_ms: u64,
    pub attach_heartbeat_ms: u64,
    pub terminal_snapshot_ms: u64,
    pub terminal_list_ms: u64,
    pub file_tree_ms: u64,
}

impl Default for DesktopPollingPolicy {
    fn default() -> Self {
        Self {
            timeline_fallback_ms: TIMELINE_FALLBACK_POLL_MS,
            runtime_events_ms: RUNTIME_EVENT_POLL_MS,
            attach_heartbeat_ms: ATTACH_HEARTBEAT_MS,
            terminal_snapshot_ms: TERMINAL_SNAPSHOT_POLL_MS,
            terminal_list_ms: TERMINAL_LIST_POLL_MS,
            file_tree_ms: FILE_TREE_POLL_MS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_defaults_are_frozen() {
        assert_eq!(
            DesktopPollingPolicy::default(),
            DesktopPollingPolicy {
                timeline_fallback_ms: 300,
                runtime_events_ms: 2_000,
                attach_heartbeat_ms: 30_000,
                terminal_snapshot_ms: 120,
                terminal_list_ms: 3_000,
                file_tree_ms: 1_500,
            }
        );
    }
}
