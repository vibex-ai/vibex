//! Deterministic combat for the local ASCII arena. Coordinates use square
//! world units; a text cell covers one unit across and two units vertically.

use std::f32::consts::TAU;

pub(super) const WIDTH: f32 = 96.0;
pub(super) const HEIGHT: f32 = 56.0;
pub(super) const STEP: f32 = 1.0 / 60.0;
pub(super) const MAX_PROJECTILES: usize = 96;
const PLAYER_RADIUS: f32 = 1.25;
const CORE_RADIUS: f32 = 2.0;
const MIN_CHARGE: f32 = 0.22;
pub(super) const FOCUS_COST: f32 = 60.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn plus(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }

    pub fn minus(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }

    pub fn scale(self, scale: f32) -> Self {
        Self::new(self.x * scale, self.y * scale)
    }

    pub fn length(self) -> f32 {
        self.x.hypot(self.y)
    }

    pub fn normalized(self) -> Self {
        let length = self.length();
        if length > f32::EPSILON {
            self.scale(1.0 / length)
        } else {
            Self::new(0.0, -1.0)
        }
    }

    fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y
    }

    fn clamped(self, margin: f32) -> Self {
        Self::new(
            self.x.clamp(margin, WIDTH - margin),
            self.y.clamp(margin, HEIGHT - margin),
        )
    }
}

