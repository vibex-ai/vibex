//! Fixed-step, local combat. Rendering and GPUI never decide a collision.

use super::geometry::circle_hit;
pub(super) use super::geometry::{HEIGHT, Vec2, WIDTH, segment_distance, smoothstep};
pub(super) use super::guardian::{Attack, Boss, BossState, Guardian, STEMS, Shard};
use super::map::{BRAZIER, CENTER, FLAME_RADIUS, Map, ScarKind};
pub(super) use encounters::grok::{
    BLADE_RADIUS as GROK_BLADE_RADIUS, FRENZY_RADIUS as GROK_FRENZY_RADIUS,
};
use std::f32::consts::{PI, TAU};

pub(super) const STEP: f32 = 1.0 / 120.0;
pub(super) const INTRO_DURATION: f32 = 1.4;
pub(super) const VICTORY_DURATION: f32 = 3.3;
pub(super) const MIN_CHARGE: f32 = 0.20;
pub(super) const MAX_CHARGE: f32 = 0.8;
pub(super) const MAX_PROJECTILES: usize = 48;
pub(super) const MAX_EFFECTS: usize = 64;
pub(super) const MAX_WAVES: usize = 8;
pub(super) const MAX_HAZARDS: usize = 24;
pub(super) const MAX_BEAMS: usize = 8;
pub(super) const PLAYER_RADIUS: f32 = 0.9;
pub(super) const CORE_RADIUS: f32 = 1.8;
pub(super) const ROLL_DURATION: f32 = 0.24;
const ROLL_COOLDOWN: f32 = 0.32;
const INPUT_BUFFER: f32 = 0.12;
const WALK_SPEED: f32 = 30.0;
const SPRINT: f32 = 1.45;
const GOO_SLOW: f32 = 0.55;
/// A knockback still this fast when it meets a wall crushes the archer.
const CRUSH_SPEED: f32 = 18.0;
pub(super) const KNOCKBACK: f32 = 58.0;

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
}
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Controls {
    pub movement: Vec2,
    pub shoot: bool,
    pub recall: bool,
    pub aim: Option<Vec2>,
    pub sprint: bool,
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
    pub sprinting: bool,
    pub slowed: bool,
    pub knockback: Vec2,
    roll_direction: Vec2,
    buffered_roll: Option<(Vec2, f32)>,
    dust: f32,
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
            sprinting: false,
            slowed: false,
            knockback: Vec2::default(),
            roll_direction: Vec2::default(),
            buffered_roll: None,
            dust: 0.0,
        }
    }
    pub fn knocked(&self) -> bool {
        self.knockback.length() > 6.0
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Arrow {
    pub state: ArrowState,
    pub position: Vec2,
    pub velocity: Vec2,
    /// Seconds of flame left after passing through the lit brazier.
    pub burning: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectileKind {
    /// Pushes the archer away instead of killing outright.
    Orb,
    Block,
    /// A fast, lethal bolt from Cline's eyes.
    Bolt,
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
    /// Harmless ember slime that slows the archer.
    Goo,
    /// A falling code block that stays behind as an obstacle.
    Drop,
    /// A falling shard from the terrace canopy.
    Icicle,
    /// A block of Kimi's long context, laid down behind it as it slides. It
    /// kills on touch until it fades, and an arrow through it clears it.
    Trail,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Hazard {
    pub kind: HazardKind,
    pub position: Vec2,
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
/// A straight, lethal line. It shows where it will fire for `delay`, then
/// burns for `duration`; a column in its way ends it where the column stands.
#[derive(Clone, Copy, Debug)]
pub(super) struct Beam {
    pub from: Vec2,
    pub to: Vec2,
    pub width: f32,
    pub time: f32,
    pub delay: f32,
    pub duration: f32,
}
impl Beam {
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
    Victory,
    Defeat,
    Wake,
    Rubble,
    Steam,
    Burst,
    Spark,
    Dust,
    Ignite,
    Flash,
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
    pub beams: Vec<Beam>,
    pub elapsed: f32,
    /// The duel's own wall clock: it starts when the intro ends, keeps counting
    /// through deaths and rebirths, and freezes on victory so the recorded
    /// clear time is the one the reader saw.
    pub run_time: f32,
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
            super::map::THRONE
        } else if boss.guardian == Guardian::ZCode {
            super::map::GATE
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
            beams: Vec::with_capacity(MAX_BEAMS),
            elapsed: 0.0,
            run_time: 0.0,
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
    /// The clear time this run records, in whole milliseconds, once it has been
    /// won. `None` while the duel is still running.
    ///
    /// The run clock starts as the intro hands control to the archer and keeps
    /// counting through deaths, so a clear time measures the whole attempt
    /// rather than one life. It freezes on victory, and it rounds up so a run
    /// can never claim a tenth of a second it did not spend.
    pub fn clear_millis(&self) -> Option<u64> {
        if self.phase != Phase::Victory {
            return None;
        }
        // A millisecond is the finest resolution the clock shows, so a run that
        // has barely begun still records the smallest positive time instead of
        // claiming an impossible zero.
        Some(((self.run_time.max(0.0) * 1000.0).ceil() as u64).max(1))
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
    /// The brazier burns only while the knot holds a forge plate down.
    pub fn brazier_lit(&self) -> bool {
        self.boss.guardian == Guardian::Codex && self.boss.plate.is_some()
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
            .lerp(self.boss.position, 0.12)
            .plus(controls.movement.scale(4.0));
        self.camera = self.camera.lerp(camera_target, 1.0 - (-STEP * 6.5).exp());
        match self.phase {
            Phase::Awakening => {
                self.awaken();
                return;
            }
            Phase::Defeat => {
                // Dying and coming back are part of the attempt, so the run
                // clock keeps moving while the archer is down.
                self.run_time += STEP;
                if self.phase_time >= 1.0 {
                    self.respawn();
                }
                return;
            }
            Phase::Rebirth => {
                self.run_time += STEP;
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
        // The run clock belongs to the duel, not to one life: it is stepped for
        // every live frame, before hit pauses and deaths can return early, so
        // the recorded clear time measures the whole attempt.
        self.run_time += STEP;
        if self.hit_pause > 0.0 {
            self.hit_pause -= STEP;
            return;
        }
        self.elapsed += STEP;
        self.player.invulnerable = (self.player.invulnerable - STEP).max(0.0);
        self.player.roll_cooldown = (self.player.roll_cooldown - STEP).max(0.0);
        if let Some((direction, remaining)) = self.player.buffered_roll {
            if self.player.roll_cooldown <= 0.0 && !self.player.knocked() {
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
        let from = self.player.position;
        self.move_player(controls);
        let recalling = self.player.roll_remaining <= 0.0
            && self.arrow.state != ArrowState::Ready
            && (controls.recall || controls.shoot);
        self.tick_boss();
        if self.phase != Phase::Battle {
            return;
        }
        self.body_contact(from);
        if self.phase == Phase::Battle {
            self.tick_arrow(recalling);
        }
        if self.phase == Phase::Battle {
            self.tick_threats(from);
        }
    }
    /// Keep the measured pose at t=0, then settle roaming into a safe
    /// encounter pose before any attack becomes possible.
    fn awaken(&mut self) {
        let transition = smoothstep(self.phase_time / INTRO_DURATION);
        self.boss.position = self.intro.position.lerp(self.intro_target, transition);
        self.boss.face(self.player.position, STEP * 1.8);
        let upright = |angle: f32| angle + ((-angle + PI).rem_euclid(TAU) - PI) * transition;
        self.boss.pitch = upright(self.intro.pitch);
        self.boss.bank = upright(self.intro.bank);
        self.boss.height = self.intro.height
            + (self.boss.hover_height(self.visual_time) - self.intro.height) * transition;
        self.boss.squash = self.intro.squash * (1.0 - transition);
        self.boss.lid = self.intro.lid * (1.0 - transition);
        if self.boss.guardian == Guardian::DeepSeek {
            self.boss.unravel = self.intro.unravel * (1.0 - transition);
        }
        self.boss.stride += STEP * 2.0;
        let hover = if self.boss.guardian == Guardian::ZCode {
            super::guardian::ZCODE_HOVER
        } else {
            0.0
        };
        for ix in 0..2 {
            self.boss.hands[ix] = self.intro.hands[ix].lerp(self.boss.resting_hand(ix), transition);
            self.boss.hand_heights[ix] =
                self.intro.hand_heights[ix] + (hover - self.intro.hand_heights[ix]) * transition;
        }
        self.boss.update_core();
        self.boss.previous_core = self.boss.core;
        if self.phase_time >= INTRO_DURATION {
            let wrap = |angle: f32| (angle + PI).rem_euclid(TAU) - PI;
            self.boss.pitch = wrap(self.boss.pitch);
            self.boss.bank = wrap(self.boss.bank);
            self.phase = Phase::Battle;
            self.phase_time = 0.0;
            self.boss.enter(BossState::Watching, 0.6);
        }
    }
    fn move_player(&mut self, controls: Controls) {
        self.player.moving = false;
        self.player.sprinting = false;
        let from = self.player.position;
        self.player.swimming =
            self.boss.guardian == Guardian::DeepSeek && !self.map.on_island(from);
        self.player.slowed = self.hazards.iter().any(|h| {
            h.kind == HazardKind::Goo && h.active() && h.position.minus(from).length() < h.radius
        });
        let rolling = self.player.roll_remaining > 0.0;
        let drawing = !rolling
            && !self.player.knocked()
            && !self.player.swimming
            && controls.shoot
            && self.arrow.state == ArrowState::Ready;
        let recalling = !rolling
            && self.arrow.state != ArrowState::Ready
            && (controls.recall || controls.shoot);
        if self.player.knocked() {
            let step = self.player.knockback.scale(STEP);
            let target = from.plus(step);
            let moved = self.map.walk(from, target, PLAYER_RADIUS);
            self.player.position = moved;
            if target.minus(moved).length() > step.length() * 0.35
                && self.player.knockback.length() > CRUSH_SPEED
            {
                self.player.knockback = Vec2::default();
                self.effect(EffectKind::Impact, moved, 0.6, 3.5);
                self.hurt();
                return;
            }
            self.player.knockback = self.player.knockback.scale((-STEP * 6.0).exp());
            self.player.charge = 0.0;
        } else if rolling {
            let progress = 1.0 - self.player.roll_remaining / ROLL_DURATION;
            let speed = 48.0 + (1.0 - progress).powi(2) * 60.0;
            self.player.position = self.map.walk(
                from,
                from.plus(self.player.roll_direction.scale(speed * STEP)),
                PLAYER_RADIUS,
            );
            self.player.roll_remaining = (self.player.roll_remaining - STEP).max(0.0);
        } else if drawing {
            self.player.charge = (self.player.charge + STEP).min(MAX_CHARGE);
            self.player.facing = self.aim_direction(controls.aim);
        } else if !recalling && controls.movement.length() > 0.0 {
            let direction = controls.movement.normalized();
            self.player.sprinting = controls.sprint && !self.player.swimming;
            let speed = WALK_SPEED
                * self.map.speed(from)
                * if self.player.slowed { GOO_SLOW } else { 1.0 }
                * if self.player.sprinting { SPRINT } else { 1.0 };
            self.player.position = self.map.walk(
                from,
                from.plus(direction.scale(speed * STEP)),
                PLAYER_RADIUS,
            );
            self.player.facing = direction;
            self.player.moving = self.player.position != from;
            self.player.stride += STEP * speed / 3.0;
        }
        if self.player.swimming {
            self.player.charge = 0.0;
        }
        if self.player.moving || rolling {
            self.player.dust -= STEP;
            if self.player.dust <= 0.0 {
                self.player.dust = if self.player.sprinting || rolling {
                    0.07
                } else {
                    0.16
                };
                let kind = if self.player.swimming {
                    EffectKind::Wake
                } else {
                    EffectKind::Dust
                };
                self.effect(kind, from, 0.4, 1.6);
            }
        }
    }
    fn body_contact(&mut self, from: Vec2) {
        let player = self.player.position;
        // The glyph floats above its dais, which the archer cannot enter.
        if self.boss.guardian != Guardian::Pi
            && !self.boss.airborne()
            && segment_distance(
                Vec2::default(),
                from.minus(self.boss.previous_position),
                player.minus(self.boss.position),
            ) < self.boss.radius() + PLAYER_RADIUS
        {
            self.hurt();
            return;
        }
        let crushed = self.boss.shards.iter().any(|shard| {
            shard.alive
                && shard.height < 2.5
                && segment_distance(
                    Vec2::default(),
                    from.minus(shard.previous),
                    player.minus(shard.position),
                ) < shard.radius() * 0.8 + PLAYER_RADIUS
        });
        if crushed {
            self.hurt();
        }
    }
    pub fn respawn(&mut self) {
        let position = self.map.spawn(&mut self.seed, self.boss.position);
        self.player = Player::new(position);
        self.player.swimming =
            self.boss.guardian == Guardian::DeepSeek && !self.map.on_island(position);
        self.arrow = Arrow::default();
        self.clear_threats();
        self.map.blocks.clear();
        let boss = &mut self.boss;
        boss.exposed = 0.0;
        boss.height = 0.0;
        boss.pitch = 0.0;
        boss.bank = 0.0;
        boss.squash = 0.0;
        boss.lid = 0.0;
        boss.fade = 1.0;
        boss.unravel = 0.0;
        boss.plate = None;
        boss.shield = 0.0;
        boss.dashes = 0;
        boss.detached = false;
        boss.pinned = [false; 2];
        boss.flash = 0.0;
        boss.generation = 0;
        boss.shards = [Shard::default(); 4];
        boss.guard = 1;
        boss.attacks = 0;
        boss.rest_hands();
        boss.enter(BossState::Watching, 1.1);
        boss.update_core();
        boss.previous_core = boss.core;
        self.buffered_shot = None;
        self.hit_pause = 0.0;
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
    /// Keyboard aim follows the weak point once it is open, the lit flame
    /// before the knot is melted, and otherwise the body.
    pub fn aim_target(&self) -> Vec2 {
        if self.boss.exposed > 0.0 {
            self.boss.core
        } else if self.brazier_lit() && self.arrow.burning <= 0.0 {
            flame_center()
        } else {
            self.boss.body_center()
        }
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
            velocity: direction.scale(100.0 + charge * 45.0),
            burning: 0.0,
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
        if self.phase != Phase::Battle || self.player.swimming {
            return;
        }
        if self.player.roll_cooldown > 0.0 || self.player.knocked() {
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
        self.player.roll_cooldown = ROLL_COOLDOWN;
        self.player.invulnerable = self.player.invulnerable.max(0.26);
        self.effect(EffectKind::Roll, self.player.position, 0.3, 2.0);
    }
    fn accepts_core_hit(&self, from: Vec2) -> bool {
        if self.boss.exposed <= 0.0 {
            return false;
        }
        match self.boss.guardian {
            Guardian::Claude => self.boss.generation >= 4,
            Guardian::Codex => !self.boss.cube_shielded(),
            Guardian::Pi | Guardian::DeepSeek => true,
            // The blue square is on the tile's face.
            Guardian::Kimi => from.minus(self.boss.core).dot(self.boss.direction) > 0.0,
            Guardian::OpenCode => from.minus(self.boss.core).dot(self.boss.direction) > 0.0,
            Guardian::Copilot => from.minus(self.boss.core).dot(self.boss.direction) < 0.0,
            // The bared core looks down the hall.
            Guardian::ZCode => from.y > self.boss.core.y - 1.0,
            // The core in the back of the ring, open while it spins and reels.
            Guardian::Grok => from.minus(self.boss.core).dot(self.boss.direction) < 0.0,
            // The whole back is open, but the head always turns to the archer.
            Guardian::Cline => from.minus(self.boss.core).dot(self.boss.direction) < 0.0,
            // The pointer facet takes an arrow only from the side it faces.
            Guardian::Cursor => super::cube::ground_direction(self.boss.facet, self.boss.yaw)
                .is_some_and(|facing| from.minus(self.boss.core).dot(facing) > 0.0),
        }
    }
    fn tick_arrow(&mut self, recalling: bool) {
        if self.arrow.state == ArrowState::Ready {
            self.arrow.position = self.player.position;
            self.arrow.burning = 0.0;
            return;
        }
        self.arrow.burning = (self.arrow.burning - STEP).max(0.0);
        if recalling {
            let speed = if self.arrow.state == ArrowState::Returning {
                (self.arrow.velocity.length() + STEP * 260.0).clamp(48.0, 130.0)
            } else {
                48.0
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
            if self.arrow.position.minus(self.player.position).length() < 2.2 {
                self.catch_arrow();
            }
            return;
        }
        let from = self.arrow.position;
        let to = from.plus(self.arrow.velocity.scale(STEP));
        let returning = self.arrow.state == ArrowState::Returning;
        // An arrow through a block of trail clears it and flies on.
        let before = self.hazards.len();
        self.hazards.retain(|h| {
            h.kind != HazardKind::Trail || segment_distance(h.position, from, to) > h.radius
        });
        if self.hazards.len() < before {
            self.effect(EffectKind::Burst, from, 0.4, 4.0);
        }
        if self.brazier_lit() && segment_distance(flame_center(), from, to) < FLAME_RADIUS {
            if self.arrow.burning <= 0.0 {
                self.effect(EffectKind::Ignite, flame_center(), 0.5, 4.0);
            }
            self.arrow.burning = 4.0;
        }
        enum Contact {
            Cover,
            Core,
            Armor,
            Carrier,
            Shard(usize),
            Knot,
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
        let boss = self.boss;
        let open = self.accepts_core_hit(from);
        if open {
            consider(
                circle_hit(
                    Vec2::default(),
                    from.minus(boss.previous_core),
                    to.minus(boss.core),
                    CORE_RADIUS,
                ),
                Contact::Core,
            );
            if returning && boss.guardian == Guardian::Cline {
                // A recalled arrow meets the whole back on its way through.
                consider(
                    circle_hit(boss.body_center(), from, to, boss.radius() * 0.8),
                    Contact::Core,
                );
            }
        }
        let visible = boss.state != BossState::Submerged && boss.fade > 0.5;
        if boss.guardian == Guardian::Claude {
            if boss.generation < 4 {
                consider(
                    circle_hit(boss.body_center(), from, to, boss.radius() * 0.8),
                    Contact::Carrier,
                );
            }
            for (ix, shard) in boss.shards.iter().enumerate() {
                if shard.alive {
                    consider(
                        circle_hit(shard.center(), from, to, shard.radius() * 0.8),
                        Contact::Shard(ix),
                    );
                }
            }
        } else if boss.guardian == Guardian::Codex
            && self.arrow.burning > 0.0
            && boss.exposed <= 0.0
        {
            consider(circle_hit(boss.body_center(), from, to, 8.5), Contact::Knot);
        }
        if !returning && visible && !open && boss.guardian != Guardian::Claude {
            let radius = if boss.guardian == Guardian::Codex && boss.exposed > 0.0 {
                3.5
            } else {
                boss.radius() * 0.75
            };
            consider(
                circle_hit(boss.body_center(), from, to, radius),
                Contact::Armor,
            );
            // A closed weak point is armor too, wherever it sits on the body.
            consider(
                circle_hit(boss.core, from, to, CORE_RADIUS + 0.7),
                Contact::Armor,
            );
        }
        if !returning && boss.guardian == Guardian::Pi {
            // Stems near the ground are stone in the arrow's path; a raised
            // stem passes overhead.
            for (ix, (width, length)) in STEMS.into_iter().enumerate() {
                if boss.hand_heights[ix] >= 10.0 {
                    continue;
                }
                for part in 0..3 {
                    let rise = boss.hand_heights[ix] + length * (part as f32 + 0.5) / 3.0;
                    let point = boss.hands[ix].plus(Vec2::new(0.0, -rise));
                    consider(circle_hit(point, from, to, width * 0.5), Contact::Armor);
                }
            }
        }
        if !returning && boss.guardian == Guardian::ZCode {
            // A fist near the floor is stone in the arrow's path.
            for ix in 0..2 {
                if boss.hand_heights[ix] < 8.0 {
                    let point = boss.hands[ix].plus(Vec2::new(0.0, -boss.hand_heights[ix]));
                    consider(circle_hit(point, from, to, 3.5), Contact::Armor);
                }
            }
        }
        if let Some((time, contact)) = contact {
            let position = from.lerp(to, time);
            match contact {
                Contact::Core => {
                    self.win();
                    return;
                }
                Contact::Carrier => {
                    self.split_spark(self.arrow.velocity.normalized());
                    self.lodge(position);
                    return;
                }
                Contact::Shard(ix) => {
                    let shard = self.boss.shards[ix];
                    self.boss.shards[ix].alive = false;
                    self.effect(
                        EffectKind::Burst,
                        shard.center(),
                        0.7,
                        5.0 + shard.size * 4.0,
                    );
                    if !returning {
                        self.lodge(position);
                        return;
                    }
                }
                Contact::Knot => {
                    self.unravel_knot();
                    self.lodge(position);
                    return;
                }
                Contact::Cover if returning => {}
                Contact::Cover | Contact::Armor => {
                    if matches!(contact, Contact::Armor) {
                        self.boss.flash = self.boss.flash.max(0.06);
                    }
                    self.lodge(position);
                    self.effect(EffectKind::Armor, position, 0.35, 2.5);
                    return;
                }
            }
        }
        self.arrow.position = to.clamped(2.0);
        if returning && segment_distance(self.player.position, from, to) < 1.8 {
            self.catch_arrow();
        } else if self.arrow.position != to {
            self.arrow.state = ArrowState::Lodged;
        }
    }
    fn lodge(&mut self, position: Vec2) {
        self.arrow.state = ArrowState::Lodged;
        self.arrow.position = position;
        self.arrow.burning = 0.0;
    }
    fn catch_arrow(&mut self) {
        self.arrow.state = ArrowState::Ready;
        self.arrow.position = self.player.position;
        self.arrow.velocity = Vec2::default();
        self.arrow.burning = 0.0;
        self.effect(EffectKind::Catch, self.player.position, 0.3, 3.0);
    }
    fn clear_threats(&mut self) {
        self.projectiles.clear();
        self.waves.clear();
        self.hazards.clear();
        self.beams.clear();
    }
    fn win(&mut self) {
        self.phase = Phase::Victory;
        self.phase_time = 0.0;
        self.arrow.position = self.boss.core;
        self.arrow.state = ArrowState::Lodged;
        self.arrow.burning = 0.0;
        self.clear_threats();
        self.effects.clear();
        for shard in &mut self.boss.shards {
            shard.alive = false;
        }
        self.boss.exposed = 0.0;
        self.boss.enter(BossState::Fallen, VICTORY_DURATION);
        self.cancel_input();
        self.effect(EffectKind::Flash, self.boss.core, 0.35, 30.0);
        self.effect(EffectKind::Victory, self.boss.core, 2.7, 16.0);
    }
    fn hurt(&mut self) {
        if self.phase != Phase::Battle || self.player.invulnerable > 0.0 {
            return;
        }
        self.phase = Phase::Defeat;
        self.phase_time = 0.0;
        self.player.knockback = Vec2::default();
        self.cancel_input();
        self.clear_threats();
        self.effect(EffectKind::Defeat, self.player.position, 0.9, 5.0);
    }
    fn tick_threats(&mut self, player_from: Vec2) {
        let mut hit = false;
        let player = self.player.position;
        let shielded = self.player.invulnerable > 0.0;
        let mut pushes = Vec::new();
        for projectile in &mut self.projectiles {
            let from = projectile.position;
            projectile.previous_position = from;
            projectile.age += STEP;
            if projectile.kind == ProjectileKind::Orb && projectile.age < 0.9 {
                let toward = player.minus(from).normalized();
                let speed = projectile.velocity.length();
                projectile.velocity = projectile
                    .velocity
                    .normalized()
                    .lerp(toward, STEP * 1.6)
                    .normalized()
                    .scale(speed);
            }
            projectile.position = from.plus(projectile.velocity.scale(STEP));
            projectile.remaining -= STEP;
            if self.map.cover_hit(from, projectile.position) {
                projectile.remaining = 0.0;
                continue;
            }
            let radius = match projectile.kind {
                ProjectileKind::Orb => 1.7,
                ProjectileKind::Block => 1.5,
                ProjectileKind::Bolt => 1.2,
            };
            if segment_distance(
                Vec2::default(),
                player_from.minus(from),
                player.minus(projectile.position),
            ) < PLAYER_RADIUS + radius
            {
                projectile.remaining = 0.0;
                match projectile.kind {
                    ProjectileKind::Block | ProjectileKind::Bolt => hit = true,
                    ProjectileKind::Orb if !shielded => {
                        pushes.push(projectile.velocity.normalized());
                    }
                    ProjectileKind::Orb => {}
                }
            }
        }
        self.projectiles
            .retain(|p| p.remaining > 0.0 && p.position == p.position.clamped(1.0));
        for push in pushes {
            self.player.knockback = push.scale(KNOCKBACK);
            self.player.charge = 0.0;
            self.player.roll_remaining = 0.0;
            self.effect(EffectKind::Spark, player, 0.4, 4.0);
            self.hit_pause = self.hit_pause.max(0.04);
        }
        for wave in &mut self.waves {
            let before = player_from.minus(wave.position).length() - wave.radius;
            wave.radius += STEP
                * match wave.kind {
                    WaveKind::Stone => 26.0,
                    WaveKind::Water => 55.0,
                };
            let after = player.minus(wave.position).length() - wave.radius;
            if before.min(after) < PLAYER_RADIUS + 0.45
                && before.max(after) > -PLAYER_RADIUS - 0.45
                && !(wave.kind == WaveKind::Water && self.map.on_island(player))
            {
                hit = true;
            }
        }
        self.waves.retain(|w| w.radius < w.maximum);
        let mut landings = Vec::new();
        for hazard in &mut self.hazards {
            let was_active = hazard.active();
            hazard.time += STEP;
            if !hazard.active() {
                continue;
            }
            if !was_active {
                landings.push((hazard.kind, hazard.position, hazard.radius));
            }
            if hazard.kind != HazardKind::Goo
                && segment_distance(hazard.position, player_from, player)
                    < hazard.radius + PLAYER_RADIUS
            {
                hit = true;
            }
        }
        self.hazards.retain(|h| h.time < h.delay + h.duration);
        for beam in &mut self.beams {
            beam.time += STEP;
            if beam.active()
                && segment_distance(player, beam.from, beam.to).min(segment_distance(
                    player_from,
                    beam.from,
                    beam.to,
                )) < beam.width + PLAYER_RADIUS
            {
                hit = true;
            }
        }
        self.beams.retain(|b| b.time < b.delay + b.duration);
        for (kind, position, radius) in landings {
            match kind {
                HazardKind::Drop => {
                    self.map.drop_block(position);
                    self.effect(EffectKind::Impact, position, 0.5, radius + 1.0);
                }
                HazardKind::Icicle => {
                    self.effect(EffectKind::Rubble, position, 0.7, radius + 2.0);
                    self.map.scar(position, radius, ScarKind::Crack);
                }
                HazardKind::Goo | HazardKind::Trail => {}
            }
        }
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
                remaining: 4.0,
                kind,
                age: 0.0,
            });
        }
    }
    /// Fires a beam from `from` along `direction`, ended by the rim or the
    /// first column in its way.
    pub(super) fn beam(
        &mut self,
        from: Vec2,
        direction: Vec2,
        width: f32,
        delay: f32,
        duration: f32,
    ) {
        if self.phase == Phase::Battle && self.beams.len() < MAX_BEAMS {
            self.beams.push(Beam {
                from,
                to: self.map.beam_end(from, direction),
                width,
                time: 0.0,
                delay,
                duration,
            });
        }
    }
    pub(super) fn hazard(
        &mut self,
        kind: HazardKind,
        position: Vec2,
        delay: f32,
        duration: f32,
        radius: f32,
    ) {
        if self.phase != Phase::Battle {
            return;
        }
        if self.hazards.len() == MAX_HAZARDS {
            // Old slime yields first, then the oldest of a trail; a falling
            // threat is never dropped.
            let yields = self
                .hazards
                .iter()
                .position(|h| h.kind == HazardKind::Goo)
                .or_else(|| {
                    self.hazards
                        .iter()
                        .position(|h| h.kind == HazardKind::Trail)
                });
            match yields {
                Some(ix) => {
                    self.hazards.remove(ix);
                }
                None => return,
            }
        }
        self.hazards.push(Hazard {
            kind,
            position,
            time: 0.0,
            delay,
            duration,
            radius,
        });
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
    /// A heavy landing: scars, breaks columns and crushes blocks in reach.
    fn impact(&mut self, position: Vec2, radius: f32) {
        self.impact_with(position, radius, 0.03);
    }
    fn impact_with(&mut self, position: Vec2, radius: f32, pause: f32) {
        self.effect(EffectKind::Impact, position, 0.85, radius);
        self.hit_pause = self.hit_pause.max(pause);
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
        for block in self.map.crush_blocks(position, radius) {
            self.effect(EffectKind::Rubble, block, 0.9, 4.0);
        }
        if self.player.position.minus(position).length() < radius + PLAYER_RADIUS {
            self.hurt();
        }
    }
    fn break_pillar(&mut self, ix: usize) {
        self.map.broken |= 1 << ix;
        let p = self.map.pillars()[ix].position;
        self.effect(EffectKind::Rubble, p, 1.4, 12.0);
        self.map.scar(p, 8.0, ScarKind::Crack);
    }
    fn random(&mut self) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed
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
                        | EffectKind::Burst
                        | EffectKind::Flash
                )
            })
            .map(|e| ((e.remaining / e.duration - 0.6) / 0.4).max(0.0) * (e.radius / 8.0).min(1.5))
            .fold(0.0, f32::max)
    }
    pub fn shake(&self) -> Vec2 {
        Vec2::new(
            (self.visual_time * TAU * 17.0).sin(),
            (self.visual_time * TAU * 13.0).cos(),
        )
        .scale(self.impact_strength() * 1.1)
    }
}

/// Aim-plane center of the brazier flame, which rises above its bowl.
pub(super) fn flame_center() -> Vec2 {
    BRAZIER.plus(Vec2::new(0.0, -3.0))
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
