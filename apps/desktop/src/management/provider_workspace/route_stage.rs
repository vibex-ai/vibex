//! The gateway's route map: where each request went, drawn as it happens.
//!
//! Agents sit on the left, the gateway in the middle and the Provider Keys
//! it can try on the right, joined by wires. A request is a dot in its
//! Agent's colour. It flies to the gateway, on to the member routing put
//! first, and waits there while that member answers. An answer flies home as
//! a hollow dot; a failure flashes at the member, the dot returns to the
//! gateway and goes on to the next one. A request picked from the list is
//! replayed the same way, from what the gateway recorded about it.
//!
//! The map is its own view, embedded cached: it asks for frames only while
//! something on it moves, and those frames repaint the map alone.

use std::time::Instant;

use super::*;

/// Horizontal anchors, fixed so the wires painted on the canvas meet the
/// node elements laid out over it. They are geometry shared by two layers,
/// not spacing.
const AGENT_W: f32 = 176.0;
const HUB_X: f32 = AGENT_W + 84.0;
const HUB_W: f32 = 132.0;
const HUB_H: f32 = 64.0;
const TARGET_X: f32 = HUB_X + HUB_W + 84.0;
const ROW_H: f32 = 48.0;
const NODE_H: f32 = 38.0;
const PAD_Y: f32 = 14.0;

/// Leg durations of a replay, in milliseconds.
const LEG_IN: f32 = 520.0;
const LEG_OUT: f32 = 620.0;
const LEG_HOLD: f32 = 520.0;
const LEG_BACK: f32 = 460.0;
const LEG_HOME: f32 = 560.0;
/// How long a finished live request stays lit after its dot is home.
const LINGER_MS: f32 = 2_400.0;

#[derive(Clone, PartialEq)]
pub(crate) struct StageAgent {
    pub id: String,
    pub label: String,
}

#[derive(Clone, PartialEq)]
pub(crate) enum TargetState {
    Ready,
    Cooling(SharedString),
    Invalid,
    /// Not tried: the member was stepped over, and why.
    Skipped(SharedString),
}

#[derive(Clone, PartialEq)]
pub(crate) struct StageTarget {
    pub profile: Option<vibex_core::ModelProviderProfile>,
    pub title: String,
    pub subtitle: String,
    pub state: TargetState,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum HopResult {
    Pending,
    Ok,
    Failed,
}

#[derive(Clone, PartialEq)]
pub(crate) struct StageHop {
    pub target: usize,
    pub result: HopResult,
    /// The tag the target shows once this hop is decided ("429 · rate limited").
    pub tag: SharedString,
}

#[derive(Clone, PartialEq)]
pub(crate) enum FlightPhase {
    /// Running now: started at this wall-clock time, streaming once the first
    /// token arrived.
    Live { started_ms: i64, streaming: bool },
    /// Finished a moment ago while the map was live.
    Landed { ended_ms: i64, ok: bool },
    /// A request picked from the list, replayed from its record.
    Replay,
}

#[derive(Clone, PartialEq)]
pub(crate) struct StageFlight {
    pub id: String,
    pub agent: usize,
    pub hops: Vec<StageHop>,
    pub phase: FlightPhase,
}

#[derive(Clone, PartialEq, Default)]
pub(crate) struct StageModel {
    pub agents: Vec<StageAgent>,
    pub targets: Vec<StageTarget>,
    pub hub_title: String,
    pub hub_running: bool,
    pub flights: Vec<StageFlight>,
    /// The request being replayed, when one is picked.
    pub pinned: Option<String>,
}

pub(crate) struct RouteStage {
    model: StageModel,
    replay_started: Option<Instant>,
}

impl RouteStage {
    pub(crate) fn new() -> Self {
        Self {
            model: StageModel::default(),
            replay_started: None,
        }
    }

    /// Takes the latest picture of the gateway. A newly picked request starts
    /// its replay from the top.
    pub(crate) fn set_model(&mut self, model: StageModel, cx: &mut Context<Self>) {
        if model == self.model {
            return;
        }
        if model.pinned != self.model.pinned {
            self.replay_started = model.pinned.as_ref().map(|_| Instant::now());
        }
        self.model = model;
        cx.notify();
    }

