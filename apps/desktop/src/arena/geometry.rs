//! Logical world coordinates and collision geometry, independent of drawing.

pub(super) const WIDTH: f32 = 96.0;
pub(super) const HEIGHT: f32 = 56.0;

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
