use super::*;
use crate::arena::guardian::{Relic, RelicState};

const RELIC_SPEED: f32 = 36.0;
pub(in crate::arena) const RELIC_RADIUS: f32 = 2.6;
const QUILL_SPEED: f32 = 85.0;
/// Quills she looses while the caduceus flies, before she goes after it.
const QUILLS: u8 = 3;
const FETCH_SPEED: f32 = 34.0;
/// How long she reels when her caduceus is struck down, and how long the
/// jolt leaves the archer reeling.
const DAZE: f32 = 2.8;
const STUN: f32 = 0.9;

impl Arena {
    /// Hermes floats over her hall and sends her caduceus ricocheting round
    /// it, off walls and columns, lethal to touch. While it flies she looses
    /// three quick quills at the archer, then goes after it and catches it.
    /// An arrow that strikes the flying caduceus brings it down: the jolt
    /// leaves the archer reeling for a moment, but leaves her dazed for
    /// longer, the gem in her circlet bare — time to recover, recall the
    /// arrow and loose it at her.
    pub(super) fn tick_hermes(&mut self) {
        let t = self.boss.progress();
        let hover = self.boss.hover_height(self.visual_time);
        self.boss.height += (hover - self.boss.height) * (STEP * 4.0).min(1.0);
        self.tick_relic();
        let toward = self.player.position.minus(self.boss.position);
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) => {
                self.boss.face(self.player.position, STEP * 4.0);
                let range = (toward.length() - 34.0).clamp(-1.0, 1.0) * STEP * 10.0;
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss.position.plus(toward.normalized().scale(range)),
                    self.boss.radius(),
                );
                if self.boss.time < self.boss.duration {
                    return;
                }
                match self.boss.relic.state {
                    RelicState::Held => {
                        // Off it goes, angled away from the straight line.
                        let aim = toward.angle() + 0.55;
                        let from = self.map.move_body(
                            self.boss.position,
                            self.boss.position.plus(Vec2::from_angle(aim).scale(6.0)),
                            RELIC_RADIUS + 0.5,
                        );
                        self.boss.relic = Relic {
                            position: from,
                            velocity: Vec2::from_angle(aim).scale(RELIC_SPEED),
                            state: RelicState::Flying,
                        };
                        self.boss.dashes = QUILLS;
                        self.boss.enter(BossState::Watching, 0.8);
                    }
                    _ if self.boss.dashes > 0 => {
                        self.boss.begin(Attack::Volley, self.player.position, 0.55);
                    }
                    _ => {
                        self.boss.attack = Attack::Glide;
                        self.boss.enter(BossState::Rushing, 8.0);
                    }
                }
            }
            (BossState::Windup, Attack::Volley) => {
                self.boss.face(self.player.position, STEP * 6.0);
                if t >= 1.0 {
                    let origin = self.boss.attack_origin();
                    let aim = self.player.position.minus(origin).normalized();
                    self.projectile(origin, aim.scale(QUILL_SPEED), ProjectileKind::Quill);
                    self.boss.dashes = self.boss.dashes.saturating_sub(1);
                    self.boss.enter(BossState::Striking, 0.3);
                }
            }
            (BossState::Striking, Attack::Volley) if t >= 1.0 => {
                self.boss.enter(BossState::Watching, 0.9);
            }
            (BossState::Rushing, Attack::Glide) => {
                // After the caduceus, wherever it is, to catch it.
                let relic = self.boss.relic.position;
                let way = relic.minus(self.boss.position);
                self.boss.face(relic, STEP * 6.0);
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss
                        .position
                        .plus(way.normalized().scale(FETCH_SPEED * STEP)),
                    self.boss.radius(),
                );
                if way.length() < 5.0 || t >= 1.0 {
                    self.boss.relic.state = RelicState::Held;
                    self.effect(EffectKind::Catch, relic, 0.4, 4.0);
                    self.boss.enter(BossState::Watching, 0.6);
                }
            }
            (BossState::Recovery, _) => {
                // Dazed, swaying in the air.
                self.boss.bank = (self.boss.time * 5.0).sin() * 0.2 * (1.0 - t);
                self.boss.height -= STEP * 2.0 * (1.0 - t);
                if t >= 1.0 {
                    self.boss.bank = 0.0;
                    self.boss.dashes = 0;
                    self.boss.enter(BossState::Watching, 0.5);
                }
            }
            _ => {}
        }
    }

    /// The caduceus in flight: it ricochets off the rim and the columns, and
    /// kills on touch.
    fn tick_relic(&mut self) {
        match self.boss.relic.state {
            RelicState::Held => {
                self.boss.relic.position = self.boss.position;
                return;
            }
            RelicState::Fallen => return,
            RelicState::Flying => {}
        }
        let mut from = self.boss.relic.position;
        if !self.map.inside(from, RELIC_RADIUS) {
            // Never lost outside the hall: back in toward the middle.
            from = self.map.move_body(CENTER, from, RELIC_RADIUS + 0.5);
        }
        let mut velocity = self.boss.relic.velocity;
        let step = velocity.scale(STEP);
        if !self
            .map
            .inside(Vec2::new(from.x + step.x, from.y), RELIC_RADIUS)
        {
            velocity.x = -velocity.x;
        }
        if !self
            .map
            .inside(Vec2::new(from.x, from.y + step.y), RELIC_RADIUS)
        {
            velocity.y = -velocity.y;
        }
        if let Some(ix) = self
            .map
            .pillar_hit(from, from.plus(velocity.scale(STEP)), RELIC_RADIUS)
        {
            let normal = from.minus(self.map.pillars()[ix].position).normalized();
            velocity = velocity.minus(normal.scale(2.0 * velocity.dot(normal)));
        }
        let to = from.plus(velocity.scale(STEP));
        self.boss.relic.velocity = velocity;
        self.boss.relic.position = to;
        if segment_distance(self.player.position, from, to) < RELIC_RADIUS + PLAYER_RADIUS {
            self.hurt();
        }
    }

    /// An arrow strikes the flying caduceus down: the archer reels from the
    /// jolt, Hermes for longer, her gem bare.
    pub(in crate::arena) fn strike_relic(&mut self) {
        let at = self.boss.relic.position;
        self.boss.relic.state = RelicState::Fallen;
        self.boss.relic.velocity = Vec2::default();
        self.player.stunned = STUN;
        self.player.charge = 0.0;
        self.boss.attack = Attack::Open;
        self.boss.exposed = DAZE;
        self.boss.dashes = 0;
        self.boss.enter(BossState::Recovery, DAZE);
        self.effect(EffectKind::Flash, at, 0.4, 10.0);
        self.effect(EffectKind::Spark, self.player.position, 0.8, 5.0);
    }
}
