use std::collections::BTreeSet;

use serde_json::Value;
use vibex_desktop_model::{
    DesktopUiStateV1, PreviewState, PreviewTarget, SidebarState, UiStateReferences,
};

const BEHAVIORAL_FIXTURE: &str =
    include_str!("../../../docs/parity/fixtures/desktop-behavioral-v1.json");

#[test]
fn behavioral_fixture_drives_deterministic_foundation_models() {
    let fixture: Value = serde_json::from_str(BEHAVIORAL_FIXTURE).unwrap();
    assert_eq!(fixture["schemaVersion"], "desktop-behavioral-fixtures.v1");
    assert_eq!(fixture["deterministic"], true);
    assert_eq!(fixture["providerFree"], true);
    assert_eq!(fixture["fixtures"].as_object().unwrap().len(), 2);

    let clock = fixture["clock"]["epochMs"].as_i64().unwrap();
    let file_path = fixture["fixtures"]["workbench.files"]["initialView"]["selectedFilePath"]
        .as_str()
        .unwrap();
    let git_path = fixture["fixtures"]["workbench.git_changes"]["initialView"]["selectedGitPath"]
        .as_str()
        .unwrap();
    let mut preview = PreviewState::default();
    let file_id = preview
        .open(
            PreviewTarget::File {
                path: file_path.to_string(),
            },
            None,
            clock,
        )
        .unwrap();
    let git_id = preview
        .open(
            PreviewTarget::GitDiff {
                path: git_path.to_string(),
                staged: false,
            },
            None,
            clock + fixture["clock"]["tickMs"].as_i64().unwrap(),
        )
        .unwrap();
    assert_eq!(file_id, "file:README.md");
    assert_eq!(git_id, "git:unstaged:apps/desktop/src/app.rs");

    let workspace_id = fixture["fixtures"]["workbench.files"]["initialView"]["selectedWorkspaceId"]
        .as_str()
        .unwrap()
        .to_string();
    let session_id = fixture["fixtures"]["workbench.files"]["initialView"]["selectedSessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let mut sidebar = SidebarState {
        row_order: vec![session_id.clone(), "stale_session".into()],
        pinned_ids: BTreeSet::from([session_id.clone(), "stale_session".into()]),
        ..Default::default()
    };
    sidebar.reconcile([session_id.clone()]);
    assert_eq!(sidebar.row_order, vec![session_id.clone()]);

    let mut state = DesktopUiStateV1::default();
    state.workbench.selected_workspace_id = Some(workspace_id.clone());
    state.workbench.selected_session_id = Some(session_id.clone());
    state.cleanup_stale_ids(&UiStateReferences {
        workspace_ids: BTreeSet::from([workspace_id]),
        session_ids: BTreeSet::from([session_id]),
        ..Default::default()
    });
    assert!(state.workbench.selected_workspace_id.is_some());
    assert!(state.workbench.selected_session_id.is_some());
}
