use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::error::{VibexError, VibexResult};

macro_rules! vibex_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            pub fn new() -> Self {
                Self(format!("{}_{}", Self::PREFIX, Uuid::new_v4().simple()))
            }

            pub fn parse(value: impl Into<String>) -> VibexResult<Self> {
                let value = value.into();
                if value.starts_with(&format!("{}_", Self::PREFIX))
                    && value.len() > Self::PREFIX.len() + 1
                {
                    Ok(Self(value))
                } else {
                    Err(VibexError::validation(
                        "invalid_id",
                        format!(
                            "expected {} id with '{}' prefix",
                            stringify!($name),
                            Self::PREFIX
                        ),
                    ))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = VibexError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

vibex_id!(ProjectId, "project");
vibex_id!(WorkspaceId, "workspace");
vibex_id!(VibexSessionId, "session");
vibex_id!(ProviderProfileId, "provider");
vibex_id!(ModelProviderProfileId, "model_provider");
vibex_id!(AgentRuntimeProfileId, "agent_runtime");
vibex_id!(AgentModelProviderBindingId, "agent_provider_binding");
vibex_id!(AgentConfiguredModelBindingId, "agent_model_binding");
vibex_id!(AgentProviderProjectionDescriptorId, "projection");
vibex_id!(AgentRuntimeProbeId, "agent_probe");
vibex_id!(AgentAuthenticationOperationId, "agent_auth");
vibex_id!(AgentAuthContextId, "agent_auth_context");
vibex_id!(McpServerId, "mcp");
vibex_id!(SkillId, "skill");
vibex_id!(PromptId, "prompt");
vibex_id!(ScheduledTaskId, "scheduled_task");
vibex_id!(ScheduledTaskRunId, "scheduled_run");
vibex_id!(AutomationGraphId, "automation_graph");
vibex_id!(AutomationNodeId, "automation_node");
vibex_id!(AutomationEdgeId, "automation_edge");
vibex_id!(AutomationRunId, "automation_run");
vibex_id!(AutomationRunStepId, "automation_step");
vibex_id!(HookId, "hook");
vibex_id!(DeviceId, "device");
vibex_id!(RequestId, "request");
vibex_id!(CorrelationId, "correlation");
vibex_id!(TimelineItemId, "timeline");
vibex_id!(TerminalId, "terminal");
vibex_id!(EventId, "event");
vibex_id!(ChannelId, "channel");
vibex_id!(RelayRoomId, "relayroom");
vibex_id!(RelayConnectionId, "relayconn");
vibex_id!(RelayPeerId, "relaypeer");
vibex_id!(RelayFrameId, "relayframe");
vibex_id!(RelaySessionId, "relaysession");
vibex_id!(RuntimeBindingId, "binding");
vibex_id!(RuntimeSwitchId, "switch");
vibex_id!(RuntimeSwitchOperationId, "switchop");
vibex_id!(RuntimeClientId, "runtime_client");
vibex_id!(RuntimeStreamId, "runtime_stream");
vibex_id!(RuntimeLeaseId, "runtime_lease");
vibex_id!(RuntimeProcessId, "runtime_process");
vibex_id!(NativeStateHomeId, "statehome");
vibex_id!(MessageSubmissionId, "submission");
vibex_id!(UsageExecutionId, "usage_execution");

impl UsageExecutionId {
    pub fn from_message_submission(submission_id: &MessageSubmissionId) -> Self {
        let suffix = submission_id
            .as_str()
            .strip_prefix("submission_")
            .unwrap_or(submission_id.as_str());
        Self(format!("{}_{}", Self::PREFIX, suffix))
    }
}

impl RuntimeProcessId {
    /// Accepts an existing provider-runtime process identity while the ACP
    /// registry migrates from its historical `acp-process-*` representation.
    /// The value remains opaque to public callers and is never used as a
    /// native-session routing key.
    pub fn from_opaque(value: impl Into<String>) -> VibexResult<Self> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 256 || value.chars().any(|ch| ch.is_control()) {
            return Err(VibexError::validation(
                "invalid_runtime_process_id",
                "runtime process id must be non-empty and bounded",
            ));
        }
        if value.starts_with(&format!("{}_", Self::PREFIX)) {
            return Ok(Self(value));
        }
        Ok(Self(format!("{}_{}", Self::PREFIX, value)))
    }

    /// Returns the opaque provider-owned value encoded after the public id
    /// prefix. This is used only by the ACP adapter and never crosses the
    /// public protocol boundary.
    pub fn opaque_value(&self) -> &str {
        self.0
            .strip_prefix(&format!("{}_", Self::PREFIX))
            .unwrap_or(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_id_uses_expected_prefix() {
        let id = VibexSessionId::new();
        assert!(id.as_str().starts_with("session_"));
    }

    #[test]
    fn rejects_wrong_prefix() {
        let err = ProjectId::parse("workspace_abc").unwrap_err();
        assert_eq!(err.code, "invalid_id");
    }

    #[test]
    fn serializes_as_string() {
        let id = DeviceId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.as_str()));
    }

    #[test]
    fn opaque_runtime_process_ids_round_trip_through_wire_shape() {
        let id = RuntimeProcessId::from_opaque("acp-process-test").unwrap();
        let encoded = serde_json::to_string(&id).unwrap();
        let decoded: RuntimeProcessId = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, id);
        assert_eq!(id.opaque_value(), "acp-process-test");
    }
}
