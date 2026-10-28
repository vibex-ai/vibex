//! Deterministic provisioning state machine for one SSH profile.
//!
//! The machine owns ordering, cancellation and retry. Effects (probe, upload,
//! install, launch, tunnel, pairing) run outside it and report back through
//! `complete_step`, `fail`, `report_detail`, `report_progress` and `ready`.
//!
//! Two invariants matter to the rest of the feature:
//!
//! - one profile has at most one active attempt;
//! - a runtime id is only recorded at `Ready`, never while earlier steps are
//!   still running.

use std::collections::HashMap;
use std::fmt;

const MAX_PROFILE_ID_CHARS: usize = 64;
const MAX_FAILURE_CODE_CHARS: usize = 64;
const MAX_FAILURE_MESSAGE_CHARS: usize = 1024;
pub const MAX_DETAIL_CHARS: usize = 512;
pub const MAX_LOG_TAIL_CHARS: usize = 4096;
const MAX_RUNTIME_ID_CHARS: usize = 128;

/// The ordered steps shown by the Connections progress surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProvisioningStep {
    ResolvingHost,
    CheckingEnvironment,
    UploadingRuntime,
    InstallingRuntime,
    StartingRuntime,
    OpeningTunnel,
    Pairing,
    Connecting,
    Ready,
}

impl ProvisioningStep {
    pub const ALL: [Self; 9] = [
        Self::ResolvingHost,
        Self::CheckingEnvironment,
        Self::UploadingRuntime,
        Self::InstallingRuntime,
        Self::StartingRuntime,
        Self::OpeningTunnel,
        Self::Pairing,
        Self::Connecting,
        Self::Ready,
    ];

    pub const fn index(self) -> usize {
        match self {
            Self::ResolvingHost => 0,
            Self::CheckingEnvironment => 1,
            Self::UploadingRuntime => 2,
            Self::InstallingRuntime => 3,
            Self::StartingRuntime => 4,
            Self::OpeningTunnel => 5,
            Self::Pairing => 6,
            Self::Connecting => 7,
            Self::Ready => 8,
        }
    }

    pub fn next(self) -> Option<Self> {
        Self::ALL.get(self.index() + 1).copied()
    }

    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningStatus {
    Pending,
    Running,
    Cancelling,
    Ready,
    Failed,
    Cancelled,
}

impl ProvisioningStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Ready | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvisioningProgress {
    pub done: u64,
    pub total: u64,
}

/// A step failure. `code` is stable, `message` is user-facing, and `log_tail`
/// is already bounded. Callers must redact secrets before passing either text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisioningFailure {
    pub step: ProvisioningStep,
    pub code: String,
    pub message: String,
    pub log_tail: String,
    pub retryable: bool,
}

