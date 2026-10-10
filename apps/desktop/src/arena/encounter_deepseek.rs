use super::*;
use crate::arena::map::ISLANDS;

impl Arena {
    pub(super) fn tick_deepseek(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching => {
                self.boss.face(self.player.position, STEP * 1.5);
                self.rest_or_prepare();
            }
            BossState::Windup => {
                self.follow_target();
                self.boss.height =
                    self.boss.from_height + (1.0 - self.boss.from_height) * smoothstep(t);
                if t >= 1.0 {
                    self.boss.from = self.boss.position;
                    self.boss.enter(
                        BossState::Striking,
                        if self.boss.attack == Attack::Pulse {
                            6.0
                        } else {
                            1.5
                        },
                    );
                }
            }
            BossState::Striking if self.boss.attack == Attack::Pulse => {
                if self.boss.fired == 0 {
                    self.boss.fired = 1;
                    let sunken = self.map.sunken;
                    self.map.sunken = 0;
                    for (ix, (position, radius)) in ISLANDS.iter().enumerate() {
                        if sunken & (1 << ix) != 0 {
                            self.effect(EffectKind::Wake, *position, 1.4, *radius + 3.0);
                        }
                    }
                    self.wave(self.boss.pulse_origin(), 3.0, 108.0, WaveKind::Water);
                    self.effect(EffectKind::Overload, self.boss.position, 1.1, 15.0);
                }
                self.boss.height = self.boss.from_height
                    + (-3.5 - self.boss.from_height) * smoothstep((t * 5.0).min(1.0));
                if t >= 1.0 {
                    self.begin_submerge();
                }
            }
            BossState::Striking => {
                self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.height = (t * PI).sin() * 21.0;
                self.boss.pitch = (0.5 - t) * (t * PI).sin() * 0.9;
                if t > 0.60 {
                    self.boss.exposed = STEP * 2.0;
                }
                if t >= 1.0 {
                    self.boss.height = 0.0;
                    self.boss.pitch = 0.0;
                    let broken = self
                        .map
                        .break_islands(self.boss.position, self.boss.impact_radius());
                    for (ix, (position, radius)) in ISLANDS.iter().enumerate() {
                        if broken & (1 << ix) != 0 {
                            self.effect(EffectKind::Rubble, *position, 1.4, *radius + 3.0);
                        }
                    }
                    self.impact(self.boss.position, self.boss.impact_radius());
                    self.effect(EffectKind::Wake, self.boss.position, 1.3, 15.0);
                    for side in [-1.0, 1.0] {
                        let target = self
                            .boss
                            .position
                            .plus(self.boss.direction.perpendicular().scale(side * 14.0))
                            .plus(self.boss.direction.scale(3.0));
                        let position = self.map.move_body(self.boss.position, target, 4.0);
                        self.hazard(HazardKind::Geyser, position, position, 0.85, 0.35, 3.0);
                    }
                    self.boss.opening(2.9);
                }
            }
            BossState::Recovery => {
                if t >= 1.0 {
                    if self.boss.attacks % 4 == 3 {
                        self.boss.enter(BossState::Watching, 0.4);
                    } else {
                        self.begin_submerge();
                    }
                }
            }
            BossState::Submerged => {
                self.boss.position = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.height = self.boss.from_height
                    + (-3.5 - self.boss.from_height) * smoothstep((t * 3.0).min(1.0));
                self.boss.face(self.player.position, STEP * 2.2);
                let wakes = (self.boss.time / 0.2) as u32;
                if wakes > self.boss.fired {
                    self.boss.fired = wakes;
                    self.effect(EffectKind::Wake, self.boss.position, 0.8, 7.0);
                }
                if t >= 1.0 {
                    self.boss.prepare(self.player.position);
                    if self.boss.attack == Attack::Pulse {
                        self.boss.duration = 1.65;
                    }
                }
            }
            _ => {}
        }
    }

    fn begin_submerge(&mut self) {
        self.boss.from = self.boss.position;
        let away = self.boss.position.minus(self.player.position).normalized();
        self.boss.target = self.map.move_body(
            self.boss.position,
            CENTER.plus(away.scale(32.0)),
            self.boss.radius(),
        );
        self.boss.exposed = 0.0;
        self.boss.enter(BossState::Submerged, 1.1);
    }
}
