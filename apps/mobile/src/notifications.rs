use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use vibex_core::{AgentNotificationIntent, AgentNotificationKind, unix_timestamp_ms};

use crate::locale;

const MAX_PENDING_ACTIONS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationAction {
    pub notification_id: String,
    pub opaque_locator: String,
}

#[derive(Default)]
struct ActionState {
    sender: Option<UnboundedSender<NotificationAction>>,
    pending: VecDeque<NotificationAction>,
}

fn action_state() -> &'static Mutex<ActionState> {
    static STATE: OnceLock<Mutex<ActionState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(ActionState::default()))
}

fn enqueue_action(notification_id: &str, opaque_locator: &str) {
    if !valid_action_value(notification_id, 256) || !valid_action_value(opaque_locator, 512) {
        return;
    }
    let action = NotificationAction {
        notification_id: notification_id.to_string(),
        opaque_locator: opaque_locator.to_string(),
    };
    let Ok(mut state) = action_state().lock() else {
        return;
    };
    if state
        .sender
        .as_ref()
        .is_some_and(|sender| sender.unbounded_send(action.clone()).is_ok())
    {
        return;
    }
    state.sender = None;
    if state.pending.back() != Some(&action) {
        if state.pending.len() == MAX_PENDING_ACTIONS {
            state.pending.pop_front();
        }
        state.pending.push_back(action);
    }
}

fn valid_action_value(value: &str, max_len: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_len && !value.chars().any(char::is_control)
}

pub fn subscribe_actions() -> UnboundedReceiver<NotificationAction> {
    let (sender, receiver) = mpsc::unbounded();
    if let Ok(mut state) = action_state().lock() {
        while let Some(action) = state.pending.pop_front() {
            if sender.unbounded_send(action).is_err() {
                break;
            }
        }
        state.sender = Some(sender);
    }
    receiver
}

pub fn present(intent: &AgentNotificationIntent) {
    if intent.expires_at_ms <= unix_timestamp_ms() {
        return;
    }
    let (title, body) = notification_copy(&intent.kind);
    platform::present(&intent.notification_id, title, body, &intent.opaque_locator);
}

pub fn request_authorization() {
    platform::request_authorization();
}

fn notification_copy(kind: &AgentNotificationKind) -> (&'static str, &'static str) {
    match kind {
        AgentNotificationKind::ApprovalRequired { .. } => (
            "Vibex",
            locale::text(
                "An Agent operation is waiting for approval",
                "有一项 Agent 操作等待批准",
                "有一項 Agent 操作等待批准",
            ),
        ),
        AgentNotificationKind::InputRequired { .. } => (
            "Vibex",
            locale::text(
                "An Agent is waiting for your input",
                "Agent 正在等待你的输入",
                "Agent 正在等待你的輸入",
            ),
        ),
        AgentNotificationKind::TurnFailed => (
            "Vibex",
            locale::text(
                "An Agent turn failed",
                "Agent 回合执行失败",
                "Agent 回合執行失敗",
            ),
        ),
        AgentNotificationKind::TurnCompleted => (
            "Vibex",
            locale::text(
                "An Agent completed its work",
                "Agent 已完成工作",
                "Agent 已完成工作",
            ),
        ),
    }
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

    fn with_activity(
        call: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<()>,
    ) {
        let Some(app) = android_app().lock().ok().and_then(|app| app.clone()) else {
            return;
        };
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
        let _ = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
            call(env, &activity)
        });
    }

    pub fn request_authorization() {
        with_activity(|env, activity| {
            env.call_method(
                activity,
                jni::jni_str!("requestNotificationAuthorization"),
                jni::jni_sig!(() -> ()),
                &[],
            )?;
            Ok(())
        });
    }

    pub fn present(notification_id: &str, title: &str, body: &str, opaque_locator: &str) {
        with_activity(|env, activity| {
            let notification_id = env.new_string(notification_id)?;
            let title = env.new_string(title)?;
            let body = env.new_string(body)?;
            let opaque_locator = env.new_string(opaque_locator)?;
            env.call_method(
                activity,
                jni::jni_str!("showAgentNotification"),
                jni::jni_sig!((
                    notification_id: JString,
                    title: JString,
                    body: JString,
                    opaque_locator: JString,
                ) -> ()),
                &[
                    (&notification_id).into(),
                    (&title).into(),
                    (&body).into(),
                    (&opaque_locator).into(),
                ],
            )?;
            Ok(())
        });
    }
}

