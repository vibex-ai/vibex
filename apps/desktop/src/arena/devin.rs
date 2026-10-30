//! Devin's flower, after the logo's three linked hexagons: a hub with a
//! mouth, three hexagon petals round it, and a vine of smaller hexagons
//! running out from each petal along the floor.
//!
//! The flower lies flat in combat, its petals and vines on the ground; at
//! home and on the lane it stands up to show the logo. `spin` turns it in its
//! own plane either way.

use std::f32::consts::{PI, TAU};

/// From the hub to the middle of each petal.
pub(super) const PETAL: f32 = 7.0;
/// A petal hexagon's radius, and a vine segment's.
pub(super) const PETAL_SIZE: f32 = 3.6;
pub(super) const SEGMENT_SIZE: f32 = 2.2;
/// The touch and arrow reach of one vine segment.
pub(super) const SEGMENT_REACH: f32 = 2.4;

/// The angle of petal `arm` in the flower's own plane when it has turned
/// `spin`: the logo's petals sit upper left, lower left and right.
pub(super) fn petal_angle(spin: f32, arm: usize) -> f32 {
    spin + PI * (2.0 / 3.0) + arm as f32 * TAU / 3.0
}
/// How far along its vine segment `k` lies from the hub.
pub(super) fn segment_radius(k: usize) -> f32 {
    PETAL + 5.4 + k as f32 * 4.9
}
