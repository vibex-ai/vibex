use super::*;
use gpui::{Modifiers, TestAppContext};

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
                                windowed_reasoning.then_some(ReasoningWindow {
                                    height: REASONING_WINDOW_HEIGHT,
                                    fold: true,
                                }),
                                cx,
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
                                    if windowed_reasoning { 172.0 } else { 300.0 }
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
        timeline_process_measured_origin(20, px(900.0), Some((2, px(300.0)))),
        px(900.0)
    );
    assert_eq!(
        timeline_process_measured_origin(3, px(72.0), Some((2, px(300.0)))),
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
