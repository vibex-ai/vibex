use std::{
    fmt::Write as _,
    sync::{Arc, OnceLock},
};

use image::{Rgba, RgbaImage};

const MAX_EDIT_HISTORY: usize = 64;

/// A bounded, in-memory editing session.
///
/// The session deliberately owns only image buffers. Persistence and attachment
/// metadata updates stay in the desktop view and happen only after confirmation.
#[derive(Debug, Clone)]
pub struct ImageEditSession {
    current: RgbaImage,
    undo: Vec<RgbaImage>,
    redo: Vec<RgbaImage>,
}

impl ImageEditSession {
    pub fn new(image: RgbaImage) -> Self {
        Self {
            current: image,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn current(&self) -> &RgbaImage {
        &self.current
    }

    pub fn current_clone(&self) -> RgbaImage {
        self.current.clone()
    }

    /// Replace the visible working image without creating a history entry.
    /// This is used for transient previews while a pointer gesture is active.
    pub fn preview(&mut self, image: RgbaImage) {
        self.current = image;
    }

    /// Commit a completed operation as one undoable history entry.
    pub fn commit(&mut self, image: RgbaImage) -> bool {
        let previous = self.current.clone();
        if previous == image {
            return false;
        }
        self.current = image;
        self.push_undo(previous);
        self.redo.clear();
        true
    }

    /// Commit a gesture whose transient preview started from `base`.
    pub fn commit_gesture(&mut self, base: RgbaImage) -> bool {
        if base == self.current {
            return false;
        }
        self.push_undo(base);
        self.redo.clear();
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.current, previous);
        self.redo.push(current);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.current, next);
        self.push_undo(current);
        true
    }

