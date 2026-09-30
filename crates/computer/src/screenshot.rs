//! Screenshot budgets and the CLI screenshot file.
//!
//! Two different problems are solved here, and conflating them is how a retina
//! window ends up costing four hundred megabytes of JSON in one tool result.
//!
//! **The agent-facing budget.** A screenshot travels native → base64 → JSON →
//! helper → runtime → MCP. The ceiling is per encoded image, and the runtime
//! walks the edge length down until the encoder fits. The walk is expressed as
//! a plan rather than a loop that decodes and re-encodes here, because the
//! runtime deliberately never decodes an image: the engine encodes, and the
//! runtime decides whether what came back is acceptable or whether to ask again
//! at a smaller scale.
//!
//! **The CLI file.** A skill-driven agent runs a command and reads text. Inline
//! base64 in a CLI transcript is unreadable and enormous, so the CLI path
//! writes the image to a private temporary file and returns its path — the same
//! shape the reference implementations converged on. The directory is `0700`,
//! the file `0600`, the payload is stripped from the JSON, and the files expire.

use std::path::{Path, PathBuf};

use vibex_core::{
    COMPUTER_MAX_SCREENSHOT_BYTES, COMPUTER_SCREENSHOT_FILE_TTL_MS, COMPUTER_SCREENSHOT_MIN_SCALE,
    COMPUTER_SCREENSHOT_SCALE_STEP, COMPUTER_SCREENSHOT_START_EDGE,
};

/// What to do with an encoded screenshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetVerdict {
    /// The payload fits; report it at this scale.
    Accept { scale: f64 },
    /// The payload is too large; ask the engine again at this smaller scale.
    Retry { scale: f64 },
    /// The walk is exhausted. The caller reports the failure rather than
    /// sending an oversized payload: a screenshot that cannot fit is a
    /// degradation, and a silent oversized one breaks the model's context.
    Exhausted,
}

/// Plans the downscale walk for one encoded screenshot.
///
/// `scale` is what the engine reported it applied when it produced the payload;
/// `1.0` means "full size". The next scale is never above the current one.
pub fn plan(bytes: usize, width: u32, height: u32, scale: f64) -> BudgetVerdict {
    if bytes <= COMPUTER_MAX_SCREENSHOT_BYTES {
        return BudgetVerdict::Accept { scale };
    }
    let starting = if scale.is_finite() && scale > 0.0 {
        scale.min(1.0)
    } else {
        1.0
    };
    // The first step is the edge limit: a 4K window starts at 1280/max(w,h).
    let edge_scale = if width.max(height) > COMPUTER_SCREENSHOT_START_EDGE {
        f64::from(COMPUTER_SCREENSHOT_START_EDGE) / f64::from(width.max(height))
    } else {
        starting
    };
    let next = (edge_scale.min(starting)) * COMPUTER_SCREENSHOT_SCALE_STEP;
    if next < COMPUTER_SCREENSHOT_MIN_SCALE {
        return BudgetVerdict::Exhausted;
    }
    BudgetVerdict::Retry {
        scale: (next * 1000.0).round() / 1000.0,
    }
}

/// Converts a point in a downscaled screenshot back into desktop coordinates.
///
/// A model reading a coordinate off an image has no idea the runtime shrank it;
/// this is the conversion that keeps a click on the picture and a click by the
/// Agent in the same place.
pub fn screenshot_point_to_desktop(x: f64, y: f64, scale: f64) -> (f64, f64) {
    if !scale.is_finite() || scale <= 0.0 {
        return (x, y);
    }
    (x / scale, y / scale)
}

/// The private directory the CLI writes screenshots into.
pub fn screenshot_directory(home: &Path) -> PathBuf {
    home.join("computer-screenshots")
}

/// Result of writing a screenshot for the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenScreenshot {
    pub path: PathBuf,
    pub expires_at_ms: i64,
    pub byte_len: usize,
}

/// Writes base64 image data to a private file and returns its path.
///
/// The directory is created `0700`, the file is written `0600`, and a stale
/// entry is only ever removed when it is older than the TTL: a CLI run that is
/// still reading the file must not have it deleted underneath.
pub fn write_screenshot_file(
    home: &Path,
    stem: &str,
    base64: &str,
    now_ms: i64,
) -> std::io::Result<WrittenScreenshot> {
    let directory = screenshot_directory(home);
    std::fs::create_dir_all(&directory)?;
    restrict_directory(&directory)?;
    cleanup_expired(&directory, now_ms)?;
    let path = directory.join(format!("{}-screenshot.img", safe_stem(stem)));
    let decoded = decode_base64(base64).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the screenshot payload is not valid base64",
        )
    })?;
    write_private_file(&path, &decoded)?;
    Ok(WrittenScreenshot {
        path,
        expires_at_ms: now_ms + COMPUTER_SCREENSHOT_FILE_TTL_MS,
        byte_len: decoded.len(),
    })
}

/// Replaces inline base64 image fields in a CLI JSON payload with a path.
///
/// Returns `true` when something was replaced, so the caller knows whether to
/// mention the file in its text output.
pub fn strip_inline_screenshots(
    value: &mut serde_json::Value,
    path: &Path,
    expires_at_ms: i64,
) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    for key in ["data", "base64", "image"] {
        if let Some(field) = object.get_mut(key)
            && field.as_str().is_some_and(|text| !text.is_empty())
        {
            *field = serde_json::Value::Null;
            changed = true;
        }
    }
    if changed {
        object.insert(
            "path".to_string(),
            serde_json::Value::String(path.to_string_lossy().to_string()),
        );
        object.insert(
            "expiresAt".to_string(),
            serde_json::Value::Number(expires_at_ms.into()),
        );
    }
    changed
}

