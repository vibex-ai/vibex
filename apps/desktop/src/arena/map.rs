//! Encounter terrain. The same footprints drive drawing, cover and navigation.

use super::{
    geometry::{HEIGHT, Vec2, WIDTH, circle_hit},
    guardian::Guardian,
};

pub(super) const CENTER: Vec2 = Vec2::new(WIDTH * 0.5, HEIGHT * 0.5);
pub(super) const MAX_SCARS: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScarKind {
    Crack,
    Scorch,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Scar {
    pub position: Vec2,
    pub radius: f32,
    pub kind: ScarKind,
}

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
    pub sunken: u8,
    pub scars: Vec<Scar>,
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
pub(super) const SEALS: [Vec2; 2] = [Vec2::new(57.0, 64.0), Vec2::new(119.0, 64.0)];
pub(super) const SEAL_RADIUS: f32 = 5.5;

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
            sunken: 0,
            scars: Vec::with_capacity(MAX_SCARS),
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
        self.pillars()
            .iter()
            .enumerate()
            .filter(|(ix, _)| self.intact(*ix))
            .filter_map(|(ix, p)| {
                circle_hit(p.position, from, to, radius + p.radius).map(|t| (ix, t))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(ix, _)| ix)
    }

    /// Projectiles meet the visible column face, which extends north of its
    /// footprint by its height. Movement continues to use ground circles.
    pub fn cover_hit(&self, from: Vec2, to: Vec2) -> bool {
        self.cover_contact(from, to).is_some()
    }

    pub fn cover_contact(&self, from: Vec2, to: Vec2) -> Option<(usize, f32)> {
        self.pillars()
            .iter()
            .enumerate()
            .filter_map(|(ix, pillar)| {
                if !self.intact(ix) {
                    return None;
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
                            return None;
                        }
                    } else {
                        let a = (low - origin) / step;
                        let b = (high - origin) / step;
                        enter = enter.max(a.min(b));
                        leave = leave.min(a.max(b));
                        if enter > leave {
                            return None;
                        }
                    }
                }
                Some((ix, enter))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
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
            && ISLANDS.iter().enumerate().any(|(ix, (center, radius))| {
                let d = p.minus(*center);
                self.sunken & (1 << ix) == 0
                    && (d.x / radius).powi(2) + (d.y / (radius * 0.68)).powi(2) < 1.0
            })
    }

    pub fn break_islands(&mut self, position: Vec2, radius: f32) -> u8 {
        let before = self.sunken;
        for (ix, (center, island_radius)) in ISLANDS.iter().enumerate() {
            let d = position.minus(*center);
            if (d.x / (island_radius + radius)).powi(2)
                + (d.y / (island_radius * 0.68 + radius)).powi(2)
                < 1.0
            {
                self.sunken |= 1 << ix;
            }
        }
        self.sunken ^ before
    }

    pub fn scar(&mut self, position: Vec2, radius: f32, kind: ScarKind) {
        if let Some(scar) = self
            .scars
            .iter_mut()
            .find(|scar| scar.kind == kind && scar.position.minus(position).length() < 3.0)
        {
            scar.radius = scar.radius.max(radius);
            return;
        }
        if self.scars.len() == MAX_SCARS {
            self.scars.remove(0);
        }
        self.scars.push(Scar {
            position,
            radius,
            kind,
        });
    }

    pub fn speed(&self, p: Vec2) -> f32 {
        if self.guardian == Guardian::DeepSeek && !self.on_island(p) {
            0.82
        } else {
            1.0
        }
    }

    pub fn spawn(&self, seed: &mut u32, avoid: Vec2) -> Vec2 {
        if self.guardian == Guardian::DeepSeek && self.sunken != 0b1111 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 17;
            *seed ^= *seed << 5;
            for offset in 0..4 {
                let ix = (*seed as usize + offset) % 4;
                let (center, radius) = ISLANDS[ix];
                let angle = (*seed >> 8) as f32 * 0.01;
                let p =
                    center.plus(Vec2::new(angle.cos(), angle.sin() * 0.68).scale(radius * 0.35));
                if self.sunken & (1 << ix) == 0 && p.minus(avoid).length() > 29.0 {
                    return p;
                }
            }
        }
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