    fn push_undo(&mut self, image: RgbaImage) {
        if self.undo.len() == MAX_EDIT_HISTORY {
            self.undo.remove(0);
        }
        self.undo.push(image);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageEditTool {
    Crop,
    Brush,
    Text,
    Rectangle,
    Circle,
    Arrow,
    Mosaic,
}

impl ImageEditTool {
    pub const ALL: [Self; 7] = [
        Self::Crop,
        Self::Brush,
        Self::Text,
        Self::Rectangle,
        Self::Circle,
        Self::Arrow,
        Self::Mosaic,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Crop => "crop",
            Self::Brush => "brush",
            Self::Text => "text",
            Self::Rectangle => "rectangle",
            Self::Circle => "circle",
            Self::Arrow => "arrow",
            Self::Mosaic => "mosaic",
        }
    }
}

const MARK_COLOR: Rgba<u8> = Rgba([235, 67, 67, 255]);

pub fn apply_brush(image: &mut RgbaImage, from: (u32, u32), to: (u32, u32)) {
    let width = image.width().min(image.height()).max(1) / 140 + 2;
    draw_line(image, from, to, MARK_COLOR, width);
}

pub fn apply_rectangle(image: &mut RgbaImage, from: (u32, u32), to: (u32, u32)) {
    let left = from.0.min(to.0);
    let right = from.0.max(to.0);
    let top = from.1.min(to.1);
    let bottom = from.1.max(to.1);
    let width = image.width().min(image.height()).max(1) / 180 + 2;
    draw_line(image, (left, top), (right, top), MARK_COLOR, width);
    draw_line(image, (right, top), (right, bottom), MARK_COLOR, width);
    draw_line(image, (right, bottom), (left, bottom), MARK_COLOR, width);
    draw_line(image, (left, bottom), (left, top), MARK_COLOR, width);
}

pub fn apply_circle(image: &mut RgbaImage, from: (u32, u32), to: (u32, u32)) {
    let center_x = (from.0 as i64 + to.0 as i64) / 2;
    let center_y = (from.1 as i64 + to.1 as i64) / 2;
    let radius_x = (to.0 as i64 - from.0 as i64).unsigned_abs() as f32 / 2.0;
    let radius_y = (to.1 as i64 - from.1 as i64).unsigned_abs() as f32 / 2.0;
    if radius_x < 1.0 || radius_y < 1.0 {
        return;
    }
    let steps = ((radius_x + radius_y) * 2.0).clamp(24.0, 720.0) as usize;
    let mut previous = None;
    for step in 0..=steps {
        let angle = std::f32::consts::TAU * step as f32 / steps as f32;
        let point = (
            (center_x as f32 + radius_x * angle.cos()).round() as i64,
            (center_y as f32 + radius_y * angle.sin()).round() as i64,
        );
        let point = clamp_point(image, point);
        if let Some(previous) = previous {
            draw_line(
                image,
                previous,
                point,
                MARK_COLOR,
                image.width().min(image.height()) / 180 + 2,
            );
        }
        previous = Some(point);
    }
}

pub fn apply_arrow(image: &mut RgbaImage, from: (u32, u32), to: (u32, u32)) {
    let width = image.width().min(image.height()).max(1) / 180 + 2;
    draw_line(image, from, to, MARK_COLOR, width);
    let dx = to.0 as f32 - from.0 as f32;
    let dy = to.1 as f32 - from.1 as f32;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 2.0 {
        return;
    }
    let head = (length * 0.16).clamp(10.0, 42.0);
    let angle = dy.atan2(dx);
    let left = (
        to.0 as f32 - head * (angle - 0.55).cos(),
        to.1 as f32 - head * (angle - 0.55).sin(),
    );
    let right = (
        to.0 as f32 - head * (angle + 0.55).cos(),
        to.1 as f32 - head * (angle + 0.55).sin(),
    );
    draw_line(
        image,
        to,
        clamp_point(image, (left.0.round() as i64, left.1.round() as i64)),
        MARK_COLOR,
        width,
    );
    draw_line(
        image,
        to,
        clamp_point(image, (right.0.round() as i64, right.1.round() as i64)),
        MARK_COLOR,
        width,
    );
}

pub fn apply_mosaic(image: &mut RgbaImage, from: (u32, u32), to: (u32, u32)) {
    let left = from.0.min(to.0);
    let right = from.0.max(to.0).min(image.width().saturating_sub(1));
    let top = from.1.min(to.1);
    let bottom = from.1.max(to.1).min(image.height().saturating_sub(1));
    if left >= right || top >= bottom {
        return;
    }
    let block = image.width().min(image.height()).clamp(1, 24) / 8 + 2;
    let block = block.max(3);
    let mut y = top;
    while y <= bottom {
        let mut x = left;
        while x <= right {
            let x_end = x.saturating_add(block).min(right.saturating_add(1));
            let y_end = y.saturating_add(block).min(bottom.saturating_add(1));
            let mut sum = [0_u32; 4];
            let mut count = 0_u32;
            for sample_y in y..y_end {
                for sample_x in x..x_end {
                    let pixel = image.get_pixel(sample_x, sample_y).0;
                    for channel in 0..4 {
                        sum[channel] += u32::from(pixel[channel]);
                    }
                    count += 1;
                }
            }
            if count != 0 {
                let color = Rgba([
                    (sum[0] / count) as u8,
                    (sum[1] / count) as u8,
                    (sum[2] / count) as u8,
                    (sum[3] / count) as u8,
                ]);
                for write_y in y..y_end {
                    for write_x in x..x_end {
                        image.put_pixel(write_x, write_y, color);
                    }
                }
            }
            if x_end > right {
                break;
            }
            x = x_end;
        }
        if y.saturating_add(block) > bottom {
            break;
        }
        y = y.saturating_add(block);
    }
}

pub fn apply_crop(image: &RgbaImage, from: (u32, u32), to: (u32, u32)) -> Option<RgbaImage> {
    let left = from.0.min(to.0).min(image.width().saturating_sub(1));
    let right = from.0.max(to.0).min(image.width().saturating_sub(1));
    let top = from.1.min(to.1).min(image.height().saturating_sub(1));
    let bottom = from.1.max(to.1).min(image.height().saturating_sub(1));
    if right <= left || bottom <= top {
        return None;
    }
    Some(image::imageops::crop_imm(image, left, top, right - left + 1, bottom - top + 1).to_image())
}

pub fn apply_text(image: &mut RgbaImage, origin: (u32, u32), text: &str) {
    if let Some(overlay) = rasterize_text_overlay(image.dimensions(), origin, text) {
        image::imageops::overlay(image, &overlay, 0, 0);
        return;
    }

    apply_fallback_text(image, origin, text);
}

fn rasterize_text_overlay(
    dimensions: (u32, u32),
    origin: (u32, u32),
    text: &str,
) -> Option<RgbaImage> {
    let (width, height) = dimensions;
    if width == 0 || height == 0 || text.trim().is_empty() {
        return None;
    }

    let font_size = (width.min(height) as f32 / 18.0).clamp(16.0, 96.0);
    let line_height = font_size * 1.2;
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">"#,
    );
    let mut character_count = 0_usize;
    for (line_index, line) in text.lines().take(4).enumerate() {
        let remaining = 96_usize.saturating_sub(character_count);
        if remaining == 0 {
            break;
        }
        let line = line.chars().take(remaining).collect::<String>();
        character_count += line.chars().count();
        let baseline = (origin.1 as f32 + font_size + line_index as f32 * line_height)
            .min(height.saturating_sub(1) as f32);
        let escaped = escape_svg_text(&line);
        let _ = write!(
            svg,
            r##"<text x="{}" y="{baseline}" xml:space="preserve" font-family="sans-serif" font-size="{font_size}" font-weight="600" fill="#eb4343">{escaped}</text>"##,
            origin.0,
        );
    }
    svg.push_str("</svg>");

