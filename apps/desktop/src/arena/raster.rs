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
    offset_x: i32,
    offset_y: i32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PixelRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub ink: u8,
}

/// Depth storage is local to a sculpture's projected bounds. A small model
/// never needs a depth surface the size of the entire arena.
pub(super) struct DepthBuffer {
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    values: Vec<f32>,
}

impl DepthBuffer {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        let width = (right - left).max(0) as usize;
        let height = (bottom - top).max(0) as usize;
        Self {
            left,
            top,
            width,
            height,
            values: vec![f32::NEG_INFINITY; width * height],
        }
    }
}

impl Raster {
    pub fn new(width: usize, height: usize) -> Self {
        Self::with_offset(width, height, 0, 0)
    }

    pub fn with_offset(width: usize, height: usize, offset_x: i32, offset_y: i32) -> Self {
        Self {
            width,
            height,
            pixels: vec![ink::CLEAR; width * height],
            offset_x,
            offset_y,
        }
    }

    pub fn get(&self, x: i32, y: i32) -> u8 {
        let (x, y) = (x + self.offset_x, y + self.offset_y);
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return ink::CLEAR;
        }
        self.pixels[y as usize * self.width + x as usize]
    }

    pub fn put(&mut self, x: i32, y: i32, ink: u8) {
        let (x, y) = (x + self.offset_x, y + self.offset_y);
        if x >= 0 && y >= 0 && x < self.width as i32 && y < self.height as i32 {
            self.pixels[y as usize * self.width + x as usize] = ink;
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, ink: u8) {
        let (x, y) = (x + self.offset_x, y + self.offset_y);
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
        let top = points
            .iter()
            .map(|p| p.1)
            .min()
            .unwrap_or(0)
            .max(-self.offset_y);
        let bottom = points
            .iter()
            .map(|p| p.1)
            .max()
            .unwrap_or(0)
            .min(self.height as i32 - self.offset_y - 1);
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

    /// Interpolated depth keeps interwoven and overlapping faces correct even
    /// when no single back-to-front ordering exists for the whole polygons.
    pub fn polygon_depth(&mut self, points: &[(f32, f32, f32)], ink: u8, depth: &mut DepthBuffer) {
        if points.len() < 3 || depth.width == 0 || depth.height == 0 {
            return;
        }
        for edge in points[1..].windows(2) {
            let [a, b, c] = [points[0], edge[0], edge[1]];
            let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
            if area.abs() < 0.0001 {
                continue;
            }
            let left = (a.0.min(b.0).min(c.0).floor() as i32)
                .max(depth.left)
                .max(-self.offset_x);
            let right = (a.0.max(b.0).max(c.0).ceil() as i32)
                .min(depth.left + depth.width as i32)
                .min(self.width as i32 - self.offset_x);
            let top = (a.1.min(b.1).min(c.1).floor() as i32)
                .max(depth.top)
                .max(-self.offset_y);
            let bottom = (a.1.max(b.1).max(c.1).ceil() as i32)
                .min(depth.top + depth.height as i32)
                .min(self.height as i32 - self.offset_y);
            if left >= right || top >= bottom {
                continue;
            }
            let weight = |p: (f32, f32), u: (f32, f32, f32), v: (f32, f32, f32)| {
                ((v.0 - u.0) * (p.1 - u.1) - (v.1 - u.1) * (p.0 - u.0)) / area
            };
            let da = (b.1 - c.1) / area;
            let db = (c.1 - a.1) / area;
            for y in top..bottom {
                let sample = (left as f32 + 0.5, y as f32 + 0.5);
                let mut wa = weight(sample, b, c);
                let mut wb = weight(sample, c, a);
                let depth_ix =
                    (y - depth.top) as usize * depth.width + (left - depth.left) as usize;
                let pixel_ix =
                    (y + self.offset_y) as usize * self.width + (left + self.offset_x) as usize;
                let width = (right - left) as usize;
                for (stored, pixel) in depth.values[depth_ix..depth_ix + width]
                    .iter_mut()
                    .zip(&mut self.pixels[pixel_ix..pixel_ix + width])
                {
                    let wc = 1.0 - wa - wb;
                    if wa >= -0.0001 && wb >= -0.0001 && wc >= -0.0001 {
                        let z = wa * a.2 + wb * b.2 + wc * c.2;
                        if z >= *stored - 0.0001 {
                            *stored = z;
                            *pixel = ink;
                        }
                    }
                    wa += da;
                    wb += db;
                }
            }
        }
    }

    #[cfg(test)]
    pub fn blit(&mut self, sprite: &Self, left: i32, top: i32) {
        for (ix, color) in sprite.pixels.iter().enumerate() {
            if *color != ink::CLEAR {
                self.put(
                    left + (ix % sprite.width) as i32 - sprite.offset_x,
                    top + (ix / sprite.width) as i32 - sprite.offset_y,
                    *color,
                );
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
    fn intersecting_surfaces_occlude_per_pixel_in_either_draw_order() {
        let sloped = [(1., 1., 0.), (9., 1., 4.), (9., 9., 4.), (1., 9., 0.)];
        let flat = [(1., 1., 2.), (9., 1., 2.), (9., 9., 2.), (1., 9., 2.)];
        let render = |reverse| {
            let mut frame = Raster::new(12, 12);
            let mut depth = DepthBuffer::new(1, 1, 9, 9);
            let faces = if reverse {
                [(&flat, ink::GOLD), (&sloped, ink::BODY)]
            } else {
                [(&sloped, ink::BODY), (&flat, ink::GOLD)]
            };
            for (points, color) in faces {
                frame.polygon_depth(points, color, &mut depth);
            }
            frame
        };
        let frame = render(false);
        assert_eq!(frame.pixels, render(true).pixels);
        assert_eq!(frame.get(2, 4), ink::GOLD);
        assert_eq!(frame.get(7, 4), ink::BODY);
        for y in 1..9 {
            for x in 1..9 {
                assert_ne!(frame.get(x, y), ink::CLEAR);
            }
        }
    }

    #[test]
    fn raised_depth_surfaces_clip_to_the_raster_without_losing_the_top_offset() {
        let mut frame = Raster::with_offset(8, 12, 0, 4);
        let mut depth = DepthBuffer::new(-3, -8, 10, 5);
        frame.polygon_depth(
            &[(-3., -8., 1.), (10., -8., 1.), (10., 5., 1.), (-3., 5., 1.)],
            ink::IVORY,
            &mut depth,
        );
        assert_eq!(frame.get(0, -4), ink::IVORY);
        assert_eq!(frame.get(7, 4), ink::IVORY);
        assert_eq!(frame.get(7, 5), ink::CLEAR);
        frame.polygon_depth(&[(0., 0., 2.); 4], ink::GOLD, &mut depth);
        assert_eq!(frame.get(0, 0), ink::IVORY);
    }

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
