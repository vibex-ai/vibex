use super::*;

impl Arena {
    pub(super) fn tick_claude(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.height = self.boss.from_height
                    + (self.boss.hover_height(self.visual_time) - self.boss.from_height)
                        * smoothstep(t);
                self.boss.face(self.player.position, STEP * 1.4);
                let toward = self.player.position.minus(self.boss.position);
                if toward.length() > 24.0 {
                    self.boss.position = self.map.move_body(
                        self.boss.position,
                        self.boss
                            .position
                            .plus(toward.normalized().scale(STEP * 6.0)),
                        self.boss.radius(),
                    );
                }
                self.boss.stride += STEP * 3.0;
                self.boss.rest_tentacles();
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target();
                let reach = self.boss.target.minus(self.boss.position);
                if t < 0.55 && reach.length() > 32.0 {
                    self.boss.target = self.boss.position.plus(reach.normalized().scale(32.0));
                }
                self.boss.rest_tentacles();
                if self.boss.attack == Attack::TendrilSweep {
                    for ix in self.boss.ward as usize * 2..self.boss.ward as usize * 2 + 2 {
                        self.boss.tentacles[ix] = self
                            .boss
                            .resting_tentacle(ix)
                            .lerp(self.boss.tentacle_strike(ix, 0.0), smoothstep(t));
                        self.boss.tentacle_heights[ix] = 2.0 + smoothstep(t) * 13.0;
                    }
                } else if self.boss.attack == Attack::Rush {
                    for ix in 0..6 {
                        self.boss.tentacles[ix] = self.boss.tentacles[ix].lerp(
                            self.boss.position.minus(self.boss.direction.scale(12.0)),
                            t * 0.3,
                        );
                    }
                } else {
                    for ix in 0..6 {
                        self.boss.tentacle_heights[ix] =
                            self.boss.tentacle_rest_height(ix) + t * 4.0;
                    }
                }
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    let rushing = self.boss.attack == Attack::Rush;
                    self.boss.enter(
                        if rushing {
                            BossState::Rushing
                        } else {
                            BossState::Striking
                        },
                        if rushing {
                            1.1
                        } else if self.boss.attack == Attack::Inhale {
                            2.65
                        } else {
                            0.48
                        },
                    );
                }
            }
            BossState::Striking if self.boss.attack == Attack::TendrilSweep => {
                for ix in self.boss.ward as usize * 2..self.boss.ward as usize * 2 + 2 {
                    self.boss.tentacles[ix] = self.boss.tentacle_strike(ix, t);
                    self.boss.tentacle_heights[ix] = (1.0 - t.powi(3)) * 15.0;
                }
                if t >= 1.0 {
                    for ix in self.boss.ward as usize * 2..self.boss.ward as usize * 2 + 2 {
                        if self.boss.severed & (1 << ix) == 0 {
                            self.impact(self.boss.tentacles[ix], self.boss.impact_radius());
                        }
                    }
                    self.boss.enter(BossState::Recovery, 2.05);
                }
            }
            BossState::Striking => {
                // Venting lifts the shell clear of every intact tendon. Waiting
                // alone never opens the underside; the arrow must cut two.
                for ix in 0..6 {
                    let rest = self.boss.resting_tentacle(ix);
                    self.boss.tentacles[ix] = self
                        .boss
                        .position
                        .plus(rest.minus(self.boss.position).scale(1.35));
                    self.boss.tentacle_heights[ix] = 2.0 + (t * PI).sin() * 2.5;
                }
                let pulse = (self.boss.time / 0.4) as u32;
                if pulse > self.boss.fired {
                    self.boss.fired = pulse;
                    self.effect(EffectKind::Steam, self.boss.core, 0.75, 7.0);
                }
                if t >= 1.0 {
                    self.recover(0.55);
                }
            }
            BossState::Rushing => {
                let from = self.boss.position;
                let next = from.plus(self.boss.direction.scale(47.0 * STEP));
                let moved = self.map.move_body(from, next, self.boss.radius());
                self.boss.position = moved;
                self.boss.rest_tentacles();
                let marks = (self.boss.time / 0.10) as u32;
                if marks > self.boss.fired {
                    self.boss.fired = marks;
                    let end = from.minus(self.boss.direction.scale(7.0));
                    let start = end.minus(self.boss.direction.scale(5.0));
                    self.hazard(HazardKind::Scorch, start, end, 0.15, 3.4, 1.7);
                    self.map.scar(end, 3.0, ScarKind::Scorch);
                    self.effect(EffectKind::Steam, end, 0.45, 3.0);
                }
                if t >= 1.0 || moved.minus(next).length() > 0.2 {
                    self.impact(self.boss.position, 5.0);
                    self.recover(0.8);
                }
            }
            BossState::Recovery => {
                self.boss.settle();
                if self.boss.attack != Attack::TendrilSweep || self.boss.exposed > 0.0 {
                    for ix in 0..6 {
                        self.boss.tentacles[ix] = self.boss.tentacles[ix]
                            .lerp(self.boss.resting_tentacle(ix), STEP * 2.0);
                    }
                }
                if t >= 1.0 {
                    if self.boss.severed.count_ones() >= 2 {
                        self.boss.severed = 0;
                        self.effect(EffectKind::Steam, self.boss.core, 0.7, 5.0);
                    }
                    self.boss.enter(BossState::Watching, 0.65);
                }
            }
            _ => {}
        }
    }
}
