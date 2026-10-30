use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use vibex_core::TimelineTurnStart;

struct LiveTimelineProbe {
    workbench: Entity<VibexWorkbench>,
}

impl Render for LiveTimelineProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let timeline = self.workbench.update(cx, |workbench, cx| {
            workbench.render_agent_workbench_for(false, window, cx)
        });
        div().w(px(600.0)).h(px(400.0)).child(timeline)
    }
}

fn live_timeline_item(sequence: i64, payload: TimelinePayload) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::parse(format!("timeline_layout-{sequence}")).unwrap(),
        session_id: VibexSessionId::parse("session_layout").unwrap(),
        sequence,
        timestamp_ms: sequence,
        source: if matches!(payload, TimelinePayload::UserMessage(_)) {
            TimelineSource::User
        } else {
            TimelineSource::Agent
        },
        kind: payload.kind(),
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: TimelineRedactionState::None,
        execution_attribution: None,
        payload_depth: vibex_core::TimelinePayloadDepth::Detail,
        payload,
    }
}

fn progress_tool(sequence: i64, tool: usize) -> TimelineItem {
    live_timeline_item(
        sequence,
        TimelinePayload::ToolCall(vibex_core::ToolCallPayload {
            tool_call_id: format!("tool-{tool}"),
            tool_name: "exec_command".into(),
            status: vibex_core::ToolCallStatus::Progress,
            summary: "Running command".into(),
            input_summary: Some("cargo check --locked".into()),
            output_summary: Some("Checking workspace".into()),
            raw_extension: None,
        }),
    )
}

fn live_tool_timeline(
    cx: &mut TestAppContext,
    windowed: bool,
) -> (Entity<VibexWorkbench>, &mut VisualTestContext) {
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| {
            // Exercise the production timeline without booting a runtime or
            // reading the developer's persisted UI preferences.
            let mut ui_state = DesktopUiStateV1::default();
            ui_state.appearance.locale = LocaleMode::En;
            ui_state.session.reasoning_display_mode = if windowed {
                ReasoningDisplayMode::Timeline
            } else {
                ReasoningDisplayMode::LatestAtBottom
            };
            ui_state.session.enhanced_command_execution_display = false;
            ui_state.session.turn_preview_rail = false;
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_id.clone());
            workbench.view_session_id = Some(session_id.clone());
            workbench
                .pending_agent_turn_session_ids
                .insert(session_id.to_string());
            let mut items = vec![live_timeline_item(
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Check the workspace".into(),
                    ..Default::default()
                }),
            )];
            for tool in 0..24 {
                if windowed {
                    items.push(live_timeline_item(
                        items.len() as i64 + 1,
                        TimelinePayload::Reasoning(vibex_core::ReasoningPayload {
                            text: format!("Check package {tool}"),
                            is_final: true,
                        }),
                    ));
                }
                items.push(progress_tool(items.len() as i64 + 1, tool));
            }
            workbench.timeline.replace_authoritative(session_id, items);
            workbench.rebuild_timeline_sizes();
            workbench.request_timeline_scroll_to_latest();
            workbench
        });
        LiveTimelineProbe { workbench }
    });
    (view.read_with(cx, |view, _| view.workbench.clone()), cx)
}

fn draw_live_timeline_frames(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
    count: usize,
) -> Vec<(Pixels, Pixels)> {
    (0..count)
        .map(|_| {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            workbench.read_with(cx, |workbench, _| {
                (
                    workbench.timeline_row_sizes[0].height,
                    workbench.timeline_scroll.offset().y,
                )
            })
        })
        .collect()
}

fn apply_live_timeline_item(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
    item: TimelineItem,
) {
    workbench.update(cx, |workbench, cx| {
        let session_id = item.session_id.clone();
        let sequence = item.sequence;
        assert!(
            workbench
                .apply_timeline_events_to_borrowed_view(
                    &session_id,
                    vec![TimelineLiveEvent {
                        session_id: session_id.clone(),
                        sequence,
                        item
                    }],
                    cx,
                )
                .0
        );
    });
}

#[gpui::test]
fn tool_progress_preserves_the_measured_timeline_extent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for windowed in [false, true] {
        let (workbench, cx) = live_tool_timeline(cx, windowed);
        for following in [true, false] {
            if !following {
                workbench.update(cx, |workbench, _| {
                    workbench.timeline_follow.set_following_bottom(false);
                    let offset = workbench.timeline_scroll.offset();
                    workbench
                        .timeline_scroll
                        .set_offset(point(offset.x, offset.y / 2.0));
                });
            }
            let frames = draw_live_timeline_frames(&workbench, cx, 16);
            let before = *frames.last().unwrap();
            assert!(frames[12..].iter().all(|frame| *frame == before));
            workbench.read_with(cx, |workbench, _| {
                assert_eq!(
                    !workbench.timeline_process_unit_heights.is_empty(),
                    windowed
                );
            });
            let end = workbench.read_with(cx, |workbench, _| {
                workbench.timeline.authoritative_end_sequence.unwrap()
            });
            for sequence in end + 1..=end + 4 {
                apply_live_timeline_item(&workbench, cx, progress_tool(sequence, 23));
                let frames = draw_live_timeline_frames(&workbench, cx, 4);
                assert!(
                    frames.iter().all(|frame| *frame == before),
                    "tool progress must not scroll unchanged content (windowed={windowed}, following={following}): {before:?} -> {frames:?}"
                );
            }
        }
    }
}

#[gpui::test]
fn tool_revisions_preserve_the_measured_timeline_extent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for windowed in [false, true] {
        let (workbench, cx) = live_tool_timeline(cx, windowed);
        let before = *draw_live_timeline_frames(&workbench, cx, 16)
            .last()
            .unwrap();
        let sequence = workbench.read_with(cx, |workbench, _| {
            workbench.timeline.authoritative_end_sequence.unwrap()
        });
        let mut revised = progress_tool(sequence, 23);
        let TimelinePayload::ToolCall(tool) = &mut revised.payload else {
            unreachable!();
        };
        tool.output_summary = Some("Checking workspace crates".into());
        apply_live_timeline_item(&workbench, cx, revised);
        let frames = draw_live_timeline_frames(&workbench, cx, 8);
        assert!(
            frames.iter().all(|frame| *frame == before),
            "a tool revision must not re-estimate unchanged geometry (windowed={windowed}): {before:?} -> {frames:?}"
        );
    }
}

#[gpui::test]
fn refreshed_tool_history_preserves_the_measured_timeline_extent(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for windowed in [false, true] {
        let (workbench, cx) = live_tool_timeline(cx, windowed);
        let before = *draw_live_timeline_frames(&workbench, cx, 16)
            .last()
            .unwrap();
        let (updates, mut items, mode) = workbench.update(cx, |workbench, _| {
            (
                workbench.begin_timeline_load(),
                workbench.timeline.items.to_vec(),
                workbench.ui_state.session.reasoning_display_mode,
            )
        });
        let item = items.last_mut().unwrap();
        let TimelinePayload::ToolCall(tool) = &mut item.payload else {
            unreachable!();
        };
        tool.output_summary = Some("Checking workspace crates".into());
        let prepared = runtime
            .block_on(timeline_loading::PreparedTimeline::prepare(
                item.session_id.clone(),
                items,
                None,
                true,
                mode,
            ))
            .unwrap();
        workbench.update(cx, |workbench, _| {
            assert!(workbench.install_prepared_timeline(prepared, &mut updates.borrow_mut()));
            workbench.rebuild_timeline_sizes();
        });
        let frames = draw_live_timeline_frames(&workbench, cx, 8);
        assert!(
            frames.iter().all(|frame| *frame == before),
            "history refresh must retain measured geometry (windowed={windowed}): {before:?} -> {frames:?}"
        );
    }
}

#[gpui::test]
fn new_tool_content_grows_the_timeline_without_reversing_follow(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for windowed in [false, true] {
        let (workbench, cx) = live_tool_timeline(cx, windowed);
        let before = *draw_live_timeline_frames(&workbench, cx, 16)
            .last()
            .unwrap();
        let sequence = workbench.read_with(cx, |workbench, _| {
            workbench.timeline.authoritative_end_sequence.unwrap() + 1
        });
        apply_live_timeline_item(&workbench, cx, progress_tool(sequence, 24));
        let mut frames = vec![before];
        frames.extend(draw_live_timeline_frames(&workbench, cx, 8));
        assert!(
            frames.last().unwrap().0 > before.0,
            "new content must acquire its full height"
        );
        assert!(
            frames
                .windows(2)
                .all(|pair| pair[1].0 >= pair[0].0 && pair[1].1 <= pair[0].1),
            "new content must not reverse bottom follow (windowed={windowed}): {frames:?}"
        );
        workbench.read_with(cx, |workbench, _| {
            assert_eq!(
                workbench.timeline_scroll.offset().y,
                -workbench.timeline_scroll.max_offset().y
            );
        });
    }
}

#[gpui::test]
fn tool_progress_stays_stable_after_returning_to_a_cached_session(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    let before = *draw_live_timeline_frames(&workbench, cx, 16)
        .last()
        .unwrap();
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let other_session = VibexSessionId::parse("session_other").unwrap();
    workbench.update(cx, |workbench, _| {
        workbench.selected_session_id = Some(other_session.clone());
        workbench.borrow_session_view(&other_session);
        workbench
            .timeline
            .replace_authoritative(other_session, Vec::new());
    });
    cx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    workbench.update(cx, |workbench, _| {
        workbench.selected_session_id = Some(session_id.clone());
        assert!(workbench.borrow_session_view(&session_id));
        workbench.refresh_borrowed_view_geometry();
    });
    let restored = draw_live_timeline_frames(&workbench, cx, 4);
    assert!(
        restored.iter().all(|frame| *frame == before),
        "restored geometry: {restored:?}"
    );
    let sequence = workbench.read_with(cx, |workbench, _| {
        workbench.timeline.authoritative_end_sequence.unwrap() + 1
    });
    apply_live_timeline_item(&workbench, cx, progress_tool(sequence, 23));
    let frames = draw_live_timeline_frames(&workbench, cx, 4);
    assert!(
        frames.iter().all(|frame| *frame == before),
        "progress after restoring: {frames:?}"
    );
}

struct TimelinePaintFrame {
    row_bounds: Bounds<Pixels>,
    scroll_offset: Point<Pixels>,
}

struct TimelineBottomFollowProbe {
    session: SessionView,
    height: f32,
    follow: bool,
    painted: Rc<RefCell<Vec<TimelinePaintFrame>>>,
}

impl Render for TimelineBottomFollowProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let height = self.height;
        self.session.timeline_row_sizes = Rc::new(vec![size(px(500.0), px(height))]);
        if self.follow {
            self.session.scroll_timeline_to_latest();
        }
        let painted = self.painted.clone();
        let scroll = self.session.timeline_scroll.clone();
        v_virtual_list(
            cx.entity(),
            "timeline-bottom-follow-probe",
            self.session.timeline_row_sizes.clone(),
            move |_, _, _, _| {
                let painted = painted.clone();
                let scroll = scroll.clone();
                vec![
                    div()
                        .debug_selector(|| "follow-turn".into())
                        .w_full()
                        .h(px(height))
                        .on_prepaint(move |bounds, _, _| {
                            painted.borrow_mut().push(TimelinePaintFrame {
                                row_bounds: bounds,
                                scroll_offset: scroll.offset(),
                            });
                        })
                        .into_any_element(),
                ]
            },
        )
        .w(px(500.0))
        .h(px(400.0))
        .py_4()
        .track_scroll(&self.session.timeline_scroll)
    }
}

