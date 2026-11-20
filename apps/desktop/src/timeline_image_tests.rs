use super::*;
use vibex_core::{
    FileOperationKind, FileOperationPayload, TimelineItem, TimelineItemId, TimelinePayload,
    TimelineRedactionState, TimelineSource, VibexSessionId,
};

fn item(sequence: i64, payload: TimelinePayload) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::parse(format!("timeline_image-{sequence}")).unwrap(),
        session_id: VibexSessionId::parse("session_image").unwrap(),
        sequence,
        timestamp_ms: sequence,
        source: TimelineSource::Agent,
        kind: payload.kind(),
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: TimelineRedactionState::None,
        execution_attribution: None,
        payload_depth: vibex_core::TimelinePayloadDepth::Detail,
        payload,
    }
}

fn file(operation: FileOperationKind, path: &str) -> TimelinePayload {
    TimelinePayload::FileOperation(FileOperationPayload {
        operation,
        path: path.into(),
        summary: String::new(),
        old_text: None,
        new_text: None,
        patch: None,
        raw_extension: None,
    })
}

/// The activity a read row shows, projected the way the renderer projects it.
fn activity(operation: FileOperationKind, path: &str) -> timeline_activity::Activity {
    let payload = file(operation, path);
    let row = vibex_desktop_model::timeline_rows(&[item(1, payload.clone())]).remove(0);
    timeline_activity::project(&row, Some(&payload))
}

#[test]
fn a_read_resolves_workspace_and_external_pictures() {
    let workspace = "/work/vibex";
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "docs/doc-picker.png"),
            Some(workspace),
        ),
        Some(PathBuf::from("/work/vibex/docs/doc-picker.png"))
    );
    // An absolute path under the root and a `file://` URI name the same file.
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "/work/vibex/docs/final.png"),
            Some(workspace),
        ),
        Some(PathBuf::from("/work/vibex/docs/final.png"))
    );
    assert_eq!(
        image_read_path(
            &activity(
                FileOperationKind::Read,
                "file:///work/vibex/docs/final-scan.png"
            ),
            Some(workspace),
        ),
        Some(PathBuf::from("/work/vibex/docs/final-scan.png"))
    );
    // A picture outside the workspace is still the one the Agent looked at, so
    // an absolute path and a `file://` URI are used as written.
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "/home/peat/shots/find1.png"),
            Some(workspace),
        ),
        Some(PathBuf::from("/home/peat/shots/find1.png"))
    );
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "file:///tmp/vibex-capture.png"),
            Some(workspace),
        ),
        Some(PathBuf::from("/tmp/vibex-capture.png"))
    );
    // An absolute path needs no workspace at all.
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "/tmp/vibex-capture.png"),
            None,
        ),
        Some(PathBuf::from("/tmp/vibex-capture.png"))
    );
}

#[test]
fn relative_reads_are_normalised_against_the_workspace() {
    let workspace = Some("/work/vibex");
    // A read that steps out of the workspace means the sibling it names, not
    // the root, so both spellings land on the same file.
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "../shots/a.png"),
            workspace
        ),
        Some(PathBuf::from("/work/shots/a.png"))
    );
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "./docs/../shots/a.png"),
            workspace,
        ),
        Some(PathBuf::from("/work/vibex/shots/a.png"))
    );
}

#[test]
fn only_image_reads_of_local_files_get_a_thumbnail() {
    let workspace = Some("/work/vibex");
    // A text read keeps its file-type mark.
    assert_eq!(
        image_read_path(&activity(FileOperationKind::Read, "src/lib.rs"), workspace),
        None
    );
    // A non-file scheme names no local picture.
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "https://example.com/shot.png"),
            workspace,
        ),
        None
    );
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "asset://shots/shot.png"),
            workspace,
        ),
        None
    );
    // An edit or a write is not a read event.
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Edit, "docs/pic.png"),
            workspace
        ),
        None
    );
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Write, "docs/pic.png"),
            workspace,
        ),
        None
    );
    // A relative path with no workspace has nothing to resolve against.
    assert_eq!(
        image_read_path(&activity(FileOperationKind::Read, "docs/pic.png"), None),
        None
    );
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "docs/pic.png"),
            Some("  ")
        ),
        None
    );
    assert_eq!(
        image_read_path(&activity(FileOperationKind::Read, ""), workspace),
        None
    );
}
