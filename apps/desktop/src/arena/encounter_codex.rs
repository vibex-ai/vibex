use super::*;

impl Arena {
    pub(super) fn tick_codex(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP * 1.6);
                self.boss.spin += STEP * 0.3;
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target();
                self.boss.spin -= STEP * 0.75;
                if t >= 1.0 {
                    let rushing = self.boss.attack == Attack::Rush;
                    self.boss.enter(
                        if rushing {
                            BossState::Rushing
                        } else {
                            BossState::Striking
                        },
                        if rushing { 3.5 } else { 1.1 },
                    );
                }
            }
            BossState::Rushing => {
                let before = self.boss.position;
                let next = before.plus(self.boss.direction.scale(51.0 * STEP));
                self.boss.spin += STEP * 7.4;
                let pillar = self.map.pillar_hit(before, next, self.boss.radius());
                if pillar.is_some() || !self.map.inside(next, self.boss.radius() + 0.5) {
                    if let Some(ix) = pillar {
                        self.map.broken |= 1 << ix;
                        let p = self.map.pillars()[ix].position;
                        self.effect(EffectKind::Rubble, p, 1.4, 12.0);
                        self.map.scar(p, 8.0, ScarKind::Crack);
                    }
                    self.boss.position = self.map.move_body(
                        before,
                        before.minus(self.boss.direction.scale(1.0)),
                        self.boss.radius(),
                    );
                    self.impact(self.boss.position, 6.0);
                    self.boss.strain = 1.0;
                    self.boss.opening(3.8);
                    self.effect(EffectKind::Overload, self.boss.core, 0.6, 8.0);
                } else if t >= 1.0 {
                    // A timeout is merely recovery. Only an actual collision
                    // can unravel the knot, including the indestructible rim.
                    self.recover(0.6);
                } else {
                    self.boss.position = next;
                    if ((self.boss.time / STEP) as u32).is_multiple_of(7) {
                        self.effect(EffectKind::Roll, before, 0.6, 4.0);
                    }
                }
            }
            BossState::Striking => {
                self.boss.spin += STEP * 1.2;
                if self.boss.fired == 0 {
                    self.boss.fired = 1;
                    match self.boss.attack {
                        Attack::Pulse => {
                            self.wave(self.boss.pulse_origin(), 10.0, 56.0, WaveKind::Stone)
                        }
                        Attack::Spokes => {
                            for (from, end) in self.spokes() {
                                self.hazard(
                                    HazardKind::Beam,
                                    from,
                                    end,
                                    0.45,
                                    0.32,
                                    self.boss.beam_radius(),
                                );
                            }
                        }
                        Attack::Barrage => {
                            for ix in 0..6 {
                                let angle = self.boss.yaw + ix as f32 * TAU / 6.0;
                                let direction = Vec2::from_angle(angle);
                                self.projectile(
                                    self.boss.core.plus(direction.scale(11.0)),
                                    Vec2::from_angle(angle + 0.28).scale(22.0),
                                    ProjectileKind::Segment,
                                );
                            }
                        }
                        _ => {}
                    }
                }
                if t >= 1.0 {
                    self.recover(0.55);
                }
            }
            BossState::Recovery => {
                self.boss.strain = (self.boss.exposed / 0.7).clamp(0.0, 1.0);
                self.boss.spin += STEP * (1.0 - smoothstep(self.boss.time / 0.4));
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.6);
                }
            }
            _ => {}
        }
    }
}