#[gpui::test]
fn timeline_bottom_follow_paints_the_current_extent_in_the_same_frame(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let painted = Rc::new(RefCell::new(Vec::new()));
    let observed = painted.clone();
    let (view, cx) = cx.add_window_view(|_, _| TimelineBottomFollowProbe {
        session: SessionView::new(SessionContentWidthMode::Standard),
        height: 600.0,
        follow: false,
        painted,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    for height in [600.0, 820.0, 520.0, 940.0, 400.0, 200.0, 760.0] {
        observed.borrow_mut().clear();
        view.update(cx, |view, cx| {
            view.height = height;
            view.follow = true;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (offset, bounds, inset, max_offset) = view.read_with(cx, |view, _| {
            let session = &view.session;
            (
                session.timeline_scroll.offset().y,
                session.timeline_scroll.bounds(),
                px(session.timeline_list_padding_top_px),
                session.timeline_scroll.max_offset().y,
            )
        });
        assert_eq!(offset, -max_offset, "height {height}");
        assert!(!observed.borrow().is_empty());
        for frame in observed.borrow().iter() {
            assert_eq!(
                frame.row_bounds.top(),
                bounds.top() + inset + frame.scroll_offset.y,
                "the row and scrollbar must use the same offset at height {height}"
            );
            if max_offset > px(0.0) {
                assert_eq!(
                    frame.row_bounds.bottom(),
                    bounds.bottom() - inset,
                    "height {height}"
                );
            }
        }
    }
}

struct ReasoningTimelineProbe {
    session: SessionView,
    unit_count: usize,
    windowed_reasoning: bool,
    measured_units: Rc<RefCell<BTreeMap<usize, f32>>>,
    measured_turn: Rc<Cell<Option<f32>>>,
    offsets: Rc<RefCell<Vec<Pixels>>>,
}

#[test]
fn reasoning_toggles_retain_geometry_and_release_only_the_changed_layout() {
    for row_id in ["reasoning", "reasoning-live:active"] {
        let mut view = SessionView::new(SessionContentWidthMode::Standard);
        view.timeline_row_sizes = Rc::new(vec![size(px(500.0), px(900.0))]);
        view.timeline_measured_turn_heights
            .insert("active".into(), 900.0);
        view.timeline_measured_turn_layout_signatures
            .insert("active".into(), 1);
        let measured = TimelineProcessUnitHeight {
            revision: 1,
            layout_width: Some(500.0),
            layout_invalidated: false,
            height: 240.0,
        };
        view.timeline_process_unit_heights
            .insert("history".into(), measured);
        if row_id == "reasoning" {
            view.timeline_process_unit_heights
                .insert(row_id.into(), measured);
        }
        let sizes = view.timeline_row_sizes.clone();
        view.set_reasoning_expanded(row_id.into(), Some("active"), false);
        assert!(Rc::ptr_eq(&view.timeline_row_sizes, &sizes));
        assert_eq!(view.timeline_measured_turn_heights["active"], 900.0);
        assert_eq!(view.timeline_process_unit_heights["history"], measured);
        assert!(
            !view
                .timeline_measured_turn_layout_signatures
                .contains_key("active")
        );
        if let Some(unit) = view.timeline_process_unit_heights.get(row_id) {
            assert_eq!(
                cached_timeline_process_unit_height(Some(unit), 2, true),
                Some(240.0)
            );
            let collapsed =
                settle_timeline_process_unit_height(Some(*unit), 2, 20.0, true, Some(500.0));
            assert_eq!(
                collapsed.height, 20.0,
                "a manual collapse bypasses the streaming hold"
            );
            assert!(!collapsed.layout_invalidated);
        }
        // The first nested prepaint still reserves the old run height. Its
        // provisional fingerprint must not block the following real collapse.
        let signature = measured_timeline_layout_signature(Some(900.0), 900.0, None, Some(2));
        assert_eq!(signature, None);
        assert_eq!(
            stable_streaming_timeline_height_for_layout(
                Some(900.0),
                680.0,
                true,
                signature,
                Some(2)
            ),
            680.0
        );
        let signature = measured_timeline_layout_signature(Some(900.0), 680.0, signature, Some(2));
        assert_eq!(signature, Some(2));
        assert_eq!(
            stable_streaming_timeline_height_for_layout(
                Some(680.0),
                660.0,
                true,
                signature,
                Some(2)
            ),
            680.0
        );
    }
}

impl Render for ReasoningTimelineProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = self.session.reasoning_expansion["reasoning"];
        for (index, measured) in std::mem::take(&mut *self.measured_units.borrow_mut()) {
            let id = format!("unit-{index}");
            let id = if index + 1 == self.unit_count {
                "reasoning"
            } else {
                &id
            };
            let settled = settle_timeline_process_unit_height(
                self.session.timeline_process_unit_heights.get(id).copied(),
                1,
                measured,
                index + 1 == self.unit_count,
                Some(500.0),
            );
            self.session
                .timeline_process_unit_heights
                .insert(id.into(), settled);
        }
        if let Some(measured) = self.measured_turn.take() {
            let previous = self
                .session
                .timeline_measured_turn_heights
                .get("active")
                .copied();
            let previous_signature = self
                .session
                .timeline_measured_turn_layout_signatures
                .get("active")
                .copied();
            let signature = Some(u64::from(expanded));
            let height = stable_streaming_timeline_height_for_layout(
                previous,
                measured,
                true,
                previous_signature,
                signature,
            );
            if let Some(signature) =
                measured_timeline_layout_signature(previous, height, previous_signature, signature)
            {
                self.session
                    .timeline_measured_turn_layout_signatures
                    .insert("active".into(), signature);
            }
            self.session
                .timeline_measured_turn_heights
                .insert("active".into(), height);
            self.session.timeline_row_sizes = Rc::new(vec![size(px(500.0), px(height))]);
        }
        if self.session.timeline_follow.following_bottom {
            self.session.scroll_timeline_to_latest();
        }
        let units = Rc::new(
            (0..self.unit_count)
                .map(|index| {
                    let id = if index + 1 == self.unit_count {
                        "reasoning".into()
                    } else {
                        format!("unit-{index}")
                    };
                    TimelineProcessUnit {
                        estimated_height: self.session.timeline_process_unit_heights[&id].height,
                        id,
                        rows: index..index + 1,
                        revision: 1,
                        streaming: index + 1 == self.unit_count,
                        kind: TimelineProcessUnitKind::Row,
                    }
                })
                .collect::<Vec<_>>(),
        );
        let mut total = px(0.0);
        let origins = Arc::new(
            units
                .iter()
                .map(|unit| {
                    let origin = total;
                    total += px(unit.estimated_height + TIMELINE_PROCESS_UNIT_GAP);
                    origin
                })
                .collect::<Vec<_>>(),
        );
        total -= px(TIMELINE_PROCESS_UNIT_GAP);
        let windowed_reasoning = self.windowed_reasoning;
        let unit_count = self.unit_count;
        let measured_units = self.measured_units.clone();
        let measured_turn = self.measured_turn.clone();
        let offsets = self.offsets.clone();
        let scroll = self.session.timeline_scroll.clone();
        let row_height = self.session.timeline_row_sizes[0].height;
        let entity = cx.weak_entity();
        v_virtual_list(
            cx.entity(),
            "reasoning-timeline-probe",
            self.session.timeline_row_sizes.clone(),
            move |_, _, window, cx| {
                let build_entity = entity.clone();
                let units_for_builder = units.clone();
                let measured_units = measured_units.clone();
                let build_unit = move |index: usize, _: &mut Window, cx: &mut App| {
                    let content = if index + 1 == unit_count {
                        let control = if expanded {
                            render_reasoning_first_line_layout(
                                cx.theme().muted_foreground,
                                div().h(px(20.0)).child("Thinking").into_any_element(),
                                Some(div().h(px(280.0)).flex_none().into_any_element()),
                                windowed_reasoning.then_some(REASONING_WINDOW_HEIGHT),
                                ReasoningReveal::new("reasoning".into(), true, 1.0),
                                None,
                            )
                        } else {
                            div().w_full().h(px(20.0)).child("Thinking")
                        };
                        let entity = build_entity.clone();
                        control
                            .id("toggle-reasoning")
                            .debug_selector(|| "toggle-reasoning".into())
                            .on_click(move |_, _, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    this.measured_units
                                        .borrow_mut()
                                        .remove(&(this.unit_count - 1));
                                    this.measured_turn.set(None);
                                    this.session.set_reasoning_expanded(
                                        "reasoning".into(),
                                        Some("active"),
                                        !expanded,
                                    );
                                    cx.notify();
                                });
                            })
                            .into_any_element()
                    } else {
                        div().h(px(180.0)).flex_none().into_any_element()
                    };
                    let measured_units = measured_units.clone();
                    let entity = build_entity.clone();
                    let cached_height = units_for_builder[index].estimated_height;
                    div()
                        .w_full()
                        .flex_none()
                        .on_prepaint(move |bounds, _, cx| {
                            let height = f32::from(bounds.size.height).ceil();
                            measured_units.borrow_mut().insert(index, height);
                            if (height - cached_height).abs() >= 1.0 {
                                let _ = entity.update(cx, |_, cx| cx.notify());
                            }
                        })
                        .child(content)
                        .into_any_element()
                };
                let content = if unit_count > TIMELINE_PROCESS_RUN_FLOW_LIMIT {
                    TimelineProcessRun {
                        units: units.clone(),
                        origins: origins.clone(),
                        total_height: total,
                        pinned_unit: None,
                        hangs_from_bottom: false,
                        session_id: None,
                        entity: WeakEntity::new_invalid(),
                        build_unit: Box::new(build_unit),
                    }
                    .into_any_element()
                } else {
                    v_flex()
                        .w_full()
                        .gap(px(TIMELINE_PROCESS_UNIT_GAP))
                        .children((0..unit_count).map(|index| build_unit(index, window, cx)))
                        .into_any_element()
                };
                let measured_turn = measured_turn.clone();
                let offsets = offsets.clone();
                let scroll = scroll.clone();
                let entity = entity.clone();
                vec![
                    h_flex()
                        .w_full()
                        .h(row_height)
                        .items_start()
                        .overflow_hidden()
                        .child(
                            v_flex()
                                .w_full()
                                .flex_none()
                                .on_prepaint(move |bounds, _, cx| {
                                    let height = f32::from(bounds.size.height).ceil();
                                    measured_turn.set(Some(height));
                                    offsets.borrow_mut().push(scroll.offset().y);
                                    if (height - f32::from(row_height)).abs() >= 1.0 {
                                        let _ = entity.update(cx, |_, cx| cx.notify());
                                    }
                                })
                                .child(content)
                                .child(div().h(px(300.0)).flex_none()),
                        )
                        .into_any_element(),
                ]
            },
        )
        .w(px(500.0))
        .h(px(700.0))
        .py_4()
        .track_scroll(&self.session.timeline_scroll)
    }
}

#[gpui::test]
fn reasoning_disclosures_preserve_history_and_never_reverse_bottom_follow(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for unit_count in [4, 20] {
        for windowed_reasoning in [true, false] {
            for following_bottom in [true, false] {
                let offsets = Rc::new(RefCell::new(Vec::new()));
                let observed = offsets.clone();
                let (view, cx) = cx.add_window_view(|_, _| {
                    let mut session = SessionView::new(SessionContentWidthMode::Standard);
                    session.agent_turn_pending = true;
                    session.reasoning_expansion.insert("reasoning".into(), true);
                    for index in 0..unit_count {
                        let id = if index + 1 == unit_count {
                            "reasoning".into()
                        } else {
                            format!("unit-{index}")
                        };
                        session.timeline_process_unit_heights.insert(
                            id,
                            TimelineProcessUnitHeight {
                                revision: 1,
                                layout_width: Some(500.0),
                                layout_invalidated: false,
                                height: if index + 1 == unit_count {
                                    // Header plus the window, or the whole body.
                                    if windowed_reasoning { 152.0 } else { 300.0 }
                                } else {
                                    180.0
                                },
                            },
                        );
                    }
                    let height = session
                        .timeline_process_unit_heights
                        .values()
                        .map(|unit| unit.height)
                        .sum::<f32>()
                        + (unit_count - 1) as f32 * TIMELINE_PROCESS_UNIT_GAP
                        + 300.0;
                    session
                        .timeline_measured_turn_heights
                        .insert("active".into(), height);
                    session
                        .timeline_measured_turn_layout_signatures
                        .insert("active".into(), 1);
                    session.timeline_row_sizes = Rc::new(vec![size(px(500.0), px(height))]);
                    ReasoningTimelineProbe {
                        session,
                        unit_count,
                        windowed_reasoning,
                        measured_units: Rc::new(RefCell::new(BTreeMap::new())),
                        measured_turn: Rc::new(Cell::new(None)),
                        offsets,
                    }
                });
                cx.run_until_parked();
                view.update(cx, |view, cx| {
                    view.session
                        .timeline_follow
                        .set_following_bottom(following_bottom);
                    if !following_bottom {
                        let offset = view.session.timeline_scroll.offset().y + px(290.0);
                        view.session
                            .timeline_scroll
                            .set_offset(point(px(0.0), offset));
                    }
                    cx.notify();
                });
                cx.run_until_parked();
                for expanded in [false, true, false, true] {
                    let before =
                        view.read_with(cx, |view, _| view.session.timeline_scroll.offset().y);
                    observed.borrow_mut().clear();
                    let trigger = cx.debug_bounds("toggle-reasoning").unwrap();
                    cx.simulate_click(
                        trigger.origin + point(px(40.0), px(10.0)),
                        Modifiers::none(),
                    );
                    // Nested unit measurement and the enclosing virtual turn
                    // each settle in the following render. Observe every one.
                    for _ in 0..4 {
                        cx.update(|window, cx| {
                            window.refresh();
                            let _ = window.draw(cx);
                        });
                    }
                    let after = view.read_with(cx, |view, _| {
                        assert_eq!(view.session.reasoning_expansion["reasoning"], expanded);
                        let height = view.session.timeline_measured_turn_heights["active"];
                        assert_eq!(view.session.timeline_row_sizes[0].height, px(height));
                        assert_eq!(view.session.timeline_process_unit_heights.len(), unit_count);
                        view.session.timeline_scroll.offset().y
                    });
                    let mut previous = before;
                    assert!(!observed.borrow().is_empty());
                    for offset in observed.borrow().iter().copied() {
                        if following_bottom {
                            assert!(
                                offset >= before.min(after) && offset <= before.max(after),
                                "offset {offset:?} outside {before:?}..{after:?}"
                            );
                            assert!(
                                if expanded {
                                    offset <= previous
                                } else {
                                    offset >= previous
                                },
                                "reasoning toggle reversed the scroll direction"
                            );
                        } else {
                            assert_eq!(offset, before, "reading history must keep its position");
                        }
                        previous = offset;
                    }
                    if following_bottom {
                        assert!(
                            if expanded {
                                after < before
                            } else {
                                after > before
                            },
                            "the disclosure must reclaim or grow its extent: units {unit_count}, windowed {windowed_reasoning}, expanded {expanded}, before {before:?}, after {after:?}"
                        );
                    }
                }
            }
        }
    }
}

struct SidebarTimelineProbe {
    sidebar_width: f32,
    scroll: VirtualListScrollHandle,
    content_max_width: Option<f32>,
    rem_size: f32,
}

impl SidebarTimelineProbe {
    fn row_sizes(&self) -> Rc<Vec<Size<Pixels>>> {
        let width = timeline_content_width(
            Some(1400.0 - self.sidebar_width),
            self.content_max_width,
            self.rem_size,
        );
        // The first turns have not been measured; the visible history retains
        // its intrinsic heights, just like the production timeline's table.
        let body =
            "A historical answer with enough text to need a width-dependent estimate. ".repeat(12);
        let estimate = estimated_markdown_body_height(&body, estimated_chars_per_line(width));
        Rc::new(
            (0..10)
                .map(|index| size(px(width), px(if index < 4 { estimate } else { 120.0 })))
                .collect(),
        )
    }
}

impl Render for SidebarTimelineProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.rem_size));
        let row_sizes = self.row_sizes();
        let rendered_sizes = row_sizes.clone();
        let content_max_width = self.content_max_width;
        h_flex()
            .w(px(1400.0))
            .h(px(700.0))
            .items_stretch()
            .child(div().w(px(self.sidebar_width)).flex_none())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        v_virtual_list(
                            cx.entity(),
                            "sidebar-resize-timeline",
                            row_sizes,
                            move |_, range, _, _| {
                                range
                                    .map(|index| {
                                        h_flex()
                                            .w_full()
                                            .h(rendered_sizes[index].height)
                                            .items_start()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .debug_selector(move || format!("resize-row-{index}"))
                                                    .w_full()
                                                    .when_some(content_max_width, |this, width| this.max_w(px(width)))
                                                    .h(px(120.0))
                                                    .child("A timeline row that fits at either sidebar width"),
                                            )
                                            .into_any_element()
                                    })
                                    .collect()
                            },
                        )
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .track_scroll(&self.scroll)
                        .px_4()
                        .py_4()
                        .gap_0(),
                    )
                    .child(div().h(px(100.0)).flex_none()),
            )
    }
}

#[gpui::test]
fn sidebar_width_changes_preserve_timeline_vertical_position(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let scroll = VirtualListScrollHandle::new();
    let scroll_for_view = scroll.clone();
    let (view, cx) = cx.add_window_view(|_, _| SidebarTimelineProbe {
        sidebar_width: 0.0,
        scroll: scroll_for_view,
        content_max_width: Some(AGENT_CONTENT_STANDARD_MAX_WIDTH),
        rem_size: 16.0,
    });
    for (content_width, rem_size) in [
        (SessionContentWidthMode::Standard, 16.0),
        (SessionContentWidthMode::Narrow, 16.0),
        (SessionContentWidthMode::Standard, 20.0),
        (SessionContentWidthMode::Narrow, 14.4),
    ] {
        view.update(cx, |view, cx| {
            view.sidebar_width = 0.0;
            view.content_max_width = session_content_max_width(content_width);
            view.rem_size = rem_size;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let history_offset = view.read_with(cx, |view, _| {
            view.row_sizes()
                .iter()
                .take(4)
                .map(|size| size.height)
                .sum::<Pixels>()
                + px(60.0)
        });
        for (offset, selector) in [
            (-history_offset, "resize-row-6"),
            (-scroll.max_offset().y, "resize-row-8"),
            (px(0.0), "resize-row-0"),
        ] {
            scroll.set_offset(point(px(0.0), offset));
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let before = cx.debug_bounds(selector).unwrap();
            let initial_offset = scroll.offset().y;
            let initial_max = scroll.max_offset().y;
            for width in [0.0, 20.0, 140.0, 220.0, 280.0, 210.0, 100.0, 0.0] {
                view.update(cx, |view, cx| {
                    view.sidebar_width = width;
                    cx.notify();
                });
                cx.update(|window, cx| {
                    let _ = window.draw(cx);
                });
                let after = cx.debug_bounds(selector).unwrap();
                assert_eq!(
                    after.size.width, before.size.width,
                    "the content still fits"
                );
                assert_eq!(
                    after.top(),
                    before.top(),
                    "sidebar width {width}, rem {rem_size}, {selector}"
                );
                assert_eq!(scroll.offset().y, initial_offset, "sidebar width {width}");
                assert_eq!(scroll.max_offset().y, initial_max, "sidebar width {width}");
            }
        }
    }
}

#[test]
fn timeline_width_follows_wrapping_insets_and_content_caps() {
    for rem_size in [14.4, 16.0, 20.0] {
        for cap in [
            AGENT_CONTENT_NARROW_MAX_WIDTH,
            AGENT_CONTENT_STANDARD_MAX_WIDTH,
        ] {
            assert_eq!(
                timeline_content_width(Some(1400.0), Some(cap), rem_size),
                cap
            );
            assert_eq!(
                timeline_content_width(Some(1120.0), Some(cap), rem_size),
                cap
            );
            assert_eq!(timeline_content_width(None, Some(cap), rem_size), cap);
        }
        for cap in [Some(AGENT_CONTENT_STANDARD_MAX_WIDTH), None] {
            assert_eq!(
                timeline_content_width(Some(600.0), cap, rem_size),
                600.0 - 2.0 * rem_size
            );
        }
        assert_eq!(
            timeline_content_width(Some(1400.0), None, rem_size),
            1400.0 - 2.0 * rem_size
        );
    }
    assert_eq!(
        timeline_content_width(None, None, 16.0),
        AGENT_CONTENT_STANDARD_MAX_WIDTH
    );
}

struct ProcessRunProbe {
    expanded: bool,
    bounds: Rc<RefCell<Vec<Bounds<Pixels>>>>,
}

impl Render for ProcessRunProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds.clone();
        let first_height = if self.expanded { 160.0 } else { 24.0 };
        let units = (0..3)
            .map(|index| TimelineProcessUnit {
                rows: index..index + 1,
                id: format!("event-{index}"),
                revision: 1,
                streaming: false,
                estimated_height: 24.0,
                kind: TimelineProcessUnitKind::Row,
            })
            .collect();
        v_flex()
            .w(px(400.0))
            .h(px(400.0))
            .child(
                div().debug_selector(|| "toggle-process".into()).child(
                    Button::new("toggle").label("Details").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.expanded = !this.expanded;
                            cx.notify();
                        },
                    )),
                ),
            )
            .child(TimelineProcessRun {
                units: Rc::new(units),
                origins: Arc::new(vec![px(0.0), px(36.0), px(72.0)]),
                total_height: px(300.0),
                pinned_unit: None,
                hangs_from_bottom: false,
                session_id: None,
                entity: WeakEntity::new_invalid(),
                build_unit: Box::new(move |index, _, _| {
                    let bounds = bounds.clone();
                    div()
                        .w_full()
                        .h(px(if index == 0 { first_height } else { 24.0 }))
                        .on_prepaint(move |value, _, _| bounds.borrow_mut()[index] = value)
                        .into_any_element()
                }),
            })
    }
}

