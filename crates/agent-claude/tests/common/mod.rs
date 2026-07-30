//! Shared offline parity-replay harness (task P1-01).
//!
//! Layout per capability:
//!   tests/fixtures/parity/<capability>/input.jsonl         raw native input (sanitized)
//!   tests/fixtures/parity/<capability>/expected_timeline.json  golden normalized output
//!   tests/fixtures/parity/<capability>/meta.json            capability metadata
//!
//! Golden files are regenerated with `UPDATE_PARITY_FIXTURES=1 cargo test ... parity`.
//! Capabilities that cannot be replayed offline carry only `meta.json`
//! (`"mode": "meta_only"`) documenting the online protocol and an
//! observation snapshot.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub const UPDATE_ENV: &str = "UPDATE_PARITY_FIXTURES";

pub fn parity_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("parity")
}

/// Every capability directory (sorted for deterministic iteration).
pub fn capability_dirs() -> Vec<PathBuf> {
    let root = parity_root();
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap_or_else(|err| panic!("parity fixture root missing at {}: {err}", root.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "no parity capability fixtures found");
    dirs
}

pub fn read_meta(dir: &Path) -> Value {
    let path = dir.join("meta.json");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("missing meta.json at {}: {err}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("invalid meta.json at {}: {err}", path.display()))
}

pub fn input_path(dir: &Path) -> PathBuf {
    dir.join("input.jsonl")
}

/// Parsed non-empty JSONL lines of `input.jsonl`, or `None` for meta-only capabilities.
pub fn read_input_lines(dir: &Path) -> Option<Vec<Value>> {
    let path = input_path(dir);
    if !path.exists() {
        return None;
    }
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("unreadable input.jsonl at {}: {err}", path.display()));
    Some(
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .enumerate()
            .map(|(index, line)| {
                serde_json::from_str(line).unwrap_or_else(|err| {
                    panic!(
                        "malformed input.jsonl line {} at {}: {err}",
                        index + 1,
                        path.display()
                    )
                })
            })
            .collect(),
    )
}

/// Stabilizes fields that are unstable across runs (timestamps, wall-clock ids).
pub fn normalize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if is_unstable_key(key) && entry.is_number() {
                    *entry = Value::from(0);
                } else {
                    normalize(entry);
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries.iter_mut() {
                normalize(entry);
            }
        }
        _ => {}
    }
}

fn is_unstable_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
    normalized.ends_with("atms") || normalized == "timestampms" || normalized == "timestamp"
}

/// Compares actual output with the golden `expected_timeline.json`.
/// With `UPDATE_PARITY_FIXTURES=1` the golden is rewritten instead.
pub fn assert_matches_golden(dir: &Path, mut actual: Value) {
    normalize(&mut actual);
    let golden_path = dir.join("expected_timeline.json");
    if std::env::var(UPDATE_ENV).ok().as_deref() == Some("1") {
        let pretty = format!("{}\n", serde_json::to_string_pretty(&actual).unwrap());
        fs::write(&golden_path, pretty)
            .unwrap_or_else(|err| panic!("failed writing golden {}: {err}", golden_path.display()));
        return;
    }

    let raw = fs::read_to_string(&golden_path).unwrap_or_else(|err| {
        panic!(
            "missing golden {} (run with {UPDATE_ENV}=1 to generate): {err}",
            golden_path.display()
        )
    });
    let mut expected: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("invalid golden {}: {err}", golden_path.display()));
    normalize(&mut expected);

    let mut diffs = Vec::new();
    structural_diff("$", &expected, &actual, &mut diffs);
    assert!(
        diffs.is_empty(),
        "parity mismatch for {} ({} field diffs):\n{}",
        dir.display(),
        diffs.len(),
        diffs.join("\n")
    );
}

