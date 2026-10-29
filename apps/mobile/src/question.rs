//! The question an Agent parked on: the form that answers it, and the record it
//! leaves behind.
//!
//! Desktop draws an approval and an input request as cards *in* the timeline,
//! where they scroll with the conversation and their controls sit beside it. A
//! phone answers with its thumb, over the composer the keyboard already owns, so
//! mobile keeps the same semantics and moves only where the form lives: one
//! panel that covers the composer, one field per screen, and a single control
//! row that walks the fields and commits the answer.
//!
//! The row on the timeline is therefore not the form. While a request is pending
//! it says only that the Agent is waiting — tapping it brings the panel back for
//! a reader who dismissed it — and once it settles it becomes a collapsed record
//! of the question and the answers given, which the reader can open to read back
//! what they chose.

use super::*;

impl MobileApp {
    /// The row an Agent's parked question leaves on the timeline.
    ///
    /// The form itself is the panel over the composer, so the row is the record
    /// of *why* the Agent stopped: one compact, tappable line that says what is
    /// being asked and brings the panel back if the reader put it away.
    pub(super) fn render_permission_request_card(
        &self,
        row: &TimelineRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(TimelinePayload::PermissionRequest(request)) =
            self.timeline_row_latest_payload(row)
        else {
            return self.render_fallback_process_row(row);
        };
        // The row's own flag is what the runtime settles: the request keeps its
        // `Pending` status after the reader answers it, so asking the payload
        // would draw the form for a decision already made.
        if row.pending_permission {
            return self.render_pending_question_row(
                row,
                request.title.clone(),
                request.id.as_str(),
                cx,
            );
        }
        self.render_settled_approval_row(row, request, cx)
    }

    pub(super) fn render_elicitation_request_card(
        &self,
        row: &TimelineRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(TimelinePayload::ElicitationRequest(request)) =
            self.timeline_row_latest_payload(row)
        else {
            return self.render_fallback_process_row(row);
        };
        if row.pending_permission {
            let title = request
                .title
                .clone()
                .unwrap_or_else(|| locale::common("Input requested").to_string());
            return self.render_pending_question_row(row, title, request.id.as_str(), cx);
        }
        self.render_settled_elicitation_row(row, request, cx)
    }

