//! Encounter terrain. The same footprints drive drawing, cover and navigation.

use super::{
    geometry::{HEIGHT, Vec2, WIDTH, segment_distance},
    guardian::Guardian,
};

pub(super) const CENTER: Vec2 = Vec2::new(WIDTH * 0.5, HEIGHT * 0.5);

#[derive(Clone, Copy, Debug)]
pub(super) struct Pillar {
    pub position: Vec2,
    pub radius: f32,
    pub height: f32,
}

#[derive(Clone, Debug)]
pub(super) struct Map {
    pub guardian: Guardian,
    /// Broken columns remain rubble for this encounter; perimeter impacts still
    /// open the rolling guardian, so missing an opening cannot exhaust the puzzle.
    pub broken: u8,
}

const GARDEN: [Pillar; 4] = pillars(
    [(40.0, 29.0), (136.0, 29.0), (36.0, 89.0), (140.0, 89.0)],
    3.0,
    9.0,
);
const COURT: [Pillar; 4] = pillars(
    [(59.0, 40.0), (117.0, 40.0), (59.0, 80.0), (117.0, 80.0)],
    3.6,
    12.0,
);
const OBSERVATORY: [Pillar; 3] = pillars([(88.0, 22.0), (44.0, 83.0), (132.0, 83.0)], 2.5, 10.0);
const FOUNDRY: [Pillar; 4] = pillars(
    [(51.0, 37.0), (125.0, 37.0), (51.0, 82.0), (125.0, 82.0)],
    4.0,
    9.0,
);
const TERRACE: [Pillar; 2] = pillars([(47.0, 58.0), (129.0, 58.0)], 3.0, 7.0);
pub(super) const ISLANDS: [(Vec2, f32); 4] = [
    (Vec2::new(48.0, 48.0), 9.0),
    (Vec2::new(122.0, 35.0), 8.0),
    (Vec2::new(104.0, 83.0), 10.0),
    (Vec2::new(66.0, 89.0), 6.0),
];
pub(super) const DAIS: [Vec2; 3] = [
    Vec2::new(88.0, 35.0),
    Vec2::new(53.0, 76.0),
    Vec2::new(123.0, 76.0),
];

const fn pillars<const N: usize>(
    positions: [(f32, f32); N],
    radius: f32,
    height: f32,
) -> [Pillar; N] {
    let mut result = [Pillar {
        position: Vec2::new(0.0, 0.0),
        radius,
        height,
    }; N];
    let mut ix = 0;
    while ix < N {
        result[ix].position = Vec2::new(positions[ix].0, positions[ix].1);
        ix += 1;
    }
    result
}

impl Map {
    pub fn new(guardian: Guardian) -> Self {
        Self {
            guardian,
            broken: 0,
        }
    }

