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
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                            // Dropping these silently is what makes a busy runtime
                            // "go quiet" with no way to tell it happened. Every
                            // bound chat gets one notice and a re-sync instead.
                            tracing::warn!(
                                target: "vibex_im",
                                skipped = count,
                                "IM timeline subscription fell behind"
                            );
                            let _ = service.recover_from_lag().await;
                        }
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

    /// Brings every bound chat back to a state the user can act on.
    ///
    /// A restart loses the in-memory broadcast ring, and a crash between writing
    /// an approval card and sending it leaves a card the user never saw. Both
    /// are reconciled here: missed final answers are counted against the stored
    /// delivery watermark, undelivered approval cards are re-sent, and a single
    /// line tells the chat what happened instead of resuming in silence.
    pub(super) async fn reconcile_on_start(&self) -> VibexResult<()> {
        let conn = self.open()?;
        for record in ImChannelRepository::list(&conn)? {
            if !record.enabled {
                continue;
            }
            let locale = self.locale_for_channel(&record.channel_id);
            let mut resend = Vec::new();
            let mut missed_total = 0_usize;
            for binding in ImBindingRepository::list_for_channel(&conn, &record.channel_id)?
                .into_iter()
                .filter(|binding| binding.status == ImBindingStatus::Active)
            {
                let page = self
                    .inner
                    .agent
                    .fetch_timeline(FetchTimelineRequest {
                        session_id: binding.session_id.clone(),
                        after_sequence: Some(binding.last_delivered_sequence),
                        before_sequence: None,
                        limit: 100,
                        max_turns: None,
                        max_bytes: None,
                    })
                    .await;
                let mut missed = 0_usize;
                let mut newest_sequence = binding.last_delivered_sequence;
                if let Ok(page) = page {
                    for item in &page.items {
                        if let TimelinePayload::AgentMessage(message) = &item.payload
                            && message.is_final
                            && !message.text.trim().is_empty()
                        {
                            missed = missed.saturating_add(1);
                        }
                        if let TimelinePayload::PermissionRequest(request) = &item.payload
                            && request.status == PermissionRequestStatus::Pending
                        {
                            resend.push((binding.clone(), request.clone()));
                        }
                        newest_sequence = newest_sequence.max(item.sequence);
                    }
                }
                if missed > 0 {
                    missed_total = missed_total.saturating_add(missed);
                    ImBindingRepository::set_delivered_sequence(
                        &conn,
                        &binding.binding_id,
                        newest_sequence,
                        unix_timestamp_ms(),
                    )?;
                }
            }
            if missed_total > 0 || !resend.is_empty() {
                let notice = render::resume_notice(locale, missed_total);
                let target_binding =
                    resend
                        .first()
                        .map(|(binding, _)| binding.clone())
                        .or_else(|| {
                            ImBindingRepository::list_for_channel(&conn, &record.channel_id)
                                .ok()
                                .and_then(|bindings| {
                                    bindings
                                        .into_iter()
                                        .find(|binding| binding.status == ImBindingStatus::Active)
                                })
                        });
                if let Some(binding) = target_binding {
                    let _ = self.send_to_binding(&binding, notice).await;
                }
            }
            for (binding, request) in resend {
                self.send_approval_card(&binding, &request, 0).await?;
            }
        }
        Ok(())
    }

    /// Tells every bound chat that live updates were missed and re-sends the
    /// state they can act on.
    ///
    /// A successful turn is not replayed: the message text is gone from the
    /// broadcast ring, and the desktop timeline is the place to read it. What is
    /// re-sent is what a user would otherwise be stuck on — a pending approval
    /// whose card never arrived, and a one-line notice that replies may be
    /// incomplete.
    async fn recover_from_lag(&self) -> VibexResult<()> {
        let conn = self.open()?;
        let channels = ImChannelRepository::list(&conn)?;
        for record in channels {
            if !record.enabled {
                continue;
            }
            let bindings = ImBindingRepository::list_for_channel(&conn, &record.channel_id)?;
            for binding in bindings
                .into_iter()
                .filter(|binding| binding.status == ImBindingStatus::Active)
            {
                let notice = render::lag_notice(self.locale_for_channel(&record.channel_id));
                let _ = self.send_to_binding(&binding, notice).await;
                let pending =
                    ImApprovalCardRepository::list_pending_for_binding(&conn, &binding.binding_id)?;
                for card in pending {
                    if let Some(request) = self
                        .pending_permission(&binding.session_id, Some(&card.request_id))
                        .await?
                    {
                        self.send_approval_card(&binding, &request, 0).await?;
                    }
                }
            }
        }
        Ok(())
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
                    if !message.text.trim().is_empty() {
                        self.send_answer_to_binding(&binding, &message.text).await?;
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
                    self.send_elicitation(&binding, request).await?;
                    ImBindingRepository::set_delivered_sequence(
                        &conn,
                        &binding.binding_id,
                        event.sequence,
                        unix_timestamp_ms(),
                    )?;
                }
                TimelinePayload::ElicitationResolution(resolution) => {
                    self.close_elicitation(resolution.request_id.as_str())
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

    /// The pending elicitation a button refers to.
    ///
    /// It is read from the timeline rather than the action row: the timeline is
    /// the fact, and a question answered on the desktop must make the chat card
    /// stale the same way an answered approval does.
    pub(super) async fn pending_elicitation(
        &self,
        session_id: &VibexSessionId,
        request_id: &str,
    ) -> VibexResult<Option<vibex_core::ElicitationRequest>> {
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
            if let TimelinePayload::ElicitationRequest(request) = &item.payload
                && request.status == vibex_core::ElicitationRequestStatus::Pending
                && request.id.as_str() == request_id
            {
                return Ok(Some(request.clone()));
            }
        }
        Ok(None)
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