#[gpui::test]
fn expanding_and_collapsing_process_events_never_overlap_siblings(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let bounds = Rc::new(RefCell::new(vec![Bounds::default(); 3]));
    let observed = bounds.clone();
    let (_, cx) = cx.add_window_view(|_, _| ProcessRunProbe {
        expanded: false,
        bounds,
    });
    cx.run_until_parked();
    for expected_height in [160.0, 24.0, 160.0, 24.0] {
        let trigger = cx.debug_bounds("toggle-process").unwrap();
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds = observed.borrow();
        assert_eq!(bounds[0].size.height, px(expected_height));
        for pair in bounds.windows(2) {
            assert_eq!(
                pair[1].top() - pair[0].bottom(),
                px(TIMELINE_PROCESS_UNIT_GAP)
            );
        }
    }
}

#[test]
fn an_offscreen_pinned_process_event_keeps_its_own_origin() {
    assert_eq!(
        timeline_process_measured_origin(20, px(900.0), Some((2, px(300.0))), px(0.0)),
        px(900.0)
    );
    assert_eq!(
        timeline_process_measured_origin(3, px(72.0), Some((2, px(300.0))), px(0.0)),
        px(300.0)
    );
    assert_eq!(
        timeline_process_measured_origin(
            3,
            px(72.0),
            Some((2, px(300.0))),
            px(TIMELINE_PROCESS_UNIT_GAP)
        ),
        px(300.0 + TIMELINE_PROCESS_UNIT_GAP)
    );
}

struct DisclosureProbe {
    progress: f32,
    height: f32,
    observed: Rc<Cell<f32>>,
}

impl Render for DisclosureProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let observed = self.observed.clone();
        v_flex().w(px(400.0)).child(
            div()
                .w_full()
                .on_prepaint(move |bounds, _, _| {
                    observed.set(f32::from(bounds.size.height));
                })
                .child(timeline_disclosure_body(
                    "probe-reveal".into(),
                    self.progress,
                    div().w_full().h(px(self.height)).into_any_element(),
                )),
        )
    }
}

#[gpui::test]
fn disclosures_measure_history_immediately_and_reverse_without_empty_frames(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let observed = Rc::new(Cell::new(0.0));
    let measured = observed.clone();
    let (view, cx) = cx.add_window_view(|_, _| DisclosureProbe {
        progress: 1.0,
        height: 120.0,
        observed,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        measured.get(),
        120.0,
        "history must reserve its real height on first paint"
    );
    for (progress, height, expected) in [
        (0.5, 120.0, 60.0),
        (0.25, 120.0, 30.0),
        (0.75, 120.0, 90.0),
        (1.0, 120.0, 120.0),
        (1.0, 240.0, 240.0),
    ] {
        view.update(cx, |view, cx| {
            view.progress = progress;
            view.height = height;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(measured.get(), expected);
    }
}

struct ReasoningRevealProbe {
    progress: f32,
    observed: Rc<Cell<f32>>,
}

impl Render for ReasoningRevealProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let observed = self.observed.clone();
        v_flex().w(px(400.0)).child(
            div()
                .w_full()
                .on_prepaint(move |bounds, _, _| {
                    observed.set(f32::from(bounds.size.height));
                })
                .child(render_reasoning_first_line_layout(
                    cx.theme().muted_foreground,
                    div().h(px(HEADER_HEIGHT)).into_any_element(),
                    Some(div().h(px(BODY_HEIGHT)).flex_none().into_any_element()),
                    None,
                    ReasoningReveal::new(
                        "reasoning-reveal-probe".into(),
                        self.progress > 0.0,
                        self.progress,
                    ),
                    None,
                )),
        )
    }
}

/// The header an expanded reasoning row keeps whatever the reveal is doing.
const HEADER_HEIGHT: f32 = 20.0;
/// The body the reveal is expected to hold back until the spring reaches it.
const BODY_HEIGHT: f32 = 200.0;

#[gpui::test]
fn a_reasoning_body_reveals_on_the_same_spring_a_tool_detail_does(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let observed = Rc::new(Cell::new(0.0));
    let measured = observed.clone();
    let (view, cx) = cx.add_window_view(|_, _| ReasoningRevealProbe {
        progress: 1.0,
        observed,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        measured.get(),
        HEADER_HEIGHT + BODY_HEIGHT,
        "an open thought reserves its real height on the first paint"
    );
    // The header is the row's own line and never travels; everything under it
    // is the body, so the reveal's share of the row is exactly the spring.
    for (progress, expected) in [
        (0.5, HEADER_HEIGHT + BODY_HEIGHT * 0.5),
        (0.25, HEADER_HEIGHT + BODY_HEIGHT * 0.25),
        (1.0, HEADER_HEIGHT + BODY_HEIGHT),
        // The click that closes a row retargets the spring; the body keeps the
        // height it had on the frame the state flipped, so the collapse is
        // drawn instead of snapping away.
        (0.75, HEADER_HEIGHT + BODY_HEIGHT * 0.75),
        (0.0, HEADER_HEIGHT),
    ] {
        view.update(cx, |view, cx| {
            view.progress = progress;
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(measured.get(), expected, "reveal at {progress}");
    }
}

/// A workbench laid out at a fixed size, so the composer's growth and the
/// conversation viewport above it can be measured against each other.
struct RailLaneProbe {
    workbench: Entity<VibexWorkbench>,
}

impl Render for RailLaneProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.workbench.update(cx, |workbench, cx| {
            workbench.render_agent_workbench(window, cx)
        });
        div().w(px(900.0)).h(px(700.0)).child(page)
    }
}

/// The turn preview rail is a reading aid for the pane, not for whatever the
/// composer is currently showing.
///
/// Expanding a composer extension (a plan, a queue, a goal) grows the composer
/// and shortens the conversation viewport above it. The rail's lane must not
/// follow that: it belongs to the session page, whose height the composer
/// cannot change.
#[gpui::test]
fn composer_plan_growth_leaves_the_turn_preview_rail_its_lane(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_id = VibexSessionId::parse("session_rail_lane").unwrap();
    let workbench_slot: Rc<RefCell<Option<Entity<VibexWorkbench>>>> = Rc::new(RefCell::new(None));
    let slot = workbench_slot.clone();
    let session_for_items = session_id.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let mut ui_state = DesktopUiStateV1::default();
        ui_state.appearance.locale = LocaleMode::En;
        ui_state.session.turn_preview_rail = true;
        let workbench = cx.new(|cx| {
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_for_items.clone());
            workbench.view_session_id = Some(session_for_items.clone());
            let item =
                |sequence: i64, payload: TimelinePayload, source: TimelineSource| TimelineItem {
                    id: TimelineItemId::parse(format!("timeline_rail_lane-{sequence}")).unwrap(),
                    session_id: session_for_items.clone(),
                    sequence,
                    timestamp_ms: sequence,
                    source,
                    kind: payload.kind(),
                    correlation_id: None,
                    provider_correlation_id: None,
                    redaction_state: TimelineRedactionState::None,
                    execution_attribution: None,
                    payload_depth: vibex_core::TimelinePayloadDepth::Detail,
                    payload,
                };
            let mut items = Vec::new();
            for turn in 0..5 {
                items.push(item(
                    items.len() as i64 + 1,
                    TimelinePayload::UserMessage(UserMessagePayload {
                        text: format!("Request {turn}"),
                        attachments: Vec::new(),
                        ..Default::default()
                    }),
                    TimelineSource::User,
                ));
            }
            // The plan rides the last turn, which is the one the composer's
            // plan card mirrors.
            items.push(item(
                items.len() as i64 + 1,
                TimelinePayload::Plan(vibex_core::PlanPayload {
                    title: "Lane plan".into(),
                    steps: (0..9)
                        .map(|index| vibex_core::PlanStepPayload {
                            title: format!("Step {index}"),
                            status: vibex_core::PlanStepStatus::Pending,
                        })
                        .collect(),
                }),
                TimelineSource::Agent,
            ));
            workbench
                .timeline
                .replace_authoritative(session_for_items.clone(), items);
            workbench.rebuild_timeline_sizes();
            workbench
        });
        *slot.borrow_mut() = Some(workbench.clone());
        RailLaneProbe { workbench }
    });
    let workbench = workbench_slot
        .borrow()
        .clone()
        .expect("the probe should keep its workbench");

    fn draw(cx: &mut VisualTestContext) {
        for _ in 0..4 {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
    }

    draw(cx);
    let collapsed_rail = cx
        .debug_bounds("agent-turn-preview-rail")
        .expect("the rail should be laid out");
    let collapsed_viewport =
        workbench.read_with(cx, |workbench, _| workbench.timeline_scroll.bounds());
    let collapsed_tick = cx
        .debug_bounds("turn-preview-tick-0")
        .expect("the rail's first tick should be laid out");

    workbench.update(cx, |workbench, cx| {
        let items = workbench.timeline.items.clone();
        let plan = current_agent_plan(&items).expect("the plan item should project");
        let identity = composer_plan_identity(&session_id, &plan, &items);
        workbench.composer_plan_expanded = Some(identity);
        cx.notify();
    });
    draw(cx);
    let expanded_rail = cx
        .debug_bounds("agent-turn-preview-rail")
        .expect("the rail should still be laid out");
    let expanded_viewport =
        workbench.read_with(cx, |workbench, _| workbench.timeline_scroll.bounds());
    let expanded_tick = cx
        .debug_bounds("turn-preview-tick-0")
        .expect("the rail's first tick should still be laid out");

    assert!(
        (collapsed_viewport.size.height - expanded_viewport.size.height).abs() > px(1.0),
        "the expanded plan should shorten the conversation viewport: collapsed={:?} expanded={:?}",
        collapsed_viewport.size.height,
        expanded_viewport.size.height
    );
    assert_eq!(
        expanded_rail, collapsed_rail,
        "the rail's lane must not follow the composer's height"
    );
    assert_eq!(
        expanded_tick, collapsed_tick,
        "opening the plan panel must not move a tick the reader is aiming at"
    );
}

/// A child Agent rendered next to a conversation must not evict its Markdown.
///
/// The child Agent rail paints whole child timelines through the timeline's own
/// builders while the parent's view is borrowed. One retention table for both
/// let the rail's surfaces push the parent's out — the assistant messages the
/// reader is looking at — and a text view that is re-created is painted again
/// from scratch: with key-order eviction the surface that sorted first was
/// dropped on every frame, so a message on screen blinked for as long as the
/// child was open. The rail's surfaces are kept per child session now, and the
/// borrowed view's own table keeps the least recently painted surface.
#[gpui::test]
fn child_agent_rail_keeps_the_parent_timelines_markdown_surfaces(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let parent_id = VibexSessionId::parse("session_rail_parent").unwrap();
    let child_id = VibexSessionId::parse("session_rail_child").unwrap();
    let (probe, cx) = cx.add_window_view(|window, cx| {
        let mut ui_state = DesktopUiStateV1::default();
        ui_state.appearance.locale = LocaleMode::En;
        let workbench = cx.new(|cx| {
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(parent_id.clone());
            workbench.view_session_id = Some(parent_id.clone());
            workbench
                .pending_agent_turn_session_ids
                .insert(parent_id.to_string());
            workbench
                .timeline
                .replace_authoritative(parent_id.clone(), rail_parent_items(3));
            workbench.rebuild_timeline_sizes();
            workbench
        });
        let mut child = ChildAgentTimelineState::new(child_id.clone());
        child
            .timeline
            .replace_authoritative(child_id.clone(), rail_child_items(210));
        child.loaded = true;
        workbench.update(cx, |workbench, _| {
            workbench
                .child_agent_timelines
                .insert(child_id.to_string(), child);
            workbench.child_agent_panel_active = true;
            workbench.child_agent_tabs = vec![child_id.clone()];
            workbench.child_agent_active_tab = Some(child_id.clone());
        });
        ChildRailProbe {
            workbench,
            rail: std::rc::Rc::new(std::cell::Cell::new(false)),
        }
    });
    let (workbench, rail) =
        probe.read_with(cx, |probe, _| (probe.workbench.clone(), probe.rail.clone()));

    fn draw(cx: &mut VisualTestContext, frames: usize) {
        for _ in 0..frames {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
    }

    draw(cx, 4);
    let parent = workbench.read_with(cx, |workbench, _| {
        markdown_surface_ids(&workbench.markdown_text_states)
    });
    assert_eq!(
        parent.len(),
        6,
        "the parent's own surfaces should be the only ones in its table: {parent:?}"
    );
    draw(cx, 2);
    let settled = workbench.read_with(cx, |workbench, _| {
        markdown_surface_ids(&workbench.markdown_text_states)
    });
    assert_eq!(parent, settled, "a settled conversation must not churn");

    rail.set(true);
    for frame in 0..5 {
        draw(cx, 1);
        let (now, rail_table) = workbench.read_with(cx, |workbench, _| {
            (
                markdown_surface_ids(&workbench.markdown_text_states),
                workbench
                    .child_agent_markdown_states
                    .get(child_id.as_str())
                    .map(markdown_surface_ids)
                    .unwrap_or_default(),
            )
        });
        for (id, state) in &parent {
            assert_eq!(
                now.get(id),
                Some(state),
                "frame {frame}: the rail re-created the parent's surface {id}"
            );
        }
        assert!(
            rail_table.len() <= MARKDOWN_TEXT_STATE_LIMIT,
            "the rail's own table stays bounded: {}",
            rail_table.len()
        );
        assert!(
            !rail_table.keys().any(|id| parent.contains_key(id)),
            "the rail may not keep the parent's surfaces at all"
        );
    }

    // Closing the tab drops everything the rail retained for that child.
    workbench.update(cx, |workbench, cx| {
        workbench.close_child_agent_tab(&child_id, cx);
    });
    draw(cx, 1);
    workbench.read_with(cx, |workbench, _| {
        assert!(
            !workbench
                .child_agent_markdown_states
                .contains_key(child_id.as_str()),
            "a closed child tab must not keep its retained text views"
        );
        assert_eq!(workbench.markdown_text_states.len(), 6);
    });
}

/// The Markdown table drops the surface its session stopped painting.
///
/// Key order was the previous rule, and it is not a recency order: filling the
/// table then repainting an early surface still evicted that surface, which is
/// the one thing a retained text view must not do while it is on screen.
#[gpui::test]
fn markdown_surface_table_evicts_the_least_recently_painted_surface(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = markdown_table_fixture(cx);
    workbench.update(cx, |workbench, cx| {
        let source: Arc<str> = Arc::from("Body");
        for index in 0..MARKDOWN_TEXT_STATE_LIMIT {
            let id = format!("markdown:surface-{index:03}");
            workbench.markdown_text_state(&id, &source, None, false, cx);
        }
        // Repaint the first surface: it is now the most recently painted one,
        // while `surface-001` is the oldest.
        workbench.markdown_text_state("markdown:surface-000", &source, None, false, cx);
        workbench.markdown_text_state("markdown:surface-192", &source, None, false, cx);
        assert!(
            workbench
                .markdown_text_states
                .contains_key("markdown:surface-000"),
            "a surface painted this frame must survive the next insertion"
        );
        assert!(
            !workbench
                .markdown_text_states
                .contains_key("markdown:surface-001"),
            "the least recently painted surface is the one that goes"
        );
        assert_eq!(
            workbench.markdown_text_states.len(),
            MARKDOWN_TEXT_STATE_LIMIT,
            "the bound still holds"
        );
    });
}

fn markdown_surface_ids(table: &BTreeMap<String, MarkdownTextState>) -> BTreeMap<String, EntityId> {
    table
        .iter()
        .map(|(id, entry)| (id.clone(), entry.state.entity_id()))
        .collect()
}

fn rail_parent_items(turns: usize) -> Vec<TimelineItem> {
    let session_id = VibexSessionId::parse("session_rail_parent").unwrap();
    let mut items = Vec::new();
    for turn in 0..turns {
        items.push(rail_item(
            &session_id,
            &format!("timeline_aaa{turn}u"),
            items.len() as i64 + 1,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: format!("Parent question {turn}"),
                ..Default::default()
            }),
        ));
        items.push(rail_item(
            &session_id,
            &format!("timeline_aaa{turn}a"),
            items.len() as i64 + 1,
            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: format!("Parent answer {turn} with some body text."),
                is_final: true,
            }),
        ));
    }
    items
}

