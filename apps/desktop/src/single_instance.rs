//! One desktop window per runtime home.
//!
//! The runtime home lock (`.vibex-runtime.lock`) already makes a second runtime
//! owner impossible, but it fails *after* the second shell has opened its own
//! window: the reader gets a "local runtime could not start" screen reporting
//! `desktop_runtime_home_locked` instead of the Vibex they were already using.
//! Opening Vibex again is a request to *see* that window, so the shell answers
//! it before it builds anything:
//!
//! * the first launch takes `<home>/.vibex-desktop-shell.lock`, binds a
//!   loopback activation listener, and records its address and a per-launch
//!   token in `<home>/.vibex-desktop-shell.json` (owner-only);
//! * every later launch finds the lock taken, asks the recorded listener to
//!   come forward, waits for the acknowledgement, and exits without opening a
//!   window.
//!
//! The port and token travel through that private file rather than a fixed
//! port, so the three channels (preview / RC / stable) never contend, and
//! another local user cannot drive the running window.
//!
//! The lock is advisory and process-scoped, so a crashed shell releases it
//! without cleanup and the next launch simply becomes the primary. A stale
//! record that outlives a crash is ignored: a launch that cannot reach the
//! listener starts normally, and the runtime's own retry and error screen then
//! explain the home it could not take.
//!
//! A home owned by something that is not this desktop shell — the TUI in
//! authority mode, or `vibex-server` — is *not* this module's business: the
//! shell lock is free, the launch continues, and the runtime still reports
//! `desktop_runtime_home_locked` with the screen that explains it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;
use vibex_core::unix_timestamp_ms;
use vibex_desktop_runtime::DesktopRuntimeConfig;

/// The shell-wide lock a live workbench holds for its lifetime.
pub const SHELL_LOCK_FILE: &str = ".vibex-desktop-shell.lock";
/// The record a later launch reads to reach the live shell.
pub const SHELL_ENDPOINT_FILE: &str = ".vibex-desktop-shell.json";
const SHELL_ENDPOINT_TEMP_FILE: &str = ".vibex-desktop-shell.json.tmp";
const ENDPOINT_SCHEMA_VERSION: &str = "vibex-desktop-shell.v1";
const ACTIVATION_SCHEMA_VERSION: &str = "vibex-desktop-shell-activation.v1";
/// How long one activation round trip may take. The listener is bound before
/// the record is published, so a healthy shell answers almost at once.
const ACTIVATION_TIMEOUT: Duration = Duration::from_millis(750);
const ACTIVATION_ATTEMPTS: u32 = 8;
const ACTIVATION_RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_ACTIVATION_LINE_BYTES: u64 = 4 * 1024;

/// What this launch should do about the home it was started for.
#[derive(Debug)]
pub enum LaunchClaim {
    /// No other shell owns the home: this process is the workbench, and it
    /// answers later launches through the returned handle.
    Primary(ShellClaim),
    /// Another live shell owns the home and has been asked to come forward.
    /// This process must exit without a window.
    Attached,
    /// Launch without claiming the home: the single-instance escape hatch is
    /// set, or the live shell could not be reached and the runtime's own home
    /// lock has to explain itself instead.
    Unguarded,
}

/// The primary shell's ownership of one runtime home.
///
/// Keeping the claim separate from the GPUI global (which just holds it alive)
/// is what lets a test exercise the launch decision without starting an
/// application.
#[derive(Debug)]
pub struct ShellClaim {
    endpoint: ShellEndpoint,
    endpoint_path: PathBuf,
    /// Held for the lifetime of the shell; the kernel releases the lock when
    /// the process exits, including after a crash.
    lock: File,
    activations: tokio_mpsc::UnboundedReceiver<()>,
}

/// Where the live shell answers, as later launches read it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellEndpoint {
    schema_version: String,
    application_id: String,
    process_id: u32,
    port: u16,
    token: String,
    started_at_ms: i64,
}

/// One launch asking the live shell to come forward.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivationRequest {
    schema_version: String,
    token: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivationResponse {
    schema_version: String,
    status: String,
}

