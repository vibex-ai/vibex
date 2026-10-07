use super::*;
use gpui::{Modifiers, TestAppContext};

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
