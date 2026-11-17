//! IM channel settings page.
//!
//! The page is a master–detail surface: the channels a runtime knows sit in a
//! list lane, the selected channel fills the work area, and adding a channel is
//! a step in that work area rather than a second form stacked on the page. The
//! page owns its async state instead of mirroring it on the workbench: a create,
//! test, or pairing-code request is a network round trip, and rebuilding the
//! settings tree while it runs would lose focus in the form.
//!
//! Side by side, the lane and the work area are two scroll regions with the
//! page title above them: a long channel list moves under the lane's own
//! heading without taking the selected channel's configuration away, and a long
//! configuration scrolls without hiding the list. The page fills the settings
//! viewport in that layout so the outer page does not move either.
//!
//! Results never become a strip pinned inside the page. The workbench's hint
//! layer already carries action feedback above every dialog, so this page hands
//! it a light hint like the rest of settings; see
//! `.trellis/spec/frontend/quality-guidelines.md`.

use gpui::{
    Anchor, AnyElement, App, ClipboardItem, Context, Entity, Hsla, IntoElement, ParentElement as _,
    Render, ScrollHandle, SharedString, Styled as _, WeakEntity, Window, div, point, prelude::*,
    px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _, Theme, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputState},
    notification::NotificationType,
    scroll::ScrollbarAxis,
    switch::Switch,
    v_flex,
};
use vibex_backend::BackendError;
use vibex_core::{
    AgentSession, ImBindingSummary, ImChannelConfig, ImChannelDraft, ImChannelId, ImChannelKind,
    ImChannelSummary, ImPairingCodeInfo, ImPairingRequest, ImPrincipalId, ImPrincipalSummary,
    ImSessionDefaults, ImTelegramConfig,
};

use crate::app::{SETTINGS_PAGE_PADDING_X, VibexWorkbench, settings_page_width};
use crate::gpui_ext::{PinnedScrollbar as _, ScrollGutter as _};
use crate::hint_layer::{self, hint_notification};
use crate::locale;
use crate::theme;

/// One hint family for this page: a newer IM result replaces the older card.
struct ImChannelHint;

/// The channel list lane. Wide enough for the names a bot carries, visibly
/// narrower than the work area.
const CHANNEL_LANE_WIDTH: f32 = 224.0;
/// The page width below which the lane and the work area stack instead of
/// sitting side by side, so the detail never gets a column too narrow to read.
const CHANNEL_LANE_MIN_PAGE_WIDTH: f32 = 560.0;
/// Where the two-column page starts, measured from the dialog's leading edge.
///
/// The other settings pages align their title on the wider page inset, but this
/// page's first column *is* the master lane: pulling it toward the edge gives
/// the work area beside it the width its forms and panels need.
const CHANNEL_PAGE_LEADING_INSET: f32 = 36.0;
/// The gutter between the lane and the work area.
const CHANNEL_COLUMN_GAP: f32 = 24.0;
/// The glyph an empty page shows above its copy, the same chat mark the
/// settings navigation uses for this section.
const CHANNEL_EMPTY_ICON: &str = "icons/vibex/chat-circle.svg";
/// How many recent sessions the default-session picker offers.
const DEFAULT_SESSION_CHOICES: usize = 5;

/// A result waiting for the window's hint layer.
struct PendingHint {
    tone: NotificationType,
    message: SharedString,
    copy_text: Option<SharedString>,
}

pub struct ImChannelsView {
    workbench: WeakEntity<VibexWorkbench>,
    name_input: Entity<InputState>,
    token_input: Entity<InputState>,
    chat_input: Entity<InputState>,
    telegram_topic_mode: bool,
    channels: Vec<ImChannelSummary>,
    selected: Option<ImChannelId>,
    /// The add-channel step is showing in the work area.
    adding: bool,
    principals: Vec<ImPrincipalSummary>,
    bindings: Vec<ImBindingSummary>,
    default_sessions: Vec<AgentSession>,
    pairing: Option<ImPairingCodeInfo>,
    pending_hint: Option<PendingHint>,
    /// The lane and the work area scroll on their own, so a long channel list
    /// never takes the selected channel's configuration away from the reader.
    lane_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    loading: bool,
    loaded: bool,
    busy: bool,
    task: Option<gpui::Task<()>>,
}

