//! The computer-use panel.
//!
//! This is the human's half of the feature, and for most Agents it is the only
//! half that can see pixels: the runtime hands a screenshot to an Agent only
//! when its adapter has been confirmed to forward image content to its model.
//! So the panel is not a nicety — it is the channel through which a person
//! watches, approves and stops an Agent that is acting on their real desktop
//! with their real accounts.
//!
//! What it renders, and why each part exists:
//!
//! * **Readiness first.** When computer use cannot run, the panel shows the
//!   named reason and the platform's own capability statement, because "it does
//!   not work" and "this machine has no desktop session" demand different
//!   actions from the reader.
//! * **The canonical target.** The runtime resolves the application the Agent
//!   is driving; the panel shows that identity, not the model's wording. This
//!   is the reader's defence against "approved A, acted on B".
//! * **The live frame.** Latest-value, never a queue; every replacement frame
//!   releases the previous texture.
//! * **The emergency stop.** One press reaches the terminal state: new calls
//!   are refused, queued input is dropped, held keys are released and a human
//!   must re-enable it.
//! * **The ledger.** The redacted record of what was attempted, with its
//!   verification state, so a reader never has to trust a summary.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, EventEmitter, FontWeight, ObjectFit, Render, RenderImage, SharedString,
    Subscription, Task, Window, div, img, px, rgb,
};
use gpui_component::ActiveTheme;
use image::Frame;
use vibex_core::{
    ComputerActionRecord, ComputerAvailability, ComputerExecutionSource, ComputerFrame,
    ComputerOperationStatus, ComputerPlatform, ComputerSession, ComputerSessionId,
    ComputerSessionState, unix_timestamp_ms,
};
use vibex_desktop_runtime::ComputerRuntime;

/// A frame after decoding, in the layout `RenderImage` wants.
struct DecodedComputerFrame {
    image: image::RgbaImage,
}

/// Events the panel raises for its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerSurfaceEvent {
    /// The panel wants to be closed.
    Closed,
}

/// What the panel is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfacePhase {
    Connecting,
    Live,
    Unavailable,
    Failed,
}

/// The computer-use panel.
pub struct ComputerSurface {
    runtime: Arc<ComputerRuntime>,
    /// The Tokio handle the runtime's service must be polled inside. GPUI's
    /// executor has no reactor, so every call goes through this.
    tokio: tokio::runtime::Handle,
    session_id: Option<ComputerSessionId>,
    session: Option<ComputerSession>,
    availability: Option<ComputerAvailability>,
    ledger: Vec<ComputerActionRecord>,
    phase: SurfacePhase,
    message: Option<String>,
    frame_image: Option<Arc<RenderImage>>,
    /// Textures that must be released on the next paint: `Window::drop_image`
    /// needs a `&mut Window`, which only `render` has.
    pending_drop: Vec<Arc<RenderImage>>,
    frame_task: Option<Task<()>>,
    heartbeat_task: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
    stopped_reason: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComputerSurfaceEvent> for ComputerSurface {}

impl ComputerSurface {
    /// Builds the panel and starts its subscriptions.
    pub fn new(
        runtime: Arc<ComputerRuntime>,
        tokio: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut surface = Self {
            runtime,
            tokio,
            session_id: None,
            session: None,
            availability: None,
            ledger: Vec::new(),
            phase: SurfacePhase::Connecting,
            message: None,
            frame_image: None,
            pending_drop: Vec::new(),
            frame_task: None,
            heartbeat_task: None,
            refresh_task: None,
            stopped_reason: None,
            _subscriptions: Vec::new(),
        };
        surface.start(cx);
        surface
    }