impl ProvisioningFailure {
    pub fn new(
        step: ProvisioningStep,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            step,
            code: bounded(code, MAX_FAILURE_CODE_CHARS, "provisioning_failed"),
            message: bounded(message, MAX_FAILURE_MESSAGE_CHARS, "provisioning failed"),
            log_tail: String::new(),
            retryable,
        }
    }

    pub fn with_log_tail(mut self, log_tail: impl Into<String>) -> Self {
        self.log_tail = tail_chars(&log_tail.into(), MAX_LOG_TAIL_CHARS);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisioningEvent {
    StepStarted {
        step: ProvisioningStep,
    },
    StepDetail {
        step: ProvisioningStep,
        detail: String,
    },
    Progress {
        step: ProvisioningStep,
        done: u64,
        total: u64,
    },
    CancelRequested {
        step: ProvisioningStep,
    },
    Cancelled {
        step: ProvisioningStep,
    },
    Failed {
        failure: ProvisioningFailure,
    },
    Ready {
        runtime_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisioningSnapshot {
    pub profile_id: String,
    pub attempt_id: u64,
    pub step: ProvisioningStep,
    pub status: ProvisioningStatus,
    pub failure: Option<ProvisioningFailure>,
    pub detail: Option<String>,
    pub progress: Option<ProvisioningProgress>,
    pub cancel_requested: bool,
    /// Only `Some` after `Ready`; never set by earlier steps.
    pub active_runtime_id: Option<String>,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisioningUpdate {
    pub snapshot: ProvisioningSnapshot,
    pub event: ProvisioningEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisioningBegin {
    Started(ProvisioningUpdate),
    Existing(ProvisioningSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisioningControlError {
    InvalidProfileId,
    AlreadyStarted,
    AlreadyFinished,
    WrongStep {
        expected: ProvisioningStep,
        actual: ProvisioningStep,
    },
    NotFailed,
    NotRunning,
    InvalidDetail,
    InvalidProgress,
    EmptyRuntimeId,
    RuntimeIdTooLong,
    AttemptIdExhausted,
    UnknownAttempt,
}

impl fmt::Display for ProvisioningControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProfileId => formatter.write_str("invalid SSH profile id"),
            Self::AlreadyStarted => formatter.write_str("provisioning has already started"),
            Self::AlreadyFinished => formatter.write_str("provisioning is already finished"),
            Self::WrongStep { expected, actual } => {
                write!(
                    formatter,
                    "wrong provisioning step: expected {expected:?}, got {actual:?}"
                )
            }
            Self::NotFailed => formatter.write_str("provisioning is not in a failed state"),
            Self::NotRunning => formatter.write_str("provisioning is not running"),
            Self::InvalidDetail => formatter.write_str("provisioning detail must not be empty"),
            Self::InvalidProgress => formatter.write_str("invalid provisioning progress"),
            Self::EmptyRuntimeId => formatter.write_str("runtime id must not be empty"),
            Self::RuntimeIdTooLong => formatter.write_str("runtime id is too long"),
            Self::AttemptIdExhausted => formatter.write_str("provisioning attempt id exhausted"),
            Self::UnknownAttempt => formatter.write_str("no provisioning attempt is active"),
        }
    }
}

impl std::error::Error for ProvisioningControlError {}

/// One running attempt for one profile.
#[derive(Debug, Clone)]
pub struct ProvisioningMachine {
    profile_id: String,
    attempt_id: u64,
    step: ProvisioningStep,
    status: ProvisioningStatus,
    failure: Option<ProvisioningFailure>,
    detail: Option<String>,
    progress: Option<ProvisioningProgress>,
    cancel_requested: bool,
    log_tail: String,
    active_runtime_id: Option<String>,
    started_at_ms: i64,
    updated_at_ms: i64,
}

impl ProvisioningMachine {
    pub fn new(
        profile_id: impl Into<String>,
        attempt_id: u64,
        now_ms: i64,
    ) -> Result<Self, ProvisioningControlError> {
        let profile_id = profile_id.into();
        if !profile_id_is_valid(&profile_id) {
            return Err(ProvisioningControlError::InvalidProfileId);
        }
        Ok(Self {
            profile_id,
            attempt_id,
            step: ProvisioningStep::ResolvingHost,
            status: ProvisioningStatus::Pending,
            failure: None,
            detail: None,
            progress: None,
            cancel_requested: false,
            log_tail: String::new(),
            active_runtime_id: None,
            started_at_ms: now_ms,
            updated_at_ms: now_ms,
        })
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn attempt_id(&self) -> u64 {
        self.attempt_id
    }

    pub fn step(&self) -> ProvisioningStep {
        self.step
    }

    pub fn status(&self) -> ProvisioningStatus {
        self.status
    }

    pub fn start(&mut self, now_ms: i64) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        if self.status != ProvisioningStatus::Pending {
            return Err(ProvisioningControlError::AlreadyStarted);
        }
        self.status = ProvisioningStatus::Running;
        self.updated_at_ms = now_ms;
        Ok(self.update(ProvisioningEvent::StepStarted { step: self.step }))
    }

    /// Marks the current effect complete and starts the next step.
    ///
    /// If a cancel was requested while the effect was in flight, the machine
    /// stops here instead: the current step is a safe point.
    pub fn complete_step(
        &mut self,
        now_ms: i64,
    ) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        if self.status == ProvisioningStatus::Cancelling {
            return Ok(self.finish_cancelled(now_ms));
        }
        if self.status != ProvisioningStatus::Running {
            return Err(ProvisioningControlError::AlreadyFinished);
        }
        let next = self
            .step
            .next()
            .ok_or(ProvisioningControlError::AlreadyFinished)?;
        self.step = next;
        self.detail = None;
        self.progress = None;
        self.updated_at_ms = now_ms;
        Ok(self.update(ProvisioningEvent::StepStarted { step: next }))
    }

    pub fn report_detail(
        &mut self,
        detail: impl Into<String>,
        now_ms: i64,
    ) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        if self.status != ProvisioningStatus::Running {
            return Err(ProvisioningControlError::NotRunning);
        }
        let detail = detail.into();
        let detail = detail.trim();
        if detail.is_empty() {
            return Err(ProvisioningControlError::InvalidDetail);
        }
        let detail = head_chars(detail, MAX_DETAIL_CHARS);
        self.detail = Some(detail.clone());
        self.updated_at_ms = now_ms;
        Ok(self.update(ProvisioningEvent::StepDetail {
            step: self.step,
            detail,
        }))
    }

    pub fn report_progress(
        &mut self,
        done: u64,
        total: u64,
        now_ms: i64,
    ) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        if self.status != ProvisioningStatus::Running {
            return Err(ProvisioningControlError::NotRunning);
        }
        if total == 0 || done > total {
            return Err(ProvisioningControlError::InvalidProgress);
        }
        self.progress = Some(ProvisioningProgress { done, total });
        self.updated_at_ms = now_ms;
        Ok(self.update(ProvisioningEvent::Progress {
            step: self.step,
            done,
            total,
        }))
    }

    /// Appends effect output to the bounded tail used by the next failure.
    ///
    /// The caller redacts secrets first; this layer only bounds the size.
    pub fn append_log(&mut self, chunk: &str) {
        if self.status.is_terminal() {
            return;
        }
        self.log_tail.push_str(chunk);
        self.log_tail = tail_chars(&self.log_tail, MAX_LOG_TAIL_CHARS);
    }

    pub fn fail(
        &mut self,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
        now_ms: i64,
    ) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        let failure = ProvisioningFailure::new(self.step, code, message, retryable);
        self.fail_with(failure, now_ms)
    }

    pub fn fail_with(
        &mut self,
        mut failure: ProvisioningFailure,
        now_ms: i64,
    ) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        if self.status == ProvisioningStatus::Cancelling || self.cancel_requested {
            return Ok(self.finish_cancelled(now_ms));
        }
        if self.status != ProvisioningStatus::Running {
            return Err(ProvisioningControlError::AlreadyFinished);
        }
        if failure.step != self.step {
            return Err(ProvisioningControlError::WrongStep {
                expected: self.step,
                actual: failure.step,
            });
        }
        if failure.log_tail.is_empty() && !self.log_tail.is_empty() {
            failure.log_tail = self.log_tail.clone();
        }
        self.failure = Some(failure.clone());
        self.status = ProvisioningStatus::Failed;
        self.updated_at_ms = now_ms;
        Ok(self.update(ProvisioningEvent::Failed { failure }))
    }

    /// Requests cancellation. A running effect is not interrupted; the machine
    /// transitions to `Cancelled` at the next safe point (`complete_step`,
    /// `poll_cancel`, or a failure report).
    pub fn cancel(&mut self, now_ms: i64) -> Option<ProvisioningUpdate> {
        match self.status {
            ProvisioningStatus::Pending | ProvisioningStatus::Running => {
                self.cancel_requested = true;
                self.status = ProvisioningStatus::Cancelling;
                self.updated_at_ms = now_ms;
                Some(self.update(ProvisioningEvent::CancelRequested { step: self.step }))
            }
            ProvisioningStatus::Cancelling => {
                Some(self.update(ProvisioningEvent::CancelRequested { step: self.step }))
            }
            ProvisioningStatus::Ready
            | ProvisioningStatus::Failed
            | ProvisioningStatus::Cancelled => None,
        }
    }

    /// Called by an effect at a safe point; returns the cancellation event when
    /// the machine has stopped.
    pub fn poll_cancel(&mut self, now_ms: i64) -> Option<ProvisioningUpdate> {
        if self.status == ProvisioningStatus::Cancelling {
            Some(self.finish_cancelled(now_ms))
        } else {
            None
        }
    }

    /// Retries the failed step. Effects must make the step idempotent and clean
    /// up their own partial output before retrying.
    pub fn retry(&mut self, now_ms: i64) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        if self.status != ProvisioningStatus::Failed {
            return Err(ProvisioningControlError::NotFailed);
        }
        self.status = ProvisioningStatus::Running;
        self.failure = None;
        self.cancel_requested = false;
        self.detail = None;
        self.progress = None;
        self.active_runtime_id = None;
        self.updated_at_ms = now_ms;
        Ok(self.update(ProvisioningEvent::StepStarted { step: self.step }))
    }

    /// Completes the Ready step. Only here does the machine expose the runtime
    /// id that may be written to the active-runtime registry.
    pub fn ready(
        &mut self,
        runtime_id: impl Into<String>,
        now_ms: i64,
    ) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        if self.status != ProvisioningStatus::Running {
            return Err(ProvisioningControlError::AlreadyFinished);
        }
        if self.step != ProvisioningStep::Ready {
            return Err(ProvisioningControlError::WrongStep {
                expected: ProvisioningStep::Ready,
                actual: self.step,
            });
        }
        let runtime_id = runtime_id.into();
        let runtime_id = runtime_id.trim();
        if runtime_id.is_empty() {
            return Err(ProvisioningControlError::EmptyRuntimeId);
        }
        if runtime_id.chars().count() > MAX_RUNTIME_ID_CHARS {
            return Err(ProvisioningControlError::RuntimeIdTooLong);
        }
        self.active_runtime_id = Some(runtime_id.to_string());
        self.status = ProvisioningStatus::Ready;
        self.updated_at_ms = now_ms;
        Ok(self.update(ProvisioningEvent::Ready {
            runtime_id: runtime_id.to_string(),
        }))
    }

    pub fn snapshot(&self) -> ProvisioningSnapshot {
        ProvisioningSnapshot {
            profile_id: self.profile_id.clone(),
            attempt_id: self.attempt_id,
            step: self.step,
            status: self.status,
            failure: self.failure.clone(),
            detail: self.detail.clone(),
            progress: self.progress,
            cancel_requested: self.cancel_requested,
            active_runtime_id: self.active_runtime_id.clone(),
            started_at_ms: self.started_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }

    fn finish_cancelled(&mut self, now_ms: i64) -> ProvisioningUpdate {
        self.status = ProvisioningStatus::Cancelled;
        self.updated_at_ms = now_ms;
        self.update(ProvisioningEvent::Cancelled { step: self.step })
    }

    fn update(&self, event: ProvisioningEvent) -> ProvisioningUpdate {
        ProvisioningUpdate {
            snapshot: self.snapshot(),
            event,
        }
    }
}