fn rail_child_items(turns: usize) -> Vec<TimelineItem> {
    let session_id = VibexSessionId::parse("session_rail_child").unwrap();
    let mut items = Vec::new();
    for turn in 0..turns {
        for (suffix, payload) in [
            (
                "u",
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: format!("Child question {turn}"),
                    ..Default::default()
                }),
            ),
            (
                "a",
                TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                    text: format!("Child answer {turn} with some body text."),
                    is_final: true,
                }),
            ),
        ] {
            items.push(rail_item(
                &session_id,
                &format!("timeline_zzz{turn}{suffix}"),
                items.len() as i64 + 1,
                payload,
            ));
        }
    }
    items
}

fn rail_item(
    session_id: &VibexSessionId,
    id: &str,
    sequence: i64,
    payload: TimelinePayload,
) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::parse(id.to_string()).unwrap(),
        session_id: session_id.clone(),
        sequence,
        timestamp_ms: sequence,
        source: if matches!(payload, TimelinePayload::UserMessage(_)) {
            TimelineSource::User
        } else {
            TimelineSource::Agent
        },
        kind: payload.kind(),
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: TimelineRedactionState::None,
        execution_attribution: None,
        payload_depth: vibex_core::TimelinePayloadDepth::Detail,
        payload,
    }
}

struct ChildRailProbe {
    workbench: Entity<VibexWorkbench>,
    rail: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Render for ChildRailProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rail_open = self.rail.get();
        let (timeline, rail) = self.workbench.update(cx, |workbench, cx| {
            let timeline = workbench.render_agent_workbench_for(false, window, cx);
            let rail = rail_open.then(|| workbench.render_child_agent_timeline_panel(window, cx));
            (timeline, rail)
        });
        div()
            .flex()
            .w(px(1200.0))
            .h(px(700.0))
            .child(div().w(px(700.0)).h_full().child(timeline))
            .when_some(rail, |this, rail| {
                this.child(div().w(px(500.0)).h_full().child(rail))
            })
    }
}

fn markdown_table_fixture(
    cx: &mut TestAppContext,
) -> (Entity<VibexWorkbench>, &mut VisualTestContext) {
    let (view, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| {
            VibexWorkbench::with_initial_state(None, DesktopUiStateV1::default(), None, window, cx)
        });
        MarkdownTableProbe { workbench }
    });
    (view.read_with(cx, |probe, _| probe.workbench.clone()), cx)
}

struct MarkdownTableProbe {
    workbench: Entity<VibexWorkbench>,
}

impl Render for MarkdownTableProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A page the reader's switch superseded must not keep its in-flight slot.
///
/// The older-history budget is per view and only a completion ever releases it.
/// A read that finishes while the reader is looking at another session used to
/// return before touching the count, so the parked view kept the slot for the
/// rest of its life: once two of those had accumulated it stopped prefetching
/// its own history at all, which is what a session that is switched to while it
/// runs looks like from the reader's side.
#[gpui::test]
fn a_history_page_a_switch_superseded_returns_its_slot(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_a = VibexSessionId::parse("session_layout").unwrap();
    let session_b = VibexSessionId::parse("session_switch_b").unwrap();
    let (workbench, cx) = live_tool_timeline(cx, true);
    let generation = workbench.update(cx, |workbench, _| {
        workbench.timeline_has_older = true;
        workbench.timeline_older_in_flight = 1;
        workbench.ensure_session_view(&session_b);
        workbench.session_generation
    });
    // The reader switches away while the page travels, so its answer arrives
    // for a view that is no longer the borrowed one.
    workbench.update(cx, |workbench, _| {
        workbench.selected_session_id = Some(session_b.clone());
        let _ = workbench.borrow_session_view(&session_b);
        workbench.finish_older_timeline_load(
            &session_a,
            4,
            generation,
            Ok(TimelinePage {
                session_id: session_a.clone(),
                items: Vec::new(),
                start_sequence: None,
                end_sequence: None,
                has_older: false,
                has_newer: false,
                starts_at_turn_start: true,
                leading_turn_start: None,
                omitted: None,
            }),
        );
    });
    let slots = workbench.read_with(cx, |workbench, _| {
        workbench
            .session_views
            .get(session_a.as_str())
            .map(|view| view.timeline_older_in_flight)
    });
    assert_eq!(
        slots,
        Some(0),
        "the parked view must not keep the slot its superseded page spent"
    );
}

/// A window that skipped stored rows never becomes the view's projection.
///
/// This is the session-switch path: the view was parked while its session kept
/// working, comes back lagged, and re-reads the newest window. When that window
/// is a live turn whose tail cap cut its middle, installing it would show a
/// conversation with a hole and — because the page still carries the session's
/// first row — `has_older` would be false, so nothing would fetch the hole back.
/// The install must refuse it instead.
#[gpui::test]
fn a_window_that_skipped_stored_rows_is_not_installed(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (workbench, cx) = live_tool_timeline(cx, true);
    let before = workbench.read_with(cx, |workbench, _| workbench.timeline.items.clone());
    let first = before.first().map(|item| item.sequence).unwrap_or(0);
    let last = before.last().map(|item| item.sequence).unwrap_or(0);
    let session_id = workbench.read_with(cx, |workbench, _| {
        workbench.timeline.session_id.clone().unwrap()
    });

    let installed = workbench.update(cx, |workbench, _| {
        let updates = workbench.begin_timeline_load();
        let prepared = runtime
            .block_on(PreparedTimeline::prepare_with_older(
                session_id,
                before,
                false,
                false,
                None,
                Some(vibex_core::TimelineSequenceGap {
                    after: first,
                    before: last,
                }),
                None,
                false,
                ReasoningDisplayMode::LatestAtBottom,
            ))
            .expect("the fixture page prepares");
        workbench.install_prepared_timeline(prepared, &mut updates.borrow_mut())
    });

    assert!(
        !installed,
        "a page that skipped stored rows must not be installed as the projection"
    );
}

/// A prefetch that could not start a read keeps the whole budget.
///
/// The slot is spent for a page that is really travelling; spending one for a
/// request that never left would leave the reader unable to ask for history,
/// because only a completion can give the slot back.
#[gpui::test]
fn a_prefetch_that_cannot_start_keeps_the_history_budget(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    // Park the reader at the top of a transcript that claims older history,
    // with no authority to read it from.
    workbench.update(cx, |workbench, _| {
        workbench.timeline_has_older = true;
        workbench.agent_loading = false;
        workbench.timeline_follow.set_following_bottom(false);
        workbench
            .timeline_scroll
            .set_offset(point(px(0.0), px(0.0)));
    });
    // The precondition is asserted rather than assumed: an empty viewport would
    // hold the prefetch back on its own and the test would pass without a fix.
    let ready = workbench.read_with(cx, |workbench, _| {
        timeline_should_prefetch_older(TimelineOlderPrefetch {
            distance_to_top: workbench.timeline_distance_to_top(),
            viewport_height: f32::from(workbench.timeline_scroll.bounds().size.height),
            has_older: workbench.timeline_has_older,
            in_flight: workbench.timeline_older_in_flight,
            following_bottom: workbench.timeline_follow.following_bottom,
            scrollable: workbench.timeline_scroll.max_offset().y > px(0.0),
            loading: workbench.agent_loading,
        })
    });
    assert!(
        ready,
        "the fixture must be in the prefetching state for this test to mean anything"
    );
    workbench.update(cx, |workbench, cx| {
        for _ in 0..4 {
            workbench.prefetch_older_timeline_if_needed(cx);
        }
    });
    let slots = workbench.read_with(cx, |workbench, _| workbench.timeline_older_in_flight);
    assert_eq!(
        slots, 0,
        "a read that never started must not spend a slot that only a completion can release"
    );
}

/// A turn whose own first row has not arrived reads as loading, not as work.
///
/// The rows a later read adds to that turn are older than the ones on screen, so
/// a duration built from the rows already here would move under the reader. The
/// view says so instead, and stops reporting a turn in progress: the tick that
/// would keep the duration moving has nothing left to move.
#[gpui::test]
fn a_turn_that_is_still_arriving_reads_as_loading(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    // Keep only the response rows: what a window shows when its budget ran out
    // inside one long turn, with that turn's first row still below the window.
    workbench.update(cx, |workbench, _| {
        let session_id = workbench.timeline.session_id.clone().unwrap();
        let tail = workbench
            .timeline
            .items
            .iter()
            .filter(|item| item.kind != vibex_core::TimelineItemKind::UserMessage)
            .cloned()
            .collect::<Vec<_>>();
        workbench.timeline.replace_authoritative(session_id, tail);
        workbench.timeline_starts_mid_turn = true;
        workbench.conversation_turns_cache_key = None;
        workbench.timeline_row_sizes = Rc::new(Vec::new());
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    let leading = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_starts_mid_turn,
            workbench
                .conversation_turns_cache
                .first()
                .map(|turn| turn.user_row.is_none()),
            workbench.conversation_turns_summary.has_incomplete_turn,
        )
    });
    assert_eq!(
        leading,
        (true, Some(true), false),
        "the arriving turn is the leading one, and it is not work in progress"
    );

    // Once the page that carries its first row lands, the view is a window of
    // whole turns again and the turn reads as work with a duration.
    workbench.update(cx, |workbench, _| {
        workbench.timeline_starts_mid_turn = false;
        workbench.conversation_turns_cache_key = None;
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    let loaded = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_starts_mid_turn,
            workbench.conversation_turns_summary.has_incomplete_turn,
        )
    });
    assert_eq!(loaded, (false, true));

    // The read that cut the window can still place the turn it opened inside.
    // The header then counts from the turn's own first row instead of from the
    // row the budget stopped at: the label is for a turn nothing can place, not
    // for rows that have not been read back yet.
    workbench.update(cx, |workbench, _| {
        workbench.timeline_starts_mid_turn = true;
        workbench.timeline.leading_turn_start = Some(TimelineTurnStart {
            sequence: 1,
            timestamp_ms: 1,
        });
        workbench.conversation_turns_cache_key = None;
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    let placed = workbench.read_with(cx, |workbench, _| {
        let leading = workbench
            .conversation_turns_cache
            .first()
            .expect("the arriving turn is the cached leading turn");
        (leading.user_row.is_none(), leading.started_at_ms)
    });
    assert_eq!(
        placed,
        (true, 1),
        "the leading turn is dated from the read that cut the window"
    );
}

/// The boundary comes from the read that produced the page, not from what the
/// rows happen to look like.
#[gpui::test]
fn an_older_page_moves_the_views_leading_end(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_a = VibexSessionId::parse("session_layout").unwrap();
    let (workbench, cx) = live_tool_timeline(cx, true);
    let generation = workbench.update(cx, |workbench, _| workbench.session_generation);
    let items = workbench.read_with(cx, |workbench, _| workbench.timeline.items.clone());
    let page =
        |starts_at_turn_start: bool, leading_turn_start: Option<TimelineTurnStart>| TimelinePage {
            session_id: session_a.clone(),
            items: items.clone(),
            start_sequence: items.first().map(|item| item.sequence),
            end_sequence: items.last().map(|item| item.sequence),
            has_older: true,
            has_newer: false,
            starts_at_turn_start,
            leading_turn_start,
            omitted: None,
        };
    // The page's items all sit below the cursor it was read for.
    let cursor = items.last().map(|item| item.sequence).unwrap_or(0) + 1;
    let head = TimelineTurnStart {
        sequence: 1,
        timestamp_ms: 1,
    };
    workbench.update(cx, |workbench, _| {
        workbench.finish_older_timeline_load(
            &session_a,
            cursor,
            generation,
            Ok(page(false, Some(head))),
        );
    });
    let leading = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_starts_mid_turn,
            workbench.timeline.leading_turn_start,
        )
    });
    assert_eq!(
        leading,
        (true, Some(head)),
        "a page that opens inside a turn carries where that turn begins"
    );
    workbench.update(cx, |workbench, _| {
        workbench.finish_older_timeline_load(&session_a, cursor, generation, Ok(page(true, None)));
    });
    let landed = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_starts_mid_turn,
            workbench.timeline.leading_turn_start,
        )
    });
    assert_eq!(
        landed,
        (false, None),
        "a page that opens on a boundary finishes the turn above it"
    );
}

/// Whether a hover change and a click reach an element inside a cached view.
///
/// A retained process unit is mounted through `Entity::cached`, and the fork
/// replays a cached view's paint range instead of painting it again. This probe
/// isolates that from the timeline: the same child view is mounted plainly in one
/// pass and cached in the next, and both are hovered and clicked in the same
/// place.
struct CachedClickChild {
    clicks: Rc<std::cell::Cell<usize>>,
    renders: Rc<std::cell::Cell<usize>>,
}

impl Render for CachedClickChild {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let clicks = self.clicks.clone();
        div()
            .debug_selector(|| "cached-click-child".to_string())
            .w_full()
            .h(px(40.0))
            .hover(|style| style.opacity(0.5))
            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                clicks.set(clicks.get() + 1)
            })
    }
}

struct CachedClickProbe {
    child: Entity<CachedClickChild>,
    cached: bool,
}

impl Render for CachedClickProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if self.cached {
            div()
                .size_full()
                .child(
                    self.child
                        .clone()
                        .cached(StyleRefinement::default().size_full()),
                )
                .into_any_element()
        } else {
            div()
                .size_full()
                .child(self.child.clone())
                .into_any_element()
        }
    }
}

