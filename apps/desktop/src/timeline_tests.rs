use super::*;
use gpui::{Modifiers, TestAppContext};

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
