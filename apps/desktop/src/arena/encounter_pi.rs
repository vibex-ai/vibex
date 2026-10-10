use super::*;
use crate::arena::map::{SEAL_RADIUS, SEALS};

impl Arena {
    pub(super) fn tick_pi(&mut self) {
        let t = self.boss.progress();
        for (ix, seal) in SEALS.iter().enumerate() {
            if self.boss.seals & (1 << ix) != 0 {
                self.boss.hands[ix] = *seal;
                self.boss.hand_heights[ix] = 0.0;
            }
        }
        let fist = match self.boss.attack {
            Attack::LeftFist => Some(0),
            Attack::RightFist => Some(1),
            _ => None,
        };
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP * 0.8);
                for ix in 0..2 {
                    if self.boss.seals & (1 << ix) == 0 {
                        self.boss.hands[ix] = self.boss.hands[ix].lerp(
                            self.boss.resting_hand(if ix == 0 { -1.0 } else { 1.0 }),
                            STEP * 4.0,
                        );
                        self.boss.hand_heights[ix] = 3.0;
                    }
                }
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target();
                if let Some(ix) = fist {
                    self.boss.hands[ix] = self
                        .boss
                        .resting_hand(if ix == 0 { -1.0 } else { 1.0 })
                        .lerp(self.boss.target, smoothstep(t));
                    self.boss.hand_heights[ix] = 3.0 + smoothstep(t) * 20.0;
                } else if self.boss.attack == Attack::Sweep {
                    self.beam_tell();
                }
                if t >= 1.0 {
                    self.boss
                        .enter(BossState::Striking, if fist.is_some() { 0.3 } else { 1.05 });
                }
            }
            BossState::Striking => {
                if let Some(ix) = fist {
                    self.boss.hand_heights[ix] = (1.0 - t.powi(3)) * 23.0;
                    if t >= 1.0 {
                        let position = self.boss.hands[ix];
                        self.impact(position, self.boss.impact_radius());
                        if position.minus(SEALS[ix]).length() < SEAL_RADIUS {
                            self.boss.seals |= 1 << ix;
                            self.boss.seal_time[ix] = 13.0;
                            self.boss.hands[ix] = SEALS[ix];
                            self.effect(EffectKind::Seal, SEALS[ix], 1.2, 7.0);
                        }
                        if self.boss.seals == 0b11 {
                            self.boss.opening(4.4);
                            self.effect(EffectKind::Overload, self.boss.core, 0.7, 9.0);
                        } else {
                            self.recover(0.45);
                        }
                    }
                } else if self.boss.attack == Attack::Sweep {
                    self.beam_strike();
                    if t >= 1.0 {
                        self.recover(0.6);
                    }
                } else {
                    let volley = (t * 3.0) as u32 + 1;
                    if volley > self.boss.fired && volley <= 3 {
                        self.boss.fired = volley;
                        for side in [-1.0, 1.0] {
                            let origin = self.boss.local(side * 10.0, 1.0, 15.0);
                            self.projectile(
                                origin,
                                self.boss.target.minus(origin).normalized().scale(29.0),
                                ProjectileKind::Block,
                            );
                        }
                    }
                    if t >= 1.0 {
                        self.recover(0.65);
                    }
                }
            }
            BossState::Recovery => {
                for ix in 0..2 {
                    if self.boss.seals & (1 << ix) == 0 {
                        self.boss.hands[ix] = self.boss.hands[ix].lerp(
                            self.boss.resting_hand(if ix == 0 { -1.0 } else { 1.0 }),
                            STEP * 3.0,
                        );
                        self.boss.hand_heights[ix] =
                            (self.boss.hand_heights[ix] + STEP * 8.0).min(3.0);
                    }
                }
                if t >= 1.0 {
                    if self.boss.seals == 0b11 {
                        self.boss.seals = 0;
                        self.boss.seal_time = [0.0; 2];
                    }
                    self.boss.enter(BossState::Watching, 0.4);
                }
            }
            _ => {}
        }
    }
}
