//! Contact sheets for reviewing guardian art by eye. Ignored by default:
//!
//! ```sh
//! VIBEX_ARENA_SHEETS=/tmp/sheets cargo test -p vibex-desktop --lib --locked \
//!     arena::art::sheets -- --ignored
//! ```
//!
//! `VIBEX_ARENA_GUARDIAN=cursor` limits the sheets to one guardian's key.

use super::*;
use crate::arena::{
    combat::{Arena, Controls, STEP},
    gait, palette,
};
use image::{Rgba, RgbaImage, imageops};

fn colors(guardian: Guardian) -> Vec<Rgba<u8>> {
    palette::material_colors(guardian)
        .iter()
        .map(|color| {
            let rgba: gpui::Rgba = (*color).into();
            Rgba([
                (rgba.r * 255.0) as u8,
                (rgba.g * 255.0) as u8,
                (rgba.b * 255.0) as u8,
                255,
            ])
        })
        .collect()
}

/// `(left, top)` is the raster coordinate of the image's top-left corner.
fn paint(
    image: &mut RgbaImage,
    layers: &[&Raster],
    colors: &[Rgba<u8>],
    left: i32,
    top: i32,
    background: Rgba<u8>,
) {
    for y in 0..image.height() as i32 {
        for x in 0..image.width() as i32 {
            let mut color = background;
            for layer in layers {
                let ink = layer.get(left + x, top + y);
                if ink != CLEAR {
                    color = colors[ink as usize];
                }
            }
            image.put_pixel(x as u32, y as u32, color);
        }
    }
}

fn tile(boss: &Boss, time: f32, colors: &[Rgba<u8>], floor: Option<&Raster>) -> RgbaImage {
    let frame = idle(boss, time, false);
    let (x, y) = pixel(boss.position);
    let mut image = RgbaImage::new(240, 240);
    let layers: Vec<&Raster> = floor.into_iter().chain([&frame]).collect();
    paint(
        &mut image,
        &layers,
        colors,
        x - 120,
        y - 170,
        Rgba([40, 44, 52, 255]),
    );
    image
}

#[test]
#[ignore = "writes contact sheets for review"]
fn sheets() {
    let Ok(dir) = std::env::var("VIBEX_ARENA_SHEETS") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let only = std::env::var("VIBEX_ARENA_GUARDIAN").ok();
    for guardian in Guardian::ALL {
        if only.as_deref().is_some_and(|key| key != guardian.key()) {
            continue;
        }
        let colors = colors(guardian);
        let scale = 2;
        let cell = 240 * scale;
        // Home roaming, at an eighth of the loop apart, over the floor.
        let mut sheet = RgbaImage::new(cell * 8, cell * 3);
        for n in 0..8 {
            let time = n as f32 * crate::arena::guardian::IDLE_LOOP / 8.0;
            let boss = Boss::idle(guardian, time, false);
            let image = tile(&boss, time, &colors, Some(floor(guardian)));
            let image = imageops::resize(&image, cell, cell, imageops::FilterType::Nearest);
            imageops::overlay(&mut sheet, &image, (n * cell) as i64, 0);
        }
        // The lane: entrance, a pass, and an exit, without the floor.
        let lap = gait::lane_lap(guardian);
        let entrance = gait::entrance(guardian);
        let times: Vec<(f32, bool)> = (0..8)
            .map(|n| (-entrance + entrance * n as f32 / 7.0, false))
            .chain((0..8).map(|n| (n as f32 * lap / 8.0, false)))
            .collect();
        for (ix, (time, _)) in times.iter().enumerate() {
            let shot = gait::shot(guardian, *time);
            let image = tile(&shot.boss, *time, &colors, None);
            let image = imageops::resize(&image, cell, cell, imageops::FilterType::Nearest);
            let row = 1 + ix / 8;
            imageops::overlay(
                &mut sheet,
                &image,
                ((ix % 8) as u32 * cell) as i64,
                (row as u32 * cell) as i64,
            );
        }
        sheet
            .save(format!("{dir}/{}-models.png", guardian.key()))
            .unwrap();
        // Battle frames with an idle archer, a second apart.
        let mut arena = Arena::new(guardian, 0.0);
        let mut battle = RgbaImage::new(WIDTH as u32 * 4, HEIGHT as u32 * 2);
        for n in 0..8 {
            let frame = super::battle(&arena, false);
            let mut image = RgbaImage::new(WIDTH as u32, HEIGHT as u32);
            paint(
                &mut image,
                &[floor(guardian), &frame],
                &colors,
                0,
                -TOP_PAD,
                Rgba([0, 0, 0, 255]),
            );
            imageops::overlay(
                &mut battle,
                &image,
                ((n % 4) * WIDTH) as i64,
                ((n / 4) * HEIGHT) as i64,
            );
            for _ in 0..(1.0 / STEP) as usize {
                arena.tick(Controls::default());
            }
        }
        battle
            .save(format!("{dir}/{}-battle.png", guardian.key()))
            .unwrap();
    }
}

/// Prints the raster window every idle, lane, entrance and exit pose of the
/// guardians fits in, for authoring `COMPANION_BOXES`.
#[test]
#[ignore = "prints companion windows for authoring"]
fn windows() {
    for guardian in Guardian::ALL {
        let mut poses = Vec::new();
        let mut time = 0.0;
        while time < crate::arena::guardian::IDLE_LOOP {
            poses.push((time, Boss::idle(guardian, time, false)));
            time += 0.1;
        }
        poses.push((0.0, Boss::idle(guardian, 0.0, true)));
        let mut time = -gait::entrance(guardian);
        while time < gait::lane_lap(guardian) {
            poses.push((time, gait::shot(guardian, time).boss));
            time += 0.05;
        }
        for from in [0.0, 4.4, 9.7] {
            let mut time = from;
            while time <= from + gait::exit(guardian) {
                poses.push((time, gait::leaving(guardian, from, time).boss));
                time += 0.05;
            }
        }
        let (mut left, mut top, mut right, mut bottom) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (time, boss) in &poses {
            let frame = idle(boss, *time, false);
            let (x, y) = pixel(boss.position);
            for (ix, ink) in frame.pixels.iter().enumerate() {
                if *ink == CLEAR || *ink == SHADOW {
                    continue;
                }
                let dx = (ix % frame.width) as i32 - x;
                let dy = (ix / frame.width) as i32 - (y + TOP_PAD);
                left = left.min(dx);
                right = right.max(dx);
                top = top.min(dy);
                bottom = bottom.max(dy);
            }
        }
        let mut heights = Vec::new();
        let mut time = 0.0;
        while time < gait::lane_lap(guardian) {
            let boss = Boss::lane(guardian, time);
            let frame = companion(&boss, time, false);
            let rows = (0..frame.height)
                .filter(|row| {
                    frame.pixels[row * frame.width..(row + 1) * frame.width]
                        .iter()
                        .any(|ink| *ink != CLEAR)
                })
                .collect::<Vec<_>>();
            heights.push((rows.last().unwrap() - rows.first().unwrap() + 1) as i32);
            time += 0.5;
        }
        heights.sort_unstable();
        eprintln!(
            "{guardian:?}: window ({}, {}, {}, {}), median height {}",
            left - 2,
            top - 2,
            right + 2,
            bottom + 2,
            heights[heights.len() / 2]
        );
    }
}
