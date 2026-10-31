use super::*;
use gpui::{Bounds, Pixels, TestAppContext, VisualTestContext, size};
use vibex_backend::{FileBackend, NativeBackend, WorkspaceBackend};
use vibex_core::OpenWorkspaceRequest;
use vibex_desktop_model::{EditorBufferModel, EditorCloseGuard};
use vibex_desktop_runtime::{DesktopRuntime, DesktopRuntimeConfig, DesktopRuntimeFacade};

fn files_panel(cx: &mut TestAppContext) -> (Entity<CodeRightRail>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(crate::hint_layer::init);
    let captured = Rc::new(std::cell::RefCell::new(None));
    let slot = captured.clone();
    let (_, cx) = cx.add_window_view(move |window, cx| {
        let fixture =
            cx.new(|cx| CodeWorkbenchFixture::new(CodeWorkbenchFixtureKind::Files, window, cx));
        *slot.borrow_mut() = Some(fixture.read(cx).right_rail.clone());
        gpui_component::Root::new(fixture, window, cx)
    });
    cx.simulate_resize(size(px(420.0), px(720.0)));
    cx.update(|window, _| window.activate_window());
    draw(cx);
    (captured.borrow().clone().expect("files panel"), cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn row_bounds(
    panel: &Entity<CodeRightRail>,
    path: &str,
    cx: &mut VisualTestContext,
) -> Bounds<Pixels> {
    let id = panel.read_with(cx, |panel, _| {
        panel
            .projection
            .files
            .rows
            .iter()
            .find(|row| row.path == path)
            .unwrap_or_else(|| panic!("expected a visible file row for {path}"))
            .id
            .clone()
    });
    cx.debug_bounds(Box::leak(id.into_boxed_str()))
        .expect("rendered file row")
}

fn right_click(panel: &Entity<CodeRightRail>, path: &str, cx: &mut VisualTestContext) {
    draw(cx);
    let bounds = row_bounds(panel, path, cx);
    let position = point(bounds.right() - px(12.0), bounds.center().y);
    cx.simulate_mouse_move(position, None, Default::default());
    cx.simulate_mouse_down(position, MouseButton::Right, Default::default());
    cx.simulate_mouse_up(position, MouseButton::Right, Default::default());
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel
            .file_context_target
            .as_ref()
            .map(|(_, path)| path.clone())),
        Some(path.to_string()),
        "only the clicked row owns the menu and its highlight",
    );
}

// Navigate from the start without letting the pointer select a menu item.
fn choose_menu_item(position: usize, cx: &mut VisualTestContext) {
    cx.simulate_mouse_move(point(px(0.0), px(0.0)), None, Default::default());
    cx.simulate_keystrokes(&format!("{}enter", "down ".repeat(position)));
    draw(cx);
}

#[gpui::test]
fn file_panel_first_right_click_copies_the_current_target(cx: &mut TestAppContext) {
    let (panel, cx) = files_panel(cx);
    for (path, menu_position, expected) in [
        ("src/lib.rs", 5, "src/lib.rs"),
        ("docs/architecture.md", 6, "./docs/architecture.md"),
        ("Cargo.toml", 7, "Cargo.toml"),
    ] {
        right_click(&panel, path, cx);
        choose_menu_item(menu_position, cx);
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
            Some(expected.to_string())
        );
        assert!(panel.read_with(cx, |panel, _| panel.file_context_target.is_none()));
    }
}

#[gpui::test]
fn file_panel_cut_and_copy_are_available_on_the_first_menu(cx: &mut TestAppContext) {
    let (panel, cx) = files_panel(cx);
    for (path, position, operation) in [
        ("src/lib.rs", 3, FileClipboardOperation::Cut),
        ("docs", 4, FileClipboardOperation::Copy),
    ] {
        right_click(&panel, path, cx);
        choose_menu_item(position, cx);
        panel.read_with(cx, |panel, _| {
            let clipboard = panel
                .file_clipboard
                .as_ref()
                .expect("enabled clipboard command");
            assert_eq!(clipboard.path, path);
            assert_eq!(clipboard.operation, operation);
        });
    }
}