/// Structured field-by-field diff; pushes one line per mismatching path.
pub fn structural_diff(path: &str, expected: &Value, actual: &Value, diffs: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(expected_map), Value::Object(actual_map)) => {
            let mut keys: Vec<&String> = expected_map.keys().chain(actual_map.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let child = format!("{path}.{key}");
                match (expected_map.get(key), actual_map.get(key)) {
                    (Some(expected), Some(actual)) => {
                        structural_diff(&child, expected, actual, diffs)
                    }
                    (Some(expected), None) => {
                        diffs.push(format!("{child}: expected {expected}, actual <missing>"))
                    }
                    (None, Some(actual)) => {
                        diffs.push(format!("{child}: expected <missing>, actual {actual}"))
                    }
                    (None, None) => unreachable!(),
                }
            }
        }
        (Value::Array(expected_items), Value::Array(actual_items)) => {
            if expected_items.len() != actual_items.len() {
                diffs.push(format!(
                    "{path}: expected array length {}, actual {}",
                    expected_items.len(),
                    actual_items.len()
                ));
            }
            for (index, (expected, actual)) in
                expected_items.iter().zip(actual_items.iter()).enumerate()
            {
                structural_diff(&format!("{path}[{index}]"), expected, actual, diffs);
            }
        }
        (expected, actual) => {
            if expected != actual {
                diffs.push(format!("{path}: expected {expected}, actual {actual}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Recording-side sanitization (used by the env-gated real recording path).
// Sanitization happens at record time so committed fixtures are always clean.
// ---------------------------------------------------------------------------

/// Sanitizes one raw recorded JSONL line before it may be written to a fixture.
pub fn sanitize_recorded_line(line: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(mut value) => {
            sanitize_recorded_value(&mut value);
            value.to_string()
        }
        Err(_) => sanitize_text(line),
    }
}

pub fn sanitize_recorded_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *entry = Value::String("[REDACTED]".to_string());
                } else {
                    sanitize_recorded_value(entry);
                }
            }
        }
        Value::Array(entries) => {
            for entry in entries.iter_mut() {
                sanitize_recorded_value(entry);
            }
        }
        Value::String(text) => {
            *text = sanitize_text(text);
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
    ["apikey", "token", "secret", "authorization", "password"]
        .iter()
        .any(|needle| normalized.contains(needle))
}

/// Replaces real user home paths with `/home/user` and redacts API-key-shaped tokens.
pub fn sanitize_text(text: &str) -> String {
    let mut output = text.to_string();
    if let Ok(home) = std::env::var("HOME")
        && home.len() > 1
    {
        output = output.replace(&home, "/home/user");
    }
    output = rewrite_home_segment(&output, "/home/");
    output = rewrite_home_segment(&output, "/Users/");
    redact_key_tokens(&output)
}

fn rewrite_home_segment(text: &str, prefix: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find(prefix) {
        let after = position + prefix.len();
        output.push_str(&rest[..after]);
        let tail = &rest[after..];
        let segment_len = tail
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')))
            .unwrap_or(tail.len());
        if segment_len > 0 {
            output.push_str("user");
        }
        rest = &tail[segment_len..];
    }
    output.push_str(rest);
    output
}

fn redact_key_tokens(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find("sk-") {
        output.push_str(&rest[..position]);
        let tail = &rest[position + 3..];
        let token_len = tail
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))
            .unwrap_or(tail.len());
        if token_len >= 8 {
            output.push_str("[REDACTED]");
        } else {
            output.push_str(&rest[position..position + 3 + token_len]);
        }
        rest = &tail[token_len..];
    }
    output.push_str(rest);
    output
}

/// Gate check: no committed parity fixture may contain a real user path or credential.
pub fn assert_fixture_tree_is_sanitized() {
    let mut leaks = Vec::new();
    let real_home = std::env::var("HOME")
        .ok()
        .filter(|home| home != "/home/user");
    scan_dir_for_leaks(&parity_root(), real_home.as_deref(), &mut leaks);
    assert!(
        leaks.is_empty(),
        "parity fixtures leak real paths/credentials:\n{}",
        leaks.join("\n")
    );
}

fn scan_dir_for_leaks(dir: &Path, real_home: Option<&str>, leaks: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.is_dir() {
            scan_dir_for_leaks(&path, real_home, leaks);
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(home) = real_home
            && contents.contains(home)
        {
            leaks.push(format!("{}: contains real home path", path.display()));
        }
        if redact_key_tokens(&contents) != contents {
            leaks.push(format!("{}: contains api-key-shaped token", path.display()));
        }
    }
}
