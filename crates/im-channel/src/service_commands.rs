//! Command cards: buttons instead of remembered arguments.
//!
//! Every command keeps its text form, because a script or an old client still
//! has to work. The buttons are for people: picking a session, a model or an
//! effort should not require typing an id, and the short code printed under
//! each card is the fallback when a client cannot show inline keyboards.

use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentId, ImRuntimeChoice, ImRuntimeOptions, RuntimeAuthSource, RuntimeModelSelection,
    RuntimeSelectionInteraction, SetDesiredAgentSessionRuntimeRequest,
};

use super::*;

/// How long a command button stays valid. The approval cards carry their own
/// provider expiry; these are shorter-lived because a stale model list is worse
/// than an expired one.
const COMMAND_ACTION_TTL_MS: i64 = 15 * 60 * 1000;
/// The most choices one card offers, so the keyboard stays scrollable.
const MAX_CHOICES: usize = 20;
/// How many sessions the picker lists.
const SESSION_PICK_LIMIT: usize = 8;

pub(super) const ACTION_SESSION_USE: &str = "session:use";
pub(super) const ACTION_RUNTIME_PREFIX: &str = "runtime:";
pub(super) const ACTION_MENU_HELP: &str = "menu:help";
pub(super) const ACTION_MENU_SESSIONS: &str = "menu:sessions";
/// Opens a picker. Unlike `runtime:*` these carry no value, so a card button
/// that only asks "which model?" can never be mistaken for a choice.
pub(super) const ACTION_MENU_MODEL: &str = "menu:model";
pub(super) const ACTION_MENU_PROVIDER: &str = "menu:provider";
pub(super) const ACTION_MENU_EFFORT: &str = "menu:effort";
pub(super) const ACTION_MENU_PREFIX: &str = "menu:";
pub(super) const ACTION_STATUS_STOP: &str = "status:stop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeField {
    Model,
    Provider,
    Effort,
    Mode,
    Agent,
}

