use super::*;

/// How fast Grok turns to keep its face to the archer, and how far behind it
/// the archer must get before it whips round instead.
const TURN_RATE: f32 = 1.3;
const BIG_TURN: f32 = 2.0;
/// Whips round it takes before it loses its temper.
const TURNS_TO_FRENZY: u8 = 3;
const FRENZY: f32 = 3.0;
const FRENZY_SPIN: f32 = 8.0;
/// The whirled blade's reach round the ring while it spins.
pub(in crate::arena) const FRENZY_RADIUS: f32 = 15.5;
pub(in crate::arena) const BLADE_RADIUS: f32 = 2.4;
/// How far the blade's swing bows out of the straight line.
const SWING: f32 = 12.0;

impl Arena {
    /// Grok keeps its face to the archer and its blade-thrown haul is its
    /// attack: the slash flies out on a swinging arc, lodges, and the ring
    /// yanks itself after it. Getting behind it makes it whip round to cover
    /// its back; after three such turns it spins in a frenzy, the blade
    /// whirled out round it, and its back flashes past the archer with every
    /// revolution until it reels.
    pub(super) fn tick_grok(&mut self) {
        let t = self.boss.progress();
        let hover = self.boss.hover_height(self.visual_time);
        self.boss.height += (hover - self.boss.height) * (STEP * 4.0).min(1.0);
        let toward = self.player.position.minus(self.boss.position);
        let desired = toward.angle() - PI * 0.5;
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) => {
                let behind = ((desired - self.boss.yaw + PI).rem_euclid(TAU) - PI).abs();
                if behind > BIG_TURN {
                    self.boss.dashes += 1;
                    self.boss.attack = Attack::Turn;
                    self.boss.enter(BossState::Striking, 0.35);
                    self.effect(EffectKind::Dust, self.boss.position, 0.4, 6.0);
                    return;
                }
                self.boss.turn_to(desired, STEP * TURN_RATE);
                // Keep a throwing distance.
                let range = (toward.length() - 32.0).clamp(-1.0, 1.0) * STEP * 10.0;
                let step = toward.normalized().scale(range);
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss.position.plus(step),
                    self.boss.radius(),
                );
                if self.boss.time >= self.boss.duration {
                    self.boss.begin(Attack::Throw, self.player.position, 0.5);
                }
            }
            (BossState::Striking, Attack::Turn) => {
                self.boss.turn_to(desired, STEP * 12.0);
                self.boss.spin += STEP * 14.0;
                if t >= 1.0 {
                    if self.boss.dashes >= TURNS_TO_FRENZY {
                        self.boss.dashes = 0;
                        self.boss.attack = Attack::Frenzy;
                        self.boss.attacks = self.boss.attacks.wrapping_add(1);
                        self.boss.exposed = FRENZY + 1.3;
                        self.boss.enter(BossState::Striking, FRENZY);
                    } else {
                        self.boss.enter(BossState::Watching, 0.45);
                    }
                }
            }
            (BossState::Windup, Attack::Throw) => {
                self.boss.turn_to(desired, STEP * 4.0);
                self.boss.spin += STEP * 10.0 * t;
                if !self.boss.target_locked() {
                    self.boss.target =
                        self.map
                            .move_body(self.boss.position, self.player.position, 1.0);
                }
                if t >= 1.0 {
                    self.boss.detached = true;
                    self.boss.from = self.boss.attack_origin();
                    self.boss.hands[0] = self.boss.from;
                    self.boss.hand_heights[0] = 6.0;
                    self.boss.enter(BossState::Striking, 0.55);
                }
            }
            (BossState::Striking, Attack::Throw) => {
                let blade = self.blade_at(t);
                self.boss.hands[0] = blade;
                self.boss.hand_heights[0] = 6.0 * (1.0 - t);
                self.boss.spin += STEP * 22.0;
                if self.player.position.minus(blade).length() < BLADE_RADIUS + PLAYER_RADIUS {
                    self.hurt();
                }
                if t >= 1.0 {
                    self.effect(EffectKind::Impact, blade, 0.5, 4.0);
                    self.boss.attack = Attack::Pull;
                    self.boss.from = self.boss.position;
                    self.boss.enter(BossState::Rushing, 0.42);
                }
            }
            (BossState::Rushing, Attack::Pull) => {
                // Hauled along the line to the lodged blade.
                let reach = self.boss.target.minus(self.boss.from);
                let stop = self.boss.target.minus(reach.normalized().scale(5.0));
                let goal = self.boss.from.lerp(stop, smoothstep(t));
                self.boss.position =
                    self.map
                        .move_body(self.boss.position, goal, self.boss.radius());
                self.boss.bank = (t * PI).sin() * 0.25;
                if t >= 1.0 {
                    self.boss.detached = false;
                    self.boss.bank = 0.0;
                    self.effect(EffectKind::Dust, self.boss.position, 0.5, 6.0);
                    self.boss.enter(BossState::Watching, 0.8);
                }
            }
            (BossState::Striking, Attack::Frenzy) => {
                self.boss.yaw += STEP * FRENZY_SPIN;
                self.boss.spin += STEP * 16.0;
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss
                        .position
                        .plus(toward.normalized().scale(STEP * 7.0)),
                    self.boss.radius(),
                );
                if toward.length() < FRENZY_RADIUS + PLAYER_RADIUS {
                    self.hurt();
                }
                if t >= 1.0 {
                    self.boss.yaw = (self.boss.yaw + PI).rem_euclid(TAU) - PI;
                    self.boss.enter(BossState::Recovery, 1.3);
                }
            }
            (BossState::Recovery, _) => {
                // Reeling: it rocks on its edge before it gathers itself.
                self.boss.bank = (self.boss.time * 9.0).sin() * 0.2 * (1.0 - t);
                if t >= 1.0 {
                    self.boss.bank = 0.0;
                    self.boss.enter(BossState::Watching, 0.6);
                }
            }
            _ => {}
        }
        self.boss.direction = Vec2::from_angle(self.boss.yaw + PI * 0.5);
    }

    /// Where the hurled blade is `t` of the way along its swinging arc.
    pub(in crate::arena) fn blade_at(&self, t: f32) -> Vec2 {
        let (from, to) = (self.boss.from, self.boss.target);
        let bow = to.minus(from).normalized().perpendicular().scale(SWING);
        let control = from.lerp(to, 0.5).plus(bow);
        let a = from.lerp(control, t);
        let b = control.lerp(to, t);
        a.lerp(b, t)
    }
}
