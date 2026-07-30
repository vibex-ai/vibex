//! Offline parity replay for sanitized Claude compatibility fixtures.
//!
//! Replays committed fixtures through offline Serde normalization and
//! transcript-import paths, then compares the resulting canonical timeline
//! payloads against golden `expected_timeline.json` files. Regenerate with
//! `UPDATE_PARITY_FIXTURES=1 cargo test -p vibex-agent-claude parity`.

mod common;

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};
use vibex_agent_claude::{
    ClaudeSessionImportPreviewRequest, parity, preview_claude_external_sessions,
};
use vibex_core::WorkspaceMode;

#[test]
fn parity_replay_matches_golden_fixtures() {
    for dir in common::capability_dirs() {
        let meta = common::read_meta(&dir);
        let mode = meta
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("meta.json missing \"mode\" in {}", dir.display()));

        let Some(lines) = common::read_input_lines(&dir) else {
            assert_eq!(
                mode,
                "meta_only",
                "{} has no input.jsonl but is not meta_only",
                dir.display()
            );
            continue;
        };

        let replay = || match mode {
            "stream_events" => replay_stream_events(&lines),
            "transcript" => replay_transcript(&dir),
            other => panic!("unknown parity mode {other:?} in {}", dir.display()),
        };
        let first = replay();
        let second = replay();
        assert_eq!(
            first,
            second,
            "replay is not deterministic in {}",
            dir.display()
        );
        common::assert_matches_golden(&dir, first);
    }
}

#[test]
fn parity_fixtures_contain_no_real_paths_or_credentials() {
    common::assert_fixture_tree_is_sanitized();
}

/// Feeds sanitized Claude stream envelopes through the offline mapper.
fn replay_stream_events(lines: &[Value]) -> Value {
    let mut chunk_index = 0_u32;
    let mut tool_blocks = HashMap::new();
    let events: Vec<Value> = lines
        .iter()
        .filter_map(|event| parity::map_stream_event(event, &mut chunk_index, &mut tool_blocks))
        .map(|event| provider_event_json(&event))
        .collect();
    json!({ "events": events })
}

/// Feeds a native Claude transcript JSONL file through the transcript import
/// parser (`preview_claude_external_sessions`).
fn replay_transcript(dir: &Path) -> Value {
    let preview = preview_claude_external_sessions(ClaudeSessionImportPreviewRequest {
        paths: vec![common::input_path(dir)],
        workspace_root: None,
        workspace_mode: WorkspaceMode::CurrentCheckout,
        provider_profile_id: None,
        correlation_id: None,
        limit: None,
    })
    .expect("claude transcript preview failed");
    assert_eq!(preview.candidates.len(), 1);
    let candidate = &preview.candidates[0];

    json!({
        "continuationStatus": candidate.continuation_status,
        "continuationReason": candidate.continuation_reason,
        "nativeSessionId": candidate.native_session_id,
        "candidateStatus": candidate.status,
        "workspaceRoot": candidate.workspace_root,
        "items": candidate
            .timeline_items
            .iter()
            .map(|item| json!({
                "source": item.source,
                "payload": item.payload,
                "providerCorrelationId": item.provider_correlation_id,
            }))
            .collect::<Vec<_>>(),
        "diagnosticCodes": candidate
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.clone())
            .collect::<Vec<_>>(),
    })
}

fn provider_event_json(event: &parity::ProviderEvent) -> Value {
    json!({
        "source": event.source,
        "payload": event.payload,
        "providerCorrelationId": event.provider_correlation_id,
    })
}

/// Env-gated recording path. CI never runs this; a developer with a managed
/// Claude ACP Adapter sets `VIBEX_PARITY_RECORD=1` and runs
/// `cargo test -p vibex-agent-claude --test parity_replay -- --ignored`.
/// Every captured line MUST pass through `common::sanitize_recorded_line`
/// before it is written into `tests/fixtures/parity/<capability>/input.jsonl`.
#[test]
#[ignore = "env-gated: capture through the managed Claude ACP adapter"]
fn parity_record_claude_acp_baseline() {
    if std::env::var("VIBEX_PARITY_RECORD").ok().as_deref() != Some("1") {
        eprintln!("skipping parity_record: set VIBEX_PARITY_RECORD=1 to record");
        return;
    }
    unimplemented!(
        "capture bounded raw events through the managed Claude ACP adapter, sanitize every line, then regenerate goldens"
    );
}

// ---------------------------------------------------------------------------
// Recording-side sanitizer unit coverage (gate: fixtures never leak secrets).
// ---------------------------------------------------------------------------

#[test]
fn sanitizer_redacts_sensitive_keys_and_home_paths() {
    let line = format!(
        r#"{{"apiKey":"sk-abcdef1234567890","cwd":"{home}/projects/demo","session_token":"top","text":"see /Users/alice/notes and sk-verysecretkey000"}}"#,
        home = std::env::var("HOME").unwrap_or_else(|_| "/home/someone".to_string())
    );

    let sanitized = common::sanitize_recorded_line(&line);
    let value: Value = serde_json::from_str(&sanitized).unwrap();

    assert_eq!(value["apiKey"], "[REDACTED]");
    assert_eq!(value["session_token"], "[REDACTED]");
    assert_eq!(value["cwd"], "/home/user/projects/demo");
    let text = value["text"].as_str().unwrap();
    assert!(text.contains("/Users/user/notes"), "text was: {text}");
    assert!(text.contains("[REDACTED]"), "text was: {text}");
    assert!(!text.contains("verysecretkey"));
}

#[test]
fn sanitizer_keeps_short_sk_prefixes_and_non_json_lines() {
    assert_eq!(common::sanitize_text("task sk-1 done"), "task sk-1 done");
    let sanitized = common::sanitize_recorded_line("not json /home/bob/file.txt");
    assert_eq!(sanitized, "not json /home/user/file.txt");
}
