use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures_channel::oneshot;
use vibex_backend::{AgentBackend as _, BackendError, BackendEvent, BackendResult};
use vibex_remote_client::{RemoteConnectionState, WebRemoteBackend};

use std::sync::Arc;

const RECONNECT_RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventRoute {
    ForwardToUi,
    PresentNotification,
    IgnoreUntilResume,
}

fn event_route(event: &BackendEvent, app_backgrounded: bool) -> EventRoute {
    if !app_backgrounded {
        EventRoute::ForwardToUi
    } else if matches!(event, BackendEvent::Notification(_)) {
        EventRoute::PresentNotification
    } else {
        EventRoute::IgnoreUntilResume
    }
}

fn process_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to initialize the mobile process runtime")
    })
}

pub fn tokio_handle() -> tokio::runtime::Handle {
    process_runtime().handle().clone()
}

#[derive(Default)]
struct ConnectionState {
    generation: u64,
    connect_attempt: u64,
    backend: Option<Arc<WebRemoteBackend>>,
    ui_sender: Option<UnboundedSender<BackendEvent>>,
    connect_task: Option<tokio::task::AbortHandle>,
    event_task: Option<tokio::task::AbortHandle>,
}

fn connection_state() -> &'static Mutex<ConnectionState> {
    static STATE: OnceLock<Mutex<ConnectionState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(ConnectionState::default()))
}

pub fn connect(
    backend: Arc<WebRemoteBackend>,
) -> oneshot::Receiver<BackendResult<vibex_core::RemoteServerInfoV2>> {
    let (sender, receiver) = oneshot::channel();
    let Ok(mut state) = connection_state().lock() else {
        let _ = sender.send(Err(state_unavailable()));
        return receiver;
    };

    let same_backend = state
        .backend
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &backend));
    let previous_backend = if same_backend {
        None
    } else {
        state.generation = state.generation.wrapping_add(1);
        if let Some(task) = state.event_task.take() {
            task.abort();
        }
        state.ui_sender = None;
        state.backend.replace(backend.clone())
    };
    state.connect_attempt = state.connect_attempt.wrapping_add(1);
    if let Some(task) = state.connect_task.take() {
        task.abort();
    }
    let generation = state.generation;
    let connect_attempt = state.connect_attempt;
    drop(state);

    if let Some(previous_backend) = previous_backend {
        platform::stop_service();
        tokio_handle().spawn(async move {
            let _ = previous_backend.disconnect().await;
        });
    }

    let task_backend = backend.clone();
    let task = tokio_handle().spawn(async move {
        let result = task_backend.connect().await;
        let current = is_current_attempt(generation, connect_attempt, &task_backend);
        let result = if current {
            if result.is_ok() {
                let _ = platform::start_service();
                start_event_reader(generation, task_backend.clone());
            }
            result
        } else {
            if result.is_ok() && !is_current_backend(generation, &task_backend) {
                let _ = task_backend.disconnect().await;
            }
            Err(BackendError::offline(
                "mobile_connection_superseded",
                "a newer mobile connection replaced this attempt",
            ))
        };
        clear_connect_task(generation, connect_attempt);
        let _ = sender.send(result);
    });
    let abort = task.abort_handle();
    drop(task);

    if let Ok(mut state) = connection_state().lock() {
        if state.generation == generation && state.connect_attempt == connect_attempt {
            state.connect_task = Some(abort);
        } else {
            abort.abort();
        }
    } else {
        abort.abort();
    }

    receiver
}

pub fn disconnect() {
    let backend = connection_state().lock().ok().and_then(|mut state| {
        state.generation = state.generation.wrapping_add(1);
        state.connect_attempt = state.connect_attempt.wrapping_add(1);
        if let Some(task) = state.connect_task.take() {
            task.abort();
        }
        if let Some(task) = state.event_task.take() {
            task.abort();
        }
        state.ui_sender = None;
        state.backend.take()
    });
    platform::stop_service();
    if let Some(backend) = backend {
        tokio_handle().spawn(async move {
            let _ = backend.disconnect().await;
        });
    }
}

pub fn subscribe_ui_events() -> UnboundedReceiver<BackendEvent> {
    let (sender, receiver) = mpsc::unbounded();
    if !crate::lifecycle::is_backgrounded()
        && let Ok(mut state) = connection_state().lock()
        && state.backend.is_some()
    {
        state.ui_sender = Some(sender);
    }
    receiver
}

pub fn suspend_ui_events() {
    if let Ok(mut state) = connection_state().lock() {
        state.ui_sender = None;
    }
}