    /// The timeline's own line for a question the Agent is still on.
    ///
    /// It is a way back to the panel rather than a second form: the reader who
    /// dismissed the sheet finds the question where they left it, and one tap
    /// reopens the same panel with the draft still in it.
    fn render_pending_question_row(
        &self,
        row: &TimelineRow,
        title: String,
        request_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let approval = row.kind == TimelineRowKind::PermissionRequest;
        let request_id = request_id.to_string();
        div()
            .id(format!("question-row:{}", row.id))
            .debug_selector({
                let selector = row.id.clone();
                move || format!("question-row:{selector}")
            })
            .w_full()
            .min_w_0()
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::accent_yellow().opacity(0.42))
            .bg(theme::bg_card())
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.reopen_question(request_id.clone(), cx)),
            )
            .child(
                svg()
                    .path(if approval {
                        "icons/shield-alert.svg"
                    } else {
                        "icons/circle-question-mark.svg"
                    })
                    .size(px(theme::ICON_SM))
                    .flex_shrink_0()
                    .text_color(theme::accent_yellow()),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(theme::font_detail())
                    .text_color(theme::text_primary())
                    .child(title),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(theme::font_micro())
                    .text_color(theme::accent_yellow())
                    .child(if approval {
                        locale::text("Waiting for confirmation", "等待确认", "等待確認")
                    } else {
                        locale::text("Waiting for your input", "等待你的输入", "等待你的輸入")
                    }),
            )
            .child(
                svg()
                    .path("icons/chevron-right.svg")
                    .size(px(theme::ICON_SM))
                    .flex_shrink_0()
                    .text_color(theme::text_muted()),
            )
            .into_any_element()
    }

    /// A settled approval, collapsed to what was decided.
    fn render_settled_approval_row(
        &self,
        row: &TimelineRow,
        request: &vibex_core::PermissionRequest,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let resolution = self.permission_resolution_for_request(request.id.as_str());
        let (response, status_label) = permission_resolution_outcome(request, resolution.as_ref());
        let status_color = match response {
            Some(PermissionResponseKind::Approve)
            | Some(PermissionResponseKind::AlwaysAllowForSession) => theme::accent_green(),
            Some(PermissionResponseKind::Deny) => theme::accent_red(),
            None => theme::text_muted(),
        };
        let expanded = self.expanded_settled_questions.contains(&row.id);
        let details_to_show = if expanded {
            request.details.len()
        } else {
            request.details.len().min(2)
        };
        let toggle_id = row.id.clone();
        div()
            .id(row.id.clone())
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::bg_card())
            .p_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(settled_status_icon(
                        response.map(settled_response_action),
                        status_color,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(theme::font_detail())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_primary())
                            .child(request.title.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(theme::font_micro())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(status_color)
                            .child(status_label),
                    )
                    .child(
                        svg()
                            .path(if expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            })
                            .size(px(theme::ICON_SM))
                            .flex_shrink_0()
                            .text_color(theme::text_muted()),
                    ),
            )
            .children(request.details.iter().take(details_to_show).map(|detail| {
                div()
                    .flex()
                    .gap_2()
                    .text_size(theme::font_caption())
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme::text_muted())
                            .child(detail.label.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_color(theme::text_secondary())
                            .child(detail.value.clone()),
                    )
            }))
            .when(request.details.len() > 2, |card| {
                card.child(
                    div()
                        .id(format!("settled-question:{}", row.id))
                        .h(px(theme::TOUCH_TARGET))
                        .flex()
                        .items_center()
                        .text_size(theme::font_caption())
                        .text_color(theme::text_muted())
                        .cursor_pointer()
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.toggle_settled_question(toggle_id.clone(), cx)
                            }),
                        )
                        .child(if expanded {
                            locale::common("Show less").to_string()
                        } else {
                            format!(
                                "{} {} {}",
                                locale::text("Show all", "显示全部", "顯示全部"),
                                request.details.len(),
                                locale::text("details", "项详情", "項詳細資料")
                            )
                        }),
                )
            })
            .into_any_element()
    }

    /// A settled input request, collapsed to the question and the answers given.
    ///
    /// The collapsed row is legible on its own — question, then what the reader
    /// chose — because that is the exchange the conversation is made of. The
    /// full question and one row per answer sit behind the disclosure.
    fn render_settled_elicitation_row(
        &self,
        row: &TimelineRow,
        request: &ElicitationRequest,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let resolution = self.elicitation_resolution_for_request(request.id.as_str());
        let (action, status_label) = elicitation_resolution_outcome(request, resolution.as_ref());
        let status_color = match action {
            Some(ElicitationResolutionAction::Accept) => theme::accent_green(),
            Some(ElicitationResolutionAction::Decline) => theme::accent_red(),
            _ => theme::text_muted(),
        };
        let title = request
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| locale::common("Input requested").to_string());
        let question = request.message.trim().to_string();
        let description = request
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty() && *description != question)
            .map(str::to_string);
        let answers = elicitation_answer_summaries(request, resolution.as_ref());
        let expanded = self.expanded_settled_questions.contains(&row.id);
        let toggle_id = row.id.clone();
        let disclosure_label = if expanded {
            locale::text("Collapse input details", "收起输入详情", "收起輸入詳情")
        } else {
            locale::text("Expand input details", "展开输入详情", "展開輸入詳情")
        };
        let header = div()
            .id(format!("settled-question:{}", row.id))
            .debug_selector({
                let selector = row.id.clone();
                move || format!("settled-question:{selector}")
            })
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .aria_label(disclosure_label)
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.toggle_settled_question(toggle_id.clone(), cx)
                }),
            )
            .child(settled_status_icon(action, status_color))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(theme::font_detail())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_primary())
                    .child(title),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(theme::font_micro())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(status_color)
                    .child(status_label),
            )
            .child(
                svg()
                    .path(if expanded {
                        "icons/chevron-down.svg"
                    } else {
                        "icons/chevron-right.svg"
                    })
                    .size(px(theme::ICON_SM))
                    .flex_shrink_0()
                    .text_color(theme::text_muted()),
            );
        let preview = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .pl(px(theme::SPACING_LG))
            .when(!question.is_empty(), |preview| {
                preview.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .text_size(theme::font_caption())
                        .text_color(theme::text_muted())
                        .child(question.clone()),
                )
            })
            .when(!answers.is_empty(), |preview| {
                preview.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .text_size(theme::font_caption())
                        .text_color(theme::text_secondary())
                        .child(elicitation_answer_preview_text(&answers)),
                )
            });
        let details = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .pl(px(theme::SPACING_LG))
            .when(!question.is_empty(), |details| {
                details.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_size(theme::font_caption())
                        .line_height(theme::line_height_for(theme::FONT_CAPTION))
                        .text_color(theme::text_primary())
                        .child(question.clone()),
                )
            })
            .when_some(description, |details, description| {
                details.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_size(theme::font_micro())
                        .line_height(theme::line_height_for(theme::FONT_MICRO))
                        .text_color(theme::text_muted())
                        .child(description),
                )
            })
            .when(!answers.is_empty(), |details| {
                details.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .border_t_1()
                        .border_color(theme::border_subtle())
                        .pt_2()
                        .children(answers.iter().map(|answer| {
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .text_size(theme::font_micro())
                                        .text_color(theme::text_muted())
                                        .child(answer.label.clone()),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .debug_selector({
                                            let field = answer.field_id.clone();
                                            move || format!("settled-answer:{field}")
                                        })
                                        .text_size(theme::font_caption())
                                        .line_height(theme::line_height_for(theme::FONT_CAPTION))
                                        .text_color(theme::text_primary())
                                        .child(answer.value.clone()),
                                )
                        })),
                )
            })
            .when(
                answers.is_empty() && action != Some(ElicitationResolutionAction::Accept),
                |details| {
                    details.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_size(theme::font_micro())
                            .text_color(theme::text_muted())
                            .child(locale::text(
                                "No input was recorded",
                                "未记录任何输入",
                                "未記錄任何輸入",
                            )),
                    )
                },
            );
        div()
            .id(row.id.clone())
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::bg_card())
            .p_3()
            .child(header)
            .child(if expanded { details } else { preview })
            .into_any_element()
    }

    /// The durable resolution that settled `request_id`, if the session kept one.
    ///
    /// A resolution is its own timeline item rather than a rewrite of the
    /// request, so a settled row reads its answers back from here. The scan runs
    /// backwards and only for settled rows — a handful per session — which keeps
    /// it off the hot path for ordinary rows.
    fn elicitation_resolution_for_request(
        &self,
        request_id: &str,
    ) -> Option<ElicitationResolution> {
        let items = &self.controller.as_ref()?.state.timeline.items;
        items.iter().rev().find_map(|item| match &item.payload {
            TimelinePayload::ElicitationResolution(resolution)
                if resolution.request_id.as_str() == request_id =>
            {
                Some(resolution.clone())
            }
            _ => None,
        })
    }

    /// The recorded response for a settled approval, the way
    /// [`Self::elicitation_resolution_for_request`] reads one for a form.
    fn permission_resolution_for_request(&self, request_id: &str) -> Option<PermissionResolution> {
        let items = &self.controller.as_ref()?.state.timeline.items;
        items.iter().rev().find_map(|item| match &item.payload {
            TimelinePayload::PermissionResolution(resolution)
                if resolution.request_id.as_str() == request_id =>
            {
                Some(resolution.clone())
            }
            _ => None,
        })
    }

    pub(super) fn toggle_settled_question(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.expanded_settled_questions.insert(id.clone()) {
            self.expanded_settled_questions.remove(&id);
        }
        self.remeasure_timeline();
        cx.notify();
    }

    /// The pending request a phone should be asking about, if any.
    ///
    /// The panel covers the composer and there is only one of those, so a phone
    /// asks one question at a time: when an Agent has parked on two, the earliest
    /// unanswered one wins, and answering it reveals the next in the order the
    /// conversation is in. A request the reader dismissed without answering is
    /// skipped here and brought back by tapping its timeline row.
    fn pending_question(&self) -> Option<PendingQuestion> {
        let controller = self.controller.as_ref()?;
        // An approval goes first: the Agent is stopped on a decision it cannot
        // take itself, and the tool the question is about is behind it.
        if let Some(approval) = controller
            .state
            .approval_surfaces(ShellKind::Compact)
            .into_iter()
            .find(|approval| {
                !self
                    .dismissed_questions
                    .contains(approval.request_id.as_str())
            })
        {
            return Some(PendingQuestion::Approval(Box::new(approval)));
        }
        controller
            .state
            .elicitation_surfaces(ShellKind::Compact)
            .into_iter()
            .map(|surface| surface.request)
            .find(|request| !self.dismissed_questions.contains(request.id.as_str()))
            .map(|request| PendingQuestion::Elicitation(Box::new(request)))
    }

    /// Every request still waiting on the reader, whatever the panel is showing.
    ///
    /// This is what keeps [`Self::dismissed_questions`] from growing forever: an
    /// id is only ever remembered while the request behind it is parked.
    fn pending_question_ids(&self) -> BTreeSet<String> {
        let Some(controller) = self.controller.as_ref() else {
            return BTreeSet::new();
        };
        controller
            .state
            .approval_surfaces(ShellKind::Compact)
            .into_iter()
            .map(|approval| approval.request_id.to_string())
            .chain(
                controller
                    .state
                    .elicitation_surfaces(ShellKind::Compact)
                    .into_iter()
                    .map(|surface| surface.request.id.to_string()),
            )
            .collect()
    }

    /// Whether the question panel should be up this frame.
    pub(super) fn question_form_open(&self) -> bool {
        self.pending_question().is_some()
    }

    /// Keeps the pager's own state in step with the request on screen.
    ///
    /// Rendered once per frame, where the pending set is already being read for
    /// the form's own sync: an id that is no longer waiting is forgotten, and a
    /// page left past the end of a shorter request is pulled back to the last
    /// field it actually has.
    pub(super) fn sync_question_form(&mut self) {
        let pending = self.pending_question_ids();
        self.dismissed_questions.retain(|id| pending.contains(id));
        let fields = self
            .pending_question()
            .as_ref()
            .map(PendingQuestion::field_count)
            .unwrap_or(1);
        self.question_form_step = self.question_form_step.min(fields.saturating_sub(1));
    }

    /// Brings back a question the reader dismissed without answering.
    pub(super) fn reopen_question(&mut self, request_id: String, cx: &mut Context<Self>) {
        if self.dismissed_questions.remove(&request_id) {
            self.question_form_step = 0;
            cx.notify();
        }
    }

    /// The panel that asks for the Agent's parked question.
    ///
    /// It is a bottom sheet like every other panel in the app, so it rises over
    /// the composer with the same scrim, grabber and pull-to-dismiss a reader
    /// already knows. Its body is the pager; its foot is the control row, which
    /// is the only place a page commits anything.
    pub(super) fn render_question_sheet(
        &self,
        presentation: SheetPresentation,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(question) = self.pending_question() else {
            return div().into_any_element();
        };
        let (title, status) = question.header();
        let progress = self.question_progress(&question);
        let body = match &question {
            PendingQuestion::Approval(approval) => self.render_approval_question(approval, cx),
            PendingQuestion::Elicitation(request) => self.render_elicitation_question(request, cx),
        };
        bottom_sheet_shell("mobile-agent-question", presentation)
            .on_dismiss(cx.listener(|this, _, _, cx| this.dismiss_question_sheet(cx)))
            .on_pan(cx.listener(|this, event, window, cx| {
                this.pull_sheet(SheetKey::AgentQuestion, event, window, cx)
            }))
            .content(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(question_header(title, progress, status))
                    .child(
                        div()
                            .id("mobile-agent-question-body")
                            .min_h_0()
                            .flex_1()
                            .px(px(theme::SPACING_MD))
                            .pb(px(theme::SPACING_SM))
                            .flex()
                            .flex_col()
                            .gap_3()
                            // The panel owns its taps: only the scrim around it
                            // dismisses the question.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(body),
                    ),
            )
            .into_any_element()
    }

    /// Where the reader is in the pager, as the header prints it.
    fn question_progress(&self, question: &PendingQuestion) -> Option<(usize, usize)> {
        let total = question.field_count();
        (total > 1).then(|| (self.question_form_step.min(total - 1) + 1, total))
    }

    /// Puts the panel's question away without answering it.
    ///
    /// Dismissal means the same thing however the reader got there — a tap on
    /// the scrim, the back gesture, or a finger that threw the panel down the
    /// screen: nothing is sent, and the request keeps waiting on its timeline
    /// row for the reader to come back to.
    ///
    /// This is the record the page keeps of that decision, and it has to be
    /// made *before* the panel's fall starts. The panel's own flag is the
    /// pending request, which the runtime owns; a fall that lands while the
    /// request is still pending would raise the panel straight back up.
    pub(super) fn put_away_question(&mut self, cx: &mut Context<Self>) {
        if let Some(question) = self.pending_question() {
            self.dismissed_questions
                .insert(question.request_id().to_string());
        }
        cx.notify();
    }

    /// A tap on the scrim is a dismissal, not an answer: nothing is sent, and the
    /// request waits on its timeline row until the reader comes back.
    pub(super) fn dismiss_question_sheet(&mut self, cx: &mut Context<Self>) {
        self.put_away_question(cx);
        self.close_sheet(SheetKey::AgentQuestion, cx);
    }

    /// Moves the pager one field, clamped to the fields the form actually has.
    fn step_question_form(&mut self, forward: bool, fields: usize, cx: &mut Context<Self>) {
        if fields == 0 {
            return;
        }
        self.question_form_step = if forward {
            (self.question_form_step + 1).min(fields - 1)
        } else {
            self.question_form_step.saturating_sub(1)
        };
        cx.notify();
    }

    /// The approval half of the panel: what the Agent wants to do, and the
    /// decisions the runtime allows.
    fn render_approval_question(
        &self,
        approval: &vibex_ui::ApprovalSurfaceModel,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let approve_id = approval.request_id.clone();
        let deny_id = approval.request_id.clone();
        let always_id = approval.request_id.clone();
        let resolving = self.controller.as_ref().is_some_and(|controller| {
            controller
                .state
                .pending_permission_resolution(approval.request_id.as_str())
        });
        let can_approve = approval
            .allowed_responses
            .contains(&PermissionResponseKind::Approve);
        let can_deny = approval
            .allowed_responses
            .contains(&PermissionResponseKind::Deny);
        let can_always = approval
            .allowed_responses
            .contains(&PermissionResponseKind::AlwaysAllowForSession);
        let mut controls = Vec::new();
        // A decision the runtime does not allow is drawn dimmed and inert
        // rather than dropped: the choices a reader has stay where they were as
        // one request gives way to the next.
        if !resolving {
            controls.push(question_action_or_disabled(
                format!("deny:{}", approval.request_id),
                approval_response_label(
                    approval,
                    PermissionResponseKind::Deny,
                    locale::common("Deny"),
                ),
                can_deny,
                QuestionActionStyle::Secondary,
                cx.listener(move |this, _, _, cx| {
                    this.resolve_permission(deny_id.clone(), PermissionResponseKind::Deny, cx)
                }),
            ));
        }
        if !resolving && (can_always || approval.allowed_responses.len() > 2) {
            controls.push(question_action_or_disabled(
                format!("always-allow:{}", approval.request_id),
                approval_response_label(
                    approval,
                    PermissionResponseKind::AlwaysAllowForSession,
                    locale::common("Always allow"),
                ),
                can_always,
                QuestionActionStyle::Secondary,
                cx.listener(move |this, _, _, cx| {
                    this.resolve_permission(
                        always_id.clone(),
                        PermissionResponseKind::AlwaysAllowForSession,
                        cx,
                    )
                }),
            ));
        }
        controls.push(question_action_or_disabled(
            format!("approve:{}", approval.request_id),
            if resolving {
                locale::common("Resolving...").to_string()
            } else {
                approval_response_label(
                    approval,
                    PermissionResponseKind::Approve,
                    locale::common("Approve"),
                )
            },
            can_approve && !resolving,
            QuestionActionStyle::Primary,
            cx.listener(move |this, _, _, cx| {
                this.resolve_permission(approve_id.clone(), PermissionResponseKind::Approve, cx)
            }),
        ));
        div()
            .min_h_0()
            .flex_1()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(theme::font_body())
                            .line_height(theme::line_height_for(theme::FONT_BODY))
                            .text_color(theme::text_primary())
                            .child(approval.title.clone()),
                    )
                    .child(question_pill(
                        permission_risk_label(approval.risk_category).to_string(),
                    )),
            )
            .when(!approval.details.is_empty(), |body| {
                // The details are what the decision is about, so they scroll
                // under their own header rather than pushing the controls below
                // the fold of a phone.
                body.child(
                    div()
                        .id("question-approval-details")
                        .min_h_0()
                        .flex_1()
                        .overflow_y_scroll()
                        .restrict_scroll_to_axis()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(approval.details.iter().map(|(label, value)| {
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(theme::font_micro())
                                        .text_color(theme::text_muted())
                                        .child(label.clone()),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .bg(theme::bg_card_dim())
                                        .px_2()
                                        .py(px(theme::SPACING_SM))
                                        .text_size(theme::font_caption())
                                        .line_height(theme::line_height_for(theme::FONT_CAPTION))
                                        .text_color(theme::text_secondary())
                                        .whitespace_normal()
                                        .child(value.clone()),
                                )
                        })),
                )
            })
            .child(question_footer(controls).when(resolving, |footer| footer.opacity(0.6)))
            .into_any_element()
    }

    /// The input-request half of the panel: one field per screen.
    fn render_elicitation_question(
        &self,
        request: &ElicitationRequest,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let total = request.fields.len();
        let pending = self.controller.as_ref().is_some_and(|controller| {
            controller
                .state
                .pending_elicitation_resolution(request.id.as_str())
        });
        let step = self.question_form_step.min(total.saturating_sub(1));
        let Some(field) = request.fields.get(step) else {
            return div().into_any_element();
        };
        let decline_id = request.id.clone();
        let accept_id = request.id.clone();
        let mut controls = Vec::new();
        // A resolution in flight freezes the form: the answer is already on its
        // way, and walking to another field would only edit a payload the
        // runtime is about to answer.
        if step > 0 {
            controls.push(question_action(
                format!("question-previous:{}", request.id),
                locale::text("Previous", "上一步", "上一步").to_string(),
                QuestionActionStyle::Quiet,
                cx.listener(move |this, _, _, cx| this.step_question_form(false, total, cx)),
            ));
        }
        controls.push(question_action(
            format!("decline-elicitation:{}", request.id),
            locale::common("Decline").to_string(),
            QuestionActionStyle::Secondary,
            cx.listener(move |this, _, _, cx| {
                this.resolve_elicitation(
                    decline_id.clone(),
                    ElicitationResolutionAction::Decline,
                    cx,
                )
            }),
        ));
        controls.push(if pending {
            question_action(
                format!("accept-elicitation:{}", request.id),
                locale::common("Submitting...").to_string(),
                QuestionActionStyle::Primary,
                cx.listener(|_, _, _, _| {}),
            )
        } else if step + 1 < total {
            question_action(
                format!("question-next:{}", request.id),
                locale::text("Next", "下一步", "下一步").to_string(),
                QuestionActionStyle::Primary,
                cx.listener(move |this, _, _, cx| this.step_question_form(true, total, cx)),
            )
        } else {
            question_action(
                format!("accept-elicitation:{}", request.id),
                locale::common("Submit").to_string(),
                QuestionActionStyle::Primary,
                cx.listener(move |this, _, _, cx| {
                    this.resolve_elicitation(
                        accept_id.clone(),
                        ElicitationResolutionAction::Accept,
                        cx,
                    )
                }),
            )
        });
        div()
            .min_h_0()
            .flex_1()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .id("question-elicitation-body")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .flex()
                    .flex_col()
                    .gap_2()
                    // The Agent's own message is why the form exists; it stays
                    // above the field rather than repeating in every step.
                    .when(!request.message.trim().is_empty(), |body| {
                        body.child(question_context(request.message.trim()))
                    })
                    .child(question_field_heading(field))
                    .child(self.render_question_field(request, field, cx)),
            )
            .child(question_footer(controls).when(pending, |footer| footer.opacity(0.6)))
            .into_any_element()
    }

    /// One field's control, in the shape the field asks for.
    fn render_question_field(
        &self,
        request: &ElicitationRequest,
        field: &ElicitationField,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match &field.kind {
            ElicitationFieldKind::Text { options, .. } if !options.is_empty() => {
                self.render_question_options(request, field, options, cx)
            }
            ElicitationFieldKind::Text { .. }
            | ElicitationFieldKind::Number { .. }
            | ElicitationFieldKind::Integer { .. } => self
                .elicitation_inputs
                .get(&(request.id.as_str().to_string(), field.id.clone()))
                .cloned()
                .map(|input| {
                    div()
                        .min_h(px(theme::TOUCH_TARGET))
                        .rounded(px(theme::RADIUS_CONTROL))
                        .border_1()
                        .border_color(theme::border_default())
                        .bg(theme::bg_card_dim())
                        .px_2()
                        .py_1()
                        .flex()
                        .items_center()
                        .on_mouse_down(MouseButton::Left, resume_keyboard_on_tap)
                        .child(Input::new(&input).appearance(false))
                        .into_any_element()
                })
                .unwrap_or_else(|| div().into_any_element()),
            ElicitationFieldKind::Boolean { .. } => {
                self.render_question_boolean(request, field, cx)
            }
            ElicitationFieldKind::MultiSelect { options, .. } => {
                self.render_question_multi_options(request, field, options, cx)
            }
            ElicitationFieldKind::Unsupported { schema_type } => div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(theme::font_caption())
                .text_color(theme::accent_red())
                .child(
                    svg()
                        .path("icons/triangle-alert.svg")
                        .size(px(theme::ICON_SM))
                        .flex_shrink_0()
                        .text_color(theme::accent_red()),
                )
                .child(format!(
                    "{}: {schema_type}",
                    locale::text(
                        "Unsupported input type",
                        "不支持的输入类型",
                        "不支援的輸入類型"
                    )
                ))
                .into_any_element(),
        }
    }

    fn render_question_options(
        &self,
        request: &ElicitationRequest,
        field: &ElicitationField,
        options: &[ElicitationOption],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = self
            .elicitation_drafts
            .get(request.id.as_str())
            .and_then(|draft| draft.text(&field.id))
            .map(str::to_string);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(options.iter().map(|option| {
                let request_id = request.id.clone();
                let field_id = field.id.clone();
                let value = option.value.clone();
                question_choice(
                    format!("elicitation:{}:{}", field.id, option.value),
                    &option.title,
                    option.description.as_deref(),
                    selected.as_deref() == Some(option.value.as_str()),
                    false,
                    cx.listener(move |this, _, _, cx| {
                        this.set_elicitation_option(
                            request_id.clone(),
                            field_id.clone(),
                            value.clone(),
                            cx,
                        )
                    }),
                )
            }))
            .into_any_element()
    }

    fn render_question_multi_options(
        &self,
        request: &ElicitationRequest,
        field: &ElicitationField,
        options: &[ElicitationOption],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(options.iter().map(|option| {
                let request_id = request.id.clone();
                let field_id = field.id.clone();
                let value = option.value.clone();
                let selected = self
                    .elicitation_drafts
                    .get(request.id.as_str())
                    .is_some_and(|draft| draft.multi_selected(&field.id, &option.value));
                question_choice(
                    format!("elicitation-multi:{}:{}", field.id, option.value),
                    &option.title,
                    option.description.as_deref(),
                    selected,
                    true,
                    cx.listener(move |this, _, _, cx| {
                        this.toggle_elicitation_multi_option(
                            request_id.clone(),
                            field_id.clone(),
                            value.clone(),
                            cx,
                        )
                    }),
                )
            }))
            .into_any_element()
    }

    fn render_question_boolean(
        &self,
        request: &ElicitationRequest,
        field: &ElicitationField,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = self
            .elicitation_drafts
            .get(request.id.as_str())
            .and_then(|draft| draft.boolean(&field.id));
        div()
            .flex()
            .gap_2()
            .children(
                [(false, locale::common("No")), (true, locale::common("Yes"))]
                    .into_iter()
                    .map(|(value, label)| {
                        let request_id = request.id.clone();
                        let field_id = field.id.clone();
                        question_choice(
                            format!("elicitation:{}:{value}", field.id),
                            label,
                            None,
                            selected == Some(value),
                            false,
                            cx.listener(move |this, _, _, cx| {
                                this.set_elicitation_boolean(
                                    request_id.clone(),
                                    field_id.clone(),
                                    value,
                                    cx,
                                )
                            }),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// The question a phone is currently asking.
enum PendingQuestion {
    Approval(Box<vibex_ui::ApprovalSurfaceModel>),
    Elicitation(Box<ElicitationRequest>),
}

impl PendingQuestion {
    fn request_id(&self) -> &RequestId {
        match self {
            PendingQuestion::Approval(approval) => &approval.request_id,
            PendingQuestion::Elicitation(request) => &request.id,
        }
    }

    /// How many pages the pager has. An approval is one page by definition: it
    /// asks for a decision, not for fields.
    fn field_count(&self) -> usize {
        match self {
            PendingQuestion::Approval(_) => 1,
            PendingQuestion::Elicitation(request) => request.fields.len(),
        }
    }

    fn header(&self) -> (String, Option<String>) {
        match self {
            PendingQuestion::Approval(approval) => (
                locale::common("Approval").to_string(),
                Some(permission_risk_label(approval.risk_category).to_string()),
            ),
            PendingQuestion::Elicitation(request) => (
                request
                    .title
                    .clone()
                    .unwrap_or_else(|| locale::common("Input requested").to_string()),
                None,
            ),
        }
    }
}

/// The panel's own header: what is being asked, where the reader is, and the risk
/// an approval carries.
fn question_header(
    title: String,
    progress: Option<(usize, usize)>,
    status: Option<String>,
) -> gpui::AnyElement {
    div()
        .flex_shrink_0()
        .px(px(theme::SPACING_MD))
        .pb(px(theme::SPACING_SM))
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_size(theme::font_heading())
                .text_color(theme::text_primary())
                .child(title),
        )
        .children(status.map(question_pill))
        .children(progress.map(|(current, total)| {
            div()
                .flex_shrink_0()
                .text_size(theme::font_micro())
                .text_color(theme::text_muted())
                .child(format!("{current}/{total}"))
        }))
        .into_any_element()
}

fn question_pill(label: String) -> gpui::AnyElement {
    div()
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_PILL))
        .bg(theme::accent_yellow().opacity(0.14))
        .px_2()
        .py(px(2.0))
        .text_size(theme::font_micro())
        .text_color(theme::accent_yellow())
        .child(label)
        .into_any_element()
}

/// The Agent's own words, above the field they are asking about.
fn question_context(message: &str) -> gpui::AnyElement {
    div()
        .w_full()
        .min_w_0()
        .text_size(theme::font_caption())
        .line_height(theme::line_height_for(theme::FONT_CAPTION))
        .text_color(theme::text_secondary())
        .whitespace_normal()
        .child(message.to_string())
        .into_any_element()
}

/// One field's own question: its title, its optional help, and the required mark.
/// The mark rides the label rather than the control, so a reader knows what the
/// form insists on before they try to submit it.
fn question_field_heading(field: &ElicitationField) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(theme::font_body())
                .line_height(theme::line_height_for(theme::FONT_BODY))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_primary())
                .child(if field.required {
                    format!("{} *", field.title)
                } else {
                    field.title.clone()
                }),
        )
        .when_some(field.description.clone(), |heading, description| {
            heading.child(
                div()
                    .text_size(theme::font_caption())
                    .line_height(theme::line_height_for(theme::FONT_CAPTION))
                    .text_color(theme::text_muted())
                    .child(description),
            )
        })
        .into_any_element()
}