pub(super) fn segment_distance(point: Vec2, from: Vec2, to: Vec2) -> f32 {
    let segment = to.minus(from);
    let length_squared = segment.dot(segment);
    let progress = if length_squared > f32::EPSILON {
        (point.minus(from).dot(segment) / length_squared).clamp(0.0, 1.0)
    } else {
        0.0
    };
    point.minus(from.plus(segment.scale(progress))).length()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Guardian {
    Ember,
    Knot,
    Prism,
}

impl Guardian {
    pub const ALL: [Self; 3] = [Self::Ember, Self::Knot, Self::Prism];

    pub fn next(self) -> Self {
        match self {
            Self::Ember => Self::Knot,
            Self::Knot => Self::Prism,
            Self::Prism => Self::Ember,
        }
    }

    pub fn agent(self) -> &'static str {
        match self {
            Self::Ember => "Claude",
            Self::Knot => "Codex",
            Self::Prism => "Gemini",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
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
    /// Keyboard-only play aims at the real core. Pointer aim is in world units.
    pub aim: Option<Vec2>,
}

#[derive(Clone, Debug)]
pub(super) struct Player {
    pub position: Vec2,
    pub facing: Vec2,
    pub health: u8,
    pub focus: f32,
    pub charge: f32,
    pub invulnerable: f32,
    pub roll_remaining: f32,
    pub roll_cooldown: f32,
    roll_direction: Vec2,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Arrow {
    pub state: ArrowState,
    pub position: Vec2,
    pub velocity: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Attack {
    Sunburst,
    Fan,
    Charge,
    Cross,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Windup {
    pub attack: Attack,
    pub origin: Vec2,
    pub target: Vec2,
    pub remaining: f32,
    pub duration: f32,
}

#[derive(Clone, Debug)]
pub(super) struct Boss {
    pub guardian: Guardian,
    pub position: Vec2,
    pub core: Vec2,
    pub exposed: f32,
    pub windup: Option<Windup>,
    pub charging: bool,
    pub charge_direction: Vec2,
    pub attacks: u32,
    rest: f32,
}

impl Boss {
    pub fn radius(&self) -> f32 {
        match self.guardian {
            Guardian::Ember => 5.8,
            Guardian::Knot => 5.0,
            Guardian::Prism => 4.8,
        }
    }

    pub fn mirage(&self) -> Vec2 {
        Vec2::new(WIDTH - self.position.x, self.position.y)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Projectile {
    pub position: Vec2,
    pub velocity: Vec2,
    pub remaining: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EffectKind {
    Impact,
    Catch,
    Roll,
    Focus,
    Cross,
    Victory,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Effect {
    pub kind: EffectKind,
    pub position: Vec2,
    pub remaining: f32,
    pub duration: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cue {
    Shielded,
    ReturnArrow,
    Caught,
    FocusEmpty,
}

#[derive(Clone, Debug)]
pub(super) struct Arena {
    pub phase: Phase,
    pub player: Player,
    pub boss: Boss,
    pub arrow: Arrow,
    pub projectiles: Vec<Projectile>,
    pub effects: Vec<Effect>,
    pub elapsed: f32,
    pub focus_remaining: f32,
    pub cue: Option<Cue>,
    cue_remaining: f32,
}

impl Arena {
    pub fn new(guardian: Guardian) -> Self {
        let position = match guardian {
            Guardian::Prism => Vec2::new(68.0, 17.0),
            _ => Vec2::new(48.0, 18.0),
        };
        Self {
            phase: Phase::Battle,
            player: Player {
                position: Vec2::new(48.0, 43.0),
                facing: Vec2::new(0.0, -1.0),
                health: 3,
                focus: 100.0,
                charge: 0.0,
                invulnerable: 0.0,
                roll_remaining: 0.0,
                roll_cooldown: 0.0,
                roll_direction: Vec2::default(),
            },
            boss: Boss {
                guardian,
                position,
                core: position,
                exposed: 0.0,
                windup: None,
                charging: false,
                charge_direction: Vec2::new(0.0, 1.0),
                attacks: 0,
                rest: 1.4,
            },
            arrow: Arrow::default(),
            projectiles: Vec::with_capacity(MAX_PROJECTILES),
            effects: Vec::with_capacity(24),
            elapsed: 0.0,
            focus_remaining: 0.0,
            cue: None,
            cue_remaining: 0.0,
        }
    }

    /// A fixed tick keeps collisions and attack windows independent of draw FPS.
    pub fn tick(&mut self, controls: Controls) {
        if self.phase != Phase::Battle {
            return;
        }
        self.elapsed += STEP;
        self.player.invulnerable = (self.player.invulnerable - STEP).max(0.0);
        self.player.roll_cooldown = (self.player.roll_cooldown - STEP).max(0.0);
        self.player.focus = (self.player.focus + STEP * 5.0).min(100.0);
        self.focus_remaining = (self.focus_remaining - STEP).max(0.0);
        self.cue_remaining -= STEP;
        if self.cue_remaining <= 0.0 {
            self.cue = None;
        }
        for effect in &mut self.effects {
            effect.remaining -= STEP;
        }
        self.effects.retain(|effect| effect.remaining > 0.0);

        let rolling = self.player.roll_remaining > 0.0;
        let recalling = !rolling
            && self.arrow.state != ArrowState::Ready
            && (controls.recall || controls.shoot);
        let charging = !rolling && controls.shoot && self.arrow.state == ArrowState::Ready;
        if rolling {
            self.player.position = self
                .player
                .position
                .plus(self.player.roll_direction.scale(53.0 * STEP))
                .clamped(3.0);
            self.player.roll_remaining = (self.player.roll_remaining - STEP).max(0.0);
        } else if charging {
            self.player.charge = (self.player.charge + STEP).min(1.0);
            self.player.facing = self.aim_direction(controls.aim);
        } else if !recalling && controls.movement.length() > 0.0 {
            let direction = controls.movement.normalized();
            self.player.position = self
                .player
                .position
                .plus(direction.scale(16.0 * STEP))
                .clamped(3.0);
            self.player.facing = direction;
        }

        // Focus slows threats, including their telegraphs, but never the player
        // or the returning arrow. It cannot damage or bypass a sealed core.
        let enemy_step = STEP
            * if self.focus_remaining > 0.0 {
                0.38
            } else {
                1.0
            };
        self.tick_boss(enemy_step);
        self.tick_projectiles(enemy_step);
        if self.phase != Phase::Battle {
            return;
        }
        self.tick_arrow(recalling);
        if self.phase == Phase::Battle
            && self.player.position.minus(self.boss.position).length()
                < self.boss.radius() + PLAYER_RADIUS - 0.7
        {
            self.hurt();
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
                position: self.player.position.plus(direction.scale(2.0)),
                velocity: direction.scale(66.0 + self.player.charge * 30.0),
            };
            self.player.facing = direction;
        }
        self.player.charge = 0.0;
    }

    pub fn cancel_input(&mut self) {
        self.player.charge = 0.0;
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
        self.player.roll_remaining = 0.23;
        self.player.roll_cooldown = 0.68;
        self.player.invulnerable = self.player.invulnerable.max(0.30);
        self.effect(EffectKind::Roll, self.player.position, 0.30);
    }

    pub fn focus(&mut self) {
        if self.phase != Phase::Battle || self.focus_remaining > 0.0 {
            return;
        }
        if self.player.focus < FOCUS_COST {
            self.show_cue(Cue::FocusEmpty);
            return;
        }
        self.player.focus -= FOCUS_COST;
        self.focus_remaining = 1.8;
        self.effect(EffectKind::Focus, self.player.position, 1.8);
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
                .scale(78.0);
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
        let core_hit = segment_distance(self.boss.core, from, to) <= CORE_RADIUS;
        if core_hit && self.boss.exposed > 0.0 {
            if returning || self.boss.guardian != Guardian::Prism {
                self.phase = Phase::Victory;
                self.arrow.position = self.boss.core;
                self.arrow.state = ArrowState::Lodged;
                self.projectiles.clear();
                self.boss.windup = None;
                self.effect(EffectKind::Victory, self.boss.core, 1.0);
                return;
            }
            self.show_cue(Cue::ReturnArrow);
        }

        if !returning
            && self.boss.exposed <= 0.0
            && self.boss.guardian != Guardian::Prism
            && segment_distance(self.boss.position, from, to) <= self.boss.radius()
        {
            self.arrow.state = ArrowState::Lodged;
            self.effect(EffectKind::Impact, from, 0.30);
            self.show_cue(Cue::Shielded);
            return;
        }
        self.arrow.position = to.clamped(1.5);
        if returning && segment_distance(self.player.position, from, to) < 1.8 {
            self.catch_arrow();
        } else if self.arrow.position != to {
            self.arrow.state = ArrowState::Lodged;
        }
    }

    fn catch_arrow(&mut self) {
        self.arrow.state = ArrowState::Ready;
        self.arrow.position = self.player.position;
        self.player.focus = (self.player.focus + 16.0).min(100.0);
        self.effect(EffectKind::Catch, self.player.position, 0.35);
        self.show_cue(Cue::Caught);
    }

    fn tick_boss(&mut self, dt: f32) {
        self.boss.exposed = (self.boss.exposed - dt).max(0.0);
        if self.boss.charging {
            let next = self
                .boss
                .position
                .plus(self.boss.charge_direction.scale(55.0 * dt));
            let clamped = next.clamped(self.boss.radius() + 1.0);
            self.boss.position = clamped;
            self.boss.core = clamped.minus(self.boss.charge_direction.scale(2.3));
            if next != clamped {
                self.boss.charging = false;
                self.boss.exposed = 2.1;
                self.boss.rest = 2.5;
                self.effect(EffectKind::Impact, clamped, 0.5);
            }
            return;
        }
        if let Some(windup) = &mut self.boss.windup {
            windup.remaining -= dt;
            if windup.remaining <= 0.0 {
                let windup = *windup;
                self.boss.windup = None;
                self.launch_attack(windup);
            }
            return;
        }
        self.boss.rest -= dt;
        if self.boss.rest > 0.0 {
            return;
        }

        let attack = match self.boss.guardian {
            Guardian::Ember if self.boss.attacks.is_multiple_of(2) => Attack::Sunburst,
            Guardian::Ember => Attack::Fan,
            Guardian::Knot => Attack::Charge,
            Guardian::Prism => Attack::Cross,
        };
        let duration = match attack {
            Attack::Sunburst | Attack::Cross => 1.1,
            Attack::Fan => 0.85,
            Attack::Charge => 1.0,
        };
        self.boss.windup = Some(Windup {
            attack,
            origin: self.boss.position,
            target: self.player.position,
            remaining: duration,
            duration,
        });
        self.boss.attacks += 1;
    }

    fn launch_attack(&mut self, windup: Windup) {
        match windup.attack {
            Attack::Sunburst => {
                let count = if self.elapsed > 25.0 { 16 } else { 12 };
                let offset = self.boss.attacks as f32 * 0.23;
                for ix in 0..count {
                    let angle = TAU * ix as f32 / count as f32 + offset;
                    self.projectile(windup.origin, Vec2::new(angle.cos(), angle.sin()), 15.0);
                }
                self.boss.exposed = 1.35;
                self.boss.rest = 2.0;
            }
            Attack::Fan => {
                let direction = windup.target.minus(windup.origin);
                let angle = direction.y.atan2(direction.x);
                for offset in [-0.36, -0.18, 0.0, 0.18, 0.36] {
                    self.projectile(
                        windup.origin,
                        Vec2::new((angle + offset).cos(), (angle + offset).sin()),
                        22.0,
                    );
                }
                self.boss.exposed = 1.55;
                self.boss.rest = 2.1;
            }
            Attack::Charge => {
                self.boss.charging = true;
                self.boss.charge_direction = windup.target.minus(windup.origin).normalized();
            }
            Attack::Cross => {
                self.effect(EffectKind::Cross, windup.target, 0.22);
                if (self.player.position.x - windup.target.x).abs() < 2.1
                    || (self.player.position.y - windup.target.y).abs() < 2.1
                {
                    self.hurt();
                }
                self.boss.position.x = WIDTH - self.boss.position.x;
                self.boss.core = self.boss.position;
                self.boss.exposed = 2.0;
                self.boss.rest = 2.65;
                for offset in [0.0, 0.25, 0.5, 0.75] {
                    let angle = TAU * offset;
                    self.projectile(
                        self.boss.position,
                        Vec2::new(angle.cos(), angle.sin()),
                        13.0,
                    );
                }
            }
        }
    }

    fn projectile(&mut self, position: Vec2, direction: Vec2, speed: f32) {
        if self.projectiles.len() < MAX_PROJECTILES {
            self.projectiles.push(Projectile {
                position: position.plus(direction.scale(self.boss.radius())),
                velocity: direction.scale(speed),
                remaining: 7.0,
            });
        }
    }

    fn tick_projectiles(&mut self, dt: f32) {
        let mut hit = false;
        for projectile in &mut self.projectiles {
            let from = projectile.position;
            projectile.position = from.plus(projectile.velocity.scale(dt));
            projectile.remaining -= dt;
            if segment_distance(self.player.position, from, projectile.position)
                < PLAYER_RADIUS + 0.55
            {
                hit = true;
                projectile.remaining = 0.0;
            }
        }
        self.projectiles.retain(|projectile| {
            projectile.remaining > 0.0
                && projectile.position.x > 1.0
                && projectile.position.x < WIDTH - 1.0
                && projectile.position.y > 1.0
                && projectile.position.y < HEIGHT - 1.0
        });
        if hit {
            self.hurt();
        }
    }

    fn hurt(&mut self) {
        if self.phase != Phase::Battle || self.player.invulnerable > 0.0 {
            return;
        }
        self.player.health = self.player.health.saturating_sub(1);
        self.player.invulnerable = 1.2;
        self.player.charge = 0.0;
        self.effect(EffectKind::Impact, self.player.position, 0.4);
        if self.player.health == 0 {
            self.phase = Phase::Defeat;
            self.cancel_input();
        }
    }

    fn effect(&mut self, kind: EffectKind, position: Vec2, duration: f32) {
        if self.effects.len() < 24 {
            self.effects.push(Effect {
                kind,
                position,
                remaining: duration,
                duration,
            });
        }
    }

    fn show_cue(&mut self, cue: Cue) {
        self.cue = Some(cue);
        self.cue_remaining = 1.5;
    }
}

#[cfg(test)]
#[path = "combat_tests.rs"]
mod tests;