#[gpui::test]
fn a_click_and_a_hover_reach_an_element_inside_a_cached_view(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for cached in [false, true] {
        let clicks = Rc::new(std::cell::Cell::new(0));
        let renders = Rc::new(std::cell::Cell::new(0));
        let observed_clicks = clicks.clone();
        let observed_renders = renders.clone();
        let (_, cx) = cx.add_window_view(|_, cx| CachedClickProbe {
            child: cx.new(|_| CachedClickChild { clicks, renders }),
            cached,
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds = cx
            .debug_bounds("cached-click-child")
            .expect("the child paints");
        let before_hover = observed_renders.get();
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        if !cached {
            assert!(
                observed_renders.get() > before_hover,
                "a hover change must reach a plainly mounted view"
            );
        }
        // A cached view is not notified when the hover changes, so its hover
        // wash goes stale until something else renders it. Recorded rather than
        // asserted: a retained unit cannot rely on hover to repaint itself, which
        // is one of the two reasons the retention was reverted.
        let hovered_through_cache = observed_renders.get() > before_hover;
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            observed_clicks.get(),
            1,
            "a click inside a cached view must reach its listener (cached={cached}, hover-through-cache={hovered_through_cache})"
        );
    }
}

/// The spike behind [`timeline_group_body`]: an expanded group's body lays out
/// only the rows the viewport shows, so a session that ran for hours stops
/// paying for the rows nobody can see.
///
/// The number this asserts is the one the measurement needs: the body must stop
/// building every row, and the rows it does build must be the band the viewport
/// is over — not the leading rows of the group.
#[gpui::test]
fn an_expanded_group_body_builds_only_the_rows_the_viewport_shows(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    const TOOL_ROWS: usize = 240;
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| {
            let mut ui_state = DesktopUiStateV1::default();
            ui_state.appearance.locale = LocaleMode::En;
            ui_state.session.enhanced_command_execution_display = false;
            ui_state.session.turn_preview_rail = false;
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_id.clone());
            workbench.view_session_id = Some(session_id.clone());
            let mut items = vec![live_timeline_item(
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Check the workspace".into(),
                    ..Default::default()
                }),
            )];
            for tool in 0..TOOL_ROWS {
                items.push(progress_tool(items.len() as i64 + 1, tool));
            }
            workbench.timeline.replace_authoritative(session_id, items);
            workbench.rebuild_timeline_sizes();
            // Open the single group the run projects before the first frame, so
            // the disclosure starts settled and its body is on screen.
            let group_id = workbench
                .conversation_turns_cache
                .iter()
                .find_map(|turn| {
                    turn.process_activity_groups_with_commands_and_file_operations
                        .first()
                        .map(|group| group.id.clone())
                })
                .expect("the tool run projects one activity group");
            workbench.timeline_command_expansion.insert(group_id, true);
            workbench.request_timeline_scroll_to_latest();
            workbench
        });
        LiveTimelineProbe { workbench }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());

    let mut built = Vec::new();
    for _ in 0..4 {
        workbench.update(cx, |workbench, cx| {
            workbench.timeline_group_rows_built.clear();
            cx.notify();
        });
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let frame = workbench.read_with(cx, |workbench, _| {
            workbench.timeline_group_rows_built.clone()
        });
        if !frame.is_empty() {
            built = frame;
        }
    }
    let group_rows = workbench.read_with(cx, |workbench, _| {
        workbench
            .conversation_turns_cache
            .iter()
            .find_map(|turn| {
                turn.process_activity_groups_with_commands_and_file_operations
                    .first()
                    .map(|group| group.end_row - group.start_row)
            })
            .unwrap_or_default()
    });
    assert!(
        group_rows > 100,
        "the fixture should project a long run, got {group_rows} rows"
    );
    // One update can draw twice — the first correction of the row table notifies
    // the workbench and the test harness draws the dirty window again — so the
    // rows the window chose are what the assertions read, not the call count.
    built.sort_unstable();
    built.dedup();
    assert!(
        !built.is_empty(),
        "the settled body must build the rows the viewport shows"
    );
    // The band is the 400px viewport plus the row window's own slack on each
    // edge — about twenty 36px rows. The bound leaves room for the rows to
    // measure shorter or taller, and still fails if the window falls back to
    // the unit run's 640px overscan, which alone would reach eighteen rows.
    assert!(
        built.len() <= 32,
        "the windowed body built {} of {group_rows} rows: {built:?}",
        built.len()
    );
    let first = built.iter().copied().min().unwrap_or_default();
    let last = built.iter().copied().max().unwrap_or_default();
    assert_eq!(
        built.len(),
        last - first + 1,
        "the body builds one contiguous window: {built:?}"
    );
    // The fixture follows the newest output, so the window must sit at the end
    // of the group rather than on its leading rows.
    assert_eq!(
        last + 1,
        group_rows,
        "the window should reach the last row, built {built:?}"
    );
}

/// The windowed body stacks its rows adjacently, exactly as the inline body it
/// stands in for does, and never paints one below the box it reserved.
///
/// The body reserves the sum of its row table and then places the visible rows
/// after each other's measured bottoms. Placing them apart by the unit run's
/// gap instead walked every row after the first past that reserved bottom, and
/// the body's own clip cut the group's newest rows off at the timeline's
/// bottom — the occlusion a reader sees while following a live turn, where the
/// newest activity is a long expanded group.
#[gpui::test]
fn an_expanded_group_body_keeps_its_rows_inside_the_box_it_reserves(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    const TOOL_ROWS: usize = 240;
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| {
            let mut ui_state = DesktopUiStateV1::default();
            ui_state.appearance.locale = LocaleMode::En;
            ui_state.session.enhanced_command_execution_display = false;
            ui_state.session.turn_preview_rail = false;
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_id.clone());
            workbench.view_session_id = Some(session_id.clone());
            let mut items = vec![live_timeline_item(
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Check the workspace".into(),
                    ..Default::default()
                }),
            )];
            for tool in 0..TOOL_ROWS {
                items.push(progress_tool(items.len() as i64 + 1, tool));
            }
            workbench.timeline.replace_authoritative(session_id, items);
            workbench.rebuild_timeline_sizes();
            let group_id = workbench
                .conversation_turns_cache
                .iter()
                .find_map(|turn| {
                    turn.process_activity_groups_with_commands_and_file_operations
                        .first()
                        .map(|group| group.id.clone())
                })
                .expect("the tool run projects one activity group");
            workbench.timeline_command_expansion.insert(group_id, true);
            workbench.request_timeline_scroll_to_latest();
            workbench
        });
        LiveTimelineProbe { workbench }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());
    for _ in 0..4 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }

    let (group_id, row_ids) = workbench.read_with(cx, |workbench, _| {
        let turn = workbench
            .conversation_turns_cache
            .iter()
            .find(|turn| !turn.process_rows.is_empty())
            .cloned()
            .expect("the fixture projects one turn");
        let group = turn
            .process_activity_groups_with_commands_and_file_operations
            .first()
            .cloned()
            .expect("the fixture projects one activity group");
        let rows = turn.process_rows[group.start_row..group.end_row]
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>();
        (group.id, rows)
    });

    let body_selector: &'static str =
        Box::leak(format!("activity-group-body:{group_id}").into_boxed_str());
    let body = cx
        .debug_bounds(body_selector)
        .expect("the expanded group body is on screen");
    let mut previous: Option<Bounds<Pixels>> = None;
    let mut last_built: Option<Bounds<Pixels>> = None;
    let mut built = 0;
    for row_id in &row_ids {
        let selector: &'static str = Box::leak(format!("activity-row:{row_id}").into_boxed_str());
        let Some(row) = cx.debug_bounds(selector) else {
            // The window builds one contiguous band, so the rows it skipped sit
            // above or below it — never between two it built.
            continue;
        };
        if let Some(previous) = previous {
            assert_eq!(
                row.top(),
                previous.bottom(),
                "rows of one group body stack adjacently, without the unit run's gap ({row_id})"
            );
        }
        assert!(
            row.bottom() <= body.bottom(),
            "a row must stay inside the box the body reserved for it ({row_id}): {row:?} vs {body:?}"
        );
        previous = Some(row);
        last_built = Some(row);
        built += 1;
    }
    assert!(built > 1, "the body must build the rows the viewport shows");
    assert_eq!(
        last_built.map(|row| row.bottom()),
        Some(body.bottom() - px(2.0)),
        "the body's last row ends at the box it reserved, minus its own padding"
    );
}

/// The process-unit walk is O(process rows) and both the renderer and the height
/// estimator ask for it, so a turn holding a whole session's work rebuilt the
/// same units twice per frame. The walk is now memoized per turn and reused
/// while its inputs hold.
#[gpui::test]
fn the_process_unit_walk_is_reused_while_its_inputs_hold(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    draw_live_timeline_frames(&workbench, cx, 8);

    let walk = |workbench: &Entity<VibexWorkbench>, cx: &mut VisualTestContext| {
        workbench.update(cx, |workbench, _| {
            let turn = workbench
                .conversation_turns_cache
                .last()
                .cloned()
                .expect("the fixture projects one turn");
            workbench.timeline_process_units(&turn)
        })
    };

    let first = walk(&workbench, cx);

    // A frame later the shape has not moved, so the walk is not repeated.
    draw_live_timeline_frames(&workbench, cx, 4);
    let reused = walk(&workbench, cx);
    assert!(
        Rc::ptr_eq(&first, &reused),
        "an unchanged turn must reuse the units the last frame built"
    );

    // Real content arriving does move the shape, so the stale list is replaced.
    let sequence = workbench.read_with(cx, |workbench, _| {
        workbench.timeline.authoritative_end_sequence.unwrap()
    });
    apply_live_timeline_item(&workbench, cx, progress_tool(sequence + 1, 99));
    let rewalked = walk(&workbench, cx);
    assert!(
        !Rc::ptr_eq(&first, &rewalked),
        "a new tool row must rebuild the turn's units"
    );

    // The memo belongs to the projection that produced it, so a turn the walk no
    // longer sees is evicted rather than growing the cache without bound.
    workbench.read_with(cx, |workbench, _| {
        assert!(workbench.timeline_process_units_cache.len() <= 1);
    });
}

/// A body that outruns the allowance is cut at a line boundary.
///
/// The head is what the Markdown surface lays out, so it has to read as whole
/// prose rather than stopping wherever the byte allowance happened to land.
#[test]
fn a_body_that_outruns_the_allowance_is_cut_at_a_line_boundary() {
    let short = "one line\n".repeat(4);
    assert_eq!(
        folded_body_parts(&short),
        (short.as_str(), 0),
        "a body that fits travels whole"
    );

    let line = "x".repeat(64);
    let long: String = (0..400).map(|_| format!("{line}\n")).collect();
    let (head, hidden) = folded_body_parts(&long);
    assert!(head.len() <= TIMELINE_BODY_VISIBLE_BYTES);
    assert!(head.len() >= TIMELINE_BODY_VISIBLE_MIN_BYTES);
    assert!(head.ends_with('\n'), "the head ends where a line does");
    assert!(long.starts_with(head));
    assert_eq!(hidden, long.len() - head.len());

    // One line that outruns the whole allowance is cut inside that line rather
    // than handed over as one endless row.
    let unbroken = "y".repeat(TIMELINE_BODY_VISIBLE_BYTES * 3);
    let (head, hidden) = folded_body_parts(&unbroken);
    assert_eq!(head.len(), TIMELINE_BODY_VISIBLE_BYTES);
    assert_eq!(hidden, unbroken.len() - head.len());

    // A multi-byte character on the allowance's edge is never split.
    let cjk = "中".repeat(TIMELINE_BODY_VISIBLE_BYTES);
    let (head, hidden) = folded_body_parts(&cjk);
    assert!(cjk.starts_with(head));
    assert!(head.is_char_boundary(head.len()));
    assert_eq!(hidden, cjk.len() - head.len());
}

/// A cut is memoized by the row's revision.
///
/// A body on screen asks for its cut every frame, so the frame path reads the
/// memo rather than scanning the body again — and a body that grew is a new
/// revision, which re-cuts.
#[gpui::test]
fn a_body_cut_is_memoized_by_the_row_revision(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    let body = "line\n".repeat(400);

    let cut = workbench.update(cx, |workbench, _| {
        workbench.timeline_body_fold_cut("row", 7, &body, false)
    });
    assert_eq!(cut, folded_body_parts(&body).0.len());

    // A sentinel under the same revision is what the next call answers with,
    // which is what proves the frame path does not rescan the body.
    workbench.update(cx, |workbench, _| {
        workbench.timeline_body_folds.insert(
            "row".to_string(),
            (
                7,
                TimelineBodyFold {
                    revealed: false,
                    cut: 11,
                },
            ),
        );
    });
    assert_eq!(
        workbench.update(cx, |workbench, _| workbench
            .timeline_body_fold_cut("row", 7, &body, false)),
        11
    );

    // A new revision is a new body, so it is cut again.
    assert_eq!(
        workbench.update(cx, |workbench, _| workbench
            .timeline_body_fold_cut("row", 8, &body, false)),
        folded_body_parts(&body).0.len()
    );
}

/// A row's chunk table is keyed by the text the row draws, not by its revision
/// alone.
///
/// The folded and opened shapes of one row share a revision — revealing is not a
/// new item — so a table cut from the head would otherwise be reused to lay out
/// the whole body, with the head's ranges and the head's height. The length of
/// the text is what tells the two tables apart.
#[gpui::test]
fn a_body_window_follows_the_text_the_row_draws(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, cx) = live_tool_timeline(cx, true);
    let body: String = (0..500)
        .map(|index| format!("paragraph {index} of a report the row draws\n\n"))
        .collect();
    let head = folded_body_parts(&body).0;
    assert!(head.len() < body.len(), "the fixture has to fold");
    let (head_len, whole_len) = workbench.update(cx, |workbench, _| {
        let head_window = workbench.timeline_body_window("row", 7, head, 80);
        let whole_window = workbench.timeline_body_window("row", 7, &body, 80);
        (head_window.body_len, whole_window.body_len)
    });
    assert_eq!(head_len, head.len(), "the folded table stops at the head");
    assert_eq!(whole_len, body.len(), "opening the row re-cuts the table");
}

