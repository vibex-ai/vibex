//! Screenshot decoding and visual regression.
//!
//! Baselines live under the runtime's data directory by default. They are only
//! written into the user's workspace when the user explicitly asks for it, so a
//! screenshot never shows up in `git status` uninvited.
//!
//! Capture prerequisites are enforced by the caller (`Overlay.hideHighlight`,
//! a fixed viewport and `deviceScaleFactor`, `document.fonts.ready`, reduced
//! motion). A diff is only as meaningful as the capture is reproducible, so an
//! obviously blank capture is reported as not credible instead of as a
//! difference.

use image::{Rgba, RgbaImage};
use sha2::{Digest, Sha256};
use vibex_core::BrowserVisualDiff;

use crate::error::{BrowserError, BrowserResult};

/// Sampled unique colours below this count mean the capture is flat (blank,
/// solid or an unpainted frame) and must not be diffed.
pub const MIN_CREDIBLE_UNIQUE_COLORS: usize = 16;
/// Per-channel tolerance before a pixel counts as changed. Anti-aliasing and
/// font rasterisation differ between runs even on identical input.
pub const DEFAULT_CHANNEL_TOLERANCE: u8 = 12;
/// Side length of the sampling grid used for the coarse diff pass.
pub const SAMPLING_STRIDE: u32 = 4;
/// Fraction of changed pixels above which a capture is reported as different at
/// all. Below this the diff is treated as noise.
pub const NOISE_RATIO: f64 = 0.0005;

/// SHA-256 over the raw RGBA bytes.
///
/// Used as a fast pre-filter: identical hashes mean the pixel diff can be
/// skipped entirely.
pub fn rgba_sha256(image: &RgbaImage) -> String {
    let mut hasher = Sha256::new();
    hasher.update(image.width().to_le_bytes());
    hasher.update(image.height().to_le_bytes());
    hasher.update(image.as_raw());
    format!("{:x}", hasher.finalize())
}

/// Counts distinct colours on a sampled grid.
pub fn sampled_unique_colors(image: &RgbaImage) -> usize {
    use std::collections::HashSet;
    let mut colors = HashSet::new();
    let stride = SAMPLING_STRIDE.max(1);
    for y in (0..image.height()).step_by(stride as usize) {
        for x in (0..image.width()).step_by(stride as usize) {
            let pixel = image.get_pixel(x, y);
            colors.insert([pixel[0], pixel[1], pixel[2]]);
        }
    }
    colors.len()
}

/// True when a capture looks like a real render rather than a blank surface.
pub fn capture_is_credible(image: &RgbaImage) -> bool {
    sampled_unique_colors(image) >= MIN_CREDIBLE_UNIQUE_COLORS
}

