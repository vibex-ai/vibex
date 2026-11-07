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
//!
//! The panel draws in two shells. The window shell is the full surface. The
//! floating shell is a small strip that stays in view while the reader works
//! somewhere else; it drops the ledger and the platform note, keeps the target
//! and the stop, and can be expanded back in one press. Both shells render the
//! same state, and the choice between them is a persisted decision the settings
//! page also exposes.

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
    ComputerActionKind, ComputerActionRecord, ComputerAvailability, ComputerExecutionSource,
    ComputerFrame, ComputerOperationStatus, ComputerPlatform, ComputerSession, ComputerSessionId,
    ComputerSessionState, ComputerUnavailableReason, VibexSessionId, unix_timestamp_ms,
};
use vibex_desktop_model::ComputerPanelMode;
use vibex_desktop_runtime::ComputerRuntime;

use crate::locale;

/// A frame after decoding, in the layout `RenderImage` wants.
struct DecodedComputerFrame {
    image: image::RgbaImage,
}

/// Events the panel raises for its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputerSurfaceEvent {
    /// The panel wants to be closed.
    Closed,
    /// The reader asked for a different shell. The owner reopens the panel in
    /// the requested mode, since the shell is a property of the window.
    ModeRequested(ComputerPanelMode),
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
    /// The shell this instance draws. The window owns the choice; the panel
    /// only renders it.
    mode: ComputerPanelMode,
    /// The Agent session this panel was opened for, when it was opened for one.
    ///
    /// A panel opened by the reader watches the panel's own session; a panel
    /// raised because an Agent started driving watches that Agent's session,
    /// because that is where the target, the ledger and the state the stop has
    /// to reach live.
    agent_session_id: Option<VibexSessionId>,
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
    /// Builds the panel in the full window shell and starts its subscriptions.
    pub fn new(
        runtime: Arc<ComputerRuntime>,
        tokio: tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_in(runtime, tokio, ComputerPanelMode::Window, None, cx)
    }

    /// Builds the panel in the requested shell and starts its subscriptions.
    ///
    /// `agent_session_id` is the Agent the panel was raised for. `None` is a
    /// panel the reader opened themselves, which watches its own session.
    pub fn new_in(
        runtime: Arc<ComputerRuntime>,
        tokio: tokio::runtime::Handle,
        mode: ComputerPanelMode,
        agent_session_id: Option<VibexSessionId>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut surface = Self {
            runtime,
            tokio,
            mode,
            agent_session_id,
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

    /// The shell this instance draws.
    pub fn mode(&self) -> ComputerPanelMode {
        self.mode
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
        // When this panel was raised for an Agent, the Agent's session is the
        // one worth rendering: its ledger, its target and its state are what
        // the reader came to see and what the stop has to reach. A panel the
        // reader opened by hand keeps its own viewer session until an Agent
        // starts driving, at which point showing that Agent beats an empty
        // viewer.
        //
        // The pinned Agent is a preference, not a lock: a session that has gone
        // away falls back to whichever Agent started last, so a panel left open
        // across Agent sessions never keeps rendering a finished one.
        let agent_session = self
            .agent_session_id
            .clone()
            .and_then(|agent_session_id| {
                self.run(self.runtime.session_for_agent(&agent_session_id))
            })
            .or_else(|| self.run(self.runtime.latest_agent_session()));
        let (session_id, frames) = match agent_session {
            Some(session_id) => {
                let frames = self.run(self.runtime.subscribe_session_frames(&session_id));
                (Some(session_id), Some(frames))
            }
            None => match self.run(
                self.runtime
                    .subscribe_frames(None, computer_tier_for_panel()),
            ) {
                Some(pair) => (Some(pair.0), Some(pair.1)),
                None => (None, None),
            },
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
        let reason = locale::text(
            "the user pressed stop in the computer panel",
            "用户在电脑操作面板按下了停止",
            "使用者在電腦操作面板按下了停止",
        );
        let stop = self.runtime.stop(reason);
        match self.run(stop) {
            Ok(()) => {
                self.stopped_reason = Some(
                    locale::text("Stopped by the user", "已被用户停止", "已被使用者停止")
                        .to_string(),
                );
                self.message = Some(
                    locale::text(
                        "Computer use is stopped. Queued input was dropped and held keys were \
                         released. Use Re-enable to let the Agent act again.",
                        "电脑操作已停止：排队中的输入已丢弃，按住的键已释放。使用“重新启用”让 Agent 再次操作。",
                        "電腦操作已停止：排隊中的輸入已丟棄，按住的鍵已釋放。使用「重新啟用」讓 Agent 再次操作。",
                    )
                    .to_string(),
                );
            }
            Err(error) => {
                self.message = Some(format!(
                    "{}{}",
                    locale::text("Stop failed: ", "停止失败：", "停止失敗："),
                    error.message
                ));
            }
        }
        self.refresh(cx);
    }

    /// Pauses Agent actions without reaching the terminal stop state.
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let reason = locale::text(
            "the user paused computer use in the panel",
            "用户在电脑操作面板中暂停了操作",
            "使用者在電腦操作面板中暫停了操作",
        );
        let pause = self.runtime.pause(&session_id, reason);
        match self.run(pause) {
            Ok(()) => {
                self.message = Some(
                    locale::text(
                        "Paused. Agent desktop calls refuse until you resume.",
                        "已暂停。在你恢复之前，Agent 的桌面调用都会被拒绝。",
                        "已暫停。在你恢復之前，Agent 的桌面呼叫都會被拒絕。",
                    )
                    .to_string(),
                )
            }
            Err(error) => {
                self.message = Some(format!(
                    "{}{}",
                    locale::text("Pause failed: ", "暂停失败：", "暫停失敗："),
                    error.message
                ))
            }
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
            Err(error) => {
                self.message = Some(format!(
                    "{}{}",
                    locale::text("Re-enable failed: ", "重新启用失败：", "重新啟用失敗："),
                    error.message
                ))
            }
        }
        self.refresh(cx);
    }

    /// Asks the owner to reopen the panel in the other shell.
    fn request_mode(&mut self, mode: ComputerPanelMode, cx: &mut Context<Self>) {
        cx.emit(ComputerSurfaceEvent::ModeRequested(mode));
    }

    /// The named reason computer use cannot run here, in the reader's language.
    fn unavailable_line(&self) -> Option<String> {
        let availability = self.availability.as_ref()?;
        let reason = availability.unavailable_reason?;
        let mut line = unavailable_reason_label(reason).to_string();
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
            Some(_) => locale::text(
                "Computer use is not available here",
                "此处无法使用电脑操作",
                "此處無法使用電腦操作",
            ),
            None => locale::text("Computer use", "电脑操作", "電腦操作"),
        };
        let engine = availability
            .as_ref()
            .and_then(|availability| availability.engine.clone())
            .unwrap_or_else(|| locale::text("engine unknown", "引擎未知", "引擎未知").to_string());
        let platform = availability
            .as_ref()
            .map(|availability| availability.platform)
            .unwrap_or(ComputerPlatform::Unknown);
        let mut header = div()
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
                        platform_label(platform)
                    ))),
            )
            .when_some(self.unavailable_line(), |this, line| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xd97706))
                        .child(SharedString::from(line)),
                )
            });
        // The floating shell keeps the header to one compact line and puts the
        // expand control beside the title, so the strip stays small.
        if self.mode.is_floating() {
            header = header.child(self.render_mode_toggle(cx));
        } else {
            header = header
                .children(availability.clone().and_then(|availability| {
                    (!availability.support.note.is_empty()).then(|| {
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(availability.support.note))
                    })
                }))
                .child(self.render_mode_toggle(cx));
        }
        header.into_any_element()
    }

    /// The control that moves the panel between the window and floating shells.
    fn render_mode_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let (id, label, next) = if self.mode.is_floating() {
            (
                "computer-panel-expand",
                locale::text("Open full window", "打开完整窗口", "開啟完整視窗"),
                ComputerPanelMode::Window,
            )
        } else {
            (
                "computer-panel-float",
                locale::text("Float on top", "浮动在最前", "浮動在最前"),
                ComputerPanelMode::Floating,
            )
        };
        div()
            .id(id)
            .mt_1()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(cx.theme().secondary)
            .text_sm()
            .cursor_pointer()
            .on_click(
                cx.listener(move |surface, _event, _window, cx| surface.request_mode(next, cx)),
            )
            .child(SharedString::from(label))
            .into_any_element()
    }

    fn render_target(&self, cx: &mut Context<Self>) -> AnyElement {
        let target = self
            .session
            .as_ref()
            .and_then(|session| session.target.as_ref())
            .map(|app| app.label())
            .unwrap_or_else(|| {
                locale::text(
                    "No application is being driven",
                    "当前没有正在操作的应用",
                    "目前沒有正在操作的应用",
                )
                .to_string()
            });
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
                    .child(SharedString::from(locale::text(
                        "Operating:",
                        "正在操作：",
                        "正在操作：",
                    ))),
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
                    .child(SharedString::from(locale::text(
                        "Stop computer use",
                        "停止电脑操作",
                        "停止電腦操作",
                    ))),
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
                        .child(SharedString::from(locale::text("Pause", "暂停", "暫停"))),
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
                        .child(SharedString::from(locale::text(
                            "Re-enable",
                            "重新启用",
                            "重新啟用",
                        ))),
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
                SurfacePhase::Connecting => Some(
                    locale::text(
                        "Waiting for the first frame of the desktop.",
                        "正在等待桌面的第一帧画面。",
                        "正在等待桌面的第一幀畫面。",
                    )
                    .to_string(),
                ),
                _ => None,
            })
            .unwrap_or_else(|| {
                locale::text(
                    "The desktop is live.",
                    "桌面已在实时显示。",
                    "桌面已在即時顯示。",
                )
                .to_string()
            });
        div()
            .relative()
            .flex_1()
            .min_h(px(if self.mode.is_floating() {
                160.0
            } else {
                240.0
            }))
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
                    ComputerExecutionSource::Agent => locale::text("agent", "Agent", "Agent"),
                    ComputerExecutionSource::User => locale::text("you", "你", "你"),
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
                                if verified {
                                    locale::text("verified", "已验证", "已驗證")
                                } else {
                                    locale::text("unverified", "未验证", "未驗證")
                                },
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(format!(
                                        "{} · {} · {}",
                                        source,
                                        action_kind_label(record.kind),
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
                    .child(SharedString::from(locale::text(
                        "What the Agent did",
                        "Agent 做了什么",
                        "Agent 做了什麼",
                    ))),
            )
            .when(entries.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(locale::text(
                            "No desktop actions yet.",
                            "还没有桌面操作。",
                            "還沒有桌面操作。",
                        ))),
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
        let floating = self.mode.is_floating();
        let mut root = div()
            .id("computer-surface")
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .when(floating, |this| this.gap_2().p_3())
            .when(!floating, |this| this.gap_3().p_4())
            .child(self.render_header(cx))
            .child(self.render_target(cx))
            .child(self.render_controls(cx))
            .child(self.render_stage(cx));
        // The floating shell drops the ledger: the strip exists to keep the
        // desktop and the stop in view, and a scrolling history would undo the
        // point of a small window. The full window keeps it.
        if !floating {
            root = root.child(self.render_ledger(cx));
        }
        root
    }
}

