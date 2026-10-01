//! The arrangement a shell reads and writes when it owns the runtime itself.
//!
//! A client with no Desktop attached persists the tree it was handed, and the
//! revision it echoes with its next change is the one the answer carried. That
//! makes the answer's revision load-bearing: if it is the revision from before
//! the change, the next change is refused as stale and the reader has to press
//! the same key twice — every time.
#![cfg(feature = "native")]

use std::sync::Arc;

use vibex_backend::{NativeBackend, SidebarBackend};
use vibex_core::{RemoteSidebarOrganizationMutation, RemoteSidebarOrganizationSnapshot};
use vibex_desktop_model::{SidebarFolderUiState, SidebarOrganizationView, UiStateStore};
use vibex_desktop_runtime::{DesktopRuntime, DesktopRuntimeConfig};

fn folder_collapse(collapsed: bool) -> RemoteSidebarOrganizationMutation {
    RemoteSidebarOrganizationMutation::SetFolderCollapsed {
        folder_id: "folder-1".to_string(),
        collapsed,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_is_answered_with_the_revision_the_next_one_is_checked_against() {
    let root = tempfile::tempdir().expect("a temp home");
    let runtime = DesktopRuntime::start(DesktopRuntimeConfig::isolated_test(root.path()))
        .await
        .expect("an isolated runtime starts");
    let backend = Arc::new(NativeBackend::new(runtime.clone()));

    // An arrangement with something to fold: one folder, placed by hand the way
    // a Desktop drag would place it.
    let store = UiStateStore::new(runtime.ui_state_path());
    let mut ui_state = store.load_read_only().expect("a readable home").state;
    ui_state.sidebar.organization.folders.insert(
        "folder-1".to_string(),
        SidebarFolderUiState {
            name: "archive".to_string(),
            project_id: Some("project_test".to_string()),
            workspace_id: None,
            auto_archive_after_days: None,
        },
    );
    ui_state.sidebar.organization.placements.push(
        vibex_desktop_model::SidebarOrganizationPlacement {
            item: vibex_desktop_model::SidebarOrganizationItem::Folder("folder-1".to_string()),
            parent_folder_id: None,
        },
    );
    store.save(&ui_state).expect("the arrangement persists");

    let first: RemoteSidebarOrganizationSnapshot = backend
        .sidebar_organization()
        .await
        .expect("the arrangement is readable");

    // The key the reader pressed first: fold the folder.
    let answered = backend
        .mutate_sidebar_organization(folder_collapse(true), Some(first.revision))
        .await
        .expect("the first change is against the tree just read");
    let view = SidebarOrganizationView::from_remote(&answered);
    assert!(
        view.organization.collapsed_folder_ids.contains("folder-1"),
        "the change did not land in the answer"
    );
    // The fingerprint is the content, so folding a folder has to move it. A
    // client that echoes a revision from before its own change is refused as
    // stale — which is the "press twice" this test exists for.
    assert_ne!(
        view.revision, first.revision,
        "the answer carried the revision from before the change"
    );

    // What the client echoes next: the revision from that answer.
    let second = backend
        .mutate_sidebar_organization(folder_collapse(false), Some(view.revision))
        .await;
    assert!(
        second.is_ok(),
        "the answer carried a revision the next change was refused against: {:?}",
        second.err().map(|error| error.message)
    );
    let reopened =
        SidebarOrganizationView::from_remote(&second.expect("the second change was accepted"));
    assert!(
        !reopened
            .organization
            .collapsed_folder_ids
            .contains("folder-1"),
        "the second change did not land"
    );

    // And the check is not vacuous: another change moves the tree again, and
    // the revision from the tree before it is refused rather than applied to
    // something the reader was not looking at.
    let renamed = backend
        .mutate_sidebar_organization(
            RemoteSidebarOrganizationMutation::RenameFolder {
                folder_id: "folder-1".to_string(),
                name: "archive-2".to_string(),
            },
            Some(reopened.revision),
        )
        .await
        .expect("renaming the folder is legal");
    assert_eq!(
        SidebarOrganizationView::from_remote(&renamed)
            .organization
            .folders
            .get("folder-1")
            .map(|folder| folder.name.as_str()),
        Some("archive-2")
    );
    let stale = backend
        .mutate_sidebar_organization(folder_collapse(true), Some(view.revision))
        .await;
    assert!(stale.is_err(), "a revision from an older tree was accepted");
}