#[gpui::test]
fn file_panel_new_entries_keep_focus_below_the_requested_directory(cx: &mut TestAppContext) {
    let (panel, cx) = files_panel(cx);
    for directory in [false, true] {
        right_click(&panel, "docs", cx);
        choose_menu_item(if directory { 2 } else { 1 }, cx);
        panel.read_with(cx, |panel, cx| {
            assert_eq!(
                panel.inline_file_action,
                Some(if directory {
                    InlineFileAction::CreateDirectory {
                        parent: "docs".into(),
                    }
                } else {
                    InlineFileAction::CreateFile {
                        parent: "docs".into(),
                    }
                })
            );
            assert!(panel.inline_path_input.read(cx).value().is_empty());
        });
        let parent = row_bounds(&panel, "docs", cx);
        let editor = cx
            .debug_bounds("inline-file-tree-editor")
            .expect("new entry row");
        assert_eq!(editor.top(), parent.bottom());
        assert_eq!(editor.size.width, parent.size.width);
        panel.update_in(cx, |panel, window, cx| {
            assert!(panel.inline_path_input.focus_handle(cx).is_focused(window));
            panel.file_search_input.focus_handle(cx).focus(window, cx);
        });
        draw(cx);
        assert!(
            panel.read_with(cx, |panel, _| panel.inline_file_action.is_none()),
            "empty blur cancels without creating anything"
        );
        panel.update_in(cx, |panel, window, cx| {
            assert!(panel.file_search_input.focus_handle(cx).is_focused(window));
        });
    }
}

#[gpui::test]
fn file_panel_rename_survives_menu_dismissal_and_cancels_with_escape(cx: &mut TestAppContext) {
    let (panel, cx) = files_panel(cx);
    for path in ["src/lib.rs", "docs"] {
        right_click(&panel, path, cx);
        choose_menu_item(8, cx);
        panel.update_in(cx, |panel, window, cx| {
            assert_eq!(
                panel.inline_file_action,
                Some(InlineFileAction::Rename {
                    source: path.into()
                })
            );
            assert!(panel.inline_path_input.focus_handle(cx).is_focused(window));
        });
        let input = cx
            .debug_bounds("inline-file-tree-name-editor")
            .expect("rename input");
        cx.simulate_click(input.center(), Default::default());
        draw(cx);
        assert!(
            panel.read_with(cx, |panel, _| panel.inline_file_action.is_some()),
            "clicking a directory's input must not focus its containing row"
        );
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(panel.read_with(cx, |panel, _| panel.inline_file_action.is_none()));
        panel.update_in(cx, |panel, window, _| {
            assert!(panel.file_tree_focus.is_focused(window));
        });
    }
}