/// The panel is a human viewer, not an Agent, so it gets the visual tier: the
/// frames it shows come from the runtime's own capture, not from a tool result.
fn computer_tier_for_panel() -> vibex_core::ComputerToolTier {
    vibex_core::ComputerToolTier::Visual
}

/// The platform name in the reader's language.
fn platform_label(platform: ComputerPlatform) -> &'static str {
    match platform {
        ComputerPlatform::Macos => locale::text("macOS", "macOS", "macOS"),
        ComputerPlatform::Windows => locale::text("Windows", "Windows", "Windows"),
        ComputerPlatform::LinuxX11 => locale::text("Linux (X11)", "Linux（X11）", "Linux（X11）"),
        ComputerPlatform::LinuxWayland => {
            locale::text("Linux (Wayland)", "Linux（Wayland）", "Linux（Wayland）")
        }
        ComputerPlatform::Unknown => locale::text("unknown", "未知", "未知"),
    }
}

/// Why computer use cannot run, in the reader's language.
fn unavailable_reason_label(reason: ComputerUnavailableReason) -> &'static str {
    match reason {
        ComputerUnavailableReason::NoDesktopSession => locale::text(
            "this machine has no desktop session",
            "本机没有可操作的桌面会话",
            "本機沒有可操作的桌面工作階段",
        ),
        ComputerUnavailableReason::AccessibilityBridgeMissing => locale::text(
            "the accessibility bridge is not running",
            "无障碍总线没有运行",
            "無障礙匯流排沒有執行",
        ),
        ComputerUnavailableReason::PlatformUnsupported => locale::text(
            "this platform cannot be driven",
            "本平台不支持被操作",
            "本平台不支援被操作",
        ),
        ComputerUnavailableReason::EngineMissing => {
            locale::text("the engine did not start", "引擎没有启动", "引擎沒有啟動")
        }
        ComputerUnavailableReason::PermissionPending => locale::text(
            "the system permission is not granted yet",
            "系统权限尚未授予",
            "系統權限尚未授予",
        ),
        ComputerUnavailableReason::PermissionRestartRequired => locale::text(
            "the system permission needs a restart",
            "系统权限需要重启后生效",
            "系統權限需要重新啟動後生效",
        ),
        ComputerUnavailableReason::RunningAsRoot => locale::text(
            "the runtime is running as root",
            "runtime 以 root 身份运行",
            "runtime 以 root 身分執行",
        ),
        ComputerUnavailableReason::RemoteRuntimeUnsupported => locale::text(
            "the paired runtime is remote",
            "配对的 runtime 在远端",
            "配對的 runtime 在遠端",
        ),
        ComputerUnavailableReason::FeatureDisabled => locale::text(
            "computer use is switched off",
            "电脑操作已被关闭",
            "電腦操作已被關閉",
        ),
    }
}

