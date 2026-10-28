//! Reconnect planning for an SSH-provisioned runtime.
//!
//! This is the deterministic half of the reconnect loop: decide which stages
//! must run, whether a turn was interrupted, and how long to wait before a
//! retry. The caller owns the actual tunnel/runtime/client effects.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectReason {
    Boot,
    TunnelExited,
    RuntimeExited,
    HostRestarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectStage {
    EnsureTunnel,
    EnsureRuntime,
    ReconnectClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectContext {
    pub ssh_runtime: bool,
    pub tunnel_running: bool,
    pub runtime_running: bool,
    pub active_turn: bool,
    pub reason: ReconnectReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectPlan {
    pub stages: Vec<ReconnectStage>,
    pub turn_interrupted: bool,
}

impl ReconnectPlan {
    pub fn new(context: ReconnectContext) -> Self {
        if !context.ssh_runtime {
            return Self {
                stages: Vec::new(),
                turn_interrupted: false,
            };
        }
        let mut stages = Vec::new();
        if !context.tunnel_running {
            stages.push(ReconnectStage::EnsureTunnel);
        }
        if !context.runtime_running {
            stages.push(ReconnectStage::EnsureRuntime);
        }
        stages.push(ReconnectStage::ReconnectClient);
        let turn_interrupted = context.active_turn
            && matches!(
                context.reason,
                ReconnectReason::RuntimeExited | ReconnectReason::HostRestarted
            );
        Self {
            stages,
            turn_interrupted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectBackoff {
    attempts: u8,
    maximum_attempts: u8,
    base: Duration,
    maximum: Duration,
}

impl ReconnectBackoff {
    pub fn new(
        maximum_attempts: u8,
        base: Duration,
        maximum: Duration,
    ) -> Result<Self, ReconnectError> {
        if maximum_attempts == 0 || base.is_zero() || maximum < base {
            return Err(ReconnectError::new(
                "ssh_reconnect_backoff_invalid",
                "the reconnect backoff bounds are invalid",
            ));
        }
        Ok(Self {
            attempts: 0,
            maximum_attempts,
            base,
            maximum,
        })
    }

    pub fn attempts(&self) -> u8 {
        self.attempts
    }

    pub fn can_retry(&self) -> bool {
        self.attempts < self.maximum_attempts
    }

    pub fn next_delay(&mut self) -> Option<Duration> {
        if !self.can_retry() {
            return None;
        }
        self.attempts = self.attempts.saturating_add(1);
        let exponent = u32::from(self.attempts.saturating_sub(1)).min(5);
        Some(self.base.saturating_mul(1u32 << exponent).min(self.maximum))
    }

    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectFailureAction {
    Retry,
    UseLocal,
}

pub fn failure_action(can_retry: bool) -> ReconnectFailureAction {
    if can_retry {
        ReconnectFailureAction::Retry
    } else {
        ReconnectFailureAction::UseLocal
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectError {
    pub code: &'static str,
    pub message: String,
}

impl ReconnectError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ReconnectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReconnectError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(
        tunnel_running: bool,
        runtime_running: bool,
        active_turn: bool,
        reason: ReconnectReason,
    ) -> ReconnectContext {
        ReconnectContext {
            ssh_runtime: true,
            tunnel_running,
            runtime_running,
            active_turn,
            reason,
        }
    }

    #[test]
    fn boot_ensures_tunnel_runtime_then_client() {
        let plan = ReconnectPlan::new(context(false, false, false, ReconnectReason::Boot));
        assert_eq!(
            plan.stages,
            vec![
                ReconnectStage::EnsureTunnel,
                ReconnectStage::EnsureRuntime,
                ReconnectStage::ReconnectClient,
            ]
        );
        assert!(!plan.turn_interrupted);
    }

    #[test]
    fn running_tunnel_and_runtime_only_reconnect_the_client() {
        let plan = ReconnectPlan::new(context(true, true, false, ReconnectReason::Boot));
        assert_eq!(plan.stages, vec![ReconnectStage::ReconnectClient]);
    }

    #[test]
    fn tunnel_exit_and_runtime_exit_run_only_what_is_needed() {
        let tunnel = ReconnectPlan::new(context(false, true, false, ReconnectReason::TunnelExited));
        assert_eq!(
            tunnel.stages,
            vec![
                ReconnectStage::EnsureTunnel,
                ReconnectStage::ReconnectClient
            ]
        );
        let runtime =
            ReconnectPlan::new(context(true, false, false, ReconnectReason::RuntimeExited));
        assert_eq!(
            runtime.stages,
            vec![
                ReconnectStage::EnsureRuntime,
                ReconnectStage::ReconnectClient
            ]
        );
    }

    #[test]
    fn local_runtime_has_no_reconnect_plan() {
        let plan = ReconnectPlan::new(ReconnectContext {
            ssh_runtime: false,
            tunnel_running: false,
            runtime_running: false,
            active_turn: true,
            reason: ReconnectReason::HostRestarted,
        });
        assert!(plan.stages.is_empty());
        assert!(!plan.turn_interrupted);
    }

    #[test]
    fn runtime_restarts_mark_an_active_turn_interrupted() {
        assert!(
            ReconnectPlan::new(context(false, false, true, ReconnectReason::RuntimeExited))
                .turn_interrupted
        );
        assert!(
            ReconnectPlan::new(context(false, false, true, ReconnectReason::HostRestarted))
                .turn_interrupted
        );
        assert!(
            !ReconnectPlan::new(context(false, false, true, ReconnectReason::Boot))
                .turn_interrupted
        );
    }

    #[test]
    fn backoff_stops_at_the_attempt_limit_and_resets() {
        let mut backoff =
            ReconnectBackoff::new(3, Duration::from_secs(1), Duration::from_secs(30)).unwrap();
        assert_eq!(backoff.next_delay(), Some(Duration::from_secs(1)));
        assert_eq!(backoff.next_delay(), Some(Duration::from_secs(2)));
        assert_eq!(backoff.next_delay(), Some(Duration::from_secs(4)));
        assert!(!backoff.can_retry());
        assert_eq!(backoff.next_delay(), None);
        backoff.reset();
        assert_eq!(backoff.next_delay(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn exhausted_retries_offer_use_local() {
        assert_eq!(failure_action(true), ReconnectFailureAction::Retry);
        assert_eq!(failure_action(false), ReconnectFailureAction::UseLocal);
        assert!(ReconnectBackoff::new(0, Duration::from_secs(1), Duration::from_secs(2)).is_err());
    }
}