impl ImChannelsView {
    pub fn new(
        workbench: WeakEntity<VibexWorkbench>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Telegram team bot"));
        let token_input = cx.new(|cx| InputState::new(window, cx).placeholder("123456:ABC-DEF..."));
        let chat_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("-1001234567890 or @channel"));
        Self {
            workbench,
            name_input,
            token_input,
            chat_input,
            telegram_topic_mode: false,
            channels: Vec::new(),
            selected: None,
            adding: false,
            principals: Vec::new(),
            bindings: Vec::new(),
            default_sessions: Vec::new(),
            pairing: None,
            pending_hint: None,
            lane_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            loading: false,
            loaded: false,
            busy: false,
            task: None,
        }
    }

    /// Returns the work area to the top when it starts showing another subject.
    ///
    /// The detail is one scroll region reused by every channel, so without this
    /// a reader who scrolled to the last section of one channel would land at
    /// the same offset on the next one, where that section may not exist.
    fn reset_detail_scroll(&self) {
        self.detail_scroll.set_offset(point(px(0.0), px(0.0)));
    }

    fn backend(&self, cx: &App) -> Option<vibex_backend::BackendFacade> {
        self.workbench
            .read_with(cx, |workbench, _| workbench.backend_facade())
            .ok()
            .flatten()
    }

    pub fn reload_public(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    /// Queues a result for the window's top hint layer.
    fn hint(&mut self, tone: NotificationType, message: impl Into<SharedString>) {
        self.pending_hint = Some(PendingHint {
            tone,
            message: message.into(),
            copy_text: None,
        });
    }

    fn hint_with_copy(
        &mut self,
        tone: NotificationType,
        message: impl Into<SharedString>,
        copy_text: impl Into<SharedString>,
    ) {
        self.pending_hint = Some(PendingHint {
            tone,
            message: message.into(),
            copy_text: Some(copy_text.into()),
        });
    }

    /// Hands the queued result to the hint layer on the next frame.
    ///
    /// The push is deferred so it never mutates a plugin entity while the page
    /// is being rendered, and it lands above the settings dialog's backdrop
    /// like every other settings result.
    fn present_hint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_hint.take() else {
            return;
        };
        window.defer(cx, move |window, cx| {
            Theme::global_mut(cx).notification.placement = Anchor::TopCenter;
            let hint = hint_notification(pending.tone, pending.message)
                .id::<ImChannelHint>()
                .autohide(!matches!(pending.tone, NotificationType::Error));
            let hint = match pending.copy_text {
                Some(copy_text) => hint.copy_text(copy_text),
                None => hint,
            };
            hint_layer::push(window, hint, cx);
        });
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        let Some(backend) = self.backend(cx) else {
            self.loading = false;
            self.loaded = true;
            self.hint(
                NotificationType::Error,
                "The runtime has no active backend.".to_string(),
            );
            cx.notify();
            return;
        };
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .list_im_channels()
                .await
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = runner.await;
                let _ = weak.update(cx, |this, cx| {
                    this.loading = false;
                    this.loaded = true;
                    match result {
                        Ok(Ok(channels)) => {
                            this.channels = channels;
                            if this.selected.is_none() {
                                this.selected = this
                                    .channels
                                    .first()
                                    .map(|channel| channel.channel_id.clone());
                            }
                            if this.selected.is_some() {
                                this.reload_selected(cx);
                            }
                        }
                        Ok(Err(error)) => this.hint(NotificationType::Error, error),
                        Err(error) => this.hint(NotificationType::Error, error.to_string()),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn reload_selected(&mut self, cx: &mut Context<Self>) {
        let Some(channel_id) = self.selected.clone() else {
            self.principals.clear();
            self.bindings.clear();
            return;
        };
        let Some(backend) = self.backend(cx) else {
            return;
        };
        let principals = gpui_tokio::Tokio::spawn(cx, {
            let backend = backend.clone();
            let channel_id = channel_id.clone();
            async move {
                backend
                    .management()
                    .list_im_principals(channel_id)
                    .await
                    .map_err(backend_error)
            }
        });
        let bindings = gpui_tokio::Tokio::spawn(cx, {
            let backend = backend.clone();
            let channel_id = channel_id.clone();
            async move {
                backend
                    .management()
                    .list_im_bindings(channel_id)
                    .await
                    .map_err(backend_error)
            }
        });
        let sessions = gpui_tokio::Tokio::spawn(cx, {
            let backend = backend.clone();
            async move {
                backend
                    .agent()
                    .list_sessions(false)
                    .await
                    .map_err(backend_error)
            }
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let principals = principals.await;
                let bindings = bindings.await;
                let sessions = sessions.await;
                let _ = weak.update(cx, |this, cx| {
                    let mut failures = Vec::new();
                    match principals {
                        Ok(Ok(principals)) => this.principals = principals,
                        Ok(Err(error)) => failures.push(error),
                        Err(error) => failures.push(error.to_string()),
                    }
                    match bindings {
                        Ok(Ok(bindings)) => this.bindings = bindings,
                        Ok(Err(error)) => failures.push(error),
                        Err(error) => failures.push(error.to_string()),
                    }
                    match sessions {
                        Ok(Ok(sessions)) => this.default_sessions = sessions,
                        Ok(Err(error)) => failures.push(error),
                        Err(error) => failures.push(error.to_string()),
                    }
                    if let Some(message) = failures.into_iter().next() {
                        this.hint(NotificationType::Error, message);
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn create_channel(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let name = self.name_input.read(cx).value().to_string();
        let token = self.token_input.read(cx).value().to_string();
        let chat_id = self.chat_input.read(cx).value().to_string();
        let topic_mode = self.telegram_topic_mode;
        if name.trim().is_empty() || token.trim().is_empty() || chat_id.trim().is_empty() {
            self.hint(
                NotificationType::Error,
                locale::text(
                    "A name, a bot token, and a chat id are required.",
                    "需要填写名称、Bot token 和聊天 ID。",
                    "需要填寫名稱、Bot token 和聊天 ID。",
                )
                .to_string(),
            );
            cx.notify();
            return;
        }
        let Some(backend) = self.backend(cx) else {
            self.hint(
                NotificationType::Error,
                "The runtime has no active backend.".to_string(),
            );
            cx.notify();
            return;
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let draft = ImChannelDraft {
                display_name: name.trim().to_string(),
                config: ImChannelConfig::Telegram(ImTelegramConfig {
                    chat_id: chat_id.trim().to_string(),
                    topic_mode,
                    require_mention_in_groups: true,
                }),
                default_session: None,
                event_filter: Vec::new(),
                locale: None,
            };
            let channel = backend
                .management()
                .create_im_channel(vibex_backend::MutationRequest::new(draft))
                .await
                .map_err(backend_error)?;
            let channel_id = channel.channel_id.clone();
            backend
                .management()
                .set_im_channel_bot_token(vibex_backend::MutationRequest::new(
                    vibex_backend::ImChannelSecretUpdateRequest {
                        channel_id: channel_id.clone(),
                        bot_token: token.trim().to_string(),
                    },
                ))
                .await
                .map_err(backend_error)?;
            backend
                .management()
                .set_im_channel_enabled(vibex_backend::MutationRequest::new(
                    vibex_backend::ImChannelEnabledRequest {
                        channel_id: channel_id.clone(),
                        enabled: true,
                    },
                ))
                .await
                .map_err(backend_error)?;
            Ok::<ImChannelId, String>(channel_id)
        });
        self.task = Some(cx.spawn(async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            let result = match runner.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(channel_id) => {
                        this.adding = false;
                        this.selected = Some(channel_id);
                        this.pairing = None;
                        this.reset_detail_scroll();
                        this.hint(
                            NotificationType::Success,
                            locale::text(
                                "Telegram channel added. Generate a pairing code, then send /pair <code> in a direct message.",
                                "Telegram 频道已添加。生成配对码，然后在私聊中发送 /pair <配对码>。",
                                "Telegram 頻道已新增。產生配對碼，然後在私聊中傳送 /pair <配對碼>。",
                            )
                            .to_string(),
                        );
                        this.loaded = false;
                        this.loading = false;
                        this.reload(cx);
                    }
                    Err(error) => this.hint(NotificationType::Error, error),
                }
                cx.notify();
            });
        }));
    }

    fn toggle_channel(&mut self, channel_id: ImChannelId, enabled: bool, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .set_im_channel_enabled(vibex_backend::MutationRequest::new(
                    vibex_backend::ImChannelEnabledRequest {
                        channel_id,
                        enabled,
                    },
                ))
                .await
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = match runner.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                let _ = weak.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(_) => {
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.hint(NotificationType::Error, error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn delete_channel(&mut self, channel_id: ImChannelId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .delete_im_channel(vibex_backend::MutationRequest::new(channel_id))
                .await
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = match runner.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                let _ = weak.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(()) => {
                            this.selected = None;
                            this.pairing = None;
                            this.principals.clear();
                            this.bindings.clear();
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.hint(NotificationType::Error, error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    /// Confirms before deleting, because the channel's secret goes with it.
    fn confirm_delete(
        &mut self,
        channel_id: ImChannelId,
        display_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.weak_entity();
        let is_dark = cx.theme().is_dark();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let channel_id = channel_id.clone();
            let display_name = display_name.clone();
            dialog
                .title(locale::text(
                    "Delete this channel?",
                    "删除这个频道？",
                    "刪除這個頻道？",
                ))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().whitespace_normal().child(locale::text(
                            "The bot token or app secret stored for this channel is erased, and its chats stop reaching the runtime.",
                            "该频道保存的 Bot token 或 App secret 会被删除，它的聊天将无法再连到运行时。",
                            "該頻道儲存的 Bot token 或 App secret 會被刪除，它的聊天將無法再連到執行階段。",
                        )))
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded(px(6.0))
                                .bg(theme::module_surface(is_dark))
                                .text_sm()
                                .font_semibold()
                                .child(display_name),
                        ),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(
                            Button::new("im-channel-delete-cancel")
                                .outline()
                                .label(locale::text("Cancel", "取消", "取消")),
                        ))
                        .child(DialogAction::new().child(
                            Button::new("im-channel-delete-confirm")
                                .danger()
                                .label(locale::text("Delete", "删除", "刪除")),
                        )),
                )
                .on_ok(move |_, _, cx| {
                    let _ =
                        entity.update(cx, |this, cx| this.delete_channel(channel_id.clone(), cx));
                    true
                })
        });
    }

    fn test_channel(&mut self, channel_id: ImChannelId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .test_im_channel(channel_id)
                .await
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = match runner.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                let _ = weak.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(report) => {
                            this.hint(
                                NotificationType::Success,
                                format!(
                                    "Connected to {}{}.",
                                    report.bot_name.unwrap_or_else(|| "Telegram".to_string()),
                                    report
                                        .chat_title
                                        .map(|title| format!(" in {title}"))
                                        .unwrap_or_default()
                                ),
                            );
                        }
                        Err(error) => this.hint(NotificationType::Error, error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn set_default_session(
        &mut self,
        channel_id: ImChannelId,
        session: AgentSession,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.busy = true;
        // The IM service reconnects the channel when its default session
        // changes, and the poller it starts boots with `tokio::spawn`. Polling
        // that future from this GPUI task runs the spawn without a reactor and
        // panics with "there is no reactor running", so the work goes to Tokio
        // and the UI task only waits for the answer.
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let selection = backend
                .agent()
                .runtime_selection(session.id.clone())
                .await
                .map_err(backend_error)?;
            let defaults = ImSessionDefaults {
                workspace_root: session.workspace_root.clone(),
                workspace_mode: session.workspace_mode,
                agent_id: selection.desired.agent_id.clone(),
                auth_source: selection.desired.auth_source.clone(),
                model: selection.desired.model.clone(),
                reasoning_effort: selection.desired.reasoning_effort.clone(),
                mode_id: selection.desired.mode_id.clone(),
                config_values: selection.desired.config_values.clone(),
            };
            backend
                .management()
                .set_im_channel_default_session(vibex_backend::MutationRequest::new(
                    vibex_backend::ImChannelDefaultSessionUpdate {
                        channel_id,
                        default_session: Some(defaults),
                    },
                ))
                .await
                .map(|_| ())
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = match runner.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                let _ = weak.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(()) => {
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.hint(NotificationType::Error, error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn clear_default_session(&mut self, channel_id: ImChannelId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        // Clearing the default reconnects the channel too; see
        // `set_default_session` for why this belongs on the Tokio runtime.
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .set_im_channel_default_session(vibex_backend::MutationRequest::new(
                    vibex_backend::ImChannelDefaultSessionUpdate {
                        channel_id,
                        default_session: None,
                    },
                ))
                .await
                .map(|_| ())
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = match runner.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.hint(NotificationType::Error, error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn create_pairing_code(&mut self, channel_id: ImChannelId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .create_im_pairing_code(vibex_backend::MutationRequest::new(ImPairingRequest {
                    channel_id,
                    ttl_ms: Some(600_000),
                    scope: vibex_core::ImPairingScope::Direct,
                    target_chat_id: None,
                }))
                .await
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            let result = match runner.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(pairing) => {
                        this.hint_with_copy(
                            NotificationType::Success,
                            locale::text(
                                "Pairing code created. Send /pair <code> in a direct message within 10 minutes.",
                                "配对码已生成。请在 10 分钟内私聊发送 /pair <配对码>。",
                                "配對碼已產生。請在 10 分鐘內私聊傳送 /pair <配對碼>。",
                            )
                            .to_string(),
                            pairing.code.clone(),
                        );
                        this.pairing = Some(pairing);
                    }
                    Err(error) => this.hint(NotificationType::Error, error),
                }
                cx.notify();
            });
        }));
    }

    fn revoke_principal(&mut self, principal_id: ImPrincipalId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .revoke_im_principal(vibex_backend::MutationRequest::new(principal_id))
                .await
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = match runner.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => this.reload_selected(cx),
                        Err(error) => this.hint(NotificationType::Error, error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn set_channel_locale(
        &mut self,
        channel_id: ImChannelId,
        locale: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .set_im_channel_locale(vibex_backend::MutationRequest::new(
                    vibex_backend::ImChannelLocaleUpdate { channel_id, locale },
                ))
                .await
                .map(|_| ())
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = match runner.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.hint(NotificationType::Error, error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn revoke_binding(&mut self, binding_id: vibex_core::ImBindingId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .management()
                .revoke_im_binding(vibex_backend::MutationRequest::new(binding_id))
                .await
                .map_err(backend_error)
        });
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = match runner.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.hint(NotificationType::Error, error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn select_channel(&mut self, channel_id: ImChannelId, cx: &mut Context<Self>) {
        self.selected = Some(channel_id);
        self.adding = false;
        self.pairing = None;
        self.reset_detail_scroll();
        self.reload_selected(cx);
        cx.notify();
    }

    fn begin_add(&mut self, cx: &mut Context<Self>) {
        self.adding = true;
        self.pairing = None;
        self.reset_detail_scroll();
        cx.notify();
    }

    fn cancel_add(&mut self, cx: &mut Context<Self>) {
        self.adding = false;
        self.reset_detail_scroll();
        cx.notify();
    }

    fn channel_kind_label(&self, kind: ImChannelKind) -> &'static str {
        match kind {
            ImChannelKind::Telegram => "Telegram",
            ImChannelKind::Lark => "Lark",
        }
    }

    fn status_dot(&self, channel: &ImChannelSummary, cx: &App) -> AnyElement {
        let color = match channel.status.state {
            vibex_core::ImChannelState::Connected => cx.theme().success,
            vibex_core::ImChannelState::Degraded => cx.theme().warning,
            vibex_core::ImChannelState::Starting => cx.theme().primary,
            _ => cx.theme().muted_foreground,
        };
        div()
            .flex_none()
            .size(px(8.0))
            .rounded_full()
            .bg(color)
            .into_any_element()
    }

    fn status_label(&self, channel: &ImChannelSummary) -> &'static str {
        match channel.status.state {
            vibex_core::ImChannelState::Connected => locale::text("Connected", "已连接", "已連線"),
            vibex_core::ImChannelState::Starting => {
                locale::text("Starting", "正在连接", "正在連線")
            }
            vibex_core::ImChannelState::Degraded => {
                locale::text("Needs attention", "需要处理", "需要處理")
            }
            vibex_core::ImChannelState::Stopped => locale::text("Stopped", "已停止", "已停止"),
            vibex_core::ImChannelState::Disabled => locale::text("Disabled", "未启用", "未啟用"),
        }
    }
}

/// A rounded plate whose rows are separated by an inset hairline.
fn panel(rows: Vec<AnyElement>, is_dark: bool, border: Hsla) -> AnyElement {
    let mut separated = Vec::with_capacity(rows.len().saturating_mul(2));
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            separated.push(
                div()
                    .flex_none()
                    .h(px(1.0))
                    .bg(border.opacity(0.5))
                    .into_any_element(),
            );
        }
        separated.push(row);
    }
    v_flex()
        .w_full()
        .min_w_0()
        .rounded(px(10.0))
        .bg(theme::module_surface(is_dark))
        .px_4()
        .py_1()
        .children(separated)
        .into_any_element()
}

/// A labeled section: the subject, optional help, and its body.
fn section(
    title: &'static str,
    description: Option<String>,
    body: AnyElement,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .w_full()
        .min_w_0()
        .gap_3()
        .child(
            v_flex()
                .gap_1()
                .child(div().text_sm().font_semibold().child(title))
                .when_some(description, |this, description| {
                    this.child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(muted)
                            .child(description),
                    )
                }),
        )
        .child(body)
        .into_any_element()
}

/// A row's identity: what it is, and one line of supporting description.
fn row_text(title: impl IntoElement, description: Option<String>, cx: &App) -> AnyElement {
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(foreground)
                .whitespace_normal()
                .child(title),
        )
        .when_some(description, |this, description| {
            this.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .whitespace_normal()
                    .child(description),
            )
        })
        .into_any_element()
}

impl Render for ImChannelsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.loaded && !self.loading {
            self.reload(cx);
        }
        self.present_hint(window, cx);

        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let page_width = settings_page_width(f32::from(window.viewport_size().width));
        // The lane and the work area only split once the page is wide enough
        // to give both a readable column; below that they stack and the page
        // itself scrolls.
        let wide = page_width >= CHANNEL_LANE_MIN_PAGE_WIDTH;
        let has_channels = !self.channels.is_empty();
        // Side by side, the page owns a definite height: the title band stays
        // put and the lane and the work area each scroll what does not fit.
        // The settings viewport above must not scroll as well, or a wheel over
        // a column would move the page's title out from under the reader.
        let split = has_channels && wide;

        let header = h_flex()
            .w_full()
            .min_w_0()
            .flex_none()
            .debug_selector(|| "im-channels-header".to_string())
            .justify_between()
            .flex_wrap()
            .items_start()
            .gap_4()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .text_color(foreground)
                            .child(locale::text("IM channels", "IM 频道", "IM 頻道")),
                    )
                    .child(div().text_xs().whitespace_normal().text_color(muted).child(
                        locale::text(
                            "Chat with Vibex and approve tool calls from Telegram.",
                            "通过 Telegram 与 Vibex 对话并批准工具调用。",
                            "透過 Telegram 與 Vibex 對話並批准工具呼叫。",
                        ),
                    )),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        Button::new("im-channels-refresh")
                            .small()
                            .ghost()
                            .label(locale::text("Refresh", "刷新", "重新整理"))
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    // With no channels the empty state owns the add action, so
                    // the header does not offer a second, identical one.
                    .when(!self.channels.is_empty(), |this| {
                        this.child(
                            Button::new("im-channels-add")
                                .small()
                                .label(locale::text(
                                    "Add Telegram bot",
                                    "添加 Telegram 机器人",
                                    "新增 Telegram 機器人",
                                ))
                                .disabled(self.busy || self.adding)
                                .on_click(cx.listener(|this, _, _, cx| this.begin_add(cx))),
                        )
                    }),
            );

        let body = if !has_channels {
            if self.adding {
                self.render_add_form(cx)
            } else {
                empty_state(
                    locale::text("No channels yet", "还没有频道", "還沒有頻道"),
                    locale::text(
                        "Add a Telegram bot to reach this runtime from chat.",
                        "添加一个 Telegram 机器人，即可在聊天里使用这个运行时。",
                        "新增一個 Telegram 機器人，即可在聊天裡使用這個執行階段。",
                    ),
                    Button::new("im-channels-add-empty")
                        .small()
                        .label(locale::text(
                            "Add Telegram bot",
                            "添加 Telegram 机器人",
                            "新增 Telegram 機器人",
                        ))
                        .on_click(cx.listener(|this, _, _, cx| this.begin_add(cx))),
                    cx,
                )
            }
        } else if wide {
            h_flex()
                .w_full()
                .min_w_0()
                .flex_1()
                .min_h_0()
                // `h_flex` centers its children on the cross axis; the columns
                // have to share the band's whole height instead, or neither
                // scroll region gets a definite viewport.
                .items_stretch()
                .gap(px(CHANNEL_COLUMN_GAP))
                .child(self.render_channel_lane(true, cx))
                .child(self.render_work_area_scroll(cx))
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .min_w_0()
                .gap_6()
                .child(self.render_channel_lane(false, cx))
                .child(self.render_work_area(cx))
                .into_any_element()
        };

        v_flex()
            .id("im-channels-page")
            .debug_selector(|| "im-channels-page".to_string())
            .w_full()
            .min_w_0()
            .gap_6()
            // The two-column page pulls its leading column toward the dialog
            // edge: the lane is the first thing the page shows, and the work
            // area beside it needs the width. The page's own title shares the
            // lane's edge, and every other settings page keeps the wider inset.
            .pl(px(if split {
                CHANNEL_PAGE_LEADING_INSET
            } else {
                SETTINGS_PAGE_PADDING_X
            }))
            .pr(px(SETTINGS_PAGE_PADDING_X))
            .py_8()
            .text_color(foreground)
            .when(split, |this| this.h_full().min_h_0())
            .child(header)
            .child(body)
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(muted)
                    .child(locale::text(
                        "Approvals use the buttons on the card, or the printed short code. Images and documents up to 10 MB are accepted; long answers are split across messages.",
                        "审批可通过卡片按钮或卡片上的短码完成。接受不超过 10 MB 的图片和文档；超长回答会自动分段发送。",
                        "審批可透過卡片按鈕或卡片上的短碼完成。接受不超過 10 MB 的圖片和文件；超長回覆會自動分段傳送。",
                    )),
            )
    }
}