impl RuntimeField {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "provider" => Some(Self::Provider),
            "effort" => Some(Self::Effort),
            "mode" => Some(Self::Mode),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }

    fn action_kind(self) -> &'static str {
        match self {
            Self::Model => "runtime:model",
            Self::Provider => "runtime:provider",
            Self::Effort => "runtime:effort",
            Self::Mode => "runtime:mode",
            Self::Agent => "runtime:agent",
        }
    }

    fn label(self, locale: ImLocale) -> &'static str {
        match (self, locale) {
            (Self::Model, ImLocale::ZhCn) => "模型",
            (Self::Model, ImLocale::ZhTw) => "模型",
            (Self::Model, _) => "Model",
            (Self::Provider, ImLocale::ZhCn) => "供应商",
            (Self::Provider, ImLocale::ZhTw) => "供應商",
            (Self::Provider, _) => "Provider",
            (Self::Effort, ImLocale::ZhCn) => "思考强度",
            (Self::Effort, ImLocale::ZhTw) => "思考強度",
            (Self::Effort, _) => "Reasoning effort",
            (Self::Mode, ImLocale::ZhCn) => "模式",
            (Self::Mode, ImLocale::ZhTw) => "模式",
            (Self::Mode, _) => "Mode",
            (Self::Agent, ImLocale::ZhCn) => "Agent",
            (Self::Agent, ImLocale::ZhTw) => "Agent",
            (Self::Agent, _) => "Agent",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ValuePayload {
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct AgentChoicePayload {
    pub agent_id: String,
    pub auth_source: RuntimeAuthSource,
    pub model: RuntimeModelSelection,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ProviderChoicePayload {
    pub auth_source: RuntimeAuthSource,
    pub model: RuntimeModelSelection,
}

impl ImChannelService {
    /// Records one one-shot button and returns the label suffix to print.
    pub(super) fn mint_action(
        &self,
        conn: &vibex_db::DbConnection,
        binding: &ImBindingRecord,
        kind: &str,
        payload: &str,
    ) -> VibexResult<ImButton> {
        let action_id = ImActionId::new();
        let short_code = action_short_code(&action_id);
        let now = unix_timestamp_ms();
        ImActionRepository::insert(
            conn,
            &ImActionRecord {
                action_id: action_id.clone(),
                short_code: short_code.clone(),
                card_id: None,
                channel_id: binding.channel_id.clone(),
                binding_id: binding.binding_id.clone(),
                kind: kind.to_string(),
                payload_json: payload.to_string(),
                expires_at_ms: now.saturating_add(COMMAND_ACTION_TTL_MS),
                consumed_at_ms: None,
                consumed_by_principal_id: None,
                created_at_ms: now,
            },
        )?;
        Ok(ImButton {
            action_id,
            label: short_code,
        })
    }

    pub(super) async fn reply_buttons(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        text: &str,
        buttons: Vec<ImButton>,
    ) -> VibexResult<()> {
        let target = ImTarget {
            chat_id: event.external_chat_id.clone(),
            thread_key: event.thread_key.clone(),
        };
        let message_id = self.record_outbound_reply(event, text);
        let result = runtime
            .outbox
            .send(
                target,
                text.to_string(),
                buttons,
                true,
                OutboundPriority::Normal,
            )
            .await;
        self.finish_outbound(&message_id, &result);
        result.map_err(im_error_to_vibex)?;
        Ok(())
    }

    pub(super) async fn send_help(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        full: bool,
    ) -> VibexResult<()> {
        let locale = self.locale(event);
        if full {
            return self.reply_card(event, runtime, &render::help(locale)).await;
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            return self
                .reply_card(event, runtime, &render::help_short(locale))
                .await;
        };
        let button = self.mint_action(&conn, &binding, ACTION_MENU_HELP, "{}")?;
        self.reply_buttons(
            event,
            runtime,
            &render::help_short(locale),
            vec![ImButton {
                action_id: button.action_id,
                label: locale
                    .text("All commands", "全部命令", "全部指令")
                    .to_string(),
            }],
        )
        .await
    }

    /// The session picker: one button per session instead of `/use 3`.
    pub(super) async fn send_sessions_card(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<()> {
        let locale = self.locale(event);
        if !principal.role.can_prompt() {
            self.reply(
                event,
                runtime,
                locale.text(
                    "Your role cannot bind sessions.",
                    "你的角色不能绑定会话。",
                    "你的角色不能綁定工作階段。",
                ),
            )
            .await?;
            return Ok(());
        }
        let mut sessions = self.inner.agent.list_sessions(false).await?;
        sessions.retain(|session| session.deleted_at_ms.is_none());
        sessions.sort_by_key(|session| std::cmp::Reverse(session.last_message_at_ms));
        sessions.truncate(SESSION_PICK_LIMIT);
        // `/use <n>` still needs the same order the card showed.
        if let Ok(mut choices) = self.inner.session_choices.lock() {
            choices.insert(
                principal.principal_id.clone(),
                sessions.iter().map(|session| session.id.clone()).collect(),
            );
        }
        let list = render::session_list(&sessions, locale);
        let conn = self.open()?;
        // Listing is not binding: a chat that has not chosen a session yet can
        // still see what is there, it just has no card to mint buttons under.
        let Some(binding) = self.binding_for(&conn, event)? else {
            let text = format!("{list}\n{}", render::session_use_hint(locale));
            self.reply_card(event, runtime, &text).await?;
            return Ok(());
        };
        if sessions.is_empty() {
            self.reply_card(event, runtime, &list).await?;
            return Ok(());
        }
        let mut buttons = Vec::new();
        for session in sessions.iter() {
            let payload = serde_json::to_string(&ValuePayload {
                value: session.id.as_str().to_string(),
            })
            .map_err(|_| im_payload_error())?;
            let minted = self.mint_action(&conn, &binding, ACTION_SESSION_USE, &payload)?;
            let title = if session.title.trim().is_empty() {
                locale.text("Untitled", "未命名", "未命名").to_string()
            } else {
                render::truncate(session.title.trim(), 28)
            };
            buttons.push(ImButton {
                action_id: minted.action_id,
                label: title,
            });
        }
        let text = format!("{list}\n{}", render::session_tap_hint(locale));
        self.reply_buttons(event, runtime, &text, buttons).await
    }

    /// The model / effort / mode / Agent picker.
    pub(super) async fn send_runtime_card(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
        field: RuntimeField,
    ) -> VibexResult<()> {
        let locale = self.locale(event);
        if !principal.role.can_prompt() {
            self.reply(
                event,
                runtime,
                locale.text(
                    "Your role cannot change the runtime.",
                    "你的角色不能修改运行参数。",
                    "你的角色不能修改執行參數。",
                ),
            )
            .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(
                event,
                runtime,
                locale.text(
                    "No session is bound.",
                    "还没有绑定会话。",
                    "還沒有綁定工作階段。",
                ),
            )
            .await?;
            return Ok(());
        };
        let options = self
            .inner
            .agent
            .runtime_options(&binding.session_id)
            .await?;
        let (choices, current) = self.runtime_card_body(&options, field, locale);
        if choices.is_empty() {
            self.reply(
                event,
                runtime,
                locale.text(
                    "Nothing to choose from here.",
                    "这里没有可选项。",
                    "這裡沒有可選項。",
                ),
            )
            .await?;
            return Ok(());
        }
        let mut buttons = Vec::new();
        for choice in choices.iter().take(MAX_CHOICES) {
            let payload = serde_json::to_string(&ValuePayload {
                value: choice.id.clone(),
            })
            .map_err(|_| im_payload_error())?;
            let minted = self.mint_action(&conn, &binding, field.action_kind(), &payload)?;
            let marker = if choice.current { "✓ " } else { "" };
            buttons.push(ImButton {
                action_id: minted.action_id,
                label: format!("{marker}{}", render::truncate(&choice.label, 40)),
            });
        }
        let text = render::picker(locale, field.label(locale), &current);
        self.reply_buttons(event, runtime, &text, buttons).await
    }

    /// The Agent picker needs the whole selection, not one value.
    async fn send_agent_card(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<()> {
        let locale = self.locale(event);
        if !principal.role.can_prompt() {
            self.reply(
                event,
                runtime,
                locale.text(
                    "Your role cannot change the runtime.",
                    "你的角色不能修改运行参数。",
                    "你的角色不能修改執行參數。",
                ),
            )
            .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(
                event,
                runtime,
                locale.text(
                    "No session is bound.",
                    "还没有绑定会话。",
                    "還沒有綁定工作階段。",
                ),
            )
            .await?;
            return Ok(());
        };
        let options = self
            .inner
            .agent
            .runtime_options(&binding.session_id)
            .await?;
        if options.agents.is_empty() {
            self.reply(
                event,
                runtime,
                locale.text(
                    "No other Agent is available.",
                    "没有其他可用的 Agent。",
                    "沒有其他可用的 Agent。",
                ),
            )
            .await?;
            return Ok(());
        }
        let current = options
            .agents
            .iter()
            .find(|choice| choice.current)
            .map(|choice| choice.agent_label.clone())
            .unwrap_or_else(|| options.agent_label.clone());
        let mut buttons = Vec::new();
        for choice in options.agents.iter().take(MAX_CHOICES) {
            let payload = serde_json::to_string(&AgentChoicePayload {
                agent_id: choice.agent_id.as_str().to_string(),
                auth_source: choice.auth_source.clone(),
                model: choice.model.clone(),
            })
            .map_err(|_| im_payload_error())?;
            let minted =
                self.mint_action(&conn, &binding, RuntimeField::Agent.action_kind(), &payload)?;
            buttons.push(ImButton {
                action_id: minted.action_id,
                label: agent_button_label(choice),
            });
        }
        let text = render::picker(locale, RuntimeField::Agent.label(locale), &current);
        self.reply_buttons(event, runtime, &text, buttons).await
    }

    /// The credential-source picker for the current Agent.
    async fn send_provider_card(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<()> {
        let locale = self.locale(event);
        if !principal.role.can_prompt() {
            self.reply(
                event,
                runtime,
                locale.text(
                    "Your role cannot change the runtime.",
                    "你的角色不能修改运行参数。",
                    "你的角色不能修改執行參數。",
                ),
            )
            .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(
                event,
                runtime,
                locale.text(
                    "No session is bound.",
                    "还没有绑定会话。",
                    "還沒有綁定工作階段。",
                ),
            )
            .await?;
            return Ok(());
        };
        let options = self
            .inner
            .agent
            .runtime_options(&binding.session_id)
            .await?;
        if options.providers.is_empty() {
            self.reply(
                event,
                runtime,
                locale.text(
                    "This Agent has one source to run under.",
                    "该 Agent 只有一个可用的供应商。",
                    "該 Agent 只有一個可用的供應商。",
                ),
            )
            .await?;
            return Ok(());
        }
        let current = options
            .providers
            .iter()
            .find(|choice| choice.current)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| options.agent_label.clone());
        let mut buttons = Vec::new();
        for choice in options.providers.iter().take(MAX_CHOICES) {
            let payload = serde_json::to_string(&ProviderChoicePayload {
                auth_source: choice.auth_source.clone(),
                model: choice.model.clone(),
            })
            .map_err(|_| im_payload_error())?;
            let minted = self.mint_action(
                &conn,
                &binding,
                RuntimeField::Provider.action_kind(),
                &payload,
            )?;
            let marker = if choice.current { "✓ " } else { "" };
            buttons.push(ImButton {
                action_id: minted.action_id,
                label: format!("{marker}{}", render::truncate(&choice.label, 40)),
            });
        }
        let text = render::picker(locale, RuntimeField::Provider.label(locale), &current);
        self.reply_buttons(event, runtime, &text, buttons).await
    }

    /// The per-field choices plus the card's body text.
    fn runtime_card_body(
        &self,
        options: &ImRuntimeOptions,
        field: RuntimeField,
        locale: ImLocale,
    ) -> (Vec<ImRuntimeChoice>, String) {
        let choices = match field {
            RuntimeField::Model => options.models.clone(),
            RuntimeField::Effort => options.efforts.clone(),
            RuntimeField::Mode => options.modes.clone(),
            RuntimeField::Agent | RuntimeField::Provider => Vec::new(),
        };
        let default = locale.text("default", "默认", "預設");
        let current = choices
            .iter()
            .find(|choice| choice.current)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| match field {
                RuntimeField::Model => options.model_label.clone(),
                RuntimeField::Effort => options
                    .effort_label
                    .clone()
                    .unwrap_or_else(|| default.to_string()),
                RuntimeField::Mode => options
                    .mode_label
                    .clone()
                    .unwrap_or_else(|| default.to_string()),
                RuntimeField::Agent => options.agent_label.clone(),
                RuntimeField::Provider => options.agent_label.clone(),
            });
        (choices, current)
    }

    /// Handles a command button. Returns whether the action was one.
    pub(super) async fn handle_action(
        self: &Arc<Self>,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
        binding: &ImBindingRecord,
        action: &ImActionRecord,
    ) -> VibexResult<bool> {
        let locale = self.locale(event);
        let already = locale.text(
            "This action was already handled.",
            "这个操作已经处理过了。",
            "這個操作已經處理過了。",
        );
        let kind = action.kind.as_str();
        if kind == ACTION_MENU_HELP {
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, already).await?;
                return Ok(true);
            }
            self.reply_card(event, runtime, &render::help(locale))
                .await?;
            return Ok(true);
        }
        if kind == ACTION_MENU_SESSIONS {
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, already).await?;
                return Ok(true);
            }
            self.send_sessions_card(event, runtime, principal).await?;
            return Ok(true);
        }
        if let Some(field) = kind
            .strip_prefix(ACTION_MENU_PREFIX)
            .and_then(RuntimeField::parse)
        {
            if !principal.role.can_prompt() {
                self.reply(
                    event,
                    runtime,
                    locale.text(
                        "Your role cannot change the runtime.",
                        "你的角色不能修改运行参数。",
                        "你的角色不能修改執行參數。",
                    ),
                )
                .await?;
                return Ok(true);
            }
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, already).await?;
                return Ok(true);
            }
            match field {
                RuntimeField::Agent => self.send_agent_card(event, runtime, principal).await?,
                RuntimeField::Provider => {
                    self.send_provider_card(event, runtime, principal).await?
                }
                _ => {
                    self.send_runtime_card(event, runtime, principal, field)
                        .await?
                }
            }
            return Ok(true);
        }
        if kind == ACTION_STATUS_STOP {
            if !principal.role.can_prompt() {
                self.reply(
                    event,
                    runtime,
                    locale.text(
                        "Your role cannot stop sessions.",
                        "你的角色不能停止会话。",
                        "你的角色不能停止工作階段。",
                    ),
                )
                .await?;
                return Ok(true);
            }
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, already).await?;
                return Ok(true);
            }
            self.inner.agent.interrupt(&binding.session_id).await?;
            self.stop_live_turn(binding).await;
            self.reply(
                event,
                runtime,
                locale.text("Stopped.", "已停止。", "已停止。"),
            )
            .await?;
            return Ok(true);
        }
        if kind == ACTION_SESSION_USE {
            if !principal.role.can_prompt() {
                self.reply(
                    event,
                    runtime,
                    locale.text(
                        "Your role cannot bind sessions.",
                        "你的角色不能绑定会话。",
                        "你的角色不能綁定工作階段。",
                    ),
                )
                .await?;
                return Ok(true);
            }
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, already).await?;
                return Ok(true);
            }
            let payload: ValuePayload =
                serde_json::from_str(&action.payload_json).map_err(|_| im_payload_error())?;
            let session_id = VibexSessionId::parse(payload.value)?;
            let session = self.inner.agent.get_session(&session_id).await?;
            let conn = self.open()?;
            let rebound = self.make_binding(event, principal, &session, Some(binding));
            ImBindingRepository::upsert(&conn, &rebound)?;
            self.reply(
                event,
                runtime,
                &format!(
                    "{} #{} · {}",
                    locale.text("Bound to", "已绑定", "已綁定"),
                    render::short_session_id(&session),
                    render::truncate(session.title.trim(), 48)
                ),
            )
            .await?;
            return Ok(true);
        }
        if let Some(field) = kind
            .strip_prefix(ACTION_RUNTIME_PREFIX)
            .and_then(RuntimeField::parse)
        {
            if !principal.role.can_prompt() {
                self.reply(
                    event,
                    runtime,
                    locale.text(
                        "Your role cannot change the runtime.",
                        "你的角色不能修改运行参数。",
                        "你的角色不能修改執行參數。",
                    ),
                )
                .await?;
                return Ok(true);
            }
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, already).await?;
                return Ok(true);
            }
            match self
                .apply_runtime_choice(binding, field, &action.payload_json)
                .await
            {
                Ok((value, selection)) => {
                    let text = render::runtime_applied(field.label(locale), &value, &selection);
                    self.reply_card(event, runtime, &text).await?;
                }
                Err(error) => {
                    let command = field.action_kind().trim_start_matches("runtime:");
                    let text = match locale {
                        ImLocale::En => {
                            format!(
                                "Could not apply that: {}. Send /{command} again.",
                                error.message
                            )
                        }
                        ImLocale::ZhCn => {
                            format!("未能应用：{}。请重新发送 /{command}。", error.message)
                        }
                        ImLocale::ZhTw => {
                            format!("未能套用：{}。請重新傳送 /{command}。", error.message)
                        }
                    };
                    self.reply(event, runtime, &text).await?;
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn consume_action(
        &self,
        action: &ImActionRecord,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<bool> {
        let conn = self.open()?;
        ImActionRepository::consume(
            &conn,
            &action.action_id,
            &principal.principal_id,
            unix_timestamp_ms(),
        )
    }

    /// Reads the options again and applies the choice against their revisions.
    /// Reads the options again and applies the choice against their revisions.
    ///
    /// Returns the chosen value's own label and the selection it produced, so a
    /// confirmation can name what changed and show what it changed to.
    async fn apply_runtime_choice(
        &self,
        binding: &ImBindingRecord,
        field: RuntimeField,
        payload_json: &str,
    ) -> VibexResult<(String, String)> {
        let locale = self.locale_for_channel(&binding.channel_id);
        let default = locale.text("default", "默认", "預設").to_string();
        let options = self
            .inner
            .agent
            .runtime_options(&binding.session_id)
            .await?;
        let mut desired = options.current.clone();
        let value = match field {
            RuntimeField::Model => {
                let payload: ValuePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                let value = options
                    .models
                    .iter()
                    .find(|choice| choice.id == payload.value)
                    .map(|choice| choice.label.clone())
                    .unwrap_or_else(|| payload.value.clone());
                desired.model = RuntimeModelSelection::explicit(payload.value);
                // A different model advertises different efforts.
                desired.reasoning_effort = None;
                value
            }
            RuntimeField::Effort => {
                let payload: ValuePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                let value = if payload.value.is_empty() {
                    default
                } else {
                    options
                        .efforts
                        .iter()
                        .find(|choice| choice.id == payload.value)
                        .map(|choice| choice.label.clone())
                        .unwrap_or_else(|| payload.value.clone())
                };
                desired.reasoning_effort = (!payload.value.is_empty()).then_some(payload.value);
                value
            }
            RuntimeField::Mode => {
                let payload: ValuePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                let value = if payload.value.is_empty() {
                    default
                } else {
                    options
                        .modes
                        .iter()
                        .find(|choice| choice.id == payload.value)
                        .map(|choice| choice.label.clone())
                        .unwrap_or_else(|| payload.value.clone())
                };
                desired.mode_id = (!payload.value.is_empty()).then_some(payload.value);
                value
            }
            RuntimeField::Agent => {
                let payload: AgentChoicePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                let value = options
                    .agents
                    .iter()
                    .find(|choice| choice.agent_id.as_str() == payload.agent_id)
                    .map(|choice| choice.agent_label.clone())
                    .unwrap_or_else(|| payload.agent_id.clone());
                desired.agent_id = AgentId::parse(&payload.agent_id)?;
                desired.auth_source = payload.auth_source;
                desired.model = payload.model;
                desired.reasoning_effort = None;
                desired.mode_id = None;
                value
            }
            RuntimeField::Provider => {
                let payload: ProviderChoicePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                let value = options
                    .providers
                    .iter()
                    .find(|choice| choice.auth_source == payload.auth_source)
                    .map(|choice| choice.label.clone())
                    .unwrap_or_else(|| locale.text("Provider", "供应商", "供應商").to_string());
                desired.auth_source = payload.auth_source;
                desired.model = payload.model;
                // A different source advertises different efforts.
                desired.reasoning_effort = None;
                value
            }
        };
        let state = self
            .inner
            .agent
            .set_desired_runtime(SetDesiredAgentSessionRuntimeRequest {
                session_id: binding.session_id.clone(),
                idempotency_key: Uuid::new_v4().simple().to_string(),
                expected_revision: options.session_revision,
                expected_selection_revision: options.selection_revision,
                desired,
                interaction: RuntimeSelectionInteraction::Seamless,
            })
            .await?;
        Ok((value, render::selection_line(locale, &state.desired)))
    }

    /// `/model`, `/effort`, `/mode`, `/agent`, with an optional direct value.
    pub(super) async fn handle_runtime_command(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
        field: RuntimeField,
        args: &str,
    ) -> VibexResult<()> {
        if args.trim().is_empty() {
            return match field {
                RuntimeField::Agent => self.send_agent_card(event, runtime, principal).await,
                RuntimeField::Provider => self.send_provider_card(event, runtime, principal).await,
                _ => {
                    self.send_runtime_card(event, runtime, principal, field)
                        .await
                }
            };
        }
        let locale = self.locale(event);
        if !principal.role.can_prompt() {
            self.reply(
                event,
                runtime,
                locale.text(
                    "Your role cannot change the runtime.",
                    "你的角色不能修改运行参数。",
                    "你的角色不能修改執行參數。",
                ),
            )
            .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(
                event,
                runtime,
                locale.text(
                    "No session is bound.",
                    "还没有绑定会话。",
                    "還沒有綁定工作階段。",
                ),
            )
            .await?;
            return Ok(());
        };
        // Switching Agent or provider needs the source and model that come with
        // the choice, so the text form opens the card rather than guessing from
        // a bare id.
        match field {
            RuntimeField::Agent => {
                self.send_agent_card(event, runtime, principal).await?;
                return Ok(());
            }
            RuntimeField::Provider => {
                self.send_provider_card(event, runtime, principal).await?;
                return Ok(());
            }
            _ => {}
        }
        let payload = serde_json::to_string(&ValuePayload {
            value: args.trim().to_string(),
        })
        .map_err(|_| im_payload_error())?;
        match self.apply_runtime_choice(&binding, field, &payload).await {
            Ok((value, selection)) => {
                let text = render::runtime_applied(field.label(locale), &value, &selection);
                self.reply(event, runtime, &text).await?;
            }
            Err(error) => {
                let command = field.action_kind().trim_start_matches("runtime:");
                let text = match locale {
                    ImLocale::En => {
                        format!(
                            "Could not apply that: {}. Send /{command} again.",
                            error.message
                        )
                    }
                    ImLocale::ZhCn => {
                        format!("未能应用：{}。请重新发送 /{command}。", error.message)
                    }
                    ImLocale::ZhTw => {
                        format!("未能套用：{}。請重新傳送 /{command}。", error.message)
                    }
                };
                self.reply(event, runtime, &text).await?;
            }
        }
        Ok(())
    }
}

/// A picker button's text.
///
/// A button cannot show an image, and a stand-in glyph reads as noise, so the
/// Agent's own name is the label; the keyboard keeps it from being elided by
/// giving a long label a row to itself.
fn agent_button_label(choice: &vibex_core::ImRuntimeAgentChoice) -> String {
    let name = if choice.agent_label.trim().is_empty() {
        choice.agent_id.as_str()
    } else {
        choice.agent_label.trim()
    };
    let marker = if choice.current { "✓ " } else { "" };
    format!("{marker}{}", render::truncate(name, 32))
}

fn im_payload_error() -> VibexError {
    VibexError::validation(
        "im_action_payload_invalid",
        "the action payload could not be encoded",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(
        agent_id: &str,
        agent_label: &str,
        current: bool,
    ) -> vibex_core::ImRuntimeAgentChoice {
        vibex_core::ImRuntimeAgentChoice {
            agent_id: AgentId::parse(agent_id).expect("agent id"),
            auth_source: RuntimeAuthSource::provider_profile(vibex_core::ProviderProfileId::new()),
            model: RuntimeModelSelection::explicit("model"),
            agent_label: agent_label.to_string(),
            current,
        }
    }

    #[test]
    fn an_agent_button_names_the_agent_and_marks_the_current_one() {
        let pick = choice("claude", "Claude Code", false);
        assert_eq!(agent_button_label(&pick), "Claude Code");
        let current = choice("claude", "Claude Code", true);
        assert_eq!(agent_button_label(&current), "✓ Claude Code");
    }

    #[test]
    fn a_long_agent_name_is_elided_by_the_label_not_by_the_client() {
        let long = choice("something", &"x".repeat(80), false);
        assert!(agent_button_label(&long).chars().count() < 40);
    }
}