    /// Plays the picked request again.
    pub(crate) fn replay(&mut self, cx: &mut Context<Self>) {
        if self.model.pinned.is_some() {
            self.replay_started = Some(Instant::now());
            cx.notify();
        }
    }

    fn stage_height(&self) -> f32 {
        let rows = self.model.agents.len().max(self.model.targets.len()).max(2) as f32;
        PAD_Y * 2.0 + rows * ROW_H
    }
}

/// A wire's two ends.
#[derive(Clone, Copy)]
struct Wire {
    from: gpui::Point<Pixels>,
    to: gpui::Point<Pixels>,
}

impl Wire {
    fn at(&self, t: f32) -> gpui::Point<Pixels> {
        let t = t.clamp(0.0, 1.0);
        let mx = (self.from.x + self.to.x) / 2.0;
        let p0 = self.from;
        let p1 = point(mx, self.from.y);
        let p2 = point(mx, self.to.y);
        let p3 = self.to;
        let u = 1.0 - t;
        let a = u * u * u;
        let b = 3.0 * u * u * t;
        let c = 3.0 * u * t * t;
        let d = t * t * t;
        point(
            p0.x * a + p1.x * b + p2.x * c + p3.x * d,
            p0.y * a + p1.y * b + p2.y * c + p3.y * d,
        )
    }