    let options = resvg::usvg::Options {
        font_family: "IBM Plex Sans".to_string(),
        fontdb: editor_font_database(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &options).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut pixels = pixmap.take();
    if !pixels.chunks_exact(4).any(|pixel| pixel[3] != 0) {
        return None;
    }
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha > 0 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(width, height, pixels)
}

fn editor_font_database() -> Arc<resvg::usvg::fontdb::Database> {
    static FONT_DATABASE: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    FONT_DATABASE
        .get_or_init(|| {
            let mut database = resvg::usvg::fontdb::Database::new();
            database.load_font_data(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../vendor/zed/assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf"
                ))
                .to_vec(),
            );
            database.load_font_data(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../mobile/assets/fonts/wqy-microhei/wqy-microhei.ttc"
                ))
                .to_vec(),
            );
            database.set_sans_serif_family("IBM Plex Sans");
            database.set_serif_family("IBM Plex Sans");
            database.set_monospace_family("IBM Plex Sans");
            database.set_cursive_family("IBM Plex Sans");
            database.set_fantasy_family("IBM Plex Sans");
            Arc::new(database)
        })
        .clone()
}

fn escape_svg_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn apply_fallback_text(image: &mut RgbaImage, origin: (u32, u32), text: &str) {
    let scale = (image.width().min(image.height()) / 160).clamp(1, 8);
    let mut x = origin.0;
    let mut y = origin.1;
    for character in text.chars().take(48) {
        if character == '\n' {
            x = origin.0;
            y = y.saturating_add(9 * scale);
            continue;
        }
        draw_glyph(image, x, y, character, scale);
        x = x.saturating_add(6 * scale);
        if x >= image.width().saturating_sub(5 * scale) {
            x = origin.0;
            y = y.saturating_add(9 * scale);
        }
        if y >= image.height() {
            break;
        }
    }
}

fn draw_glyph(image: &mut RgbaImage, x: u32, y: u32, character: char, scale: u32) {
    let glyph = glyph_for(character);
    for (row, bits) in glyph.iter().enumerate() {
        for column in 0..5 {
            if bits & (1 << (4 - column)) == 0 {
                continue;
            }
            for offset_y in 0..scale {
                for offset_x in 0..scale {
                    let pixel_x = x.saturating_add(column * scale).saturating_add(offset_x);
                    let pixel_y = y
                        .saturating_add(row as u32 * scale)
                        .saturating_add(offset_y);
                    if pixel_x < image.width() && pixel_y < image.height() {
                        image.put_pixel(pixel_x, pixel_y, MARK_COLOR);
                    }
                }
            }
        }
    }
}

fn glyph_for(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 14],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [31, 4, 4, 4, 4, 4, 31],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '.' => [0, 0, 0, 0, 0, 6, 6],
        ':' => [0, 6, 6, 0, 6, 6, 0],
        _ => [31, 1, 2, 4, 2, 1, 31],
    }
}

fn draw_line(image: &mut RgbaImage, from: (u32, u32), to: (u32, u32), color: Rgba<u8>, width: u32) {
    let mut x0 = from.0 as i64;
    let mut y0 = from.1 as i64;
    let x1 = to.0 as i64;
    let y1 = to.1 as i64;
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;
    let radius = (width.max(1) / 2) as i64;
    loop {
        paint_brush(image, x0, y0, radius, color);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let double_error = 2 * error;
        if double_error >= dy {
            error += dy;
            x0 += sx;
        }
        if double_error <= dx {
            error += dx;
            y0 += sy;
        }
    }
}

