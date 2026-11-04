//! Performance probes for the timeline renderer.
//!
//! These are measurement tools, not assertions: they replay a session's payloads
//! through the timeline and report what a frame costs, so a change can be judged
//! by numbers instead of by feel. A normal test run skips them — they are
//! `#[ignore]`d — and the ones that replay a session need one supplied:
//!
//! ```text
//! VIBEX_PROBE_FIXTURE=/tmp/session_payloads.jsonl \
//!   cargo test -p vibex-desktop --lib probe_real_session_frames -- --ignored --nocapture
//! ```
//!
//! A fixture is one `TimelinePayload` JSON per line, in session order. Dumps are
//! **never committed**: they are a reader's own conversation, they live outside
//! the repository, and this module only ever reads the path the operator hands
//! it. Everything the module builds for itself is synthetic.
//!
//! Environment:
//! - `VIBEX_PROBE_FIXTURE` path to the payload dump (required by session probes)
//! - `VIBEX_PROBE_FRAMES`  frames to measure (default 12)
//! - `VIBEX_PROBE_EXPAND`  what to open: `none`, `groups`, `bodies`, `tools`, `all`
//! - `VIBEX_PROBE_SERIES`  print every frame's cost, not just the summary
//!
//! The method that has found this renderer's real costs, in the order it is worth
//! repeating: measure a session at rest and then while scrolling; bisect what is
//! open (groups, bodies, tool cards) with `VIBEX_PROBE_EXPAND`; read how many
//! Markdown surfaces a frame built. A cost that follows neither the screen nor
//! the open disclosures is in the element machinery, and the surface count says
//! which side of the frame is paying for it.

use super::*;
use gpui::{TestAppContext, VisualTestContext};
use std::time::Instant;

struct ProbeWindow {
    workbench: Entity<VibexWorkbench>,
    render_millis: Rc<std::cell::RefCell<Vec<f64>>>,
}

impl Render for ProbeWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let start = Instant::now();
        let timeline = self.workbench.update(cx, |workbench, cx| {
            workbench.render_agent_workbench_for(false, window, cx)
        });
        self.render_millis
            .borrow_mut()
            .push(start.elapsed().as_secs_f64() * 1000.0);
        div().w(px(900.0)).h(px(700.0)).child(timeline)
    }
}

/// The payload dump to replay, when the operator named one.
///
/// The probes skip without it: a fixture is a reader's own conversation, so it
/// is never part of the repository and never defaulted to a path on someone's
/// machine.
fn probe_fixture() -> Option<String> {
    match std::env::var("VIBEX_PROBE_FIXTURE") {
        Ok(path) if !path.trim().is_empty() => Some(path),
        _ => {
            println!("skipped: set VIBEX_PROBE_FIXTURE=<payload dump> to replay a session");
            None
        }
    }
}

/// How many frames a probe measures.
fn probe_frames(default_frames: usize) -> usize {
    std::env::var("VIBEX_PROBE_FRAMES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default_frames)
}

/// Report a probe's frame samples beside the surfaces those frames built.
fn report(label: &str, mut samples: Vec<f64>, mut surfaces: Vec<usize>) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    surfaces.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[(samples.len() as f64 * 0.95) as usize];
    let max = *samples.last().unwrap();
    let surfaces_p50 = surfaces[surfaces.len() / 2];
    let surfaces_max = *surfaces.last().unwrap();
    println!(
        "{label:<28} p50={p50:8.2}ms p95={p95:8.2}ms max={max:8.2}ms  surfaces/frame p50={surfaces_p50} max={surfaces_max}"
    );
}

fn load_items(path: &str) -> Vec<TimelineItem> {
    let raw = std::fs::read_to_string(path).expect("fixture");
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            let payload: TimelinePayload = serde_json::from_str(line).expect("payload");
            TimelineItem {
                id: TimelineItemId::parse(format!("timeline_probe-{index}")).unwrap(),
                session_id: session_id.clone(),
                sequence: index as i64 + 1,
                timestamp_ms: index as i64 + 1,
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
        })
        .collect()
}