/// A selectable answer: a whole row, so the target is the row rather than the
/// word inside it.
fn question_choice(
    id: String,
    title: &str,
    description: Option<&str>,
    selected: bool,
    multi: bool,
    on_select: impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static,
) -> gpui::AnyElement {
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .min_h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(if selected {
            theme::primary()
        } else {
            theme::border_default()
        })
        .when(selected, |choice| choice.bg(theme::primary().opacity(0.10)))
        .px_3()
        .py(px(theme::SPACING_SM))
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .on_mouse_up(MouseButton::Left, on_select)
        .child(
            div()
                .size(px(theme::ICON_MD))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(if multi { 4.0 } else { theme::ICON_MD / 2.0 }))
                .border_1()
                .border_color(if selected {
                    theme::primary()
                } else {
                    theme::border_default()
                })
                .when(selected, |mark| mark.bg(theme::primary()))
                .when(selected, |mark| {
                    mark.child(
                        svg()
                            .path("icons/check.svg")
                            .size(px(theme::ICON_SM))
                            .text_color(theme::primary_foreground()),
                    )
                }),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .min_w_0()
                        .text_size(theme::font_body())
                        .text_color(theme::text_primary())
                        .child(title.to_string()),
                )
                .when_some(description.map(str::to_string), |labels, description| {
                    labels.child(
                        div()
                            .min_w_0()
                            .text_size(theme::font_micro())
                            .text_color(theme::text_muted())
                            .child(description),
                    )
                }),
        )
        .into_any_element()
}