impl ActivationResponse {
    fn new(status: &str) -> Self {
        Self {
            schema_version: ACTIVATION_SCHEMA_VERSION.to_string(),
            status: status.to_string(),
        }
    }
}

impl ShellClaim {
    fn claim(config: &DesktopRuntimeConfig) -> io::Result<Option<Self>> {
        fs::create_dir_all(&config.home_dir)?;
        let lock = open_private(&config.home_dir.join(SHELL_LOCK_FILE))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(None),
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let endpoint = ShellEndpoint {
            schema_version: ENDPOINT_SCHEMA_VERSION.to_string(),
            application_id: config.application_id.clone(),
            process_id: std::process::id(),
            port: listener.local_addr()?.port(),
            token: new_token(),
            started_at_ms: unix_timestamp_ms(),
        };
        // The listener is answering before the record exists, so a launch that
        // reads the record never dials a port nobody is listening on.
        let (sender, activations) = tokio_mpsc::unbounded_channel();
        let token = endpoint.token.clone();
        std::thread::Builder::new()
            .name("vibex-shell-activation".to_string())
            .spawn(move || serve_activations(listener, token, sender))?;

        let endpoint_path = config.home_dir.join(SHELL_ENDPOINT_FILE);
        // Publishing the record is what lets the next launch find this window,
        // so a shell that cannot publish it must not pretend to be the primary:
        // returning the error drops the lock.
        write_endpoint(&endpoint_path, &endpoint)?;
        Ok(Some(Self {
            endpoint,
            endpoint_path,
            lock,
            activations,
        }))
    }

    fn take_activations(&mut self) -> tokio_mpsc::UnboundedReceiver<()> {
        std::mem::replace(&mut self.activations, tokio_mpsc::unbounded_channel().1)
    }
}

impl Drop for ShellClaim {
    fn drop(&mut self) {
        // A successor may already have replaced the record before this shell
        // shut down; only a record that still names this launch is ours.
        let ours = fs::read_to_string(&self.endpoint_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<ShellEndpoint>(&raw).ok())
            .is_some_and(|endpoint| endpoint.token == self.endpoint.token);
        if ours {
            let _ = fs::remove_file(&self.endpoint_path);
        }
        let _ = self.lock.unlock();
    }
}

/// Claims this launch for the shell that owns `config.home_dir`.
pub fn claim_launch(config: &DesktopRuntimeConfig) -> LaunchClaim {
    if !single_instance_enabled() {
        return LaunchClaim::Unguarded;
    }
    match ShellClaim::claim(config) {
        Ok(Some(claim)) => LaunchClaim::Primary(claim),
        Ok(None) => match notify_with(
            &config.home_dir,
            ACTIVATION_ATTEMPTS,
            ACTIVATION_RETRY_DELAY,
        ) {
            NotifyOutcome::Activated => LaunchClaim::Attached,
            // Something holds the lock but will not take a request from us. A
            // silent no-op would leave the reader with nothing at all, so this
            // falls back to the normal launch and its explanation.
            NotifyOutcome::Unreachable => {
                eprintln!(
                    "vibex-desktop: the running Vibex shell did not answer its activation \
                     channel; opening this launch instead"
                );
                LaunchClaim::Unguarded
            }
        },
        Err(error) => {
            // A home that cannot hold the shell lock is a home the runtime is
            // about to report on anyway; the window must still open so that
            // report is visible.
            eprintln!("vibex-desktop: single-instance launch handling unavailable: {error}");
            LaunchClaim::Unguarded
        }
    }
}

/// Keeps the claim for the process lifetime and forwards every activation
/// request to the running workbench.
pub fn install(mut claim: ShellClaim, cx: &mut App) {
    let activations = claim.take_activations();
    cx.set_global(ShellClaimGlobal { _claim: claim });
    cx.spawn(async move |cx| {
        let mut activations = activations;
        while activations.recv().await.is_some() {
            cx.update(crate::system_tray::handle_second_launch);
        }
    })
    .detach();
}

/// The claim lives in a global so the lock and the record stay held for as long
/// as the process does, without threading a handle through every call site.
struct ShellClaimGlobal {
    _claim: ShellClaim,
}

