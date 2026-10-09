//! Choreography belongs to each guardian. Shared physics does not imply shared
//! movement, attack geometry, terrain interactions or weak-point conditions.

use super::*;
use crate::arena::map::{CENTER, DAIS};
use std::f32::consts::{PI, TAU};

impl Arena {
    pub(super) fn tick_boss(&mut self) {
        self.boss.previous_position = self.boss.position;
        self.boss.exposed = (self.boss.exposed - STEP).max(0.0);
        self.boss.time += STEP;
        match self.boss.guardian {
            Guardian::Claude => self.tick_claude(),
            Guardian::Codex => self.tick_codex(),
            Guardian::Pi => self.tick_pi(),
            Guardian::OpenCode => self.tick_opencode(),
            Guardian::DeepSeek => self.tick_deepseek(),
            Guardian::Copilot => self.tick_copilot(),
        }
        self.boss.update_core();
    }

    fn follow_target(&mut self, lock: f32) {
        if self.boss.progress() < lock {
            self.boss.target = self.player.position;
            if matches!(self.boss.attack, Attack::Leap | Attack::Dive) {
                self.boss.target = self.map.move_body(
                    self.boss.position,
                    self.player.position,
                    if self.boss.guardian == Guardian::DeepSeek {
                        8.0
                    } else {
                        self.boss.radius()
                    },
                );
            }
            self.boss.face(self.player.position, STEP * 3.2);
        }
    }
    fn rest_or_prepare(&mut self) {
        if self.boss.time >= self.boss.duration {
            self.boss.prepare(self.player.position);
        }
    }

