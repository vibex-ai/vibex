use super::*;

const DRIFT: f32 = 8.0;
/// Slams before it spins.
const SLAMS: u8 = 2;
const SPIN: f32 = 3.6;
pub(in crate::arena) const SPIN_RATE: f32 = 2.4;
/// The beat of its hops while it spins, and the motes each hop flings.
const HOP: f32 = 0.6;
const SPRAY: u32 = 6;
const MOTE_SPEED: f32 = 16.0;

impl Arena {
    /// Antigravity hovers with a leg turned to the archer, so the heart
    /// hanging inside its arch stays hidden. It leaps and slams twice, each
    /// landing flinging weightless motes toward the four corners; then it
    /// floats in place and spins, hopping, sprinkling motes in a spiral. The
    /// heart takes an arrow only through the arch's opening, which sweeps past
    /// the archer twice a turn while it spins and for a moment after.
    pub(super) fn tick_antigravity(&mut self) {
        let t = self.boss.progress();
        let toward = self.player.position.minus(self.boss.position);
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) => {
                let hover = self.boss.hover_height(self.visual_time);
                self.boss.height += (hover - self.boss.height) * (STEP * 4.0).min(1.0);
                // A leg to the archer: the opening faces across their line.
                self.boss.turn_to(toward.angle(), STEP * 2.5);
                let range = (toward.length() - 30.0).clamp(-1.0, 1.0) * STEP * DRIFT;
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss.position.plus(toward.normalized().scale(range)),
                    self.boss.radius(),
                );
                self.boss.pitch *= 0.95;
                if self.boss.time >= self.boss.duration {
                    if self.boss.dashes < SLAMS {
                        self.boss.dashes += 1;
                        self.boss.begin(Attack::Leap, self.player.position, 0.7);
                    } else {
                        self.boss.dashes = 0;
                        self.boss.attack = Attack::Frenzy;
                        self.boss.attacks = self.boss.attacks.wrapping_add(1);
                        self.boss.exposed = SPIN + 0.8;
                        self.boss.enter(BossState::Striking, SPIN);
                    }
                }
            }
            (BossState::Windup, Attack::Leap) => {
                self.follow_target(80.0);
                self.boss.turn_to(toward.angle(), STEP * 2.5);
                self.boss.height += (9.0 - self.boss.height) * (STEP * 4.0).min(1.0);
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(BossState::Striking, 0.55);
                }
            }
            (BossState::Striking, Attack::Leap) => {
                if self.arc(14.0) {
                    let at = self.boss.target;
                    self.impact(at, self.boss.impact_radius());
                    for (x, y) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
                        self.projectile(
                            at,
                            Vec2::new(x, y).normalized().scale(MOTE_SPEED),
                            ProjectileKind::Mote,
                        );
                    }
                    self.boss.enter(BossState::Recovery, 0.5);
                }
            }
            (BossState::Striking, Attack::Frenzy) => {
                self.boss.yaw += STEP * SPIN_RATE;
                self.boss.direction = Vec2::from_angle(self.boss.yaw + PI * 0.5);
                let (hop, u) = (
                    (self.boss.time / HOP) as u32,
                    (self.boss.time / HOP).fract(),
                );
                self.boss.height = 4.0 + (u * PI).sin() * 4.0;
                if hop + 1 > self.boss.fired {
                    self.boss.fired = hop + 1;
                    let start = self.boss.time * 1.3;
                    for n in 0..SPRAY {
                        let a = start + n as f32 * TAU / SPRAY as f32;
                        self.projectile(
                            self.boss.position,
                            Vec2::from_angle(a).scale(MOTE_SPEED),
                            ProjectileKind::Mote,
                        );
                    }
                }
                if t >= 1.0 {
                    self.boss.enter(BossState::Recovery, 0.8);
                }
            }
            (BossState::Recovery, _) => {
                let hover = self.boss.hover_height(self.visual_time);
                self.boss.height += (hover - self.boss.height) * (STEP * 4.0).min(1.0);
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.9);
                }
            }
            _ => {}
        }
    }
}