impl Global for ShellClaimGlobal {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotifyOutcome {
    /// The live shell accepted the request and is coming forward.
    Activated,
    /// No live shell answered within the attempt budget.
    Unreachable,
}

fn notify_with(home: &Path, attempts: u32, delay: Duration) -> NotifyOutcome {
    let path = home.join(SHELL_ENDPOINT_FILE);
    let mut unanswered = 0;
    for attempt in 0..attempts {
        if attempt > 0 {
            std::thread::sleep(delay);
        }
        let endpoint = match read_endpoint(&path) {
            Ok(Some(endpoint)) => endpoint,
            // The primary publishes the record right after taking the lock; a
            // launch that arrives a moment early simply tries again.
            Ok(None) => continue,
            Err(()) => return NotifyOutcome::Unreachable,
        };
        match send_activation(&endpoint) {
            Ok(true) => return NotifyOutcome::Activated,
            // Either the record was replaced under us or the port is dead. A
            // fresh read and one more try covers both; a third failure would
            // only keep the reader waiting on a shell that cannot answer.
            Ok(false) | Err(_) => {
                unanswered += 1;
                if unanswered >= 2 {
                    return NotifyOutcome::Unreachable;
                }
            }
        }
    }
    NotifyOutcome::Unreachable
}

fn read_endpoint(path: &Path) -> Result<Option<ShellEndpoint>, ()> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let endpoint: ShellEndpoint = serde_json::from_str(&raw).map_err(|_| ())?;
    if endpoint.schema_version != ENDPOINT_SCHEMA_VERSION || endpoint.port == 0 {
        return Err(());
    }
    Ok(Some(endpoint))
}

fn send_activation(endpoint: &ShellEndpoint) -> io::Result<bool> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, ACTIVATION_TIMEOUT)?;
    stream.set_read_timeout(Some(ACTIVATION_TIMEOUT))?;
    stream.set_write_timeout(Some(ACTIVATION_TIMEOUT))?;
    let request = ActivationRequest {
        schema_version: ACTIVATION_SCHEMA_VERSION.to_string(),
        token: endpoint.token.clone(),
    };
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let line = read_bounded_line(&mut reader)?;
    let response: ActivationResponse = serde_json::from_str(&line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "malformed activation answer"))?;
    if response.schema_version != ACTIVATION_SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected activation answer schema",
        ));
    }
    Ok(response.status == "accepted")
}

fn serve_activations(
    listener: TcpListener,
    token: String,
    sender: tokio_mpsc::UnboundedSender<()>,
) {
    for stream in listener.incoming().flatten() {
        if !serve_activation(stream, &token, &sender) {
            // The workbench dropped the receiver: nothing can be brought
            // forward anymore, so the thread can stop with it.
            break;
        }
    }
}

/// Answers one connection. Returns false only when the shell is going away.
fn serve_activation(
    mut stream: TcpStream,
    token: &str,
    sender: &tokio_mpsc::UnboundedSender<()>,
) -> bool {
    let _ = stream.set_read_timeout(Some(ACTIVATION_TIMEOUT));
    let _ = stream.set_write_timeout(Some(ACTIVATION_TIMEOUT));
    let authorized = read_activation_token(&stream).is_some_and(|candidate| {
        // Constant-time comparison: the token is as good as a password for
        // whoever can reach this loopback port.
        candidate.len() == token.len()
            && candidate
                .bytes()
                .zip(token.bytes())
                .fold(0u8, |difference, (candidate, expected)| {
                    difference | (candidate ^ expected)
                })
                == 0
    });
    if !authorized {
        let _ = write_activation_response(&mut stream, "rejected");
        return true;
    }
    let queued = sender.send(()).is_ok();
    let _ = write_activation_response(&mut stream, if queued { "accepted" } else { "rejected" });
    queued
}

fn read_activation_token(stream: &TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let line = read_bounded_line(&mut reader).ok()?;
    let request: ActivationRequest = serde_json::from_str(&line).ok()?;
    (request.schema_version == ACTIVATION_SCHEMA_VERSION).then_some(request.token)
}