fn build(
    cx: &mut TestAppContext,
    items: Vec<TimelineItem>,
    expand: bool,
) -> (
    Entity<VibexWorkbench>,
    Rc<std::cell::RefCell<Vec<f64>>>,
    &mut VisualTestContext,
) {
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let render_millis: Rc<std::cell::RefCell<Vec<f64>>> = Rc::default();
    let render_sink = render_millis.clone();
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let workbench = cx.new(|cx| {
            let mut ui_state = DesktopUiStateV1::default();
            ui_state.appearance.locale = LocaleMode::En;
            ui_state.session.reasoning_display_mode = ReasoningDisplayMode::Timeline;
            ui_state.session.enhanced_command_execution_display = false;
            ui_state.session.turn_preview_rail = false;
            let mut workbench =
                VibexWorkbench::with_initial_state(None, ui_state, None, window, cx);
            workbench.agent_loading = false;
            workbench.selected_session_id = Some(session_id.clone());
            workbench.view_session_id = Some(session_id.clone());
            workbench.timeline.replace_authoritative(session_id, items);
            workbench.rebuild_timeline_sizes();
            if expand {
                let groups = workbench
                    .conversation_turns_cache
                    .iter()
                    .flat_map(|turn| {
                        turn.process_activity_groups_with_commands_and_file_operations
                            .iter()
                            .map(|group| group.id.clone())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                for group in groups {
                    workbench.timeline_command_expansion.insert(group, true);
                }
            }
            workbench.request_timeline_scroll_to_latest();
            workbench
        });
        ProbeWindow {
            workbench,
            render_millis,
        }
    });
    (
        view.read_with(cx, |view, _| view.workbench.clone()),
        render_sink,
        cx,
    )
}

fn measure_minimal(cx: &mut TestAppContext, label: &str, body: &str, frames: usize) {
    struct Minimal {
        body: String,
    }
    impl Render for Minimal {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            TextView::markdown("minimal-md", self.body.clone())
                .selectable(true)
                .text_sm()
                .line_height(px(22.0))
        }
    }
    let body = body.to_string();
    let (_, cx) = cx.add_window_view(move |_, _| Minimal { body });
    for _ in 0..4 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    for _ in 0..frames {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!(
        "{label:<28} mean={mean:8.2}ms min={:8.2}ms max={:8.2}ms",
        samples.first().unwrap(),
        samples.last().unwrap()
    );
}

fn measure_stateful(
    cx: &mut TestAppContext,
    label: &str,
    body: &str,
    frames: usize,
    motion: bool,
    _selectable: bool,
) {
    struct Stateful {
        state: Entity<TextViewState>,
    }
    impl Render for Stateful {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            TextView::new(&self.state).text_sm().line_height(px(22.0))
        }
    }
    let body = body.to_string();
    let (_, cx) = cx.add_window_view(move |_, cx| {
        let body = body.clone();
        let state = cx.new(|cx| {
            let state = TextViewState::markdown(&body, cx);
            if motion {
                state.motion(crate::markdown::timeline_text_motion(false))
            } else {
                state
            }
        });
        Stateful { state }
    });
    for _ in 0..4 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    for _ in 0..frames {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!(
        "{label:<34} mean={mean:8.2}ms min={:8.2}ms max={:8.2}ms",
        samples.first().unwrap(),
        samples.last().unwrap()
    );
}

fn measure(
    workbench: &Entity<VibexWorkbench>,
    cx: &mut VisualTestContext,
    label: &str,
    frames: usize,
) {
    // Warm every cache the first frames populate.
    for _ in 0..3 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    let mut surfaces = Vec::new();
    for _ in 0..frames {
        workbench.update(cx, |_, cx| cx.notify());
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        surfaces.push(workbench.read_with(cx, |workbench, _| workbench.markdown_surfaces_built));
    }
    if std::env::var("VIBEX_PROBE_SERIES").is_ok() {
        println!(
            "{label} series: {:?}",
            samples
                .iter()
                .map(|v| (v * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        );
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let p50 = samples[samples.len() / 2];
    let p95 = samples[(samples.len() as f64 * 0.95) as usize];
    let max = *samples.last().unwrap();
    let (rows, units, groups) = workbench.read_with(cx, |workbench, _| {
        let rows = workbench
            .conversation_turns_cache
            .iter()
            .map(|turn| turn.process_rows.len())
            .sum::<usize>();
        let groups = workbench
            .conversation_turns_cache
            .iter()
            .map(|turn| {
                turn.process_activity_groups_with_commands_and_file_operations
                    .len()
            })
            .sum::<usize>();
        (rows, 0usize, groups)
    });
    surfaces.sort_unstable();
    println!(
        "{label:<28} mean={mean:8.2}ms p50={p50:8.2}ms p95={p95:8.2}ms max={max:8.2}ms  rows={rows} groups={groups} units={units} surfaces/frame p50={} max={}",
        surfaces[surfaces.len() / 2],
        surfaces.last().unwrap()
    );
}

fn measure_in_column(
    cx: &mut TestAppContext,
    label: &str,
    body: &str,
    frames: usize,
    column_width: f32,
) {
    struct InColumn {
        workbench: Entity<MinimalMarkdownHost>,
        column_width: f32,
    }
    struct MinimalMarkdownHost {
        state: Entity<TextViewState>,
    }
    impl Render for MinimalMarkdownHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            TextView::new(&self.state)
                .selectable(true)
                .text_sm()
                .line_height(px(22.0))
        }
    }
    impl Render for InColumn {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // The timeline builds its rows inside a virtual list: each row is
            // requested for layout, measured, and then laid out again.
            v_virtual_list(
                cx.entity(),
                "perf-probe-list",
                Rc::new(vec![size(px(self.column_width), px(400.0))]),
                {
                    let workbench = self.workbench.clone();
                    let column_width = self.column_width;
                    move |_, _range, _window, _cx| {
                        vec![
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .child(
                                    div()
                                        .min_w_0()
                                        .when(column_width > 0.0, |this| this.w(px(column_width)))
                                        .child(workbench.clone()),
                                )
                                .into_any_element(),
                        ]
                    }
                },
            )
        }
    }
    let body = body.to_string();
    let (_, cx) = cx.add_window_view(move |_, cx| {
        let body = body.clone();
        let workbench = cx.new(|cx| {
            let state = cx.new(|cx| TextViewState::markdown(&body, cx));
            MinimalMarkdownHost { state }
        });
        InColumn {
            workbench,
            column_width,
        }
    });
    for _ in 0..4 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    for _ in 0..frames {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!(
        "{label:<40} mean={mean:8.2}ms min={:8.2}ms",
        samples.first().unwrap()
    );
}

/// Reproduces the timeline's virtual-row wrapper exactly: a full-width flex
/// row, centred, holding a `w_full().min_w_0().max_w(cap)` column.
fn measure_timeline_row(
    cx: &mut TestAppContext,
    label: &str,
    body: &str,
    frames: usize,
    row_height: f32,
) {
    struct Host {
        state: Entity<TextViewState>,
        row_height: f32,
    }
    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            h_flex()
                .w_full()
                .h(px(self.row_height))
                .justify_center()
                .items_start()
                .overflow_hidden()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .max_w(px(720.0))
                        .child(markdown_text_view(
                            &self.state,
                            MarkdownPresentation::Agent,
                            cx,
                            |_, _, _, _| {},
                        )),
                )
        }
    }
    let body = body.to_string();
    let (_, cx) = cx.add_window_view(move |_, cx| {
        let body = body.clone();
        let state = cx.new(|cx| TextViewState::markdown(&body, cx));
        Host { state, row_height }
    });
    for _ in 0..4 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    for _ in 0..frames {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!(
        "{label:<32} mean={mean:8.2}ms min={:8.2}ms",
        samples.first().unwrap()
    );
}