    pub fn pillars(&self) -> &'static [Pillar] {
        match self.guardian {
            Guardian::Claude => &GARDEN,
            Guardian::Codex => &COURT,
            Guardian::Pi => &OBSERVATORY,
            Guardian::OpenCode => &FOUNDRY,
            Guardian::DeepSeek => &[],
            Guardian::Copilot => &TERRACE,
        }
    }

    pub fn intact(&self, ix: usize) -> bool {
        self.broken & (1 << ix) == 0
    }

    pub fn inside(&self, p: Vec2, margin: f32) -> bool {
        let d = p.minus(CENTER);
        let x = d.x.abs();
        let y = d.y.abs();
        match self.guardian {
            Guardian::Claude => {
                x < 73.0 - margin && y < 46.0 - margin && x + y < 105.0 - margin * 1.42
            }
            Guardian::Codex => (x / (67.0 - margin)).powi(2) + (y / (47.0 - margin)).powi(2) < 1.0,
            Guardian::Pi => {
                x < 69.0 - margin && y < 45.0 - margin && x * 0.55 + y < 69.0 - margin * 1.2
            }
            Guardian::OpenCode => x < 73.0 - margin && y < 47.0 - margin,
            Guardian::DeepSeek => {
                (x / (76.0 - margin)).powi(2) + (y / (49.0 - margin)).powi(2) < 1.0
            }
            Guardian::Copilot => {
                x < 67.0 - margin && y < 49.0 - margin && !(x > 44.0 - margin && y > 35.0 - margin)
            }
        }
    }

    pub fn is_clear(&self, position: Vec2, radius: f32) -> bool {
        self.inside(position, radius)
            && self.pillars().iter().enumerate().all(|(ix, pillar)| {
                !self.intact(ix)
                    || position.minus(pillar.position).length() > radius + pillar.radius
            })
    }

    pub fn move_body(&self, from: Vec2, to: Vec2, radius: f32) -> Vec2 {
        let delta = to.minus(from);
        let steps = (delta.length() / 0.6).ceil().max(1.0) as usize;
        let step = delta.scale(1.0 / steps as f32);
        let mut p = from;
        for _ in 0..steps {
            let next = p.plus(step);
            if self.is_clear(next, radius) {
                p = next;
            } else {
                let x = Vec2::new(next.x, p.y);
                if self.is_clear(x, radius) {
                    p = x;
                }
                let y = Vec2::new(p.x, next.y);
                if self.is_clear(y, radius) {
                    p = y;
                }
            }
        }
        p
    }

    pub fn pillar_hit(&self, from: Vec2, to: Vec2, radius: f32) -> Option<usize> {
        self.pillars().iter().enumerate().find_map(|(ix, pillar)| {
            (self.intact(ix)
                && segment_distance(pillar.position, from, to) <= radius + pillar.radius)
                .then_some(ix)
        })
    }

    /// Projectiles meet the visible column face, which extends north of its
    /// footprint by its height. Movement continues to use ground circles.
    pub fn cover_hit(&self, from: Vec2, to: Vec2) -> bool {
        self.pillars().iter().enumerate().any(|(ix, pillar)| {
            if !self.intact(ix) {
                return false;
            }
            let low = Vec2::new(
                pillar.position.x - pillar.radius,
                pillar.position.y - pillar.height - pillar.radius * 0.5,
            );
            let high = Vec2::new(
                pillar.position.x + pillar.radius,
                pillar.position.y + pillar.radius * 0.5,
            );
            let delta = to.minus(from);
            let mut enter: f32 = 0.0;
            let mut leave: f32 = 1.0;
            for (origin, step, low, high) in [
                (from.x, delta.x, low.x, high.x),
                (from.y, delta.y, low.y, high.y),
            ] {
                if step.abs() < f32::EPSILON {
                    if origin < low || origin > high {
                        return false;
                    }
                } else {
                    let a = (low - origin) / step;
                    let b = (high - origin) / step;
                    enter = enter.max(a.min(b));
                    leave = leave.min(a.max(b));
                    if enter > leave {
                        return false;
                    }
                }
            }
            true
        })
    }

    pub fn beam_end(&self, from: Vec2, direction: Vec2) -> Vec2 {
        let mut end = from;
        for ix in 1..360 {
            let p = from.plus(direction.scale(ix as f32 * 0.5));
            if !self.inside(p, 0.5) || self.cover_hit(end, p) {
                break;
            }
            end = p;
        }
        end
    }

    pub fn on_island(&self, p: Vec2) -> bool {
        self.guardian == Guardian::DeepSeek
            && ISLANDS.iter().any(|(center, radius)| {
                let d = p.minus(*center);
                (d.x / radius).powi(2) + (d.y / (radius * 0.68)).powi(2) < 1.0
            })
    }

    pub fn speed(&self, p: Vec2) -> f32 {
        if self.guardian == Guardian::DeepSeek && !self.on_island(p) {
            0.82
        } else {
            1.0
        }
    }

    pub fn spawn(&self, seed: &mut u32, avoid: Vec2) -> Vec2 {
        let mut best = CENTER.plus(Vec2::new(0.0, 30.0));
        let mut distance = -1.0;
        for _ in 0..80 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 17;
            *seed ^= *seed << 5;
            let x = 22.0 + (*seed & 0xffff) as f32 / 65535.0 * 132.0;
            let y = 19.0 + (*seed >> 16) as f32 / 65535.0 * 78.0;
            let p = Vec2::new(x, y);
            if !self.is_clear(p, 3.0) {
                continue;
            }
            let d = p.minus(avoid).length();
            if d > 29.0 {
                return p;
            }
            if d > distance {
                best = p;
                distance = d;
            }
        }
        best
    }
}