/// A short label for one ledger entry's action kind.
///
/// A kind this build does not know is shown as `other`, because the enum is
/// unknown-safe and a newer runtime's entry must still be listable.
fn action_kind_label(kind: ComputerActionKind) -> &'static str {
    match kind {
        ComputerActionKind::ListApps => locale::text("apps", "应用列表", "應用清單"),
        ComputerActionKind::LaunchApp => locale::text("launch", "启动", "啟動"),
        ComputerActionKind::KillApp => locale::text("quit", "退出", "結束"),
        ComputerActionKind::GetAppState => locale::text("observe", "读取界面", "讀取介面"),
        ComputerActionKind::Click => locale::text("click", "点击", "點擊"),
        ComputerActionKind::TypeText => locale::text("type", "输入", "輸入"),
        ComputerActionKind::SetValue => locale::text("set", "写入", "寫入"),
        ComputerActionKind::PressKey => locale::text("key", "按键", "按鍵"),
        ComputerActionKind::Scroll => locale::text("scroll", "滚动", "捲動"),
        ComputerActionKind::Screenshot => locale::text("screenshot", "截图", "截圖"),
        ComputerActionKind::Permissions => locale::text("permissions", "权限", "權限"),
        ComputerActionKind::Verify => locale::text("verify", "验证", "驗證"),
        ComputerActionKind::ClipboardRead => locale::text("clipboard read", "读剪贴板", "讀剪貼板"),
        ComputerActionKind::ClipboardWrite => {
            locale::text("clipboard write", "写剪贴板", "寫剪貼板")
        }
        ComputerActionKind::Stop => locale::text("stop", "停止", "停止"),
        ComputerActionKind::Pause => locale::text("pause", "暂停", "暫停"),
        ComputerActionKind::Resume => locale::text("resume", "恢复", "恢復"),
        ComputerActionKind::Unknown => locale::text("other", "其他", "其他"),
    }
}