#[gpui::test]
fn file_panel_compact_directory_menu_uses_the_clicked_segment(cx: &mut TestAppContext) {
    let (panel, cx) = files_panel(cx);
    let workbench = panel.read_with(cx, |panel, _| panel.workbench.clone());
    workbench.update(cx, |workbench, cx| {
        let workspace_id = workbench.workspace.as_ref().unwrap().id.clone();
        let generation = workbench.file_tree.begin_load("docs");
        let entries = ["docs/guides", "docs/guides/setup"]
            .into_iter()
            .map(|path| FileTreeEntry {
                workspace_id: workspace_id.clone(),
                path: path.to_string(),
                name: path.rsplit('/').next().unwrap().to_string(),
                parent_path: Some(relative_parent_path(path).to_string()),
                kind: FileEntryKind::Directory,
                size_bytes: None,
                modified_at_ms: None,
                hidden: false,
                ignored: false,
            })
            .collect();
        assert!(
            workbench
                .file_tree
                .apply_entries(&workspace_id, generation, "docs", entries,)
        );
        cx.notify();
    });
    draw(cx);
    for command in [5, 1] {
        let segment = cx
            .debug_bounds("file-tree-segment:docs/guides")
            .expect("compact directory segment");
        cx.simulate_mouse_down(segment.center(), MouseButton::Right, Default::default());
        cx.simulate_mouse_up(segment.center(), MouseButton::Right, Default::default());
        draw(cx);
        assert_eq!(
            panel.read_with(cx, |panel, _| panel
                .file_context_target
                .as_ref()
                .map(|(_, path)| path.clone())),
            Some("docs/guides".into())
        );
        choose_menu_item(command, cx);
        if command == 5 {
            assert_eq!(
                cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
                Some("docs/guides".into())
            );
        }
    }
    panel.update_in(cx, |panel, window, cx| {
        assert_eq!(
            panel.inline_file_action,
            Some(InlineFileAction::CreateFile {
                parent: "docs/guides".into(),
            })
        );
        assert!(panel.inline_path_input.focus_handle(cx).is_focused(window));
    });
    let parent = row_bounds(&panel, "docs/guides/setup", cx);
    let input = cx.debug_bounds("inline-file-tree-editor").unwrap();
    assert_eq!(input.top(), parent.bottom());
}

