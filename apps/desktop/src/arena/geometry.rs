//! Logical world coordinates and collision geometry, independent of drawing.

pub(super) const WIDTH: f32 = 176.0;
pub(super) const HEIGHT: f32 = 116.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    pub fn plus(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
    pub fn minus(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
    pub fn scale(self, scale: f32) -> Self {
        Self::new(self.x * scale, self.y * scale)
    }
    pub fn length(self) -> f32 {
        self.x.hypot(self.y)
    }
    pub fn normalized(self) -> Self {
        let length = self.length();
        if length > f32::EPSILON {
            self.scale(1.0 / length)
        } else {
            Self::new(0.0, -1.0)
        }
    }
    pub fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y
    }
    pub fn perpendicular(self) -> Self {
        Self::new(-self.y, self.x)
    }
    pub fn angle(self) -> f32 {
        self.y.atan2(self.x)
    }
    pub fn from_angle(angle: f32) -> Self {
        Self::new(angle.cos(), angle.sin())
    }
    pub fn lerp(self, other: Self, progress: f32) -> Self {
        self.plus(other.minus(self).scale(progress.clamp(0.0, 1.0)))
    }
    pub fn clamped(self, margin: f32) -> Self {
        Self::new(
            self.x.clamp(margin, WIDTH - margin),
            self.y.clamp(margin, HEIGHT - margin),
        )
    }
}

pub(super) fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub(super) fn segment_distance(point: Vec2, from: Vec2, to: Vec2) -> f32 {
    let segment = to.minus(from);
    let length_squared = segment.dot(segment);
    let progress = if length_squared > f32::EPSILON {
        (point.minus(from).dot(segment) / length_squared).clamp(0.0, 1.0)
    } else {
        0.0
    };
    point.minus(from.plus(segment.scale(progress))).length()
}

/// Earliest contact along a swept segment. Starting inside counts as contact,
/// which also makes this useful for relative-motion projectile collisions.
pub(super) fn circle_hit(point: Vec2, from: Vec2, to: Vec2, radius: f32) -> Option<f32> {
    let relative = from.minus(point);
    let delta = to.minus(from);
    let c = relative.dot(relative) - radius * radius;
    if c <= 0.0 {
        return Some(0.0);
    }
    let a = delta.dot(delta);
    if a <= f32::EPSILON {
        return None;
    }
    let b = relative.dot(delta);
    let discriminant = b * b - a * c;
    if discriminant < 0.0 {
        return None;
    }
    let time = (-b - discriminant.sqrt()) / a;
    (0.0..=1.0).contains(&time).then_some(time)
}

pub(super) fn segments_distance(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> f32 {
    let cross = |u: Vec2, v: Vec2| u.x * v.y - u.y * v.x;
    let ab = b.minus(a);
    let cd = d.minus(c);
    let determinant = cross(ab, cd);
    if determinant.abs() > f32::EPSILON {
        let offset = c.minus(a);
        let t = cross(offset, cd) / determinant;
        let u = cross(offset, ab) / determinant;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            return 0.0;
        }
    }
    segment_distance(a, c, d)
        .min(segment_distance(b, c, d))
        .min(segment_distance(c, a, b))
        .min(segment_distance(d, a, b))
}