    fn tick_claude(&mut self) {
        let t = self.boss.progress();
        let double = self.boss.attack == Attack::Clap;
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP * 1.5);
                let distance = self.player.position.minus(self.boss.position);
                if distance.length() > 17.0 {
                    self.boss.position = self.map.move_body(
                        self.boss.position,
                        self.boss
                            .position
                            .plus(distance.normalized().scale(8.0 * STEP)),
                        self.boss.radius(),
                    );
                    self.boss.stride += STEP * 7.0;
                }
                self.boss.rest_hands();
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target(0.56);
                let reach = self.boss.target.minus(self.boss.position);
                if t < 0.56 && reach.length() > 25.0 {
                    self.boss.target = self.boss.position.plus(reach.normalized().scale(25.0));
                }
                self.boss.pitch = -smoothstep(t) * if double { 0.2 } else { 0.07 };
                for ix in 0..2 {
                    if double || (ix == 0) == (self.boss.attack == Attack::LeftFist) {
                        let side = if ix == 0 { -1.0 } else { 1.0 };
                        let rest = self.boss.resting_hand(side);
                        let target = self
                            .boss
                            .target
                            .plus(Vec2::new(if double { side * 4.5 } else { 0.0 }, 0.0));
                        self.boss.hands[ix] = rest.lerp(target, smoothstep(t));
                        self.boss.hand_heights[ix] =
                            3.0 + smoothstep(t) * if double { 22.0 } else { 16.0 };
                    }
                }
                if t >= 1.0 {
                    self.boss
                        .enter(BossState::Striking, if double { 0.34 } else { 0.22 });
                }
            }
            BossState::Striking => {
                self.boss.pitch = t * 0.1;
                for ix in 0..2 {
                    if double || (ix == 0) == (self.boss.attack == Attack::LeftFist) {
                        self.boss.hand_heights[ix] =
                            (1.0 - t.powi(3)) * if double { 25.0 } else { 19.0 };
                    }
                }
                if t >= 1.0 {
                    if double {
                        for hand in self.boss.hands {
                            self.impact(hand, self.boss.impact_radius());
                        }
                        let target = self.boss.target;
                        // The split fault travels away from both planted fists,
                        // leaving the opening at the mascot's chest readable.
                        for side in [-1.0, 1.0] {
                            let origin = target.plus(Vec2::new(side * 4.5, 0.0));
                            self.hazard(
                                HazardKind::Fissure,
                                origin,
                                origin.plus(Vec2::new(side * 24.0, 9.0)),
                                0.3,
                                0.35,
                                1.3,
                            );
                        }
                        self.boss.opening(2.45);
                    } else {
                        let ix = usize::from(self.boss.attack == Attack::RightFist);
                        self.impact(self.boss.hands[ix], self.boss.impact_radius());
                        self.boss.enter(BossState::Recovery, 0.48);
                    }
                }
            }
            BossState::Recovery => {
                self.boss.settle();
                for ix in 0..2 {
                    let side = if ix == 0 { -1.0 } else { 1.0 };
                    self.boss.hands[ix] =
                        self.boss.hands[ix].lerp(self.boss.resting_hand(side), STEP * 3.2);
                    self.boss.hand_heights[ix] = 3.0 * smoothstep(t);
                }
                if t >= 1.0 {
                    self.boss
                        .enter(BossState::Watching, if double { 0.65 } else { 0.22 });
                }
            }
            _ => {}
        }
    }

    fn tick_codex(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP * 1.3);
                self.boss.spin += STEP * 0.18;
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target(0.58);
                self.boss.bank = (t * PI).sin() * 0.16;
                self.boss.spin -= STEP * 0.6;
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(
                        if self.boss.attack == Attack::Leap {
                            BossState::Striking
                        } else {
                            BossState::Rushing
                        },
                        if self.boss.attack == Attack::Leap {
                            1.1
                        } else {
                            3.6
                        },
                    );
                }
            }
            BossState::Rushing => {
                let before = self.boss.position;
                let next = before.plus(self.boss.direction.scale(48.0 * STEP));
                self.boss.spin += STEP * 7.4;
                self.boss.bank = 0.0;
                let pillar = self.map.pillar_hit(before, next, self.boss.radius());
                if pillar.is_some() || !self.map.inside(next, self.boss.radius() + 0.5) || t >= 1.0
                {
                    if let Some(ix) = pillar {
                        self.map.broken |= 1 << ix;
                        self.effect(
                            EffectKind::Rubble,
                            self.map.pillars()[ix].position,
                            1.4,
                            11.0,
                        );
                    }
                    self.impact(before, 5.0);
                    self.boss.position = self.map.move_body(
                        before,
                        before.minus(self.boss.direction.scale(1.0)),
                        self.boss.radius(),
                    );
                    self.boss.opening(if pillar.is_some() { 3.0 } else { 2.25 });
                } else {
                    self.boss.position = next;
                    if ((self.boss.time / STEP) as u32).is_multiple_of(7) {
                        self.effect(EffectKind::Roll, before, 0.65, 3.0);
                    }
                }
            }
            BossState::Striking => {
                self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.height = (t * PI).sin() * 15.0;
                self.boss.spin += STEP * 4.0;
                if t >= 1.0 {
                    self.boss.height = 0.0;
                    self.impact(self.boss.position, self.boss.impact_radius());
                    self.wave(self.boss.position, 7.0, 35.0, WaveKind::Stone);
                    self.boss.opening(2.2);
                }
            }
            BossState::Recovery => {
                self.boss.bank = (self.boss.time * 13.0).sin() * (1.0 - t) * 0.12;
                self.boss.spin += STEP * (1.0 - smoothstep(self.boss.time / 0.45));
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.6);
                }
            }
            _ => {}
        }
    }

    fn tick_pi(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.height = self.boss.from_height
                    + (self.boss.hover_height(self.visual_time) - self.boss.from_height)
                        * smoothstep(t);
                self.boss.face(self.player.position, STEP);
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target(0.5);
                self.boss.height =
                    self.boss.from_height + (4.5 - self.boss.from_height) * smoothstep(t);
                if t >= 1.0 {
                    let target = self.boss.target;
                    if self.boss.attack == Attack::Cross {
                        for direction in [Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)] {
                            self.hazard(
                                HazardKind::Rune,
                                target.minus(direction.scale(22.0)),
                                target.plus(direction.scale(22.0)),
                                0.48,
                                0.26,
                                1.0,
                            );
                        }
                    } else {
                        self.wave(self.boss.position, 5.0, 54.0, WaveKind::Magic);
                        for ix in 0..8 {
                            let angle = ix as f32 * TAU / 8.0 + self.boss.direction.angle();
                            self.projectile(
                                self.boss.position,
                                Vec2::from_angle(angle).scale(17.0),
                                ProjectileKind::Rune,
                            );
                        }
                    }
                    self.effect(EffectKind::Cross, self.boss.position, 0.6, 7.0);
                    self.boss.opening(2.5);
                }
            }
            BossState::Recovery => {
                self.boss.height = (self.boss.height - STEP * 3.0).max(0.0);
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.ward = (self.boss.ward + 1) % 3;
                    self.boss.target = DAIS[self.boss.ward as usize];
                    self.effect(EffectKind::Teleport, self.boss.from, 0.9, 8.0);
                    self.boss.enter(BossState::Relocating, 1.05);
                }
            }
            BossState::Relocating => {
                self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.height = (t * PI).sin() * 9.0;
                if t >= 1.0 {
                    self.boss.height = 0.0;
                    self.effect(EffectKind::Teleport, self.boss.position, 0.5, 5.0);
                    self.boss.enter(BossState::Watching, 0.55);
                }
            }
            _ => {}
        }
    }

    fn tick_opencode(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP * 2.0);
                self.rest_or_prepare();
            }
            BossState::Windup => {
                if self.boss.attack == Attack::Stomp {
                    if t < 0.5 {
                        let d = self.player.position.minus(self.boss.position);
                        self.boss.direction = if d.x.abs() > d.y.abs() {
                            Vec2::new(d.x.signum(), 0.0)
                        } else {
                            Vec2::new(0.0, d.y.signum())
                        };
                        self.boss.target = self.map.move_body(
                            self.boss.position,
                            self.boss.position.plus(self.boss.direction.scale(16.0)),
                            self.boss.radius(),
                        );
                    }
                    let direction = self.boss.direction;
                    self.boss
                        .face(self.boss.position.plus(direction), STEP * 4.0);
                    self.boss.direction = direction;
                    self.boss.pitch = -(t * PI).sin() * 0.15;
                    if t >= 1.0 {
                        self.boss.from = self.boss.position;
                        self.boss.enter(BossState::Striking, 0.6);
                    }
                } else {
                    self.follow_target(0.55);
                    self.boss.beam_end = self
                        .map
                        .beam_end(self.boss.attack_origin(), self.boss.direction);
                    if t >= 1.0 {
                        self.boss.enter(BossState::Striking, 1.1);
                    }
                }
            }
            BossState::Striking => {
                if self.boss.attack == Attack::Stomp {
                    // A full edge-over-edge turn, including a rising center and
                    // changing faces; the body is never a translated flat icon.
                    self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                    self.boss.pitch = -t * TAU;
                    self.boss.height = Boss::rolling_height(self.boss.pitch);
                    self.boss.yaw = self.boss.direction.angle() - PI * 0.5;
                    if t >= 1.0 {
                        self.boss.pitch = 0.0;
                        self.boss.height = 0.0;
                        self.impact(self.boss.position, self.boss.impact_radius());
                        self.boss.enter(BossState::Watching, 0.22);
                    }
                } else {
                    // The committed beam is blocked by the actual standing
                    // columns. Rendered endpoint and lethal segment are shared.
                    let origin = self.boss.attack_origin();
                    self.boss.beam_end = self.map.beam_end(origin, self.boss.direction);
                    self.hazards.retain(|h| h.kind != HazardKind::Beam);
                    self.hazard(
                        HazardKind::Beam,
                        origin,
                        self.boss.beam_end,
                        0.0,
                        STEP * 2.0,
                        1.4,
                    );
                    if self.boss.fired == 0 {
                        self.boss.fired = 1;
                        self.effect(EffectKind::Sweep, origin, 0.5, 3.0);
                    }
                    if t >= 1.0 {
                        self.hazards.retain(|h| h.kind != HazardKind::Beam);
                        self.effect(EffectKind::Steam, origin, 2.3, 5.0);
                        self.boss.opening(2.6);
                    }
                }
            }
            BossState::Recovery if t >= 1.0 => {
                self.boss.enter(BossState::Watching, 0.7);
            }
            _ => {}
        }
    }

    fn tick_deepseek(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP);
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target(0.52);
                self.boss.height = if self.boss.attack == Attack::Leap {
                    -2.0 + t * 3.0
                } else {
                    -2.0
                };
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(
                        if self.boss.attack == Attack::Leap {
                            BossState::Striking
                        } else {
                            BossState::Rushing
                        },
                        if self.boss.attack == Attack::Leap {
                            1.4
                        } else {
                            0.8
                        },
                    );
                }
            }
            BossState::Rushing => {
                self.boss.height = (t * PI).sin() * 2.5;
                let next = self
                    .boss
                    .position
                    .plus(self.boss.direction.scale(STEP * 49.0));
                if self.map.inside(next, 8.0) {
                    self.boss.position = next;
                }
                self.boss.bank = (t * TAU).sin() * 0.12;
                if ((self.boss.time / STEP) as u32).is_multiple_of(7) {
                    self.effect(EffectKind::Wake, self.boss.position, 0.9, 6.5);
                }
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.target =
                        CENTER.plus(Vec2::from_angle(self.elapsed * 0.73).scale(32.0));
                    self.boss.enter(BossState::Submerged, 1.1);
                }
            }
            BossState::Submerged => {
                self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.height = self.boss.from_height
                    + (-3.5 - self.boss.from_height) * smoothstep((t * 4.0).min(1.0));
                self.boss.face(self.player.position, STEP * 2.0);
                if ((self.boss.time / STEP) as u32).is_multiple_of(10) {
                    self.effect(EffectKind::Wake, self.boss.position, 0.75, 5.0);
                }
                if t >= 1.0 {
                    self.boss.prepare(self.player.position);
                }
            }
            BossState::Striking => {
                self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.height = (t * PI).sin() * 23.0;
                self.boss.pitch = (0.5 - t) * 1.15;
                if t >= 1.0 {
                    self.boss.height = 0.0;
                    self.boss.pitch = 0.0;
                    self.impact(self.boss.position, self.boss.impact_radius());
                    self.effect(EffectKind::Wake, self.boss.position, 1.5, 14.0);
                    self.wave(self.boss.position, 9.0, 45.0, WaveKind::Water);
                    for side in [-1.0, 1.0] {
                        let p = self
                            .boss
                            .position
                            .plus(self.boss.direction.perpendicular().scale(side * 14.0));
                        self.hazard(HazardKind::Geyser, p, p, 0.7, 0.25, 4.0);
                    }
                    self.boss.opening(2.7);
                }
            }
            BossState::Recovery => {
                self.boss.settle();
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.6);
                }
            }
            _ => {}
        }
    }

    fn tick_copilot(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.height = self.boss.from_height
                    + (self.boss.hover_height(self.visual_time) - self.boss.from_height)
                        * smoothstep(t);
                let tangent = self
                    .player
                    .position
                    .minus(self.boss.position)
                    .normalized()
                    .perpendicular();
                let next = self.boss.position.plus(tangent.scale(10.0 * STEP));
                if self.map.inside(next, 12.0) {
                    self.boss.position = next;
                }
                self.boss.face(self.player.position, STEP * 2.0);
                self.boss.bank = 0.15;
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target(0.63);
                let apex = if self.boss.attack == Attack::Dive {
                    21.0
                } else {
                    11.0
                };
                self.boss.height =
                    self.boss.from_height + (apex - self.boss.from_height) * smoothstep(t);
                self.boss.bank = (1.0 - t) * 0.15;
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(
                        BossState::Striking,
                        if self.boss.attack == Attack::Dive {
                            0.72
                        } else {
                            0.8
                        },
                    );
                }
            }
            BossState::Striking => {
                if self.boss.attack == Attack::Volley {
                    let salvo = (t * 3.0).floor() as u32;
                    if salvo > self.boss.fired {
                        self.boss.fired = salvo;
                        for side in [-1.0, 1.0] {
                            let origin = self.boss.local(side * 10.0, 2.0, 0.0);
                            let angle = self.boss.target.minus(origin).angle();
                            for spread in [-0.14, 0.0, 0.14] {
                                self.projectile(
                                    origin,
                                    Vec2::from_angle(angle + spread).scale(26.0),
                                    ProjectileKind::Feather,
                                );
                            }
                            self.effect(EffectKind::Feather, origin, 0.45, 4.0);
                        }
                    }
                    if t >= 1.0 {
                        self.boss.enter(BossState::Watching, 0.45);
                    }
                } else {
                    self.boss.position = self.boss.from.lerp(self.boss.target, t * t);
                    self.boss.height = (1.0 - t.powi(3)) * 21.0;
                    self.boss.pitch = t * 0.55;
                    if ((self.boss.time / STEP) as u32).is_multiple_of(6) {
                        self.effect(EffectKind::Feather, self.boss.position, 0.65, 7.0);
                    }
                    if t >= 1.0 {
                        self.boss.height = 0.0;
                        self.boss.pitch = 0.15;
                        self.impact(self.boss.position, self.boss.impact_radius());
                        self.effect(EffectKind::Feather, self.boss.position, 1.4, 15.0);
                        self.boss.opening(2.6);
                    }
                }
            }
            BossState::Recovery => {
                self.boss.settle();
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.7);
                }
            }
            _ => {}
        }
    }
}