impl ImChannelsView {
    /// The list lane: one selectable row per channel.
    ///
    /// Side by side with the work area, the lane keeps its "Channels" heading
    /// in place and scrolls only the rows under it, so the page's title and
    /// the lane's own subject stay visible while the reader moves through a
    /// long list. Stacked, the lane grows with its rows and the page scrolls.
    fn render_channel_lane(&self, wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let surface = cx.theme().secondary;

        let mut rows = v_flex().w_full().min_w_0().gap_1();
        for channel in &self.channels {
            let channel_id = channel.channel_id.clone();
            let selected = self.selected.as_ref() == Some(&channel.channel_id);
            let meta = format!(
                "{} · {}",
                self.channel_kind_label(channel.kind),
                self.status_label(channel)
            );
            rows = rows.child(
                Button::new(SharedString::from(format!(
                    "im-channel-select-{}",
                    channel.channel_id.as_str()
                )))
                .small()
                .ghost()
                .w_full()
                .h(px(32.0))
                .justify_start()
                .px_2()
                .rounded(px(6.0))
                .selected(selected)
                .when(selected, |this| this.bg(surface))
                .child(self.status_dot(channel, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(if selected { foreground } else { muted })
                        .truncate()
                        .child(channel.display_name.clone()),
                )
                .child(div().flex_none().text_xs().text_color(muted).child(meta))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.select_channel(channel_id.clone(), cx)),
                ),
            );
        }

