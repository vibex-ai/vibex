use super::*;

/// How close the archer must come before the true twin breaks away.
const SPLIT_RANGE: f32 = 24.0;
const DART: f32 = 0.5;
/// How long the true twin stays apart, bare, before it glides home.
const APART: f32 = 2.6;
const REJOIN: f32 = 1.0;
const BOLT_SPEED: f32 = 75.0;
const SPARK_SPEED: f32 = 14.0;

impl Arena {
    /// Gemini is two stars as one: a pale white twin that nothing harms, and
    /// the true twin in the logo's colours in front of it. Together they
    /// drift and loose star-bolts that lead the archer's movement, with sparks
    /// thrown out to the four diagonals. Come close and the true twin breaks
    /// away, darting off to a spot of its own: it is bare until it glides home.
    pub(super) fn tick_gemini(&mut self) {
        let t = self.boss.progress();
        let hover = self.boss.hover_height(self.visual_time);
        self.boss.height += (hover - self.boss.height) * (STEP * 4.0).min(1.0);
        self.boss.spin += STEP * 0.6;
        let toward = self.player.position.minus(self.boss.position);
        // A star is an emblem: it keeps its face to the viewer, swaying a
        // little toward the archer's side.
        let sway = (toward.x / 60.0).clamp(-1.0, 1.0) * 0.3;
        self.boss.turn_to(sway, STEP * 2.0);
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) => {
                let range = (toward.length() - 32.0).clamp(-1.0, 1.0) * STEP * 9.0;
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss.position.plus(toward.normalized().scale(range)),
                    self.boss.radius(),
                );
                if self.boss.attacks > 0 && toward.length() < SPLIT_RANGE {
                    self.gemini_split();
                } else if self.boss.time >= self.boss.duration {
                    self.boss.begin(Attack::Volley, self.player.position, 0.6);
                }
            }
            (BossState::Windup, Attack::Volley) => {
                if t >= 1.0 {
                    let origin = self.boss.attack_origin();
                    let aim = self.lead(origin, BOLT_SPEED);
                    self.projectile(origin, aim.scale(BOLT_SPEED), ProjectileKind::Bolt);
                    for (x, y) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
                        self.projectile(
                            origin,
                            Vec2::new(x, y).normalized().scale(SPARK_SPEED),
                            ProjectileKind::Mote,
                        );
                    }
                    self.effect(EffectKind::Flash, origin, 0.25, 5.0);
                    self.boss.enter(BossState::Striking, 0.3);
                }
            }
            (BossState::Striking, Attack::Volley) if t >= 1.0 => {
                self.boss.enter(BossState::Watching, 1.3);
            }
            (BossState::Rushing, Attack::Dash) => {
                let goal = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.hands[0] = goal;
                self.boss.hand_heights[0] = self.boss.height + (t * PI).sin() * 4.0;
                if t >= 1.0 {
                    self.boss.enter(BossState::Recovery, APART);
                }
            }
            (BossState::Recovery, Attack::Dash) => {
                self.boss.hand_heights[0] = self.boss.height + (self.boss.time * 5.0).sin() * 0.8;
                if t >= 1.0 {
                    // Home to the white twin.
                    self.boss.attack = Attack::Glide;
                    self.boss.from = self.boss.hands[0];
                    self.boss.enter(BossState::Rushing, REJOIN);
                }
            }
            (BossState::Rushing, Attack::Glide) => {
                self.boss.hands[0] = self.boss.from.lerp(self.boss.position, smoothstep(t));
                self.boss.hand_heights[0] += (self.boss.height - self.boss.hand_heights[0]) * t;
                if t >= 1.0 {
                    self.boss.detached = false;
                    self.boss.enter(BossState::Watching, 1.2);
                }
            }
            _ => {}
        }
    }

    /// The true twin breaks away to a spot of its own, clear of the archer.
    fn gemini_split(&mut self) {
        let mut spot = self.boss.position;
        for _ in 0..12 {
            let a = (self.random() % 628) as f32 * 0.01;
            let r = 22.0 + (self.random() % 200) as f32 * 0.1;
            let p = self.map.move_body(
                CENTER,
                CENTER.plus(Vec2::new(a.cos() * r * 1.4, a.sin() * r * 0.8)),
                3.0,
            );
            if p.minus(self.player.position).length() > 26.0 {
                spot = p;
                break;
            }
        }
        self.boss.detached = true;
        self.boss.attack = Attack::Dash;
        self.boss.attacks = self.boss.attacks.wrapping_add(1);
        self.boss.from = self.boss.position;
        self.boss.target = spot;
        self.boss.hands[0] = self.boss.position;
        self.boss.hand_heights[0] = self.boss.height;
        self.boss.exposed = DART + APART;
        self.boss.enter(BossState::Rushing, DART);
        self.effect(EffectKind::Spark, self.boss.body_center(), 0.5, 8.0);
    }

    /// Where to aim so a shot at `speed` meets the archer if they keep going
    /// as they are.
    fn lead(&self, origin: Vec2, speed: f32) -> Vec2 {
        let velocity = if self.player.moving {
            let pace = if self.player.sprinting {
                WALK_SPEED * SPRINT
            } else {
                WALK_SPEED
            };
            self.player.facing.scale(pace)
        } else {
            Vec2::default()
        };
        let mut aim = self.player.position;
        for _ in 0..3 {
            let time = aim.minus(origin).length() / speed;
            aim = self.player.position.plus(velocity.scale(time));
        }
        aim.minus(origin).normalized()
    }
}
