//! Background provisioning service.
//!
//! Runs the effects runner on a dedicated current-thread Tokio runtime and
//! forwards snapshots over a channel. The workbench can drain that channel from
//! its own task without making the effect futures `Send`.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::provisioning::{
    ProvisioningControlError, ProvisioningCoordinator, ProvisioningSnapshot,
};
use super::provisioning_run::{
    ProvisioningEffects, ProvisioningRunOutcome, RunCancellation, run_provisioning_with_updates,
};

#[derive(Debug)]
pub enum ProvisioningServiceEvent<E> {
    Update(ProvisioningSnapshot),
    Finished {
        outcome: Result<ProvisioningRunOutcome, ProvisioningControlError>,
        effects: E,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisioningServiceError {
    pub code: &'static str,
    pub message: String,
}

impl ProvisioningServiceError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProvisioningServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProvisioningServiceError {}

pub struct ProvisioningService<E> {
    cancel: RunCancellation,
    events: Receiver<ProvisioningServiceEvent<E>>,
    worker: Option<JoinHandle<()>>,
}

impl<E> ProvisioningService<E>
where
    E: ProvisioningEffects + Send + 'static,
{
    pub fn start(
        effects: E,
        profile_id: String,
        now_ms: i64,
        retry_failed: bool,
    ) -> Result<Self, ProvisioningServiceError> {
        Self::start_with_cancellation(
            effects,
            profile_id,
            now_ms,
            retry_failed,
            RunCancellation::new(),
        )
    }

    pub fn start_with_cancellation(
        mut effects: E,
        profile_id: String,
        now_ms: i64,
        retry_failed: bool,
        cancel: RunCancellation,
    ) -> Result<Self, ProvisioningServiceError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                ProvisioningServiceError::new(
                    "ssh_provisioning_runtime_failed",
                    format!("the provisioning runtime could not be created: {error}"),
                )
            })?;
        let (sender, events) = mpsc::channel();
        let worker_cancel = cancel.clone();
        let worker = thread::Builder::new()
            .name("vibex-ssh-provisioning".to_string())
            .spawn(move || {
                let mut coordinator = ProvisioningCoordinator::default();
                let update_sender = sender.clone();
                let outcome = runtime.block_on(run_provisioning_with_updates(
                    &mut coordinator,
                    &profile_id,
                    now_ms,
                    &mut effects,
                    &worker_cancel,
                    retry_failed,
                    |snapshot| {
                        let _ = update_sender.send(ProvisioningServiceEvent::Update(snapshot));
                    },
                ));
                let _ = sender.send(ProvisioningServiceEvent::Finished { outcome, effects });
            })
            .map_err(|error| {
                ProvisioningServiceError::new(
                    "ssh_provisioning_worker_failed",
                    format!("the provisioning worker could not be started: {error}"),
                )
            })?;
        Ok(Self {
            cancel,
            events,
            worker: Some(worker),
        })
    }

    pub fn try_recv(&self) -> Result<ProvisioningServiceEvent<E>, TryRecvError> {
        self.events.try_recv()
    }

    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ProvisioningServiceEvent<E>, RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn join(&mut self) -> Result<(), ProvisioningServiceError> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker.join().map_err(|_| {
            ProvisioningServiceError::new(
                "ssh_provisioning_worker_panicked",
                "the provisioning worker stopped unexpectedly",
            )
        })
    }
}

impl<E> Drop for ProvisioningService<E> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_connection::provisioning_run::ProvisioningEffectError;

    struct FakeEffects {
        calls: Vec<&'static str>,
        cancel_at: Option<&'static str>,
        cancellation: RunCancellation,
    }

    impl FakeEffects {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                cancel_at: None,
                cancellation: RunCancellation::new(),
            }
        }

        fn record(&mut self, name: &'static str) -> Result<(), ProvisioningEffectError> {
            self.calls.push(name);
            if self.cancel_at == Some(name) {
                self.cancellation.cancel();
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
            Ok("runtime-1".to_string())
        }
    }

    fn drain(
        service: &ProvisioningService<FakeEffects>,
    ) -> (
        Vec<ProvisioningSnapshot>,
        ProvisioningRunOutcome,
        FakeEffects,
    ) {
        let mut updates = Vec::new();
        loop {
            match service.recv_timeout(Duration::from_secs(5)).unwrap() {
                ProvisioningServiceEvent::Update(snapshot) => updates.push(snapshot),
                ProvisioningServiceEvent::Finished { outcome, effects } => {
                    return (updates, outcome.unwrap(), effects);
                }
            }
        }
    }

    #[test]
    fn runs_effects_on_the_worker_and_emits_updates() {
        let mut service =
            ProvisioningService::start(FakeEffects::new(), "profile-1".to_string(), 10, false)
                .unwrap();
        let (updates, outcome, effects) = drain(&service);
        assert!(matches!(outcome, ProvisioningRunOutcome::Ready(_)));
        assert_eq!(effects.calls.len(), 9);
        assert!(
            updates
                .iter()
                .any(|snapshot| snapshot.step == crate::ssh_connection::ProvisioningStep::Ready)
        );
        assert!(
            updates
                .iter()
                .any(|snapshot| snapshot.active_runtime_id.is_some())
        );
        service.join().unwrap();
    }

    #[test]
    fn cancel_stops_the_worker_before_the_next_effect() {
        let cancellation = RunCancellation::new();
        let effects = FakeEffects {
            cancel_at: Some("install"),
            cancellation: cancellation.clone(),
            ..FakeEffects::new()
        };
        let mut service = ProvisioningService::start_with_cancellation(
            effects,
            "profile-1".to_string(),
            10,
            false,
            cancellation,
        )
        .unwrap();
        let (_, outcome, effects) = drain(&service);
        assert!(matches!(outcome, ProvisioningRunOutcome::Cancelled(_)));
        assert_eq!(effects.calls, vec!["resolve", "check", "upload", "install"]);
        service.join().unwrap();
    }
}