#[gpui::test]
async fn file_panel_mutations_reach_the_workspace(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(gpui_tokio::init);
    let home = tempfile::tempdir().expect("isolated runtime home");
    let project = tempfile::tempdir().expect("isolated project");
    std::fs::create_dir(project.path().join("docs")).unwrap();
    let home_path = home.path().to_path_buf();
    let project_path = project.path().to_path_buf();
    let (runtime, backend, workspace) = cx
        .update(|cx| {
            gpui_tokio::Tokio::spawn(cx, async move {
                let runtime = DesktopRuntime::start(DesktopRuntimeConfig::isolated_test(home_path))
                    .await
                    .unwrap();
                let backend = Arc::new(NativeBackend::new(runtime.clone()));
                let workspace = backend
                    .open_workspace(MutationRequest::new(OpenWorkspaceRequest {
                        root_path: project_path.to_string_lossy().into_owned(),
                        mode: None,
                    }))
                    .await
                    .unwrap();
                (runtime, backend, workspace.workspace)
            })
        })
        .await
        .unwrap();
    let (panel, cx) = files_panel(cx);
    let workbench = panel.read_with(cx, |panel, _| panel.workbench.clone());
    workbench.update(cx, |workbench, cx| {
        workbench.backend = Some(backend.facade());
        workbench.workspace = Some(WorkbenchWorkspace {
            id: workspace.id.clone(),
            root: project.path().to_path_buf(),
            generation: workbench.workspace_generation,
        });
        workbench.file_tree.reset_workspace(workspace.id.clone());
        workbench.load_tree(cx);
    });
    cx.condition(&workbench, |workbench, _| !workbench.tree_loading)
        .await;
    draw(cx);

    right_click(&panel, "docs", cx);
    choose_menu_item(1, cx);
    cx.condition(&workbench, |workbench, _| workbench.tree_tasks.is_empty())
        .await;
    draw(cx);
    cx.simulate_input("new.txt");
    cx.simulate_keystrokes("enter");
    cx.condition(&panel, |panel, _| !panel.inline_file_submitting)
        .await;
    cx.condition(&workbench, |workbench, _| !workbench.tree_loading)
        .await;
    assert_eq!(
        std::fs::read(project.path().join("docs/new.txt")).unwrap(),
        b""
    );
    assert!(panel.read_with(cx, |panel, _| panel.inline_file_action.is_none()));
    panel.update_in(cx, |panel, window, _| {
        assert!(panel.file_tree_focus.is_focused(window));
    });
    draw(cx);

    right_click(&panel, "docs", cx);
    choose_menu_item(2, cx);
    cx.condition(&workbench, |workbench, _| workbench.tree_tasks.is_empty())
        .await;
    draw(cx);
    cx.simulate_input("new-folder");
    panel.update_in(cx, |panel, window, cx| {
        panel.file_search_input.focus_handle(cx).focus(window, cx);
    });
    cx.condition(&panel, |panel, _| {
        !panel.inline_file_submitting && panel.inline_file_action.is_none()
    })
    .await;
    cx.condition(&workbench, |workbench, _| !workbench.tree_loading)
        .await;
    assert!(project.path().join("docs/new-folder").is_dir());
    panel.update_in(cx, |panel, window, cx| {
        assert!(panel.file_search_input.focus_handle(cx).is_focused(window));
    });

    for (path, directory) in [("", true), ("docs", true), ("docs/new.txt", false)] {
        let facade = backend.facade();
        let resolved = resolve_external_open_path(&facade, &workspace.id, path, directory)
            .await
            .unwrap();
        assert_eq!(
            resolved,
            project.path().join(path).canonicalize().unwrap(),
            "local openers receive the real object, including folders and the workspace root"
        );
    }
    assert!(
        backend
            .resolve_local_path(workspace.id.clone(), "../outside".into())
            .await
            .is_err()
    );
    assert!(
        backend
            .resolve_local_path(workspace.id.clone(), "missing.txt".into())
            .await
            .is_err()
    );

    cx.condition(&workbench, |workbench, _| !workbench.tree_loading)
        .await;
    std::fs::write(project.path().join("docs/new.txt"), b"preserve these bytes").unwrap();
    right_click(&panel, "docs/new.txt", cx);
    choose_menu_item(4, cx);
    right_click(&panel, "docs/new-folder", cx);
    choose_menu_item(5, cx);
    cx.condition(&workbench, |workbench, _| {
        !workbench.file_mutation_pending && !workbench.tree_loading
    })
    .await;
    assert_eq!(
        std::fs::read(project.path().join("docs/new-folder/new.txt")).unwrap(),
        b"preserve these bytes"
    );
    assert!(project.path().join("docs/new.txt").exists());

    right_click(&panel, "docs/new.txt", cx);
    choose_menu_item(3, cx);
    right_click(&panel, "", cx);
    choose_menu_item(3, cx);
    cx.condition(&workbench, |workbench, _| {
        !workbench.file_mutation_pending && !workbench.tree_loading
    })
    .await;
    assert!(!project.path().join("docs/new.txt").exists());
    assert_eq!(
        std::fs::read(project.path().join("new.txt")).unwrap(),
        b"preserve these bytes"
    );

    right_click(&panel, "new.txt", cx);
    choose_menu_item(8, cx);
    cx.simulate_input("renamed.txt");
    cx.simulate_keystrokes("enter");
    cx.condition(&workbench, |workbench, _| {
        !workbench.file_mutation_pending && !workbench.tree_loading
    })
    .await;
    assert!(!project.path().join("new.txt").exists());
    assert!(project.path().join("renamed.txt").exists());

    right_click(&panel, "renamed.txt", cx);
    choose_menu_item(9, cx);
    assert!(cx.debug_bounds("dialog-0").is_some());
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(project.path().join("renamed.txt").exists());
    right_click(&panel, "renamed.txt", cx);
    choose_menu_item(9, cx);
    cx.simulate_keystrokes("enter");
    cx.condition(&workbench, |workbench, _| {
        !workbench.file_mutation_pending && !workbench.tree_loading
    })
    .await;
    assert!(!project.path().join("renamed.txt").exists());

    // A target created after the menu opens must preserve the failed draft.
    right_click(&panel, "", cx);
    choose_menu_item(2, cx);
    std::fs::create_dir(project.path().join("existing-folder")).unwrap();
    cx.simulate_input("existing-folder");
    cx.simulate_keystrokes("enter");
    cx.condition(&panel, |panel, _| {
        !panel.inline_file_submitting && panel.inline_file_error.is_some()
    })
    .await;
    assert!(workbench.read_with(cx, |workbench, _| {
        workbench
            .pending_error
            .as_deref()
            .is_some_and(|error| error.contains("file_create_directory_target_exists"))
    }));
    panel.update_in(cx, |panel, window, cx| {
        assert!(panel.inline_file_action.is_some());
        assert_eq!(panel.inline_path_input.read(cx).value(), "existing-folder");
        assert!(panel.inline_path_input.focus_handle(cx).is_focused(window));
    });
    cx.simulate_keystrokes("escape");
    draw(cx);
    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn file_panel_remote_directories_are_not_read_as_files() {
    let backend = vibex_backend::DisconnectedBackend::facade();
    let workspace_id = WorkspaceId::new();
    assert_eq!(
        backend
            .file()
            .resolve_local_path(workspace_id.clone(), "docs".into())
            .await
            .unwrap(),
        None
    );
    let error = resolve_external_open_path(&backend, &workspace_id, "docs", true)
        .await
        .unwrap_err();
    assert_eq!(error.code, "remote_directory_open_unavailable");
}

/// Opens a file tab on a real runtime and returns the workbench, wired the
/// way the panel is wired in production.
async fn editor_workbench<'a>(
    cx: &'a mut TestAppContext,
    project: &std::path::Path,
) -> (
    Entity<CodeWorkbench>,
    &'a mut VisualTestContext,
    Arc<DesktopRuntime>,
) {
    cx.executor().allow_parking();
    cx.update(gpui_tokio::init);
    let home = tempfile::tempdir().expect("isolated runtime home");
    let home_path = home.path().to_path_buf();
    let project_path = project.to_path_buf();
    let (runtime, backend, workspace) = cx
        .update(|cx| {
            gpui_tokio::Tokio::spawn(cx, async move {
                let runtime = DesktopRuntime::start(DesktopRuntimeConfig::isolated_test(home_path))
                    .await
                    .unwrap();
                let backend = Arc::new(NativeBackend::new(runtime.clone()));
                let workspace = backend
                    .open_workspace(MutationRequest::new(OpenWorkspaceRequest {
                        root_path: project_path.to_string_lossy().into_owned(),
                        mode: None,
                    }))
                    .await
                    .unwrap();
                (runtime, backend, workspace.workspace)
            })
        })
        .await
        .unwrap();
    // The runtime home has to outlive the runtime handle the test drives.
    std::mem::forget(home);
    let (workbench, cx) = crate::code_workbench::tests::fixture_workbench_with_root(cx);
    workbench.update(cx, |workbench, _| {
        workbench.backend = Some(backend.facade());
        workbench.local_workspace_root = Some(
            project
                .canonicalize()
                .expect("the project directory canonicalizes"),
        );
        workbench.workspace = Some(WorkbenchWorkspace {
            id: workspace.id.clone(),
            root: project.to_path_buf(),
            generation: workbench.workspace_generation,
        });
        workbench.file_tree.reset_workspace(workspace.id.clone());
    });
    (workbench, cx, runtime)
}

