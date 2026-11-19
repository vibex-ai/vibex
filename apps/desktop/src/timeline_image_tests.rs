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
fn a_read_of_a_workspace_picture_resolves_to_the_file() {
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
}

#[test]
fn only_pictures_inside_the_workspace_get_a_thumbnail() {
    let workspace = Some("/work/vibex");
    // A text read keeps its file-type mark.
    assert_eq!(
        image_read_path(&activity(FileOperationKind::Read, "src/lib.rs"), workspace),
        None
    );
    // A picture outside the workspace has no path the preview can promise.
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "/work/other/private.png"),
            workspace,
        ),
        None
    );
    // A relative escape is rejected the same way.
    assert_eq!(
        image_read_path(
            &activity(FileOperationKind::Read, "../private.png"),
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
    // A session without a workspace cannot resolve a path at all.
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
}