/// How a control in the panel's foot is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QuestionActionStyle {
    /// The one decision the page is asking for.
    Primary,
    /// A decision that is available but not the point of the page.
    Secondary,
    /// Walking the form rather than answering it.
    Quiet,
}

/// A control in the panel's foot.
///
/// Both halves of the question share one foot, so every action has the same
/// shape: full-height, thumb-wide, and always present — a control that
/// disappeared as the reader moved between fields would move the commitment out
/// from under the thumb reaching for it.
fn question_action(
    id: String,
    label: String,
    style: QuestionActionStyle,
    on_press: impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static,
) -> gpui::AnyElement {
    question_action_or_disabled(id, label, true, style, on_press)
}

/// [`question_action`], with the one exception the foot has: a decision the
/// runtime does not allow is drawn dimmed and inert rather than dropped, so the
/// choices the reader has do not move as the request they are looking at changes.
fn question_action_or_disabled(
    id: String,
    label: String,
    enabled: bool,
    style: QuestionActionStyle,
    on_press: impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static,
) -> gpui::AnyElement {
    let primary = style == QuestionActionStyle::Primary;
    let quiet = style == QuestionActionStyle::Quiet;
    let text_color = if primary {
        theme::primary_foreground()
    } else if quiet {
        theme::text_muted()
    } else {
        theme::text_primary()
    };
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(if primary {
            theme::primary()
        } else if quiet {
            theme::border_subtle()
        } else {
            theme::border_default()
        })
        .when(primary, |control| control.bg(theme::primary()))
        .when(!primary && !quiet, |control| {
            control.bg(theme::bg_card_dim())
        })
        .when(!enabled, |control| control.opacity(0.45))
        .flex()
        .items_center()
        .justify_center()
        .px_2()
        .when(enabled, |control| {
            control
                .cursor_pointer()
                .active(|style| style.opacity(0.82))
                .on_mouse_up(MouseButton::Left, on_press)
        })
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(theme::font_caption())
                .font_weight(FontWeight::MEDIUM)
                .text_color(text_color)
                .child(label),
        )
        .into_any_element()
}