/// A long settled body is drawn as its head until the reader opens it.
///
/// The Markdown surface lays its whole body out on every frame it is on screen,
/// so a screenful of long answers is what turns a 60 fps timeline into a 20 fps
/// one. A settled body that outruns the allowance therefore travels as its head,
/// with a control on the row that pays for the rest — and the head itself is
/// past the viewport minimum, so it is laid out as chunks of itself. The row's
/// measurement follows both cuts, or the timeline would reserve height nothing
/// fills.
#[gpui::test]
fn a_long_settled_body_is_drawn_as_its_head_until_the_reader_opens_it(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let long_body: String = (0..900)
        .map(|line| format!("line {line} of a settled answer\n"))
        .collect();
    assert!(
        long_body.len() > TIMELINE_BODY_VISIBLE_BYTES,
        "the fixture has to outrun the fold allowance: {} bytes",
        long_body.len()
    );
    let (view, cx) = cx.add_window_view({
        let long_body = long_body.clone();
        move |window, cx| {
            let workbench = cx.new(|cx| {
                let mut ui_state = DesktopUiStateV1::default();
                ui_state.appearance.locale = LocaleMode::En;
                ui_state.session.turn_preview_rail = false;
                let mut workbench =
                    VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
                workbench.agent_loading = false;
                workbench.selected_session_id = Some(session_id.clone());
                workbench.view_session_id = Some(session_id.clone());
                workbench.timeline.replace_authoritative(
                    session_id.clone(),
                    vec![
                        live_timeline_item(
                            1,
                            TimelinePayload::UserMessage(UserMessagePayload {
                                text: "first question".into(),
                                attachments: Vec::new(),
                                ..Default::default()
                            }),
                        ),
                        live_timeline_item(
                            2,
                            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                                text: long_body.clone(),
                                is_final: true,
                            }),
                        ),
                        // The answer is followed by another turn, which is what
                        // settles it: only the last turn can still be live.
                        live_timeline_item(
                            3,
                            TimelinePayload::UserMessage(UserMessagePayload {
                                text: "second question".into(),
                                attachments: Vec::new(),
                                ..Default::default()
                            }),
                        ),
                        live_timeline_item(
                            4,
                            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                                text: "short answer".into(),
                                is_final: true,
                            }),
                        ),
                    ],
                );
                workbench.conversation_turns_cache_key = None;
                workbench.rebuild_timeline_sizes();
                workbench
            });
            LiveTimelineProbe { workbench }
        }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());
    let _ = draw_live_timeline_frames(&workbench, cx, 2);

    let (row_id, folded_height, full_estimate) = workbench.read_with(cx, |workbench, _| {
        let row = workbench.conversation_turns_cache[0]
            .conclusion_row
            .as_ref()
            .expect("the settled turn's answer is a row");
        assert!(
            !row.streaming,
            "the fixture's long answer has to be settled for this test to mean anything"
        );
        (
            row.id.clone(),
            f32::from(workbench.timeline_row_sizes[0].height),
            estimated_markdown_body_height(
                &row.body,
                workbench.estimated_timeline_chars_per_line(),
            ),
        )
    });

    let folded_source = folded_body_parts(&long_body).0.len();
    // The head is past the viewport minimum, so the fold draws it through the
    // same chunk table an opened body gets. The table's length is the proof it
    // was cut from the head: the row still draws the allowance, not the body.
    let (folded_table, whole_surface) = workbench.read_with(cx, |workbench, _| {
        let table = workbench
            .timeline_body_windows
            .get(&row_id)
            .map(|(_, window)| {
                (
                    window.body_len,
                    window.chunks.last().map_or(0, |chunk| chunk.end),
                )
            });
        let surface = workbench
            .markdown_text_states
            .get(&format!("markdown:{row_id}"))
            .map(|state| state.source.len());
        (table, surface)
    });
    assert_eq!(
        folded_table,
        Some((folded_source, folded_source)),
        "the chunk table stops at the fold's head"
    );
    assert!(
        whole_surface.is_none(),
        "the head is not handed to one surface whole: {whole_surface:?}"
    );
    assert!(
        folded_height < full_estimate,
        "the row reserves the folded body's height, not the whole one: {folded_height} against {full_estimate}"
    );
    // The harness looks a selector up by `&'static str`, and this row's id is
    // minted per run.
    let control_selector: &'static str =
        Box::leak(format!("timeline-body-reveal:{row_id}").into_boxed_str());
    assert!(
        cx.debug_bounds(control_selector).is_some(),
        "a folded body carries the control that opens it"
    );
    let body_selector: &'static str =
        Box::leak(format!("timeline-body-window:{row_id}").into_boxed_str());
    assert!(
        cx.debug_bounds(body_selector).is_some(),
        "the folded head is laid out in the transcript like any windowed body"
    );

    // The press belongs to the control: without claiming it, the same press
    // starts a window-level text selection over the body the click opens.
    // The folded head is taller than the test window, so the control below it
    // is off screen. Scroll the transcript the way a reader would until the
    // control is in view, then press it where it now sits.
    let control_bounds = cx
        .debug_bounds(control_selector)
        .expect("the folded row's control is laid out");
    workbench.update(cx, |workbench, cx| {
        // Scrolling up is the reader leaving the live edge; without this the
        // next frame pulls the transcript back to the bottom.
        workbench.timeline_follow.set_following_bottom(false);
        let offset = workbench.timeline_scroll.offset();
        workbench.timeline_scroll.set_offset(point(
            px(0.0),
            offset.y + px(120.0) - control_bounds.origin.y,
        ));
        cx.notify();
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    let control_bounds = cx
        .debug_bounds(control_selector)
        .expect("the folded row's control is still laid out");
    cx.simulate_mouse_down(
        control_bounds.center(),
        MouseButton::Left,
        Modifiers::none(),
    );
    assert!(
        cx.update(|_, cx| gpui_base::GlobalState::is_text_selection_suppressed(cx)),
        "the control claims its press so the body is not selected"
    );
    // The reader's release opens the rest, and the row measures the whole body.
    cx.simulate_mouse_up(
        control_bounds.center(),
        MouseButton::Left,
        Modifiers::none(),
    );
    // The surface parses a new document in the background, so the row's
    // measurement lands after the parse rather than on the next frame.
    for _ in 0..8 {
        cx.run_until_parked();
        let _ = draw_live_timeline_frames(&workbench, cx, 1);
    }
    let (opened_height, opened_surfaces) = workbench.read_with(cx, |workbench, _| {
        let prefix = format!("markdown-chunk:{row_id}:");
        (
            f32::from(workbench.timeline_row_sizes[0].height),
            workbench
                .markdown_text_states
                .iter()
                .filter(|(id, _)| id.starts_with(&prefix))
                .map(|(_, state)| state.source.len())
                .collect::<Vec<_>>(),
        )
    });
    // Opening a body this long splits it into chunks the transcript lays out as
    // the screen reaches them: one surface never receives the whole body, and
    // the row is as tall as the body so the reader scrolls the page through it.
    assert!(
        cx.debug_bounds(body_selector).is_some(),
        "the opened row is still laid out in the transcript"
    );
    assert!(
        !opened_surfaces.is_empty()
            && opened_surfaces
                .iter()
                .all(|len| *len <= TIMELINE_BODY_CHUNK_MAX_BYTES),
        "no surface receives the whole body: {opened_surfaces:?}"
    );
    assert!(
        opened_height > folded_height && opened_height <= full_estimate * 1.5,
        "the row reserves the body rather than a box: {opened_height}, folded {folded_height}, estimate {full_estimate}"
    );

    // Only settled prose folds: a body still arriving is the reader's live view
    // of the Agent's work, and a tool card bounds itself.
    workbench.read_with(cx, |workbench, _| {
        let settled = workbench.conversation_turns_cache[0]
            .conclusion_row
            .as_ref()
            .expect("the settled turn's answer is a row");
        let mut streaming = settled.clone();
        streaming.streaming = true;
        assert!(
            !timeline_row_body_folds(&streaming, true),
            "a body still arriving is the live view"
        );
        assert!(
            timeline_row_body_folds(&streaming, false),
            "a row whose turn has finished folds, even when the projection left it streaming"
        );
        let mut tool = settled.clone();
        tool.kind = TimelineRowKind::Command;
        assert!(
            !timeline_row_body_folds(&tool, false),
            "only prose bodies fold"
        );
    });
}

/// A body between the two allowances is read as chunks even though it is shown
/// whole.
///
/// The fold allowance decides how much of a message the reader sees; the
/// viewport allowance decides how it is laid out. Tying the second to the first
/// — or to the reader having opened the body — left the band between them on the
/// single-surface frame path, which is the band the larger fold allowance now
/// admits.
#[gpui::test]
fn a_settled_body_between_the_allowances_is_read_as_chunks(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let body: String = (0..150)
        .map(|index| {
            format!("line {index} of an answer that is shown whole but laid out in chunks\n")
        })
        .collect();
    assert!(
        body.len() > TIMELINE_BODY_VIEWPORT_MIN_BYTES && body.len() <= TIMELINE_BODY_VISIBLE_BYTES,
        "the fixture has to sit between the allowances: {} bytes",
        body.len()
    );
    let (view, cx) = cx.add_window_view({
        let body = body.clone();
        move |window, cx| {
            let workbench = cx.new(|cx| {
                let mut ui_state = DesktopUiStateV1::default();
                ui_state.appearance.locale = LocaleMode::En;
                ui_state.session.turn_preview_rail = false;
                let mut workbench =
                    VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
                workbench.agent_loading = false;
                workbench.selected_session_id = Some(session_id.clone());
                workbench.view_session_id = Some(session_id.clone());
                workbench.timeline.replace_authoritative(
                    session_id.clone(),
                    vec![
                        live_timeline_item(
                            1,
                            TimelinePayload::UserMessage(UserMessagePayload {
                                text: "first question".into(),
                                attachments: Vec::new(),
                                ..Default::default()
                            }),
                        ),
                        live_timeline_item(
                            2,
                            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                                text: body.clone(),
                                is_final: true,
                            }),
                        ),
                        // The answer is followed by another turn, which is what
                        // settles it: only the last turn can still be live.
                        live_timeline_item(
                            3,
                            TimelinePayload::UserMessage(UserMessagePayload {
                                text: "second question".into(),
                                attachments: Vec::new(),
                                ..Default::default()
                            }),
                        ),
                        live_timeline_item(
                            4,
                            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                                text: "short answer".into(),
                                is_final: true,
                            }),
                        ),
                    ],
                );
                workbench.conversation_turns_cache_key = None;
                workbench.rebuild_timeline_sizes();
                workbench
            });
            LiveTimelineProbe { workbench }
        }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    let (row_id, hidden, table, surface) = workbench.read_with(cx, |workbench, _| {
        let row = workbench.conversation_turns_cache[0]
            .conclusion_row
            .as_ref()
            .expect("the settled turn's answer is a row");
        (
            row.id.clone(),
            folded_body_parts(&row.body).1,
            workbench
                .timeline_body_windows
                .get(&row.id)
                .map(|(_, window)| (window.body_len, window.chunks.len())),
            workbench
                .markdown_text_states
                .get(&format!("markdown:{}", row.id))
                .map(|state| state.source.len()),
        )
    });
    assert_eq!(hidden, 0, "the whole body is shown");
    let (table_len, chunks) = table.expect("a body past the viewport minimum has a table");
    assert_eq!(
        table_len,
        body.len(),
        "the table is cut from the whole body"
    );
    assert!(chunks > 1, "one surface would lay the body out whole");
    assert!(
        surface.is_none(),
        "the body is not handed to one surface: {surface:?}"
    );
    let control: &'static str =
        Box::leak(format!("timeline-body-reveal:{row_id}").into_boxed_str());
    assert!(
        cx.debug_bounds(control).is_none(),
        "nothing is hidden, so the row carries no fold control"
    );
}

/// The allowance in a language where a character is three bytes.
///
/// Twelve kilobytes is about four thousand Chinese characters, and a body at the
/// allowance is not folded at all; one character more is, and what it shows is at
/// least half the allowance — the fold is a reading choice, and a reader looking
/// at a Chinese message should get as much of it as the allowance promises.
#[test]
fn a_chinese_body_folds_at_the_allowance_and_keeps_at_least_half_of_it() {
    let at_allowance = "中".repeat(TIMELINE_BODY_VISIBLE_BYTES / 3);
    assert_eq!(at_allowance.len(), TIMELINE_BODY_VISIBLE_BYTES);
    let (head, hidden) = folded_body_parts(&at_allowance);
    assert_eq!(hidden, 0, "a body at the allowance is not folded");
    assert_eq!(head.len(), at_allowance.len());

    let over = format!("{at_allowance}中");
    let (head, hidden) = folded_body_parts(&over);
    assert_eq!(hidden, 3, "one character past the allowance folds");
    assert_eq!(head.len(), TIMELINE_BODY_VISIBLE_BYTES);
    assert!(
        head.len() >= TIMELINE_BODY_VISIBLE_MIN_BYTES,
        "the head keeps at least half the allowance"
    );
    assert!(head.is_char_boundary(head.len()));
}

/// A windowed body's chunks are the body itself, cut on paragraph breaks.
///
/// A chunk is a layout unit: it has to start on a whole Markdown block so a
/// table, a list or a fence is not cut in half, and the chunks together have to
/// be the body exactly, or the window would show something the row does not say.
#[test]
fn a_windowed_body_is_cut_on_paragraph_breaks_and_keeps_fences_whole() {
    let paragraphs: String = (0..400)
        .map(|index| format!("paragraph {index} with enough words to wrap\n\n"))
        .collect();
    let chunks = timeline_body_chunks(&paragraphs);
    assert!(chunks.len() > 1, "a long body is more than one chunk");
    assert_eq!(chunks.first().map(|chunk| chunk.start), Some(0));
    assert_eq!(chunks.last().map(|chunk| chunk.end), Some(paragraphs.len()));
    for pair in chunks.windows(2) {
        assert_eq!(pair[0].end, pair[1].start, "the chunks are adjacent");
    }
    for chunk in &chunks {
        assert!(chunk.len() <= TIMELINE_BODY_CHUNK_MAX_BYTES);
        assert!(paragraphs.is_char_boundary(chunk.start));
        assert!(paragraphs.is_char_boundary(chunk.end));
        assert!(
            paragraphs[chunk.clone()].starts_with("paragraph"),
            "a chunk opens on a whole block"
        );
    }
    let rebuilt: String = chunks
        .iter()
        .map(|chunk| &paragraphs[chunk.clone()])
        .collect();
    assert_eq!(rebuilt, paragraphs, "the chunks are the body");

    // A fence that fits inside the allowance travels whole.
    let fenced = format!(
        "intro line\n\n```rust\n{}\n```\n\nafter the fence\n",
        "let value = 1;\n".repeat(120)
    );
    let chunks = timeline_body_chunks(&fenced);
    let fences = chunks
        .iter()
        .filter(|chunk| fenced[chunk.start..chunk.end].matches("```").count() > 0)
        .collect::<Vec<_>>();
    assert_eq!(fences.len(), 1, "the fence lands inside one chunk");
    assert_eq!(
        fenced[fences[0].start..fences[0].end]
            .matches("```")
            .count(),
        2,
        "and it opens and closes there"
    );
}