    /// Runs one runtime future on the runtime's own Tokio executor.
    ///
    /// GPUI's executor has no reactor, so a service call made straight from a
    /// render would panic with "there is no reactor running". The calls are
    /// short (a snapshot read, a watch subscription), so blocking the UI thread
    /// for their duration is the honest trade for a viewer panel.
    fn run<F: std::future::Future>(&self, future: F) -> F::Output {
        self.tokio.block_on(future)
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
        // The ledger and the session state move while the panel is open, so it
        // polls: an activity list that only updates when the reader clicks
        // something is a list that lies about a running Agent.
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let alive = this.update(cx, |surface, cx| surface.refresh(cx));
                if alive.is_err() {
                    return;
                }
            }
        }));
        // The heartbeat is what keeps the disconnect guard from pausing a
        // session while a human is actually watching it.
        self.heartbeat_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let alive = this.update(cx, |surface, _| {
                    surface.runtime.note_heartbeat();
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
    }

    /// Reloads availability, the session and the ledger.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let availability = self.run(self.runtime.availability());
        let subscribe = self
            .runtime
            .subscribe_frames(None, computer_tier_for_panel());
        let (session_id, frames) = match self.run(subscribe) {
            Some(pair) => (Some(pair.0), Some(pair.1)),
            None => (None, None),
        };
        self.session_id = session_id.clone();
        if let Some(session_id) = &session_id {
            let snapshot = self.run(self.runtime.service().session_snapshot(session_id));
            if let Some(snapshot) = snapshot {
                self.session = Some(snapshot.session);
                self.ledger = snapshot.ledger;
            }
        }
        let open = self
            .session
            .as_ref()
            .map(|session| !session.state.accepts_actions())
            .unwrap_or(false);
        self.stopped_reason = self
            .session
            .as_ref()
            .and_then(|session| session.paused_reason.clone());
        self.availability = Some(availability.clone());
        self.phase = if !availability.is_usable() {
            SurfacePhase::Unavailable
        } else if self.session.is_none() {
            // Usable but unreachable: the endpoint exists and the session does
            // not, which is a failure rather than a slow start.
            SurfacePhase::Failed
        } else if open {
            SurfacePhase::Connecting
        } else {
            SurfacePhase::Live
        };
        if let Some(frames) = frames {
            self.watch_frames(frames, cx);
        }
        cx.notify();
    }

    fn watch_frames(
        &mut self,
        mut frames: tokio::sync::watch::Receiver<Option<ComputerFrame>>,
        cx: &mut Context<Self>,
    ) {
        // A watch channel holds the newest value only: a frame that arrives
        // while the UI is busy replaces the one waiting rather than queueing
        // behind it. That is the whole point of the latest-value slot.
        self.frame_task = Some(cx.spawn(async move |this, cx| {
            loop {
                if frames.changed().await.is_err() {
                    return;
                }
                let frame = frames.borrow_and_update().clone();
                let Some(frame) = frame else {
                    continue;
                };
                let alive = this.update(cx, |surface, cx| {
                    surface.accept_frame(frame);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        }));
    }

    fn accept_frame(&mut self, frame: ComputerFrame) {
        let Ok(decoded) = decode_frame(&frame.bytes) else {
            // A frame that cannot be decoded is not worth tearing the panel
            // down for; the next one will arrive shortly.
            return;
        };
        if let Some(previous) = self.frame_image.take() {
            self.pending_drop.push(previous);
        }
        self.frame_image = Some(Arc::new(RenderImage::new(vec![Frame::new(decoded.image)])));
        self.phase = SurfacePhase::Live;
    }

    /// The emergency stop. One press reaches the terminal state.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let stop = self
            .runtime
            .stop("the user pressed stop in the computer panel");
        match self.run(stop) {
            Ok(()) => {
                self.stopped_reason = Some("stopped by the user".to_string());
                self.message = Some(
                    "Computer use is stopped. Queued input was dropped and held keys were \
                     released. Use Re-enable to let the Agent act again."
                        .to_string(),
                );
            }
            Err(error) => {
                self.message = Some(format!("Stop failed: {}", error.message));
            }
        }
        self.refresh(cx);
    }

    /// Pauses Agent actions without reaching the terminal stop state.
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let pause = self
            .runtime
            .pause(&session_id, "the user paused computer use in the panel");
        match self.run(pause) {
            Ok(()) => {
                self.message =
                    Some("Paused. Agent desktop calls refuse until you resume.".to_string())
            }
            Err(error) => self.message = Some(format!("Pause failed: {}", error.message)),
        }
        self.refresh(cx);
    }

    /// Re-enables after a pause or a stop. Always a human action.
    pub fn resume(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let resume = self.runtime.resume(&session_id);
        match self.run(resume) {
            Ok(()) => self.message = None,
            Err(error) => self.message = Some(format!("Re-enable failed: {}", error.message)),
        }
        self.refresh(cx);
    }

    /// The reason computer use cannot run here, when it cannot.
    fn unavailable_line(&self) -> Option<String> {
        let availability = self.availability.as_ref()?;
        let reason = availability.unavailable_reason?;
        let mut line = reason.as_str().replace('_', " ");
        if let Some(detail) = &availability.detail {
            line.push_str(&format!(" — {detail}"));
        }
        Some(line)
    }

    fn session_state(&self) -> Option<ComputerSessionState> {
        self.session.as_ref().map(|session| session.state)
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let availability = self.availability.clone();
        let title = match availability
            .as_ref()
            .and_then(|availability| availability.unavailable_reason)
        {
            Some(_) => "Computer use is not available here",
            None => "Computer use",
        };
        let engine = availability
            .as_ref()
            .and_then(|availability| availability.engine.clone())
            .unwrap_or_else(|| "engine unknown".to_string());
        let platform = availability
            .as_ref()
            .map(|availability| availability.platform)
            .unwrap_or(ComputerPlatform::Unknown);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(format!(
                        "{} · {engine}",
                        platform.as_str()
                    ))),
            )
            .when_some(self.unavailable_line(), |this, line| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xd97706))
                        .child(SharedString::from(line)),
                )
            })
            .children(availability.and_then(|availability| {
                (!availability.support.note.is_empty()).then(|| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(availability.support.note))
                })
            }))
            .into_any_element()
    }

    fn render_target(&self, cx: &mut Context<Self>) -> AnyElement {
        let target = self
            .session
            .as_ref()
            .and_then(|session| session.target.as_ref())
            .map(|app| app.label())
            .unwrap_or_else(|| "No application is being driven".to_string());
        let driving = self
            .session
            .as_ref()
            .map(|session| session.state == ComputerSessionState::Running)
            .unwrap_or(false);
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from("Operating:")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(if driving {
                        rgb(0x16a34a).into()
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(SharedString::from(target)),
            )
            .into_any_element()
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.session_state();
        let usable = self
            .availability
            .as_ref()
            .map(|availability| availability.is_usable())
            .unwrap_or(false);
        let can_resume = usable
            && matches!(
                state,
                Some(ComputerSessionState::PausedByUser)
                    | Some(ComputerSessionState::PausedOffline)
                    | Some(ComputerSessionState::StoppedByUser)
            );
        let can_pause = usable && state == Some(ComputerSessionState::Running);
        div()
            .flex()
            .items_center()
            .gap_2()
            // The stop is the largest control on purpose: it is the one thing
            // that must be findable while the reader is alarmed.
            .child(
                div()
                    .id("computer-emergency-stop")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0xdc2626))
                    .text_color(rgb(0xffffff))
                    .font_weight(FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .on_click(cx.listener(|surface, _event, _window, cx| surface.stop(cx)))
                    .child(SharedString::from("Stop computer use")),
            )
            .when(can_pause, |this| {
                this.child(
                    div()
                        .id("computer-pause")
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(cx.theme().secondary)
                        .cursor_pointer()
                        .on_click(cx.listener(|surface, _event, _window, cx| surface.pause(cx)))
                        .child(SharedString::from("Pause")),
                )
            })
            .when(can_resume, |this| {
                this.child(
                    div()
                        .id("computer-resume")
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(cx.theme().secondary)
                        .cursor_pointer()
                        .on_click(cx.listener(|surface, _event, _window, cx| surface.resume(cx)))
                        .child(SharedString::from("Re-enable")),
                )
            })
            .into_any_element()
    }

    fn render_stage(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let image = self.frame_image.clone();
        let phase = self.phase;
        let message = self
            .message
            .clone()
            .or_else(|| self.unavailable_line())
            .or_else(|| match phase {
                SurfacePhase::Connecting => {
                    Some("Waiting for the first frame of the desktop.".to_string())
                }
                _ => None,
            })
            .unwrap_or_else(|| "The desktop is live.".to_string());
        div()
            .relative()
            .flex_1()
            .min_h(px(240.0))
            .w_full()
            .overflow_hidden()
            .rounded_md()
            .bg(cx.theme().background)
            .when_some(image, |this, image| {
                this.child(img(image).w_full().h_full().object_fit(ObjectFit::Contain))
            })
            .when(phase != SurfacePhase::Live, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_4()
                        .child(
                            div()
                                .max_w(px(560.0))
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(message)),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_ledger(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = unix_timestamp_ms();
        let entries: Vec<AnyElement> = self
            .ledger
            .iter()
            .rev()
            .take(12)
            .map(|record| {
                let verified = record.status == ComputerOperationStatus::Verified;
                let colour: gpui::Hsla = match record.status {
                    ComputerOperationStatus::Verified => rgb(0x16a34a).into(),
                    ComputerOperationStatus::Dispatched => rgb(0xd97706).into(),
                    ComputerOperationStatus::Failed => rgb(0xdc2626).into(),
                    ComputerOperationStatus::Unknown => cx.theme().muted_foreground,
                };
                let source = match record.execution_source {
                    ComputerExecutionSource::Agent => "agent",
                    ComputerExecutionSource::User => "you",
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .py_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_xs().text_color(colour).child(SharedString::from(
                                if verified { "verified" } else { "unverified" },
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(format!(
                                        "{} · {} · {}",
                                        source,
                                        record.kind.as_str(),
                                        relative_time(record.at_ms, now)
                                    ))),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .child(SharedString::from(record.summary.clone())),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from("What the Agent did")),
            )
            .when(entries.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from("No desktop actions yet.")),
                )
            })
            .children(entries)
            .into_any_element()
    }
}