/// The panel's foot: the controls a reader answers with, in the order a thumb
/// reads them — the way out on the left, the commitment on the right.
fn question_footer(controls: Vec<gpui::AnyElement>) -> gpui::Div {
    div()
        .flex_shrink_0()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .children(controls)
}

/// The outcome of a settled approval, as its timeline row reports it.
fn permission_resolution_outcome(
    request: &vibex_core::PermissionRequest,
    resolution: Option<&PermissionResolution>,
) -> (Option<PermissionResponseKind>, &'static str) {
    if let Some(resolution) = resolution {
        return (
            Some(resolution.response),
            permission_resolution_label(resolution.response),
        );
    }
    match request.status {
        vibex_core::PermissionRequestStatus::Approved => (
            Some(PermissionResponseKind::Approve),
            permission_resolution_label(PermissionResponseKind::Approve),
        ),
        vibex_core::PermissionRequestStatus::Denied => (
            Some(PermissionResponseKind::Deny),
            permission_resolution_label(PermissionResponseKind::Deny),
        ),
        vibex_core::PermissionRequestStatus::Expired => {
            (None, locale::text("Expired", "已过期", "已過期"))
        }
        vibex_core::PermissionRequestStatus::Pending => {
            (None, locale::text("Resolved", "已完成", "已完成"))
        }
    }
}

