//! Fixed-step, local combat. Rendering and GPUI never decide a collision.

pub(super) use super::geometry::{HEIGHT, Vec2, WIDTH, segment_distance, smoothstep};
use super::geometry::{circle_hit, segments_distance};
pub(super) use super::guardian::{Attack, Boss, BossState, Guardian};
use super::map::{CENTER, Map, ScarKind};
use std::f32::consts::{PI, TAU};

pub(super) const STEP: f32 = 1.0 / 60.0;
pub(super) const INTRO_DURATION: f32 = 1.4;
pub(super) const VICTORY_DURATION: f32 = 3.3;
pub(super) const MIN_CHARGE: f32 = 0.30;
pub(super) const MAX_PROJECTILES: usize = 48;
pub(super) const MAX_EFFECTS: usize = 64;
pub(super) const MAX_WAVES: usize = 8;
pub(super) const MAX_HAZARDS: usize = 16;
const PLAYER_RADIUS: f32 = 0.9;
const CORE_RADIUS: f32 = 1.8;
pub(super) const ROLL_DURATION: f32 = 0.28;
const INPUT_BUFFER: f32 = 0.12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Awakening,
    Battle,
    Victory,
    Defeat,
    Rebirth,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ArrowState {
    #[default]
    Ready,
    Flying,
    Lodged,
    Returning,
    Tethered,
}
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Controls {
    pub movement: Vec2,
    pub shoot: bool,
    pub recall: bool,
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
    pub swimming: bool,
    roll_direction: Vec2,
    buffered_roll: Option<(Vec2, f32)>,
}
impl Player {
    fn new(position: Vec2) -> Self {
        Self {
            position,
            facing: Vec2::new(0.0, -1.0),
            charge: 0.0,
            invulnerable: 1.2,
            roll_remaining: 0.0,
            roll_cooldown: 0.0,
            stride: 0.0,
            moving: false,
            swimming: false,
            roll_direction: Vec2::default(),
            buffered_roll: None,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Arrow {
    pub state: ArrowState,
    pub position: Vec2,
    pub velocity: Vec2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectileKind {
    Orb,
    ChargedOrb,
    Block,
    Segment,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Projectile {
    pub position: Vec2,
    pub previous_position: Vec2,
    pub velocity: Vec2,
    pub remaining: f32,
    pub kind: ProjectileKind,
    pub age: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WaveKind {
    Stone,
    Water,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Wave {
    pub position: Vec2,
    pub radius: f32,
    pub maximum: f32,
    pub kind: WaveKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HazardKind {
    Beam,
    Geyser,
    Scorch,
    Gravity,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Hazard {
    pub kind: HazardKind,
    pub position: Vec2,
    pub end: Vec2,
    pub time: f32,
    pub delay: f32,
    pub duration: f32,
    pub radius: f32,
}
impl Hazard {
    pub fn active(&self) -> bool {
        self.time >= self.delay && self.time < self.delay + self.duration
    }
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
    Wake,
    Rubble,
    Steam,
    Sever,
    Overload,
    Seal,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Effect {
    pub kind: EffectKind,
    pub position: Vec2,
    pub remaining: f32,
    pub duration: f32,
    pub radius: f32,
}

#[derive(Clone, Debug)]
pub(super) struct Arena {
    pub phase: Phase,
    pub player: Player,
    pub boss: Boss,
    pub arrow: Arrow,
    pub map: Map,
    pub projectiles: Vec<Projectile>,
    pub waves: Vec<Wave>,
    pub effects: Vec<Effect>,
    pub hazards: Vec<Hazard>,
    pub elapsed: f32,
    pub visual_time: f32,
    pub phase_time: f32,
    pub camera: Vec2,
    pub deaths: u32,
    intro: Boss,
    intro_target: Vec2,
    seed: u32,
    hit_pause: f32,
    buffered_shot: Option<(Vec2, f32)>,
}

impl Arena {
    #[cfg(test)]
    pub fn new(guardian: Guardian, idle_time: f32) -> Self {
        Self::from_preview(Boss::new(guardian), idle_time, 0x6d2b79f5)
    }
    pub fn from_preview(boss: Boss, idle_time: f32, seed: u32) -> Self {
        let map = Map::new(boss.guardian);
        let mut seed = seed.max(1);
        let intro_target = if boss.guardian == Guardian::Pi {
            CENTER.plus(Vec2::new(0.0, -18.0))
        } else if map.is_clear(boss.position, boss.radius() + 1.0) {
            boss.position
        } else {
            CENTER
        };
        let position = map.spawn(&mut seed, intro_target);
        let mut player = Player::new(position);
        player.swimming = boss.guardian == Guardian::DeepSeek && !map.on_island(position);
        Self {
            phase: Phase::Awakening,
            player,
            boss,
            intro: boss,
            intro_target,
            arrow: Arrow::default(),
            map,
            projectiles: Vec::with_capacity(MAX_PROJECTILES),
            waves: Vec::with_capacity(MAX_WAVES),
            effects: Vec::with_capacity(MAX_EFFECTS),
            hazards: Vec::with_capacity(MAX_HAZARDS),
            elapsed: 0.0,
            visual_time: idle_time,
            phase_time: 0.0,
            camera: boss.position,
            deaths: 0,
            seed,
            hit_pause: 0.0,
            buffered_shot: None,
        }
    }
    pub fn needs_tick(&self) -> bool {
        self.phase != Phase::Victory || self.phase_time < VICTORY_DURATION
    }
    pub fn outcome_ready(&self) -> bool {
        self.phase == Phase::Victory && self.phase_time >= VICTORY_DURATION
    }
    pub fn home_opacity(&self) -> f32 {
        match self.phase {
            Phase::Awakening => 1.0 - smoothstep(self.phase_time / 0.5),
            Phase::Victory => smoothstep((self.phase_time - 2.6) / 0.7),
            _ => 0.0,
        }
    }
    pub fn ground_visibility(&self) -> f32 {
        match self.phase {
            Phase::Awakening => smoothstep(self.phase_time / 0.9),
            Phase::Victory => 1.0 - smoothstep((self.phase_time - 2.6) / 0.7),
            _ => 1.0,
        }
    }
    /// Both the radial windup and its live beams use these clipped rays.
    pub fn spokes(&self) -> [(Vec2, Vec2); 6] {
        std::array::from_fn(|ix| {
            let direction = Vec2::from_angle(self.boss.yaw + ix as f32 * TAU / 6.0);
            let from = self.boss.core.plus(direction.scale(9.0));
            (from, self.map.beam_end(from, direction))
        })
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
        self.effects.retain(|e| e.remaining > 0.0);
        let camera_target = self
            .player
            .position
            .lerp(self.boss.position, 0.10)
            .plus(controls.movement.scale(3.0));
        self.camera = self.camera.lerp(camera_target, 1.0 - (-STEP * 5.5).exp());
        match self.phase {
            Phase::Awakening => {
                // Keep the measured pose at t=0, then settle roaming into a safe
                // encounter position before any attack becomes possible.
                self.boss.position = self.intro.position.lerp(
                    self.intro_target,
                    smoothstep(self.phase_time / INTRO_DURATION),
                );
                self.boss.face(self.player.position, STEP * 1.8);
                let transition = smoothstep(self.phase_time / INTRO_DURATION);
                let pitch_delta = (-self.intro.pitch + PI).rem_euclid(TAU) - PI;
                self.boss.pitch = self.intro.pitch + pitch_delta * transition;
                self.boss.height = self.intro.height
                    + (self.boss.hover_height(self.visual_time) - self.intro.height) * transition;
                if self.boss.guardian == Guardian::OpenCode {
                    self.boss.height = Boss::rolling_height(self.boss.pitch);
                }
                self.boss.bank = self.intro.bank * (1.0 - transition);
                self.boss.stride += STEP * 2.0;
                self.boss.rest_hands();
                self.boss.rest_tentacles();
                self.boss.update_core();
                self.boss.previous_core = self.boss.core;
                if self.phase_time >= INTRO_DURATION {
                    self.phase = Phase::Battle;
                    self.phase_time = 0.0;
                    self.boss.enter(BossState::Watching, 0.75);
                }
                return;
            }
            Phase::Defeat => {
                if self.phase_time >= 1.0 {
                    self.respawn();
                }
                return;
            }
            Phase::Rebirth => {
                if self.phase_time >= 0.7 {
                    self.phase = Phase::Battle;
                    self.phase_time = 0.0;
                }
                return;
            }
            Phase::Victory => {
                self.boss.time += STEP;
                return;
            }
            Phase::Battle => {}
        }
        if self.hit_pause > 0.0 {
            self.hit_pause -= STEP;
            return;
        }
        self.elapsed += STEP;
        self.player.invulnerable = (self.player.invulnerable - STEP).max(0.0);
        self.player.roll_cooldown = (self.player.roll_cooldown - STEP).max(0.0);
        if let Some((direction, remaining)) = self.player.buffered_roll {
            if self.player.roll_cooldown <= 0.0 {
                self.start_roll(direction);
            } else {
                self.player.buffered_roll =
                    (remaining > STEP).then_some((direction, remaining - STEP));
            }
        }
        if let Some((direction, charge)) = self.buffered_shot.take()
            && self.arrow.state == ArrowState::Ready
            && self.player.roll_remaining <= 0.0
        {
            self.fire_arrow(direction, charge);
        }
        self.player.moving = false;
        let from = self.player.position;
        self.player.swimming =
            self.boss.guardian == Guardian::DeepSeek && !self.map.on_island(from);
        let rolling = self.player.roll_remaining > 0.0;
        let recalling = !rolling
            && self.arrow.state != ArrowState::Ready
            && (controls.recall || controls.shoot);
        let drawing = !rolling
            && !self.player.swimming
            && controls.shoot
            && self.arrow.state == ArrowState::Ready;
        if rolling {
            let progress = 1.0 - self.player.roll_remaining / ROLL_DURATION;
            let speed = 36.0 + (1.0 - progress).powi(2) * 46.0;
            self.player.position = self.map.move_body(
                from,
                from.plus(self.player.roll_direction.scale(speed * STEP)),
                PLAYER_RADIUS,
            );
            self.player.roll_remaining = (self.player.roll_remaining - STEP).max(0.0);
        } else if drawing {
            self.player.charge = (self.player.charge + STEP).min(0.9);
            self.player.facing = self.aim_direction(controls.aim);
        } else if !recalling && controls.movement.length() > 0.0 {
            let direction = controls.movement.normalized();
            let speed = 20.0 * self.map.speed(from);
            self.player.position = self.map.move_body(
                from,
                from.plus(direction.scale(speed * STEP)),
                PLAYER_RADIUS,
            );
            self.player.facing = direction;
            self.player.moving = self.player.position != from;
            self.player.stride += STEP * 10.0;
        }
        if self.player.swimming {
            self.player.charge = 0.0;
        }
        self.tick_boss();
        if self.phase != Phase::Battle {
            return;
        }
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
        if self.phase == Phase::Battle {
            self.tick_threats(from);
        }
    }
    pub fn respawn(&mut self) {
        let position = self.map.spawn(&mut self.seed, self.boss.position);
        self.player = Player::new(position);
        self.player.swimming =
            self.boss.guardian == Guardian::DeepSeek && !self.map.on_island(position);
        self.arrow = Arrow::default();
        self.clear_threats();
        self.boss.exposed = 0.0;
        self.boss.height = 0.0;
        self.boss.pitch = 0.0;
        self.boss.bank = 0.0;
        self.boss.rest_hands();
        self.boss.rest_tentacles();
        self.boss.severed = 0;
        self.boss.seals = 0;
        self.boss.seal_time = [0.0; 2];
        self.boss.strain = 0.0;
        self.boss.tether = 0.0;
        self.boss.attacks = 0;
        self.buffered_shot = None;
        self.hit_pause = 0.0;
        self.boss.enter(BossState::Watching, 1.1);
        self.boss.update_core();
        self.boss.previous_core = self.boss.core;
        self.phase = Phase::Rebirth;
        self.phase_time = 0.0;
        self.deaths = self.deaths.saturating_add(1);
        self.effect(EffectKind::Catch, position, 0.7, 4.0);
    }
    fn aim_direction(&self, aim: Option<Vec2>) -> Vec2 {
        aim.unwrap_or_else(|| self.aim_target())
            .minus(self.player.position)
            .normalized()
    }
    pub fn aim_target(&self) -> Vec2 {
        if self.boss.exposed <= 0.0 {
            if self.boss.guardian == Guardian::Claude
                && let Some(ix) = (0..6)
                    .filter(|ix| self.boss.tentacle_vulnerable(*ix))
                    .min_by(|a, b| {
                        self.boss
                            .tentacle_joint(*a)
                            .minus(self.player.position)
                            .length()
                            .total_cmp(
                                &self
                                    .boss
                                    .tentacle_joint(*b)
                                    .minus(self.player.position)
                                    .length(),
                            )
                    })
            {
                return self.boss.tentacle_joint(ix);
            }
            if self.boss.guardian == Guardian::Copilot
                && let Some(projectile) = self
                    .projectiles
                    .iter()
                    .find(|p| p.kind == ProjectileKind::ChargedOrb)
            {
                return projectile.position;
            }
        }
        self.boss.core
    }
    pub fn release_shot(&mut self, aim: Option<Vec2>) {
        if self.phase == Phase::Battle
            && self.arrow.state == ArrowState::Ready
            && self.player.roll_remaining <= 0.0
            && self.player.charge >= MIN_CHARGE
            && (self.boss.guardian != Guardian::DeepSeek
                || self.map.on_island(self.player.position))
        {
            let direction = self.aim_direction(aim);
            if self.hit_pause > 0.0 {
                self.buffered_shot = Some((direction, self.player.charge));
            } else {
                self.fire_arrow(direction, self.player.charge);
            }
        }
        self.player.charge = 0.0;
    }
    fn fire_arrow(&mut self, direction: Vec2, charge: f32) {
        self.arrow = Arrow {
            state: ArrowState::Flying,
            position: self.player.position.plus(direction.scale(1.6)),
            velocity: direction.scale(86.0 + charge * 28.0),
        };
        self.player.facing = direction;
        self.effect(EffectKind::Shot, self.arrow.position, 0.18, 2.0);
    }
    pub fn cancel_input(&mut self) {
        self.player.charge = 0.0;
        self.player.moving = false;
        self.buffered_shot = None;
        self.player.buffered_roll = None;
        if self.arrow.state == ArrowState::Returning {
            self.arrow.state = ArrowState::Lodged;
        }
    }
    pub fn roll(&mut self, movement: Vec2) {
        if self.phase != Phase::Battle {
            return;
        }
        if self.player.roll_cooldown > 0.0 {
            if self.player.roll_cooldown <= INPUT_BUFFER {
                self.player.buffered_roll = Some((movement, INPUT_BUFFER));
            }
            return;
        }
        self.start_roll(movement);
    }
    fn start_roll(&mut self, movement: Vec2) {
        self.cancel_input();
        self.player.roll_direction = if movement.length() > 0.0 {
            movement.normalized()
        } else {
            self.player.facing
        };
        self.player.facing = self.player.roll_direction;
        self.player.roll_remaining = ROLL_DURATION;
        self.player.roll_cooldown = 0.50;
        self.player.invulnerable = 0.30;
        self.effect(EffectKind::Roll, self.player.position, 0.38, 2.0);
    }
    fn accepts_core_hit(&self, from: Vec2) -> bool {
        if self.boss.exposed <= 0.0 {
            return false;
        }
        match self.boss.guardian {
            Guardian::Claude | Guardian::Codex | Guardian::Pi | Guardian::Copilot => true,
            Guardian::OpenCode => from.minus(self.boss.core).dot(self.boss.direction) > 0.0,
            Guardian::DeepSeek => {
                self.boss.state == BossState::Recovery || self.boss.state == BossState::Striking
            }
        }
    }
    fn tick_arrow(&mut self, recalling: bool) {
        if self.arrow.state == ArrowState::Ready {
            self.arrow.position = self.player.position;
            return;
        }
        if self.arrow.state == ArrowState::Tethered {
            self.arrow.position = self.boss.core;
            if !self.boss.is_inhaling() {
                self.arrow.state = ArrowState::Lodged;
                self.arrow.position = self.boss.core.plus(self.boss.direction.scale(9.0));
                self.boss.tether = 0.0;
            } else if recalling {
                self.boss.tether = (self.boss.tether + STEP / 0.62).min(1.0);
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss.position.plus(
                        self.player
                            .position
                            .minus(self.boss.position)
                            .normalized()
                            .scale(5.0 * STEP),
                    ),
                    self.boss.radius(),
                );
                if self.boss.tether >= 1.0 {
                    self.boss.opening(4.8);
                    self.hazards.retain(|h| h.kind != HazardKind::Gravity);
                    self.arrow.state = ArrowState::Returning;
                    self.arrow.velocity = Vec2::default();
                    self.effect(EffectKind::Rubble, self.boss.core, 1.2, 9.0);
                    self.hit_pause = 0.05;
                }
            }
            return;
        }
        if recalling {
            let speed = if self.arrow.state == ArrowState::Returning {
                (self.arrow.velocity.length() + STEP * 200.0).clamp(38.0, 116.0)
            } else {
                38.0
            };
            self.arrow.state = ArrowState::Returning;
            self.arrow.velocity = self
                .player
                .position
                .minus(self.arrow.position)
                .normalized()
                .scale(speed);
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
        enum Contact {
            Cover,
            Core,
            Armor,
            Mouth,
            Tentacle(usize),
            Drone(usize),
        }
        let mut contact: Option<(f32, Contact)> = self
            .map
            .cover_contact(from, to)
            .map(|(_, t)| (t, Contact::Cover));
        let mut consider = |time: Option<f32>, kind| {
            if let Some(time) = time
                && contact.as_ref().is_none_or(|(before, _)| time < *before)
            {
                contact = Some((time, kind));
            }
        };
        if self.accepts_core_hit(from) {
            consider(
                circle_hit(
                    Vec2::default(),
                    from.minus(self.boss.previous_core),
                    to.minus(self.boss.core),
                    CORE_RADIUS,
                ),
                Contact::Core,
            );
        }
        let swallowing = self.boss.guardian == Guardian::OpenCode
            && self.boss.is_inhaling()
            && from.minus(self.boss.core).dot(self.boss.direction) > 0.0;
        if swallowing {
            consider(circle_hit(self.boss.core, from, to, 3.2), Contact::Mouth);
        }
        for ix in 0..6 {
            if self.boss.tentacle_vulnerable(ix) {
                consider(
                    circle_hit(self.boss.tentacle_joint(ix), from, to, 2.4),
                    Contact::Tentacle(ix),
                );
            }
        }
        for (ix, projectile) in self.projectiles.iter().enumerate() {
            if projectile.kind == ProjectileKind::ChargedOrb {
                consider(
                    circle_hit(
                        Vec2::default(),
                        from.minus(projectile.position),
                        to.minus(projectile.position.plus(projectile.velocity.scale(STEP))),
                        2.7,
                    ),
                    Contact::Drone(ix),
                );
            }
        }
        if !returning
            && !swallowing
            && self.boss.state != BossState::Submerged
            && !self.accepts_core_hit(from)
        {
            consider(
                circle_hit(self.boss.core, from, to, self.boss.radius() * 0.7),
                Contact::Armor,
            );
        }
        if let Some((time, contact)) = contact {
            let position = from.lerp(to, time);
            match contact {
                Contact::Core => {
                    self.win();
                    return;
                }
                Contact::Mouth => {
                    self.arrow.state = ArrowState::Tethered;
                    self.arrow.position = self.boss.core;
                    self.arrow.velocity = Vec2::default();
                    self.boss.tether = 0.0;
                    self.effect(EffectKind::Catch, self.boss.core, 0.4, 4.0);
                    return;
                }
                Contact::Tentacle(ix) => {
                    self.boss.severed |= 1 << ix;
                    self.arrow.state = ArrowState::Lodged;
                    self.arrow.position = position;
                    self.effect(EffectKind::Sever, self.boss.tentacle_joint(ix), 1.2, 7.0);
                    self.hit_pause = 0.06;
                    if self.boss.severed.count_ones() >= 2 {
                        self.boss.opening(4.4);
                        self.effect(EffectKind::Steam, self.boss.core, 1.4, 8.0);
                    }
                    return;
                }
                Contact::Drone(ix) => {
                    let origin = self.projectiles[ix].position;
                    self.projectiles.retain(|p| {
                        !matches!(p.kind, ProjectileKind::Orb | ProjectileKind::ChargedOrb)
                    });
                    self.hazards.retain(|h| h.kind != HazardKind::Beam);
                    self.boss.opening(4.8);
                    self.arrow.state = ArrowState::Lodged;
                    self.arrow.position = position;
                    self.effect(EffectKind::Overload, origin, 1.0, 10.0);
                    self.effect(EffectKind::Overload, self.boss.core, 0.8, 7.0);
                    self.hit_pause = 0.07;
                    return;
                }
                Contact::Cover if returning => {}
                Contact::Cover | Contact::Armor => {
                    self.arrow.state = ArrowState::Lodged;
                    self.arrow.position = position;
                    self.effect(EffectKind::Armor, position, 0.35, 2.5);
                    return;
                }
            }
        }
        for hazard in &mut self.hazards {
            if hazard.kind == HazardKind::Scorch
                && segments_distance(from, to, hazard.position, hazard.end) < hazard.radius
            {
                hazard.duration = 0.0;
            }
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
        self.arrow.velocity = Vec2::default();
        self.effect(EffectKind::Catch, self.player.position, 0.3, 3.0);
    }
    fn clear_threats(&mut self) {
        self.projectiles.clear();
        self.waves.clear();
        self.hazards.clear();
    }
    fn win(&mut self) {
        self.phase = Phase::Victory;
        self.phase_time = 0.0;
        self.arrow.position = self.boss.core;
        self.arrow.state = ArrowState::Lodged;
        self.clear_threats();
        self.effects.clear();
        self.boss.enter(BossState::Fallen, VICTORY_DURATION);
        self.cancel_input();
        self.effect(EffectKind::Victory, self.boss.core, 2.7, 16.0);
    }
    fn hurt(&mut self) {
        if self.phase != Phase::Battle || self.player.invulnerable > 0.0 {
            return;
        }
        self.phase = Phase::Defeat;
        self.phase_time = 0.0;
        self.cancel_input();
        self.clear_threats();
        self.effect(EffectKind::Defeat, self.player.position, 0.9, 5.0);
    }
    fn tick_threats(&mut self, player_from: Vec2) {
        let mut hit = false;
        for hazard in &self.hazards {
            if hazard.kind == HazardKind::Gravity
                && hazard.active()
                && self.player.roll_remaining <= 0.0
            {
                let toward = hazard.position.minus(self.player.position);
                if toward.length() < hazard.radius && toward.length() > 10.0 {
                    self.player.position = self.map.move_body(
                        self.player.position,
                        self.player
                            .position
                            .plus(toward.normalized().scale(STEP * 5.0)),
                        PLAYER_RADIUS,
                    );
                }
            }
        }
        let broken_before = self.map.broken;
        for projectile in &mut self.projectiles {
            let from = projectile.position;
            projectile.previous_position = from;
            projectile.age += STEP;
            if projectile.kind == ProjectileKind::Orb && projectile.age < 1.4 {
                let direction = self.player.position.minus(from).normalized();
                projectile.velocity = projectile
                    .velocity
                    .normalized()
                    .lerp(direction, STEP * 1.1)
                    .normalized()
                    .scale(23.0);
            }
            if projectile.kind == ProjectileKind::ChargedOrb {
                if projectile.age < 1.0 {
                    projectile.velocity = Vec2::default();
                } else {
                    let direction = self.player.position.minus(from).normalized();
                    projectile.velocity = if projectile.velocity.length() < f32::EPSILON {
                        direction
                    } else {
                        projectile
                            .velocity
                            .normalized()
                            .lerp(direction, STEP * 1.8)
                            .normalized()
                    }
                    .scale(12.0);
                }
            }
            projectile.position = from.plus(projectile.velocity.scale(STEP));
            projectile.remaining -= STEP;
            if let Some((ix, _)) = self.map.cover_contact(from, projectile.position) {
                if matches!(
                    projectile.kind,
                    ProjectileKind::Block | ProjectileKind::Segment
                ) {
                    self.map.broken |= 1 << ix;
                }
                projectile.remaining = 0.0;
                continue;
            }
            let radius = match projectile.kind {
                ProjectileKind::ChargedOrb => 2.0,
                ProjectileKind::Block => 1.6,
                ProjectileKind::Orb | ProjectileKind::Segment => 1.0,
            };
            if segment_distance(
                Vec2::default(),
                player_from.minus(from),
                self.player.position.minus(projectile.position),
            ) < PLAYER_RADIUS + radius
                && (projectile.kind != ProjectileKind::ChargedOrb || projectile.age >= 1.0)
            {
                hit = true;
                projectile.remaining = 0.0;
            }
        }
        for ix in 0..self.map.pillars().len() {
            if (self.map.broken ^ broken_before) & (1 << ix) != 0 {
                let position = self.map.pillars()[ix].position;
                self.effect(EffectKind::Rubble, position, 1.3, 9.0);
                self.map.scar(position, 6.0, ScarKind::Crack);
            }
        }
        self.projectiles
            .retain(|p| p.remaining > 0.0 && p.position == p.position.clamped(1.0));
        for wave in &mut self.waves {
            let before = player_from.minus(wave.position).length() - wave.radius;
            wave.radius += STEP
                * match wave.kind {
                    WaveKind::Stone => 23.0,
                    WaveKind::Water => 18.0,
                };
            let after = self.player.position.minus(wave.position).length() - wave.radius;
            if before.min(after) < PLAYER_RADIUS + 0.45
                && before.max(after) > -PLAYER_RADIUS - 0.45
                && !(wave.kind == WaveKind::Water && self.map.on_island(self.player.position))
            {
                hit = true;
            }
        }
        self.waves.retain(|w| w.radius < w.maximum);
        for hazard in &mut self.hazards {
            let was_active = hazard.active();
            hazard.time += STEP;
            if !was_active && !hazard.active() {
                continue;
            }
            let distance = segments_distance(
                player_from,
                self.player.position,
                hazard.position,
                hazard.end,
            );
            if hazard.kind != HazardKind::Gravity && distance < hazard.radius + PLAYER_RADIUS {
                hit = true;
            }
        }
        self.hazards.retain(|h| h.time < h.delay + h.duration);
        if hit {
            self.hurt();
        }
    }
    fn wave(&mut self, position: Vec2, radius: f32, maximum: f32, kind: WaveKind) {
        if self.phase == Phase::Battle && self.waves.len() < MAX_WAVES {
            self.waves.push(Wave {
                position,
                radius,
                maximum,
                kind,
            });
        }
    }
    fn projectile(&mut self, position: Vec2, velocity: Vec2, kind: ProjectileKind) {
        if self.phase == Phase::Battle && self.projectiles.len() < MAX_PROJECTILES {
            self.projectiles.push(Projectile {
                position,
                previous_position: position,
                velocity,
                remaining: 6.0,
                kind,
                age: 0.0,
            });
        }
    }
    fn hazard(
        &mut self,
        kind: HazardKind,
        position: Vec2,
        end: Vec2,
        delay: f32,
        duration: f32,
        radius: f32,
    ) {
        if self.phase == Phase::Battle && self.hazards.len() < MAX_HAZARDS {
            self.hazards.push(Hazard {
                kind,
                position,
                end,
                time: 0.0,
                delay,
                duration,
                radius,
            });
        }
    }
    fn effect(&mut self, kind: EffectKind, position: Vec2, duration: f32, radius: f32) {
        if self.effects.len() < MAX_EFFECTS {
            self.effects.push(Effect {
                kind,
                position,
                remaining: duration,
                duration,
                radius,
            });
        }
    }
    fn impact(&mut self, position: Vec2, radius: f32) {
        self.effect(EffectKind::Impact, position, 0.85, radius);
        self.hit_pause = 0.055;
        if self.boss.guardian != Guardian::DeepSeek {
            self.map.scar(position, radius, ScarKind::Crack);
        }
        for ix in 0..self.map.pillars().len() {
            let pillar = self.map.pillars()[ix];
            if self.map.intact(ix)
                && pillar.position.minus(position).length() < radius + pillar.radius
            {
                self.map.broken |= 1 << ix;
                self.effect(EffectKind::Rubble, pillar.position, 1.3, 9.0);
            }
        }
        if self.player.position.minus(position).length() < radius + PLAYER_RADIUS {
            self.hurt();
        }
    }
    pub fn impact_strength(&self) -> f32 {
        self.effects
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EffectKind::Impact
                        | EffectKind::Victory
                        | EffectKind::Defeat
                        | EffectKind::Rubble
                        | EffectKind::Sever
                        | EffectKind::Overload
                )
            })
            .map(|e| ((e.remaining / e.duration - 0.65) / 0.35).max(0.0))
            .fold(0.0, f32::max)
    }
    pub fn shake(&self) -> Vec2 {
        Vec2::new(
            (self.visual_time * TAU * 17.0).sin(),
            (self.visual_time * TAU * 13.0).cos(),
        )
        .scale(self.impact_strength() * 0.65)
    }
}

#[path = "encounters.rs"]
mod encounters;
#[cfg(test)]
#[path = "combat_pilot_tests.rs"]
mod pilot_tests;
#[cfg(test)]
#[path = "combat_puzzle_tests.rs"]
mod puzzle_tests;
#[cfg(test)]
#[path = "combat_tests.rs"]
mod tests;