    fn path(&self, width: Pixels, dashed: bool) -> Option<gpui::Path<Pixels>> {
        let mx = (self.from.x + self.to.x) / 2.0;
        let mut builder = PathBuilder::stroke(width);
        if dashed {
            builder = builder.dash_array(&[px(3.0), px(4.0)]);
        }
        builder.move_to(self.from);
        builder.cubic_bezier_to(self.to, point(mx, self.from.y), point(mx, self.to.y));
        builder.build().ok()
    }
}

/// Where a dot is drawn and how.
#[derive(Clone, Copy, PartialEq)]
enum DotStyle {
    Out,
    Held,
    HomeOk,
    HomeErr,
}

/// What one flight looks like at one moment.
#[derive(Default)]
struct FlightFrame {
    /// The wire a dot is on and how far along, or a target it waits at.
    dot: Option<(DotPlace, DotStyle)>,
    /// Hops whose answer is known by now.
    revealed: usize,
    /// Streaming particles flow home along these wires.
    streaming: bool,
    /// The flight is over and has faded out.
    gone: bool,
    /// How strongly its wires are lit, 0 to 1.
    glow: f32,
}

#[derive(Clone, Copy)]
enum DotPlace {
    AgentWire { t: f32 },
    TargetWire { target: usize, t: f32 },
    At { target: usize },
}

/// A replay's legs, in order: which wire, which way, for how long.
#[derive(Clone, Copy)]
enum Leg {
    In,
    Out(usize),
    Hold(usize, usize),
    Back(usize, bool),
    Home(usize, bool),
}

fn legs(flight: &StageFlight) -> Vec<(Leg, f32)> {
    let mut legs = vec![(Leg::In, LEG_IN)];
    let last = flight.hops.len().saturating_sub(1);
    for (index, hop) in flight.hops.iter().enumerate() {
        legs.push((Leg::Out(hop.target), LEG_OUT));
        legs.push((Leg::Hold(hop.target, index), LEG_HOLD));
        let ok = hop.result == HopResult::Ok;
        if index < last {
            legs.push((Leg::Back(hop.target, ok), LEG_BACK));
        } else {
            legs.push((Leg::Back(hop.target, ok), LEG_BACK));
            legs.push((Leg::Home(hop.target, ok), LEG_HOME));
        }
    }
    legs
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn replay_frame(flight: &StageFlight, elapsed: f32) -> FlightFrame {
    let mut at = 0.0;
    let mut revealed = 0;
    for (leg, duration) in legs(flight) {
        if elapsed < at + duration {
            let t = ease((elapsed - at) / duration);
            let dot = match leg {
                Leg::In => (DotPlace::AgentWire { t }, DotStyle::Out),
                Leg::Out(target) => (DotPlace::TargetWire { target, t }, DotStyle::Out),
                Leg::Hold(target, _) => (DotPlace::At { target }, DotStyle::Held),
                Leg::Back(target, ok) => (
                    DotPlace::TargetWire { target, t: 1.0 - t },
                    if ok {
                        DotStyle::HomeOk
                    } else {
                        DotStyle::HomeErr
                    },
                ),
                Leg::Home(_, ok) => (
                    DotPlace::AgentWire { t: 1.0 - t },
                    if ok {
                        DotStyle::HomeOk
                    } else {
                        DotStyle::HomeErr
                    },
                ),
            };
            return FlightFrame {
                dot: Some(dot),
                revealed,
                streaming: false,
                gone: false,
                glow: 1.0,
            };
        }
        at += duration;
        if let Leg::Hold(_, index) = leg {
            revealed = index + 1;
        }
    }
    FlightFrame {
        dot: None,
        revealed: flight.hops.len(),
        streaming: false,
        gone: false,
        glow: 1.0,
    }
}

fn flight_frame(
    flight: &StageFlight,
    now_ms: i64,
    replay_elapsed: f32,
    reduce_motion: bool,
) -> FlightFrame {
    match flight.phase {
        FlightPhase::Replay => {
            if reduce_motion {
                return FlightFrame {
                    dot: None,
                    revealed: flight.hops.len(),
                    glow: 1.0,
                    ..FlightFrame::default()
                };
            }
            replay_frame(flight, replay_elapsed)
        }
        FlightPhase::Live {
            started_ms,
            streaming,
        } => {
            let Some(current) = flight.hops.last() else {
                return FlightFrame {
                    dot: Some((DotPlace::AgentWire { t: 1.0 }, DotStyle::Held)),
                    glow: 1.0,
                    ..FlightFrame::default()
                };
            };
            let elapsed = (now_ms - started_ms).max(0) as f32;
            let revealed = flight.hops.len() - 1;
            let dot = if reduce_motion || elapsed >= LEG_IN + LEG_OUT {
                (
                    DotPlace::At {
                        target: current.target,
                    },
                    DotStyle::Held,
                )
            } else if elapsed < LEG_IN {
                (
                    DotPlace::AgentWire {
                        t: ease(elapsed / LEG_IN),
                    },
                    DotStyle::Out,
                )
            } else {
                (
                    DotPlace::TargetWire {
                        target: current.target,
                        t: ease((elapsed - LEG_IN) / LEG_OUT),
                    },
                    DotStyle::Out,
                )
            };
            FlightFrame {
                dot: Some(dot),
                revealed,
                streaming: streaming && !reduce_motion,
                gone: false,
                glow: 1.0,
            }
        }
        FlightPhase::Landed { ended_ms, ok } => {
            let elapsed = (now_ms - ended_ms).max(0) as f32;
            let Some(last) = flight.hops.last() else {
                return FlightFrame {
                    gone: true,
                    ..FlightFrame::default()
                };
            };
            let style = if ok {
                DotStyle::HomeOk
            } else {
                DotStyle::HomeErr
            };
            let travel = LEG_BACK + LEG_HOME;
            let dot = if reduce_motion || elapsed >= travel {
                None
            } else if elapsed < LEG_BACK {
                Some((
                    DotPlace::TargetWire {
                        target: last.target,
                        t: 1.0 - ease(elapsed / LEG_BACK),
                    },
                    style,
                ))
            } else {
                Some((
                    DotPlace::AgentWire {
                        t: 1.0 - ease((elapsed - LEG_BACK) / LEG_HOME),
                    },
                    style,
                ))
            };
            let fade = ((elapsed - travel) / LINGER_MS).clamp(0.0, 1.0);
            FlightFrame {
                dot,
                revealed: flight.hops.len(),
                streaming: false,
                gone: fade >= 1.0,
                glow: 1.0 - fade,
            }
        }
    }
}

/// The colour an Agent's requests fly in: one of the theme's chart hues,
/// chosen by its id so it stays the same from one request to the next. None
/// is the danger colour, which on this map means a failure.
pub(crate) fn agent_hue(agent_id: &str, cx: &App) -> Hsla {
    let theme = cx.theme();
    let palette = [
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
        theme.blue,
        theme.cyan,
        theme.magenta,
    ];
    let hash = agent_id.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    });
    palette[hash as usize % palette.len()]
}

