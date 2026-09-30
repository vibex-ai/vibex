//! A deterministic in-process engine.
//!
//! Two callers need one:
//!
//! * **Tests.** The alternative is a machine with a real desktop and a real
//!   driver, which makes the policy, reference-lifecycle and stop contracts
//!   untestable exactly where they matter.
//! * **The contract probe.** `--probe` asserts the degradation and tier
//!   contracts on a build machine with no desktop at all.
//!
//! It is deliberately not wired into the product path: the runtime spawns the
//! real helper and nothing else. A fixture cannot be selected by configuration.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::sync::Mutex;

use vibex_core::{
    ComputerApplication, ComputerPermissionReport, ComputerPermissionState, ComputerPlatform,
    ComputerRect, ComputerScreenshot, ComputerUnavailableReason, ComputerUnverifiedReason,
    ComputerWindow,
};

use crate::engine::{
    ComputerEngine, EngineActionResult, EngineAppState, EngineClick, EngineDelivery, EngineElement,
    EnginePressKey, EngineProbe, EngineScroll, EngineSetValue, EngineStateRequest, EngineTypeText,
};
use crate::error::{ComputerError, ComputerResult, codes};

/// A scripted desktop: two applications, one window each, a handful of
/// elements, and switches for every degraded path the runtime has to handle.
#[derive(Debug)]
pub struct FixtureEngine {
    /// When set, `probe` reports no desktop session.
    pub no_desktop: AtomicBool,
    /// When set, `probe` reports a missing accessibility bridge.
    pub missing_bridge: AtomicBool,
    /// When set, `probe` reports a missing engine.
    pub engine_missing: AtomicBool,
    /// When set, every click fails with `background_unavailable`.
    pub background_unavailable: AtomicBool,
    /// When set, the tree is empty and the observation is degraded.
    pub degraded_tree: AtomicBool,
    /// Counts the times the engine released held input.
    pub released_keys: AtomicBool,
    /// Counts actions, so a test can prove a queued action never ran.
    pub action_count: AtomicU64,
    /// The generation the fixture reports; tests bump it to simulate a rerender.
    pub generation: AtomicU64,
    /// The last delivery mode a click used, so foreground escalation is
    /// observable.
    pub last_delivery: Mutex<Option<String>>,
    /// Applications, including the Vibex host window the self-target guard
    /// needs to see.
    pub apps: Vec<ComputerApplication>,
}

impl Default for FixtureEngine {
    fn default() -> Self {
        let notes = ComputerApplication {
            app_id: "com.example.notes".to_string(),
            display_name: "Notes".to_string(),
            executable_path: Some("/usr/bin/notes".to_string()),
            bundle_id: Some("com.example.notes".to_string()),
            running: true,
            pid: Some(4242),
            windows: vec![window("w1", "Draft")],
        };
        let mail = ComputerApplication {
            app_id: "com.example.mail".to_string(),
            display_name: "Mail".to_string(),
            executable_path: Some("/usr/bin/mail".to_string()),
            bundle_id: Some("com.example.mail".to_string()),
            running: true,
            pid: Some(4343),
            windows: vec![window("w2", "Inbox")],
        };
        // The host application, which the runtime must refuse to drive.
        let vibex = ComputerApplication {
            app_id: "dev.vibex.desktop".to_string(),
            display_name: "Vibex".to_string(),
            executable_path: Some("/usr/bin/vibex-desktop".to_string()),
            bundle_id: Some("dev.vibex.desktop".to_string()),
            running: true,
            pid: Some(std::process::id() as i32),
            windows: vec![window("host-window", "Vibex")],
        };
        Self {
            no_desktop: AtomicBool::new(false),
            missing_bridge: AtomicBool::new(false),
            engine_missing: AtomicBool::new(false),
            background_unavailable: AtomicBool::new(false),
            degraded_tree: AtomicBool::new(false),
            released_keys: AtomicBool::new(false),
            action_count: AtomicU64::new(0),
            generation: AtomicU64::new(1),
            last_delivery: Mutex::new(None),
            apps: vec![notes, mail, vibex],
        }
    }
}

fn window(window_id: &str, title: &str) -> ComputerWindow {
    ComputerWindow {
        window_id: window_id.to_string(),
        title: title.to_string(),
        bounds: Some(ComputerRect {
            x: 100.0,
            y: 100.0,
            width: 800.0,
            height: 600.0,
        }),
        focused: false,
        frontmost: false,
    }
}

impl FixtureEngine {
    /// Every fixture application, including the host window.
    pub fn applications() -> Vec<ComputerApplication> {
        FixtureEngine::default().apps
    }

