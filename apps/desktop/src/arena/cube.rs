//! Cursor's cube: which face carries the pointer facet as the cube tumbles,
//! and how a quarter-turn about the leading bottom edge carries the body.
//!
//! Faces are named in model space, where +y is the way the cube rolls. The
//! cube is symmetric, so its yaw can snap by quarter-turns at no visible cost
//! as long as the facet is re-expressed to keep its world direction.

use super::geometry::Vec2;
use std::f32::consts::{FRAC_PI_4, SQRT_2};

pub(super) const SIDE: f32 = 13.0;
pub(super) const HALF: f32 = SIDE * 0.5;

pub(super) const EAST: u8 = 0;
pub(super) const WEST: u8 = 1;
pub(super) const FRONT: u8 = 2;
pub(super) const BACK: u8 = 3;
pub(super) const UP: u8 = 4;
pub(super) const DOWN: u8 = 5;

/// The facet's outward unit normal in model space.
pub(super) fn normal(facet: u8) -> (f32, f32, f32) {
    match facet {
        EAST => (1.0, 0.0, 0.0),
        WEST => (-1.0, 0.0, 0.0),
        FRONT => (0.0, 1.0, 0.0),
        BACK => (0.0, -1.0, 0.0),
        UP => (0.0, 0.0, 1.0),
        _ => (0.0, 0.0, -1.0),
    }
}
fn from_normal(x: f32, y: f32, z: f32) -> u8 {
    if z > 0.5 {
        UP
    } else if z < -0.5 {
        DOWN
    } else if x > 0.5 {
        EAST
    } else if x < -0.5 {
        WEST
    } else if y > 0.5 {
        FRONT
    } else {
        BACK
    }
}
/// Where a point drawn on the front face at `(x, z)` across it lands once the
/// facet is turned onto its own face, `out` from the cube's center, in model
/// space about the center. The sculpture turns the facet the same way.
pub(super) fn facet_point(facet: u8, x: f32, z: f32, out: f32) -> (f32, f32, f32) {
    match facet {
        UP => (x, -z, out),
        DOWN => (x, z, -out),
        BACK => (x, -out, -z),
        EAST => (out, -x, z),
        WEST => (-out, x, z),
        _ => (x, out, z),
    }
}
/// Where the pointer's weak point sits across the facet: on the lit
/// pointer, along the line from its tip to its notch.
pub(super) const POINTER: (f32, f32) = (-2.2, 2.2);
/// The facet after one forward quarter-turn: the top rolls over the front.
pub(super) fn rolled(facet: u8) -> u8 {
    match facet {
        UP => FRONT,
        FRONT => DOWN,
        DOWN => BACK,
        BACK => UP,
        side => side,
    }
}
/// The same world face once the cube's yaw has turned by `turn` radians, a
/// whole number of quarter-turns.
pub(super) fn turned(facet: u8, turn: f32) -> u8 {
    let (x, y, z) = normal(facet);
    let (s, c) = (-turn).sin_cos();
    from_normal(x * c - y * s, x * s + y * c, z)
}
/// Where a horizontal facet points on the ground for a cube at `yaw`, or
/// `None` while it faces the sky or the floor.
pub(super) fn ground_direction(facet: u8, yaw: f32) -> Option<Vec2> {
    let (x, y, z) = normal(facet);
    if z.abs() > 0.5 {
        return None;
    }
    let (s, c) = yaw.sin_cos();
    Some(Vec2::new(x * c - y * s, x * s + y * c))
}
/// The yaw whose forward axis is `direction`.
pub(super) fn yaw_toward(direction: Vec2) -> f32 {
    (-direction.x).atan2(direction.y)
}
/// A quarter-turn `theta` radians along about the leading bottom edge: how far
/// the center has risen above rest, and how far forward it has travelled.
pub(super) fn tumble(theta: f32) -> (f32, f32) {
    let reach = HALF * SQRT_2;
    (
        reach * (FRAC_PI_4 + theta).sin() - HALF,
        HALF - reach * (FRAC_PI_4 + theta).cos(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI};

    #[test]
    fn four_quarter_turns_bring_every_facet_home() {
        for facet in 0..6 {
            let mut f = facet;
            for _ in 0..4 {
                f = rolled(f);
            }
            assert_eq!(f, facet);
            assert_eq!(turned(turned(facet, FRAC_PI_2), -FRAC_PI_2), facet);
        }
    }

    #[test]
    fn a_turned_cube_keeps_its_facet_pointing_the_same_way() {
        for facet in [EAST, WEST, FRONT, BACK] {
            for yaw in [0.0, FRAC_PI_2, PI, -FRAC_PI_2] {
                for turn in [FRAC_PI_2, PI, -FRAC_PI_2] {
                    let before = ground_direction(facet, yaw).unwrap();
                    let after = ground_direction(turned(facet, turn), yaw + turn).unwrap();
                    assert!(before.minus(after).length() < 1e-4, "{facet} {yaw} {turn}");
                }
            }
        }
    }

    #[test]
    fn a_quarter_turn_lands_one_side_forward_at_rest() {
        let (lift, advance) = tumble(0.0);
        assert!(lift.abs() < 1e-4 && advance.abs() < 1e-4);
        let (lift, advance) = tumble(FRAC_PI_2);
        assert!(lift.abs() < 1e-4 && (advance - SIDE).abs() < 1e-4);
        assert!(tumble(FRAC_PI_4).0 > 2.0);
        assert_eq!(yaw_toward(Vec2::new(0.0, 1.0)), 0.0);
        assert!(
            ground_direction(FRONT, yaw_toward(Vec2::new(1.0, 0.0)))
                .unwrap()
                .minus(Vec2::new(1.0, 0.0))
                .length()
                < 1e-4
        );
    }
}
