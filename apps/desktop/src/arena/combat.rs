//! Fixed-step combat. World coordinates are independent of the pixel renderer.

use std::f32::consts::TAU;

pub(super) use super::guardian::{Attack, Boss, BossState, Guardian};

pub(super) use super::geometry::{HEIGHT, Vec2, WIDTH, segment_distance, smoothstep};
pub(super) const STEP: f32 = 1.0 / 60.0;
pub(super) const INTRO_DURATION: f32 = 1.2;
pub(super) const MIN_CHARGE: f32 = 0.30;
pub(super) const MAX_PROJECTILES: usize = 48;
pub(super) const MAX_EFFECTS: usize = 48;
pub(super) const MAX_WAVES: usize = 8;
const PLAYER_RADIUS: f32 = 1.0;
const CORE_RADIUS: f32 = 1.65;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Awakening,
    Battle,
    Victory,
    Defeat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ArrowState {
    #[default]
    Ready,
    Flying,
    Lodged,
    Returning,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Controls {
    pub movement: Vec2,
    pub shoot: bool,
    pub recall: bool,
    /// Keyboard attacks use the core; pointer attacks use the measured projection.
    pub aim: Option<Vec2>,
}

#[derive(Clone, Debug)]
pub(super) struct Player {
    pub position: Vec2,
    pub facing: Vec2,
    pub charge: f32,
    pub invulnerable: f32,
    pub roll_remaining: f32,
    pub roll_cooldown: f32,
    pub stride: f32,
    pub moving: bool,
    roll_direction: Vec2,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Arrow {
    pub state: ArrowState,
    pub position: Vec2,
    pub velocity: Vec2,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Projectile {
    pub position: Vec2,
    pub velocity: Vec2,
    pub remaining: f32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Wave {
    pub position: Vec2,
    pub radius: f32,
    pub maximum: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EffectKind {
    Impact,
    Armor,
    Catch,
    Roll,
    Shot,
    Cross,
    Sweep,
    Victory,
    Defeat,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Effect {
    pub kind: EffectKind,
    pub position: Vec2,
    pub direction: Vec2,
    pub remaining: f32,
    pub duration: f32,
    pub radius: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cue {
    Shielded,
    ReturnArrow,
    Caught,
}

#[derive(Clone, Debug)]
pub(super) struct Arena {
    pub phase: Phase,
    pub player: Player,
    pub boss: Boss,
    pub arrow: Arrow,
    pub projectiles: Vec<Projectile>,
    pub waves: Vec<Wave>,
    pub effects: Vec<Effect>,
    pub elapsed: f32,
    pub visual_time: f32,
    pub phase_time: f32,
    pub cue: Option<Cue>,
    cue_remaining: f32,
}

impl Arena {
    pub fn new(guardian: Guardian, idle_time: f32) -> Self {
        Self {
            phase: Phase::Awakening,
            player: Player {
                position: Vec2::new(48.0, 43.0),
                facing: Vec2::new(0.0, -1.0),
                charge: 0.0,
                invulnerable: 0.0,
                roll_remaining: 0.0,
                roll_cooldown: 0.0,
                stride: 0.0,
                moving: false,
                roll_direction: Vec2::default(),
            },
            boss: Boss::new(guardian),
            arrow: Arrow::default(),
            projectiles: Vec::with_capacity(MAX_PROJECTILES),
            waves: Vec::with_capacity(MAX_WAVES),
            effects: Vec::with_capacity(MAX_EFFECTS),
            elapsed: 0.0,
            visual_time: idle_time,
            phase_time: 0.0,
            cue: None,
            cue_remaining: 0.0,
        }
    }

    pub fn needs_tick(&self) -> bool {
        matches!(self.phase, Phase::Awakening | Phase::Battle) || self.phase_time < 1.25
    }

    pub fn outcome_ready(&self) -> bool {
        matches!(self.phase, Phase::Victory | Phase::Defeat) && self.phase_time >= 1.0
    }

    pub fn tick(&mut self, controls: Controls) {
        if !self.needs_tick() {
            return;
        }
        self.visual_time += STEP;
        self.phase_time += STEP;
        for effect in &mut self.effects {
            effect.remaining -= STEP;
        }
        self.effects.retain(|effect| effect.remaining > 0.0);
        if self.phase == Phase::Awakening {
            if self.phase_time >= INTRO_DURATION {
                self.phase = Phase::Battle;
                self.phase_time = 0.0;
                self.boss.enter(BossState::Watching, 0.65);
            }
            return;
        }
        if self.phase != Phase::Battle {
            return;
        }
        self.elapsed += STEP;
        self.player.invulnerable = (self.player.invulnerable - STEP).max(0.0);
        self.player.roll_cooldown = (self.player.roll_cooldown - STEP).max(0.0);
        self.cue_remaining -= STEP;
        if self.cue_remaining <= 0.0 {
            self.cue = None;
        }
        self.player.moving = false;
        let from = self.player.position;
        let rolling = self.player.roll_remaining > 0.0;
        let recalling = !rolling
            && self.arrow.state != ArrowState::Ready
            && (controls.recall || controls.shoot);
        let drawing = !rolling && controls.shoot && self.arrow.state == ArrowState::Ready;
        if rolling {
            self.player.position = from
                .plus(self.player.roll_direction.scale(52.0 * STEP))
                .clamped(3.5);
            self.player.roll_remaining = (self.player.roll_remaining - STEP).max(0.0);
        } else if drawing {
            self.player.charge = (self.player.charge + STEP).min(0.9);
            self.player.facing = self.aim_direction(controls.aim);
        } else if !recalling && controls.movement.length() > 0.0 {
            let direction = controls.movement.normalized();
            self.player.position = from.plus(direction.scale(18.0 * STEP)).clamped(3.5);
            self.player.facing = direction;
            self.player.moving = true;
            self.player.stride += STEP * 10.0;
        }
        self.tick_boss();
        if self.phase != Phase::Battle {
            return;
        }
        self.tick_threats(from);
        if self.phase != Phase::Battle {
            return;
        }
        // Relative motion catches fast charges and rolls that cross a body in one tick.
        if !self.boss.airborne()
            && segment_distance(
                Vec2::default(),
                from.minus(self.boss.previous_position),
                self.player.position.minus(self.boss.position),
            ) < self.boss.radius() + PLAYER_RADIUS
        {
            self.hurt();
        }
        if self.phase == Phase::Battle {
            self.tick_arrow(recalling);
        }
    }

    fn aim_direction(&self, aim: Option<Vec2>) -> Vec2 {
        aim.unwrap_or(self.boss.core)
            .minus(self.player.position)
            .normalized()
    }

    pub fn release_shot(&mut self, aim: Option<Vec2>) {
        if self.phase == Phase::Battle
            && self.arrow.state == ArrowState::Ready
            && self.player.roll_remaining <= 0.0
            && self.player.charge >= MIN_CHARGE
        {
            let direction = self.aim_direction(aim);
            self.arrow = Arrow {
                state: ArrowState::Flying,
                position: self.player.position.plus(direction.scale(1.6)),
                velocity: direction.scale(72.0 + self.player.charge * 28.0),
            };
            self.player.facing = direction;
            self.effect(EffectKind::Shot, self.arrow.position, 0.18, 2.0);
        }
        self.player.charge = 0.0;
    }

    pub fn cancel_input(&mut self) {
        self.player.charge = 0.0;
        self.player.moving = false;
        if self.arrow.state == ArrowState::Returning {
            self.arrow.state = ArrowState::Lodged;
        }
    }

    pub fn roll(&mut self, movement: Vec2) {
        if self.phase != Phase::Battle || self.player.roll_cooldown > 0.0 {
            return;
        }
        self.cancel_input();
        self.player.roll_direction = if movement.length() > 0.0 {
            movement.normalized()
        } else {
            self.player.facing
        };
        self.player.facing = self.player.roll_direction;
        self.player.roll_remaining = 0.26;
        self.player.roll_cooldown = 0.54;
        self.player.invulnerable = 0.28;
        self.effect(EffectKind::Roll, self.player.position, 0.38, 2.0);
    }

    fn tick_arrow(&mut self, recalling: bool) {
        if self.arrow.state == ArrowState::Ready {
            self.arrow.position = self.player.position;
            return;
        }
        if recalling {
            self.arrow.state = ArrowState::Returning;
            self.arrow.velocity = self
                .player
                .position
                .minus(self.arrow.position)
                .normalized()
                .scale(74.0);
        } else if self.arrow.state == ArrowState::Returning {
            self.arrow.state = ArrowState::Lodged;
        }
        if self.arrow.state == ArrowState::Lodged {
            if self.arrow.position.minus(self.player.position).length() < 2.0 {
                self.catch_arrow();
            }
            return;
        }
        let from = self.arrow.position;
        let to = from.plus(self.arrow.velocity.scale(STEP));
        let returning = self.arrow.state == ArrowState::Returning;
        if self.boss.exposed > 0.0 && segment_distance(self.boss.core, from, to) <= CORE_RADIUS {
            if returning || self.boss.guardian != Guardian::Pi {
                self.phase = Phase::Victory;
                self.phase_time = 0.0;
                self.arrow.position = self.boss.core;
                self.arrow.state = ArrowState::Lodged;
                self.projectiles.clear();
                self.waves.clear();
                self.boss.enter(BossState::Fallen, 1.25);
                self.cancel_input();
                self.effect(EffectKind::Victory, self.boss.core, 1.25, 18.0);
                return;
            }
            self.show_cue(Cue::ReturnArrow);
        }
        if !returning
            && self.boss.exposed <= 0.0
            && self.boss.guardian != Guardian::Pi
            && !self.boss.airborne()
            && segment_distance(self.boss.core, from, to) <= self.boss.radius()
        {
            self.arrow.state = ArrowState::Lodged;
            self.effect(EffectKind::Armor, from, 0.35, 3.0);
            self.show_cue(Cue::Shielded);
            return;
        }
        self.arrow.position = to.clamped(2.0);
        if returning && segment_distance(self.player.position, from, to) < 1.8 {
            self.catch_arrow();
        } else if self.arrow.position != to {
            self.arrow.state = ArrowState::Lodged;
        }
    }

    fn catch_arrow(&mut self) {
        self.arrow.state = ArrowState::Ready;
        self.arrow.position = self.player.position;
        self.effect(EffectKind::Catch, self.player.position, 0.3, 3.0);
        self.show_cue(Cue::Caught);
    }

    fn tick_boss(&mut self) {
        self.boss.previous_position = self.boss.position;
        self.boss.exposed = (self.boss.exposed - STEP).max(0.0);
        self.boss.time += STEP;
        self.boss.animate(STEP);
        let progress = self.boss.progress();
        match self.boss.state {
            BossState::Dormant | BossState::Fallen => {}
            BossState::Watching | BossState::Recovery => {
                if self.boss.time >= self.boss.duration {
                    self.boss.prepare(self.player.position);
                }
            }
            BossState::Windup => {
                // The last 40% is committed. Every attack leaves a real dodge window.
                if progress < 0.60 {
                    self.boss.target = self.player.position.clamped(6.0);
                    self.boss.direction = self
                        .boss
                        .target
                        .minus(self.boss.attack_origin())
                        .normalized();
                }
                if progress >= 1.0 {
                    let state = if matches!(self.boss.attack, Attack::Rush | Attack::Dash) {
                        BossState::Rushing
                    } else {
                        BossState::Striking
                    };
                    self.boss.from = self.boss.position;
                    self.boss.enter(state, self.boss.attack.strike_duration());
                }
            }
            BossState::Rushing => {
                let speed = if self.boss.attack == Attack::Dash {
                    60.0
                } else {
                    45.0
                };
                let next = self
                    .boss
                    .position
                    .plus(self.boss.direction.scale(speed * STEP));
                self.boss.position = next.clamped(self.boss.radius() + 2.0);
                let hit_wall = self.boss.position != next;
                if hit_wall || (self.boss.attack == Attack::Dash && progress >= 1.0) {
                    self.effect(EffectKind::Impact, self.boss.position, 0.7, 6.0);
                    let last_dash = self.boss.guardian != Guardian::DeepSeek
                        || self.boss.attacks.is_multiple_of(3);
                    self.boss.exposed = if last_dash { 2.1 } else { 0.0 };
                    self.boss
                        .enter(BossState::Recovery, if last_dash { 2.35 } else { 0.28 });
                }
            }
            BossState::Striking => {
                if matches!(self.boss.attack, Attack::Leap | Attack::Dive) {
                    self.boss.position =
                        self.boss.from.lerp(self.boss.target, smoothstep(progress));
                }
                if progress >= 1.0 {
                    self.impact();
                }
            }
        }
        self.boss.update_core();
    }

    fn impact(&mut self) {
        let attack = self.boss.attack;
        let target = self.boss.target;
        let opening = match attack {
            Attack::LeftFist | Attack::RightFist | Attack::Stomp | Attack::Volley => 0.0,
            Attack::Clap | Attack::Leap | Attack::Dive => 2.0,
            Attack::Cross | Attack::Pulse | Attack::Sweep => 2.1,
            Attack::Rush | Attack::Dash => unreachable!("rushes recover at their endpoint"),
        };
        self.boss.exposed = opening;
        self.boss.enter(
            BossState::Recovery,
            if opening > 0.0 { opening + 0.25 } else { 0.38 },
        );
        match attack {
            Attack::LeftFist
            | Attack::RightFist
            | Attack::Clap
            | Attack::Stomp
            | Attack::Leap
            | Attack::Dive => {
                let radius = attack.impact_radius();
                self.effect(EffectKind::Impact, target, 0.8, radius);
                if self.player.position.minus(target).length() < radius + PLAYER_RADIUS {
                    self.hurt();
                }
                if self.phase == Phase::Battle
                    && matches!(attack, Attack::Clap | Attack::Leap | Attack::Dive)
                {
                    self.wave(target, radius, radius + 14.0);
                }
            }
            Attack::Cross => {
                self.effect(EffectKind::Cross, target, 0.42, 1.5);
                if (self.player.position.x - target.x).abs() < 1.5 + PLAYER_RADIUS
                    || (self.player.position.y - target.y).abs() < 1.5 + PLAYER_RADIUS
                {
                    self.hurt();
                }
            }
            Attack::Pulse => {
                self.wave(self.boss.position, 5.0, 46.0);
                self.effect(EffectKind::Impact, self.boss.position, 0.45, 5.0);
            }
            Attack::Sweep => {
                let origin = self.boss.attack_origin();
                self.effect(EffectKind::Sweep, origin, 0.42, 2.0);
                let end = origin.plus(self.boss.direction.scale(120.0));
                if segment_distance(self.player.position, origin, end) < 2.0 + PLAYER_RADIUS {
                    self.hurt();
                }
            }
            Attack::Volley => {
                for side in [-1.0, 1.0] {
                    let origin = self.boss.position.plus(Vec2::new(side * 7.0, 0.0));
                    let direction = target.minus(origin);
                    let angle = direction.y.atan2(direction.x);
                    for offset in [-0.16, 0.0, 0.16] {
                        let angle = angle + offset;
                        self.projectile(origin, Vec2::new(angle.cos(), angle.sin()).scale(23.0));
                    }
                }
            }
            Attack::Rush | Attack::Dash => {}
        }
    }

    fn wave(&mut self, position: Vec2, radius: f32, maximum: f32) {
        if self.waves.len() < MAX_WAVES {
            self.waves.push(Wave {
                position,
                radius,
                maximum,
            });
        }
    }
    fn projectile(&mut self, position: Vec2, velocity: Vec2) {
        if self.projectiles.len() < MAX_PROJECTILES {
            self.projectiles.push(Projectile {
                position,
                velocity,
                remaining: 5.0,
            });
        }
    }

    fn tick_threats(&mut self, player_from: Vec2) {
        let mut hit = false;
        for projectile in &mut self.projectiles {
            let from = projectile.position;
            projectile.position = from.plus(projectile.velocity.scale(STEP));
            projectile.remaining -= STEP;
            if segment_distance(
                Vec2::default(),
                player_from.minus(from),
                self.player.position.minus(projectile.position),
            ) < PLAYER_RADIUS + 0.65
            {
                hit = true;
                projectile.remaining = 0.0;
            }
        }
        self.projectiles
            .retain(|p| p.remaining > 0.0 && p.position == p.position.clamped(1.0));
        for wave in &mut self.waves {
            let before = player_from.minus(wave.position).length() - wave.radius;
            wave.radius += 19.0 * STEP;
            let after = self.player.position.minus(wave.position).length() - wave.radius;
            if before.min(after) < PLAYER_RADIUS + 0.5 && before.max(after) > -PLAYER_RADIUS - 0.5 {
                hit = true;
            }
        }
        self.waves.retain(|wave| wave.radius < wave.maximum);
        if hit {
            self.hurt();
        }
    }

    fn hurt(&mut self) {
        if self.phase != Phase::Battle || self.player.invulnerable > 0.0 {
            return;
        }
        self.phase = Phase::Defeat;
        self.phase_time = 0.0;
        self.cancel_input();
        self.projectiles.clear();
        self.waves.clear();
        self.effect(EffectKind::Defeat, self.player.position, 0.8, 5.0);
    }

    fn effect(&mut self, kind: EffectKind, position: Vec2, duration: f32, radius: f32) {
        if self.effects.len() < MAX_EFFECTS {
            self.effects.push(Effect {
                kind,
                position,
                direction: self.boss.direction,
                remaining: duration,
                duration,
                radius,
            });
        }
    }
    fn show_cue(&mut self, cue: Cue) {
        self.cue = Some(cue);
        self.cue_remaining = 1.5;
    }

    pub fn impact_strength(&self) -> f32 {
        self.effects
            .iter()
            .filter(|effect| {
                matches!(
                    effect.kind,
                    EffectKind::Impact | EffectKind::Victory | EffectKind::Defeat
                )
            })
            .map(|effect| ((effect.remaining / effect.duration - 0.55) / 0.45).max(0.0))
            .fold(0.0, f32::max)
    }
    pub fn shake(&self) -> Vec2 {
        let amplitude = self.impact_strength() * 0.65;
        Vec2::new(
            (self.visual_time * TAU * 17.0).sin(),
            (self.visual_time * TAU * 13.0).cos(),
        )
        .scale(amplitude)
    }
}

#[cfg(test)]
#[path = "combat_tests.rs"]
mod tests;
