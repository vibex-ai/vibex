use super::*;
use crate::arena::guardian::DEVIN_VINE;

/// How fast the flower turns, sweeping its vines round.
const TURN: f32 = 0.9;
const INHALE: f32 = 2.4;
/// How long a swallowed arrow may sit before the flower spits it out.
const CHOKE: f32 = 5.0;
/// How long the heart stays bare once a petal is torn away.
const BARE: f32 = 3.5;
/// A vine regrows a segment this often.
const REGROW: f32 = 4.5;
const SPORES: u32 = 3;
const SPORE_SPEED: f32 = 18.0;

impl Arena {
    /// Devin is a flower rooted in the middle of its greenhouse. It turns,
    /// sweeping a vine of hexagons out from each petal along the floor —
    /// lethal to touch; an arrow knocks a segment off, and they regrow. It
    /// breathes out puffs of spores that drift after the archer. When it
    /// breathes in, its mouth opens: an arrow loosed into it is swallowed, and
    /// recalling it tears a petal away, baring the heart through the gap.
    pub(super) fn tick_devin(&mut self) {
        let t = self.boss.progress();
        self.boss.position = CENTER;
        self.boss.previous_position = CENTER;
        self.boss.height *= 0.9;
        self.boss.pitch *= 0.9;
        let breathing = self.boss.attack == Attack::Inhale && self.boss.state == BossState::Windup;
        self.boss.spin += STEP * if breathing { TURN * 0.4 } else { TURN };
        // The vines sweep the floor.
        let player = self.player.position;
        let touched = (0..3).any(|arm| {
            (0..self.boss.vines[arm] as usize).any(|k| {
                self.boss.vine_point(arm, k).minus(player).length()
                    < crate::arena::devin::SEGMENT_REACH + PLAYER_RADIUS
            })
        });
        if touched {
            self.hurt();
        }
        // A short vine grows back, one segment at a time.
        self.boss.shield += STEP;
        if self.boss.shield >= REGROW {
            self.boss.shield = 0.0;
            if let Some(arm) = (0..3)
                .filter(|arm| {
                    self.boss.torn & (1 << arm) == 0 && self.boss.vines[*arm] < DEVIN_VINE
                })
                .min_by_key(|arm| self.boss.vines[*arm])
            {
                self.boss.vines[arm] += 1;
            }
        }
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) if self.boss.time >= self.boss.duration => {
                // Spores first, then a breath in, by turns.
                if !self.boss.attacks.is_multiple_of(2) {
                    self.boss.attack = Attack::Inhale;
                    self.boss.attacks = self.boss.attacks.wrapping_add(1);
                    self.boss.enter(BossState::Windup, INHALE);
                } else {
                    self.boss.begin(Attack::Volley, self.player.position, 0.6);
                }
            }
            (BossState::Windup, Attack::Inhale) if t >= 1.0 => {
                // Nothing swallowed: it breathes the spores back out.
                self.devin_spores(SPORES + 2);
                self.boss.enter(BossState::Watching, 1.4);
            }
            (BossState::Windup, Attack::Volley) if t >= 1.0 => {
                self.devin_spores(SPORES);
                self.boss.enter(BossState::Watching, 1.8);
            }
            (BossState::Recovery, Attack::Inhale) if t >= 1.0 => {
                // Choking on an arrow nobody pulled: it spits it out.
                self.boss.swallowed = false;
                self.arrow.state = ArrowState::Lodged;
                self.arrow.position = self.map.move_body(
                    CENTER,
                    CENTER.plus(self.player.position.minus(CENTER).normalized().scale(14.0)),
                    1.0,
                );
                self.boss.enter(BossState::Watching, 1.0);
            }
            (BossState::Recovery, Attack::Open) if t >= 1.0 => {
                // The torn petal grows back, its vine a stub.
                for arm in 0..3 {
                    if self.boss.torn & (1 << arm) != 0 {
                        self.boss.vines[arm] = 1;
                    }
                }
                self.boss.torn = 0;
                self.boss.enter(BossState::Watching, 1.2);
            }
            _ => {}
        }
    }

    fn devin_spores(&mut self, count: u32) {
        let mouth = self.boss.body_center();
        let aim = self.player.position.minus(mouth).angle();
        for n in 0..count {
            let a = aim + (n as f32 - (count as f32 - 1.0) * 0.5) * 0.7;
            self.projectile(
                mouth,
                Vec2::from_angle(a).scale(SPORE_SPEED),
                ProjectileKind::Spore,
            );
        }
        self.effect(EffectKind::Steam, mouth, 0.6, 6.0);
    }

    /// An arrow into the open mouth: it sticks there, and the flower chokes.
    pub(in crate::arena) fn swallow(&mut self) {
        self.boss.swallowed = true;
        self.arrow.state = ArrowState::Lodged;
        self.arrow.position = self.boss.body_center();
        self.arrow.velocity = Vec2::default();
        self.boss.enter(BossState::Recovery, CHOKE);
        self.effect(EffectKind::Burst, self.boss.body_center(), 0.4, 4.0);
    }

    /// The swallowed arrow hauled back out tears away the petal nearest the
    /// archer, and the heart is bare through the gap.
    pub(in crate::arena) fn tear_petal(&mut self) {
        let toward = self.player.position.minus(self.boss.position).normalized();
        let arm = (0..3)
            .max_by(|a, b| {
                let along = |arm: usize| {
                    self.boss
                        .petal_point(arm)
                        .minus(self.boss.position)
                        .normalized()
                        .dot(toward)
                };
                along(*a).total_cmp(&along(*b))
            })
            .unwrap();
        self.boss.swallowed = false;
        // The arrow comes away with the petal, out through the gap it leaves.
        self.arrow.position = self.boss.petal_point(arm);
        self.boss.torn |= 1 << arm;
        self.boss.vines[arm] = 0;
        self.boss.attack = Attack::Open;
        self.boss.exposed = BARE;
        self.boss.enter(BossState::Recovery, BARE);
        self.effect(EffectKind::Burst, self.boss.petal_point(arm), 0.8, 8.0);
    }
}