/// Decodes PNG or JPEG bytes into RGBA.
pub fn decode_image(bytes: &[u8]) -> BrowserResult<RgbaImage> {
    let decoded = image::load_from_memory(bytes).map_err(|error| {
        BrowserError::validation(
            "browser_image_decode_failed",
            "the captured image could not be decoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(decoded.to_rgba8())
}

/// Encodes RGBA as PNG.
pub fn encode_png(image: &RgbaImage) -> BrowserResult<Vec<u8>> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buffer, image::ImageFormat::Png)
        .map_err(|error| {
            BrowserError::process(
                "browser_image_encode_failed",
                "the screenshot could not be encoded as PNG",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    Ok(buffer.into_inner())
}

/// Compares a capture against a baseline.
///
/// Returns `None` when the two images are byte-identical, in which case no
/// pixel work is needed at all.
pub fn diff_against_baseline(
    baseline_key: &str,
    baseline: &RgbaImage,
    capture: &RgbaImage,
    tolerance: u8,
) -> Option<BrowserVisualDiff> {
    if baseline.dimensions() == capture.dimensions()
        && rgba_sha256(baseline) == rgba_sha256(capture)
    {
        return Some(BrowserVisualDiff {
            baseline_key: baseline_key.to_string(),
            width: capture.width(),
            height: capture.height(),
            size_changed: false,
            changed_ratio: 0.0,
            changed_regions: 0,
            identical: true,
            capture_not_credible: false,
        });
    }

    let credible = capture_is_credible(capture);
    let size_changed = baseline.dimensions() != capture.dimensions();
    let (width, height) = (
        baseline.width().min(capture.width()),
        baseline.height().min(capture.height()),
    );
    if width == 0 || height == 0 {
        return Some(BrowserVisualDiff {
            baseline_key: baseline_key.to_string(),
            width,
            height,
            size_changed,
            changed_ratio: 1.0,
            changed_regions: 0,
            identical: false,
            capture_not_credible: true,
        });
    }

    let stride = SAMPLING_STRIDE.max(1);
    let mut sampled = 0u64;
    let mut changed = 0u64;
    // A coarse mask groups neighbouring changed pixels into regions so the
    // report talks about areas, not individual noisy pixels.
    let mask_width = width.div_ceil(stride);
    let mask_height = height.div_ceil(stride);
    let mut mask = vec![false; (mask_width * mask_height) as usize];

    for y in (0..height).step_by(stride as usize) {
        for x in (0..width).step_by(stride as usize) {
            sampled += 1;
            let left = baseline.get_pixel(x, y);
            let right = capture.get_pixel(x, y);
            if channel_distance(left, right) > tolerance {
                changed += 1;
                let index = (y / stride) * mask_width + (x / stride);
                if let Some(slot) = mask.get_mut(index as usize) {
                    *slot = true;
                }
            }
        }
    }

    let changed_ratio = if sampled == 0 {
        0.0
    } else {
        changed as f64 / sampled as f64
    };

    Some(BrowserVisualDiff {
        baseline_key: baseline_key.to_string(),
        width,
        height,
        size_changed,
        changed_ratio,
        changed_regions: count_regions(&mask, mask_width, mask_height),
        identical: false,
        capture_not_credible: !credible,
    })
}

fn channel_distance(left: &Rgba<u8>, right: &Rgba<u8>) -> u8 {
    let difference = |a: u8, b: u8| a.abs_diff(b);
    difference(left[0], right[0])
        .max(difference(left[1], right[1]))
        .max(difference(left[2], right[2]))
}

/// Counts 4-connected components in the coarse change mask.
fn count_regions(mask: &[bool], width: u32, height: u32) -> u32 {
    let mut visited = vec![false; mask.len()];
    let mut regions = 0u32;
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;
            if !mask[index] || visited[index] {
                continue;
            }
            regions += 1;
            let mut stack = vec![(x, y)];
            visited[index] = true;
            while let Some((cx, cy)) = stack.pop() {
                let neighbours = [
                    (cx.wrapping_sub(1), cy),
                    (cx + 1, cy),
                    (cx, cy.wrapping_sub(1)),
                    (cx, cy + 1),
                ];
                for (nx, ny) in neighbours {
                    if nx >= width || ny >= height {
                        continue;
                    }
                    let neighbour = (ny * width + nx) as usize;
                    if mask[neighbour] && !visited[neighbour] {
                        visited[neighbour] = true;
                        stack.push((nx, ny));
                    }
                }
            }
        }
    }
    regions
}

/// True when the diff is meaningful enough to report as a change.
pub fn diff_is_significant(diff: &BrowserVisualDiff) -> bool {
    !diff.identical
        && (diff.size_changed
            || diff.changed_ratio > NOISE_RATIO
            || diff.changed_regions > 0 && diff.changed_ratio > 0.0)
}

/// Renders a human-readable summary of a diff, for tool output.
pub fn describe_diff(diff: &BrowserVisualDiff) -> String {
    if diff.identical {
        return format!("`{}` is byte-identical to the baseline.", diff.baseline_key);
    }
    if diff.capture_not_credible {
        return format!(
            "The new capture for `{}` looks blank or flat (fewer than {} distinct colours); it was \
             probably taken before the page painted. Wait for the page to settle and capture again.",
            diff.baseline_key, MIN_CREDIBLE_UNIQUE_COLORS
        );
    }
    let mut summary = format!(
        "`{}`: {:.2}% of sampled pixels changed across {} region(s) ({}×{}).",
        diff.baseline_key,
        diff.changed_ratio * 100.0,
        diff.changed_regions,
        diff.width,
        diff.height
    );
    if diff.size_changed {
        summary.push_str(" The capture dimensions differ from the baseline.");
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba(color))
    }

    fn noisy(width: u32, height: u32) -> RgbaImage {
        let mut image = RgbaImage::new(width, height);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = Rgba([
                ((x * 7) % 256) as u8,
                ((y * 11) % 256) as u8,
                ((x + y) % 256) as u8,
                255,
            ]);
        }
        image
    }

    #[test]
    fn identical_captures_short_circuit_the_diff() {
        let baseline = noisy(64, 64);
        let diff = diff_against_baseline("home", &baseline, &baseline.clone(), 0).unwrap();
        assert!(diff.identical);
        assert_eq!(diff.changed_ratio, 0.0);
        assert_eq!(diff.changed_regions, 0);
    }

    #[test]
    fn hashing_is_stable_and_size_sensitive() {
        let image = noisy(32, 32);
        assert_eq!(rgba_sha256(&image), rgba_sha256(&image.clone()));
        assert_ne!(rgba_sha256(&image), rgba_sha256(&noisy(32, 33)));
    }

    #[test]
    fn flat_captures_are_not_credible() {
        assert!(!capture_is_credible(&solid(64, 64, [255, 255, 255, 255])));
        assert!(capture_is_credible(&noisy(64, 64)));
    }

    #[test]
    fn a_blank_capture_is_reported_as_not_credible_rather_than_as_a_change() {
        let baseline = noisy(64, 64);
        let capture = solid(64, 64, [255, 255, 255, 255]);
        let diff =
            diff_against_baseline("home", &baseline, &capture, DEFAULT_CHANNEL_TOLERANCE).unwrap();
        assert!(diff.capture_not_credible);
        assert!(!diff.identical);
        assert!(describe_diff(&diff).contains("blank or flat"));
    }

    #[test]
    fn tolerance_absorbs_antialiasing_level_noise() {
        let baseline = solid(32, 32, [100, 100, 100, 255]);
        let slightly_off = solid(32, 32, [105, 103, 108, 255]);
        let diff =
            diff_against_baseline("home", &baseline, &slightly_off, DEFAULT_CHANNEL_TOLERANCE)
                .unwrap();
        assert_eq!(diff.changed_ratio, 0.0);
        assert_eq!(diff.changed_regions, 0);
    }

    #[test]
    fn a_real_change_is_detected_and_grouped_into_regions() {
        let baseline = noisy(128, 128);
        let mut capture = baseline.clone();
        for y in 10..40 {
            for x in 10..40 {
                capture.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        for y in 80..90 {
            for x in 90..100 {
                capture.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let diff =
            diff_against_baseline("home", &baseline, &capture, DEFAULT_CHANNEL_TOLERANCE).unwrap();
        assert!(!diff.identical);
        assert!(diff.changed_ratio > 0.0);
        assert_eq!(diff.changed_regions, 2);
        assert!(diff_is_significant(&diff));
    }

    #[test]
    fn size_changes_are_flagged() {
        let baseline = noisy(64, 64);
        let capture = noisy(64, 96);
        let diff =
            diff_against_baseline("home", &baseline, &capture, DEFAULT_CHANNEL_TOLERANCE).unwrap();
        assert!(diff.size_changed);
        assert!(diff_is_significant(&diff));
        assert!(describe_diff(&diff).contains("dimensions differ"));
    }

    #[test]
    fn png_round_trips_through_decode() {
        let image = noisy(24, 24);
        let encoded = encode_png(&image).unwrap();
        let decoded = decode_image(&encoded).unwrap();
        assert_eq!(decoded.dimensions(), image.dimensions());
        assert_eq!(rgba_sha256(&decoded), rgba_sha256(&image));
    }

    #[test]
    fn undecodable_bytes_produce_a_typed_error() {
        let error = decode_image(b"not an image").unwrap_err();
        assert_eq!(error.code, "browser_image_decode_failed");
    }

    #[test]
    fn region_counting_handles_an_empty_mask() {
        assert_eq!(count_regions(&[], 0, 0), 0);
    }
}
