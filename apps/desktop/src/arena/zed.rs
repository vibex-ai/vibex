//! ZCode's Z, after the Z.ai logo: a long diagonal slab, and two bars that
//! sit across its ends at home and fly off as fists in a fight.
//!
//! Model space: x across the face, z up, +y toward the viewer. The outlines
//! are the logo's own parallelograms, scaled so the Z stands 24 units tall.

use super::geometry::Vec2;

/// An outline in the face's (x, z) plane: lower left, lower right, upper
/// right, upper left.
pub(super) type Outline = [(f32, f32); 4];

/// The slab: the logo's broad diagonal from lower left to upper right.
pub(super) const SLAB: Outline = [(-10.8, 2.0), (-2.46, 2.0), (10.8, 26.0), (2.46, 26.0)];
/// The two bars, top left and bottom right, as outlines about their own
/// centres, and where those centres sit on the assembled Z.
pub(super) const BARS: [(Outline, (f32, f32)); 2] = [
    (
        [(-5.15, -1.77), (3.19, -1.77), (5.15, 1.77), (-3.19, 1.77)],
        (-5.06, 24.23),
    ),
    (
        [(-5.14, -1.77), (3.18, -1.77), (5.14, 1.77), (-3.18, 1.77)],
        (4.99, 3.77),
    ),
];
/// The slab's depth, and each bar's.
pub(super) const SLAB_DEPTH: f32 = 6.0;
pub(super) const BAR_DEPTH: f32 = 5.0;

/// Where bar `ix` sits on a Z standing at `position` with `yaw` and lifted
/// `height`: the ground point under its centre and the centre's height.
pub(super) fn seated(position: Vec2, yaw: f32, height: f32, ix: usize) -> (Vec2, f32) {
    let (x, z) = BARS[ix].1;
    let (s, c) = yaw.sin_cos();
    (position.plus(Vec2::new(x * c, x * s * 0.62)), height + z)
}