fn buffer_of(
    workbench: &Entity<CodeWorkbench>,
    path: &str,
    cx: &VisualTestContext,
) -> EditorBufferModel {
    workbench.read_with(cx, |workbench, _| {
        workbench
            .editors
            .buffers
            .get(path)
            .cloned()
            .unwrap_or_else(|| panic!("expected an editor buffer for {path}"))
    })
}

/// A rewrite on disk reaches a clean open buffer: the tab follows the file
/// without the reader doing anything.
#[gpui::test]
async fn a_watched_clean_buffer_follows_an_external_rewrite(cx: &mut TestAppContext) {
    let project = tempfile::tempdir().expect("isolated project");
    std::fs::write(project.path().join("lib.rs"), "fn one() {}\n").unwrap();
    let (workbench, cx, runtime) = editor_workbench(cx, project.path()).await;
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.open_file("lib.rs".into(), false, window, cx);
    });
    cx.condition(&workbench, |workbench, _| {
        workbench
            .editors
            .buffers
            .get("lib.rs")
            .is_some_and(|buffer| buffer.saved_content == "fn one() {}\n")
    })
    .await;
    workbench.update(cx, |workbench, cx| workbench.sync_file_watch(cx));
    assert!(
        workbench.read_with(cx, |workbench, _| workbench.file_watch.is_some()),
        "opening a local file registers a watcher"
    );

    std::fs::write(project.path().join("lib.rs"), "fn two() {}\n").unwrap();
    cx.condition(&workbench, |workbench, _| {
        workbench
            .editors
            .buffers
            .get("lib.rs")
            .is_some_and(|buffer| buffer.saved_content == "fn two() {}\n")
    })
    .await;
    let buffer = buffer_of(&workbench, "lib.rs", cx);
    assert_eq!(buffer.content, "fn two() {}\n");
    assert!(!buffer.dirty, "following the disk is not a local edit");
    assert_eq!(buffer.external, EditorExternalState::Current);

    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