/// Reads one newline-terminated line, refusing to buffer more than a request
/// can legitimately take.
fn read_bounded_line(reader: &mut BufReader<TcpStream>) -> io::Result<String> {
    let mut line = String::new();
    let read = reader
        .take(MAX_ACTIVATION_LINE_BYTES)
        .read_line(&mut line)?;
    if read == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(line)
}

fn write_activation_response(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, &ActivationResponse::new(status))?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn write_endpoint(path: &Path, endpoint: &ShellEndpoint) -> io::Result<()> {
    let temporary = path.with_file_name(SHELL_ENDPOINT_TEMP_FILE);
    // Truncate: a record left by a crash is longer than the new one whenever
    // the port or the pid got shorter, and a half-overwritten record is exactly
    // the file the next launch would refuse to parse.
    let mut file = create_private(&temporary)?;
    let bytes = serde_json::to_vec(endpoint).map_err(io::Error::other)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    replace_file(&temporary, path)?;
    if let Some(parent) = path.parent() {
        // The rename is what publishes the record; syncing the directory keeps
        // it there across a crash, which is exactly when it is read again.
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
    }
    Ok(())
}

/// Puts `source` at `destination`, replacing any earlier record.
#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

/// Windows `rename` refuses to replace an existing destination, so the record
/// from the previous run is removed first. A launch that reads the gap simply
/// sees no record yet and tries again.
#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    match fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(source, destination)
}

/// Opens `path` for reading and writing without truncating it: the lock file is
/// a token whose contents never matter, and recreating it would drop the lock.
fn open_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    open_private_with(options, path)
}

fn create_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(true);
    open_private_with(options, path)
}