/// Keeps at most one attempt per profile. Different profiles are independent;
/// a global install queue belongs to the provisioning scheduler, not here.
#[derive(Debug, Default)]
pub struct ProvisioningCoordinator {
    next_attempt_id: u64,
    attempts: HashMap<String, ProvisioningMachine>,
}

impl ProvisioningCoordinator {
    pub fn begin(
        &mut self,
        profile_id: &str,
        now_ms: i64,
    ) -> Result<ProvisioningBegin, ProvisioningControlError> {
        if let Some(machine) = self.attempts.get(profile_id) {
            return Ok(ProvisioningBegin::Existing(machine.snapshot()));
        }
        let attempt_id = self
            .next_attempt_id
            .checked_add(1)
            .ok_or(ProvisioningControlError::AttemptIdExhausted)?;
        self.next_attempt_id = attempt_id;
        let mut machine = ProvisioningMachine::new(profile_id, attempt_id, now_ms)?;
        let update = machine.start(now_ms)?;
        self.attempts.insert(profile_id.to_string(), machine);
        Ok(ProvisioningBegin::Started(update))
    }

    pub fn snapshot(&self, profile_id: &str) -> Option<ProvisioningSnapshot> {
        self.attempts
            .get(profile_id)
            .map(ProvisioningMachine::snapshot)
    }