/// A dirty buffer is never overwritten by a rewrite: it becomes a conflict the
/// reader resolves, and its text survives.
#[gpui::test]
async fn a_watched_dirty_buffer_reports_a_conflict_instead_of_reloading(cx: &mut TestAppContext) {
    let project = tempfile::tempdir().expect("isolated project");
    std::fs::write(project.path().join("lib.rs"), "fn one() {}\n").unwrap();
    let (workbench, cx, runtime) = editor_workbench(cx, project.path()).await;
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.open_file("lib.rs".into(), false, window, cx);
    });
    cx.condition(&workbench, |workbench, _| {
        workbench.editors.buffers.contains_key("lib.rs")
    })
    .await;
    workbench.update(cx, |workbench, cx| workbench.sync_file_watch(cx));

    workbench.update(cx, |workbench, _| {
        workbench
            .editors
            .buffers
            .get_mut("lib.rs")
            .unwrap()
            .update_content("fn mine() {}\n");
    });
    std::fs::write(project.path().join("lib.rs"), "fn theirs() {}\n").unwrap();
    cx.condition(&workbench, |workbench, _| {
        matches!(
            workbench
                .editors
                .buffers
                .get("lib.rs")
                .map(|buffer| &buffer.external),
            Some(EditorExternalState::Changed { .. })
        )
    })
    .await;
    let buffer = buffer_of(&workbench, "lib.rs", cx);
    assert_eq!(
        buffer.content, "fn mine() {}\n",
        "the watcher never replaces the text the reader typed"
    );
    assert!(buffer.dirty);
    assert_eq!(editor_status(&buffer), "External conflict");

    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

/// Asking for the file on disk wins over the local text: `Reload` discards the
/// edits and the buffer becomes clean again.
#[gpui::test]
async fn reloading_from_disk_replaces_the_local_text(cx: &mut TestAppContext) {
    let project = tempfile::tempdir().expect("isolated project");
    std::fs::write(project.path().join("lib.rs"), "fn one() {}\n").unwrap();
    let (workbench, cx, runtime) = editor_workbench(cx, project.path()).await;
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.open_file("lib.rs".into(), false, window, cx);
    });
    cx.condition(&workbench, |workbench, _| {
        workbench.editors.buffers.contains_key("lib.rs")
    })
    .await;
    workbench.update(cx, |workbench, _| {
        workbench
            .editors
            .buffers
            .get_mut("lib.rs")
            .unwrap()
            .update_content("fn mine() {}\n");
    });
    std::fs::write(project.path().join("lib.rs"), "fn theirs() {}\n").unwrap();

    workbench.update_in(cx, |workbench, window, cx| {
        workbench.reload_editor_from_disk("lib.rs".into(), window, cx);
    });
    cx.condition(&workbench, |workbench, _| {
        workbench
            .editors
            .buffers
            .get("lib.rs")
            .is_some_and(|buffer| !buffer.dirty && buffer.content == "fn theirs() {}\n")
    })
    .await;
    let buffer = buffer_of(&workbench, "lib.rs", cx);
    assert_eq!(buffer.content, "fn theirs() {}\n");
    assert!(!buffer.dirty);

    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