fn safe_stem(stem: &str) -> String {
    let cleaned: String = stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "computer".to_string()
    } else {
        cleaned.chars().take(64).collect()
    }
}

fn cleanup_expired(directory: &Path, now_ms: i64) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with("-screenshot.img") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(now_ms);
        if now_ms.saturating_sub(modified_ms) > COMPUTER_SCREENSHOT_FILE_TTL_MS {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_directory(directory: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_directory(_directory: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// Minimal base64 decoder for the CLI file path.
///
/// A dependency-free decoder keeps the CLI path free of an image stack: the
/// runtime never decodes pixels, only the transfer encoding.
pub(crate) fn decode_base64(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (index, byte) in TABLE.iter().enumerate() {
        lookup[*byte as usize] = index as u8;
    }
    let filtered: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if filtered.is_empty() {
        return None;
    }
    let mut output = Vec::with_capacity(filtered.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in filtered {
        if byte == b'=' {
            break;
        }
        let value = lookup[byte as usize];
        if value == 255 {
            return None;
        }
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_payload_is_accepted_at_its_scale() {
        assert_eq!(
            plan(1000, 1280, 800, 1.0),
            BudgetVerdict::Accept { scale: 1.0 }
        );
    }

    #[test]
    fn an_oversized_payload_walks_down() {
        let verdict = plan(COMPUTER_MAX_SCREENSHOT_BYTES + 1, 3840, 2160, 1.0);
        let BudgetVerdict::Retry { scale } = verdict else {
            panic!("a 4K payload should ask for a smaller scale");
        };
        assert!(scale < 1.0);
        // The walk never grows the image.
        let verdict = plan(COMPUTER_MAX_SCREENSHOT_BYTES + 1, 1280, 800, 0.5);
        let BudgetVerdict::Retry { scale } = verdict else {
            panic!("an oversized payload must retry");
        };
        assert!(scale <= 0.5);
    }

    #[test]
    fn the_walk_is_exhausted_rather_than_oversized() {
        let verdict = plan(
            COMPUTER_MAX_SCREENSHOT_BYTES + 1,
            1280,
            800,
            COMPUTER_SCREENSHOT_MIN_SCALE,
        );
        assert_eq!(verdict, BudgetVerdict::Exhausted);
    }

    #[test]
    fn a_zero_scale_does_not_produce_a_division_by_zero() {
        assert_eq!(screenshot_point_to_desktop(10.0, 10.0, 0.0), (10.0, 10.0));
        assert_eq!(screenshot_point_to_desktop(10.0, 10.0, 0.5), (20.0, 20.0));
    }

    #[test]
    fn writing_a_screenshot_creates_a_private_file() {
        let home = tempfile::tempdir().unwrap();
        let written =
            write_screenshot_file(home.path(), "com.example.mail", "QUJD", 1_000).unwrap();
        assert!(written.path.exists());
        assert_eq!(written.byte_len, 3);
        assert_eq!(std::fs::read(&written.path).unwrap(), b"ABC");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = written.path.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a screenshot file must not be world readable");
            let directory_mode = screenshot_directory(home.path())
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(directory_mode, 0o700);
        }
    }

    #[test]
    fn an_expired_file_is_removed_and_a_fresh_one_is_not() {
        use std::time::{Duration, SystemTime};
        let home = tempfile::tempdir().unwrap();
        let now = 1_700_000_000_000i64;
        let first = write_screenshot_file(home.path(), "one", "QUJD", now).unwrap();
        let second = write_screenshot_file(home.path(), "two", "QUJD", now).unwrap();
        // Age the first file past its TTL without touching the second.
        let expired = SystemTime::UNIX_EPOCH
            + Duration::from_millis((now - COMPUTER_SCREENSHOT_FILE_TTL_MS - 1_000) as u64);
        let file = std::fs::File::options()
            .write(true)
            .open(&first.path)
            .unwrap();
        file.set_modified(expired).unwrap();
        drop(file);
        // A later run prunes the expired file and keeps the fresh one.
        let _ = write_screenshot_file(home.path(), "three", "QUJD", now).unwrap();
        assert!(
            !first.path.exists(),
            "an expired screenshot must be removed"
        );
        assert!(second.path.exists(), "a fresh screenshot must be kept");
    }

    #[test]
    fn a_hostile_stem_cannot_escape_the_directory() {
        let home = tempfile::tempdir().unwrap();
        let written = write_screenshot_file(home.path(), "../../etc/passwd", "QUJD", 0).unwrap();
        assert_eq!(
            written.path.parent().unwrap(),
            screenshot_directory(home.path())
        );
    }

    #[test]
    fn invalid_base64_is_an_error_rather_than_a_partial_file() {
        let home = tempfile::tempdir().unwrap();
        assert!(write_screenshot_file(home.path(), "x", "not base64!", 0).is_err());
    }

    #[test]
    fn stripping_replaces_the_payload_with_a_path() {
        let mut value = serde_json::json!({
            "screenshot": { "data": "QUJD", "width": 10 }
        });
        let nested = value.get_mut("screenshot").unwrap();
        let replaced = strip_inline_screenshots(nested, Path::new("/tmp/shot.img"), 42);
        assert!(replaced);
        assert_eq!(nested["data"], serde_json::Value::Null);
        assert_eq!(nested["path"], "/tmp/shot.img");
        assert_eq!(nested["expiresAt"], 42);
        // A second pass over the same object changes nothing.
        assert!(!strip_inline_screenshots(
            nested,
            Path::new("/tmp/shot.img"),
            42
        ));
    }
}
