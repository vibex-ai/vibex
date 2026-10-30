use super::*;
use crate::arena::{
    map::GATE,
    zed::{Breath, RISE, breath},
};

/// How long an arm tunnels to its next spot, and how deep it runs.
const BURROW: f32 = 1.3;
const BURIED: f32 = -6.0;
/// How long an emerged arm aims before it fires, and the freezing laser.
const AIM: f32 = 0.85;
const FIRE: f32 = 0.45;
pub(in crate::arena) const LASER_WIDTH: f32 = 2.0;
/// How near the laser must pass the mouth to break a tooth.
const BITE: f32 = 3.5;
/// The ice the laser leaves: spaced along its line, lethal for a while.
const SPIKE_GAP: f32 = 6.0;
const SPIKE_LIFE: f32 = 4.0;
const SPIKE_RADIUS: f32 = 2.2;
/// How hard the slab's breath shoves an archer in the open as it starts,
/// and how hard it keeps pushing after: under the speed a wall crushes at.
const SHOVE: f32 = 30.0;
const GUST: f32 = 12.0;

impl Arena {
    /// ZCode stands on its dais and breathes: it rests, rears back with its
    /// mouth open, then blows, and its breath sweeps an archer in the open
    /// down the hall — into a wall, fatally — unless a column shelters them.
    /// Its two bars are arms that tunnel under the floor by turns, rise near
    /// the archer, and fire a freezing laser at them that leaves a line of ice
    /// spikes behind. A laser that passes the mouth breaks a tooth from it;
    /// with both teeth gone, the core shows each time the slab rears back.
    pub(super) fn tick_zcode(&mut self) {
        let t = self.boss.progress();
        self.boss.position = GATE;
        self.boss.previous_position = GATE;
        self.boss.yaw *= 0.95;
        self.zcode_breathe();
        let ix = self.boss.guard;
        // The idle arm stays down out of sight.
        let other = 1 - ix;
        self.boss.hand_heights[other] +=
            (BURIED - self.boss.hand_heights[other]) * (STEP * 6.0).min(1.0);
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) => {
                self.boss.hand_heights[ix] +=
                    (BURIED - self.boss.hand_heights[ix]) * (STEP * 6.0).min(1.0);
                if self.boss.time >= self.boss.duration {
                    // The other arm takes its turn, tunnelling to a spot near
                    // the archer.
                    self.boss.guard = other;
                    self.boss.attack = Attack::Burrow;
                    self.boss.from = self.boss.hands[other];
                    self.boss.target = self.zcode_rise_spot();
                    self.boss.enter(BossState::Rushing, BURROW);
                }
            }
            (BossState::Rushing, Attack::Burrow) => {
                self.boss.hands[ix] = self.boss.from.lerp(self.boss.target, smoothstep(t));
                self.boss.hand_heights[ix] = BURIED;
                if t >= 1.0 {
                    self.boss.begin(Attack::Laser, self.player.position, AIM);
                    self.boss.dashes = 0;
                }
            }
            (BossState::Windup, Attack::Laser) => {
                // It rises out of the floor and takes aim.
                self.boss.hand_heights[ix] = BURIED + (4.0 - BURIED) * smoothstep(t / 0.35);
                if !self.boss.target_locked() {
                    self.boss.target = self.player.position;
                } else if self.boss.dashes == 0 {
                    self.boss.dashes = 1;
                    self.zcode_lay_laser(ix);
                }
                if t >= 1.0 {
                    self.boss.enter(BossState::Striking, FIRE);
                    if self.boss.dashes == 2 && self.boss.teeth > 0 {
                        // Through the mouth: a tooth breaks away.
                        self.boss.teeth -= 1;
                        let mouth = self.zcode_mouth();
                        self.effect(EffectKind::Burst, mouth, 0.8, 8.0);
                        self.effect(EffectKind::Rubble, mouth, 0.9, 6.0);
                    }
                }
            }
            (BossState::Striking, Attack::Laser) if t >= 1.0 => {
                self.boss.enter(BossState::Recovery, 0.5);
            }
            (BossState::Recovery, _) => {
                self.boss.hand_heights[ix] +=
                    (BURIED - self.boss.hand_heights[ix]) * (STEP * 8.0).min(1.0);
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.7);
                }
            }
            _ => {}
        }
    }

    /// The slab's breath, on its own clock: rearing back, it bares the core
    /// once its teeth are gone; blowing, it sweeps an unsheltered archer down
    /// the hall.
    fn zcode_breathe(&mut self) {
        self.boss.shield += STEP;
        match breath(self.boss.shield) {
            Breath::Rest => self.boss.pitch *= 0.9,
            Breath::Rise(u) => {
                self.boss.pitch = 0.22 * smoothstep(u / 0.4);
                if self.boss.teeth == 0 {
                    // Bare for as long as it stays reared back.
                    self.boss.exposed = self.boss.exposed.max((1.0 - u) * RISE + STEP);
                }
            }
            Breath::Blow(u) => {
                self.boss.pitch = 0.22 - 0.4 * smoothstep(u / 0.2);
                let mouth = self.zcode_mouth();
                let away = self.player.position.minus(mouth);
                let open = self.player.position.y > mouth.y
                    && !self.map.cover_hit(mouth, self.player.position);
                if open && self.player.roll_remaining <= 0.0 {
                    let strength = if u < 0.08 { SHOVE } else { GUST };
                    let push = away.normalized().scale(strength);
                    if self.player.knockback.length() < push.length() {
                        self.player.knockback = push;
                    }
                }
            }
        }
    }

    /// Lays the arm's laser at its lock: from the arm through the archer to
    /// the wall, or ended by the mouth when it passes it.
    fn zcode_lay_laser(&mut self, ix: usize) {
        let from = self.boss.hands[ix];
        let direction = self.boss.target.minus(from).normalized();
        let mut end = self.map.beam_end(from, direction);
        let mouth = self.zcode_mouth();
        let delay = self.boss.duration - self.boss.time;
        if self.boss.teeth > 0 && segment_distance(mouth, from, end) < BITE {
            end = mouth;
            self.boss.dashes = 2;
        }
        if self.phase == Phase::Battle && self.beams.len() < MAX_BEAMS {
            self.beams.push(Beam {
                from,
                to: end,
                width: LASER_WIDTH,
                time: 0.0,
                delay,
                duration: FIRE,
            });
        }
        // Ice erupts along the line once it has fired.
        let length = end.minus(from).length();
        let mut along = SPIKE_GAP;
        while along < length {
            let spot = from.plus(direction.scale(along));
            self.hazard(
                HazardKind::Spike,
                spot,
                delay + FIRE * 0.6,
                SPIKE_LIFE,
                SPIKE_RADIUS,
            );
            along += SPIKE_GAP;
        }
    }

    /// Where the next arm rises: near the archer but not on them.
    fn zcode_rise_spot(&mut self) -> Vec2 {
        for _ in 0..16 {
            let a = (self.random() % 628) as f32 * 0.01;
            let spot = self
                .player
                .position
                .plus(Vec2::new(a.cos(), a.sin()).scale(24.0));
            if self.map.standable(spot, 3.0) && spot.minus(GATE).length() > 20.0 {
                return spot;
            }
        }
        self.map
            .move_body(GATE, CENTER.plus(Vec2::new(0.0, 30.0)), 3.0)
    }

    /// The ground point before the slab's mouth.
    pub(in crate::arena) fn zcode_mouth(&self) -> Vec2 {
        GATE.plus(Vec2::new(0.0, 5.0))
    }
}
