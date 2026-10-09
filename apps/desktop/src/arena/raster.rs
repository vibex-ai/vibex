//! Palette-indexed pixel art, painted as merged rectangles on one canvas.
//! Integer raster coordinates never pass through a font or filtered image sampler.

pub(super) mod ink {
    pub const CLEAR: u8 = 0;
    pub const OUTLINE: u8 = 1;
    pub const DEPTH: u8 = 2;
    pub const FLOOR_SHADE: u8 = 3;
    pub const FLOOR: u8 = 4;
    pub const FLOOR_LIGHT: u8 = 5;
    pub const SEAM: u8 = 6;
    pub const WALL: u8 = 7;
    pub const WALL_LIGHT: u8 = 8;
    pub const MOSS_DARK: u8 = 9;
    pub const MOSS: u8 = 10;
    pub const MOSS_LIGHT: u8 = 11;
    pub const GRASS: u8 = 12;
    pub const BODY_SHADOW: u8 = 13;
    pub const BODY_DARK: u8 = 14;
    pub const BODY: u8 = 15;
    pub const BODY_LIGHT: u8 = 16;
    pub const BODY_GLEAM: u8 = 17;
    pub const GOLD: u8 = 18;
    pub const GOLD_DARK: u8 = 19;
    pub const CORE_DARK: u8 = 20;
    pub const CORE: u8 = 21;
    pub const CORE_LIGHT: u8 = 22;
    pub const WHITE: u8 = 23;
    pub const SKIN_DARK: u8 = 24;
    pub const SKIN: u8 = 25;
    pub const HAIR: u8 = 26;
    pub const HAIR_LIGHT: u8 = 27;
    pub const CLOTH_DARK: u8 = 28;
    pub const CLOTH: u8 = 29;
    pub const CLOTH_LIGHT: u8 = 30;
    pub const CAPE: u8 = 31;
    pub const CAPE_LIGHT: u8 = 32;
    pub const SHADOW: u8 = 33;
    pub const WATER_DARK: u8 = 34;
    pub const WATER: u8 = 35;
    pub const WATER_LIGHT: u8 = 36;
    pub const FIRE: u8 = 37;
    pub const WARN: u8 = 38;
    pub const WARN_DARK: u8 = 39;
    pub const DUST: u8 = 40;
    pub const IVORY: u8 = 41;
    pub const LEAF: u8 = 42;
    pub const COUNT: usize = 43;
}

#[derive(Clone)]
pub(super) struct Raster {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PixelRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub ink: u8,
}

