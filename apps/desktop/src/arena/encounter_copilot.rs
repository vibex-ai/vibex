use super::*;

impl Arena {
    pub(super) fn tick_copilot(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.height = self.boss.from_height
                    + (self.boss.hover_height(self.visual_time) - self.boss.from_height)
                        * smoothstep(t);
                let tangent = self
                    .player
                    .position
                    .minus(self.boss.position)
                    .normalized()
                    .perpendicular();
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss.position.plus(tangent.scale(STEP * 5.0)),
                    self.boss.radius(),
                );
                self.boss.face(self.player.position, STEP * 2.0);
                self.boss.bank *= 0.9;
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target();
                if self.boss.attack == Attack::VisorBeam {
                    self.beam_tell();
                }
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    let dash = self.boss.attack == Attack::Dash;
                    self.boss.enter(
                        if dash {
                            BossState::Rushing
                        } else {
                            BossState::Striking
                        },
                        if dash {
                            0.9
                        } else if self.boss.attack == Attack::Volley {
                            1.65
                        } else {
                            0.75
                        },
                    );
                }
            }
            BossState::Striking if self.boss.attack == Attack::Volley => {
                if self.boss.fired == 0 {
                    self.boss.fired = 1;
                    self.projectiles
                        .retain(|p| p.kind != ProjectileKind::ChargedOrb);
                    let point = self.boss.core.plus(self.boss.direction.scale(13.0));
                    self.projectile(point, Vec2::default(), ProjectileKind::ChargedOrb);
                    self.effect(EffectKind::Cross, point, 0.65, 5.0);
                }
                let volley = (self.boss.time / 0.55) as u32 + 1;
                if volley > self.boss.fired && volley <= 3 {
                    self.boss.fired = volley;
                    for side in [-1.0, 1.0] {
                        let origin = self.boss.local(side * 8.0, 3.0, 9.0);
                        self.projectile(
                            origin,
                            self.boss.target.minus(origin).normalized().scale(23.0),
                            ProjectileKind::Orb,
                        );
                    }
                }
                if t >= 1.0 {
                    self.recover(0.45);
                }
            }
            BossState::Striking => {
                self.beam_strike();
                if t >= 1.0 {
                    self.recover(0.55);
                }
            }
            BossState::Rushing => {
                self.boss.position = self.boss.from.lerp(self.boss.target, t * t);
                self.boss.height = self.boss.from_height * (1.0 - t.powi(3)) + (t * PI).sin() * 5.0;
                self.boss.bank = (t * PI).sin() * 0.14;
                if t >= 1.0 {
                    self.boss.height = 0.0;
                    self.impact(self.boss.position, self.boss.impact_radius());
                    self.recover(0.65);
                }
            }
            BossState::Recovery => {
                if self.boss.exposed > 0.0 {
                    self.boss.height = self.boss.from_height
                        + (1.5 - self.boss.from_height)
                            * smoothstep((self.boss.time / 0.7).min(1.0));
                }
                self.boss.bank *= 0.92;
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.7);
                }
            }
            _ => {}
        }
    }
}
