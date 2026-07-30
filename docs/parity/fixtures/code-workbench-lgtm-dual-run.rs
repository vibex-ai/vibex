use std::hint::black_box;
use std::time::Instant;

use diff_core::{DiffRow as LgtmRow, FileStatus};
use serde_json::{Value, json};
use vibex_desktop_model::{
    UnifiedDiffLineKind, VirtualDiffRows, parse_unified_diff,
};

fn normalized_vibex(patch: &str) -> Value {
    let files = parse_unified_diff(patch)
        .into_iter()
        .map(|file| {
            let mut additions = 0;
            let mut deletions = 0;
            let mut contexts = 0;
            let mut hunks = 0;
            let mut newline_markers = 0;
            for line in &file.lines {
                match line.kind {
                    UnifiedDiffLineKind::Add => additions += 1,
                    UnifiedDiffLineKind::Delete => deletions += 1,
                    UnifiedDiffLineKind::Context => contexts += 1,
                    UnifiedDiffLineKind::Hunk => hunks += 1,
                    UnifiedDiffLineKind::Meta => newline_markers += 1,
                }
            }
            json!({
                "oldPath": file.old_path.filter(|path| path != "/dev/null"),
                "newPath": file.new_path.filter(|path| path != "/dev/null"),
                "binary": file.binary,
                "renamed": file.renamed,
                "copied": file.copied,
                "hunks": hunks,
                "additions": additions,
                "deletions": deletions,
                "contexts": contexts,
                "newlineMarkers": newline_markers,
            })
        })
        .collect::<Vec<_>>();
    json!({ "files": files })
}

fn normalized_lgtm(patch: &str) -> Value {
    let files = diff_core::parse_patch(patch)
        .files
        .into_iter()
        .map(|file| {
            let contexts = file
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .filter(|row| matches!(row, LgtmRow::Context { .. }))
                .count();
            json!({
                "oldPath": file.old_path,
                "newPath": file.new_path,
                "binary": file.status == FileStatus::Binary,
                "renamed": file.status == FileStatus::Renamed,
                "copied": false,
                "hunks": file.hunks.len(),
                "additions": file.additions,
                "deletions": file.deletions,
                "contexts": contexts,
                "newlineMarkers": 0,
            })
        })
        .collect::<Vec<_>>();
    json!({ "files": files })
}

fn word_signals(patch: &str) -> Value {
    let files = parse_unified_diff(patch);
    let mut vibex = VirtualDiffRows::new("dual-run", &files);
    let vibex_changed_rows = vibex
        .visible_window(0, vibex.len(), 0)
        .into_iter()
        .filter(|row| !row.word_spans.is_empty())
        .count();
    let lgtm_changed_rows = diff_core::parse_patch(patch)
        .files
        .iter()
        .flat_map(|file| &file.hunks)
        .flat_map(|hunk| &hunk.rows)
        .filter(|row| match row {
            LgtmRow::Added { intra, .. } | LgtmRow::Removed { intra, .. } => !intra.is_empty(),
            LgtmRow::Context { .. } => false,
        })
        .count();
    json!({
        "vibexChangedRows": vibex_changed_rows,
        "lgtmChangedRows": lgtm_changed_rows,
        "bothProduceBoundedWordSignals": vibex_changed_rows > 0 && lgtm_changed_rows > 0,
    })
}

fn median_micros(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn benchmark(patch: &str) -> Value {
    let mut vibex = Vec::new();
    let mut lgtm = Vec::new();
    for _ in 0..9 {
        let started = Instant::now();
        black_box(parse_unified_diff(black_box(patch)));
        vibex.push(started.elapsed().as_micros());

        let started = Instant::now();
        black_box(diff_core::parse_patch(black_box(patch)));
        lgtm.push(started.elapsed().as_micros());
    }
    json!({
        "iterations": 9,
        "inputLines": patch.lines().count(),
        "vibexMedianMicros": median_micros(vibex),
        "lgtmMedianMicros": median_micros(lgtm),
    })
}

fn large_patch() -> String {
    let mut patch = String::from(
        "diff --git a/large.rs b/large.rs\n--- a/large.rs\n+++ b/large.rs\n@@ -1,10000 +1,10000 @@\n",
    );
    for index in 0..10_000 {
        patch.push_str(&format!("-let old_{index} = {index};\n"));
    }
    for index in 0..10_000 {
        patch.push_str(&format!("+let new_{index} = {index};\n"));
    }
    patch
}

fn main() {
    let fixtures = [
        (
            "hunk_word",
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-let old_name = 1;\n+let new_name = 1;\n keep();\n",
        ),
        (
            "rename",
            "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-old\n+new\n",
        ),
        (
            "binary",
            "diff --git a/logo.png b/logo.png\nindex 111..222 100644\nBinary files a/logo.png and b/logo.png differ\n",
        ),
        (
            "crlf",
            "diff --git a/crlf.txt b/crlf.txt\r\n--- a/crlf.txt\r\n+++ b/crlf.txt\r\n@@ -1 +1 @@\r\n-old\r\n+new\r\n",
        ),
        (
            "quoted_octal_utf8",
            "diff --git \"a/\\346\\226\\207.md\" \"b/\\346\\226\\207.md\"\n--- \"a/\\346\\226\\207.md\"\n+++ \"b/\\346\\226\\207.md\"\n@@ -1 +1 @@\n-old\n+new\n",
        ),
        (
            "no_newline_marker",
            "diff --git a/no-newline.txt b/no-newline.txt\n--- a/no-newline.txt\n+++ b/no-newline.txt\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n",
        ),
        (
            "copy",
            "diff --git a/source.rs b/copied.rs\nsimilarity index 100%\ncopy from source.rs\ncopy to copied.rs\n",
        ),
        (
            "malformed_hunk",
            "diff --git a/bad.txt b/bad.txt\n--- a/bad.txt\n+++ b/bad.txt\n@@ malformed @@\n-old\n+new\n",
        ),
    ];

    let fixture_reports = fixtures
        .into_iter()
        .map(|(name, patch)| {
            let vibex = normalized_vibex(patch);
            let lgtm = normalized_lgtm(patch);
            let bounded = std::panic::catch_unwind(|| {
                let _ = normalized_vibex(patch);
                let _ = normalized_lgtm(patch);
            })
            .is_ok();
            json!({
                "name": name,
                "semanticMatch": vibex == lgtm,
                "boundedWithoutPanic": bounded,
                "vibex": vibex,
                "lgtm": lgtm,
                "wordSignals": (name == "hunk_word").then(|| word_signals(patch)),
            })
        })
        .collect::<Vec<_>>();
    let large = large_patch();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schemaVersion": "vibex-lgtm-dual-run.v1",
            "fixtures": fixture_reports,
            "performance": benchmark(&large),
        }))
        .unwrap()
    );
}