/// A revealed long body is laid out by the screen, not by its own length.
///
/// The point of the split is what it does not lay out: the body scrolls with
/// the transcript like any other row — one scrollbar, and whatever the screen
/// shows is shown whole — while only the chunks the screen reaches are built.
#[gpui::test]
fn a_revealed_long_body_lays_out_only_the_chunks_the_screen_shows(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let long_body: String = (0..4000)
        .map(|index| format!("paragraph {index} of a report nobody should lay out whole\n\n"))
        .collect();
    assert!(long_body.len() > TIMELINE_BODY_VIEWPORT_MIN_BYTES);
    let (view, cx) = cx.add_window_view({
        let long_body = long_body.clone();
        move |window, cx| {
            let workbench = cx.new(|cx| {
                let mut ui_state = DesktopUiStateV1::default();
                ui_state.appearance.locale = LocaleMode::En;
                ui_state.session.turn_preview_rail = false;
                let mut workbench =
                    VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
                workbench.agent_loading = false;
                workbench.selected_session_id = Some(session_id.clone());
                workbench.view_session_id = Some(session_id.clone());
                workbench.timeline.replace_authoritative(
                    session_id.clone(),
                    vec![
                        live_timeline_item(
                            1,
                            TimelinePayload::UserMessage(UserMessagePayload {
                                text: "first question".into(),
                                attachments: Vec::new(),
                                ..Default::default()
                            }),
                        ),
                        live_timeline_item(
                            2,
                            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                                text: long_body.clone(),
                                is_final: true,
                            }),
                        ),
                        live_timeline_item(
                            3,
                            TimelinePayload::UserMessage(UserMessagePayload {
                                text: "second question".into(),
                                attachments: Vec::new(),
                                ..Default::default()
                            }),
                        ),
                        live_timeline_item(
                            4,
                            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                                text: "short answer".into(),
                                is_final: true,
                            }),
                        ),
                    ],
                );
                workbench.conversation_turns_cache_key = None;
                workbench.rebuild_timeline_sizes();
                workbench
            });
            LiveTimelineProbe { workbench }
        }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());
    let (row_id, turn_id) = workbench.read_with(cx, |workbench, _| {
        let turn = &workbench.conversation_turns_cache[0];
        (
            turn.conclusion_row
                .as_ref()
                .expect("the settled turn's answer is a row")
                .id
                .clone(),
            turn.id.clone(),
        )
    });
    workbench.update(cx, |workbench, cx| {
        workbench.toggle_timeline_row_body(row_id.clone(), Some(turn_id.clone()), cx)
    });
    for _ in 0..6 {
        cx.run_until_parked();
        let _ = draw_live_timeline_frames(&workbench, cx, 1);
    }

    let (chunk_count, laid_out, laid_out_bytes, row_height, full_estimate) =
        workbench.read_with(cx, |workbench, _| {
            let (_, window) = workbench
                .timeline_body_windows
                .get(&row_id)
                .expect("the opened body has a chunk table");
            let prefix = format!("markdown-chunk:{row_id}:");
            let laid_out = workbench
                .markdown_text_states
                .keys()
                .filter(|key| key.starts_with(&prefix))
                .filter_map(|key| key.rsplit(':').next()?.parse::<usize>().ok())
                .collect::<Vec<_>>();
            (
                window.chunks.len(),
                laid_out.len(),
                laid_out
                    .iter()
                    .filter_map(|index| window.chunks.get(*index))
                    .map(|chunk| chunk.len())
                    .sum::<usize>(),
                f32::from(workbench.timeline_row_sizes[0].height),
                estimated_markdown_body_height(
                    &long_body,
                    workbench.estimated_timeline_chars_per_line(),
                ),
            )
        });
    assert!(
        chunk_count > 8,
        "the fixture has to need a window: {chunk_count}"
    );
    assert!(
        laid_out < chunk_count / 4,
        "only the chunks the screen shows are laid out: {laid_out} of {chunk_count}"
    );
    assert!(
        laid_out_bytes < TIMELINE_BODY_CHUNK_MAX_BYTES * 4,
        "and only their text is laid out: {laid_out_bytes} bytes of {}",
        long_body.len()
    );
    assert!(
        row_height > full_estimate / 2.0,
        "the row reserves the body, not a box: {row_height} against {full_estimate}"
    );
    let body_selector: &'static str =
        Box::leak(format!("timeline-body-window:{row_id}").into_boxed_str());
    assert!(
        cx.debug_bounds(body_selector).is_some(),
        "the opened body is laid out in the transcript"
    );

    // Scrolling the transcript builds the chunks the screen reaches, and only
    // those: the body has no scroll of its own.
    let (total_height, last_index) = workbench.read_with(cx, |workbench, _| {
        let (_, window) = workbench.timeline_body_windows.get(&row_id).unwrap();
        (
            f32::from(window.total_height()),
            window.chunks.len().saturating_sub(1),
        )
    });
    // Scroll until the end of the body is on screen. Where that is depends on
    // the heights the chunks measure to, so the test walks to it rather than
    // guessing a fraction of the table.
    let mut reached = false;
    let mut laid_out = 0;
    let mut fraction = 0.5_f32;
    while fraction <= 1.05 && !reached {
        workbench.update(cx, |workbench, cx| {
            workbench
                .timeline_scroll
                .set_offset(point(px(0.0), px(-(total_height * fraction))));
            cx.notify();
        });
        for _ in 0..2 {
            cx.run_until_parked();
            let _ = draw_live_timeline_frames(&workbench, cx, 1);
        }
        (reached, laid_out) = workbench.read_with(cx, |workbench, _| {
            let prefix = format!("markdown-chunk:{row_id}:");
            (
                workbench
                    .markdown_text_states
                    .contains_key(&format!("{prefix}{last_index}")),
                workbench
                    .markdown_text_states
                    .keys()
                    .filter(|key| key.starts_with(&prefix))
                    .count(),
            )
        });
        fraction += 0.1;
    }
    assert!(reached, "the screen lays out the chunk it scrolled to");
    assert!(
        laid_out < chunk_count,
        "and still only the band it shows: {laid_out} of {chunk_count}"
    );

    // Folding it back has to cut a table for the head, not reuse the one the
    // whole body was read through.
    workbench.update(cx, |workbench, cx| {
        workbench.toggle_timeline_row_body(row_id.clone(), Some(turn_id.clone()), cx);
    });
    for _ in 0..3 {
        cx.run_until_parked();
        let _ = draw_live_timeline_frames(&workbench, cx, 1);
    }
    let folded_again = workbench.read_with(cx, |workbench, _| {
        f32::from(workbench.timeline_row_sizes[0].height)
    });
    assert!(
        folded_again < row_height,
        "folding back measures the head again: {folded_again} against {row_height}"
    );
}

/// An answer that lands while its session is on screen stays open.
///
/// The reader is watching the turn finish: folding the answer at that instant
/// hands them a control to press before they can read what just arrived. A turn
/// that settles while the reader is elsewhere — every other test's shape —
/// folds on its own, which is what the same row shows before the answer lands.
#[gpui::test]
fn an_answer_that_lands_on_screen_stays_open(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let long_answer: String = (0..400)
        .map(|index| format!("paragraph {index} of the answer that just arrived\n\n"))
        .collect();
    assert!(long_answer.len() > TIMELINE_BODY_VISIBLE_BYTES);
    let batch_session_id = session_id.clone();
    let (view, cx) = cx.add_window_view({
        let long_answer = long_answer.clone();
        move |window, cx| {
            let workbench = cx.new(|cx| {
                let mut ui_state = DesktopUiStateV1::default();
                ui_state.appearance.locale = LocaleMode::En;
                ui_state.session.turn_preview_rail = false;
                let mut workbench =
                    VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
                workbench.agent_loading = false;
                workbench.selected_session_id = Some(session_id.clone());
                workbench.view_session_id = Some(session_id.clone());
                workbench.timeline.replace_authoritative(
                    session_id.clone(),
                    vec![
                        live_timeline_item(
                            1,
                            TimelinePayload::UserMessage(UserMessagePayload {
                                text: "question".into(),
                                attachments: Vec::new(),
                                ..Default::default()
                            }),
                        ),
                        live_timeline_item(
                            2,
                            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                                text: long_answer.clone(),
                                is_final: false,
                            }),
                        ),
                    ],
                );
                workbench.upsert_session_snapshot(AgentSession {
                    id: session_id.clone(),
                    title: "Watched".into(),
                    project_id: ProjectId::parse("project_probe").unwrap(),
                    workspace_id: vibex_core::WorkspaceId::parse("workspace_probe").unwrap(),
                    workspace_root: "/probe".into(),
                    workspace_mode: WorkspaceMode::CurrentCheckout,
                    agent_id: AgentId::parse("codex").unwrap(),
                    state: AgentSessionState::Idle,
                    safety: AgentSessionSafety::workspace_write_ask_on_risk(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                    last_message_at_ms: 1,
                    archived_at_ms: None,
                    deleted_at_ms: None,
                });
                workbench.conversation_turns_cache_key = None;
                workbench.rebuild_timeline_sizes();
                workbench
            });
            LiveTimelineProbe { workbench }
        }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());
    let _ = draw_live_timeline_frames(&workbench, cx, 2);
    // The answer arrives, the way the live stream delivers it.
    workbench.update(cx, |workbench, cx| {
        workbench.apply_live_timeline_batch(
            vec![TimelineLiveEvent {
                session_id: batch_session_id.clone(),
                sequence: 3,
                item: live_timeline_item(
                    3,
                    TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                        text: long_answer.clone(),
                        is_final: true,
                    }),
                ),
            }],
            cx,
        );
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 3);
    let (row_id, full_body, foldable, revealed) = workbench.read_with(cx, |workbench, _| {
        let turn = &workbench.conversation_turns_cache[0];
        let row = turn
            .conclusion_row
            .as_ref()
            .or_else(|| turn.process_rows.iter().max_by_key(|row| row.body.len()))
            .expect("the finished turn holds its answer");
        (
            row.id.clone(),
            row.body.len(),
            workbench.row_body_folds(row),
            workbench.timeline_row_body_revealed(&row.id),
        )
    });
    assert!(
        full_body > TIMELINE_BODY_VISIBLE_BYTES,
        "the fixture's answer has to be long enough to fold: {full_body}"
    );
    assert!(
        foldable,
        "the answer is the kind of body that folds once its turn is done"
    );
    assert!(
        revealed,
        "the answer that landed in front of the reader is left open"
    );
    let (drawn, chunked) = workbench.read_with(cx, |workbench, _| {
        let prefix = format!("markdown-chunk:{row_id}:");
        (
            workbench
                .markdown_text_states
                .get(&format!("markdown:{row_id}"))
                .map(|state| state.source.len()),
            workbench
                .markdown_text_states
                .keys()
                .filter(|key| key.starts_with(&prefix))
                .count(),
        )
    });
    assert!(
        drawn.is_none(),
        "the answer that landed on screen is not drawn as a folded head: {drawn:?}"
    );
    assert!(chunked > 0, "it is read whole, as chunks of itself");
}

/// Opening or folding a body leaves the reader's place on screen.
///
/// The control is under the reader's pointer, and following the bottom would
/// grow the row upward: the text they were reading would slide out of view.
#[gpui::test]
fn a_body_toggle_leaves_the_screen_where_the_reader_left_it(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let mut items = Vec::new();
    let mut sequence = 1_i64;
    for turn in 0..6 {
        items.push(live_timeline_item(
            sequence,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: format!("question {turn}"),
                attachments: Vec::new(),
                ..Default::default()
            }),
        ));
        sequence += 1;
        let body: String = (0..200)
            .map(|index| {
                format!(
                    "answer {turn} paragraph {index} {}\n\n",
                    "words ".repeat(20)
                )
            })
            .collect();
        items.push(live_timeline_item(
            sequence,
            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: body,
                is_final: true,
            }),
        ));
        sequence += 1;
    }
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let workbench = cx.new(|cx| {
            let mut ui_state = DesktopUiStateV1::default();
            ui_state.appearance.locale = LocaleMode::En;
            ui_state.session.turn_preview_rail = false;
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_id.clone());
            workbench.view_session_id = Some(session_id.clone());
            workbench
                .timeline
                .replace_authoritative(session_id.clone(), items);
            workbench.conversation_turns_cache_key = None;
            workbench.rebuild_timeline_sizes();
            workbench.request_timeline_scroll_to_latest();
            workbench
        });
        LiveTimelineProbe { workbench }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());
    let _ = draw_live_timeline_frames(&workbench, cx, 3);
    // The reader is at the live edge when they open a body above it.
    let (row_id, turn_id, before) = workbench.read_with(cx, |workbench, _| {
        let turn = &workbench.conversation_turns_cache[0];
        (
            turn.conclusion_row
                .as_ref()
                .expect("the first turn's answer is a row")
                .id
                .clone(),
            turn.id.clone(),
            f32::from(workbench.timeline_scroll.offset().y),
        )
    });
    assert!(
        workbench.read_with(cx, |workbench, _| workbench
            .timeline_follow
            .following_bottom),
        "the fixture starts at the live edge"
    );
    workbench.update(cx, |workbench, cx| {
        workbench.toggle_timeline_row_body(row_id.clone(), Some(turn_id.clone()), cx);
    });
    let _ = draw_live_timeline_frames(&workbench, cx, 3);
    let (after, following, anchored) = workbench.read_with(cx, |workbench, _| {
        (
            f32::from(workbench.timeline_scroll.offset().y),
            workbench.timeline_follow.following_bottom,
            workbench.timeline_follow.has_anchor(),
        )
    });
    assert!(
        !following,
        "opening a body hands the screen back to the reader"
    );
    assert!(anchored, "and the row they were reading is the anchor");
    assert!(
        (after - before).abs() <= 2.0,
        "opening a body leaves the screen where it was: {before} then {after}"
    );
}

/// A tool call the reader can open, carrying enough output to be worth it.
fn finished_tool(sequence: i64, tool: usize) -> TimelineItem {
    live_timeline_item(
        sequence,
        TimelinePayload::ToolCall(vibex_core::ToolCallPayload {
            tool_call_id: format!("tool-{tool}"),
            tool_name: "exec_command".into(),
            status: vibex_core::ToolCallStatus::Completed,
            summary: format!("Ran cargo test {tool}"),
            input_summary: Some(format!("cargo test --locked -p crate-{tool}")),
            output_summary: Some((0..6).map(|line| format!("result line {line}\n")).collect()),
            raw_extension: None,
        }),
    )
}

/// Two turns of tool calls, the last of them on screen at the live edge.
fn conversation_with_tools(finished: bool) -> Vec<TimelineItem> {
    let mut items = Vec::new();
    for turn in 0..2_usize {
        let next = |items: &Vec<TimelineItem>| items.len() as i64 + 1;
        items.push(live_timeline_item(
            next(&items),
            TimelinePayload::UserMessage(UserMessagePayload {
                text: format!("question {turn}"),
                ..Default::default()
            }),
        ));
        for tool in 0..4 {
            items.push(finished_tool(next(&items), turn * 10 + tool));
        }
        if finished || turn == 0 {
            items.push(live_timeline_item(
                next(&items),
                TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                    text: format!("answer {turn}"),
                    is_final: true,
                }),
            ));
        }
    }
    items
}

/// The conversation parked on its bottom edge, following, with the last turn's
/// tool rows drawn. `open_row` is one the reader opened before they got there.
fn tool_rows_at_the_live_edge(
    cx: &mut TestAppContext,
    items: Vec<TimelineItem>,
    open_row: Option<usize>,
) -> (Entity<VibexWorkbench>, Vec<String>, &mut VisualTestContext) {
    tool_rows_in_display_at_the_live_edge(cx, items, open_row, false)
}

fn tool_rows_in_display_at_the_live_edge(
    cx: &mut TestAppContext,
    items: Vec<TimelineItem>,
    open_row: Option<usize>,
    enhanced_command_display: bool,
) -> (Entity<VibexWorkbench>, Vec<String>, &mut VisualTestContext) {
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let workbench = cx.new(|cx| {
            let mut ui_state = DesktopUiStateV1::default();
            ui_state.appearance.locale = LocaleMode::En;
            ui_state.session.enhanced_command_execution_display = enhanced_command_display;
            ui_state.session.turn_preview_rail = false;
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_id.clone());
            workbench.view_session_id = Some(session_id.clone());
            workbench
                .timeline
                .replace_authoritative(session_id.clone(), items);
            let turn = workbench
                .conversation_turns_cached()
                .last()
                .unwrap()
                .clone();
            workbench
                .timeline_process_expansion
                .insert(turn.id.clone(), true);
            for group in
                timeline_process_activity_groups_for_display(&turn, enhanced_command_display)
            {
                workbench
                    .timeline_command_expansion
                    .insert(group.id.clone(), true);
            }
            if let Some(index) = open_row {
                workbench
                    .timeline_command_expansion
                    .insert(turn.process_rows[index].id.clone(), true);
            }
            workbench.rebuild_timeline_sizes();
            workbench.request_timeline_scroll_to_latest();
            workbench
        });
        LiveTimelineProbe { workbench }
    });
    let workbench = view.read_with(cx, |view, _| view.workbench.clone());
    for _ in 0..40 {
        draw_disclosure_frame(cx);
    }
    let rows = workbench.read_with(cx, |workbench, _| {
        assert!(workbench.timeline_follow.following_bottom);
        assert!(workbench.timeline_scroll.max_offset().y > px(0.0));
        workbench
            .conversation_turns_cache
            .last()
            .unwrap()
            .process_rows
            .iter()
            .map(|row| row.id.clone())
            .collect()
    });
    (workbench, rows, cx)
}

fn draw_disclosure_frame(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(16));
    cx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

fn activity_row_bounds(cx: &mut VisualTestContext, row_id: &str) -> Bounds<Pixels> {
    let selector = format!("activity-row:{row_id}").leak();
    cx.debug_bounds(selector).expect("the row is on screen")
}

/// Clicks a row's header where it is painted now.
fn click_activity_header(cx: &mut VisualTestContext, row_id: &str) -> Point<Pixels> {
    let position = activity_row_bounds(cx, row_id).origin + point(px(80.0), px(8.0));
    cx.simulate_click(position, Modifiers::none());
    position
}