fn start_event_reader(generation: u64, backend: Arc<WebRemoteBackend>) {
    let should_start = connection_state().lock().is_ok_and(|state| {
        state.generation == generation
            && state
                .backend
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &backend))
            && state.event_task.is_none()
    });
    if !should_start {
        return;
    }

    let task_backend = backend.clone();
    let cleanup_backend = backend.clone();
    let task = tokio_handle().spawn(async move {
        read_events(generation, task_backend).await;
        clear_event_task(generation, &cleanup_backend);
    });
    let abort = task.abort_handle();
    drop(task);

    if let Ok(mut state) = connection_state().lock() {
        if state.generation == generation
            && state
                .backend
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &backend))
            && state.event_task.is_none()
        {
            state.event_task = Some(abort);
        } else {
            abort.abort();
        }
    } else {
        abort.abort();
    }
}

async fn read_events(generation: u64, backend: Arc<WebRemoteBackend>) {
    let Ok(mut subscription) = backend.subscribe() else {
        stop_service_if_current(generation, &backend);
        return;
    };
    let mut disconnected_forwarded = false;

    loop {
        if !is_current_backend(generation, &backend) {
            return;
        }
        match subscription.next().await {
            Ok(Some(event)) => {
                disconnected_forwarded = matches!(event, BackendEvent::Disconnected);
                dispatch_event(generation, &backend, event);
            }
            Ok(None) | Err(_) => {
                if is_terminal(backend.connection_state().state) {
                    stop_service_if_current(generation, &backend);
                    return;
                }
                if !disconnected_forwarded && !crate::lifecycle::is_backgrounded() {
                    forward_to_ui(generation, &backend, BackendEvent::Disconnected);
                    disconnected_forwarded = true;
                }
                tokio::time::sleep(RECONNECT_RETRY_DELAY).await;
            }
        }
        if is_terminal(backend.connection_state().state) {
            stop_service_if_current(generation, &backend);
            return;
        }
    }
}

fn dispatch_event(generation: u64, backend: &Arc<WebRemoteBackend>, event: BackendEvent) {
    match event_route(&event, crate::lifecycle::is_backgrounded()) {
        EventRoute::PresentNotification => {
            if let BackendEvent::Notification(notification) = &event {
                crate::notifications::present(notification);
            }
        }
        EventRoute::IgnoreUntilResume => {}
        EventRoute::ForwardToUi => {
            let fallback_notification = match &event {
                BackendEvent::Notification(notification) => Some(notification.clone()),
                _ => None,
            };
            if !forward_to_ui(generation, backend, event)
                && let Some(notification) = fallback_notification
            {
                crate::notifications::present(&notification);
            }
        }
    }
}

fn forward_to_ui(generation: u64, backend: &Arc<WebRemoteBackend>, event: BackendEvent) -> bool {
    let Ok(mut state) = connection_state().lock() else {
        return false;
    };
    if state.generation != generation
        || !state
            .backend
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, backend))
    {
        return false;
    }
    let Some(sender) = state.ui_sender.as_ref() else {
        return false;
    };
    if sender.unbounded_send(event).is_ok() {
        true
    } else {
        state.ui_sender = None;
        false
    }
}

fn is_current_backend(generation: u64, backend: &Arc<WebRemoteBackend>) -> bool {
    connection_state().lock().is_ok_and(|state| {
        state.generation == generation
            && state
                .backend
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, backend))
    })
}

fn is_current_attempt(
    generation: u64,
    connect_attempt: u64,
    backend: &Arc<WebRemoteBackend>,
) -> bool {
    connection_state().lock().is_ok_and(|state| {
        state.generation == generation
            && state.connect_attempt == connect_attempt
            && state
                .backend
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, backend))
    })
}

fn clear_connect_task(generation: u64, connect_attempt: u64) {
    if let Ok(mut state) = connection_state().lock()
        && state.generation == generation
        && state.connect_attempt == connect_attempt
    {
        state.connect_task = None;
    }
}

fn clear_event_task(generation: u64, backend: &Arc<WebRemoteBackend>) {
    if let Ok(mut state) = connection_state().lock()
        && state.generation == generation
        && state
            .backend
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, backend))
    {
        state.event_task = None;
    }
}

fn stop_service_if_current(generation: u64, backend: &Arc<WebRemoteBackend>) {
    if is_current_backend(generation, backend) {
        platform::stop_service();
    }
}

fn is_terminal(state: RemoteConnectionState) -> bool {
    matches!(
        state,
        RemoteConnectionState::Revoked | RemoteConnectionState::Incompatible
    )
}