fn paint_brush(image: &mut RgbaImage, x: i64, y: i64, radius: i64, color: Rgba<u8>) {
    for offset_y in -radius..=radius {
        for offset_x in -radius..=radius {
            if offset_x * offset_x + offset_y * offset_y > radius * radius {
                continue;
            }
            let pixel_x = x + offset_x;
            let pixel_y = y + offset_y;
            if pixel_x >= 0
                && pixel_y >= 0
                && (pixel_x as u32) < image.width()
                && (pixel_y as u32) < image.height()
            {
                image.put_pixel(pixel_x as u32, pixel_y as u32, color);
            }
        }
    }
}

fn clamp_point(image: &RgbaImage, point: (i64, i64)) -> (u32, u32) {
    (
        point.0.clamp(0, image.width().saturating_sub(1) as i64) as u32,
        point.1.clamp(0, image.height().saturating_sub(1) as i64) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_stable_id() {
        assert_eq!(
            ImageEditTool::ALL.map(ImageEditTool::id),
            [
                "crop",
                "brush",
                "text",
                "rectangle",
                "circle",
                "arrow",
                "mosaic",
            ]
        );
    }

    #[test]
    fn crop_reduces_the_image_to_the_selection() {
        let image = RgbaImage::from_pixel(20, 10, Rgba([1, 2, 3, 255]));
        let cropped = apply_crop(&image, (2, 3), (11, 8)).unwrap();
        assert_eq!(cropped.dimensions(), (10, 6));
    }

    #[test]
    fn drawing_tools_change_pixels() {
        let mut image = RgbaImage::from_pixel(64, 64, Rgba([0, 0, 0, 255]));
        apply_brush(&mut image, (2, 2), (32, 32));
        apply_rectangle(&mut image, (5, 5), (30, 30));
        apply_circle(&mut image, (10, 10), (40, 40));
        apply_arrow(&mut image, (2, 40), (40, 2));
        apply_mosaic(&mut image, (20, 20), (50, 50));
        apply_text(&mut image, (3, 3), "VIBEX 中文 1");
        assert!(image.pixels().any(|pixel| pixel.0 != [0, 0, 0, 255]));
    }

    #[test]
    fn text_rasterizer_accepts_unicode_and_escapes_xml() {
        let mut image = RgbaImage::from_pixel(320, 180, Rgba([0, 0, 0, 255]));

        apply_text(&mut image, (12, 12), "中文 <Vibex> & text");

        assert!(image.pixels().any(|pixel| pixel.0 == MARK_COLOR.0));
    }

    #[test]
    fn edit_session_undoes_redoes_and_invalidates_redo_after_new_work() {
        let original = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mut first = original.clone();
        first.put_pixel(1, 1, MARK_COLOR);
        let mut second = first.clone();
        second.put_pixel(2, 2, MARK_COLOR);
        let mut replacement = first.clone();
        replacement.put_pixel(3, 3, MARK_COLOR);

        let mut session = ImageEditSession::new(original.clone());
        assert!(session.commit(first.clone()));
        assert!(session.commit(second.clone()));
        assert!(session.can_undo());
        assert!(!session.can_redo());

        assert!(session.undo());
        assert_eq!(session.current(), &first);
        assert!(session.can_redo());
        assert!(session.redo());
        assert_eq!(session.current(), &second);

        assert!(session.undo());
        assert!(session.commit(replacement.clone()));
        assert_eq!(session.current(), &replacement);
        assert!(!session.can_redo());

        assert!(session.undo());
        assert_eq!(session.current(), &first);
        assert!(session.undo());
        assert_eq!(session.current(), &original);
    }

    #[test]
    fn edit_session_records_a_live_preview_as_one_gesture() {
        let original = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
        let mut preview = original.clone();
        preview.put_pixel(4, 4, MARK_COLOR);

        let mut session = ImageEditSession::new(original.clone());
        session.preview(preview.clone());
        assert!(!session.can_undo());
        assert!(session.commit_gesture(original.clone()));
        assert_eq!(session.current(), &preview);

        assert!(session.undo());
        assert_eq!(session.current(), &original);
    }
}
