//! Sequenced provisioning effects.
//!
//! `ProvisioningMachine` owns step order; this module drives it with a set of
//! effects (resolve, probe, upload, install, start, tunnel, pair, connect,
//! activate). Real effects can wrap the SSH modules; tests use a fake to prove
//! ordering, cancel safety, retry and activation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::provisioning::{
    ProvisioningControlError, ProvisioningCoordinator, ProvisioningEvent, ProvisioningFailure,
    ProvisioningSnapshot, ProvisioningStatus, ProvisioningStep, ProvisioningUpdate,
};

#[derive(Debug, Clone, Default)]
pub struct RunCancellation {
    flag: Arc<AtomicBool>,
}

impl RunCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisioningEffectError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub log_tail: String,
}

impl ProvisioningEffectError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            log_tail: String::new(),
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn with_log_tail(mut self, log_tail: impl Into<String>) -> Self {
        self.log_tail = log_tail.into();
        self
    }
}

impl std::fmt::Display for ProvisioningEffectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProvisioningEffectError {}

/// The effects behind one provisioning attempt. Implementations hold their own
/// profile/config/artifact state; the runner only guarantees order and
/// cancellation safe points.
#[allow(async_fn_in_trait)]
pub trait ProvisioningEffects {
    async fn resolve(&mut self) -> Result<(), ProvisioningEffectError>;
    async fn check_environment(&mut self) -> Result<(), ProvisioningEffectError>;
    async fn upload(&mut self) -> Result<(), ProvisioningEffectError>;
    async fn install(&mut self) -> Result<(), ProvisioningEffectError>;
    async fn start(&mut self) -> Result<(), ProvisioningEffectError>;
    async fn open_tunnel(&mut self) -> Result<(), ProvisioningEffectError>;
    async fn pair(&mut self) -> Result<(), ProvisioningEffectError>;
    async fn connect(&mut self) -> Result<(), ProvisioningEffectError>;
    /// Called only after the final step. Returns the runtime id to activate.
    async fn activate(&mut self) -> Result<String, ProvisioningEffectError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisioningRunOutcome {
    Ready(ProvisioningSnapshot),
    Failed(ProvisioningSnapshot),
    Cancelled(ProvisioningSnapshot),
    AlreadyExists(ProvisioningSnapshot),
}

impl ProvisioningRunOutcome {
    pub fn snapshot(&self) -> &ProvisioningSnapshot {
        match self {
            Self::Ready(snapshot)
            | Self::Failed(snapshot)
            | Self::Cancelled(snapshot)
            | Self::AlreadyExists(snapshot) => snapshot,
        }
    }
}

/// Drives one profile to Ready.
///
/// `retry_failed` lets a caller resume an existing failed attempt; otherwise a
/// duplicate call returns `AlreadyExists` with the current snapshot and runs no
/// effects.
pub async fn run_provisioning<E: ProvisioningEffects>(
    coordinator: &mut ProvisioningCoordinator,
    profile_id: &str,
    now_ms: i64,
    effects: &mut E,
    cancellation: &RunCancellation,
    retry_failed: bool,
) -> Result<ProvisioningRunOutcome, ProvisioningControlError> {
    run_provisioning_with_updates(
        coordinator,
        profile_id,
        now_ms,
        effects,
        cancellation,
        retry_failed,
        |_| {},
    )
    .await
}

/// Same as [`run_provisioning`], but emits each snapshot after a step begins or
/// ends. The callback runs on the caller's thread/task.
pub async fn run_provisioning_with_updates<E, F>(
    coordinator: &mut ProvisioningCoordinator,
    profile_id: &str,
    now_ms: i64,
    effects: &mut E,
    cancellation: &RunCancellation,
    retry_failed: bool,
    mut on_update: F,
) -> Result<ProvisioningRunOutcome, ProvisioningControlError>
where
    E: ProvisioningEffects,
    F: FnMut(ProvisioningSnapshot),
{
    match coordinator.begin(profile_id, now_ms)? {
        super::provisioning::ProvisioningBegin::Started(update) => {
            on_update(update.snapshot);
        }
        super::provisioning::ProvisioningBegin::Existing(snapshot) => {
            if snapshot.status == ProvisioningStatus::Failed && retry_failed {
                let update = coordinator.retry(profile_id, now_ms)?;
                on_update(update.snapshot);
            } else {
                on_update(snapshot.clone());
                return Ok(ProvisioningRunOutcome::AlreadyExists(snapshot));
            }
        }
    }
    if cancellation.is_cancelled() {
        let outcome = stop_cancelled(coordinator, profile_id, now_ms);
        on_update(outcome.snapshot().clone());
        return Ok(outcome);
    }
    drive(
        coordinator,
        profile_id,
        now_ms,
        effects,
        cancellation,
        &mut on_update,
    )
    .await
}

async fn drive<E: ProvisioningEffects, F: FnMut(ProvisioningSnapshot)>(
    coordinator: &mut ProvisioningCoordinator,
    profile_id: &str,
    now_ms: i64,
    effects: &mut E,
    cancellation: &RunCancellation,
    on_update: &mut F,
) -> Result<ProvisioningRunOutcome, ProvisioningControlError> {
    loop {
        if cancellation.is_cancelled() {
            let outcome = stop_cancelled(coordinator, profile_id, now_ms);
            on_update(outcome.snapshot().clone());
            return Ok(outcome);
        }
        let step = current_step(coordinator, profile_id);
        if step == ProvisioningStep::Ready {
            return match effects.activate().await {
                Ok(runtime_id) => {
                    let machine = coordinator
                        .attempt_mut(profile_id)
                        .expect("provisioning attempt disappeared");
                    let update = machine.ready(runtime_id, now_ms)?;
                    on_update(update.snapshot.clone());
                    Ok(ProvisioningRunOutcome::Ready(update.snapshot))
                }
                Err(error) => {
                    let outcome = step_error(coordinator, profile_id, error, now_ms)?;
                    on_update(outcome.snapshot().clone());
                    Ok(outcome)
                }
            };
        }

        let result = match step {
            ProvisioningStep::ResolvingHost => effects.resolve().await,
            ProvisioningStep::CheckingEnvironment => effects.check_environment().await,
            ProvisioningStep::UploadingRuntime => effects.upload().await,
            ProvisioningStep::InstallingRuntime => effects.install().await,
            ProvisioningStep::StartingRuntime => effects.start().await,
            ProvisioningStep::OpeningTunnel => effects.open_tunnel().await,
            ProvisioningStep::Pairing => effects.pair().await,
            ProvisioningStep::Connecting => effects.connect().await,
            ProvisioningStep::Ready => unreachable!("ready is handled above"),
        };
        if let Err(error) = result {
            let outcome = step_error(coordinator, profile_id, error, now_ms)?;
            on_update(outcome.snapshot().clone());
            return Ok(outcome);
        }
        if cancellation.is_cancelled() {
            let outcome = stop_cancelled(coordinator, profile_id, now_ms);
            on_update(outcome.snapshot().clone());
            return Ok(outcome);
        }
        let update = complete(coordinator, profile_id, now_ms)?;
        on_update(update.snapshot.clone());
        if cancelled(&update) {
            return Ok(outcome(coordinator, profile_id));
        }
    }
}

fn stop_cancelled(
    coordinator: &mut ProvisioningCoordinator,
    profile_id: &str,
    now_ms: i64,
) -> ProvisioningRunOutcome {
    let _ = coordinator.cancel(profile_id, now_ms);
    if let Some(machine) = coordinator.attempt_mut(profile_id) {
        let _ = machine.poll_cancel(now_ms);
    }
    outcome(coordinator, profile_id)
}

fn outcome(coordinator: &ProvisioningCoordinator, profile_id: &str) -> ProvisioningRunOutcome {
    let snapshot = coordinator
        .snapshot(profile_id)
        .expect("provisioning attempt disappeared");
    match snapshot.status {
        ProvisioningStatus::Ready => ProvisioningRunOutcome::Ready(snapshot),
        ProvisioningStatus::Failed => ProvisioningRunOutcome::Failed(snapshot),
        ProvisioningStatus::Cancelled => ProvisioningRunOutcome::Cancelled(snapshot),
        ProvisioningStatus::Pending
        | ProvisioningStatus::Running
        | ProvisioningStatus::Cancelling => ProvisioningRunOutcome::AlreadyExists(snapshot),
    }
}

fn step_error(
    coordinator: &mut ProvisioningCoordinator,
    profile_id: &str,
    error: ProvisioningEffectError,
    now_ms: i64,
) -> Result<ProvisioningRunOutcome, ProvisioningControlError> {
    let machine = coordinator
        .attempt_mut(profile_id)
        .expect("provisioning attempt disappeared");
    let step = machine.step();
    let failure = ProvisioningFailure::new(step, error.code, error.message, error.retryable)
        .with_log_tail(error.log_tail);
    let update = machine.fail_with(failure, now_ms)?;
    Ok(ProvisioningRunOutcome::Failed(update.snapshot))
}

fn complete(
    coordinator: &mut ProvisioningCoordinator,
    profile_id: &str,
    now_ms: i64,
) -> Result<ProvisioningUpdate, ProvisioningControlError> {
    coordinator
        .attempt_mut(profile_id)
        .expect("provisioning attempt disappeared")
        .complete_step(now_ms)
}

fn current_step(coordinator: &ProvisioningCoordinator, profile_id: &str) -> ProvisioningStep {
    coordinator
        .snapshot(profile_id)
        .expect("provisioning attempt disappeared")
        .step
}

fn cancelled(update: &ProvisioningUpdate) -> bool {
    matches!(update.event, ProvisioningEvent::Cancelled { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EFFECTS: [&str; 9] = [
        "resolve", "check", "upload", "install", "start", "tunnel", "pair", "connect", "activate",
    ];

    struct FakeEffects {
        calls: Vec<&'static str>,
        fail_at: Option<&'static str>,
        cancel_at: Option<&'static str>,
        cancellation: RunCancellation,
        runtime_id: String,
    }

    impl FakeEffects {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                fail_at: None,
                cancel_at: None,
                cancellation: RunCancellation::new(),
                runtime_id: "runtime-1".to_string(),
            }
        }

        fn record(&mut self, name: &'static str) -> Result<(), ProvisioningEffectError> {
            self.calls.push(name);
            if self.cancel_at == Some(name) {
                self.cancellation.cancel();
            }
            if self.fail_at == Some(name) {
                return Err(ProvisioningEffectError::new(
                    format!("{name}_failed"),
                    format!("{name} failed"),
                )
                .retryable(true));
            }
            Ok(())
        }
    }

    #[allow(async_fn_in_trait)]
    impl ProvisioningEffects for FakeEffects {
        async fn resolve(&mut self) -> Result<(), ProvisioningEffectError> {
            self.record("resolve")
        }
        async fn check_environment(&mut self) -> Result<(), ProvisioningEffectError> {
            self.record("check")
        }
        async fn upload(&mut self) -> Result<(), ProvisioningEffectError> {
            self.record("upload")
        }
        async fn install(&mut self) -> Result<(), ProvisioningEffectError> {
            self.record("install")
        }
        async fn start(&mut self) -> Result<(), ProvisioningEffectError> {
            self.record("start")
        }
        async fn open_tunnel(&mut self) -> Result<(), ProvisioningEffectError> {
            self.record("tunnel")
        }
        async fn pair(&mut self) -> Result<(), ProvisioningEffectError> {
            self.record("pair")
        }
        async fn connect(&mut self) -> Result<(), ProvisioningEffectError> {
            self.record("connect")
        }
        async fn activate(&mut self) -> Result<String, ProvisioningEffectError> {
            self.record("activate")?;
            Ok(self.runtime_id.clone())
        }
    }

    async fn run_with(
        effects: &mut FakeEffects,
        retry_failed: bool,
    ) -> (
        ProvisioningRunOutcome,
        ProvisioningCoordinator,
        RunCancellation,
    ) {
        let mut coordinator = ProvisioningCoordinator::default();
        let cancellation = effects.cancellation.clone();
        let outcome = run_provisioning(
            &mut coordinator,
            "profile-1",
            10,
            effects,
            &cancellation,
            retry_failed,
        )
        .await
        .unwrap();
        (outcome, coordinator, cancellation)
    }

    #[tokio::test]
    async fn runs_every_effect_in_order_and_activates_only_at_ready() {
        let mut effects = FakeEffects::new();
        let (outcome, coordinator, _) = run_with(&mut effects, false).await;
        assert_eq!(effects.calls, EFFECTS);
        let ProvisioningRunOutcome::Ready(snapshot) = outcome else {
            panic!("expected ready");
        };
        assert_eq!(snapshot.active_runtime_id.as_deref(), Some("runtime-1"));
        assert_eq!(
            coordinator.snapshot("profile-1").unwrap().status,
            ProvisioningStatus::Ready
        );
    }

    #[tokio::test]
    async fn failure_stops_at_the_effect_and_retry_resumes_there() {
        for failure in EFFECTS {
            let mut effects = FakeEffects::new();
            effects.fail_at = Some(failure);
            let (outcome, mut coordinator, cancellation) = run_with(&mut effects, false).await;
            assert!(matches!(outcome, ProvisioningRunOutcome::Failed(_)));
            assert_eq!(effects.calls.last().copied(), Some(failure));
            assert_eq!(
                coordinator.snapshot("profile-1").unwrap().status,
                ProvisioningStatus::Failed
            );

            effects.fail_at = None;
            let outcome = run_provisioning(
                &mut coordinator,
                "profile-1",
                20,
                &mut effects,
                &cancellation,
                true,
            )
            .await
            .unwrap();
            assert!(matches!(outcome, ProvisioningRunOutcome::Ready(_)));
            let failed_index = EFFECTS.iter().position(|name| *name == failure).unwrap();
            assert_eq!(
                &effects.calls[effects.calls.len() - (EFFECTS.len() - failed_index)..],
                &EFFECTS[failed_index..]
            );
        }
    }

    #[tokio::test]
    async fn duplicate_run_returns_existing_without_effects() {
        let mut effects = FakeEffects::new();
        let (_, mut coordinator, cancellation) = run_with(&mut effects, false).await;
        let calls = effects.calls.clone();
        let outcome = run_provisioning(
            &mut coordinator,
            "profile-1",
            30,
            &mut effects,
            &cancellation,
            false,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, ProvisioningRunOutcome::AlreadyExists(_)));
        assert_eq!(effects.calls, calls);
    }

    #[tokio::test]
    async fn cancellation_after_an_effect_stops_before_the_next_one() {
        let mut effects = FakeEffects::new();
        effects.cancel_at = Some("install");
        let (outcome, coordinator, _) = run_with(&mut effects, false).await;
        assert!(matches!(outcome, ProvisioningRunOutcome::Cancelled(_)));
        assert_eq!(effects.calls, vec!["resolve", "check", "upload", "install"]);
        let snapshot = coordinator.snapshot("profile-1").unwrap();
        assert_eq!(snapshot.status, ProvisioningStatus::Cancelled);
        assert_eq!(snapshot.active_runtime_id, None);
    }

    #[tokio::test]
    async fn cancel_before_start_runs_no_effects() {
        let mut effects = FakeEffects::new();
        effects.cancellation.cancel();
        let (outcome, coordinator, _) = run_with(&mut effects, false).await;
        assert!(matches!(outcome, ProvisioningRunOutcome::Cancelled(_)));
        assert!(effects.calls.is_empty());
        assert_eq!(
            coordinator.snapshot("profile-1").unwrap().status,
            ProvisioningStatus::Cancelled
        );
    }
}