fn permission_resolution_label(response: PermissionResponseKind) -> &'static str {
    match response {
        PermissionResponseKind::Approve => locale::common("Approve"),
        PermissionResponseKind::Deny => locale::common("Deny"),
        PermissionResponseKind::AlwaysAllowForSession => locale::common("Always allow"),
    }
}

/// The settled state of an elicitation as the timeline reports it: the durable
/// resolution action when the session kept one, otherwise the request status.
fn elicitation_resolution_outcome(
    request: &ElicitationRequest,
    resolution: Option<&ElicitationResolution>,
) -> (Option<ElicitationResolutionAction>, &'static str) {
    match resolution.map(|resolution| resolution.action) {
        Some(action) => (Some(action), elicitation_resolution_label(action)),
        None => match request.status {
            vibex_core::ElicitationRequestStatus::Accepted => (
                Some(ElicitationResolutionAction::Accept),
                elicitation_resolution_label(ElicitationResolutionAction::Accept),
            ),
            vibex_core::ElicitationRequestStatus::Declined => (
                Some(ElicitationResolutionAction::Decline),
                elicitation_resolution_label(ElicitationResolutionAction::Decline),
            ),
            vibex_core::ElicitationRequestStatus::Cancelled => (
                Some(ElicitationResolutionAction::Cancel),
                elicitation_resolution_label(ElicitationResolutionAction::Cancel),
            ),
            vibex_core::ElicitationRequestStatus::Pending => {
                (None, locale::text("Resolved", "已完成", "已完成"))
            }
        },
    }
}

