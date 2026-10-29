use super::*;
use crate::arena::cube::{self, DOWN, FRONT, HALF, SIDE, UP};

/// Quarter-turns in one run before the pointer facet charges.
const STEPS: u8 = 4;
const TUMBLE: f32 = 0.34;
const PAUSE: f32 = 0.14;
const CHARGE: f32 = 0.8;
const FIRE: f32 = 0.45;
pub(super) const BEAM_WIDTH: f32 = 2.4;
/// A blasted cube lands with its facet toward the viewer and to the right.
const LANDED: f32 = -PI * 0.25;

/// The nearest yaw that stands the cube corner-on along the diagonals.
fn lattice(yaw: f32) -> f32 {
    ((yaw - PI * 0.25) / (PI * 0.5)).round() * PI * 0.5 + PI * 0.25
}

impl Arena {
    /// Cursor tumbles toward the archer four faces at a time, then its
    /// pointer facet charges and fires along whichever way it now faces. The
    /// facet is bare while it charges, but only from the side it points to:
    /// stand beside the line it will fire down, not on it. A facet pressed to
    /// the floor fires into the ground instead and blasts the cube over, and it
    /// lands dazed with the facet turned toward the viewer.
    pub(super) fn tick_cursor(&mut self) {
        let t = self.boss.progress();
        match self.boss.state {
            BossState::Watching if self.boss.time < self.boss.duration => {
                // The entry turns the cube freely toward the archer; it eases
                // back onto the diagonals it rolls along.
                self.rotate_yaw(lattice(self.boss.yaw), STEP * 3.0);
            }
            BossState::Watching => {
                if self.boss.dashes > 0 {
                    self.cursor_step();
                } else if self.boss.attack == Attack::Tumble {
                    self.cursor_charge();
                } else {
                    self.boss.dashes = STEPS;
                    self.cursor_step();
                }
            }
            BossState::Rushing => {
                let theta = smoothstep(t) * PI * 0.5;
                let (lift, advance) = cube::tumble(theta);
                self.boss.position = self.boss.from.plus(self.boss.direction.scale(advance));
                self.boss.height = lift;
                self.boss.pitch = -theta;
                if t >= 1.0 {
                    self.boss.position = self.boss.target;
                    self.boss.height = 0.0;
                    self.boss.pitch = 0.0;
                    self.boss.facet = cube::rolled(self.boss.facet);
                    self.boss.squash = 0.3;
                    self.boss.dashes = self.boss.dashes.saturating_sub(1);
                    let edge = self.boss.position.minus(self.boss.direction.scale(HALF));
                    self.effect(EffectKind::Dust, edge, 0.5, 4.0);
                    self.boss.enter(BossState::Watching, PAUSE);
                }
            }
            BossState::Windup if t >= 1.0 => match self.boss.facet {
                UP => {
                    // Fired at the sky: a harmless flare, and the next run.
                    let top = self.boss.local(0.0, 0.0, SIDE + 6.0);
                    self.effect(EffectKind::Flash, top, 0.4, 8.0);
                    self.boss.enter(BossState::Watching, 0.9);
                }
                DOWN => {
                    // Fired into the floor: the blast throws the cube over.
                    let at = self.boss.position;
                    self.impact_with(at, 7.0, 0.05);
                    self.boss.attack = Attack::Leap;
                    self.boss.spin = self.boss.yaw;
                    self.boss.from = at;
                    let inward = CENTER.minus(at).normalized().scale(10.0);
                    self.boss.target = self.map.move_body(at, at.plus(inward), self.boss.radius());
                    self.boss.enter(BossState::Striking, 0.8);
                }
                _ => {
                    self.effect(EffectKind::Flash, self.boss.core, 0.3, 6.0);
                    self.boss.enter(BossState::Striking, FIRE);
                }
            },
            BossState::Striking if self.boss.attack == Attack::Leap => {
                // A backward flip that turns to face the viewer on the way,
                // corner-on. `spin`, unused by the cube otherwise, holds the
                // yaw it left.
                let yaw = self.boss.spin;
                let turn = (LANDED - yaw + PI).rem_euclid(TAU) - PI;
                self.boss.yaw = yaw + turn * smoothstep(t);
                self.boss.pitch = smoothstep(t) * PI * 0.5;
                if self.arc(18.0) {
                    self.boss.yaw = LANDED;
                    self.boss.pitch = 0.0;
                    self.boss.facet = FRONT;
                    self.impact(self.boss.target, self.boss.impact_radius());
                    self.boss.exposed = 1.9;
                    self.boss.enter(BossState::Recovery, 2.0);
                    self.effect(EffectKind::Spark, self.boss.core, 0.8, 6.0);
                }
            }
            BossState::Striking if t >= 1.0 => self.boss.enter(BossState::Watching, 0.9),
            BossState::Recovery if t >= 1.0 => self.boss.enter(BossState::Watching, 0.6),
            _ => {}
        }
    }

    /// One quarter-turn toward the archer along the diagonal grid, or the
    /// charge when every way toward them is blocked.
    fn cursor_step(&mut self) {
        let delta = self.player.position.minus(self.boss.position);
        let mut choices = [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)]
            .map(|(x, y)| Vec2::new(x, y).normalized());
        choices.sort_by(|a, b| b.dot(delta).total_cmp(&a.dot(delta)));
        let radius = self.boss.radius();
        let Some(direction) = choices.into_iter().take(2).find(|d| {
            self.map
                .is_clear(self.boss.position.plus(d.scale(SIDE)), radius)
        }) else {
            self.boss.dashes = 0;
            self.cursor_charge();
            return;
        };
        // The cube is the same from every side, so it turns to roll forward
        // at no visible cost; the facet keeps the way it points.
        self.boss.yaw = lattice(self.boss.yaw);
        let yaw = cube::yaw_toward(direction);
        self.boss.facet = cube::turned(self.boss.facet, yaw - self.boss.yaw);
        self.boss.yaw = yaw;
        self.boss.direction = direction;
        self.boss.attack = Attack::Tumble;
        self.boss.from = self.boss.position;
        self.boss.target = self.boss.position.plus(direction.scale(SIDE));
        self.boss.enter(BossState::Rushing, TUMBLE);
    }

    fn cursor_charge(&mut self) {
        self.boss.attack = Attack::Laser;
        self.boss.attacks = self.boss.attacks.wrapping_add(1);
        self.boss.target = self.boss.position;
        let charge = if let Some(facing) = cube::ground_direction(self.boss.facet, self.boss.yaw) {
            let origin = self.boss.position.plus(facing.scale(HALF));
            self.beam(origin, facing, BEAM_WIDTH, CHARGE, FIRE);
            self.boss.target = self.map.beam_end(origin, facing);
            self.boss.exposed = CHARGE + 0.1;
            CHARGE
        } else if self.boss.facet == DOWN {
            CHARGE * 0.8
        } else {
            CHARGE
        };
        self.boss.enter(BossState::Windup, charge);
    }
}