impl Render for ComputerSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Release the previous frame's texture. Without this the sprite atlas
        // grows without bound: `RenderImage::new` mints a new image id on every
        // frame and nothing else evicts it.
        for image in std::mem::take(&mut self.pending_drop) {
            let _ = window.drop_image(image);
        }
        div()
            .id("computer-surface")
            .flex()
            .flex_col()
            .size_full()
            .gap_3()
            .p_4()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(self.render_target(cx))
            .child(self.render_controls(cx))
            .child(self.render_stage(cx))
            .child(self.render_ledger(cx))
    }
}

/// The panel is a human viewer, not an Agent, so it gets the visual tier: the
/// frames it shows come from the runtime's own capture, not from a tool result.
fn computer_tier_for_panel() -> vibex_core::ComputerToolTier {
    vibex_core::ComputerToolTier::Visual
}

/// A relative-time label, in the same shape the browser panel uses.
fn relative_time(at_ms: i64, now_ms: i64) -> String {
    let delta = now_ms.saturating_sub(at_ms);
    if delta < 1_000 {
        return "just now".to_string();
    }
    let seconds = delta / 1_000;
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    format!("{}h ago", minutes / 60)
}

/// Decodes an encoded desktop frame into the BGRA layout `RenderImage` wants.
fn decode_frame(bytes: &[u8]) -> Result<DecodedComputerFrame, String> {
    let decoded = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
    let mut rgba = decoded.into_rgba8();
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(DecodedComputerFrame { image: rgba })
}