impl Render for RouteStage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let height = self.stage_height();
        let hub_y = height / 2.0;
        let now_ms = vibex_core::unix_timestamp_ms();
        let reduce_motion =
            cx.reduce_motion() || crate::motion::pauses_while_inactive(!window.is_window_active());
        let replay_elapsed = self
            .replay_started
            .map(|started| started.elapsed().as_secs_f32() * 1000.0)
            .unwrap_or(f32::MAX);

        let agent_y = |index: usize| PAD_Y + index as f32 * ROW_H + ROW_H / 2.0;
        let target_y = |index: usize| PAD_Y + index as f32 * ROW_H + ROW_H / 2.0;

        let frames = self
            .model
            .flights
            .iter()
            .map(|flight| flight_frame(flight, now_ms, replay_elapsed, reduce_motion))
            .collect::<Vec<_>>();
        // Frames keep coming while anything moves: a dot in flight, a member
        // waiting (its pulse), a stream, or a landing fading out.
        let animating = !reduce_motion
            && frames
                .iter()
                .zip(&self.model.flights)
                .any(|(frame, flight)| {
                    !frame.gone
                        && (frame.dot.is_some()
                            || frame.streaming
                            || matches!(flight.phase, FlightPhase::Landed { .. }))
                });
        if animating {
            window.request_animation_frame();
        }

        // What each target shows now: a tag for a decided hop, a highlight for
        // a member being waited on.
        let mut target_tags: Vec<Option<(SharedString, HopResult)>> =
            vec![None; self.model.targets.len()];
        let mut target_waiting = vec![None::<Hsla>; self.model.targets.len()];
        let mut agent_active = vec![None::<Hsla>; self.model.agents.len()];
        for (flight, frame) in self.model.flights.iter().zip(&frames) {
            if frame.gone {
                continue;
            }
            let hue = self
                .model
                .agents
                .get(flight.agent)
                .map(|agent| agent_hue(&agent.id, cx))
                .unwrap_or(cx.theme().muted_foreground);
            if let Some(slot) = agent_active.get_mut(flight.agent) {
                *slot = Some(hue);
            }
            for hop in flight.hops.iter().take(frame.revealed) {
                if let Some(slot) = target_tags.get_mut(hop.target) {
                    *slot = Some((hop.tag.clone(), hop.result));
                }
            }
            if let Some((DotPlace::At { target }, _)) = frame.dot
                && let Some(slot) = target_waiting.get_mut(target)
            {
                *slot = Some(hue);
            }
        }

        // ----- the canvas: wires, dots, the gateway's pulse ---------------
        let model = self.model.clone();
        let theme = cx.theme().clone();
        let hues = model
            .agents
            .iter()
            .map(|agent| agent_hue(&agent.id, cx))
            .collect::<Vec<_>>();
        let pulse_phase = (now_ms % 1600) as f32 / 1600.0;
        let any_live = model
            .flights
            .iter()
            .any(|flight| matches!(flight.phase, FlightPhase::Live { .. }));
        let canvas_layer = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let origin = bounds.origin;
                let at = |x: f32, y: f32| point(origin.x + px(x), origin.y + px(y));
                let hub_left = at(HUB_X, hub_y);
                let hub_right = at(HUB_X + HUB_W, hub_y);
                let agent_wire = |index: usize| Wire {
                    from: at(AGENT_W, agent_y(index)),
                    to: hub_left,
                };
                let target_wire = |index: usize| Wire {
                    from: hub_right,
                    to: at(TARGET_X, target_y(index)),
                };

                // Lit wires, by flight.
                let mut lit_agents = vec![None::<(Hsla, f32)>; model.agents.len()];
                let mut lit_targets = vec![None::<(Hsla, f32, bool)>; model.targets.len()];
                for (flight, frame) in model.flights.iter().zip(&frames) {
                    if frame.gone {
                        continue;
                    }
                    let hue = hues
                        .get(flight.agent)
                        .copied()
                        .unwrap_or(theme.muted_foreground);
                    if let Some(slot) = lit_agents.get_mut(flight.agent) {
                        *slot = Some((hue, frame.glow));
                    }
                    let shown = match flight.phase {
                        FlightPhase::Replay => frame.revealed.max(
                            // the hop being flown to is lit as it is tried
                            match frame.dot {
                                Some((DotPlace::TargetWire { .. }, _))
                                | Some((DotPlace::At { .. }, _)) => frame.revealed + 1,
                                _ => frame.revealed,
                            },
                        ),
                        _ => flight.hops.len(),
                    };
                    for (index, hop) in flight.hops.iter().take(shown).enumerate() {
                        let failed = hop.result == HopResult::Failed && index < frame.revealed;
                        if let Some(slot) = lit_targets.get_mut(hop.target) {
                            *slot =
                                Some((if failed { theme.danger } else { hue }, frame.glow, failed));
                        }
                    }
                }

                for index in 0..model.agents.len() {
                    let wire = agent_wire(index);
                    match lit_agents[index] {
                        Some((hue, glow)) => {
                            if let Some(path) = wire.path(px(1.0), false) {
                                window.paint_path(path, theme.border);
                            }
                            if let Some(path) = wire.path(px(2.0), false) {
                                window.paint_path(path, hue.opacity(0.85 * glow));
                            }
                        }
                        None => {
                            if let Some(path) = wire.path(px(1.0), false) {
                                window.paint_path(path, theme.border);
                            }
                        }
                    }
                }
                for index in 0..model.targets.len() {
                    let wire = target_wire(index);
                    let skipped = matches!(model.targets[index].state, TargetState::Skipped(_));
                    match lit_targets[index] {
                        Some((color, glow, failed)) => {
                            if let Some(path) = wire.path(px(1.0), false) {
                                window.paint_path(path, theme.border);
                            }
                            if let Some(path) = wire.path(px(2.0), failed) {
                                window.paint_path(path, color.opacity(0.85 * glow));
                            }
                        }
                        None => {
                            if let Some(path) = wire.path(px(1.0), skipped) {
                                window.paint_path(
                                    path,
                                    theme.border.opacity(if skipped { 0.6 } else { 1.0 }),
                                );
                            }
                        }
                    }
                }

                // The gateway breathes while anything runs through it.
                if any_live {
                    let center = at(HUB_X + HUB_W / 2.0, hub_y);
                    let grow = 6.0 + 10.0 * pulse_phase;
                    let ring = Bounds::new(
                        point(
                            center.x - px(HUB_W / 2.0 + grow),
                            center.y - px(HUB_H / 2.0 + grow),
                        ),
                        gpui::size(px(HUB_W + grow * 2.0), px(HUB_H + grow * 2.0)),
                    );
                    window.paint_quad(
                        gpui::outline(
                            ring,
                            theme.primary.opacity(0.35 * (1.0 - pulse_phase)),
                            gpui::BorderStyle::Solid,
                        )
                        .corner_radii(px(14.0 + grow)),
                    );
                }

                // Streams: three particles flowing home along the lit wires.
                for (flight, frame) in model.flights.iter().zip(&frames) {
                    if !frame.streaming {
                        continue;
                    }
                    let Some(current) = flight.hops.last() else {
                        continue;
                    };
                    let hue = hues
                        .get(flight.agent)
                        .copied()
                        .unwrap_or(theme.muted_foreground);
                    let phase = (now_ms % 1400) as f32 / 1400.0;
                    for particle in 0..3 {
                        let t = (phase + particle as f32 / 3.0) % 1.0;
                        // The first half of the trip is the member's wire back
                        // to the gateway, the second the Agent's.
                        let position = if t < 0.5 {
                            target_wire(current.target).at(1.0 - t * 2.0)
                        } else {
                            agent_wire(flight.agent).at(1.0 - (t - 0.5) * 2.0)
                        };
                        paint_dot(window, position, 2.5, hue.opacity(0.9), None);
                    }
                }

                // Dots.
                for (flight, frame) in model.flights.iter().zip(&frames) {
                    let Some((place, style)) = frame.dot else {
                        continue;
                    };
                    let hue = hues
                        .get(flight.agent)
                        .copied()
                        .unwrap_or(theme.muted_foreground);
                    let position = match place {
                        DotPlace::AgentWire { t } => agent_wire(flight.agent).at(t),
                        DotPlace::TargetWire { target, t } => target_wire(target).at(t),
                        DotPlace::At { target } => at(TARGET_X - 6.0, target_y(target)),
                    };
                    match style {
                        DotStyle::Out => {
                            paint_dot(window, position, 9.0, hue.opacity(0.18), None);
                            paint_dot(window, position, 4.5, hue, None);
                        }
                        DotStyle::Held => {
                            let breath = 0.5 + 0.5 * (pulse_phase * std::f32::consts::TAU).sin();
                            paint_dot(
                                window,
                                position,
                                7.0 + 4.0 * breath,
                                hue.opacity(0.22),
                                None,
                            );
                            paint_dot(window, position, 4.5, hue, None);
                        }
                        DotStyle::HomeOk => {
                            paint_dot(window, position, 8.0, hue.opacity(0.16), None);
                            paint_dot(window, position, 4.5, theme.background, Some(hue));
                        }
                        DotStyle::HomeErr => {
                            paint_dot(window, position, 8.0, theme.danger.opacity(0.18), None);
                            paint_dot(window, position, 4.5, theme.danger, None);
                        }
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        // ----- the nodes ----------------------------------------------------
        let mut stage = div()
            .id("management-route-stage")
            .relative()
            .w_full()
            .min_w(px(TARGET_X + 220.0))
            .h(px(height))
            .child(canvas_layer);

        for (index, agent) in self.model.agents.iter().enumerate() {
            let hue = agent_hue(&agent.id, cx);
            let active = agent_active[index];
            stage = stage.child(
                h_flex()
                    .absolute()
                    .left_0()
                    .top(px(agent_y(index) - NODE_H / 2.0))
                    .w(px(AGENT_W))
                    .h(px(NODE_H))
                    .items_center()
                    .gap_2()
                    .px_2()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(match active {
                        Some(hue) => hue.opacity(0.8),
                        None => cx.theme().border,
                    })
                    .bg(cx.theme().background)
                    .child(agent_brand_icon(&agent.id, px(16.0), None))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_medium()
                            .child(agent.label.clone()),
                    )
                    .child(div().flex_none().size(px(8.0)).rounded_full().bg(hue)),
            );
        }

        stage = stage.child(
            v_flex()
                .absolute()
                .left(px(HUB_X))
                .top(px(hub_y - HUB_H / 2.0))
                .w(px(HUB_W))
                .h(px(HUB_H))
                .items_center()
                .justify_center()
                .gap_1()
                .rounded(px(14.0))
                .border_1()
                .border_color(if any_live {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .bg(cx.theme().background)
                .child(
                    h_flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            Icon::default()
                                .path("icons/vibex/server.svg")
                                .size(px(14.0))
                                .text_color(cx.theme().primary),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .child(self.model.hub_title.clone()),
                        ),
                )
                .child(
                    div()
                        .px_2()
                        .rounded_full()
                        .text_size(px(10.0))
                        .bg(if self.model.hub_running {
                            cx.theme().success.opacity(0.14)
                        } else {
                            cx.theme().muted
                        })
                        .text_color(if self.model.hub_running {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(if self.model.hub_running {
                            management_locale_text("routing", "路由中", "路由中")
                        } else {
                            management_locale_text("off", "未运行", "未執行")
                        }),
                ),
        );

        for (index, target) in self.model.targets.iter().enumerate() {
            let faded = matches!(target.state, TargetState::Skipped(_));
            let waiting = target_waiting[index];
            let tag = target_tags[index].clone();
            let state_tag = match &tag {
                Some((label, HopResult::Ok)) => Some(
                    Tag::color(ColorName::Green)
                        .xsmall()
                        .child(label.clone())
                        .into_any_element(),
                ),
                Some((label, HopResult::Failed)) => Some(
                    Tag::color(ColorName::Red)
                        .xsmall()
                        .child(label.clone())
                        .into_any_element(),
                ),
                Some((label, HopResult::Pending)) => Some(
                    Tag::color(ColorName::Neutral)
                        .xsmall()
                        .child(label.clone())
                        .into_any_element(),
                ),
                None => match &target.state {
                    TargetState::Ready => None,
                    TargetState::Cooling(label) => Some(
                        Tag::color(ColorName::Amber)
                            .xsmall()
                            .child(label.clone())
                            .into_any_element(),
                    ),
                    TargetState::Invalid => Some(
                        Tag::color(ColorName::Red)
                            .xsmall()
                            .child(management_locale_text("invalid", "已失效", "已失效"))
                            .into_any_element(),
                    ),
                    TargetState::Skipped(label) => Some(
                        Tag::color(ColorName::Neutral)
                            .xsmall()
                            .child(label.clone())
                            .into_any_element(),
                    ),
                },
            };
            let mark = match target.profile.as_ref() {
                Some(profile) => provider_logo_tile(profile, px(26.0), cx),
                None => ws_icon_tile("icons/vibex/database.svg", px(26.0), None, cx),
            };
            stage = stage.child(
                h_flex()
                    .absolute()
                    .left(px(TARGET_X))
                    .right_0()
                    .top(px(target_y(index) - NODE_H / 2.0))
                    .h(px(NODE_H))
                    .items_center()
                    .gap_2()
                    .px_2()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(match (waiting, &tag) {
                        (Some(hue), _) => hue,
                        (None, Some((_, HopResult::Failed))) => cx.theme().danger.opacity(0.6),
                        (None, Some((_, HopResult::Ok))) => cx.theme().success.opacity(0.6),
                        _ => cx.theme().border,
                    })
                    .bg(cx.theme().background)
                    .when(faded, |row| row.opacity(0.55))
                    .child(mark)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_medium()
                                    .child(target.title.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(10.5))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if waiting.is_some() {
                                        management_locale_text(
                                            "waiting for the answer…",
                                            "等待响应…",
                                            "等待回應…",
                                        )
                                        .to_string()
                                    } else {
                                        target.subtitle.clone()
                                    }),
                            ),
                    )
                    .children(state_tag.map(|tag| div().flex_none().child(tag))),
            );
        }

        if self.model.agents.is_empty() && self.model.targets.is_empty() {
            return div()
                .id("management-route-stage")
                .w_full()
                .py_8()
                .flex()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(management_locale_text(
                    "Nothing routes through the gateway yet. Give an Agent a route group and its requests appear here.",
                    "还没有经过网关的路由。让 Agent 使用路由组后，它的请求就会出现在这里。",
                    "還沒有經過閘道的路由。讓 Agent 使用路由組後，它的請求就會出現在這裡。",
                ))
                .into_any_element();
        }
        div()
            .w_full()
            .overflow_x_scrollbar()
            .child(stage)
            .into_any_element()
    }
}

fn paint_dot(
    window: &mut Window,
    center: gpui::Point<Pixels>,
    radius: f32,
    color: Hsla,
    ring: Option<Hsla>,
) {
    let bounds = Bounds::new(
        point(center.x - px(radius), center.y - px(radius)),
        gpui::size(px(radius * 2.0), px(radius * 2.0)),
    );
    let mut quad = gpui::fill(bounds, color).corner_radii(px(radius));
    if let Some(ring) = ring {
        quad = quad.border_widths(px(2.0)).border_color(ring);
    }
    window.paint_quad(quad);
}