    fn elements(&self, app_id: &str) -> Vec<EngineElement> {
        if self.degraded_tree.load(Ordering::SeqCst) {
            return Vec::new();
        }
        let send_label = if app_id == "com.example.mail" {
            "Send"
        } else {
            "Save"
        };
        vec![
            EngineElement {
                index: 0,
                role: "AXWindow".to_string(),
                name: "Window".to_string(),
                value: None,
                editable: false,
                secure: false,
                disabled: false,
                bounds: Some(ComputerRect {
                    x: 100.0,
                    y: 100.0,
                    width: 800.0,
                    height: 600.0,
                }),
            },
            EngineElement {
                index: 1,
                role: "AXTextField".to_string(),
                name: "Message".to_string(),
                value: Some("hello".to_string()),
                editable: true,
                secure: false,
                disabled: false,
                bounds: Some(ComputerRect {
                    x: 120.0,
                    y: 200.0,
                    width: 400.0,
                    height: 40.0,
                }),
            },
            EngineElement {
                index: 2,
                role: "AXSecureTextField".to_string(),
                name: "Password".to_string(),
                // The engine never reads a credential value, so the fixture
                // never carries one either.
                value: None,
                editable: true,
                secure: true,
                disabled: false,
                bounds: Some(ComputerRect {
                    x: 120.0,
                    y: 260.0,
                    width: 400.0,
                    height: 40.0,
                }),
            },
            EngineElement {
                index: 3,
                role: "AXButton".to_string(),
                name: send_label.to_string(),
                value: None,
                editable: false,
                secure: false,
                disabled: false,
                bounds: Some(ComputerRect {
                    x: 500.0,
                    y: 500.0,
                    width: 90.0,
                    height: 30.0,
                }),
            },
            EngineElement {
                index: 4,
                role: "AXButton".to_string(),
                name: "Cancel".to_string(),
                value: None,
                editable: false,
                secure: false,
                disabled: false,
                bounds: Some(ComputerRect {
                    x: 600.0,
                    y: 500.0,
                    width: 90.0,
                    height: 30.0,
                }),
            },
        ]
    }

    fn find_app(&self, app_id: &str) -> ComputerResult<ComputerApplication> {
        self.apps
            .iter()
            .find(|app| app.app_id == app_id)
            .cloned()
            .ok_or_else(|| {
                ComputerError::validation(codes::UNKNOWN_APP, "no such application")
                    .with_diagnostic("app_id", app_id.to_string())
            })
    }