fn elicitation_resolution_label(action: ElicitationResolutionAction) -> &'static str {
    match action {
        ElicitationResolutionAction::Accept => locale::text("Submitted", "已提交", "已提交"),
        ElicitationResolutionAction::Decline => locale::text("Declined", "已拒绝", "已拒絕"),
        ElicitationResolutionAction::Cancel => locale::text("Cancelled", "已取消", "已取消"),
    }
}

/// The mark a settled row wears for its outcome.
///
/// The label beside it says the same thing in words, so the icon is never the
/// only carrier.
fn settled_status_icon(
    action: Option<ElicitationResolutionAction>,
    color: gpui::Hsla,
) -> gpui::AnyElement {
    let path = match action {
        Some(ElicitationResolutionAction::Accept) => "icons/check.svg",
        Some(ElicitationResolutionAction::Decline) => "icons/x.svg",
        Some(ElicitationResolutionAction::Cancel) | None => "icons/minus.svg",
    };
    svg()
        .path(path)
        .size(px(theme::ICON_SM))
        .flex_shrink_0()
        .text_color(color)
        .into_any_element()
}

/// An approval's response kind as the settled-icon vocabulary a form uses.
fn settled_response_action(response: PermissionResponseKind) -> ElicitationResolutionAction {
    match response {
        PermissionResponseKind::Approve | PermissionResponseKind::AlwaysAllowForSession => {
            ElicitationResolutionAction::Accept
        }
        PermissionResponseKind::Deny => ElicitationResolutionAction::Decline,
    }
}

/// One settled answer, ready to paint.
struct ElicitationAnswerSummary {
    /// The field the answer belongs to, so a rendered answer is addressable
    /// without depending on the words in it.
    field_id: String,
    label: String,
    /// Human-readable value, for the collapsed one-line preview.
    value: String,
}

fn elicitation_field_options(kind: &ElicitationFieldKind) -> &[ElicitationOption] {
    match kind {
        ElicitationFieldKind::Text { options, .. }
        | ElicitationFieldKind::MultiSelect { options, .. } => options,
        ElicitationFieldKind::Number { .. }
        | ElicitationFieldKind::Integer { .. }
        | ElicitationFieldKind::Boolean { .. }
        | ElicitationFieldKind::Unsupported { .. } => &[],
    }
}

/// The fields a settled request actually recorded answers for, in form order.
///
/// Optional fields the user left empty have no answer entry, and a declined or
/// cancelled request has none at all — the caller decides how to say so.
fn elicitation_answer_summaries(
    request: &ElicitationRequest,
    resolution: Option<&ElicitationResolution>,
) -> Vec<ElicitationAnswerSummary> {
    let Some(resolution) = resolution else {
        return Vec::new();
    };
    request
        .fields
        .iter()
        .filter_map(|field| {
            let answer = resolution.answers.get(&field.id)?;
            Some(elicitation_answer_summary(field, answer))
        })
        .collect()
}