    pub fn attempt_mut(&mut self, profile_id: &str) -> Option<&mut ProvisioningMachine> {
        self.attempts.get_mut(profile_id)
    }

    pub fn cancel(&mut self, profile_id: &str, now_ms: i64) -> Option<ProvisioningUpdate> {
        self.attempts
            .get_mut(profile_id)
            .and_then(|machine| machine.cancel(now_ms))
    }

    pub fn retry(
        &mut self,
        profile_id: &str,
        now_ms: i64,
    ) -> Result<ProvisioningUpdate, ProvisioningControlError> {
        self.attempts
            .get_mut(profile_id)
            .ok_or(ProvisioningControlError::UnknownAttempt)?
            .retry(now_ms)
    }

    pub fn finish(&mut self, profile_id: &str) -> Option<ProvisioningSnapshot> {
        self.attempts
            .remove(profile_id)
            .map(|machine| machine.snapshot())
    }

    pub fn active_attempts(&self) -> usize {
        self.attempts.len()
    }
}

fn profile_id_is_valid(profile_id: &str) -> bool {
    !profile_id.is_empty()
        && profile_id.chars().count() <= MAX_PROFILE_ID_CHARS
        && profile_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn bounded(value: impl Into<String>, max: usize, fallback: &str) -> String {
    let value = value.into();
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        head_chars(value, max)
    }
}

