use super::*;

impl ImChannelService {
    pub(super) async fn event_loop(this: Weak<Self>, mut shutdown: watch::Receiver<bool>) {
        let Some(service) = this.upgrade() else {
            return;
        };
        let mut timeline = service.inner.agent.subscribe_timeline();
        let mut sessions = service.inner.agent.subscribe_session_updates();
        let mut notifications = service.inner.agent.subscribe_notifications();
        drop(service);
        loop {
            let Some(service) = this.upgrade() else {
                break;
            };
            tokio::select! {
                _ = shutdown.changed() => break,
                event = timeline.recv() => {
                    match event {
                        Ok(event) => {
                            if let Err(error) = service.handle_timeline_event(event).await {
                                tracing::warn!(target: "vibex_im", error_code = %error.code, "IM timeline event failed");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                event = sessions.recv() => {
                    if let Ok(session) = event {
                        let _ = service.handle_session_update(session).await;
                    }
                }
                event = notifications.recv() => {
                    if let Ok(notification) = event {
                        let _ = service.handle_notification(notification).await;
                    }
                }
            }
        }
    }

    async fn handle_timeline_event(&self, event: TimelineLiveEvent) -> VibexResult<()> {
        let conn = self.open()?;
        let bindings = ImBindingRepository::list_active_for_session(&conn, &event.session_id)?;
        if bindings.is_empty() {
            return Ok(());
        }
        let event_id = timeline_event_id(&event.item.payload);
        for binding in bindings {
            let Some(record) = ImChannelRepository::get(&conn, &binding.channel_id)? else {
                continue;
            };
            if !record.enabled {
                continue;
            }
            if let Some(event_id) = event_id
                && !event_allowed(&record, event_id)
            {
                continue;
            }
            match &event.item.payload {
                TimelinePayload::AgentMessage(message) if message.is_final => {
                    let text = render::final_answer(&message.text);
                    if !text.trim().is_empty() {
                        self.send_to_binding(&binding, &text).await?;
                    }
                    ImBindingRepository::set_delivered_sequence(
                        &conn,
                        &binding.binding_id,
                        event.sequence,
                        unix_timestamp_ms(),
                    )?;
                }
                TimelinePayload::PermissionRequest(request)
                    if request.status == PermissionRequestStatus::Pending =>
                {
                    self.send_approval_card(&binding, request, event.sequence)
                        .await?;
                }
                TimelinePayload::PermissionResolution(resolution) => {
                    self.update_approval_cards(
                        &resolution.session_id,
                        resolution.request_id.as_str(),
                        resolution.response,
                    )
                    .await?;
                }
                TimelinePayload::Error(error) => {
                    let text = render::error_message(&error.code, &error.message);
                    self.send_to_binding(&binding, &text).await?;
                    ImBindingRepository::set_delivered_sequence(
                        &conn,
                        &binding.binding_id,
                        event.sequence,
                        unix_timestamp_ms(),
                    )?;
                }
                TimelinePayload::ElicitationRequest(request) => {
                    self.send_to_binding(
                        &binding,
                        &format!(
                            "The Agent needs an answer: {}\nOpen Vibex to answer this question.",
                            request.message
                        ),
                    )
                    .await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn handle_session_update(&self, session: AgentSession) -> VibexResult<()> {
        let conn = self.open()?;
        let bindings = ImBindingRepository::list_active_for_session(&conn, &session.id)?;
        for binding in bindings {
            if binding.title.as_deref() == Some(session.title.as_str()) {
                continue;
            }
            ImBindingRepository::update_title(
                &conn,
                &binding.binding_id,
                &session.title,
                unix_timestamp_ms(),
            )?;
            if binding.title_sync_enabled
                && binding.thread_kind.as_deref() == Some("telegram_forum_topic")
                && !binding.thread_key.is_empty()
                && let Some(runtime) = self
                    .inner
                    .channels
                    .read()
                    .await
                    .get(&binding.channel_id)
                    .cloned()
            {
                let _ = runtime
                    .adapter
                    .edit_thread_title(&binding.thread_key, &topic_title(&session.title))
                    .await;
            }
        }
        Ok(())
    }

    async fn handle_notification(&self, notification: AgentNotificationIntent) -> VibexResult<()> {
        if let AgentNotificationKind::ApprovalRequired { request_id } = notification.kind {
            let conn = self.open()?;
            let bindings =
                ImBindingRepository::list_active_for_session(&conn, &notification.session_id)?;
            for binding in bindings {
                if let Some(request) = self
                    .pending_permission(&notification.session_id, Some(request_id.as_str()))
                    .await?
                {
                    self.send_approval_card(&binding, &request, 0).await?;
                }
            }
        }
        Ok(())
    }

    pub(super) async fn pending_permission(
        &self,
        session_id: &VibexSessionId,
        request_id: Option<&str>,
    ) -> VibexResult<Option<PermissionRequest>> {
        let page = self
            .inner
            .agent
            .fetch_timeline(FetchTimelineRequest {
                session_id: session_id.clone(),
                after_sequence: None,
                before_sequence: None,
                limit: 100,
                max_turns: None,
                max_bytes: None,
            })
            .await?;
        for item in page.items.iter().rev() {
            if let TimelinePayload::PermissionRequest(request) = &item.payload
                && request.status == PermissionRequestStatus::Pending
                && request_id.is_none_or(|id| request.id.as_str() == id)
            {
                return Ok(Some(request.clone()));
            }
        }
        Ok(None)
    }
}