/// Deleting the file behind an open tab is reported as a deletion, and the
/// text stays in the recovery buffer instead of the tab going blank.
#[gpui::test]
async fn deleting_a_watched_file_keeps_its_content_recoverable(cx: &mut TestAppContext) {
    let project = tempfile::tempdir().expect("isolated project");
    std::fs::write(project.path().join("lib.rs"), "fn one() {}\n").unwrap();
    let (workbench, cx, runtime) = editor_workbench(cx, project.path()).await;
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.open_file("lib.rs".into(), false, window, cx);
    });
    cx.condition(&workbench, |workbench, _| {
        workbench.editors.buffers.contains_key("lib.rs")
    })
    .await;
    workbench.update(cx, |workbench, cx| workbench.sync_file_watch(cx));
    // A buffer with unsaved work is the case that matters: the deleted file
    // must not take the reader's text with it.
    workbench.update(cx, |workbench, _| {
        workbench
            .editors
            .buffers
            .get_mut("lib.rs")
            .unwrap()
            .update_content("fn mine() {}\n");
    });

    std::fs::remove_file(project.path().join("lib.rs")).unwrap();
    cx.condition(&workbench, |workbench, _| {
        workbench
            .editors
            .buffers
            .get("lib.rs")
            .is_some_and(|buffer| buffer.external == EditorExternalState::Deleted)
    })
    .await;
    let buffer = buffer_of(&workbench, "lib.rs", cx);
    assert_eq!(buffer.content, "fn mine() {}\n");
    assert_eq!(buffer.close_guard(), EditorCloseGuard::DirtyMissingFile);
    assert_eq!(editor_status(&buffer), "Deleted - recovery buffer");

    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}

/// A save this editor makes is not reported back to itself as an external
/// change: after every save the buffer stays clean and the tab keeps following
/// the file.
#[gpui::test]
async fn the_editor_does_not_conflict_with_its_own_save(cx: &mut TestAppContext) {
    let project = tempfile::tempdir().expect("isolated project");
    std::fs::write(project.path().join("lib.rs"), "fn one() {}\n").unwrap();
    let (workbench, cx, runtime) = editor_workbench(cx, project.path()).await;
    workbench.update_in(cx, |workbench, window, cx| {
        workbench.open_file("lib.rs".into(), false, window, cx);
    });
    cx.condition(&workbench, |workbench, _| {
        workbench.editors.buffers.contains_key("lib.rs")
    })
    .await;
    workbench.update(cx, |workbench, cx| workbench.sync_file_watch(cx));

    workbench.update(cx, |workbench, _| {
        workbench
            .editors
            .buffers
            .get_mut("lib.rs")
            .unwrap()
            .update_content("fn mine() {}\n");
    });
    workbench.update(cx, |workbench, cx| {
        assert!(workbench.save_editor("lib.rs".into(), false, cx));
    });
    cx.condition(&workbench, |workbench, _| {
        workbench
            .editors
            .buffers
            .get("lib.rs")
            .is_some_and(|buffer| !buffer.dirty && buffer.pending_save.is_none())
    })
    .await;
    // Let the watcher's debounce and read run, then check the buffer is still
    // clean: the save must not come back as an external change.
    draw(cx);
    cx.background_executor
        .timer(std::time::Duration::from_millis(300))
        .await;
    draw(cx);
    let buffer = buffer_of(&workbench, "lib.rs", cx);
    assert!(!buffer.dirty);
    assert_eq!(buffer.external, EditorExternalState::Current);
    assert_eq!(buffer.content, "fn mine() {}\n");

    cx.update(|_, cx| {
        gpui_tokio::Tokio::spawn(cx, async move { runtime.shutdown().await.unwrap() })
    })
    .await
    .unwrap();
}
