//! IM channel settings page.
//!
//! The page owns its async state instead of mirroring it on the workbench: a
//! channel create, test, or pairing-code request is a network round trip, and
//! rebuilding the whole settings tree while it runs would lose focus in the
//! form. The workbench only supplies the backend facade.

use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    switch::Switch,
    v_flex,
};
use vibex_backend::BackendError;
use vibex_core::{
    AgentSession, ImBindingSummary, ImChannelConfig, ImChannelDraft, ImChannelId, ImChannelKind,
    ImChannelSummary, ImLarkConfig, ImLarkTransport, ImPairingCodeInfo, ImPairingRequest,
    ImPrincipalId, ImPrincipalRole, ImPrincipalSummary, ImSessionDefaults, ImTelegramConfig,
};

use crate::app::VibexWorkbench;

pub struct ImChannelsView {
    workbench: WeakEntity<VibexWorkbench>,
    name_input: Entity<InputState>,
    token_input: Entity<InputState>,
    chat_input: Entity<InputState>,
    lark_app_id_input: Entity<InputState>,
    lark_app_secret_input: Entity<InputState>,
    lark_chat_input: Entity<InputState>,
    telegram_topic_mode: bool,
    channels: Vec<ImChannelSummary>,
    selected: Option<ImChannelId>,
    principals: Vec<ImPrincipalSummary>,
    bindings: Vec<ImBindingSummary>,
    default_sessions: Vec<AgentSession>,
    pairing: Option<ImPairingCodeInfo>,
    notice: Option<String>,
    error: Option<String>,
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
        let lark_app_id_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("cli_xxxxxxxxx"));
        let lark_app_secret_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Lark app secret"));
        let lark_chat_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("oc_xxxxxxxxx (optional)"));
        Self {
            workbench,
            name_input,
            token_input,
            chat_input,
            lark_app_id_input,
            lark_app_secret_input,
            lark_chat_input,
            telegram_topic_mode: false,
            channels: Vec::new(),
            selected: None,
            principals: Vec::new(),
            bindings: Vec::new(),
            default_sessions: Vec::new(),
            pairing: None,
            notice: None,
            error: None,
            loading: false,
            loaded: false,
            busy: false,
            task: None,
        }
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

    fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let Some(backend) = self.backend(cx) else {
            self.loading = false;
            self.loaded = true;
            self.error = Some("The runtime has no active backend.".to_string());
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
                            this.error = None;
                        }
                        Ok(Err(error)) => this.error = Some(error),
                        Err(error) => this.error = Some(error.to_string()),
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
                    match principals {
                        Ok(Ok(principals)) => this.principals = principals,
                        Ok(Err(error)) => this.error = Some(error),
                        Err(error) => this.error = Some(error.to_string()),
                    }
                    match bindings {
                        Ok(Ok(bindings)) => this.bindings = bindings,
                        Ok(Err(error)) => this.error = Some(error),
                        Err(error) => this.error = Some(error.to_string()),
                    }
                    match sessions {
                        Ok(Ok(sessions)) => this.default_sessions = sessions,
                        Ok(Err(error)) => this.error = Some(error),
                        Err(error) => this.error = Some(error.to_string()),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn create_lark_channel(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let app_id = self.lark_app_id_input.read(cx).value().to_string();
        let app_secret = self.lark_app_secret_input.read(cx).value().to_string();
        let chat_id = self.lark_chat_input.read(cx).value().to_string();
        if app_id.trim().is_empty() || app_secret.trim().is_empty() {
            self.error = Some("Lark app id and app secret are required.".to_string());
            cx.notify();
            return;
        }
        let Some(backend) = self.backend(cx) else {
            self.error = Some("The runtime has no active backend.".to_string());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let display_name = if chat_id.trim().is_empty() {
                    "Lark bot".to_string()
                } else {
                    chat_id.trim().to_string()
                };
                let draft = ImChannelDraft {
                    display_name,
                    config: ImChannelConfig::Lark(ImLarkConfig {
                        app_id: app_id.trim().to_string(),
                        chat_id: (!chat_id.trim().is_empty())
                            .then(|| chat_id.trim().to_string()),
                        transport: ImLarkTransport::LongConnection,
                    }),
                    default_session: None,
                    event_filter: Vec::new(),
                    locale: None,
                };
                let result = async {
                    let channel = backend
                        .management()
                        .create_im_channel(vibex_backend::MutationRequest::new(draft))
                        .await
                        .map_err(backend_error)?;
                    backend
                        .management()
                        .set_im_channel_app_secret(vibex_backend::MutationRequest::new(
                            vibex_backend::ImChannelAppSecretUpdateRequest {
                                channel_id: channel.channel_id.clone(),
                                app_secret: app_secret.trim().to_string(),
                            },
                        ))
                        .await
                        .map_err(backend_error)?;
                    backend
                        .management()
                        .set_im_channel_enabled(vibex_backend::MutationRequest::new(
                            vibex_backend::ImChannelEnabledRequest {
                                channel_id: channel.channel_id,
                                enabled: true,
                            },
                        ))
                        .await
                        .map(|_| ())
                        .map_err(backend_error)
                }
                .await;
                let _ = weak.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(()) => {
                            this.notice = Some("Lark channel created. Generate a pairing code, then send /pair <code> in a direct message.".to_string());
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.error = Some(error),
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
            self.error = Some("Name, bot token, and chat id are required.".to_string());
            cx.notify();
            return;
        }
        let Some(backend) = self.backend(cx) else {
            self.error = Some("The runtime has no active backend.".to_string());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.task = Some(cx.spawn(async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
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
            let created = backend
                .management()
                .create_im_channel(vibex_backend::MutationRequest::new(draft))
                .await
                .map_err(backend_error);
            let result = match created {
                Ok(channel) => {
                    let token_result = backend
                        .management()
                        .set_im_channel_bot_token(vibex_backend::MutationRequest::new(
                            vibex_backend::ImChannelSecretUpdateRequest {
                                channel_id: channel.channel_id.clone(),
                                bot_token: token.trim().to_string(),
                            },
                        ))
                        .await
                        .map_err(backend_error);
                    match token_result {
                        Ok(()) => backend
                            .management()
                            .set_im_channel_enabled(vibex_backend::MutationRequest::new(
                                vibex_backend::ImChannelEnabledRequest {
                                    channel_id: channel.channel_id,
                                    enabled: true,
                                },
                            ))
                            .await
                            .map(|_| ())
                            .map_err(backend_error),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            };
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.notice = Some("Telegram channel created. Generate a pairing code, then send /pair <code> in a direct message.".to_string());
                        this.loaded = false;
                        this.loading = false;
                        this.reload(cx);
                    }
                    Err(error) => this.error = Some(error),
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
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .set_im_channel_enabled(vibex_backend::MutationRequest::new(
                        vibex_backend::ImChannelEnabledRequest {
                            channel_id,
                            enabled,
                        },
                    ))
                    .await
                    .map_err(backend_error);
                let _ = weak.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(_) => {
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.error = Some(error),
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
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .delete_im_channel(vibex_backend::MutationRequest::new(channel_id))
                    .await
                    .map_err(backend_error);
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
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn test_channel(&mut self, channel_id: ImChannelId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.busy = true;
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .test_im_channel(channel_id)
                    .await
                    .map_err(backend_error);
                let _ = weak.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(report) => {
                            this.notice = Some(format!(
                                "Connected to {}{}.",
                                report.bot_name.unwrap_or_else(|| "Telegram".to_string()),
                                report
                                    .chat_title
                                    .map(|title| format!(" in {title}"))
                                    .unwrap_or_default()
                            ));
                            this.error = None;
                        }
                        Err(error) => this.error = Some(error),
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
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = async {
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
                }
                .await;
                let _ = weak.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Ok(()) => {
                            this.notice = Some(format!(
                                "Default session set from “{}” for /new.",
                                session.title
                            ));
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.error = Some(error),
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
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .set_im_channel_default_session(vibex_backend::MutationRequest::new(
                        vibex_backend::ImChannelDefaultSessionUpdate {
                            channel_id,
                            default_session: None,
                        },
                    ))
                    .await
                    .map(|_| ())
                    .map_err(backend_error);
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
                            this.notice = Some("Default session cleared.".to_string());
                            this.loaded = false;
                            this.loading = false;
                            this.reload(cx);
                        }
                        Err(error) => this.error = Some(error),
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
        self.task = Some(cx.spawn(async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            let result = backend
                .management()
                .create_im_pairing_code(vibex_backend::MutationRequest::new(ImPairingRequest {
                    channel_id,
                    role: ImPrincipalRole::Operator,
                    ttl_ms: Some(600_000),
                }))
                .await
                .map_err(backend_error);
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(pairing) => {
                        this.notice = Some("Pairing code created. Send /pair <code> in a direct message within 10 minutes.".to_string());
                        this.pairing = Some(pairing);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn revoke_principal(&mut self, principal_id: ImPrincipalId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .revoke_im_principal(vibex_backend::MutationRequest::new(principal_id))
                    .await
                    .map_err(backend_error);
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => this.reload_selected(cx),
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn select_channel(&mut self, channel_id: ImChannelId, cx: &mut Context<Self>) {
        self.selected = Some(channel_id);
        self.pairing = None;
        self.reload_selected(cx);
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
            .size(px(8.0))
            .rounded_full()
            .bg(color)
            .into_any_element()
    }

    fn status_label(&self, channel: &ImChannelSummary) -> &'static str {
        match channel.status.state {
            vibex_core::ImChannelState::Connected => "Connected",
            vibex_core::ImChannelState::Starting => "Starting",
            vibex_core::ImChannelState::Degraded => "Needs attention",
            vibex_core::ImChannelState::Stopped => "Stopped",
            vibex_core::ImChannelState::Disabled => "Disabled",
        }
    }
}

impl Render for ImChannelsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.loaded && !self.loading {
            self.reload(cx);
        }
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let surface = cx.theme().secondary;

        let add_form = v_flex()
            .gap_2()
            .p_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(border)
            .bg(surface)
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(foreground)
                    .child("Add Telegram channel"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.name_input).small().w_full())
                    .child(Input::new(&self.chat_input).small().w_full()),
            )
            .child(Input::new(&self.token_input).small().w_full())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
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
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Forum topic mode: create one topic per /new session. The bot must be a forum admin."),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("The bot token is stored in the OS keychain (or the runtime's host secret file)."),
                    )
                    .child(
                        Button::new("im-channel-create")
                            .small()
                            .primary()
                            .label(if self.busy { "Working…" } else { "Add channel" })
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.create_channel(cx))),
                    ),
            );

        let add_lark_form = v_flex()
            .gap_2()
            .p_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(border)
            .bg(surface)
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(foreground)
                    .child("Add Lark / Feishu channel"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.lark_app_id_input).small().w_full())
                    .child(Input::new(&self.lark_chat_input).small().w_full()),
            )
            .child(Input::new(&self.lark_app_secret_input).small().w_full())
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("The app secret is stored in the OS keychain (or the runtime's host secret file). Subscribe the app to card.action.trigger so approval cards can be answered with a button."),
                    )
                    .child(
                        Button::new("im-lark-create")
                            .small()
                            .primary()
                            .label(if self.busy { "Working…" } else { "Add Lark channel" })
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.create_lark_channel(cx))),
                    ),
            );

        let list = if self.channels.is_empty() {
            div()
                .text_sm()
                .text_color(muted)
                .child(if self.loading {
                    "Loading channels…"
                } else {
                    "No IM channels yet. Add a Telegram bot or Lark app above."
                })
                .into_any_element()
        } else {
            v_flex()
                .gap_1()
                .children(self.channels.iter().map(|channel| {
                    let channel_id = channel.channel_id.clone();
                    let selected = self.selected.as_ref() == Some(&channel.channel_id);
                    h_flex()
                        .gap_2()
                        .items_center()
                        .p_2()
                        .rounded(px(6.0))
                        .when(selected, |this| this.bg(surface))
                        .child(self.status_dot(channel, cx))
                        .child(
                            v_flex()
                                .flex_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(foreground)
                                        .child(channel.display_name.clone()),
                                )
                                .child(div().text_xs().text_color(muted).child(format!(
                                    "{} · {} · {}",
                                    self.channel_kind_label(channel.kind),
                                    self.status_label(channel),
                                    if channel.configured {
                                        "secret configured"
                                    } else {
                                        "secret missing"
                                    }
                                ))),
                        )
                        .child(
                            Switch::new(format!(
                                "im-channel-enabled-{}",
                                channel.channel_id.as_str()
                            ))
                            .small()
                            .checked(channel.enabled)
                            .on_click(cx.listener({
                                let channel_id = channel_id.clone();
                                move |this, enabled, _, cx| {
                                    this.toggle_channel(channel_id.clone(), *enabled, cx);
                                }
                            })),
                        )
                        .child(
                            Button::new(format!(
                                "im-channel-select-{}",
                                channel.channel_id.as_str()
                            ))
                            .small()
                            .ghost()
                            .label("Open")
                            .on_click(cx.listener({
                                let channel_id = channel_id.clone();
                                move |this, _, _, cx| this.select_channel(channel_id.clone(), cx)
                            })),
                        )
                        .into_any_element()
                }))
                .into_any_element()
        };

        let selected_panel = if let Some(channel_id) = self.selected.clone() {
            let channel = self
                .channels
                .iter()
                .find(|channel| channel.channel_id == channel_id)
                .cloned();
            let mut panel = v_flex()
                .gap_2()
                .p_3()
                .rounded(px(8.0))
                .border_1()
                .border_color(border);
            if let Some(channel) = channel {
                panel = panel
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(foreground)
                                    .child(channel.display_name.clone()),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("im-channel-test")
                                            .small()
                                            .outline()
                                            .label("Test connection")
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
                                            .label("Delete")
                                            .disabled(self.busy)
                                            .on_click(cx.listener({
                                                let channel_id = channel_id.clone();
                                                move |this, _, _, cx| this.delete_channel(channel_id.clone(), cx)
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("im-channel-pair")
                                    .small()
                                    .primary()
                                    .label("Generate pairing code")
                                    .disabled(self.busy)
                                    .on_click(cx.listener({
                                        let channel_id = channel_id.clone();
                                        move |this, _, _, cx| this.create_pairing_code(channel_id.clone(), cx)
                                    })),
                            )
                            .when_some(self.pairing.as_ref(), |this, pairing| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .font_semibold()
                                        .text_color(foreground)
                                        .child(pairing.code.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Send /pair <code> to the bot in a direct message. The code expires in 10 minutes."),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(foreground)
                            .child(format!("Paired identities ({})", self.principals.len())),
                    );
                if self.principals.is_empty() {
                    panel = panel.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("No identities have paired with this channel yet."),
                    );
                } else {
                    for principal in &self.principals {
                        let principal_id = principal.principal_id.clone();
                        panel = panel.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(foreground)
                                                .child(principal.display_name.clone()),
                                        )
                                        .child(div().text_xs().text_color(muted).child(format!(
                                            "{:?} · {:?}",
                                            principal.role, principal.status
                                        ))),
                                )
                                .child(
                                    Button::new(format!(
                                        "im-principal-revoke-{}",
                                        principal.principal_id.as_str()
                                    ))
                                    .small()
                                    .ghost()
                                    .label("Revoke")
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.revoke_principal(principal_id.clone(), cx)
                                        },
                                    )),
                                ),
                        );
                    }
                }
                panel = panel.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Session bindings: {}", self.bindings.len())),
                );
                panel = panel.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(
                            v_flex()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(foreground)
                                        .child("Default session for /new"),
                                )
                                .child(div().text_xs().text_color(muted).child(
                                    if channel.default_session.is_some() {
                                        "Configured. /new can start without a bound session."
                                    } else {
                                        "Pick a recent session to reuse its workspace and Agent."
                                    },
                                )),
                        )
                        .when(channel.default_session.is_some(), |this| {
                            this.child(
                                Button::new("im-default-clear")
                                    .small()
                                    .ghost()
                                    .label("Clear")
                                    .on_click(cx.listener({
                                        let channel_id = channel_id.clone();
                                        move |this, _, _, cx| {
                                            this.clear_default_session(channel_id.clone(), cx)
                                        }
                                    })),
                            )
                        }),
                );
                for session in self.default_sessions.iter().take(5) {
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
                    panel = panel.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("#{short} · {}", session.title)),
                            )
                            .child(
                                Button::new(format!("im-default-use-{}", session.id.as_str()))
                                    .small()
                                    .outline()
                                    .label("Use as default")
                                    .disabled(self.busy)
                                    .on_click(cx.listener({
                                        let channel_id = channel_id.clone();
                                        move |this, _, _, cx| {
                                            this.set_default_session(
                                                channel_id.clone(),
                                                session.clone(),
                                                cx,
                                            )
                                        }
                                    })),
                            ),
                    );
                }
            }
            panel.into_any_element()
        } else {
            div().into_any_element()
        };

        v_flex()
            .id("im-channels-page")
            .gap_4()
            .p_4()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_lg()
                                    .font_semibold()
                                    .text_color(foreground)
                                    .child("IM channels"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("Chat with Vibex and approve tool calls from Telegram."),
                            ),
                    )
                    .child(
                        Button::new("im-channels-refresh")
                            .small()
                            .ghost()
                            .label("Refresh")
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .p_2()
                        .rounded(px(6.0))
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .p_2()
                        .rounded(px(6.0))
                        .bg(cx.theme().success.opacity(0.12))
                        .text_xs()
                        .text_color(cx.theme().success)
                        .child(notice),
                )
            })
            .child(add_form)
            .child(add_lark_form)
            .child(list)
            .child(selected_panel)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Telegram and Lark are supported. Approvals use the buttons on the card, or the printed short code. Images and documents up to 10 MB are accepted; long answers are split across messages."),
            )
    }
}

fn backend_error(error: BackendError) -> String {
    format!("{}: {}", error.code, error.message)
}
