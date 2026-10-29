use super::*;

const WADDLE: f32 = 10.0;
const HAIRBALLS: u32 = 6;
pub(in crate::arena) const BUG_RADIUS: f32 = 9.0;
/// How long the spat bug ticks before it bursts.
const FUSE: f32 = 2.6;
/// How long a cat caught in its own bug's burst lies dazed, gem bare.
const DAZE: f32 = 2.6;
const YARN_SPEED: f32 = 40.0;

impl Arena {
    /// CodeBuddy waddles after the archer. It coughs up hairballs that rain
    /// down round the archer and kill where they land; each lies a moment,
    /// then rolls back to the cat, lethal on the way. Then it inhales and
    /// spits a bug bomb at the archer's feet and keeps waddling after them:
    /// lead it over its own bug before the fuse runs out, and the burst
    /// knocks it dazed with the gem on its chest bare.
    pub(super) fn tick_codebuddy(&mut self) {
        let t = self.boss.progress();
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) => {
                self.boss.face(self.player.position, STEP * 4.0);
                let toward = self.player.position.minus(self.boss.position).normalized();
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss.position.plus(toward.scale(WADDLE * STEP)),
                    self.boss.radius(),
                );
                // A waddle: it rocks from foot to foot as it goes.
                self.boss.bank = (self.boss.time * 9.0).sin() * 0.08;
                self.boss.height = (self.boss.time * 18.0).sin().abs() * 0.8;
                if self.boss.time >= self.boss.duration {
                    if self.boss.attacks.is_multiple_of(2) {
                        self.boss.begin(Attack::Rain, self.player.position, 0.8);
                    } else {
                        self.boss.begin(Attack::Bomb, self.player.position, 1.0);
                    }
                }
            }
            (BossState::Windup, Attack::Rain) => {
                // Hunched over, heaving.
                self.boss.squash = 0.5 * smoothstep(t);
                self.boss.bank = 0.0;
                if t >= 1.0 {
                    for n in 0..HAIRBALLS {
                        let a = (self.random() % 628) as f32 * 0.01;
                        let r = 4.0 + (self.random() % 120) as f32 * 0.1;
                        let spot = self.map.move_body(
                            self.player.position,
                            self.player.position.plus(Vec2::from_angle(a).scale(r)),
                            2.0,
                        );
                        self.hazard(HazardKind::Hairball, spot, 0.6 + n as f32 * 0.12, 0.1, 3.0);
                    }
                    self.boss.squash = -0.3;
                    self.effect(EffectKind::Dust, self.boss.attack_origin(), 0.5, 4.0);
                    self.boss.enter(BossState::Striking, 0.6);
                }
            }
            (BossState::Windup, Attack::Bomb) => {
                // A long breath in, head thrown back.
                self.boss.pitch = 0.2 * smoothstep(t);
                if !self.boss.target_locked() {
                    self.boss.target =
                        self.map
                            .move_body(self.boss.position, self.player.position, 1.0);
                }
                if t >= 1.0 {
                    self.boss.pitch = -0.15;
                    self.hazard(HazardKind::Bug, self.boss.target, FUSE, 0.15, BUG_RADIUS);
                    self.effect(EffectKind::Spark, self.boss.attack_origin(), 0.4, 4.0);
                    self.boss.enter(BossState::Striking, 0.4);
                }
            }
            (BossState::Striking, Attack::Rain) if t >= 1.0 => {
                self.boss.enter(BossState::Watching, 1.6);
            }
            (BossState::Striking, Attack::Bomb) if t >= 1.0 => {
                // It keeps after the archer while its bug ticks.
                self.boss.enter(BossState::Watching, FUSE + 1.2);
            }
            (BossState::Recovery, _) => {
                // Dazed: it sways on its seat, stars round its head.
                self.boss.yaw += (self.boss.time * 6.0).sin() * STEP * 2.0;
                self.boss.bank = (self.boss.time * 5.0).sin() * 0.15 * (1.0 - t);
                if t >= 1.0 {
                    self.boss.bank = 0.0;
                    self.boss.enter(BossState::Watching, 1.0);
                }
            }
            _ => {}
        }
        self.boss.pitch *= 0.95;
    }

    /// A hairball that has lain long enough rolls back to the cat.
    pub(in crate::arena) fn yarn_home(&mut self, from: Vec2) {
        let way = self.boss.position.minus(from);
        let length = way.length();
        if self.phase != Phase::Battle || length < 3.0 || self.projectiles.len() >= MAX_PROJECTILES
        {
            return;
        }
        self.projectiles.push(Projectile {
            position: from,
            previous_position: from,
            velocity: way.normalized().scale(YARN_SPEED),
            remaining: length / YARN_SPEED,
            kind: ProjectileKind::Yarn,
            age: 0.0,
        });
    }

    /// The bug bursts: a cat caught in it is knocked dazed, its gem bare.
    pub(in crate::arena) fn bug_burst(&mut self, at: Vec2, radius: f32) {
        self.effect(EffectKind::Burst, at, 0.6, radius);
        self.effect(EffectKind::Impact, at, 0.6, radius);
        self.hit_pause = self.hit_pause.max(0.04);
        if self.boss.guardian == Guardian::CodeBuddy
            && self.boss.position.minus(at).length() < radius + self.boss.radius()
        {
            self.boss.exposed = DAZE;
            self.boss.squash = 0.8;
            self.boss.enter(BossState::Recovery, DAZE);
            self.effect(EffectKind::Spark, self.boss.body_center(), 0.8, 8.0);
        }
    }
}