fn head_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn tail_chars(value: &str, max: usize) -> String {
    let count = value.chars().count();
    if count <= max {
        value.to_string()
    } else {
        value.chars().skip(count - max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> ProvisioningMachine {
        ProvisioningMachine::new("profile-1", 1, 0).unwrap()
    }

    fn started_at(step: ProvisioningStep) -> ProvisioningMachine {
        let mut machine = machine();
        machine.start(0).unwrap();
        while machine.step() != step {
            machine.complete_step(0).unwrap();
        }
        machine
    }

    #[test]
    fn happy_path_walks_every_step_and_activates_only_at_ready() {
        let mut machine = machine();
        let update = machine.start(10).unwrap();
        assert_eq!(
            update.event,
            ProvisioningEvent::StepStarted {
                step: ProvisioningStep::ResolvingHost
            }
        );
        for expected in ProvisioningStep::ALL.iter().skip(1) {
            assert_eq!(machine.snapshot().active_runtime_id, None);
            let update = machine.complete_step(20).unwrap();
            assert_eq!(
                update.event,
                ProvisioningEvent::StepStarted { step: *expected }
            );
        }
        assert_eq!(machine.status(), ProvisioningStatus::Running);
        assert_eq!(machine.step(), ProvisioningStep::Ready);
        assert_eq!(machine.snapshot().active_runtime_id, None);
        assert_eq!(
            machine.complete_step(20).unwrap_err(),
            ProvisioningControlError::AlreadyFinished
        );

        let update = machine.ready("runtime-1", 30).unwrap();
        assert_eq!(
            update.event,
            ProvisioningEvent::Ready {
                runtime_id: "runtime-1".to_string()
            }
        );
        assert_eq!(machine.status(), ProvisioningStatus::Ready);
        assert_eq!(
            machine.snapshot().active_runtime_id.as_deref(),
            Some("runtime-1")
        );
    }

    #[test]
    fn every_non_ready_step_can_fail_and_retry_in_place() {
        for step in ProvisioningStep::ALL
            .iter()
            .copied()
            .filter(|step| !step.is_ready())
        {
            let mut machine = started_at(step);
            let update = machine
                .fail("probe_failed", "probe failed", true, 20)
                .unwrap();
            assert_eq!(machine.status(), ProvisioningStatus::Failed);
            let failure = machine.snapshot().failure.unwrap();
            assert_eq!(failure.step, step);
            assert_eq!(failure.code, "probe_failed");
            assert!(failure.retryable);
            assert!(matches!(
                update.event,
                ProvisioningEvent::Failed { failure } if failure.step == step
            ));

            let update = machine.retry(30).unwrap();
            assert_eq!(machine.status(), ProvisioningStatus::Running);
            assert_eq!(machine.step(), step);
            assert!(machine.snapshot().failure.is_none());
            assert_eq!(update.event, ProvisioningEvent::StepStarted { step });
            assert!(machine.complete_step(40).is_ok());
        }
    }

    #[test]
    fn cancel_waits_for_the_current_step_safe_point() {
        let mut machine = started_at(ProvisioningStep::CheckingEnvironment);
        let update = machine.cancel(10).unwrap();
        assert_eq!(
            update.event,
            ProvisioningEvent::CancelRequested {
                step: ProvisioningStep::CheckingEnvironment
            }
        );
        assert_eq!(machine.status(), ProvisioningStatus::Cancelling);
        assert!(machine.snapshot().cancel_requested);

        let update = machine.complete_step(20).unwrap();
        assert_eq!(
            update.event,
            ProvisioningEvent::Cancelled {
                step: ProvisioningStep::CheckingEnvironment
            }
        );
        assert_eq!(machine.status(), ProvisioningStatus::Cancelled);
        assert_eq!(machine.snapshot().active_runtime_id, None);
        assert!(machine.complete_step(30).is_err());
    }

    #[test]
    fn cancel_wins_over_a_late_step_failure() {
        let mut machine = started_at(ProvisioningStep::StartingRuntime);
        machine.cancel(10).unwrap();
        let update = machine
            .fail("start_failed", "start failed", true, 20)
            .unwrap();
        assert_eq!(
            update.event,
            ProvisioningEvent::Cancelled {
                step: ProvisioningStep::StartingRuntime
            }
        );
        assert_eq!(machine.status(), ProvisioningStatus::Cancelled);
        assert!(machine.snapshot().failure.is_none());
    }

    #[test]
    fn cancel_before_start_still_reaches_cancelled() {
        let mut machine = machine();
        assert!(machine.cancel(10).is_some());
        assert_eq!(machine.status(), ProvisioningStatus::Cancelling);
        let update = machine.poll_cancel(20).unwrap();
        assert_eq!(
            update.event,
            ProvisioningEvent::Cancelled {
                step: ProvisioningStep::ResolvingHost
            }
        );
        assert_eq!(machine.status(), ProvisioningStatus::Cancelled);
    }

    #[test]
    fn retry_is_only_allowed_after_failure() {
        let mut machine = machine();
        assert_eq!(
            machine.retry(10).unwrap_err(),
            ProvisioningControlError::NotFailed
        );
        machine.start(10).unwrap();
        assert_eq!(
            machine.retry(20).unwrap_err(),
            ProvisioningControlError::NotFailed
        );
    }

    #[test]
    fn detail_progress_and_log_tail_are_bounded() {
        let mut machine = started_at(ProvisioningStep::UploadingRuntime);
        let detail = "x".repeat(MAX_DETAIL_CHARS + 40);
        machine.report_detail(detail, 10).unwrap();
        assert_eq!(
            machine.snapshot().detail.unwrap().chars().count(),
            MAX_DETAIL_CHARS
        );
        assert_eq!(
            machine.report_progress(2, 3, 10).unwrap().event,
            ProvisioningEvent::Progress {
                step: ProvisioningStep::UploadingRuntime,
                done: 2,
                total: 3
            }
        );
        assert_eq!(
            machine.report_progress(4, 3, 10).unwrap_err(),
            ProvisioningControlError::InvalidProgress
        );
        assert_eq!(
            machine.report_detail("   ", 10).unwrap_err(),
            ProvisioningControlError::InvalidDetail
        );

        machine.append_log(&"y".repeat(MAX_LOG_TAIL_CHARS + 100));
        let failure = machine
            .fail("upload_failed", "upload failed", true, 20)
            .unwrap()
            .snapshot
            .failure
            .unwrap();
        assert_eq!(failure.log_tail.chars().count(), MAX_LOG_TAIL_CHARS);
    }

    #[test]
    fn ready_requires_the_ready_step_and_a_runtime_id() {
        let mut machine = started_at(ProvisioningStep::Connecting);
        assert_eq!(
            machine.ready("runtime-1", 10).unwrap_err(),
            ProvisioningControlError::WrongStep {
                expected: ProvisioningStep::Ready,
                actual: ProvisioningStep::Connecting
            }
        );
        machine.complete_step(10).unwrap();
        assert_eq!(
            machine.ready("   ", 10).unwrap_err(),
            ProvisioningControlError::EmptyRuntimeId
        );
        assert!(
            machine
                .ready("r".repeat(MAX_RUNTIME_ID_CHARS + 1), 10)
                .is_err()
        );
        assert!(machine.ready("runtime-1", 10).is_ok());
    }

    #[test]
    fn a_failure_without_a_log_tail_uses_the_machine_log() {
        let mut machine = started_at(ProvisioningStep::InstallingRuntime);
        machine.append_log("partial output");
        let failure = machine
            .fail("install_failed", "install failed", false, 10)
            .unwrap()
            .snapshot
            .failure
            .unwrap();
        assert_eq!(failure.log_tail, "partial output");
    }

    #[test]
    fn coordinator_returns_the_existing_attempt_and_starts_a_new_one_after_finish() {
        let mut coordinator = ProvisioningCoordinator::default();
        let ProvisioningBegin::Started(first) = coordinator.begin("profile-1", 0).unwrap() else {
            panic!("first begin should start an attempt");
        };
        assert_eq!(first.snapshot.attempt_id, 1);

        let ProvisioningBegin::Existing(existing) = coordinator.begin("profile-1", 0).unwrap()
        else {
            panic!("a second begin should return the existing attempt");
        };
        assert_eq!(existing.attempt_id, 1);
        assert_eq!(coordinator.active_attempts(), 1);

        assert!(coordinator.finish("profile-1").is_some());
        let ProvisioningBegin::Started(second) = coordinator.begin("profile-1", 0).unwrap() else {
            panic!("a new attempt should start after finish");
        };
        assert_eq!(second.snapshot.attempt_id, 2);
    }

    #[test]
    fn different_profiles_keep_independent_attempts() {
        let mut coordinator = ProvisioningCoordinator::default();
        let ProvisioningBegin::Started(a) = coordinator.begin("profile-a", 0).unwrap() else {
            panic!("profile-a should start");
        };
        let ProvisioningBegin::Started(b) = coordinator.begin("profile-b", 0).unwrap() else {
            panic!("profile-b should start");
        };
        assert_eq!(coordinator.active_attempts(), 2);
        assert_ne!(a.snapshot.attempt_id, b.snapshot.attempt_id);
        assert_eq!(
            coordinator.snapshot("profile-a").unwrap().profile_id,
            "profile-a"
        );
        assert_eq!(
            coordinator.snapshot("profile-b").unwrap().profile_id,
            "profile-b"
        );
    }

    #[test]
    fn coordinator_can_cancel_and_retry_an_attempt() {
        let mut coordinator = ProvisioningCoordinator::default();
        coordinator.begin("profile-1", 0).unwrap();
        assert!(coordinator.cancel("profile-1", 10).is_some());
        {
            let machine = coordinator.attempt_mut("profile-1").unwrap();
            assert!(machine.poll_cancel(20).is_some());
        }
        assert_eq!(
            coordinator.snapshot("profile-1").unwrap().status,
            ProvisioningStatus::Cancelled
        );
        assert!(coordinator.retry("profile-1", 30).is_err());
        assert!(coordinator.cancel("missing", 10).is_none());
    }
}