#[cfg(target_os = "android")]
pub use platform::initialize as initialize_android;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_vibex_mobile_GpuiNativeActivity_nativeOnNotificationActivated<
    'caller,
>(
    mut unowned_env: jni::EnvUnowned<'caller>,
    _class: jni::objects::JClass<'caller>,
    notification_id: jni::objects::JString<'caller>,
    opaque_locator: jni::objects::JString<'caller>,
) {
    unowned_env
        .with_env(|_| -> jni::errors::Result<()> {
            enqueue_action(&notification_id.to_string(), &opaque_locator.to_string());
            Ok(())
        })
        .resolve::<jni::errors::LogErrorAndDefault>()
}

#[cfg(target_os = "ios")]
mod platform {
    use std::ffi::CString;

    unsafe extern "C" {
        fn vibex_ios_request_notification_authorization();
        fn vibex_ios_show_agent_notification(
            notification_id: *const std::ffi::c_char,
            title: *const std::ffi::c_char,
            body: *const std::ffi::c_char,
            opaque_locator: *const std::ffi::c_char,
        );
    }

    pub fn request_authorization() {
        unsafe { vibex_ios_request_notification_authorization() };
    }

    pub fn present(notification_id: &str, title: &str, body: &str, opaque_locator: &str) {
        let values =
            [notification_id, title, body, opaque_locator].map(|value| CString::new(value).ok());
        let [
            Some(notification_id),
            Some(title),
            Some(body),
            Some(opaque_locator),
        ] = values
        else {
            return;
        };
        unsafe {
            vibex_ios_show_agent_notification(
                notification_id.as_ptr(),
                title.as_ptr(),
                body.as_ptr(),
                opaque_locator.as_ptr(),
            );
        }
    }
}

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn vibex_mobile_notification_activated(
    notification_id: *const std::ffi::c_char,
    opaque_locator: *const std::ffi::c_char,
) {
    if notification_id.is_null() || opaque_locator.is_null() {
        return;
    }
    let notification_id = unsafe { std::ffi::CStr::from_ptr(notification_id) };
    let opaque_locator = unsafe { std::ffi::CStr::from_ptr(opaque_locator) };
    if let (Ok(notification_id), Ok(opaque_locator)) =
        (notification_id.to_str(), opaque_locator.to_str())
    {
        enqueue_action(notification_id, opaque_locator);
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod platform {
    pub fn request_authorization() {}
    pub fn present(_: &str, _: &str, _: &str, _: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn reset_actions() {
        *action_state().lock().unwrap() = ActionState::default();
    }

    #[test]
    fn activation_rejects_invalid_or_unbounded_values() {
        let _guard = test_lock();
        reset_actions();
        let mut receiver = subscribe_actions();
        enqueue_action("", "session_valid");
        enqueue_action("notification", "line\nbreak");
        enqueue_action(&"n".repeat(257), "session_valid");
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn activation_emits_a_valid_action_once() {
        let _guard = test_lock();
        reset_actions();
        let mut receiver = subscribe_actions();
        enqueue_action("notification-a", "opaque-session-locator");
        assert_eq!(
            receiver.try_recv().unwrap(),
            NotificationAction {
                notification_id: "notification-a".to_string(),
                opaque_locator: "opaque-session-locator".to_string(),
            }
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn activation_is_buffered_until_the_app_subscribes() {
        let _guard = test_lock();
        reset_actions();
        enqueue_action("notification-cold-start", "opaque-cold-start");
        let mut receiver = subscribe_actions();
        assert_eq!(
            receiver.try_recv().unwrap(),
            NotificationAction {
                notification_id: "notification-cold-start".to_string(),
                opaque_locator: "opaque-cold-start".to_string(),
            }
        );
    }
}
