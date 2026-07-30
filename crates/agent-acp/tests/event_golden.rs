use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use vibex_agent_acp::{
    AgentEventEnricherKind, AgentEventInput, AgentEventInputSource, normalize_agent_event,
};
use vibex_core::{AgentEventLocation, AgentEventRawOutput, TimelinePayload, ToolCallStatus};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureEvent {
    native_event_id: String,
    tool_name: String,
    title: String,
    status: ToolCallStatus,
    raw_input: Option<Value>,
    output_summary: Option<String>,
    raw_output: Option<AgentEventRawOutput>,
    content: Option<Value>,
    #[serde(default)]
    locations: Vec<AgentEventLocation>,
    #[serde(default)]
    meta: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureMeta {
    compatibility_identity: String,
    enricher: String,
    native_baseline_references: Vec<String>,
}

#[test]
fn live_transcript_and_expected_canonical_timeline_match() {
    for case_dir in case_directories() {
        let meta: FixtureMeta = serde_json::from_str(
            &fs::read_to_string(case_dir.join("meta.json")).expect("event fixture meta must read"),
        )
        .expect("event fixture meta must decode");
        validate_baseline_references(&meta);
        let enricher = match meta.enricher.as_str() {
            "claude" => AgentEventEnricherKind::Claude,
            "codex" => AgentEventEnricherKind::Codex,
            "passthrough" => AgentEventEnricherKind::Passthrough,
            other => panic!("unknown fixture enricher {other}"),
        };
        let live = replay(
            &case_dir.join("live.jsonl"),
            AgentEventInputSource::Live,
            &meta.compatibility_identity,
            enricher,
        );
        let live_again = replay(
            &case_dir.join("live.jsonl"),
            AgentEventInputSource::Live,
            &meta.compatibility_identity,
            enricher,
        );
        let transcript = replay(
            &case_dir.join("transcript.jsonl"),
            AgentEventInputSource::Transcript,
            &meta.compatibility_identity,
            enricher,
        );
        let transcript_again = replay(
            &case_dir.join("transcript.jsonl"),
            AgentEventInputSource::Transcript,
            &meta.compatibility_identity,
            enricher,
        );
        assert_eq!(live, live_again, "live replay must be deterministic");
        assert_eq!(
            transcript, transcript_again,
            "transcript replay must be deterministic"
        );
        assert_eq!(live, transcript, "live and transcript semantics diverged");

        let expected_path = case_dir.join("expected_timeline.json");
        if std::env::var("UPDATE_ACP_EVENT_FIXTURES").ok().as_deref() == Some("1") {
            fs::write(
                &expected_path,
                format!("{}\n", serde_json::to_string_pretty(&live).unwrap()),
            )
            .expect("event fixture golden must update");
        }
        let expected: Value = serde_json::from_str(
            &fs::read_to_string(&expected_path).expect("event fixture golden must read"),
        )
        .expect("event fixture golden must decode");
        assert_eq!(live, expected, "canonical event golden drifted");
    }
}

#[test]
fn canonical_event_fixtures_contain_no_private_or_secret_material() {
    for case_dir in case_directories() {
        for entry in fs::read_dir(case_dir).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_file() {
                continue;
            }
            let content = fs::read_to_string(&path).unwrap();
            for forbidden in [
                "/home/alice",
                "/Users/alice",
                "apiKey",
                "authorization",
                "bearer ",
                "password",
                "private_key",
                "secret-value",
                "sk-live",
            ] {
                assert!(
                    !content
                        .to_ascii_lowercase()
                        .contains(&forbidden.to_ascii_lowercase()),
                    "{} leaked forbidden marker {forbidden}",
                    path.display()
                );
            }
        }
    }
}

fn replay(
    path: &Path,
    source: AgentEventInputSource,
    compatibility_identity: &str,
    enricher: AgentEventEnricherKind,
) -> Value {
    let events = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| {
            let fixture: FixtureEvent = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("invalid {}: {error}", path.display()));
            normalize_agent_event(
                enricher,
                &AgentEventInput {
                    source,
                    compatibility_identity: compatibility_identity.to_string(),
                    native_event_id: fixture.native_event_id,
                    tool_name: fixture.tool_name,
                    title: fixture.title,
                    status: fixture.status,
                    raw_input: fixture.raw_input,
                    output_summary: fixture.output_summary,
                    raw_output: fixture.raw_output,
                    content: fixture.content,
                    locations: fixture.locations,
                    meta: fixture.meta,
                },
            )
        })
        .map(|event| {
            let provider = event.into_provider_event();
            let kind = provider.payload.kind();
            let payload: TimelinePayload = provider.payload;
            json!({
                "source": provider.source,
                "kind": kind,
                "payload": payload,
                "providerCorrelationId": provider.provider_correlation_id,
                "redactionState": provider.redaction_state,
            })
        })
        .collect::<Vec<_>>();
    json!({ "events": events })
}

fn case_directories() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events");
    let mut directories = fs::read_dir(root)
        .expect("event fixture root must exist")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            path.is_dir().then_some(path)
        })
        .collect::<Vec<_>>();
    directories.sort();
    assert!(!directories.is_empty(), "event fixture root is empty");
    directories
}

fn validate_baseline_references(meta: &FixtureMeta) {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("agent-acp crate must live under workspace/crates")
        .to_path_buf();
    assert!(!meta.native_baseline_references.is_empty());
    for reference in &meta.native_baseline_references {
        assert!(
            workspace.join(reference).exists(),
            "P1 native baseline reference does not exist: {reference}"
        );
    }
}
