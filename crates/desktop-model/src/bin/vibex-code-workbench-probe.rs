use std::time::Instant;

use serde::Serialize;
use vibex_core::{
    FileEntryKind, FileTreeEntry, GitChange, GitChangeKind, GitDiffResponse, GitStatusSummary,
    WorkspaceId,
};
use vibex_desktop_model::{
    BoundedImageCache, DIFF_DEFAULT_OVERSCAN, DIFF_WORD_CACHE_ROWS, FileTreeProjection,
    GIT_DIFF_CACHE_ITEM_LIMIT, GitQueryKind, GitSelectionKey, GitWorkbenchState, ImageCacheKey,
    UnifiedDiffFile, UnifiedDiffLine, UnifiedDiffLineKind, VirtualDiffRows,
};

const TREE_ENTRY_COUNT: usize = 100_000;
const TREE_DEPTH: usize = 8;
const DIFF_ROW_COUNT: usize = 20_000;
const DEEP_WINDOW_START: usize = 19_500;
const DEEP_WINDOW_ROWS: usize = 120;
const INITIAL_DIFF_ROWS: usize = 500;
const SWITCH_ITERATIONS: usize = 128;
const TREE_OPERATION_LIMIT_MICROS: u64 = 15_000_000;
const DIFF_OPERATION_LIMIT_MICROS: u64 = 5_000_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeWorkbenchProbe {
    schema_version: &'static str,
    status: &'static str,
    tree: TreeProbe,
    diff: DiffProbe,
    caches: CacheProbe,
    generations: GenerationProbe,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeProbe {
    input_entries: usize,
    depth: usize,
    compact_chain_segments: usize,
    expanded_visible_rows: usize,
    deep_window_start: usize,
    deep_window_rows: usize,
    search_visible_rows: usize,
    search_leaf_found: bool,
    stale_load_rejected: bool,
    apply_micros: u64,
    expand_micros: u64,
    search_micros: u64,
    operation_limit_micros: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiffProbe {
    input_rows: usize,
    projected_rows: usize,
    initial_window_rows: usize,
    deep_window_start: usize,
    deep_window_rows: usize,
    deep_first_row_id: String,
    cached_word_rows: usize,
    cache_limit_rows: usize,
    construct_micros: u64,
    initial_window_micros: u64,
    deep_window_micros: u64,
    operation_limit_micros: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheProbe {
    git_insertions: usize,
    git_resident_items: usize,
    git_item_limit: usize,
    oldest_git_item_evicted: bool,
    newest_git_item_resident: bool,
    image_insertions: usize,
    image_resident_items: usize,
    image_resident_bytes: usize,
    image_evictions: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationProbe {
    workspace_switches: usize,
    stale_workspace_results_rejected: usize,
    revision_switches: usize,
    stale_revision_results_rejected: usize,
    final_diff_cache_items: usize,
}

fn elapsed_micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn entry(
    workspace_id: &WorkspaceId,
    path: String,
    parent_path: Option<String>,
    kind: FileEntryKind,
) -> FileTreeEntry {
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    FileTreeEntry {
        workspace_id: workspace_id.clone(),
        path,
        name,
        parent_path,
        kind,
        size_bytes: None,
        modified_at_ms: None,
        hidden: false,
        ignored: false,
    }
}

fn tree_probe() -> TreeProbe {
    let workspace_id = WorkspaceId::new();
    let mut entries = Vec::with_capacity(TREE_ENTRY_COUNT);
    let mut directories = Vec::with_capacity(TREE_DEPTH);
    let mut parent = None;
    let mut current = String::new();
    for depth in 0..TREE_DEPTH {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(&format!("level-{depth}"));
        entries.push(entry(
            &workspace_id,
            current.clone(),
            parent.clone(),
            FileEntryKind::Directory,
        ));
        directories.push(current.clone());
        parent = Some(current.clone());
    }
    let leaf_parent = parent.expect("tree fixture has a leaf directory");
    let file_count = TREE_ENTRY_COUNT - TREE_DEPTH;
    for index in 0..file_count {
        let name = if index + 1 == file_count {
            "needle-target.rs".to_string()
        } else {
            format!("file-{index:05}.rs")
        };
        entries.push(entry(
            &workspace_id,
            format!("{leaf_parent}/{name}"),
            Some(leaf_parent.clone()),
            FileEntryKind::File,
        ));
    }

    let mut tree = FileTreeProjection::default();
    tree.reset_workspace(workspace_id.clone());
    let generation = tree.begin_load("");
    let started = Instant::now();
    let applied = tree.apply_entries(&workspace_id, generation, "", entries);
    let apply_micros = elapsed_micros(started);

    let started = Instant::now();
    for directory in &directories {
        assert!(tree.toggle_expanded(directory));
    }
    let expand_micros = elapsed_micros(started);
    let compact_chain_segments = tree
        .visible_row(1)
        .map(|row| row.path_chain.len())
        .unwrap_or_default();
    let expanded_visible_rows = tree.visible_row_count();
    let deep_window_start = expanded_visible_rows.saturating_sub(200);
    let deep_window_rows = tree.visible_window(deep_window_start, 120, 24).len();

    let started = Instant::now();
    tree.set_query("needle-target");
    let search_micros = elapsed_micros(started);
    let search_visible_rows = tree.visible_row_count();
    let search_leaf_found = tree
        .all_visible_rows()
        .last()
        .is_some_and(|row| row.path.ends_with("needle-target.rs"));

    let stale_generation = tree.begin_load("");
    let next_workspace = WorkspaceId::new();
    tree.reset_workspace(next_workspace);
    let stale_load_rejected = !tree.apply_entries(&workspace_id, stale_generation, "", Vec::new());

    assert!(applied);
    TreeProbe {
        input_entries: TREE_ENTRY_COUNT,
        depth: TREE_DEPTH,
        compact_chain_segments,
        expanded_visible_rows,
        deep_window_start,
        deep_window_rows,
        search_visible_rows,
        search_leaf_found,
        stale_load_rejected,
        apply_micros,
        expand_micros,
        search_micros,
        operation_limit_micros: TREE_OPERATION_LIMIT_MICROS,
    }
}

fn large_diff_file() -> UnifiedDiffFile {
    UnifiedDiffFile {
        old_path: Some("large.rs".to_string()),
        new_path: Some("large.rs".to_string()),
        header: Vec::new(),
        lines: (0..DIFF_ROW_COUNT)
            .map(|index| UnifiedDiffLine {
                kind: if index % 2 == 0 {
                    UnifiedDiffLineKind::Delete
                } else {
                    UnifiedDiffLineKind::Add
                },
                old_line: (index % 2 == 0).then_some(index as u32 + 1),
                new_line: (index % 2 == 1).then_some(index as u32 + 1),
                content: format!("let value_{index} = {index};"),
            })
            .collect(),
        binary: false,
        renamed: false,
        copied: false,
    }
}

fn diff_probe() -> DiffProbe {
    let started = Instant::now();
    let mut rows = VirtualDiffRows::new("probe-r1", &[large_diff_file()]);
    let construct_micros = elapsed_micros(started);

    let started = Instant::now();
    let initial = rows.visible_window(0, INITIAL_DIFF_ROWS, DIFF_DEFAULT_OVERSCAN);
    let initial_window_micros = elapsed_micros(started);

    let started = Instant::now();
    let deep = rows.visible_window(DEEP_WINDOW_START, DEEP_WINDOW_ROWS, DIFF_DEFAULT_OVERSCAN);
    let deep_window_micros = elapsed_micros(started);
    let deep_first_row_id = deep
        .first()
        .map(|row| row.row.id.clone())
        .unwrap_or_default();

    for start in (0..rows.len()).step_by(256) {
        let length = 256.min(rows.len().saturating_sub(start));
        let _ = rows.visible_window(start, length, DIFF_DEFAULT_OVERSCAN);
    }

    DiffProbe {
        input_rows: DIFF_ROW_COUNT,
        projected_rows: rows.len(),
        initial_window_rows: initial.len(),
        deep_window_start: DEEP_WINDOW_START,
        deep_window_rows: deep.len(),
        deep_first_row_id,
        cached_word_rows: rows.cached_word_rows(),
        cache_limit_rows: DIFF_WORD_CACHE_ROWS,
        construct_micros,
        initial_window_micros,
        deep_window_micros,
        operation_limit_micros: DIFF_OPERATION_LIMIT_MICROS,
    }
}

fn diff_patch(index: usize) -> String {
    format!(
        "diff --git a/file-{index}.rs b/file-{index}.rs\n--- a/file-{index}.rs\n+++ b/file-{index}.rs\n@@ -1 +1 @@\n-old {index}\n+new {index}\n"
    )
}

fn cache_probe() -> CacheProbe {
    let workspace_id = WorkspaceId::new();
    let mut git = GitWorkbenchState::default();
    git.reset_workspace(workspace_id.clone());
    let git_insertions = GIT_DIFF_CACHE_ITEM_LIMIT + 8;
    for index in 0..git_insertions {
        let path = format!("file-{index}.rs");
        let ticket = git
            .begin_query(GitQueryKind::Diff, path.clone())
            .expect("workspace is selected");
        assert!(git.apply_diff(
            &ticket,
            GitDiffResponse {
                workspace_id: workspace_id.clone(),
                path,
                staged: false,
                diff: diff_patch(index),
                truncated: false,
            },
        ));
    }
    let oldest = GitSelectionKey {
        path: "file-0.rs".to_string(),
        staged: false,
    };
    let newest = GitSelectionKey {
        path: format!("file-{}.rs", git_insertions - 1),
        staged: false,
    };

    let mut image = BoundedImageCache::with_budget(8, 800);
    let image_insertions = 32;
    for index in 0..image_insertions {
        image
            .insert(
                ImageCacheKey {
                    path: format!("image-{index}.png"),
                    revision: format!("r{index}"),
                },
                5,
                5,
                100,
            )
            .expect("fixture image fits the bounded cache");
    }

    CacheProbe {
        git_insertions,
        git_resident_items: git.diffs.len(),
        git_item_limit: GIT_DIFF_CACHE_ITEM_LIMIT,
        oldest_git_item_evicted: !git.diffs.contains_key(&oldest),
        newest_git_item_resident: git.diffs.contains_key(&newest),
        image_insertions,
        image_resident_items: image.resident_items(),
        image_resident_bytes: image.resident_bytes(),
        image_evictions: image.evictions(),
    }
}

fn status(workspace_id: WorkspaceId) -> GitStatusSummary {
    GitStatusSummary {
        workspace_id,
        repo_path: ".".to_string(),
        branch: Some("main".to_string()),
        short_commit: Some("0000000".to_string()),
        detached: false,
        dirty: true,
        staged_count: 0,
        unstaged_count: 1,
        untracked_count: 0,
        changes: vec![GitChange {
            path: "src/lib.rs".to_string(),
            original_path: None,
            kind: GitChangeKind::Modified,
            staged: false,
            unstaged: true,
            additions: 1,
            deletions: 1,
        }],
        captured_at_ms: 1,
    }
}

fn generation_probe() -> GenerationProbe {
    let mut git = GitWorkbenchState::default();
    let mut stale_workspace_results_rejected = 0;
    for _ in 0..SWITCH_ITERATIONS {
        let previous = WorkspaceId::new();
        git.reset_workspace(previous.clone());
        let ticket = git
            .begin_query(GitQueryKind::Status, "status")
            .expect("workspace is selected");
        git.reset_workspace(WorkspaceId::new());
        if !git.apply_status(&ticket, status(previous)) {
            stale_workspace_results_rejected += 1;
        }
    }

    let workspace_id = WorkspaceId::new();
    git.reset_workspace(workspace_id.clone());
    let mut stale_revision_results_rejected = 0;
    for index in 0..SWITCH_ITERATIONS {
        let path = format!("revision-{index}.rs");
        let ticket = git
            .begin_query(GitQueryKind::Diff, path.clone())
            .expect("workspace is selected");
        git.invalidate_queries();
        if !git.apply_diff(
            &ticket,
            GitDiffResponse {
                workspace_id: workspace_id.clone(),
                path,
                staged: false,
                diff: diff_patch(index),
                truncated: false,
            },
        ) {
            stale_revision_results_rejected += 1;
        }
    }

    GenerationProbe {
        workspace_switches: SWITCH_ITERATIONS,
        stale_workspace_results_rejected,
        revision_switches: SWITCH_ITERATIONS,
        stale_revision_results_rejected,
        final_diff_cache_items: git.diffs.len(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tree = tree_probe();
    let diff = diff_probe();
    let caches = cache_probe();
    let generations = generation_probe();
    let compact_expanded_rows = TREE_ENTRY_COUNT - TREE_DEPTH + 2;
    let passed = tree.input_entries == TREE_ENTRY_COUNT
        && tree.compact_chain_segments == TREE_DEPTH
        && tree.expanded_visible_rows == compact_expanded_rows
        && tree.deep_window_rows <= 168
        && tree.search_visible_rows == 3
        && tree.search_leaf_found
        && tree.stale_load_rejected
        && tree.apply_micros <= TREE_OPERATION_LIMIT_MICROS
        && tree.expand_micros <= TREE_OPERATION_LIMIT_MICROS
        && tree.search_micros <= TREE_OPERATION_LIMIT_MICROS
        && diff.input_rows == DIFF_ROW_COUNT
        && diff.projected_rows == DIFF_ROW_COUNT
        && diff.initial_window_rows == INITIAL_DIFF_ROWS
        && diff.deep_window_rows == DEEP_WINDOW_ROWS
        && diff.deep_first_row_id == "diff:0:19500"
        && diff.cached_word_rows <= DIFF_WORD_CACHE_ROWS
        && diff.construct_micros <= DIFF_OPERATION_LIMIT_MICROS
        && diff.initial_window_micros <= DIFF_OPERATION_LIMIT_MICROS
        && diff.deep_window_micros <= DIFF_OPERATION_LIMIT_MICROS
        && caches.git_resident_items <= GIT_DIFF_CACHE_ITEM_LIMIT
        && caches.oldest_git_item_evicted
        && caches.newest_git_item_resident
        && caches.image_resident_items == 8
        && caches.image_resident_bytes == 800
        && caches.image_evictions == 24
        && generations.stale_workspace_results_rejected == SWITCH_ITERATIONS
        && generations.stale_revision_results_rejected == SWITCH_ITERATIONS
        && generations.final_diff_cache_items == 0;
    let report = CodeWorkbenchProbe {
        schema_version: "code-workbench-probe.v1",
        status: if passed { "passed" } else { "failed" },
        tree,
        diff,
        caches,
        generations,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !passed {
        std::process::exit(1);
    }
    Ok(())
}
