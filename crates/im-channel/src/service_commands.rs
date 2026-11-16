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
pub(super) const ACTION_STATUS_STOP: &str = "status:stop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeField {
    Model,
    Effort,
    Mode,
    Agent,
}

impl RuntimeField {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "effort" => Some(Self::Effort),
            "mode" => Some(Self::Mode),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }

    fn action_kind(self) -> &'static str {
        match self {
            Self::Model => "runtime:model",
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
                false,
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
            return self.reply(event, runtime, &render::help(locale, "/")).await;
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            return self
                .reply(event, runtime, &render::help_short(locale))
                .await;
        };
        let button = self.mint_action(&conn, &binding, ACTION_MENU_HELP, "{}")?;
        let label = match locale {
            ImLocale::En => "All commands",
            ImLocale::ZhCn => "全部命令",
            ImLocale::ZhTw => "全部指令",
        };
        self.reply_buttons(
            event,
            runtime,
            &render::help_short(locale),
            vec![ImButton {
                action_id: button.action_id,
                label: format!("{label} ({})", button.label),
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
        if !principal.role.can_prompt() {
            self.reply(event, runtime, "Your role cannot bind sessions.")
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
        let conn = self.open()?;
        let locale = self.locale(event);
        // Listing is not binding: a chat that has not chosen a session yet can
        // still see what is there, it just has no card to mint buttons under.
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(event, runtime, &render::session_list(&sessions, locale))
                .await?;
            return Ok(());
        };
        let mut text = render::session_list(&sessions, locale);
        let mut buttons = Vec::new();
        for session in sessions.iter().take(SESSION_PICK_LIMIT) {
            let payload = serde_json::to_string(&ValuePayload {
                value: session.id.as_str().to_string(),
            })
            .map_err(|_| im_payload_error())?;
            let minted = self.mint_action(&conn, &binding, ACTION_SESSION_USE, &payload)?;
            let title = if session.title.trim().is_empty() {
                "Untitled".to_string()
            } else {
                render::truncate(session.title.trim(), 28)
            };
            buttons.push(ImButton {
                action_id: minted.action_id,
                label: title,
            });
        }
        // The list already ends with "Use /use <n>"; the buttons replace it.
        if !buttons.is_empty() {
            text = text.replace(
                match locale {
                    ImLocale::En => "Use /use <n> to bind one.",
                    ImLocale::ZhCn => "用 /use <序号> 绑定一个。",
                    ImLocale::ZhTw => "用 /use <序號> 綁定一個。",
                },
                match locale {
                    ImLocale::En => "Tap a session to bind it.",
                    ImLocale::ZhCn => "点一个会话即可绑定。",
                    ImLocale::ZhTw => "點一個工作階段即可綁定。",
                },
            );
        }
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
        if !principal.role.can_prompt() {
            self.reply(event, runtime, "Your role cannot change the runtime.")
                .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(event, runtime, "No session is bound.").await?;
            return Ok(());
        };
        let locale = self.locale(event);
        let options = self
            .inner
            .agent
            .runtime_options(&binding.session_id)
            .await?;
        let (choices, text) = self.runtime_card_body(&options, field, locale);
        if choices.is_empty() {
            self.reply(
                event,
                runtime,
                match locale {
                    ImLocale::En => "This selection has nothing to choose from.",
                    ImLocale::ZhCn => "当前选择没有可选项。",
                    ImLocale::ZhTw => "目前選擇沒有可選項。",
                },
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
        self.reply_buttons(event, runtime, &text, buttons).await
    }

    /// The Agent picker needs the whole selection, not one value.
    async fn send_agent_card(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        principal: &ImPrincipalRecord,
    ) -> VibexResult<()> {
        if !principal.role.can_prompt() {
            self.reply(event, runtime, "Your role cannot change the runtime.")
                .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(event, runtime, "No session is bound.").await?;
            return Ok(());
        };
        let locale = self.locale(event);
        let options = self
            .inner
            .agent
            .runtime_options(&binding.session_id)
            .await?;
        if options.agents.is_empty() {
            self.reply(event, runtime, "No other Agent is available.")
                .await?;
            return Ok(());
        }
        let mut text = format!(
            "{}\n{}",
            RuntimeField::Agent.label(locale),
            render::runtime_current(locale, &options)
        );
        text.push('\n');
        text.push_str(match locale {
            ImLocale::En => "Tap an Agent to switch. It applies to the next turn.",
            ImLocale::ZhCn => "点选 Agent 切换，下一轮生效。",
            ImLocale::ZhTw => "點選 Agent 切換，下一輪生效。",
        });
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
            let marker = if choice.current { "✓ " } else { "" };
            buttons.push(ImButton {
                action_id: minted.action_id,
                label: format!("{marker}{}", render::truncate(&choice.label, 40)),
            });
        }
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
            RuntimeField::Agent => Vec::new(),
        };
        let mut text = format!(
            "{}\n{}",
            field.label(locale),
            render::runtime_current(locale, options)
        );
        text.push('\n');
        text.push_str(match locale {
            ImLocale::En => "Tap a value. It applies to the next turn.",
            ImLocale::ZhCn => "点选后下一轮生效。",
            ImLocale::ZhTw => "點選後下一輪生效。",
        });
        (choices, text)
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
        let kind = action.kind.as_str();
        if kind == ACTION_MENU_HELP {
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, "This action was already handled.")
                    .await?;
                return Ok(true);
            }
            self.reply(event, runtime, &render::help(self.locale(event), "/"))
                .await?;
            return Ok(true);
        }
        if kind == ACTION_MENU_SESSIONS {
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, "This action was already handled.")
                    .await?;
                return Ok(true);
            }
            self.send_sessions_card(event, runtime, principal).await?;
            return Ok(true);
        }
        if kind == ACTION_STATUS_STOP {
            if !principal.role.can_prompt() {
                self.reply(event, runtime, "Your role cannot stop sessions.")
                    .await?;
                return Ok(true);
            }
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, "This action was already handled.")
                    .await?;
                return Ok(true);
            }
            self.inner.agent.interrupt(&binding.session_id).await?;
            self.stop_live_turn(binding).await;
            self.reply(event, runtime, "Stopped the current turn.")
                .await?;
            return Ok(true);
        }
        if kind == ACTION_SESSION_USE {
            if !principal.role.can_prompt() {
                self.reply(event, runtime, "Your role cannot bind sessions.")
                    .await?;
                return Ok(true);
            }
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, "This action was already handled.")
                    .await?;
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
                    "Bound to #{} · {}",
                    render::short_session_id(&session),
                    session.title
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
                self.reply(event, runtime, "Your role cannot change the runtime.")
                    .await?;
                return Ok(true);
            }
            if !self.consume_action(action, principal)? {
                self.reply(event, runtime, "This action was already handled.")
                    .await?;
                return Ok(true);
            }
            let locale = self.locale(event);
            match self
                .apply_runtime_choice(binding, field, &action.payload_json)
                .await
            {
                Ok(summary) => {
                    self.reply(
                        event,
                        runtime,
                        &format!("{}: {summary}", field.label(locale)),
                    )
                    .await?;
                }
                Err(error) => {
                    let text = match locale {
                        ImLocale::En => format!(
                            "Could not apply that: {}. Send /{} again.",
                            error.message,
                            field.action_kind().trim_start_matches("runtime:")
                        ),
                        ImLocale::ZhCn => format!(
                            "未能应用：{}。请重新发送 /{}。",
                            error.message,
                            field.action_kind().trim_start_matches("runtime:")
                        ),
                        ImLocale::ZhTw => format!(
                            "未能套用：{}。請重新傳送 /{}。",
                            error.message,
                            field.action_kind().trim_start_matches("runtime:")
                        ),
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
    async fn apply_runtime_choice(
        &self,
        binding: &ImBindingRecord,
        field: RuntimeField,
        payload_json: &str,
    ) -> VibexResult<String> {
        let options = self
            .inner
            .agent
            .runtime_options(&binding.session_id)
            .await?;
        let mut desired = options.current.clone();
        match field {
            RuntimeField::Model => {
                let payload: ValuePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                desired.model = RuntimeModelSelection::explicit(payload.value);
                // A different model advertises different efforts.
                desired.reasoning_effort = None;
            }
            RuntimeField::Effort => {
                let payload: ValuePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                desired.reasoning_effort = (!payload.value.is_empty()).then_some(payload.value);
            }
            RuntimeField::Mode => {
                let payload: ValuePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                desired.mode_id = (!payload.value.is_empty()).then_some(payload.value);
            }
            RuntimeField::Agent => {
                let payload: AgentChoicePayload =
                    serde_json::from_str(payload_json).map_err(|_| im_payload_error())?;
                desired.agent_id = AgentId::parse(&payload.agent_id)?;
                desired.auth_source = payload.auth_source;
                desired.model = payload.model;
                desired.reasoning_effort = None;
                desired.mode_id = None;
            }
        }
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
        Ok(render::runtime_selection_summary(
            self.locale_for_channel(&binding.channel_id),
            &state.desired,
        ))
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
                _ => {
                    self.send_runtime_card(event, runtime, principal, field)
                        .await
                }
            };
        }
        if !principal.role.can_prompt() {
            self.reply(event, runtime, "Your role cannot change the runtime.")
                .await?;
            return Ok(());
        }
        let conn = self.open()?;
        let Some(binding) = self.binding_for(&conn, event)? else {
            self.reply(event, runtime, "No session is bound.").await?;
            return Ok(());
        };
        let locale = self.locale(event);
        // The text form takes the value itself: an id for a model, the literal
        // effort or mode, and `id` for an Agent (which can only be chosen from
        // the card, because a switch needs its source and model too).
        if field == RuntimeField::Agent {
            self.send_agent_card(event, runtime, principal).await?;
            return Ok(());
        }
        let payload = serde_json::to_string(&ValuePayload {
            value: args.trim().to_string(),
        })
        .map_err(|_| im_payload_error())?;
        match self.apply_runtime_choice(&binding, field, &payload).await {
            Ok(summary) => {
                self.reply(
                    event,
                    runtime,
                    &format!("{}: {summary}", field.label(locale)),
                )
                .await?;
            }
            Err(error) => {
                self.reply(
                    event,
                    runtime,
                    &format!("Could not apply that: {}", error.message),
                )
                .await?;
            }
        }
        Ok(())
    }
}

fn im_payload_error() -> VibexError {
    VibexError::validation(
        "im_action_payload_invalid",
        "the action payload could not be encoded",
    )
}