/// A relative-time label, in the same shape the browser panel uses.
fn relative_time(at_ms: i64, now_ms: i64) -> String {
    let delta = now_ms.saturating_sub(at_ms);
    if delta < 1_000 {
        return locale::text("just now", "刚刚", "剛剛").to_string();
    }
    let seconds = delta / 1_000;
    if seconds < 60 {
        return format!("{seconds}{}", locale::text("s ago", " 秒前", " 秒前"));
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}{}", locale::text("m ago", " 分钟前", " 分鐘前"));
    }
    format!(
        "{}{}",
        minutes / 60,
        locale::text("h ago", " 小时前", " 小時前")
    )
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
        // The labels follow the active locale; English is the process default.
        assert_eq!(relative_time(1_000, 1_500), "just now");
        assert_eq!(relative_time(0, 5_000), "5s ago");
        assert_eq!(relative_time(0, 120_000), "2m ago");
        assert_eq!(relative_time(0, 7_200_000), "2h ago");
    }

    #[test]
    fn every_action_kind_has_a_label() {
        // A raw `as_str()` token would be an English-only leak into the panel;
        // every kind the drawer can receive has a localized label instead.
        //
        // The comparison is against the translated locales, not English: several
        // labels are spelled exactly like their token in English ("click",
        // "scroll", "stop"), so comparing English could never tell a real
        // translation from a fallback. What matters is that switching language
        // changes the label away from the token.
        let kinds = [
            ComputerActionKind::ListApps,
            ComputerActionKind::LaunchApp,
            ComputerActionKind::KillApp,
            ComputerActionKind::GetAppState,
            ComputerActionKind::Click,
            ComputerActionKind::TypeText,
            ComputerActionKind::SetValue,
            ComputerActionKind::PressKey,
            ComputerActionKind::Scroll,
            ComputerActionKind::Screenshot,
            ComputerActionKind::Permissions,
            ComputerActionKind::Verify,
            ComputerActionKind::ClipboardRead,
            ComputerActionKind::ClipboardWrite,
            ComputerActionKind::Stop,
            ComputerActionKind::Pause,
            ComputerActionKind::Resume,
            ComputerActionKind::Unknown,
        ];
        use crate::locale::{self, ResolvedLocale};
        use vibex_desktop_model::LocaleMode;

        locale::apply_locale(LocaleMode::ZhCn);
        for kind in kinds {
            let label = action_kind_label(kind);
            assert!(!label.is_empty(), "{kind:?} has no label");
            assert_ne!(
                label,
                kind.as_str(),
                "{kind:?} fell back to its raw token under ZhCn"
            );
        }
        locale::apply_locale(LocaleMode::ZhTw);
        for kind in kinds {
            assert_ne!(
                action_kind_label(kind),
                kind.as_str(),
                "{kind:?} fell back to its raw token under ZhTw"
            );
        }
        locale::apply_locale(LocaleMode::En);
        assert_eq!(locale::current_locale(), ResolvedLocale::En);
    }

    #[test]
    fn the_panel_follows_the_selected_locale() {
        // The process-wide locale is what `text()` reads, so switching it is
        // exactly what a language change does to the panel. Restore English at
        // the end so the rest of the suite sees the default.
        use crate::locale::{self, ResolvedLocale};
        use vibex_desktop_model::LocaleMode;

        locale::apply_locale(LocaleMode::ZhCn);
        assert_eq!(action_kind_label(ComputerActionKind::Click), "点击");
        assert_eq!(platform_label(ComputerPlatform::Macos), "macOS");
        assert_eq!(relative_time(0, 5_000), "5 秒前");

        locale::apply_locale(LocaleMode::ZhTw);
        assert_eq!(action_kind_label(ComputerActionKind::Click), "點擊");

        locale::apply_locale(LocaleMode::En);
        assert_eq!(action_kind_label(ComputerActionKind::Click), "click");
        assert_eq!(relative_time(0, 5_000), "5s ago");
        assert_eq!(locale::current_locale(), ResolvedLocale::En);
    }

    #[test]
    fn every_unavailable_reason_and_platform_has_a_label() {
        for reason in [
            ComputerUnavailableReason::NoDesktopSession,
            ComputerUnavailableReason::AccessibilityBridgeMissing,
            ComputerUnavailableReason::PlatformUnsupported,
            ComputerUnavailableReason::EngineMissing,
            ComputerUnavailableReason::PermissionPending,
            ComputerUnavailableReason::PermissionRestartRequired,
            ComputerUnavailableReason::RunningAsRoot,
            ComputerUnavailableReason::RemoteRuntimeUnsupported,
            ComputerUnavailableReason::FeatureDisabled,
        ] {
            assert!(!unavailable_reason_label(reason).is_empty());
        }
        for platform in [
            ComputerPlatform::Macos,
            ComputerPlatform::Windows,
            ComputerPlatform::LinuxX11,
            ComputerPlatform::LinuxWayland,
            ComputerPlatform::Unknown,
        ] {
            assert!(!platform_label(platform).is_empty());
        }
    }

    #[test]
    fn a_frame_that_is_not_an_image_is_rejected_rather_than_painted() {
        assert!(decode_frame(b"not an image").is_err());
    }
}