impl Raster {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![ink::CLEAR; width * height],
        }
    }

    pub fn get(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return ink::CLEAR;
        }
        self.pixels[y as usize * self.width + x as usize]
    }

    pub fn put(&mut self, x: i32, y: i32, ink: u8) {
        if x >= 0 && y >= 0 && x < self.width as i32 && y < self.height as i32 {
            self.pixels[y as usize * self.width + x as usize] = ink;
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, ink: u8) {
        let left = x.clamp(0, self.width as i32) as usize;
        let right = (x + width).clamp(0, self.width as i32) as usize;
        if left >= right {
            return;
        }
        for row in y.max(0)..(y + height).min(self.height as i32) {
            self.pixels[row as usize * self.width + left..row as usize * self.width + right]
                .fill(ink);
        }
    }

    pub fn line(&mut self, from: (i32, i32), to: (i32, i32), ink: u8) {
        let (mut x, mut y) = from;
        let dx = (to.0 - x).abs();
        let dy = -(to.1 - y).abs();
        let sx = if x < to.0 { 1 } else { -1 };
        let sy = if y < to.1 { 1 } else { -1 };
        let mut error = dx + dy;
        loop {
            self.put(x, y, ink);
            if (x, y) == to {
                break;
            }
            let doubled = error * 2;
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
        }
    }

    pub fn ellipse(&mut self, center: (i32, i32), radii: (i32, i32), ink: u8) {
        let (rx, ry) = radii;
        if rx <= 0 || ry <= 0 {
            return;
        }
        for y in -ry..=ry {
            let half = (rx as f32 * (1.0 - (y * y) as f32 / (ry * ry) as f32).max(0.0).sqrt())
                .round() as i32;
            self.rect(center.0 - half, center.1 + y, half * 2 + 1, 1, ink);
        }
    }

    pub fn ring(&mut self, center: (i32, i32), radii: (i32, i32), ink: u8, dashed: bool) {
        let steps = ((radii.0.max(radii.1) * 7) as usize).clamp(16, 1600);
        let mut previous = None;
        for ix in 0..=steps {
            let angle = std::f32::consts::TAU * ix as f32 / steps as f32;
            let point = (
                center.0 + (angle.cos() * radii.0 as f32).round() as i32,
                center.1 + (angle.sin() * radii.1 as f32).round() as i32,
            );
            if (!dashed || ix / 5 % 3 != 0)
                && let Some(previous) = previous
            {
                self.line(previous, point, ink);
            }
            previous = Some(point);
        }
    }

    pub fn polygon(&mut self, points: &[(i32, i32)], ink: u8) {
        let top = points.iter().map(|p| p.1).min().unwrap_or(0).max(0);
        let bottom = points
            .iter()
            .map(|p| p.1)
            .max()
            .unwrap_or(0)
            .min(self.height as i32 - 1);
        let mut intersections = Vec::with_capacity(points.len());
        for y in top..=bottom {
            intersections.clear();
            for (a, b) in points.iter().zip(points.iter().cycle().skip(1)) {
                if (a.1 <= y && b.1 > y) || (b.1 <= y && a.1 > y) {
                    intersections.push(a.0 + (y - a.1) * (b.0 - a.0) / (b.1 - a.1));
                }
            }
            intersections.sort_unstable();
            for pair in intersections.chunks_exact(2) {
                self.rect(pair[0], y, pair[1] - pair[0] + 1, 1, ink);
            }
        }
    }

    pub fn blit(&mut self, sprite: &Self, left: i32, top: i32) {
        for y in 0..sprite.height as i32 {
            for x in 0..sprite.width as i32 {
                let color = sprite.get(x, y);
                if color != ink::CLEAR {
                    self.put(left + x, top + y, color);
                }
            }
        }
    }

    pub fn transformed(&mut self, sprite: &Self, center: (i32, i32), angle: f32, visibility: f32) {
        let radius = (sprite.width.max(sprite.height) as f32 * 0.72).ceil() as i32;
        let (sin, cos) = angle.sin_cos();
        for y in -radius..=radius {
            for x in -radius..=radius {
                if !dither(x, y, visibility) {
                    continue;
                }
                let sx =
                    (x as f32 * cos + y as f32 * sin + (sprite.width / 2) as f32).round() as i32;
                let sy =
                    (-x as f32 * sin + y as f32 * cos + (sprite.height / 2) as f32).round() as i32;
                let color = sprite.get(sx, sy);
                if color != ink::CLEAR {
                    self.put(center.0 + x, center.1 + y, color);
                }
            }
        }
    }

    /// Merge horizontal runs with identical runs on the previous row. Work and
    /// memory stay linear in raster size, including large flat backgrounds.
    pub fn rectangles(&self) -> Vec<PixelRect> {
        let mut rectangles: Vec<PixelRect> = Vec::with_capacity(self.width * 12);
        let mut previous: Vec<(usize, usize, u8, usize)> = Vec::new();
        let mut current = Vec::new();
        for y in 0..self.height {
            current.clear();
            let mut x = 0;
            let mut previous_ix = 0;
            while x < self.width {
                let color = self.pixels[y * self.width + x];
                let start = x;
                while x < self.width && self.pixels[y * self.width + x] == color {
                    x += 1;
                }
                if color == ink::CLEAR {
                    continue;
                }
                while previous_ix < previous.len() && previous[previous_ix].0 < start {
                    previous_ix += 1;
                }
                let matching = previous
                    .get(previous_ix)
                    .filter(|p| p.0 == start && p.1 == x && p.2 == color);
                let ix = if let Some(previous) = matching {
                    rectangles[previous.3].height += 1;
                    previous.3
                } else {
                    rectangles.push(PixelRect {
                        x: start as u16,
                        y: y as u16,
                        width: (x - start) as u16,
                        height: 1,
                        ink: color,
                    });
                    rectangles.len() - 1
                };
                current.push((start, x, color, ix));
            }
            std::mem::swap(&mut previous, &mut current);
        }
        rectangles
    }
}

pub(super) fn hash(x: i32, y: i32) -> u32 {
    let mut value = (x as u32)
        .wrapping_mul(374_761_393)
        .wrapping_add((y as u32).wrapping_mul(668_265_263));
    value = (value ^ (value >> 13)).wrapping_mul(1_274_126_177);
    value ^ (value >> 16)
}

pub(super) fn dither(x: i32, y: i32, visibility: f32) -> bool {
    const BAYER: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
    f32::from(BAYER[(y.rem_euclid(4) * 4 + x.rem_euclid(4)) as usize]) < visibility * 16.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_transforms_preserve_odd_sprites_and_single_pixels() {
        for width in [1, 3, 4, 13] {
            let mut sprite = Raster::new(width, 3);
            sprite.rect(0, 0, width as i32, 3, ink::BODY);
            sprite.put(0, 1, ink::BODY_LIGHT);
            sprite.put(width as i32 - 1, 2, ink::BODY_DARK);
            let mut placed = Raster::new(24, 24);
            placed.transformed(&sprite, (12, 12), 0.0, 1.0);
            for y in 0..3 {
                for x in 0..width as i32 {
                    assert_eq!(
                        placed.get(12 - (width / 2) as i32 + x, 11 + y),
                        sprite.get(x, y)
                    );
                }
            }
            assert_eq!(
                placed
                    .pixels
                    .iter()
                    .filter(|ink| **ink != ink::CLEAR)
                    .count(),
                width * 3
            );
        }
    }
}
