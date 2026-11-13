use super::*;

impl ImChannelService {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn apply_resolution(
        &self,
        event: &ImInboundEvent,
        runtime: &ChannelRuntime,
        binding: &ImBindingRecord,
        request: &PermissionRequest,
        option: &PermissionResponseOption,
        principal: &ImPrincipalRecord,
        card_id: Option<&str>,
    ) -> VibexResult<String> {
        let resolution = PermissionResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            response: option.response,
            responder_device_id: None,
            provider_resolution_id: Some(option.option_id.clone()),
            note: Some(format!(
                "via IM channel ({}, {})",
                principal.display_name,
                principal.principal_id.as_str()
            )),
            resolved_at_ms: unix_timestamp_ms(),
        };
        self.inner
            .agent
            .resolve_permission(ResolvePermissionRequest {
                session_id: request.session_id.clone(),
                request_id: request.id.clone(),
                resolution,
            })
            .await?;
        let status = match option.response {
            PermissionResponseKind::Approve => "approved",
            PermissionResponseKind::Deny => "denied",
            PermissionResponseKind::AlwaysAllowForSession => "approved for this session",
        };
        let conn = self.open()?;
        if let Some(card_id) = card_id.filter(|id| !id.is_empty()) {
            ImApprovalCardRepository::set_status(&conn, card_id, status, unix_timestamp_ms())?;
            ImActionRepository::invalidate_for_card(&conn, card_id, unix_timestamp_ms())?;
        }
        let cards = ImApprovalCardRepository::list_for_request(
            &conn,
            &request.session_id,
            request.id.as_str(),
        )?;
        for card in cards {
            ImApprovalCardRepository::set_status(
                &conn,
                &card.card_id,
                status,
                unix_timestamp_ms(),
            )?;
            ImActionRepository::invalidate_for_card(&conn, &card.card_id, unix_timestamp_ms())?;
            if let Some(message_id) = card.external_message_id.as_deref() {
                let _ = runtime
                    .outbox
                    .edit(
                        ImTarget {
                            chat_id: binding.external_chat_id.clone(),
                            thread_key: binding.thread_key.clone(),
                        },
                        message_id.to_string(),
                        format!("{}: {}\n{}", status, request.title, principal.display_name),
                    )
                    .await;
            }
        }
        let _ = event;
        Ok(format!("{status}: {}", request.title))
    }

    pub(super) async fn send_approval_card(
        &self,
        binding: &ImBindingRecord,
        request: &PermissionRequest,
        sequence: i64,
    ) -> VibexResult<()> {
        let conn = self.open()?;
        let existing_card = ImApprovalCardRepository::get_by_request(
            &conn,
            &binding.binding_id,
            request.id.as_str(),
        )?;
        if let Some(card) = existing_card.as_ref()
            && (card.external_message_id.is_some() || card.status != "pending")
        {
            return Ok(());
        }
        let Some(runtime) = self
            .inner
            .channels
            .read()
            .await
            .get(&binding.channel_id)
            .cloned()
        else {
            return Ok(());
        };
        let card_id = existing_card
            .as_ref()
            .map(|card| card.card_id.clone())
            .unwrap_or_else(|| format!("imcard_{}", Uuid::new_v4().simple()));
        let now = unix_timestamp_ms();
        ImApprovalCardRepository::upsert(
            &conn,
            &ImApprovalCardRecord {
                card_id: card_id.clone(),
                binding_id: binding.binding_id.clone(),
                session_id: binding.session_id.clone(),
                request_id: request.id.as_str().to_string(),
                external_message_id: None,
                status: "pending".to_string(),
                created_at_ms: existing_card
                    .as_ref()
                    .map(|card| card.created_at_ms)
                    .unwrap_or(now),
                updated_at_ms: now,
            },
        )?;
        if existing_card.is_some() {
            ImActionRepository::invalidate_for_card(&conn, &card_id, now)?;
        }
        let mut buttons = Vec::new();
        let mut codes = Vec::new();
        for (choice, kind, label) in [
            (
                ApprovalChoice::Approve,
                PermissionResponseKind::Approve,
                "Approve",
            ),
            (
                ApprovalChoice::Always,
                PermissionResponseKind::AlwaysAllowForSession,
                "Always allow",
            ),
            (ApprovalChoice::Deny, PermissionResponseKind::Deny, "Deny"),
        ] {
            let Some(option) = request
                .response_options
                .iter()
                .find(|option| option.response == kind)
            else {
                continue;
            };
            let action_id = ImActionId::new();
            let short_code = action_short_code(&action_id);
            let payload = ApprovalActionPayload {
                request_id: request.id.as_str().to_string(),
                option_id: option.option_id.clone(),
                choice: choice.as_str().to_string(),
            };
            ImActionRepository::insert(
                &conn,
                &ImActionRecord {
                    action_id: action_id.clone(),
                    short_code: short_code.clone(),
                    card_id: Some(card_id.clone()),
                    channel_id: binding.channel_id.clone(),
                    binding_id: binding.binding_id.clone(),
                    kind: choice.as_str().to_string(),
                    payload_json: serde_json::to_string(&payload).map_err(|_| {
                        VibexError::validation(
                            "im_action_payload_invalid",
                            "approval action payload could not be encoded",
                        )
                    })?,
                    expires_at_ms: request
                        .expires_at_ms
                        .unwrap_or_else(|| now.saturating_add(PAIRING_TTL_MS)),
                    consumed_at_ms: None,
                    consumed_by_principal_id: None,
                    created_at_ms: now,
                },
            )?;
            buttons.push(ImButton {
                action_id,
                label: format!("{label} ({short_code})"),
            });
            codes.push(format!("{label}: {short_code}"));
        }
        if buttons.is_empty() {
            return Ok(());
        }
        let mut text =
            render::permission_text(request, self.locale_for_channel(&binding.channel_id));
        text.push('\n');
        text.push_str(&codes.join(" · "));
        let target = ImTarget {
            chat_id: binding.external_chat_id.clone(),
            thread_key: binding.thread_key.clone(),
        };
        // The card is the one message a user must be able to act on later, so it
        // outranks a progress reply when the provider is rate limiting us.
        if let Ok(sent) = runtime
            .outbox
            .send(target, text, buttons, OutboundPriority::Critical)
            .await
        {
            ImApprovalCardRepository::set_message_id(
                &conn,
                &card_id,
                &sent.message_id,
                unix_timestamp_ms(),
            )?;
        }
        ImBindingRepository::set_delivered_sequence(
            &conn,
            &binding.binding_id,
            sequence,
            unix_timestamp_ms(),
        )?;
        Ok(())
    }

    pub(super) async fn update_approval_cards(
        &self,
        session_id: &VibexSessionId,
        request_id: &str,
        response: PermissionResponseKind,
    ) -> VibexResult<()> {
        let conn = self.open()?;
        let cards = ImApprovalCardRepository::list_for_request(&conn, session_id, request_id)?;
        let status = match response {
            PermissionResponseKind::Approve => "approved",
            PermissionResponseKind::Deny => "denied",
            PermissionResponseKind::AlwaysAllowForSession => "approved for this session",
        };
        for card in cards {
            ImApprovalCardRepository::set_status(
                &conn,
                &card.card_id,
                status,
                unix_timestamp_ms(),
            )?;
            ImActionRepository::invalidate_for_card(&conn, &card.card_id, unix_timestamp_ms())?;
            let Some(binding) = ImBindingRepository::get(&conn, &card.binding_id)? else {
                continue;
            };
            if let Some(message_id) = card.external_message_id.as_deref()
                && let Some(runtime) = self
                    .inner
                    .channels
                    .read()
                    .await
                    .get(&binding.channel_id)
                    .cloned()
            {
                let _ = runtime
                    .outbox
                    .edit(
                        ImTarget {
                            chat_id: binding.external_chat_id.clone(),
                            thread_key: binding.thread_key.clone(),
                        },
                        message_id.to_string(),
                        format!("{status}: approval request {}", request_id),
                    )
                    .await;
            }
        }
        Ok(())
    }
}