        let heading = div()
            .flex_none()
            .text_xs()
            .font_semibold()
            .text_color(muted)
            .child(locale::text("Channels", "频道", "頻道"));

        let lane = if wide {
            v_flex()
                .w(px(CHANNEL_LANE_WIDTH))
                .flex_none()
                .min_h_0()
                .gap_2()
                .child(heading)
                .child(
                    v_flex()
                        .id("im-channel-lane-scroll")
                        .debug_selector(|| "im-channel-lane-scroll".to_string())
                        .w_full()
                        .flex_1()
                        .min_h_0()
                        .track_scroll(&self.lane_scroll)
                        .overflow_y_scroll()
                        // The rows end flush with the lane's trailing edge, so
                        // the bar needs a lane of its own to keep the status
                        // text out from under the thumb.
                        .scroll_gutter()
                        .child(rows)
                        .pinned_scrollbar(&self.lane_scroll, ScrollbarAxis::Vertical),
                )
        } else {
            v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(heading)
                .child(rows)
        };
        lane.into_any_element()
    }

    /// The work area inside its own scroll region.
    ///
    /// The lane and the work area are separate scroll regions on purpose: a
    /// long channel list must never take the selected channel's configuration
    /// away, and a long configuration must never hide the list of channels.
    fn render_work_area_scroll(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("im-channel-work-area-scroll")
            .debug_selector(|| "im-channel-work-area-scroll".to_string())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .track_scroll(&self.detail_scroll)
            .overflow_y_scroll()
            .scroll_gutter()
            .child(self.render_work_area(cx))
            .pinned_scrollbar(&self.detail_scroll, ScrollbarAxis::Vertical)
            .into_any_element()
    }

    /// The work area: the add step, the selected channel, or a resting state.
    fn render_work_area(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.adding {
            return self.render_add_form(cx);
        }
        let Some(channel) = self
            .selected
            .as_ref()
            .and_then(|channel_id| {
                self.channels
                    .iter()
                    .find(|channel| &channel.channel_id == channel_id)
            })
            .cloned()
        else {
            return v_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .justify_center()
                .py_8()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(locale::text(
                            "Select a channel to manage it.",
                            "选择一个频道进行管理。",
                            "選擇一個頻道進行管理。",
                        )),
                )
                .into_any_element();
        };
        self.render_channel_detail(&channel, cx)
    }

    /// The add-channel step: one short form, committed with its own button.
    fn render_add_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let is_dark = cx.theme().is_dark();
        let border = cx.theme().border;

        let field = |label: &'static str, input: AnyElement, cx: &App| {
            let muted = cx.theme().muted_foreground;
            v_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(div().text_xs().text_color(muted).child(label))
                .child(input)
                .into_any_element()
        };

        let form = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(field(
                locale::text("Name", "名称", "名稱"),
                Input::new(&self.name_input).small().w_full().into_any_element(),
                cx,
            ))
            .child(field(
                locale::text("Bot token", "Bot token", "Bot token"),
                Input::new(&self.token_input).small().w_full().into_any_element(),
                cx,
            ))
            .child(field(
                locale::text("Chat id", "聊天 ID", "聊天 ID"),
                Input::new(&self.chat_input).small().w_full().into_any_element(),
                cx,
            ))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .items_center()
                    .py_2()
                    .child(
                        Switch::new("im-telegram-topic-mode")
                            .small()
                            .checked(self.telegram_topic_mode)
                            .on_click(cx.listener(|this, enabled, _, cx| {
                                this.telegram_topic_mode = *enabled;
                                cx.notify();
                            })),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().text_color(foreground).child(locale::text(
                                "Forum topic mode",
                                "话题模式",
                                "話題模式",
                            )))
                            .child(div().text_xs().text_color(muted).whitespace_normal().child(
                                locale::text(
                                    "Create one topic per /new session. The bot must be a forum admin.",
                                    "每个 /new 会话创建一个话题。机器人必须是论坛管理员。",
                                    "每個 /new 工作階段建立一個話題。機器人必須是論壇管理員。",
                                ),
                            )),
                    ),
            );
        let requirements = panel(
            vec![
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .py_3()
                    .child(row_text(
                        locale::text("Where the token lives", "Token 存放位置", "Token 存放位置"),
                        Some(
                            locale::text(
                                "The bot token is stored in the OS keychain, or the runtime's host secret file on a headless deployment.",
                                "Bot token 存在操作系统钥匙串；无界面部署则存在运行时的主机密钥文件。",
                                "Bot token 存在作業系統鑰匙圈；無介面部署則存在執行時的主機密鑰檔案。",
                            )
                            .to_string(),
                        ),
                        cx,
                    ))
                    .into_any_element(),
            ],
            is_dark,
            border,
        );

        v_flex()
            .w_full()
            .min_w_0()
            .gap_6()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .justify_between()
                    .flex_wrap()
                    .items_start()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_base().font_semibold().text_color(foreground).child(
                                locale::text("Add a Telegram bot", "添加 Telegram 机器人", "新增 Telegram 機器人"),
                            ))
                            .child(div().text_xs().text_color(muted).whitespace_normal().child(
                                locale::text(
                                    "Create the bot with @BotFather, then paste its token and the chat it should serve.",
                                    "先用 @BotFather 创建机器人，再填入它的 token 和要服务的聊天。",
                                    "先用 @BotFather 建立機器人，再填入它的 token 和要服務的聊天。",
                                ),
                            )),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .child(
                                Button::new("im-channel-add-cancel")
                                    .small()
                                    .ghost()
                                    .label(locale::text("Cancel", "取消", "取消"))
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_add(cx))),
                            )
                            .child(
                                Button::new("im-channel-create")
                                    .small()
                                    .primary()
                                    .loading(self.busy)
                                    .label(locale::text("Add bot", "添加机器人", "新增機器人"))
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.create_channel(cx))),
                            ),
                    ),
            )
            .child(form)
            .child(requirements)
            .into_any_element()
    }

    /// The selected channel, grouped into the subjects an operator works with.
    fn render_channel_detail(
        &self,
        channel: &ImChannelSummary,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let is_dark = cx.theme().is_dark();
        let channel_id = channel.channel_id.clone();

        let header = h_flex()
            .w_full()
            .min_w_0()
            .justify_between()
            .flex_wrap()
            .items_start()
            .gap_4()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .items_center()
                            .child(self.status_dot(channel, cx))
                            .child(
                                div()
                                    .text_base()
                                    .font_semibold()
                                    .text_color(foreground)
                                    .truncate()
                                    .child(channel.display_name.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(self.status_label(channel)),
                            ),
                    )
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{} · {}",
                        self.channel_kind_label(channel.kind),
                        if channel.configured {
                            locale::text("Secret configured", "密钥已配置", "密鑰已設定")
                        } else {
                            locale::text("Secret missing", "缺少密钥", "缺少密鑰")
                        }
                    ))),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_3()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(locale::text("Enabled", "启用", "啟用")),
                            )
                            .child(
                                Switch::new(format!(
                                    "im-channel-enabled-{}",
                                    channel.channel_id.as_str()
                                ))
                                .small()
                                .checked(channel.enabled)
                                .disabled(self.busy)
                                .on_click(cx.listener({
                                    let channel_id = channel_id.clone();
                                    move |this, enabled, _, cx| {
                                        this.toggle_channel(channel_id.clone(), *enabled, cx);
                                    }
                                })),
                            ),
                    )
                    .child(
                        Button::new("im-channel-test")
                            .small()
                            .outline()
                            .label(locale::text("Test", "测试", "測試"))
                            .disabled(self.busy)
                            .on_click(cx.listener({
                                let channel_id = channel_id.clone();
                                move |this, _, _, cx| this.test_channel(channel_id.clone(), cx)
                            })),
                    )
                    .child(
                        Button::new("im-channel-delete")
                            .small()
                            .ghost()
                            .text_color(cx.theme().danger)
                            .label(locale::text("Delete", "删除", "刪除"))
                            .disabled(self.busy)
                            .on_click(cx.listener({
                                let channel_id = channel_id.clone();
                                let display_name = channel.display_name.clone();
                                move |this, _, window, cx| {
                                    this.confirm_delete(
                                        channel_id.clone(),
                                        display_name.clone(),
                                        window,
                                        cx,
                                    )
                                }
                            })),
                    ),
            );

        // One row for pairing: the code replaces the subject once it exists,
        // so the row does not grow a second line for the same fact.
        let pairing_title: AnyElement = match self
            .pairing
            .as_ref()
            .filter(|pairing| pairing.channel_id == channel_id)
        {
            Some(pairing) => {
                let code = pairing.code.clone();
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .child(SharedString::from(code.clone())),
                    )
                    .child(
                        Button::new("im-channel-pair-copy")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Copy)
                            .tooltip(locale::text(
                                "Copy pairing code",
                                "复制配对码",
                                "複製配對碼",
                            ))
                            .accessibility_label(locale::text(
                                "Copy pairing code",
                                "复制配对码",
                                "複製配對碼",
                            ))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                            }),
                    )
                    .into_any_element()
            }
            None => div()
                .text_sm()
                .child(locale::text("Pairing code", "配对码", "配對碼"))
                .into_any_element(),
        };
        let pairing_rows = vec![
            h_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .py_3()
                .items_center()
                .child(row_text(
                    pairing_title,
                    Some(
                        locale::text(
                            "Send /pair <code> to the bot in a direct message. The code expires in 10 minutes.",
                            "在私聊中向机器人发送 /pair <配对码>。配对码 10 分钟后过期。",
                            "在私聊中向機器人傳送 /pair <配對碼>。配對碼 10 分鐘後過期。",
                        )
                        .to_string(),
                    ),
                    cx,
                ))
                .child(
                    Button::new("im-channel-pair")
                        .small()
                        .outline()
                        .label(locale::text("Generate code", "生成配对码", "產生配對碼"))
                        .disabled(self.busy)
                        .on_click(cx.listener({
                            let channel_id = channel_id.clone();
                            move |this, _, _, cx| this.create_pairing_code(channel_id.clone(), cx)
                        })),
                )
                .into_any_element(),
        ];

        let mut identity_rows = Vec::new();
        if self.principals.is_empty() {
            identity_rows.push(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .py_3()
                    .child(div().text_xs().text_color(muted).child(locale::text(
                        "No identity has paired with this channel yet.",
                        "还没有身份配对这个频道。",
                        "還沒有身分配對這個頻道。",
                    )))
                    .into_any_element(),
            );
        } else {
            for principal in &self.principals {
                let principal_id = principal.principal_id.clone();
                identity_rows.push(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_3()
                        .py_3()
                        .items_center()
                        .child(row_text(
                            principal.display_name.clone(),
                            Some(format!("{:?}", principal.status)),
                            cx,
                        ))
                        .child(
                            Button::new(SharedString::from(format!(
                                "im-principal-revoke-{}",
                                principal.principal_id.as_str()
                            )))
                            .small()
                            .ghost()
                            .label(locale::text("Revoke", "撤销", "撤銷"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.revoke_principal(principal_id.clone(), cx)
                                },
                            )),
                        )
                        .into_any_element(),
                );
            }
        }

        let mut binding_rows = Vec::new();
        if self.bindings.is_empty() {
            binding_rows.push(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .py_3()
                    .child(div().text_xs().text_color(muted).child(locale::text(
                        "No chat is bound to a session yet. Use /use in the chat, or /new.",
                        "还没有聊天绑定到会话。可在聊天里用 /use，或用 /new。",
                        "還沒有聊天綁定到工作階段。可在聊天裡用 /use，或用 /new。",
                    )))
                    .into_any_element(),
            );
        } else {
            for binding in &self.bindings {
                let binding_id = binding.binding_id.clone();
                let title = binding
                    .title
                    .clone()
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| {
                        format!(
                            "session {}",
                            &binding.session_id.as_str()
                                [..8.min(binding.session_id.as_str().len())]
                        )
                    });
                let thread = if binding.thread_key.is_empty() {
                    String::new()
                } else {
                    format!(" · thread {}", binding.thread_key)
                };
                binding_rows.push(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_3()
                        .py_3()
                        .items_center()
                        .child(row_text(
                            title,
                            Some(format!(
                                "chat {} · {:?}{thread}",
                                binding.external_chat_id, binding.status
                            )),
                            cx,
                        ))
                        .child(
                            Button::new(SharedString::from(format!(
                                "im-binding-revoke-{}",
                                binding.binding_id.as_str()
                            )))
                            .small()
                            .ghost()
                            .label(locale::text("Unbind", "解绑", "解除綁定"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.revoke_binding(binding_id.clone(), cx),
                            )),
                        )
                        .into_any_element(),
                );
            }
        }

        let mut default_rows = vec![
            h_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .py_3()
                .items_center()
                .child(row_text(
                    locale::text(
                        "Default session for /new",
                        "/new 的默认会话",
                        "/new 的預設工作階段",
                    ),
                    Some(if channel.default_session.is_some() {
                        locale::text(
                            "Configured. /new can start without a bound session.",
                            "已配置。/new 无需绑定会话即可开始。",
                            "已設定。/new 無需綁定工作階段即可開始。",
                        )
                        .to_string()
                    } else {
                        locale::text(
                            "Pick a recent session to reuse its workspace and Agent.",
                            "选一个最近的会话，复用它当时的工作区和 Agent。",
                            "選一個最近的工作階段，重複使用它當時的工作區和 Agent。",
                        )
                        .to_string()
                    }),
                    cx,
                ))
                .when(channel.default_session.is_some(), |this| {
                    this.child(
                        Button::new("im-default-clear")
                            .small()
                            .ghost()
                            .label(locale::text("Clear", "清除", "清除"))
                            .disabled(self.busy)
                            .on_click(cx.listener({
                                let channel_id = channel_id.clone();
                                move |this, _, _, cx| {
                                    this.clear_default_session(channel_id.clone(), cx)
                                }
                            })),
                    )
                })
                .into_any_element(),
        ];
        for session in self.default_sessions.iter().take(DEFAULT_SESSION_CHOICES) {
            let session = session.clone();
            let short = session
                .id
                .as_str()
                .rsplit('_')
                .next()
                .unwrap_or_default()
                .chars()
                .take(8)
                .collect::<String>();
            default_rows.push(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .py_3()
                    .items_center()
                    .child(row_text(format!("#{short} · {}", session.title), None, cx))
                    .child(
                        Button::new(SharedString::from(format!(
                            "im-default-use-{}",
                            session.id.as_str()
                        )))
                        .small()
                        .outline()
                        .label(locale::text("Use as default", "设为默认", "設為預設"))
                        .disabled(self.busy)
                        .on_click(cx.listener({
                            let channel_id = channel_id.clone();
                            move |this, _, _, cx| {
                                this.set_default_session(channel_id.clone(), session.clone(), cx)
                            }
                        })),
                    )
                    .into_any_element(),
            );
        }

        let locale_choice = |id: String,
                             label: &'static str,
                             value: Option<&'static str>,
                             current: Option<&str>,
                             channel_id: ImChannelId,
                             cx: &mut Context<Self>| {
            let selected = current == value;
            Button::new(SharedString::from(id))
                .small()
                .ghost()
                .selected(selected)
                .when(selected, |this| this.bg(cx.theme().secondary))
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_channel_locale(channel_id.clone(), value.map(str::to_string), cx)
                }))
                .into_any_element()
        };
        let language_row = panel(
            vec![
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .py_3()
                    .items_center()
                    .child(row_text(
                        locale::text("Reply language", "回复语言", "回覆語言"),
                        Some(
                            locale::text(
                                "How the bot speaks in this chat; the host language is the default.",
                                "机器人在这个聊天里使用的语言；默认跟随系统。",
                                "機器人在這個聊天裡使用的語言；預設跟隨系統。",
                            )
                            .to_string(),
                        ),
                        cx,
                    ))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .child(locale_choice(
                                format!("im-locale-en-{}", channel_id.as_str()),
                                "English",
                                Some("en"),
                                channel.locale.as_deref(),
                                channel_id.clone(),
                                cx,
                            ))
                            .child(locale_choice(
                                format!("im-locale-zh-{}", channel_id.as_str()),
                                "中文",
                                Some("zh-CN"),
                                channel.locale.as_deref(),
                                channel_id.clone(),
                                cx,
                            ))
                            .child(locale_choice(
                                format!("im-locale-auto-{}", channel_id.as_str()),
                                locale::text("Host", "跟随系统", "跟隨系統"),
                                None,
                                channel.locale.as_deref(),
                                channel_id.clone(),
                                cx,
                            )),
                    )
                    .into_any_element(),
            ],
            is_dark,
            border,
        );

        v_flex()
            .w_full()
            .min_w_0()
            .gap_6()
            .child(header)
            .child(section(
                locale::text("Pairing", "配对", "配對"),
                Some(
                    locale::text(
                        "A pairing code is how a chat proves it belongs to this channel.",
                        "配对码用于让一个聊天证明自己属于这个频道。",
                        "配對碼用於讓一個聊天證明自己屬於這個頻道。",
                    )
                    .to_string(),
                ),
                panel(pairing_rows, is_dark, border),
                cx,
            ))
            .child(section(
                locale::text("Paired identities", "已配对身份", "已配對身分"),
                Some(format!(
                    "{} ({})",
                    locale::text(
                        "Who may talk to this channel",
                        "可以与该频道对话的身份",
                        "可以與該頻道對話的身分",
                    ),
                    self.principals.len()
                )),
                panel(identity_rows, is_dark, border),
                cx,
            ))
            .child(section(
                locale::text("Session bindings", "会话绑定", "工作階段綁定"),
                Some(format!(
                    "{} ({})",
                    locale::text(
                        "Chats currently driving a session",
                        "当前正在驱动会话的聊天",
                        "目前正在驅動工作階段的聊天",
                    ),
                    self.bindings.len()
                )),
                panel(binding_rows, is_dark, border),
                cx,
            ))
            .child(section(
                locale::text("New sessions", "新会话", "新工作階段"),
                Some(
                    locale::text(
                        "/new reuses the workspace and Agent of the session chosen here.",
                        "/new 会复用这里所选会话的工作区和 Agent。",
                        "/new 會重複使用這裡所選工作階段的工作區和 Agent。",
                    )
                    .to_string(),
                ),
                panel(default_rows, is_dark, border),
                cx,
            ))
            .child(section(
                locale::text("Language", "语言", "語言"),
                None,
                language_row,
                cx,
            ))
            .into_any_element()
    }
}