fn timeline_offset(workbench: &Entity<VibexWorkbench>, cx: &VisualTestContext) -> Pixels {
    workbench.read_with(cx, |workbench, _| workbench.timeline_scroll.offset().y)
}

fn timeline_is_on_the_bottom_edge(
    workbench: &Entity<VibexWorkbench>,
    cx: &VisualTestContext,
) -> bool {
    workbench.read_with(cx, |workbench, _| {
        workbench.timeline_follow.following_bottom
            && workbench.timeline_scroll.offset().y == -workbench.timeline_scroll.max_offset().y
    })
}

/// A row opens where it is, wherever the reader is.
///
/// Further up the timeline the row grows below a still screen. Following the
/// bottom used to grow it upward, out from under the pointer, and resolved each
/// frame's offset from a row table a frame behind the content, so everything
/// under the row was thrown down by a frame of the spring and came back.
#[gpui::test]
fn a_row_opened_at_the_live_edge_leaves_the_screen_where_it_was(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, rows, cx) = tool_rows_at_the_live_edge(cx, conversation_with_tools(true), None);
    let row = &rows[3];
    let header = activity_row_bounds(cx, row).top();
    let offset = timeline_offset(&workbench, cx);

    let pointer = click_activity_header(cx, row);
    for _ in 0..30 {
        draw_disclosure_frame(cx);
        assert_eq!(activity_row_bounds(cx, row).top(), header);
        assert_eq!(timeline_offset(&workbench, cx), offset);
    }
    let (following, max_offset) = workbench.read_with(cx, |workbench, _| {
        (
            workbench.timeline_follow.following_bottom,
            workbench.timeline_scroll.max_offset().y,
        )
    });
    assert!(!following, "the opened row hands the screen to the reader");
    assert!(max_offset > -offset, "the row opened into the extent");

    // The header never left the pointer, so the same click closes it, and the
    // row gives back exactly what it took: the reader is on the live edge again
    // and nothing else would resume the follow.
    cx.simulate_click(pointer, Modifiers::none());
    for _ in 0..30 {
        draw_disclosure_frame(cx);
        assert_eq!(activity_row_bounds(cx, row).top(), header);
        assert_eq!(timeline_offset(&workbench, cx), offset);
    }
    assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
}

/// A row closing on the bottom edge cannot leave the screen where it was: the
/// content has to come down to meet the viewport. What it can do is come down
/// in one piece, with what is under the row sitting still.
#[gpui::test]
fn a_row_closing_on_the_bottom_edge_keeps_that_edge_still(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, rows, cx) =
        tool_rows_at_the_live_edge(cx, conversation_with_tools(true), Some(2));
    let header = activity_row_bounds(cx, &rows[2]).top();
    let below = activity_row_bounds(cx, &rows[3]).top();

    click_activity_header(cx, &rows[2]);
    let mut previous = header;
    for _ in 0..30 {
        draw_disclosure_frame(cx);
        let drawn = activity_row_bounds(cx, &rows[3]).top();
        assert!(
            (drawn - below).abs() <= px(1.0),
            "the row below moved with the spring: {below:?} then {drawn:?}"
        );
        let top = activity_row_bounds(cx, &rows[2]).top();
        assert!(
            top >= previous,
            "the closing row reversed: {previous:?} then {top:?}"
        );
        previous = top;
    }
    assert!(
        previous > header,
        "the viewport came down to the new bottom"
    );
    assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
    workbench.read_with(cx, |workbench, _| {
        assert!(
            !workbench.timeline_rows_hang_from_bottom(),
            "a stream's held slack must stay under the content"
        );
    });
}

/// A running turn is what the live edge is for, so a row opening in it stays
/// there, and grows into the viewport without disturbing what is under it.
#[gpui::test]
fn a_row_opened_in_a_running_turn_stays_on_the_live_edge(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, rows, cx) =
        tool_rows_at_the_live_edge(cx, conversation_with_tools(false), None);
    let below = activity_row_bounds(cx, &rows[3]).top();
    let header = activity_row_bounds(cx, &rows[2]).top();
    let turn_id = workbench.read_with(cx, |workbench, _| {
        workbench
            .conversation_turns_cache
            .last()
            .unwrap()
            .id
            .clone()
    });

    workbench.update(cx, |workbench, cx| {
        workbench.set_timeline_disclosure(rows[2].clone(), Some(&turn_id), true);
        cx.notify();
    });
    let mut previous = header;
    for _ in 0..30 {
        draw_disclosure_frame(cx);
        let drawn = activity_row_bounds(cx, &rows[3]).top();
        assert!(
            (drawn - below).abs() <= px(1.0),
            "the row below moved with the spring: {below:?} then {drawn:?}"
        );
        let top = activity_row_bounds(cx, &rows[2]).top();
        assert!(
            top <= previous,
            "the opening row reversed: {previous:?} then {top:?}"
        );
        previous = top;
    }
    assert!(previous < header, "the viewport rose to the new bottom");
    assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
}

/// Opening a row reads its detail back, and the answer lands while the row is
/// still travelling. The turn's extent stands until prepaint measures the new
/// content: an estimate in the row table for one frame moved the whole timeline
/// on the bottom edge, where the scroll offset is resolved from that table.
#[gpui::test]
fn a_detail_landing_keeps_the_extent_the_turn_was_drawn_at(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let items = conversation_with_tools(true);
    let summaries = items
        .iter()
        .cloned()
        .map(TimelineItem::into_summary)
        .collect();
    let (workbench, rows, cx) = tool_rows_at_the_live_edge(cx, summaries, None);
    let row = rows[3].clone();
    let header = activity_row_bounds(cx, &row).top();
    click_activity_header(cx, &row);
    draw_disclosure_frame(cx);
    draw_disclosure_frame(cx);

    workbench.update(cx, |workbench, _| {
        let drawn = workbench.timeline_row_sizes.clone();
        let sequence = workbench.timeline_row_sequence(&row).unwrap();
        let detail = items
            .iter()
            .find(|item| item.sequence == sequence)
            .unwrap()
            .clone();
        let session_id = workbench.timeline.session_id.clone().unwrap();
        let generation = workbench.session_generation;
        workbench.finish_timeline_detail(&row, &session_id, generation, Ok(detail));
        assert_eq!(
            workbench.timeline_row_sizes, drawn,
            "the row table keeps what prepaint measured"
        );
    });
    for _ in 0..30 {
        draw_disclosure_frame(cx);
        assert_eq!(activity_row_bounds(cx, &row).top(), header);
    }
}

/// One turn of `units` commentary-and-tool pairs: past the flow limit, its
/// process section is the windowed run.
fn turn_of_units(units: usize, finished: bool) -> Vec<TimelineItem> {
    let mut items = vec![live_timeline_item(
        1,
        TimelinePayload::UserMessage(UserMessagePayload {
            text: "question".into(),
            ..Default::default()
        }),
    )];
    for unit in 0..units {
        items.push(live_timeline_item(
            items.len() as i64 + 1,
            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: format!("checking package {unit}"),
                is_final: false,
            }),
        ));
        items.push(finished_tool(items.len() as i64 + 1, unit));
    }
    if finished {
        items.push(live_timeline_item(
            items.len() as i64 + 1,
            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "answer".into(),
                is_final: true,
            }),
        ));
    }
    items
}

/// One turn of a single long tool run, whose group body windows its rows.
fn turn_of_one_long_group(rows: usize) -> Vec<TimelineItem> {
    let mut items = vec![live_timeline_item(
        1,
        TimelinePayload::UserMessage(UserMessagePayload {
            text: "question".into(),
            ..Default::default()
        }),
    )];
    for tool in 0..rows {
        items.push(finished_tool(items.len() as i64 + 1, tool));
    }
    items
}

/// The tool rows of the last turn, in order, whatever separates them.
fn tool_row_ids(workbench: &Entity<VibexWorkbench>, cx: &VisualTestContext) -> Vec<String> {
    workbench.read_with(cx, |workbench, _| {
        workbench
            .conversation_turns_cache
            .last()
            .unwrap()
            .process_rows
            .iter()
            .filter(|row| row.kind == TimelineRowKind::ToolCall)
            .map(|row| row.id.clone())
            .collect()
    })
}

/// Toggles `row` and asserts the row under it never leaves its place, while the
/// row itself, and the whole timeline above it, travels one way and never back.
fn assert_toggling_keeps_the_row_below_still(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
    row: &str,
    below: &str,
    expanded: bool,
) {
    let turn_id = workbench.read_with(cx, |workbench, _| {
        workbench
            .conversation_turns_cache
            .last()
            .unwrap()
            .id
            .clone()
    });
    let rest = activity_row_bounds(cx, below).top();
    let mut previous = activity_row_bounds(cx, row).top();
    workbench.update(cx, |workbench, cx| {
        workbench.set_timeline_disclosure(row.to_string(), Some(&turn_id), expanded);
        cx.notify();
    });
    for frame in 0..40 {
        draw_disclosure_frame(cx);
        let drawn = activity_row_bounds(cx, below).top();
        assert!(
            (drawn - rest).abs() <= px(1.0),
            "frame {frame}: the row below moved with the spring: {rest:?} then {drawn:?}"
        );
        let top = activity_row_bounds(cx, row).top();
        let reversed = if expanded {
            top > previous + px(1.0)
        } else {
            top < previous - px(1.0)
        };
        assert!(
            !reversed,
            "frame {frame}: the toggled row reversed: {previous:?} then {top:?}"
        );
        previous = top;
    }
}

/// A long turn draws its process section through the windowed run, whose
/// reserved height is another frame behind the rows it places.
#[gpui::test]
fn a_windowed_run_keeps_the_rows_under_a_disclosure_still_on_the_live_edge(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let (workbench, _, cx) = tool_rows_at_the_live_edge(cx, turn_of_units(24, false), None);
    let rows = tool_row_ids(&workbench, cx);
    let (row, below) = (&rows[rows.len() - 2], &rows[rows.len() - 1]);
    assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, true);
    assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, false);
    assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
}

/// A long group's body is windowed too, and reserves its height the same way.
#[gpui::test]
fn a_windowed_group_keeps_the_rows_under_a_disclosure_still_on_the_live_edge(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let (workbench, _, cx) = tool_rows_at_the_live_edge(cx, turn_of_one_long_group(40), None);
    let rows = tool_row_ids(&workbench, cx);
    let (row, below) = (&rows[rows.len() - 2], &rows[rows.len() - 1]);
    assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, true);
    assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, false);
    assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
}

/// The same long turn once it has finished: the row was opened earlier, the
/// reader has come to the bottom edge, and closing it brings the viewport down.
#[gpui::test]
fn a_windowed_run_keeps_the_rows_under_a_closing_row_still_on_the_bottom_edge(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    // Commentary and tool rows alternate, so the second to last tool is here.
    let (workbench, _, cx) = tool_rows_at_the_live_edge(cx, turn_of_units(24, true), Some(45));
    let rows = tool_row_ids(&workbench, cx);
    let (row, below) = (&rows[rows.len() - 2], &rows[rows.len() - 1]);
    assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, false);
    assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
}

/// A group in the enhanced display opens from the height it was drawn at too.
#[gpui::test]
fn a_windowed_group_in_the_enhanced_display_keeps_the_rows_under_a_disclosure_still(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let (workbench, _, cx) =
        tool_rows_in_display_at_the_live_edge(cx, turn_of_one_long_group(40), None, true);
    let rows = tool_row_ids(&workbench, cx);
    let (row, below) = (&rows[rows.len() - 2], &rows[rows.len() - 1]);
    assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, true);
    assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, false);
    assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
}

/// With reduced motion a row lands in one frame, and everything it changes
/// reaches the row table over the next few. Nothing may move while they do.
#[gpui::test]
fn a_row_that_lands_in_one_frame_keeps_the_rows_under_it_still(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for items in [turn_of_units(24, false), turn_of_one_long_group(40)] {
        let (workbench, _, cx) = tool_rows_at_the_live_edge(cx, items, None);
        // After the fixture: a workbench applies its own appearance settings.
        cx.update(|_, cx| cx.set_reduce_motion(true));
        let rows = tool_row_ids(&workbench, cx);
        let (row, below) = (&rows[rows.len() - 2], &rows[rows.len() - 1]);
        assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, true);
        assert_toggling_keeps_the_row_below_still(&workbench, cx, row, below, false);
        assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
    }
}

/// A row that opened in one frame has not landed where it started, even though
/// the scroll handle still reports the extent it was last laid out against.
#[gpui::test]
fn a_row_that_opens_in_one_frame_leaves_the_follow_until_it_closes(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, rows, cx) = tool_rows_at_the_live_edge(cx, conversation_with_tools(true), None);
    // After the fixture: a workbench applies its own appearance settings.
    cx.update(|_, cx| cx.set_reduce_motion(true));
    let row = &rows[3];
    let header = activity_row_bounds(cx, row).top();
    let closed = activity_row_bounds(cx, row).size.height;
    let pointer = click_activity_header(cx, row);
    draw_disclosure_frame(cx);
    assert!(
        activity_row_bounds(cx, row).size.height > closed,
        "with reduced motion the row is open in the first frame"
    );
    assert!(workbench.read_with(cx, |workbench, _| !workbench.timeline_disclosure_animating));
    for _ in 0..8 {
        draw_disclosure_frame(cx);
        assert_eq!(activity_row_bounds(cx, row).top(), header);
        assert!(
            workbench.read_with(cx, |workbench, _| !workbench
                .timeline_follow
                .following_bottom),
            "the opened row hands the screen to the reader"
        );
    }
    cx.simulate_click(pointer, Modifiers::none());
    for _ in 0..8 {
        draw_disclosure_frame(cx);
        assert_eq!(activity_row_bounds(cx, row).top(), header);
    }
    assert!(timeline_is_on_the_bottom_edge(&workbench, cx));
}

/// A disclosure of the child Agent rail names a turn of another timeline. The
/// parent's follow, and the rows it hangs, are not that row's to move.
#[gpui::test]
fn a_disclosure_of_another_timeline_leaves_this_one_alone(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (workbench, _, cx) = tool_rows_at_the_live_edge(cx, conversation_with_tools(true), None);
    workbench.update(cx, |workbench, _| {
        workbench.hold_timeline_for_disclosure(Some("turn-of-the-child-session"), true);
        assert!(workbench.timeline_follow.following_bottom);
        assert!(!workbench.timeline_disclosure_animating);
        assert!(!workbench.timeline_disclosure_holds_screen);
    });
}

#[test]
fn a_window_places_its_units_from_the_edge_that_holds_still() {
    // Three units of 100 with a gap of 10, the middle one measuring 130 now.
    let origins = [px(0.0), px(110.0), px(220.0)];
    let total = px(320.0);
    let measured = [(0, 100.0), (1, 130.0), (2, 100.0)];
    let gap = px(10.0);
    assert_eq!(
        timeline_window_origins(&origins, total, &measured, gap, false),
        vec![px(0.0), px(110.0), px(250.0)],
        "the top holds, and the unit under the change moves"
    );
    assert_eq!(
        timeline_window_origins(&origins, total, &measured, gap, true),
        vec![px(-30.0), px(80.0), px(220.0)],
        "the bottom holds, and the unit under the change stays"
    );
    // A window that stops short of the run hangs from the table's own edge.
    assert_eq!(
        timeline_window_origins(&origins, total, &measured[..2], gap, true),
        vec![px(-30.0), px(80.0)],
    );
    // A pinned unit is not a neighbour of the window it was built beside.
    let pinned = [(0, 100.0), (2, 130.0)];
    assert_eq!(
        timeline_window_origins(&origins, total, &pinned, gap, true),
        vec![px(0.0), px(190.0)],
    );
}
