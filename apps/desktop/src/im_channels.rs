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
    ImChannelSummary, ImLarkConfig, ImLarkRegion, ImLarkTransport, ImPairingCodeInfo,
    ImPairingRequest, ImPrincipalId, ImPrincipalRole, ImPrincipalSummary, ImSessionDefaults,
    ImTelegramConfig,
};

use crate::app::VibexWorkbench;
use crate::locale;

pub struct ImChannelsView {
    workbench: WeakEntity<VibexWorkbench>,
    name_input: Entity<InputState>,
    token_input: Entity<InputState>,
    chat_input: Entity<InputState>,
    lark_app_id_input: Entity<InputState>,
    lark_app_secret_input: Entity<InputState>,
    lark_chat_input: Entity<InputState>,
    telegram_topic_mode: bool,
    /// The role a newly minted pairing code grants.
    pairing_role: ImPrincipalRole,
    /// Which Lark deployment the app was created in.
    lark_region: ImLarkRegion,
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
            pairing_role: ImPrincipalRole::Operator,
            lark_region: ImLarkRegion::Feishu,
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
        let region = self.lark_region;
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
        // Enabling the channel connects the adapter, and the adapter spawns its
        // long connection onto the Tokio runtime. The work has to be started
        // there; polling it on the UI executor panics with "there is no reactor
        // running".
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let display_name = if chat_id.trim().is_empty() {
                "Lark bot".to_string()
            } else {
                chat_id.trim().to_string()
            };
            let draft = ImChannelDraft {
                display_name,
                config: ImChannelConfig::Lark(ImLarkConfig {
                    app_id: app_id.trim().to_string(),
                    chat_id: (!chat_id.trim().is_empty()).then(|| chat_id.trim().to_string()),
                    transport: ImLarkTransport::LongConnection,
                    region,
                }),
                default_session: None,
                event_filter: Vec::new(),
                locale: None,
            };
            async {
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
            .await
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
            let created = backend
                .management()
                .create_im_channel(vibex_backend::MutationRequest::new(draft))
                .await
                .map_err(backend_error);
            match created {
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
            }
        });
        self.task = Some(cx.spawn(async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            let result = match runner.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
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
        let role = self.pairing_role;
        self.task = Some(cx.spawn(async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            let result = backend
                .management()
                .create_im_pairing_code(vibex_backend::MutationRequest::new(ImPairingRequest {
                    channel_id,
                    role,
                    ttl_ms: Some(600_000),
                    scope: vibex_core::ImPairingScope::Direct,
                    target_chat_id: None,
                }))
                .await
                .map_err(backend_error);
            let _ = weak.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(pairing) => {
                        this.notice = Some(
                            locale::text(
                                "Pairing code created. Send /pair <code> in a direct message within 10 minutes.",
                                "配对码已生成。请在 10 分钟内私聊发送 /pair <配对码>。",
                                "配對碼已產生。請在 10 分鐘內私聊傳送 /pair <配對碼>。",
                            )
                            .to_string(),
                        );
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

    /// Adds or removes one event id from the channel's forwarded set.
    fn toggle_event_filter(
        &mut self,
        channel: ImChannelSummary,
        event_id: &'static str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        let mut filter = channel.event_filter.clone();
        if enabled {
            if !filter.iter().any(|id| id == event_id) {
                filter.push(event_id.to_string());
            }
        } else {
            filter.retain(|id| id != event_id);
        }
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .set_im_channel_event_filter(vibex_backend::MutationRequest::new(
                        vibex_backend::ImChannelEventFilterUpdate {
                            channel_id: channel.channel_id.clone(),
                            event_filter: filter,
                        },
                    ))
                    .await
                    .map(|_| ())
                    .map_err(backend_error);
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
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

    fn set_channel_locale(
        &mut self,
        channel_id: ImChannelId,
        locale: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .set_im_channel_locale(vibex_backend::MutationRequest::new(
                        vibex_backend::ImChannelLocaleUpdate { channel_id, locale },
                    ))
                    .await
                    .map(|_| ())
                    .map_err(backend_error);
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
                            this.notice = Some("Channel language updated.".to_string());
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

    fn set_principal_role(
        &mut self,
        principal_id: ImPrincipalId,
        role: ImPrincipalRole,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .set_im_principal_role(vibex_backend::MutationRequest::new(
                        vibex_backend::ImPrincipalRoleUpdate { principal_id, role },
                    ))
                    .await
                    .map(|_| ())
                    .map_err(backend_error);
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
                            this.notice = Some("Role updated.".to_string());
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

    fn revoke_binding(&mut self, binding_id: vibex_core::ImBindingId, cx: &mut Context<Self>) {
        let Some(backend) = self.backend(cx) else {
            return;
        };
        self.task = Some(cx.spawn(
            async move |weak: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let result = backend
                    .management()
                    .revoke_im_binding(vibex_backend::MutationRequest::new(binding_id))
                    .await
                    .map_err(backend_error);
                let _ = weak.update(cx, |this, cx| {
                    match result {
                        Ok(()) => {
                            this.notice = Some("Binding removed.".to_string());
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

    fn role_label(role: ImPrincipalRole) -> &'static str {
        match role {
            ImPrincipalRole::Observer => locale::text("Observer", "只读", "唯讀"),
            ImPrincipalRole::Approver => locale::text("Approver", "审批", "審批"),
            ImPrincipalRole::Operator => locale::text("Operator", "操作员", "操作員"),
            ImPrincipalRole::Admin => locale::text("Admin", "管理员", "管理員"),
        }
    }

    fn next_role(role: ImPrincipalRole) -> ImPrincipalRole {
        match role {
            ImPrincipalRole::Observer => ImPrincipalRole::Approver,
            ImPrincipalRole::Approver => ImPrincipalRole::Operator,
            ImPrincipalRole::Operator => ImPrincipalRole::Admin,
            ImPrincipalRole::Admin => ImPrincipalRole::Observer,
        }
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
                    .child(locale::text("Add Telegram channel", "添加 Telegram 频道", "新增 Telegram 頻道")),
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
                            .child(locale::text(
                                "Forum topic mode: create one topic per /new session. The bot must be a forum admin.",
                                "话题模式：每个 /new 会话创建一个话题。机器人必须是论坛管理员。",
                                "話題模式：每個 /new 工作階段建立一個話題。機器人必須是論壇管理員。",
                            )),
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
                            .child(locale::text(
                                "The bot token is stored in the OS keychain (or the runtime's host secret file).",
                                "Bot token 存在操作系统钥匙串（或运行时的主机密钥文件）中。",
                                "Bot token 存在作業系統鑰匙圈（或執行時的主機密鑰檔案）中。",
                            )),
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
                    .child(locale::text("Add Lark / Feishu channel", "添加 Lark / 飞书频道", "新增 Lark / 飛書頻道")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Input::new(&self.lark_app_id_input).small().w_full())
                    .child(Input::new(&self.lark_chat_input).small().w_full()),
            )
            .child(Input::new(&self.lark_app_secret_input).small().w_full())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(locale::text(
                                "Deployment (the app must exist there)",
                                "部署区域（应用必须在该区域注册）",
                                "部署區域（應用必須在該區域註冊）",
                            )),
                    )
                    .child(
                        Button::new("im-lark-region-feishu")
                            .small()
                            .outline()
                            .label(locale::text("飞书 中国", "飞书 中国", "飛書 中國"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.lark_region = ImLarkRegion::Feishu;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("im-lark-region-lark")
                            .small()
                            .outline()
                            .label(locale::text("Lark 国际", "Lark 国际", "Lark 國際"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.lark_region = ImLarkRegion::Lark;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(match self.lark_region {
                                ImLarkRegion::Feishu => {
                                    locale::text("open.feishu.cn", "open.feishu.cn", "open.feishu.cn")
                                }
                                ImLarkRegion::Lark => locale::text(
                                    "open.larksuite.com",
                                    "open.larksuite.com",
                                    "open.larksuite.com",
                                ),
                            }),
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
                            .child(locale::text(
                                "The app secret is stored in the OS keychain (or the runtime's host secret file). Subscribe the app to card.action.trigger so approval cards can be answered with a button.",
                                "App secret 存在操作系统钥匙串（或运行时的主机密钥文件）中。请为应用订阅 card.action.trigger，这样审批卡片就能用按钮回答。",
                                "App secret 存在作業系統鑰匙圈（或執行時的主機密鑰檔案）中。請為應用訂閱 card.action.trigger，這樣審批卡片就能用按鈕回答。",
                            )),
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
                            .label(locale::text("Open", "打开", "開啟"))
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
                                            .label(locale::text("Test connection", "测试连接", "測試連線"))
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
                                            .label(locale::text("Delete", "删除", "刪除"))
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
                                Button::new("im-pairing-role")
                                    .small()
                                    .outline()
                                    .label(format!("Role: {}", Self::role_label(self.pairing_role)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.pairing_role = Self::next_role(this.pairing_role);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("im-channel-pair")
                                    .small()
                                    .primary()
                                    .label(locale::text("Generate pairing code", "生成配对码", "產生配對碼"))
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
                            .child(locale::text(
                        "Send /pair <code> to the bot in a direct message. The code expires in 10 minutes.",
                        "在私聊中向机器人发送 /pair <配对码>。配对码 10 分钟后过期。",
                        "在私聊中向機器人傳送 /pair <配對碼>。配對碼 10 分鐘後過期。",
                    )),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(foreground)
                            .child(format!(
                                "{} ({})",
                                locale::text("Paired identities", "已配对身份", "已配對身分"),
                                self.principals.len()
                            )),
                    );
                if self.principals.is_empty() {
                    panel = panel.child(div().text_xs().text_color(muted).child(locale::text(
                        "No identities have paired with this channel yet.",
                        "还没有身份配对这个频道。",
                        "還沒有身分配對這個頻道。",
                    )));
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
                                        "im-principal-role-{}",
                                        principal.principal_id.as_str()
                                    ))
                                    .small()
                                    .outline()
                                    .label(Self::role_label(principal.role))
                                    .on_click(cx.listener({
                                        let principal_id = principal_id.clone();
                                        let next = Self::next_role(principal.role);
                                        move |this, _, _, cx| {
                                            this.set_principal_role(principal_id.clone(), next, cx)
                                        }
                                    })),
                                )
                                .child(
                                    Button::new(format!(
                                        "im-principal-revoke-{}",
                                        principal.principal_id.as_str()
                                    ))
                                    .small()
                                    .ghost()
                                    .label(locale::text("Revoke", "撤销", "撤銷"))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.revoke_principal(principal_id.clone(), cx)
                                        },
                                    )),
                                ),
                        );
                    }
                }
                panel = panel.child(div().text_sm().text_color(foreground).child(format!(
                    "{} ({})",
                    locale::text("Session bindings", "会话绑定", "工作階段綁定"),
                    self.bindings.len()
                )));
                if self.bindings.is_empty() {
                    panel = panel.child(div().text_xs().text_color(muted).child(locale::text(
                        "No chat is bound to a session yet. Use /use in the chat, or /new.",
                        "还没有聊天绑定到会话。可在聊天里用 /use，或用 /new。",
                        "還沒有聊天綁定到工作階段。可在聊天裡用 /use，或用 /new。",
                    )));
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
                        panel = panel.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .child(div().text_xs().text_color(foreground).child(title))
                                        .child(div().text_xs().text_color(muted).child({
                                            let thread = if binding.thread_key.is_empty() {
                                                String::new()
                                            } else {
                                                format!(" · thread {}", binding.thread_key)
                                            };
                                            format!(
                                                "chat {} · {:?}{thread}",
                                                binding.external_chat_id, binding.status
                                            )
                                        })),
                                )
                                .child(
                                    Button::new(format!(
                                        "im-binding-revoke-{}",
                                        binding.binding_id.as_str()
                                    ))
                                    .small()
                                    .ghost()
                                    .label(locale::text("Unbind", "解绑", "解除綁定"))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.revoke_binding(binding_id.clone(), cx)
                                        },
                                    )),
                                ),
                        );
                    }
                }
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
                                        .child(locale::text("Default session for /new", "/new 的默认会话", "/new 的預設工作階段")),
                                )
                                .child(div().text_xs().text_color(muted).child(
                                    if channel.default_session.is_some() {
                                        "Configured. /new can start without a bound session."
                                    } else {
                                        locale::text(
                                        "Pick a recent session to reuse its workspace and Agent.",
                                        "选一个最近的会话，复用它当时的工作区和 Agent。",
                                        "選一個最近的工作階段，重複使用它當時的工作區和 Agent。",
                                    )
                                    },
                                )),
                        )
                        .when(channel.default_session.is_some(), |this| {
                            this.child(
                                Button::new("im-default-clear")
                                    .small()
                                    .ghost()
                                    .label(locale::text("Clear", "清除", "清除"))
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
                                    .label(locale::text("Use as default", "设为默认", "設為預設"))
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

                // Events this channel forwards. The list is the full known set,
                // with the channel's current selection checked.
                panel = panel.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(foreground)
                                .child(locale::text("Events sent to this chat", "发送到这个聊天的活动", "傳送到這個聊天的活動")),
                        )
                        .child(div().text_xs().text_color(muted).child(
                            "Approvals and errors are the default set. Turning one off stops it for every binding on this channel.",
                        )),
                );
                for event_id in vibex_core::IM_EVENT_FILTER_IDS {
                    let enabled = channel.event_filter.iter().any(|id| id == event_id);
                    let channel_for_toggle = channel.clone();
                    panel = panel.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(foreground)
                                    .child(*event_id),
                            )
                            .child(
                                Switch::new(format!(
                                    "im-event-{}-{}",
                                    channel_id.as_str(),
                                    event_id
                                ))
                                .small()
                                .checked(enabled)
                                .on_click(cx.listener(
                                    move |this, checked, _, cx| {
                                        this.toggle_event_filter(
                                            channel_for_toggle.clone(),
                                            event_id,
                                            *checked,
                                            cx,
                                        )
                                    },
                                )),
                            ),
                    );
                }

                // Language for this channel's replies.
                panel = panel.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(
                            v_flex()
                                .child(div().text_sm().text_color(foreground).child(locale::text(
                                    "Reply language",
                                    "回复语言",
                                    "回覆語言",
                                )))
                                .child(div().text_xs().text_color(muted).child(format!(
                                    "{} (host default when unset)",
                                    channel
                                        .locale
                                        .clone()
                                        .unwrap_or_else(|| "Not set".to_string())
                                ))),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new(format!("im-locale-en-{}", channel_id.as_str()))
                                        .small()
                                        .ghost()
                                        .label("English")
                                        .on_click(cx.listener({
                                            let channel_id = channel_id.clone();
                                            move |this, _, _, cx| {
                                                this.set_channel_locale(
                                                    channel_id.clone(),
                                                    Some("en".to_string()),
                                                    cx,
                                                )
                                            }
                                        })),
                                )
                                .child(
                                    Button::new(format!("im-locale-zh-{}", channel_id.as_str()))
                                        .small()
                                        .ghost()
                                        .label("中文")
                                        .on_click(cx.listener({
                                            let channel_id = channel_id.clone();
                                            move |this, _, _, cx| {
                                                this.set_channel_locale(
                                                    channel_id.clone(),
                                                    Some("zh-CN".to_string()),
                                                    cx,
                                                )
                                            }
                                        })),
                                )
                                .child(
                                    Button::new(format!("im-locale-auto-{}", channel_id.as_str()))
                                        .small()
                                        .ghost()
                                        .label(locale::text("Host", "跟随系统", "跟隨系統"))
                                        .on_click(cx.listener({
                                            let channel_id = channel_id.clone();
                                            move |this, _, _, cx| {
                                                this.set_channel_locale(
                                                    channel_id.clone(),
                                                    None,
                                                    cx,
                                                )
                                            }
                                        })),
                                ),
                        ),
                );
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
                                    .child(locale::text("IM channels", "IM 频道", "IM 頻道")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(locale::text(
                                "Chat with Vibex and approve tool calls from Telegram.",
                                "通过 IM 与 Vibex 对话并批准工具调用。",
                                "透過 IM 與 Vibex 對話並批准工具呼叫。",
                            )),
                            ),
                    )
                    .child(
                        Button::new("im-channels-refresh")
                            .small()
                            .ghost()
                            .label(locale::text("Refresh", "刷新", "重新整理"))
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
                    .child(locale::text(
                        "Telegram and Lark are supported. Approvals use the buttons on the card, or the printed short code. Images and documents up to 10 MB are accepted; long answers are split across messages.",
                        "支持 Telegram 和 Lark。审批可通过卡片按钮或卡片上的短码完成。接受不超过 10 MB 的图片和文档；超长回答会自动分段发送。",
                        "支援 Telegram 和 Lark。審批可透過卡片按鈕或卡片上的短碼完成。接受不超過 10 MB 的圖片和文件；超長回覆會自動分段傳送。",
                    )),
            )
    }
}

fn backend_error(error: BackendError) -> String {
    format!("{}: {}", error.code, error.message)
}
