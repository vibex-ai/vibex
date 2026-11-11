//! Background provisioning service.
//!
//! Drives the effects runner on a dedicated thread inside the caller's Tokio
//! runtime and forwards snapshots over a channel. The workbench can drain that
//! channel from its own task without making the effect futures `Send`.
//!
//! The runtime is the caller's on purpose. A run ends by handing a connected
//! backend to the workbench, and a socket belongs to the runtime that created
//! it for its whole life: a private runtime here would be dropped with the
//! worker, and the first write from the installed client would fail with
//! "A Tokio 1.x context was found, but it is being shutdown."

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
        runtime: &tokio::runtime::Handle,
        effects: E,
        profile_id: String,
        now_ms: i64,
        retry_failed: bool,
    ) -> Result<Self, ProvisioningServiceError> {
        Self::start_with_cancellation(
            runtime,
            effects,
            profile_id,
            now_ms,
            retry_failed,
            RunCancellation::new(),
        )
    }

    /// Starts a run on `runtime`, which must outlive the backend it produces.
    ///
    /// The run is driven by a dedicated thread so the effects future never has
    /// to be `Send`, but the future is entered through `runtime` rather than a
    /// runtime of this service's own. A current-thread runtime cannot be
    /// driven this way — `Handle::block_on` does not run its I/O and timer
    /// drivers — so it is rejected instead of hanging the run.
    pub fn start_with_cancellation(
        runtime: &tokio::runtime::Handle,
        mut effects: E,
        profile_id: String,
        now_ms: i64,
        retry_failed: bool,
        cancel: RunCancellation,
    ) -> Result<Self, ProvisioningServiceError> {
        if runtime.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread {
            return Err(ProvisioningServiceError::new(
                "ssh_provisioning_runtime_unsupported",
                "SSH provisioning needs a multi-thread runtime, because the \
                 connected backend outlives the run",
            ));
        }
        let runtime = runtime.clone();
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

    /// The runtime a test run borrows. It outlives the run exactly as the
    /// workbench's does, so the socket the effects would open stays usable.
    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a multi-thread runtime for the run")
    }

    struct FakeEffects {
        calls: Vec<&'static str>,
        cancel_at: Option<&'static str>,
        cancellation: RunCancellation,
        /// Where a stand-in for the runtime's WebSocket connects. The socket it
        /// produces outlives the run, exactly like the backend it stands in for.
        address: Option<std::net::SocketAddr>,
        socket: Option<tokio::net::TcpStream>,
        /// Set when the connect step ran inside a runtime at all. A run driven
        /// by a runtime the service owns would leave this naming a runtime that
        /// no longer exists by the time the caller sees the effects.
        connect_runtime: Option<tokio::runtime::Handle>,
    }

    impl FakeEffects {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                cancel_at: None,
                cancellation: RunCancellation::new(),
                address: None,
                socket: None,
                connect_runtime: None,
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
            self.record("connect")?;
            self.connect_runtime = tokio::runtime::Handle::try_current().ok();
            if let Some(address) = self.address {
                let socket = tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(|error| {
                        ProvisioningEffectError::new("fake_connect_failed", error.to_string())
                    })?;
                self.socket = Some(socket);
            }
            Ok(())
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
        let runtime = test_runtime();
        let mut service = ProvisioningService::start(
            runtime.handle(),
            FakeEffects::new(),
            "profile-1".to_string(),
            10,
            false,
        )
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
        let runtime = test_runtime();
        let cancellation = RunCancellation::new();
        let effects = FakeEffects {
            cancel_at: Some("install"),
            cancellation: cancellation.clone(),
            ..FakeEffects::new()
        };
        let mut service = ProvisioningService::start_with_cancellation(
            runtime.handle(),
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

    /// The bug this service exists to avoid: a socket opened during the run has
    /// to keep working after the run hands its backend to the workbench.
    ///
    /// A private current-thread runtime inside the worker reproduces the field
    /// failure exactly — the backend is installed, and the first catalogue RPC
    /// comes back as `remote_socket_write_failed: … A Tokio 1.x context was
    /// found, but it is being shutdown.` — so this test writes on the socket
    /// after `join`, from the caller's runtime, and fails if the runtime that
    /// opened it was dropped.
    #[test]
    fn a_socket_opened_by_the_run_still_writes_after_the_worker_exits() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            use std::io::Read as _;
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut received = [0u8; 2];
            socket.read_exact(&mut received).unwrap();
            received
        });

        let runtime = test_runtime();
        let effects = FakeEffects {
            address: Some(address),
            ..FakeEffects::new()
        };
        let mut service = ProvisioningService::start(
            runtime.handle(),
            effects,
            "profile-1".to_string(),
            10,
            false,
        )
        .unwrap();
        let (_, _, mut effects) = drain(&service);
        service.join().unwrap();

        // The run is over and its worker thread is gone; the caller's runtime
        // is the only one left, and it is the one the installed client uses.
        assert_eq!(
            effects.connect_runtime.as_ref().map(|handle| handle.id()),
            Some(runtime.handle().id()),
            "connect must run on the caller's runtime, not a private one"
        );
        use tokio::io::AsyncWriteExt as _;
        let mut socket = effects.socket.take().expect("connect opened a socket");
        runtime
            .block_on(async move { socket.write_all(b"ok").await })
            .expect("a socket opened by the run stays writable after it ends");
        assert_eq!(&server.join().unwrap(), b"ok");
    }

    #[test]
    fn a_current_thread_runtime_is_rejected_instead_of_hanging_the_run() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = match ProvisioningService::start(
            runtime.handle(),
            FakeEffects::new(),
            "profile-1".to_string(),
            10,
            false,
        ) {
            Ok(_) => panic!("a current-thread runtime cannot drive the run"),
            Err(error) => error,
        };
        assert_eq!(error.code, "ssh_provisioning_runtime_unsupported");
    }
}