fn state_unavailable() -> BackendError {
    BackendError::failed(
        "mobile_connection_state_unavailable",
        "the mobile connection manager is unavailable",
    )
}

#[cfg(target_os = "android")]
mod platform {
    use std::sync::{Mutex, OnceLock};

    use gpui_android::AndroidApp;
    use jni::{JavaVM, objects::JObject, refs::Global};

    fn android_app() -> &'static Mutex<Option<AndroidApp>> {
        static APP: OnceLock<Mutex<Option<AndroidApp>>> = OnceLock::new();
        APP.get_or_init(|| Mutex::new(None))
    }

    pub fn initialize(app: &AndroidApp) {
        if let Ok(mut current) = android_app().lock() {
            *current = Some(app.clone());
        }
    }

    fn call_activity(method: &'static jni::strings::JNIStr) -> bool {
        let Some(app) = android_app().lock().ok().and_then(|app| app.clone()) else {
            return false;
        };
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
            env.call_method(activity, method, jni::jni_sig!(() -> ()), &[])?;
            Ok(())
        })
        .is_ok()
    }

    pub fn start_service() -> bool {
        call_activity(jni::jni_str!("startRemoteConnectionService"))
    }

    pub fn stop_service() {
        let _ = call_activity(jni::jni_str!("stopRemoteConnectionService"));
    }
}

#[cfg(target_os = "android")]
pub use platform::initialize as initialize_android;

#[cfg(not(target_os = "android"))]
mod platform {
    pub fn start_service() -> bool {
        true
    }

    pub fn stop_service() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANDROID_MANIFEST: &str = include_str!("../android/app/src/main/AndroidManifest.xml");
    const ANDROID_ACTIVITY: &str =
        include_str!("../android/app/src/main/java/ai/vibex/mobile/GpuiNativeActivity.java");
    const ANDROID_CONNECTION_SERVICE: &str =
        include_str!("../android/app/src/main/java/ai/vibex/mobile/RemoteConnectionService.java");
    const BACKGROUND_CONNECTION_SOURCE: &str = include_str!("background_connection.rs");

    fn notification() -> BackendEvent {
        BackendEvent::Notification(vibex_core::AgentNotificationIntent {
            notification_id: "notification-a".to_string(),
            source_event_id: vibex_core::TimelineItemId::new(),
            session_id: vibex_core::VibexSessionId::new(),
            kind: vibex_core::AgentNotificationKind::TurnCompleted,
            created_at_ms: 1,
            expires_at_ms: 2,
            opaque_locator: "opaque-session-locator".to_string(),
        })
    }

    #[test]
    fn background_reader_presents_only_notifications() {
        assert_eq!(
            event_route(&notification(), true),
            EventRoute::PresentNotification
        );
        assert_eq!(
            event_route(&BackendEvent::Disconnected, true),
            EventRoute::IgnoreUntilResume
        );
        assert_eq!(event_route(&notification(), false), EventRoute::ForwardToUi);
    }

    #[test]
    fn revoked_and_incompatible_connections_stop_background_delivery() {
        assert!(is_terminal(RemoteConnectionState::Revoked));
        assert!(is_terminal(RemoteConnectionState::Incompatible));
        assert!(!is_terminal(RemoteConnectionState::Online));
        assert!(!is_terminal(RemoteConnectionState::Offline));
    }

    #[test]
    fn android_background_connection_packaging_contract_is_complete() {
        assert!(ANDROID_MANIFEST.contains(
            r#"<uses-permission android:name="android.permission.FOREGROUND_SERVICE" />"#
        ));
        assert!(ANDROID_MANIFEST.contains(
            r#"<uses-permission android:name="android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE" />"#
        ));
        assert!(ANDROID_MANIFEST.contains(
            r#"<service
            android:name=".RemoteConnectionService"
            android:exported="false"
            android:foregroundServiceType="connectedDevice"
            android:stopWithTask="false" />"#
        ));
        assert!(ANDROID_CONNECTION_SERVICE.contains("return START_NOT_STICKY;"));

        for method in [
            "startRemoteConnectionService",
            "stopRemoteConnectionService",
        ] {
            assert!(
                ANDROID_ACTIVITY.contains(&format!("public void {method}()")),
                "Android Activity is missing the {method} JNI bridge"
            );
            assert!(
                BACKGROUND_CONNECTION_SOURCE.contains(&format!(r#"jni::jni_str!("{method}")"#)),
                "Rust is missing the {method} JNI call"
            );
        }
    }
}
