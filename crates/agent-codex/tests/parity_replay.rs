//! Offline Serde replay of sanitized Codex wire fixtures and transcripts.

mod common;

use std::path::Path;

use serde_json::{Value, json};
use vibex_agent_codex::{
    CodexSessionImportPreviewRequest, parity, preview_codex_external_sessions,
};
use vibex_core::WorkspaceMode;

#[test]
fn parity_replay_matches_golden_fixtures() {
    for dir in common::capability_dirs() {
        let meta = common::read_meta(&dir);
        let mode = meta
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("meta.json missing mode in {}", dir.display()));
        let Some(lines) = common::read_input_lines(&dir) else {
            assert_eq!(mode, "meta_only");
            continue;
        };
        let replay = || match mode {
            "thread_items" => replay_thread_items(&lines),
            "transcript" => replay_transcript(&dir),
            other => panic!("unknown parity mode {other:?} in {}", dir.display()),
        };
        let first = replay();
        assert_eq!(
            first,
            replay(),
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

fn replay_thread_items(lines: &[Value]) -> Value {
    let events = lines
        .iter()
        .flat_map(parity::map_wire_item)
        .map(|event| provider_event_json(&event))
        .collect::<Vec<_>>();
    json!({ "events": events })
}

fn replay_transcript(dir: &Path) -> Value {
    let preview = preview_codex_external_sessions(CodexSessionImportPreviewRequest {
        paths: vec![common::input_path(dir)],
        workspace_root: None,
        workspace_mode: WorkspaceMode::CurrentCheckout,
        provider_profile_id: None,
        correlation_id: None,
        limit: None,
    })
    .expect("codex transcript preview failed");
    assert_eq!(preview.candidates.len(), 1);
    let candidate = &preview.candidates[0];
    json!({
        "continuationStatus": candidate.continuation_status,
        "continuationReason": candidate.continuation_reason,
        "nativeThreadId": candidate.native_thread_id,
        "candidateStatus": candidate.status,
        "workspaceRoot": candidate.workspace_root,
        "items": candidate.timeline_items.iter().map(|item| json!({
            "source": item.source,
            "payload": item.payload,
            "providerCorrelationId": item.provider_correlation_id,
        })).collect::<Vec<_>>(),
        "diagnosticCodes": candidate.diagnostics.iter()
            .map(|diagnostic| diagnostic.code.clone()).collect::<Vec<_>>(),
    })
}

fn provider_event_json(event: &parity::ProviderEvent) -> Value {
    json!({
        "source": event.source,
        "payload": event.payload,
        "providerCorrelationId": event.provider_correlation_id,
    })
}

#[test]
#[ignore = "env-gated: capture through the managed Codex ACP adapter"]
fn parity_record_codex_acp_baseline() {
    assert_eq!(
        std::env::var("VIBEX_PARITY_RECORD").ok().as_deref(),
        Some("1")
    );
    unimplemented!(
        "capture bounded raw events through the managed Codex ACP adapter, sanitize every line, then regenerate goldens"
    );
}

#[test]
fn sanitizer_redacts_sensitive_keys_and_home_paths() {
    let line = format!(
        r#"{{"api_key":"sk-abcdef1234567890","cwd":"{home}/projects/demo","authorization":"Bearer x","text":"see /Users/alice/notes and sk-verysecretkey000"}}"#,
        home = std::env::var("HOME").unwrap_or_else(|_| "/home/someone".to_string())
    );
    let sanitized = common::sanitize_recorded_line(&line);
    let value: Value = serde_json::from_str(&sanitized).unwrap();
    assert_eq!(value["api_key"], "[REDACTED]");
    assert_eq!(value["authorization"], "[REDACTED]");
    assert_eq!(value["cwd"], "/home/user/projects/demo");
    let text = value["text"].as_str().unwrap();
    assert!(text.contains("/Users/user/notes"));
    assert!(text.contains("[REDACTED]"));
    assert!(!text.contains("verysecretkey"));
}
