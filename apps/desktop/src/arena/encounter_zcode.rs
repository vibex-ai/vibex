use super::*;
use crate::arena::{
    guardian::ZCODE_HOVER,
    map::{GATE, SWITCH_RADIUS, SWITCHES},
};

/// A fist's landing reach.
const SLAM_REACH: f32 = 6.0;
const SWEEP: f32 = 2.4;
pub(in crate::arena) const SWEEP_WIDTH: f32 = 2.2;
/// How long the bared core stays open before it fires down the hall.
const OPEN: f32 = 1.5;
/// How long a fist holds one switch alone before it gives up.
const PATIENCE: f32 = 7.0;

impl Arena {
    /// ZCode stands on its dais and fights with the bars of its Z: they slam
    /// the archer by turns, and a fist that lands on one of the two floor
    /// switches stays there holding it down. Every fourth blow the slab's
    /// eye sweeps a laser through a half circle instead; roll through it or
    /// hide behind a column. With both switches held, the shutter over the
    /// core slides open for a moment before it fires straight down the hall.
    pub(super) fn tick_zcode(&mut self) {
        let t = self.boss.progress();
        self.boss.position = GATE;
        self.boss.previous_position = GATE;
        self.boss.face(self.player.position, STEP * 0.6);
        self.boss.yaw = self.boss.yaw.clamp(-0.3, 0.3);
        let striking = matches!(self.boss.attack, Attack::LeftFist | Attack::RightFist)
            && matches!(self.boss.state, BossState::Windup | BossState::Striking);
        for ix in 0..2 {
            // A free fist drifts back to its station between blows.
            if !self.boss.pinned[ix] && !(striking && self.boss.guard == ix) {
                let rest = self.boss.resting_hand(ix);
                self.boss.hands[ix] = self.boss.hands[ix].lerp(rest, STEP * 4.0);
                self.boss.hand_heights[ix] +=
                    (ZCODE_HOVER - self.boss.hand_heights[ix]) * (STEP * 4.0).min(1.0);
            }
        }
        // A fist holding a switch on its own gives up in the end.
        if self.boss.pinned[0] != self.boss.pinned[1] {
            self.boss.shield += STEP;
            if self.boss.shield > PATIENCE {
                self.boss.pinned = [false; 2];
            }
        } else {
            self.boss.shield = 0.0;
        }
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) if self.boss.time >= self.boss.duration => {
                if self.boss.pinned == [true, true] {
                    self.boss.attack = Attack::Open;
                    self.boss.attacks = self.boss.attacks.wrapping_add(1);
                    self.boss.enter(BossState::Windup, 0.45);
                    self.effect(EffectKind::Steam, self.boss.core, 0.6, 6.0);
                } else if self.boss.attacks % 4 == 3 {
                    self.boss.begin(Attack::Sweep, self.player.position, 0.9);
                } else {
                    let preferred = (self.boss.attacks % 2) as usize;
                    let ix = if self.boss.pinned[preferred] {
                        1 - preferred
                    } else {
                        preferred
                    };
                    self.boss.guard = ix;
                    let fist = if ix == 0 {
                        Attack::LeftFist
                    } else {
                        Attack::RightFist
                    };
                    self.boss.begin(fist, self.player.position, 0.6);
                }
            }
            (BossState::Windup, Attack::LeftFist | Attack::RightFist) => {
                let ix = self.boss.guard;
                if !self.boss.target_locked() {
                    self.boss.target = self.map.move_body(GATE, self.player.position, 1.0);
                }
                let target = self.boss.target;
                self.boss.hands[ix] = self.boss.hands[ix].lerp(target, STEP * 10.0);
                self.boss.hand_heights[ix] += (18.0 - self.boss.hand_heights[ix]) * (STEP * 8.0);
                if t >= 1.0 {
                    let height = self.boss.hand_heights[ix];
                    self.boss.enter(BossState::Striking, 0.16);
                    self.boss.from_height = height;
                }
            }
            (BossState::Striking, Attack::LeftFist | Attack::RightFist) => {
                let ix = self.boss.guard;
                self.boss.hands[ix] = self.boss.target;
                self.boss.hand_heights[ix] =
                    self.boss.from_height + (1.8 - self.boss.from_height) * t * t;
                if t >= 1.0 {
                    let at = self.boss.target;
                    self.impact(at, SLAM_REACH);
                    let held = |other: usize, boss: &Boss| {
                        boss.pinned[other]
                            && boss.hands[other].minus(at).length() < SWITCH_RADIUS * 2.0
                    };
                    if let Some(switch) = SWITCHES
                        .iter()
                        .find(|s| s.minus(at).length() < SWITCH_RADIUS)
                        && !held(1 - ix, &self.boss)
                    {
                        // It lands on a switch and stays to hold it down.
                        self.boss.pinned[ix] = true;
                        self.boss.hands[ix] = *switch;
                        self.effect(EffectKind::Spark, *switch, 0.6, 7.0);
                    }
                    self.boss.enter(BossState::Recovery, 0.5);
                }
            }
            (BossState::Recovery, Attack::LeftFist | Attack::RightFist) if t >= 1.0 => {
                self.boss.enter(BossState::Watching, 0.4);
            }
            (BossState::Windup, Attack::Sweep) if t >= 1.0 => {
                self.boss.enter(BossState::Striking, SWEEP);
            }
            (BossState::Striking, Attack::Sweep) => {
                // West, through south, to east.
                let angle = PI - PI * t;
                self.beam(
                    self.zcode_eye(),
                    Vec2::from_angle(angle),
                    SWEEP_WIDTH,
                    0.0,
                    STEP * 2.0,
                );
                if t >= 1.0 {
                    self.boss.enter(BossState::Watching, 0.8);
                }
            }
            (BossState::Windup, Attack::Open) if t >= 1.0 => {
                self.boss.exposed = OPEN;
                self.boss.enter(BossState::Recovery, OPEN);
            }
            (BossState::Recovery, Attack::Open) if t >= 1.0 => {
                // Bared too long: it fires straight down the hall and lets go.
                self.beam(self.zcode_eye(), Vec2::new(0.0, 1.0), 3.5, 0.35, 0.6);
                self.boss.pinned = [false; 2];
                self.boss.enter(BossState::Watching, 1.4);
            }
            _ => {}
        }
    }

    /// Where the slab's lasers leave the floor: in front of its dais.
    pub(in crate::arena) fn zcode_eye(&self) -> Vec2 {
        GATE.plus(Vec2::new(0.0, 4.0))
    }
}