/// A first-run or empty-collection surface: what is missing and the next step.
///
/// The resting surface is the same plate a settings group paints — the module
/// fill, rounded, without an outline — which is how the remote-instance page
/// shows its own empty state. An outlined card inside the settings page read as
/// a second dialog rather than as the page's own surface.
fn empty_state(
    title: &'static str,
    description: &'static str,
    action: Button,
    cx: &App,
) -> AnyElement {
    let is_dark = cx.theme().is_dark();
    let muted = cx.theme().muted_foreground;
    v_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .justify_center()
        .gap_3()
        .px_6()
        .py_8()
        .rounded(px(10.0))
        .bg(theme::module_surface(is_dark))
        .child(
            Icon::default()
                .path(CHANNEL_EMPTY_ICON)
                .size(px(28.0))
                .text_color(muted),
        )
        .child(div().text_sm().font_semibold().child(title))
        .child(
            div()
                .max_w(px(420.0))
                .text_center()
                .text_xs()
                .whitespace_normal()
                .text_color(muted)
                .child(description),
        )
        .child(action)
        .into_any_element()
}

fn backend_error(error: BackendError) -> String {
    format!("{}: {}", error.code, error.message)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use gpui::{ScrollDelta, ScrollEvent, TestAppContext};

    /// The page is a source-level contract in two places that a render test
    /// would need the whole workbench to reach.
    #[test]
    fn the_page_offers_telegram_only_and_reports_through_the_hint_layer() {
        let source = include_str!("im_channels.rs");
        // Only the Telegram entry exists; the Lark app form is gone. The
        // needles are split because the test's own text is part of `source`.
        assert!(source.contains("Add Telegram bot"));
        assert!(!source.contains(concat!("create_lark", "_channel")));
        assert!(!source.contains(concat!("im-lark", "-create")));
        assert!(!source.contains(concat!("Add Lark ", "channel")));
        // Results ride the window's top hint layer, never a strip in the page.
        assert!(source.contains("hint_layer::push"));
        assert!(!source.contains(concat!(".when_some(self.", "notice")));
        assert!(!source.contains(concat!(".when_some(self.", "error")));
    }

    /// Finds backend futures the page polls on GPUI's own executor.
    ///
    /// The IM service reconnects a channel with `tokio::spawn`, a reactor-backed
    /// boot, so a backend future awaited by a `cx.spawn` body that is not inside
    /// `gpui_tokio::Tokio::spawn` panics with "there is no reactor running" —
    /// what made "set as default" take the app down. Only production code is
    /// scanned: the test module quotes the shapes this looks for.
    fn backend_awaited_on_the_gpui_executor() -> Vec<String> {
        let source = include_str!("im_channels.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or_default();
        let mut depth = 0usize;
        let mut tokio_depth: Option<usize> = None;
        let mut found = Vec::new();
        for line in source.lines() {
            let guarded = tokio_depth.is_some_and(|open| depth > open);
            if !guarded && line.contains("backend") && line.contains(".await") {
                found.push(line.trim().to_string());
            }
            if line.contains("gpui_tokio::Tokio::spawn") {
                tokio_depth = Some(depth);
            }
            depth += line.matches('{').count();
            depth = depth.saturating_sub(line.matches('}').count());
        }
        found
    }

    /// Every backend request the page makes leaves the GPUI executor.
    ///
    /// A Reactor-backed service call awaited on a foreground task panics the
    /// worker and takes the workbench with it, so the bridge is a contract
    /// rather than a style choice.
    #[test]
    fn backend_requests_run_on_the_tokio_runtime() {
        let offenders = backend_awaited_on_the_gpui_executor();
        assert!(
            offenders.is_empty(),
            "these lines poll a backend future on GPUI's executor, where the IM \
             service's `tokio::spawn` panics with 'there is no reactor running'; \
             hand the future to `gpui_tokio::Tokio::spawn` instead:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// The page body follows the settings page measure, so the master–detail
    /// stacks at the same widths the row-based pages do.
    #[test]
    fn the_work_area_stacks_on_a_narrow_page() {
        assert!(CHANNEL_LANE_MIN_PAGE_WIDTH < crate::app::settings_page_width(1280.0));
        assert!(crate::app::settings_page_width(640.0) < CHANNEL_LANE_MIN_PAGE_WIDTH);
    }

    /// Side by side, the two columns move on their own.
    ///
    /// The page fills the settings viewport with a fixed title band above two
    /// independent scroll regions: one channel's configuration can be read
    /// while the list of channels stays in place, and a long list never takes
    /// the work area with it. Each region tracks its own handle and pins its
    /// own bar, so neither bar can drift with the rows.
    #[gpui::test]
    fn the_two_columns_scroll_independently(cx: &mut TestAppContext) {
        struct LaneProbe {
            view: Entity<ImChannelsView>,
            // The page fills this viewport, so it never has anything of its
            // own to scroll; tracked to prove that.
            viewport_scroll: ScrollHandle,
        }

        impl Render for LaneProbe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                // The same nesting the settings dialog gives a page: a
                // definite-height viewport whose child fills it. The page's own
                // `h_full` is what turns the two columns into bounded scroll
                // regions instead of content-sized blocks.
                div()
                    .id("im-channels-test-viewport")
                    .debug_selector(|| "im-channels-test-viewport".to_string())
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.viewport_scroll)
                    .child(self.view.clone())
            }
        }

        cx.update(gpui_component::init);
        cx.update(gpui_tokio::init);
        cx.update(crate::hint_layer::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        // The page only reads the workbench for its backend, and this layout
        // test never asks it for one: an invalid handle keeps the real
        // workbench — and its runtime lock — out of the test entirely.
        // `Root` is the kit's own root: the page pushes its results onto the
        // window's hint layer, which only exists under it.
        let probe_slot: Rc<RefCell<Option<Entity<LaneProbe>>>> = Rc::new(RefCell::new(None));
        let slot = probe_slot.clone();
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| ImChannelsView::new(WeakEntity::new_invalid(), window, cx));
            let probe = cx.new(|_| LaneProbe {
                view,
                viewport_scroll: ScrollHandle::new(),
            });
            *slot.borrow_mut() = Some(probe.clone());
            gpui_component::Root::new(probe, window, cx)
        });
        let probe = probe_slot
            .borrow()
            .clone()
            .expect("the rooted window built its page");
        cx.run_until_parked();
        // A page with no channels keeps the resting empty state, which has
        // nothing to scroll; the split layout is the state a runtime with
        // channels reaches.
        let channels = (0..24)
            .map(|index| ImChannelSummary {
                channel_id: ImChannelId::new(),
                display_name: format!("Channel {index}"),
                kind: ImChannelKind::Telegram,
                enabled: true,
                configured: true,
                default_session: None,
                event_filter: Vec::new(),
                locale: None,
                status: Default::default(),
                created_at_ms: 0,
                updated_at_ms: 0,
            })
            .collect::<Vec<_>>();
        probe.update(cx, |probe, cx| {
            probe.view.update(cx, |view, cx| {
                view.channels = channels;
                view.selected = view
                    .channels
                    .first()
                    .map(|channel| channel.channel_id.clone());
                cx.notify();
            });
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (lane_scroll, detail_scroll, viewport_scroll) = probe.read_with(cx, |probe, cx| {
            probe.view.read_with(cx, |view, _| {
                (
                    view.lane_scroll.clone(),
                    view.detail_scroll.clone(),
                    probe.viewport_scroll.clone(),
                )
            })
        });
        let lane_area = cx
            .debug_bounds("im-channel-lane-scroll")
            .expect("the lane should own a scroll viewport");
        let detail_area = cx
            .debug_bounds("im-channel-work-area-scroll")
            .expect("the work area should own a scroll viewport");
        assert!(
            lane_area.size.height > px(120.0),
            "the lane needs a real viewport to scroll in, got {:?}",
            lane_area.size
        );
        assert!(
            detail_area.size.height > px(120.0),
            "the work area needs a real viewport to scroll in, got {:?}",
            detail_area.size
        );
        let page_area = cx
            .debug_bounds("im-channels-page")
            .expect("the page should be laid out");
        let viewport_area = cx
            .debug_bounds("im-channels-test-viewport")
            .expect("the probe viewport should be laid out");
        // The page fills the viewport rather than growing with its content:
        // that is the definite height the two column viewports divide between
        // them, and the reason the outer page has nothing left to scroll.
        assert_eq!(
            page_area.size.height, viewport_area.size.height,
            "the page should fill the settings viewport"
        );
        // The page's first column sits at its own tighter inset and the work
        // area starts one gap after it: the lane is pulled toward the dialog
        // edge instead of the wider page inset the row-based pages use.
        assert_eq!(
            lane_area.left() - page_area.left(),
            px(CHANNEL_PAGE_LEADING_INSET),
            "the lane should start at the page's tighter inset"
        );
        assert_eq!(
            detail_area.left() - lane_area.right(),
            px(CHANNEL_COLUMN_GAP),
            "the work area should follow the lane after one gutter"
        );

        let header_area = cx
            .debug_bounds("im-channels-header")
            .expect("the page title should be laid out");
        cx.simulate_event(ScrollEvent {
            position: lane_area.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-80.0))),
            modifiers: Default::default(),
            touch_phase: gpui::TouchPhase::Moved,
            ..Default::default()
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            lane_scroll.offset().y < px(0.0),
            "the lane should follow the wheel over its rows: offset {:?}",
            lane_scroll.offset()
        );
        assert_eq!(
            detail_scroll.offset().y,
            px(0.0),
            "moving the lane must leave the work area where it was"
        );
        assert_eq!(
            cx.debug_bounds("im-channels-header"),
            Some(header_area),
            "the page title stays put while a column scrolls"
        );

        cx.simulate_event(ScrollEvent {
            position: point(detail_area.center().x, detail_area.top() + px(40.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-80.0))),
            modifiers: Default::default(),
            touch_phase: gpui::TouchPhase::Moved,
            ..Default::default()
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            detail_scroll.offset().y < px(0.0),
            "the work area should follow the wheel over its own content: offset {:?}",
            detail_scroll.offset()
        );
        // Neither column's wheel moved the page itself: the title is fixed and
        // the two regions are the only things that travel.
        assert_eq!(
            viewport_scroll.offset().y,
            px(0.0),
            "the settings viewport must not scroll the page as well"
        );
    }
}