/// The scale a frame's pixels carry, exposed for the pointer math the panel
/// will use when it becomes a remote control rather than a viewer.
pub fn desktop_point_from_frame_point(frame: &ComputerFrame, x: f64, y: f64) -> (f64, f64) {
    frame.frame_point_to_desktop(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::ComputerSessionId;

    fn frame(scale: f64) -> ComputerFrame {
        ComputerFrame {
            session_id: ComputerSessionId::new(),
            sequence: 1,
            format: "image/jpeg".to_string(),
            bytes: vec![0xff, 0xd8, 0xff],
            width: 1280,
            height: 800,
            origin_x: 100.0,
            origin_y: 50.0,
            scale,
            at_ms: 0,
        }
    }

    #[test]
    fn frame_points_map_onto_desktop_points() {
        let frame = frame(2.0);
        assert_eq!(
            desktop_point_from_frame_point(&frame, 10.0, 10.0),
            (120.0, 70.0)
        );
    }

    #[test]
    fn relative_time_reads_in_human_units() {
        assert_eq!(relative_time(1_000, 1_500), "just now");
        assert_eq!(relative_time(0, 5_000), "5s ago");
        assert_eq!(relative_time(0, 120_000), "2m ago");
        assert_eq!(relative_time(0, 7_200_000), "2h ago");
    }

    #[test]
    fn a_frame_that_is_not_an_image_is_rejected_rather_than_painted() {
        assert!(decode_frame(b"not an image").is_err());
    }
}
