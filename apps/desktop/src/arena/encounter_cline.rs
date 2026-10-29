use super::*;
use crate::arena::map::STATIONS;

const BOLT_SPEED: f32 = 58.0;

fn station(ix: usize) -> Vec2 {
    STATIONS[ix % STATIONS.len()]
}

impl Arena {
    /// Cline glides clockwise round four stations and fires a fan of bolts
    /// from its eyes at each. It always turns its face to the archer, and its
    /// whole back is the weak point: an arrow loosed past it and recalled
    /// through it from behind wins.
    pub(super) fn tick_cline(&mut self) {
        let t = self.boss.progress();
        // The back is never closed; the head simply never shows it.
        self.boss.exposed = 1.0;
        let hover = self.boss.hover_height(self.visual_time);
        self.boss.height += (hover - self.boss.height) * (STEP * 5.0).min(1.0);
        self.boss.face(self.player.position, STEP * 5.0);
        self.boss.pitch *= 0.9;
        match self.boss.state {
            BossState::Watching if self.boss.time >= self.boss.duration => {
                // On to the station after the nearest one, clockwise.
                let nearest = (0..STATIONS.len())
                    .min_by(|a, b| {
                        let d = |ix: usize| station(ix).minus(self.boss.position).length();
                        d(*a).total_cmp(&d(*b))
                    })
                    .unwrap();
                self.boss.guard = (nearest + 1) % STATIONS.len();
                self.boss.attack = Attack::Glide;
                self.boss.from = self.boss.position;
                self.boss.target = station(self.boss.guard);
                let distance = self.boss.target.minus(self.boss.from).length();
                self.boss
                    .enter(BossState::Rushing, (distance / 48.0).clamp(0.7, 1.4));
            }
            BossState::Rushing => {
                let goal = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.position =
                    self.map
                        .move_body(self.boss.position, goal, self.boss.radius());
                if t >= 1.0 {
                    self.boss.begin(Attack::Fan, self.player.position, 0.5);
                }
            }
            BossState::Windup => {
                if !self.boss.target_locked() {
                    self.boss.target = self.player.position;
                }
                if t >= 1.0 {
                    self.cline_fan();
                    self.boss.enter(BossState::Striking, 0.45);
                }
            }
            BossState::Striking => {
                // The recoil of the volley.
                self.boss.pitch = (t * PI).sin() * 0.2;
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.35);
                }
            }
            _ => {}
        }
    }

    fn cline_fan(&mut self) {
        let origin = self.boss.attack_origin();
        let aim = self.boss.target.minus(origin).angle();
        for offset in self.boss.fan() {
            let angle = aim + offset;
            self.projectile(
                origin,
                Vec2::from_angle(angle).scale(BOLT_SPEED),
                ProjectileKind::Bolt,
            );
        }
        self.effect(EffectKind::Spark, origin, 0.35, 5.0);
    }
}