/// The timeline's own container: a `v_virtual_list` whose rows are built from
/// the visible range, each row a fixed estimated height.
fn measure_virtual_row(
    cx: &mut TestAppContext,
    label: &str,
    body: &str,
    frames: usize,
    row_height: f32,
) {
    struct Host {
        markdown: Entity<MinimalMarkdownRow>,
        row_height: f32,
    }
    struct MinimalMarkdownRow {
        state: Entity<TextViewState>,
    }
    impl Render for MinimalMarkdownRow {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            markdown_text_view(
                &self.state,
                MarkdownPresentation::Agent,
                cx,
                |_, _, _, _| {},
            )
        }
    }
    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            v_virtual_list(
                cx.entity(),
                "perf-probe-timeline",
                Rc::new(vec![size(px(720.0), px(self.row_height))]),
                {
                    let markdown = self.markdown.clone();
                    let row_height = self.row_height;
                    move |_, _range, _window, _cx| {
                        vec![
                            h_flex()
                                .w_full()
                                .h(px(row_height))
                                .justify_center()
                                .items_start()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .max_w(px(720.0))
                                        .child(markdown.clone()),
                                )
                                .into_any_element(),
                        ]
                    }
                },
            )
        }
    }
    let body = body.to_string();
    let (_, cx) = cx.add_window_view(move |_, cx| {
        let body = body.clone();
        let markdown = cx.new(|cx| {
            let state = cx.new(|cx| TextViewState::markdown(&body, cx));
            MinimalMarkdownRow { state }
        });
        Host {
            markdown,
            row_height,
        }
    });
    for _ in 0..4 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    for _ in 0..frames {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!(
        "{label:<32} mean={mean:8.2}ms min={:8.2}ms",
        samples.first().unwrap()
    );
}

fn measure_app_markdown(cx: &mut TestAppContext, label: &str, body: &str, frames: usize) {
    struct Host {
        state: Entity<TextViewState>,
    }
    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(880.0)).child(markdown_text_view(
                &self.state,
                MarkdownPresentation::Agent,
                cx,
                |_, _, _, _| {},
            ))
        }
    }
    let body = body.to_string();
    let (_, cx) = cx.add_window_view(move |_, cx| {
        let body = body.clone();
        let state = cx.new(|cx| {
            TextViewState::markdown(&body, cx).motion(crate::markdown::timeline_text_motion(false))
        });
        Host { state }
    });
    for _ in 0..4 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    for _ in 0..frames {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!(
        "{label:<40} mean={mean:8.2}ms min={:8.2}ms",
        samples.first().unwrap()
    );
}

