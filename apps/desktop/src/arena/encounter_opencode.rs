use super::*;

impl Arena {
    pub(super) fn tick_opencode(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP * 1.5);
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target();
                if self.boss.attack == Attack::Stomp && t < 0.55 {
                    let reach = self.boss.target.minus(self.boss.position);
                    if reach.length() > 24.0 {
                        self.boss.target = self.map.move_body(
                            self.boss.position,
                            self.boss.position.plus(reach.normalized().scale(24.0)),
                            self.boss.radius(),
                        );
                    }
                }
                if self.boss.attack == Attack::Sweep {
                    self.beam_tell();
                }
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(
                        BossState::Striking,
                        match self.boss.attack {
                            Attack::Inhale => 3.0,
                            Attack::Stomp => 0.8,
                            _ => 1.05,
                        },
                    );
                }
            }
            BossState::Striking => match self.boss.attack {
                Attack::Sweep => {
                    self.beam_strike();
                    if t >= 1.0 {
                        self.recover(0.6);
                    }
                }
                Attack::Inhale => {
                    if self.boss.fired == 0 {
                        self.boss.fired = 1;
                        self.hazard(
                            HazardKind::Gravity,
                            self.boss.position,
                            self.boss.position,
                            0.0,
                            self.boss.duration,
                            38.0,
                        );
                    }
                    if t >= 1.0 {
                        self.recover(0.3);
                    }
                }
                Attack::Stomp => {
                    self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                    self.boss.pitch = -t * TAU;
                    self.boss.height = Boss::rolling_height(self.boss.pitch);
                    if t >= 1.0 {
                        self.boss.pitch = 0.0;
                        self.boss.height = 0.0;
                        self.impact(self.boss.position, self.boss.impact_radius());
                        self.wave(self.boss.position, 9.0, 32.0, WaveKind::Stone);
                        self.recover(0.7);
                    }
                }
                _ => {
                    let volley = (t * 3.0) as u32 + 1;
                    if volley > self.boss.fired && volley <= 3 {
                        self.boss.fired = volley;
                        for side in [-1.0, 1.0] {
                            let origin = self.boss.local(side * 11.0, 2.0, 9.0);
                            let angle = self.boss.target.minus(origin).angle() + side * 0.12;
                            self.projectile(
                                origin,
                                Vec2::from_angle(angle).scale(25.0),
                                ProjectileKind::Block,
                            );
                        }
                    }
                    if t >= 1.0 {
                        self.recover(0.45);
                    }
                }
            },
            BossState::Recovery if t >= 1.0 => {
                self.boss.tether = 0.0;
                self.boss.enter(BossState::Watching, 0.55);
            }
            _ => {}
        }
    }
}