fn elicitation_answer_summary(
    field: &ElicitationField,
    answer: &ElicitationAnswerValue,
) -> ElicitationAnswerSummary {
    let options = elicitation_field_options(&field.kind);
    let option_title = |value: &str| {
        options
            .iter()
            .find(|option| option.value == value)
            .map(|option| option.title.clone())
            .unwrap_or_else(|| value.to_string())
    };
    let value = match answer {
        ElicitationAnswerValue::String(value) => option_title(value),
        ElicitationAnswerValue::Integer(value) => value.to_string(),
        ElicitationAnswerValue::Number(value) => value.clone(),
        ElicitationAnswerValue::Boolean(value) => if *value {
            locale::common("Yes")
        } else {
            locale::common("No")
        }
        .to_string(),
        ElicitationAnswerValue::StringArray(values) => {
            let titles = values
                .iter()
                .map(|value| option_title(value))
                .collect::<Vec<_>>();
            if titles.is_empty() {
                locale::text("None", "未选择", "未選擇").to_string()
            } else {
                titles.join(locale::text(", ", "、", "、"))
            }
        }
    };
    ElicitationAnswerSummary {
        field_id: field.id.clone(),
        label: field.title.clone(),
        value: if value.trim().is_empty() {
            "—".to_string()
        } else {
            value
        },
    }
}

fn elicitation_answer_preview_text(answers: &[ElicitationAnswerSummary]) -> String {
    answers
        .iter()
        .map(|answer| format!("{}: {}", answer.label, answer.value))
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::ElicitationRequestStatus;

    fn request(fields: Vec<ElicitationField>) -> ElicitationRequest {
        ElicitationRequest {
            id: RequestId::new(),
            session_id: VibexSessionId::new(),
            provider_request_id: None,
            tool_call_id: None,
            message: "Which approach?".into(),
            title: None,
            description: None,
            fields,
            status: ElicitationRequestStatus::Pending,
            requested_at_ms: 1,
        }
    }

    fn text_field(id: &str, required: bool) -> ElicitationField {
        ElicitationField {
            id: id.into(),
            title: format!("{id} title"),
            description: None,
            required,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: None,
                options: Vec::new(),
            },
        }
    }

    /// A settled request keeps its `Pending` status — the resolution is its own
    /// timeline item — so the record has to come from the answers, not the
    /// status, or a reader would see their own choice reported as unresolved.
    #[test]
    fn a_settled_form_reads_its_answers_out_of_the_resolution() {
        let request = request(vec![text_field("approach", true)]);
        let resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            action: ElicitationResolutionAction::Accept,
            answers: BTreeMap::from([(
                "approach".into(),
                ElicitationAnswerValue::String("global".into()),
            )]),
            responder_device_id: None,
            resolved_at_ms: 2,
        };
        let (action, label) = elicitation_resolution_outcome(&request, Some(&resolution));
        assert_eq!(action, Some(ElicitationResolutionAction::Accept));
        assert_eq!(label, locale::text("Submitted", "已提交", "已提交"));
        let answers = elicitation_answer_summaries(&request, Some(&resolution));
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].value, "global");
    }

    /// A declined request records no answers, so the row says nothing was filled
    /// in rather than showing an empty form.
    #[test]
    fn a_declined_form_records_no_answers() {
        let request = request(vec![text_field("approach", true)]);
        let resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            action: ElicitationResolutionAction::Decline,
            answers: BTreeMap::new(),
            responder_device_id: None,
            resolved_at_ms: 2,
        };
        assert!(elicitation_answer_summaries(&request, Some(&resolution)).is_empty());
        let (action, label) = elicitation_resolution_outcome(&request, Some(&resolution));
        assert_eq!(action, Some(ElicitationResolutionAction::Decline));
        assert_eq!(label, locale::text("Declined", "已拒绝", "已拒絕"));
    }

    /// The answer a reader picked by value is recorded by title, so the record
    /// reads the way the choice did.
    #[test]
    fn an_option_answer_reads_as_its_choice_title() {
        let request = request(vec![ElicitationField {
            id: "approach".into(),
            title: "Approach".into(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: None,
                options: vec![ElicitationOption {
                    value: "global".into(),
                    title: "Global state".into(),
                    description: None,
                }],
            },
        }]);
        let resolution = ElicitationResolution {
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            action: ElicitationResolutionAction::Accept,
            answers: BTreeMap::from([(
                "approach".into(),
                ElicitationAnswerValue::String("global".into()),
            )]),
            responder_device_id: None,
            resolved_at_ms: 2,
        };
        let answers = elicitation_answer_summaries(&request, Some(&resolution));
        assert_eq!(answers[0].value, "Global state");
        assert_eq!(answers[0].label, "Approach");
    }

    /// A request with no resolution of its own falls back to the status the
    /// runtime recorded, which is the only thing a truncated timeline carries.
    #[test]
    fn a_settled_approval_without_a_resolution_falls_back_to_its_status() {
        let mut request = vibex_core::PermissionRequest {
            id: RequestId::new(),
            session_id: VibexSessionId::new(),
            project_id: None,
            workspace_id: None,
            provider_request_id: None,
            risk_category: PermissionRiskCategory::Command,
            title: "Run the tests".into(),
            details: Vec::new(),
            allowed_responses: vec![PermissionResponseKind::Approve],
            response_options: Vec::new(),
            status: vibex_core::PermissionRequestStatus::Denied,
            requested_at_ms: 1,
            expires_at_ms: None,
        };
        let (response, label) = permission_resolution_outcome(&request, None);
        assert_eq!(response, Some(PermissionResponseKind::Deny));
        assert_eq!(label, locale::common("Deny"));

        request.status = vibex_core::PermissionRequestStatus::Expired;
        let (response, label) = permission_resolution_outcome(&request, None);
        assert_eq!(response, None);
        assert_eq!(label, locale::text("Expired", "已过期", "已過期"));
    }

    /// Approve and always-allow share the settled mark: both are a "yes", and
    /// the label beside the mark is what tells them apart.
    #[test]
    fn approving_responses_share_one_settled_mark() {
        assert_eq!(
            settled_response_action(PermissionResponseKind::Approve),
            ElicitationResolutionAction::Accept
        );
        assert_eq!(
            settled_response_action(PermissionResponseKind::AlwaysAllowForSession),
            ElicitationResolutionAction::Accept
        );
        assert_eq!(
            settled_response_action(PermissionResponseKind::Deny),
            ElicitationResolutionAction::Decline
        );
    }
}