#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_app_markdown(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let base =
        "The resolver walks the selection map and returns the resolved adapter for the session.";
    let code = base.replace("selection map", "`selection map`");
    for n in [20, 40] {
        for (tag, para) in [("plain", base), ("code", code.as_str())] {
            let text = vec![para; n].join("\n\n");
            measure_app_markdown(cx, &format!("{tag} {n} paras"), &text, 6);
        }
    }
}

#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_min_content_layout(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let base =
        "The resolver walks the selection map and returns the resolved adapter for the session.";
    let code = base.replace("selection map", "`selection map`");
    for n in [10, 20, 40] {
        let plain = vec![base; n].join("\n\n");
        let coded = vec![code.as_str(); n].join("\n\n");
        measure_timeline_row(cx, &format!("timeline plain {n} paras"), &plain, 6, 600.0);
        measure_timeline_row(cx, &format!("timeline code  {n} paras"), &coded, 6, 600.0);
        // The virtual list gives a row the estimated height, which for an
        // unmeasured turn is the estimator's guess, not the body's real extent.
        measure_timeline_row(cx, &format!("squeezed code  {n} paras"), &coded, 6, 200.0);
        measure_virtual_row(cx, &format!("virtual code  {n} paras"), &coded, 6, 300.0);
        measure_virtual_row(cx, &format!("virtual plain {n} paras"), &plain, 6, 300.0);
    }
}

#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_layout_widths(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let base =
        "The resolver walks the selection map and returns the resolved adapter for the session.";
    let code = base.replace("selection map", "`selection map`");
    let text = vec![code; 40].join("\n\n");
    for width in [900.0, 700.0, 600.0] {
        measure_in_column(cx, &format!("code 40 paras col={width}"), &text, 6, width);
    }
}

#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_minimal_markdown(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let base =
        "The resolver walks the selection map and returns the resolved adapter for the session.";
    let para_plain = base;
    let para_code = base.replace("selection map", "`selection map`");
    for (label, text) in [
        ("plain 40 paras", vec![para_plain; 40].join("\n\n")),
        ("code  40 paras", vec![para_code.clone(); 40].join("\n\n")),
    ] {
        measure_minimal(cx, &format!("minimal   {label}"), &text, 6);
        measure_stateful(
            cx,
            &format!("stateful  {label} motion=0 sel=0"),
            &text,
            6,
            false,
            false,
        );
        measure_stateful(
            cx,
            &format!("stateful  {label} motion=0 sel=1"),
            &text,
            6,
            false,
            true,
        );
        measure_stateful(
            cx,
            &format!("stateful  {label} motion=1 sel=1"),
            &text,
            6,
            true,
            true,
        );
    }
}

