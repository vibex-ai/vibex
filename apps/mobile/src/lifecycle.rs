use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use gpui::AppLifecyclePhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobileLifecycleEvent {
    Backgrounded,
    Resumed,
}

static APP_BACKGROUNDED: AtomicBool = AtomicBool::new(false);

fn event_sender() -> &'static Mutex<Option<UnboundedSender<MobileLifecycleEvent>>> {
    static SENDER: OnceLock<Mutex<Option<UnboundedSender<MobileLifecycleEvent>>>> = OnceLock::new();
    SENDER.get_or_init(|| Mutex::new(None))
}

fn enqueue(event: MobileLifecycleEvent) {
    APP_BACKGROUNDED.store(
        event == MobileLifecycleEvent::Backgrounded,
        Ordering::Release,
    );
    if event == MobileLifecycleEvent::Backgrounded {
        crate::background_connection::suspend_ui_events();
    }
    if let Ok(sender) = event_sender().lock()
        && let Some(sender) = sender.as_ref()
    {
        let _ = sender.unbounded_send(event);
    }
}

pub fn is_backgrounded() -> bool {
    APP_BACKGROUNDED.load(Ordering::Acquire)
}

fn transition(backgrounded: &mut bool, phase: AppLifecyclePhase) -> Option<MobileLifecycleEvent> {
    match phase {
        AppLifecyclePhase::Background if !*backgrounded => {
            *backgrounded = true;
            Some(MobileLifecycleEvent::Backgrounded)
        }
        AppLifecyclePhase::Foreground | AppLifecyclePhase::Active if *backgrounded => {
            *backgrounded = false;
            Some(MobileLifecycleEvent::Resumed)
        }
        AppLifecyclePhase::Active
        | AppLifecyclePhase::Inactive
        | AppLifecyclePhase::Background
        | AppLifecyclePhase::Foreground => None,
    }
}

pub fn attach(platform: &dyn gpui::Platform) {
    let mut backgrounded = is_backgrounded();
    platform.on_app_lifecycle(Box::new(move |phase| {
        if let Some(event) = transition(&mut backgrounded, phase) {
            enqueue(event);
        }
    }));
}

pub fn subscribe() -> UnboundedReceiver<MobileLifecycleEvent> {
    let (sender, receiver) = mpsc::unbounded();
    if let Ok(mut current) = event_sender().lock() {
        *current = Some(sender);
    }
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_emits_one_resume_for_each_background_transition() {
        let mut backgrounded = false;

        assert_eq!(
            transition(&mut backgrounded, AppLifecyclePhase::Active),
            None
        );
        assert_eq!(
            transition(&mut backgrounded, AppLifecyclePhase::Inactive),
            None
        );
        assert_eq!(
            transition(&mut backgrounded, AppLifecyclePhase::Background),
            Some(MobileLifecycleEvent::Backgrounded)
        );
        assert_eq!(
            transition(&mut backgrounded, AppLifecyclePhase::Background),
            None
        );
        assert_eq!(
            transition(&mut backgrounded, AppLifecyclePhase::Foreground),
            Some(MobileLifecycleEvent::Resumed)
        );
        assert_eq!(
            transition(&mut backgrounded, AppLifecyclePhase::Active),
            None
        );
    }

    #[test]
    fn recreated_activity_clears_the_process_background_state() {
        let mut backgrounded = true;

        assert_eq!(
            transition(&mut backgrounded, AppLifecyclePhase::Foreground),
            Some(MobileLifecycleEvent::Resumed)
        );
        assert!(!backgrounded);
    }
}