fn open_private_with(mut options: OpenOptions, path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // `mode` only applies to a file this call created; a record or lock
        // left by an older run keeps its own bits until this reset.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn new_token() -> String {
    format!("{}-{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Whether this launch should still defer to a running shell.
///
/// The escape hatch exists for development and for a harness that needs two
/// independent shells on one home; the product default is one window per home.
fn single_instance_enabled() -> bool {
    enabled_from(std::env::var("VIBEX_DESKTOP_SINGLE_INSTANCE").ok())
}

fn enabled_from(value: Option<String>) -> bool {
    match value {
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(home: &Path) -> DesktopRuntimeConfig {
        DesktopRuntimeConfig::isolated_test(home)
    }

    fn wait_for_activation(receiver: &mut tokio_mpsc::UnboundedReceiver<()>) -> bool {
        for _ in 0..200 {
            if receiver.try_recv().is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn the_first_launch_owns_the_home_and_the_next_one_defers() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let first = ShellClaim::claim(&config)
            .expect("take the shell lock")
            .expect("the home starts unowned");
        assert!(
            ShellClaim::claim(&config)
                .expect("probe the shell lock")
                .is_none(),
            "a second launch must not become a second window"
        );
        drop(first);
        assert!(
            ShellClaim::claim(&config)
                .expect("reclaim the shell lock")
                .is_some(),
            "the lock must be released when the shell exits"
        );
    }

    #[test]
    fn separate_homes_are_separate_shells() {
        let directory = tempfile::tempdir().unwrap();
        let preview = config(&directory.path().join("preview"));
        let stable = config(&directory.path().join("stable"));
        let _preview = ShellClaim::claim(&preview).unwrap().unwrap();
        let _stable = ShellClaim::claim(&stable).unwrap().unwrap();
    }

    #[test]
    fn a_second_launch_brings_the_first_shell_forward() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let mut primary = ShellClaim::claim(&config).unwrap().unwrap();
        let mut activations = primary.take_activations();

        assert_eq!(
            notify_with(directory.path(), 4, Duration::from_millis(20)),
            NotifyOutcome::Activated
        );
        assert!(
            wait_for_activation(&mut activations),
            "the live shell must be told to come forward"
        );
        assert_eq!(
            notify_with(directory.path(), 1, Duration::ZERO),
            NotifyOutcome::Activated,
            "a later launch is answered too"
        );
        assert!(wait_for_activation(&mut activations));
    }

    /// The listener answers before the workbench exists, so a launch during
    /// startup is queued rather than refused.
    #[test]
    fn an_activation_arriving_before_the_workbench_is_queued() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let mut primary = ShellClaim::claim(&config).unwrap().unwrap();
        assert_eq!(
            notify_with(directory.path(), 1, Duration::ZERO),
            NotifyOutcome::Activated
        );
        let mut activations = primary.take_activations();
        assert!(wait_for_activation(&mut activations));
    }

    #[test]
    fn a_request_without_the_token_never_reaches_the_shell() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let mut primary = ShellClaim::claim(&config).unwrap().unwrap();
        let mut activations = primary.take_activations();

        let record = directory.path().join(SHELL_ENDPOINT_FILE);
        let endpoint: ShellEndpoint =
            serde_json::from_str(&fs::read_to_string(record).unwrap()).unwrap();
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
        let mut stream = TcpStream::connect_timeout(&address, ACTIVATION_TIMEOUT).unwrap();
        stream.set_read_timeout(Some(ACTIVATION_TIMEOUT)).unwrap();
        let request = ActivationRequest {
            schema_version: ACTIVATION_SCHEMA_VERSION.to_string(),
            token: "not the token".to_string(),
        };
        serde_json::to_writer(&mut stream, &request).unwrap();
        stream.write_all(b"\n").unwrap();
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer).unwrap();
        assert!(answer.contains("rejected"), "answer was {answer:?}");
        assert!(
            !wait_for_activation(&mut activations),
            "an unauthenticated request must not move the window"
        );
    }

    #[test]
    fn a_stale_record_times_out_instead_of_hanging() {
        let directory = tempfile::tempdir().unwrap();
        // Bind a port and give it back: the record now names nobody, the way a
        // crashed shell leaves it.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let endpoint = ShellEndpoint {
            schema_version: ENDPOINT_SCHEMA_VERSION.to_string(),
            application_id: "dev.vibex.desktop.test".to_string(),
            process_id: 0,
            port,
            token: new_token(),
            started_at_ms: 0,
        };
        write_endpoint(&directory.path().join(SHELL_ENDPOINT_FILE), &endpoint).unwrap();
        assert_eq!(
            notify_with(directory.path(), 2, Duration::from_millis(10)),
            NotifyOutcome::Unreachable
        );
        // A crashed shell released the lock with it, so the next launch is the
        // primary and simply replaces the record the dead listener left behind.
        assert!(
            matches!(
                claim_launch(&config(directory.path())),
                LaunchClaim::Primary(_)
            ),
            "a stale record must not stop the next launch from opening a window"
        );
    }

    #[test]
    fn a_launch_on_an_owned_home_attaches_and_the_record_is_private() {
        let directory = tempfile::tempdir().unwrap();
        let config = config(directory.path());
        let LaunchClaim::Primary(_primary) = claim_launch(&config) else {
            panic!("the first launch must own the home");
        };
        assert!(
            matches!(claim_launch(&config), LaunchClaim::Attached),
            "the second launch must not open a window"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(directory.path().join(SHELL_ENDPOINT_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the activation token is a secret");
        }
    }

    #[test]
    fn the_escape_hatch_disables_the_claim() {
        assert!(enabled_from(None));
        assert!(enabled_from(Some("1".to_string())));
        for disabled in ["0", "false", "no", "off", " OFF "] {
            assert!(
                !enabled_from(Some(disabled.to_string())),
                "{disabled:?} must start an independent shell"
            );
        }
    }

    #[test]
    fn the_entry_point_claims_the_shell_before_it_opens_a_window() {
        let entry_point = include_str!("main.rs");
        let claim = entry_point
            .find("app::claim_workbench_launch()")
            .expect("the workbench launch must claim the shell");
        let open = entry_point
            .find("app::open_workbench_window(cx)")
            .expect("the workbench launch must open its window");
        assert!(
            claim < open,
            "a launch has to know whether it is the primary before it builds a window"
        );
        assert!(
            entry_point.contains("single_instance::install("),
            "the primary shell must keep its activation listener alive"
        );
        let attached = entry_point
            .find("LaunchClaim::Attached =>")
            .expect("the attached branch must exist");
        assert!(
            entry_point[attached..open].contains("return;"),
            "a launch that attached to the live shell must exit without a window"
        );
    }
}