#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_real_session_frames(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    for expand in [false, true] {
        let items = load_items(&path);
        let count = items.len();
        let (workbench, render_millis, cx) = build(cx, items, expand);
        let frames = std::env::var("VIBEX_PROBE_FRAMES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(12);
        measure(
            &workbench,
            cx,
            &format!("expand={expand} (n={count})"),
            frames,
        );
        let renders = render_millis.borrow();
        if renders.len() > 4 {
            let tail = &renders[renders.len() - 5..];
            println!(
                "   render-only tail: {:?}",
                tail.iter()
                    .map(|v| (v * 10.0).round() / 10.0)
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// Probe: the same payloads drawn in a narrow pane.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_narrow_pane_frames(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let width = std::env::var("VIBEX_PROBE_WIDTH")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(720.0);
    let items = load_items(&path);
    let count = items.len();
    let (workbench, _render_millis, cx) = build(cx, items, false);
    let window_handle = cx.update(|window, _| window.window_handle());
    cx.simulate_window_resize(window_handle, gpui::size(gpui::px(width), gpui::px(900.0)));
    measure(&workbench, cx, &format!("pane={width} (n={count})"), 10);
}

/// Probe: appends one item per frame, the way streaming does.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_streaming_frames(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let all = load_items(&path);
    let split = std::env::var("VIBEX_PROBE_SPLIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(all.len() - 1);
    let (initial, tail) = all.split_at(split.min(all.len()));
    let (workbench, _render_millis, cx) = build(cx, initial.to_vec(), false);
    for expand in [false, true] {
        let _ = expand;
    }
    // Warm.
    for _ in 0..3 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut steady = Vec::new();
    let mut appended = Vec::new();
    for item in tail.iter() {
        // Steady frame first: nothing changed.
        let start = Instant::now();
        workbench.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        steady.push(start.elapsed().as_secs_f64() * 1000.0);

        let next = item.clone();
        workbench.update(cx, |workbench, cx| {
            workbench
                .timeline
                .apply_live(vibex_core::TimelineLiveEvent {
                    session_id: next.session_id.clone(),
                    sequence: next.sequence,
                    item: next.clone(),
                });
            workbench.rebuild_timeline_sizes();
            cx.notify();
        });
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        appended.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mean = |v: &[f64]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    println!(
        "steady   mean={:8.2}ms max={:8.2}ms n={}",
        mean(&steady),
        steady.iter().cloned().fold(0.0, f64::max),
        steady.len()
    );
    println!(
        "appended mean={:8.2}ms max={:8.2}ms n={}",
        mean(&appended),
        appended.iter().cloned().fold(0.0, f64::max),
        appended.len()
    );
}

/// Probe: the first frames after the view is built, no warm-up.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_cold_switch_frames(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let items = load_items(&path);
    let count = items.len();
    let (workbench, _r, cx) = build(cx, items, false);
    let mut samples = Vec::new();
    for _ in 0..14 {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "cold n={count}: {:?}",
        samples
            .iter()
            .map(|v| (v * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    let _ = workbench;
}

/// Probe: streaming appends into the long answer, cold.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_stream_answer(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let items = load_items(&path);
    let split = items.len().saturating_sub(2);
    let (initial, _) = items.split_at(split);
    let (workbench, _r, cx) = build(cx, initial.to_vec(), false);
    let full = std::env::var("VIBEX_PROBE_ANSWER")
        .ok()
        .and_then(|v| std::fs::read_to_string(v).ok())
        .unwrap_or_default();
    // Append the answer in chunks of N chars, one frame each.
    let chunk = std::env::var("VIBEX_PROBE_CHUNK")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(200);
    let mut samples = Vec::new();
    for piece in full.as_bytes().chunks(chunk) {
        let text = String::from_utf8_lossy(piece).to_string();
        let seq = 999_999;
        let mut item = initial.last().cloned().unwrap();
        item.sequence = seq;
        item.timestamp_ms = seq;
        item.payload = vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
            text,
            is_final: false,
        });
        workbench.update(cx, |workbench, cx| {
            workbench
                .timeline
                .apply_live(vibex_core::TimelineLiveEvent {
                    session_id: item.session_id.clone(),
                    sequence: seq,
                    item: item.clone(),
                });
            workbench.rebuild_timeline_sizes();
            cx.notify();
        });
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "answer chunks={} first10={:?} mean={:.2}",
        samples.len(),
        samples
            .iter()
            .take(10)
            .map(|v| (v * 10.0).round() / 10.0)
            .collect::<Vec<_>>(),
        samples.iter().sum::<f64>() / samples.len().max(1) as f64
    );
}

/// Probe: draws with and without an invalidation.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_draw_without_notify(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let items = load_items(&path);
    let (workbench, _r, cx) = build(cx, items, false);
    for _ in 0..3 {
        workbench.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut quiet = Vec::new();
    for _ in 0..6 {
        let start = Instant::now();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        quiet.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mut noisy = Vec::new();
    for _ in 0..6 {
        workbench.update(cx, |_, cx| cx.notify());
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        noisy.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    println!(
        "quiet mean={:.2}ms  notify mean={:.2}ms",
        mean(&quiet),
        mean(&noisy)
    );
}

/// Probe: the answer row visible, then new rows appended.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_append_with_answer_visible(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let all = load_items(&path);
    let split = std::env::var("VIBEX_PROBE_SPLIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(all.len());
    let (initial, tail) = all.split_at(split.min(all.len()));
    let (workbench, _r, cx) = build(cx, initial.to_vec(), false);
    for _ in 0..4 {
        workbench.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut steady = Vec::new();
    for _ in 0..5 {
        workbench.update(cx, |_, cx| cx.notify());
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        steady.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mut appended = Vec::new();
    let last = initial.last().cloned();
    for (offset, _) in tail.iter().enumerate().take(6) {
        let mut item = last.clone().unwrap();
        item.sequence = 900_000 + offset as i64;
        item.timestamp_ms = item.sequence;
        workbench.update(cx, |workbench, cx| {
            workbench
                .timeline
                .apply_live(vibex_core::TimelineLiveEvent {
                    session_id: item.session_id.clone(),
                    sequence: item.sequence,
                    item: item.clone(),
                });
            workbench.rebuild_timeline_sizes();
            cx.notify();
        });
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        appended.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    println!(
        "steady mean={:.2}ms {:?}",
        mean(&steady),
        steady
            .iter()
            .map(|v| (v * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    println!(
        "append mean={:.2}ms {:?}",
        mean(&appended),
        appended
            .iter()
            .map(|v| (v * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
}

/// Probe: two draws back to back, same content.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_double_draw(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let items = load_items(&path);
    let (workbench, _r, cx) = build(cx, items, false);
    for _ in 0..3 {
        workbench.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    for pass in 0..3 {
        workbench.update(cx, |_, cx| cx.notify());
        cx.update(|window, _| {
            window.refresh();
        });
        let mut times = Vec::new();
        for _ in 0..2 {
            let start = Instant::now();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "pass {pass}: first={:.2}ms second={:.2}ms",
            times[0], times[1]
        );
    }
}

/// Probe: the answer row on screen while its text keeps growing.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_growing_answer_visible(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let items = load_items(&path);
    let seq = std::env::var("VIBEX_PROBE_SEQ")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(206331);
    // The fixture renumbers sequences from 1, so index by payload kind instead.
    let full = items
        .iter()
        .rev()
        .find_map(|item| match &item.payload {
            vibex_core::TimelinePayload::AgentMessage(message) => Some(message.text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    println!("full answer len={}", full.len());
    // Start with a short answer in place, so the row already occupies a turn.
    let mut initial = items.clone();
    let mut stub = items.last().cloned().unwrap();
    stub.sequence = seq;
    stub.timestamp_payload_stub();
    initial.retain(|item| item.sequence != seq);
    let (workbench, _r, cx) = build(cx, initial, false);
    workbench.update(cx, |workbench, cx| {
        workbench.request_timeline_scroll_to_latest();
        workbench.rebuild_timeline_sizes();
        cx.notify();
    });
    for _ in 0..4 {
        workbench.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let chunk = std::env::var("VIBEX_PROBE_CHUNK")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(400);
    let mut samples = Vec::new();
    let mut len: usize;
    let mut boundaries = Vec::new();
    let mut cursor = 0usize;
    for piece in full.as_bytes().chunks(chunk) {
        let _ = piece;
        cursor = (cursor + chunk).min(full.len());
        while cursor < full.len() && !full.is_char_boundary(cursor) {
            cursor += 1;
        }
        boundaries.push(cursor);
    }
    for cursor in boundaries {
        len = cursor;
        let mut item = stub.clone();
        item.payload = vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
            text: full[..len].to_string(),
            is_final: false,
        });
        workbench.update(cx, |workbench, cx| {
            workbench
                .timeline
                .apply_live(vibex_core::TimelineLiveEvent {
                    session_id: item.session_id.clone(),
                    sequence: seq,
                    item: item.clone(),
                });
            workbench.rebuild_timeline_sizes();
            cx.notify();
        });
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push((len, start.elapsed().as_secs_f64() * 1000.0));
    }
    println!(
        "growing chunks={:?}",
        samples
            .iter()
            .map(|(len, ms)| (len, (ms * 10.0).round() / 10.0))
            .collect::<Vec<_>>()
    );
}

trait StubTimestamp {
    fn timestamp_payload_stub(&mut self);
}
impl StubTimestamp for TimelineItem {
    fn timestamp_payload_stub(&mut self) {
        self.timestamp_ms = self.sequence;
    }
}

/// Probe: characterise what makes an inline-code span expensive.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_span_shape(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let orig = [(
        "agent_message",
        serde_json::json!({"type":"agent_message","data":{"text":"x","isFinal":true}}),
    )];
    let _ = orig;
    let template = r#"{"type":"agent_message","data":{"text":"","isFinal":true}}"#;
    let make = |text: String| {
        serde_json::from_str::<TimelinePayload>(&template.replace(
            r#""text":"""#,
            &format!("\"text\":{}", serde_json::to_string(&text).unwrap()),
        ))
        .unwrap()
    };
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let build_items = |payload: TimelinePayload| {
        vec![TimelineItem {
            id: TimelineItemId::parse("timeline_probe-0").unwrap(),
            session_id: session_id.clone(),
            sequence: 1,
            timestamp_ms: 1,
            source: TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
            payload,
        }]
    };
    let cases: Vec<(String, String)> = vec![
        ("code 512".into(), format!("`{}`", "z".repeat(512))),
        ("code 1024".into(), format!("`{}`", "z".repeat(1024))),
        (
            "code 512 x2".into(),
            format!("`{}`\n\n`{}`", "z".repeat(512), "z".repeat(512)),
        ),
        ("bold 1024".into(), format!("**{}**", "z".repeat(1024))),
        (
            "code 16 chars x 32".into(),
            (0..32)
                .map(|_| format!("`{}`", "z".repeat(16)))
                .collect::<Vec<_>>()
                .join(" "),
        ),
    ];
    for (label, text) in cases {
        let items = build_items(make(text));
        let (workbench, _r, cx) = build(cx, items, false);
        for _ in 0..2 {
            workbench.update(cx, |_, cx| cx.notify());
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
        let mut samples = Vec::new();
        for _ in 0..4 {
            workbench.update(cx, |_, cx| cx.notify());
            let start = Instant::now();
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        println!("{label:<24} mean={mean:8.2}ms");
    }
}

/// Probe: gpui-base alone, one long inline-code span in a virtual row.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_base_span(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for (label, text) in [
        ("plain 4000", "z".repeat(4000)),
        ("code 4000", format!("`{}`", "z".repeat(4000))),
        ("code 4000 selectable", format!("`{}`", "z".repeat(4000))),
    ] {
        let selectable = label.ends_with("selectable");
        measure_virtual_text(cx, label, &text, 5, selectable);
    }
}

fn measure_virtual_text(
    cx: &mut TestAppContext,
    label: &str,
    body: &str,
    frames: usize,
    selectable: bool,
) {
    struct Host {
        state: Entity<TextViewState>,
        selectable: bool,
    }
    impl Render for Host {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(720.0)).child(
                TextView::new(&self.state)
                    .selectable(self.selectable)
                    .text_sm()
                    .line_height(px(22.0)),
            )
        }
    }
    let body = body.to_string();
    let (_, cx) = cx.add_window_view(move |_, cx| {
        let body = body.clone();
        let state = cx.new(|cx| TextViewState::markdown(&body, cx));
        Host { state, selectable }
    });
    for _ in 0..3 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    for _ in 0..frames {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!("{label:<26} mean={mean:8.2}ms");
}

/// Probe: bisect which wrapper makes one long code span expensive.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_wrapper_bisect(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let body = format!("`{}`", "z".repeat(4000));
    for shape in 0..4 {
        measure_wrapped(cx, &body, shape);
    }
}

fn measure_wrapped(cx: &mut TestAppContext, body: &str, shape: u8) {
    struct Host {
        state: Entity<TextViewState>,
        shape: u8,
    }
    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = TextView::new(&self.state)
                .selectable(true)
                .text_sm()
                .line_height(px(22.0));
            match self.shape {
                0 => div().w(px(720.0)).child(view).into_any_element(),
                1 => h_flex()
                    .w_full()
                    .h(px(600.0))
                    .justify_center()
                    .items_start()
                    .overflow_hidden()
                    .child(div().w_full().min_w_0().max_w(px(720.0)).child(view))
                    .into_any_element(),
                _ => v_virtual_list(
                    cx.entity(),
                    format!("probe-list-{}", self.shape),
                    Rc::new(vec![size(px(720.0), px(600.0))]),
                    {
                        let state = self.state.clone();
                        move |_, _range, _window, _cx| {
                            vec![
                                h_flex()
                                    .w_full()
                                    .h(px(600.0))
                                    .justify_center()
                                    .items_start()
                                    .overflow_hidden()
                                    .child(
                                        div().w_full().min_w_0().max_w(px(720.0)).child(
                                            TextView::new(&state)
                                                .selectable(true)
                                                .text_sm()
                                                .line_height(px(22.0)),
                                        ),
                                    )
                                    .into_any_element(),
                            ]
                        }
                    },
                )
                .into_any_element(),
            }
        }
    }
    let body = body.to_string();
    let shape_copy = shape;
    let (_, cx) = cx.add_window_view(move |_, cx| {
        let body = body.clone();
        let state = cx.new(|cx| TextViewState::markdown(&body, cx));
        Host { state, shape }
    });
    let _ = shape_copy;
    for _ in 0..3 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let mut samples = Vec::new();
    for _ in 0..4 {
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!("shape {shape}: mean={mean:8.2}ms");
}

/// Probe: the reader's report — a session with everything open, scrolling.
///
/// The shape that matters once bodies are revealed: what a frame costs while the
/// screen moves through long prose, and how many surfaces that frame builds.
/// `VIBEX_PROBE_EXPAND` bisects what is open, because the answer is rarely "all
/// of it": on the session this was written against, groups and tool cards were
/// free (about 19 ms a frame at rest) and revealed bodies carried the whole cost
/// (98 ms), which the surface count then confirmed.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_expanded_scroll(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let expand = std::env::var("VIBEX_PROBE_EXPAND").unwrap_or_else(|_| "all".to_string());
    let open_groups = matches!(expand.as_str(), "all" | "groups");
    let open_bodies = matches!(expand.as_str(), "all" | "bodies");
    let open_tools = matches!(expand.as_str(), "all" | "tools");
    let items = load_items(&path);
    let count = items.len();
    let (workbench, _render_millis, cx) = build(cx, items, open_groups);
    workbench.update(cx, |workbench, _| {
        let rows: Vec<String> = workbench
            .conversation_turns_cache
            .iter()
            .flat_map(|turn| {
                turn.user_row
                    .iter()
                    .chain(turn.process_rows.iter())
                    .chain(turn.conclusion_row.iter())
                    .map(|row| row.id.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        for row in rows {
            if open_bodies {
                workbench.timeline_body_reveal.insert(row.clone(), true);
            }
            if open_tools {
                workbench.timeline_command_expansion.insert(row, true);
            }
        }
        workbench.conversation_turns_cache_key = None;
        workbench.rebuild_timeline_sizes();
    });
    for _ in 0..3 {
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }
    let viewport = workbench.read_with(cx, |workbench, _| {
        f32::from(workbench.timeline_scroll.bounds().size.height)
    });
    let max_offset = workbench.read_with(cx, |workbench, _| {
        f32::from(workbench.timeline_scroll.max_offset().y)
    });
    let step = (viewport / 3.0).max(120.0);
    let mut samples = Vec::new();
    let mut surfaces = Vec::new();
    let mut offset = 0.0_f32;
    for _ in 0..probe_frames(24) {
        offset = (offset + step).min(max_offset);
        workbench.update(cx, |workbench, cx| {
            workbench
                .timeline_scroll
                .set_offset(point(px(0.0), px(-offset)));
            cx.notify();
        });
        let start = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        surfaces.push(workbench.read_with(cx, |workbench, _| workbench.markdown_surfaces_built));
    }
    println!("probe fixture n={count} expand={expand} content={max_offset:.0}px");
    report(&format!("expanded scroll ({expand})"), samples, surfaces);
}

/// Probe: how the projection folds a run of message deltas.
///
/// A message a provider narrates as chunks must read as one body. When it does
/// not, the transcript shows the stream's own chunk boundaries — one short line
/// per chunk, sometimes cut mid-word — which is a projection question, not a
/// rendering one. This prints the AgentMessage rows the fixture projects: how
/// many there are, how long each body is, and how many items each absorbed.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_message_delta_folding(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let Some(path) = probe_fixture() else {
        return;
    };
    let items = load_items(&path);
    let (workbench, _render_millis, cx) = build(cx, items, false);
    workbench.read_with(cx, |workbench, _| {
        let mut rows = Vec::new();
        for turn in workbench.conversation_turns_cache.iter() {
            for row in turn.process_rows.iter().chain(turn.conclusion_row.iter()) {
                if row.kind != TimelineRowKind::AgentMessage {
                    continue;
                }
                rows.push((
                    row.body.len(),
                    row.item_ids.len(),
                    row.streaming,
                    row.conclusion,
                ));
            }
        }
        let absorbed = rows.iter().map(|(_, items, _, _)| *items).sum::<usize>();
        println!(
            "probe message rows: {} for {} items (absorbed {absorbed})",
            rows.len(),
            workbench.timeline.items.len()
        );
        for (index, (len, items, streaming, conclusion)) in rows.iter().enumerate().take(24) {
            println!(
                "  row {index}: body={len} items={items} streaming={streaming} conclusion={conclusion}"
            );
        }
    });
}

/// Probe: what an unrevealed long body costs a frame, by its size.
///
/// The fold allowance is a reading choice only if a body that is *not* folded is
/// still laid out as chunks: an unrevealed row draws its head, and the head is as
/// long as the allowance. This measures what that costs at each size, so raising
/// the allowance stays a decision about reading rather than a guess about frames.
/// `VIBEX_PROBE_SIZES` sets the sizes in bytes (default
/// 4096,8192,12288,16384,32768); the label reports the head actually drawn, which
/// is the body until it passes the fold allowance and the allowance after that.
#[gpui::test]
#[ignore = "performance probe: --ignored to run"]
fn probe_unrevealed_body_frames(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let session_id = VibexSessionId::parse("session_layout").unwrap();
    let sizes = std::env::var("VIBEX_PROBE_SIZES")
        .unwrap_or_else(|_| "4096,8192,12288,16384,32768".to_string())
        .split(',')
        .filter_map(|value| value.trim().parse::<usize>().ok())
        .collect::<Vec<_>>();
    for bytes in sizes {
        let body = synthetic_body(bytes);
        let head_bytes = folded_body_parts(&body).0.len();
        let items = vec![
            probe_item(
                1,
                TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                    text: "question".into(),
                    attachments: Vec::new(),
                    ..Default::default()
                }),
            ),
            probe_item(
                2,
                TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                    text: body,
                    is_final: true,
                }),
            ),
        ];
        let (workbench, render_millis, cx) = build(cx, items, false);
        // A finished session: the answer is settled, so the row folds.
        workbench.update(cx, |workbench, _| {
            workbench.upsert_session_snapshot(AgentSession {
                id: session_id.clone(),
                title: "Probe".into(),
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
                tools: Default::default(),
                transport: vibex_core::SessionTransport::Acp,
                terminal: None,
            });
            workbench.conversation_turns_cache_key = None;
            workbench.rebuild_timeline_sizes();
        });
        measure(
            &workbench,
            cx,
            &format!("unrevealed head {head_bytes} B of {bytes} B"),
            10,
        );
        drop(render_millis);
    }
}

/// One timeline item of a probe's own making.
fn probe_item(sequence: i64, payload: TimelinePayload) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::parse(format!("timeline_probe-{sequence}")).unwrap(),
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

/// A body of exactly `bytes` bytes of prose.
fn synthetic_body(bytes: usize) -> String {
    let mut body = String::with_capacity(bytes + 64);
    let mut line = 0_usize;
    while body.len() < bytes {
        body.push_str(&format!(
            "line {line} of a settled answer that is read folded\n"
        ));
        line += 1;
    }
    let cut = vibex_ui::markdown::utf8_prefix(&body, bytes).len();
    body.truncate(cut);
    body
}