    fn guard_available(&self) -> ComputerResult<()> {
        if self.engine_missing.load(Ordering::SeqCst) {
            return Err(ComputerError::capability(
                codes::ENGINE_MISSING,
                "the desktop engine is not installed",
            ));
        }
        if self.no_desktop.load(Ordering::SeqCst) {
            return Err(ComputerError::capability(
                codes::NO_DESKTOP,
                "this machine has no desktop session",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ComputerEngine for FixtureEngine {
    async fn probe(&self) -> ComputerResult<EngineProbe> {
        let unavailable_reason = if self.engine_missing.load(Ordering::SeqCst) {
            Some(ComputerUnavailableReason::EngineMissing)
        } else if self.no_desktop.load(Ordering::SeqCst) {
            Some(ComputerUnavailableReason::NoDesktopSession)
        } else if self.missing_bridge.load(Ordering::SeqCst) {
            Some(ComputerUnavailableReason::AccessibilityBridgeMissing)
        } else {
            None
        };
        Ok(EngineProbe {
            engine: Some("fixture 0".to_string()),
            platform: ComputerPlatform::LinuxX11,
            permissions: ComputerPermissionReport {
                accessibility: if self.missing_bridge.load(Ordering::SeqCst) {
                    ComputerPermissionState::Denied
                } else {
                    ComputerPermissionState::NotRequired
                },
                screen_recording: ComputerPermissionState::NotRequired,
                input_injection: ComputerPermissionState::NotRequired,
                restart_required_after_grant: false,
            },
            tool_surface: Some("list_apps,get_window_state,click".to_string()),
            degraded: Vec::new(),
            unavailable_reason,
            detail: None,
        })
    }

    async fn list_apps(&self) -> ComputerResult<Vec<ComputerApplication>> {
        self.guard_available()?;
        Ok(self.apps.clone())
    }

    async fn get_app_state(&self, request: EngineStateRequest) -> ComputerResult<EngineAppState> {
        self.guard_available()?;
        let app = self.find_app(&request.app_id)?;
        let degraded =
            self.degraded_tree.load(Ordering::SeqCst) || self.missing_bridge.load(Ordering::SeqCst);
        let window = app.windows.first().cloned();
        Ok(EngineAppState {
            app,
            window_id: window.as_ref().map(|window| window.window_id.clone()),
            window_title: window.as_ref().map(|window| window.title.clone()),
            tree_digest: format!(
                "gen-{}-{}",
                self.generation.load(Ordering::SeqCst),
                request.app_id
            ),
            elements: self.elements(&request.app_id),
            truncated: false,
            degraded,
            degraded_reason: if degraded {
                Some("the accessibility bridge is not answering".to_string())
            } else {
                None
            },
            screenshot: request.screenshot.then(|| ComputerScreenshot {
                mime_type: "image/png".to_string(),
                base64: "Zml4dHVyZQ==".to_string(),
                width: 1280,
                height: 800,
                scale: 1.0,
            }),
            window_bounds: window.and_then(|window| window.bounds),
        })
    }

    async fn click(&self, request: EngineClick) -> ComputerResult<EngineActionResult> {
        self.guard_available()?;
        *self.last_delivery.lock().await = Some(request.delivery.as_str().to_string());
        if self.background_unavailable.load(Ordering::SeqCst)
            && request.delivery == EngineDelivery::Background
        {
            return Err(ComputerError::capability(
                codes::BACKGROUND_UNAVAILABLE,
                "the background delivery is unavailable for this target",
            ));
        }
        self.action_count.fetch_add(1, Ordering::SeqCst);
        if request.delivery == EngineDelivery::Foreground {
            return Ok(EngineActionResult {
                asserted: false,
                unverified_reason: Some(ComputerUnverifiedReason::ForegroundEscalation),
                detail: Some("delivered after a foreground takeover".to_string()),
                cursor: request.point,
                tree_digest_after: None,
            });
        }
        Ok(EngineActionResult::asserted())
    }

    async fn type_text(&self, _request: EngineTypeText) -> ComputerResult<EngineActionResult> {
        self.guard_available()?;
        self.action_count.fetch_add(1, Ordering::SeqCst);
        Ok(EngineActionResult::unverified(
            ComputerUnverifiedReason::SyntheticInput,
        ))
    }

    async fn set_value(&self, _request: EngineSetValue) -> ComputerResult<EngineActionResult> {
        self.guard_available()?;
        self.action_count.fetch_add(1, Ordering::SeqCst);
        Ok(EngineActionResult::asserted())
    }

    async fn press_key(&self, _request: EnginePressKey) -> ComputerResult<EngineActionResult> {
        self.guard_available()?;
        self.action_count.fetch_add(1, Ordering::SeqCst);
        Ok(EngineActionResult::unverified(
            ComputerUnverifiedReason::SyntheticInput,
        ))
    }

    async fn scroll(&self, _request: EngineScroll) -> ComputerResult<EngineActionResult> {
        self.guard_available()?;
        self.action_count.fetch_add(1, Ordering::SeqCst);
        Ok(EngineActionResult::asserted())
    }

    async fn screenshot(
        &self,
        _app_id: Option<&str>,
        _window_id: Option<&str>,
    ) -> ComputerResult<Option<ComputerScreenshot>> {
        self.guard_available()?;
        Ok(Some(ComputerScreenshot {
            mime_type: "image/png".to_string(),
            base64: "Zml4dHVyZQ==".to_string(),
            width: 1280,
            height: 800,
            scale: 1.0,
        }))
    }

    async fn release_all_keys(&self) -> ComputerResult<()> {
        self.released_keys.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn user_activity_age_ms(&self) -> ComputerResult<Option<i64>> {
        Ok(Some(60_000))
    }

    async fn launch_app(&self, app_id: &str) -> ComputerResult<ComputerApplication> {
        self.guard_available()?;
        self.find_app(app_id)
    }

    async fn kill_app(&self, _app_id: &str) -> ComputerResult<()> {
        self.guard_available()?;
        Ok(())
    }
}

/// A shared fixture, for tests that need to observe the engine's switches.
pub fn shared_fixture() -> Arc<FixtureEngine> {
    Arc::new(FixtureEngine::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_fixture_switches_reach_every_degraded_path() {
        let engine = FixtureEngine::default();
        let probe = engine.probe().await.unwrap();
        assert!(probe.unavailable_reason.is_none());

        engine.no_desktop.store(true, Ordering::SeqCst);
        assert_eq!(
            engine.probe().await.unwrap().unavailable_reason,
            Some(ComputerUnavailableReason::NoDesktopSession)
        );
        assert!(engine.list_apps().await.is_err());

        engine.no_desktop.store(false, Ordering::SeqCst);
        engine.engine_missing.store(true, Ordering::SeqCst);
        let error = engine.list_apps().await.unwrap_err();
        assert!(error.is_unavailable_here());
    }

    #[tokio::test]
    async fn the_fixture_never_returns_a_credential_value() {
        let engine = FixtureEngine::default();
        let state = engine
            .get_app_state(EngineStateRequest {
                app_id: "com.example.notes".to_string(),
                window_id: None,
                max_elements: 40,
                extended: false,
                screenshot: false,
            })
            .await
            .unwrap();
        let secure = state
            .elements
            .iter()
            .find(|element| element.secure)
            .expect("the fixture has a secure field");
        assert!(secure.value.is_none());
    }

    #[tokio::test]
    async fn the_fixture_reports_background_unavailability_and_foreground() {
        let engine = FixtureEngine::default();
        engine.background_unavailable.store(true, Ordering::SeqCst);
        let error = engine
            .click(EngineClick {
                app_id: "com.example.notes".to_string(),
                window_id: None,
                element_index: Some(4),
                point: None,
                button: "left".to_string(),
                click_count: 1,
                delivery: EngineDelivery::Background,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, codes::BACKGROUND_UNAVAILABLE);
        let result = engine
            .click(EngineClick {
                app_id: "com.example.notes".to_string(),
                window_id: None,
                element_index: Some(4),
                point: None,
                button: "left".to_string(),
                click_count: 1,
                delivery: EngineDelivery::Foreground,
            })
            .await
            .unwrap();
        assert!(!result.asserted);
    }
}
