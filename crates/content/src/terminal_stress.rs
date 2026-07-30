//! Bounded Linux terminal stress evidence.
//!
//! The runner drives the same PTY manager and emulator used by the GPUI
//! surface. It deliberately keeps output transient: reports contain only
//! counts, hashes, timings, and process-resource observations.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use vibex_core::{
    TerminalCreateRequest, TerminalResizeRequest, TerminalSession, TerminalStatus, VibexError,
    VibexResult, WorkspaceId,
};
use vibex_terminal::{TerminalManager, TerminalRawOutputChunk, TerminalRawSnapshot};

use crate::{
    TERMINAL_MODEL_BUDGET_BYTES, TERMINAL_SCROLLBACK_LINES, TerminalFrameCache,
    TerminalSurfaceBackend,
};

pub const TERMINAL_STRESS_SCHEMA_VERSION: &str = "terminal-stress-linux-run.v1";
const TEN_MIB: usize = 10 * 1024 * 1024;
const RAW_CAPACITY: usize = 32 * 1024 * 1024;
const SCROLLBACK_LINES: usize = 10_000;
const BURST_FRAMES: usize = 120;
const DEFAULT_SOAK_SECONDS: u64 = 300;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStressRun {
    pub schema_version: &'static str,
    pub status: &'static str,
    pub platform: &'static str,
    pub architecture: &'static str,
    pub soak_requested_seconds: u64,
    pub soak_observed_seconds: u64,
    pub throughput: ThroughputStress,
    pub burst: BurstStress,
    pub scrollback: ScrollbackStress,
    pub lifecycle: LifecycleStress,
    pub resize: ResizeStress,
    pub sequence_rebuild: SequenceRebuildStress,
    pub soak: SoakStress,
    pub resources: ProcessResourceStress,
    pub privacy: StressPrivacy,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThroughputStress {
    pub fixture_bytes: usize,
    pub fixture_sha256: String,
    pub observed_sha256: String,
    pub elapsed_ms: u128,
    pub data_loss_observed: bool,
    pub raw_dropped_chunks: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BurstStress {
    pub requested_frames: usize,
    pub observed_frames: usize,
    pub elapsed_ms: u128,
    pub source_frames_per_second: f64,
    pub data_loss_observed: bool,
    pub max_snapshot_ms: u128,
    pub render_updates: u64,
    pub full_repaints: u64,
    pub partial_repaints: u64,
    pub changed_rows: u64,
    pub max_parse_frame_ms: u128,
    pub bounded_repaint: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollbackStress {
    pub requested_lines: usize,
    pub observed_history_lines: usize,
    pub frame_rows: u16,
    pub frame_columns: u16,
    pub model_resident_bytes: usize,
    pub model_budget_bytes: usize,
    pub bounded: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleStress {
    pub create_count: usize,
    pub restore_count: usize,
    pub kill_count: usize,
    pub all_sessions_closed: bool,
    pub statuses_valid: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizeStress {
    pub requested_rows: u16,
    pub requested_columns: u16,
    pub observed_rows: u16,
    pub observed_columns: u16,
    pub reflow_marker_observed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceRebuildStress {
    pub empty_incremental_snapshot: bool,
    pub full_retained_bytes: usize,
    pub incremental_bytes: usize,
    pub incremental_snapshot_bounded: bool,
    pub gap_injected: bool,
    pub rebuild_observed: bool,
    pub data_loss_observed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoakStress {
    pub requested_seconds: u64,
    pub observed_seconds: u64,
    pub activity_ticks: u64,
    pub snapshots: u64,
    pub sequence_gaps: u64,
    pub raw_dropped_chunks: u64,
    pub max_tick_ms: u128,
    pub render_updates: u64,
    pub full_repaints: u64,
    pub partial_repaints: u64,
    pub completed_requested_duration: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResourceStress {
    pub procfs_available: bool,
    pub baseline_rss_bytes: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
    pub final_rss_bytes: Option<u64>,
    pub rss_growth_bytes: Option<u64>,
    pub baseline_fd_count: Option<usize>,
    pub peak_fd_count: Option<usize>,
    pub final_fd_count: Option<usize>,
    pub baseline_child_count: Option<usize>,
    pub peak_child_count: Option<usize>,
    pub final_child_count: Option<usize>,
    pub fd_leak_observed: bool,
    pub child_leak_observed: bool,
    pub rss_budget_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StressPrivacy {
    pub raw_output_stored: bool,
    pub terminal_marker_stored: bool,
    pub workspace_path_stored: bool,
    pub environment_stored: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct ProcSample {
    rss_bytes: Option<u64>,
    fd_count: Option<usize>,
    child_count: Option<usize>,
}

/// Run the full stress matrix. A zero duration is useful for bounded unit and
/// CI checks; task evidence uses the user-approved five-minute duration.
pub fn run_terminal_stress(soak_seconds: Option<u64>) -> VibexResult<TerminalStressRun> {
    let requested_seconds = soak_seconds.unwrap_or(DEFAULT_SOAK_SECONDS);
    let workspace = stress_workspace()?;
    let manager = TerminalManager::with_raw_observation_capacity(2_000, RAW_CAPACITY);
    let baseline = proc_sample();
    let mut peak = baseline;

    let throughput = run_throughput(&manager, &workspace, &mut peak)?;
    close_all(&manager, &mut peak)?;
    let burst = run_burst(&manager, &workspace, &mut peak)?;
    close_all(&manager, &mut peak)?;
    let scrollback = run_scrollback(&manager, &workspace, &mut peak)?;
    close_all(&manager, &mut peak)?;
    let resize = run_resize(&manager, &workspace, &mut peak)?;
    close_all(&manager, &mut peak)?;
    let sequence_rebuild = run_sequence_rebuild(&manager, &workspace, &mut peak)?;
    close_all(&manager, &mut peak)?;
    let lifecycle = run_lifecycle(&manager, &workspace, &mut peak)?;
    close_all(&manager, &mut peak)?;
    let soak = run_soak(&manager, &workspace, requested_seconds, &mut peak)?;
    close_all(&manager, &mut peak)?;

    wait_for_no_children(baseline.child_count);
    let final_sample = proc_sample();
    peak = merge_peak(peak, final_sample);
    let resources = resource_report(baseline, peak, final_sample);
    let status = if throughput.data_loss_observed
        || throughput.raw_dropped_chunks != 0
        || burst.data_loss_observed
        || !(100.0..=130.0).contains(&burst.source_frames_per_second)
        || !burst.bounded_repaint
        || sequence_rebuild.data_loss_observed
        || !sequence_rebuild.empty_incremental_snapshot
        || !sequence_rebuild.incremental_snapshot_bounded
        || !scrollback.bounded
        || !lifecycle.all_sessions_closed
        || !lifecycle.statuses_valid
        || resize.requested_rows != resize.observed_rows
        || resize.requested_columns != resize.observed_columns
        || !resize.reflow_marker_observed
        || !sequence_rebuild.rebuild_observed
        || !soak.completed_requested_duration
        || soak.sequence_gaps != 0
        || soak.raw_dropped_chunks != 0
        || soak.snapshots < soak.activity_ticks
        || soak.render_updates != soak.activity_ticks
        || soak.full_repaints > 2
        || (requested_seconds > 0 && (soak.activity_ticks == 0 || soak.partial_repaints == 0))
        || (cfg!(target_os = "linux") && !resources.procfs_available)
        || resources.fd_leak_observed
        || resources.child_leak_observed
        || resources
            .rss_growth_bytes
            .is_some_and(|growth| growth > resources.rss_budget_bytes)
    {
        "failed"
    } else {
        "passed"
    };

    let observed_seconds = soak.observed_seconds;
    let report = TerminalStressRun {
        schema_version: TERMINAL_STRESS_SCHEMA_VERSION,
        status,
        platform: platform_name(),
        architecture: architecture_name(),
        soak_requested_seconds: requested_seconds,
        soak_observed_seconds: observed_seconds,
        throughput,
        burst,
        scrollback,
        lifecycle,
        resize,
        sequence_rebuild,
        soak,
        resources,
        privacy: StressPrivacy {
            raw_output_stored: false,
            terminal_marker_stored: false,
            workspace_path_stored: false,
            environment_stored: false,
        },
    };
    let _ = fs::remove_dir_all(&workspace);
    Ok(report)
}

fn run_throughput(
    manager: &TerminalManager,
    workspace: &Path,
    peak: &mut ProcSample,
) -> VibexResult<ThroughputStress> {
    let session = create_session(manager, workspace, "throughput", 24, 100)?;
    let expected = deterministic_fixture();
    let expected_hash = digest(&expected);
    let command = format!(
        "python3 -c \"import sys;d=bytes((i%251 for i in range({TEN_MIB})));w=sys.stdout.buffer.write;w(b'VIBEX_TP_BEGIN');w(d);w(b'VIBEX_TP_END');w(b'VIBEX_TP_DONE');sys.stdout.flush()\"\n"
    );
    let started = Instant::now();
    manager.write_bytes(&session.id, command.as_bytes())?;
    wait_for_marker(
        manager,
        &session.id,
        b"VIBEX_TP_DONE",
        Duration::from_secs(45),
        peak,
    )?;
    let snapshot = manager.raw_snapshot(&session.id)?;
    let body = extract_between(&snapshot, b"VIBEX_TP_BEGIN", b"VIBEX_TP_END").ok_or_else(|| {
        stress_error(
            "terminal_throughput_markers_missing",
            "10 MiB output markers were not observed",
        )
    })?;
    let observed_hash = digest(&body);
    Ok(ThroughputStress {
        fixture_bytes: expected.len(),
        fixture_sha256: expected_hash,
        observed_sha256: observed_hash.clone(),
        elapsed_ms: started.elapsed().as_millis(),
        data_loss_observed: body.len() != expected.len() || observed_hash != digest(&expected),
        raw_dropped_chunks: snapshot.dropped_chunks,
    })
}

fn run_burst(
    manager: &TerminalManager,
    workspace: &Path,
    peak: &mut ProcSample,
) -> VibexResult<BurstStress> {
    let session = create_session(manager, workspace, "burst", 24, 120)?;
    let started = Instant::now();
    let command = format!(
        "python3 -c \"import sys,time;s=time.perf_counter();w=sys.stdout.write;[(time.sleep(max(0,s+i/120-time.perf_counter())),w('\\x1b[2K\\x1b[H VIBEX_BURST_F%03d\\n'%i),sys.stdout.flush()) for i in range({BURST_FRAMES})];w('VIBEX_BURST_DONE\\n');sys.stdout.flush()\"\n"
    );
    manager.write_bytes(&session.id, command.as_bytes())?;
    let mut backend = TerminalSurfaceBackend::new(session.rows, session.cols);
    backend.lifecycle_mut().activate(1)?;
    let initial_frame = backend.frame();
    let mut cache = TerminalFrameCache::new(&initial_frame);
    let mut render_updates = 0_u64;
    let mut full_repaints = 1_u64;
    let mut partial_repaints = 0_u64;
    let mut changed_rows = u64::from(initial_frame.rows);
    let mut max_parse_frame_ms = 0_u128;
    let deadline = Instant::now() + Duration::from_secs(15);
    let snapshot = loop {
        if Instant::now() >= deadline {
            return Err(stress_error(
                "terminal_burst_timeout",
                "120 FPS terminal burst timed out",
            ));
        }
        let snapshot_started = Instant::now();
        let snapshot = manager.raw_snapshot(&session.id)?;
        let parse_started = Instant::now();
        let outcome = backend.sync(&snapshot)?;
        if outcome.ingested_chunks > 0 {
            let update = cache.apply(&backend.frame());
            render_updates += 1;
            full_repaints += u64::from(update.full_repaint);
            partial_repaints += u64::from(!update.full_repaint);
            changed_rows += update.changed_rows.len() as u64;
        }
        max_parse_frame_ms = max_parse_frame_ms.max(parse_started.elapsed().as_millis());
        *peak = merge_peak(*peak, proc_sample());
        if snapshot.dropped_chunks != 0 {
            return Err(stress_error(
                "terminal_raw_output_dropped",
                "terminal raw output ring dropped data",
            ));
        }
        if find_subslice(&flatten_snapshot(&snapshot), b"VIBEX_BURST_DONE").is_some() {
            break snapshot;
        }
        let remaining = Duration::from_millis(16).saturating_sub(snapshot_started.elapsed());
        thread::sleep(remaining);
    };
    let elapsed = started.elapsed();
    let snapshot_started = Instant::now();
    let final_snapshot = manager.raw_snapshot(&session.id)?;
    let max_snapshot_ms = snapshot_started.elapsed().as_millis();
    let bytes = flatten_snapshot(&final_snapshot);
    let observed_frames = count_marker(&bytes, b"VIBEX_BURST_F");
    let elapsed_seconds = elapsed.as_secs_f64().max(f64::EPSILON);
    let bounded_repaint = render_updates > 0
        && render_updates <= BURST_FRAMES as u64
        && full_repaints <= 2
        && partial_repaints > 0
        && changed_rows <= render_updates.saturating_add(1) * u64::from(session.rows)
        && max_parse_frame_ms <= 50;
    Ok(BurstStress {
        requested_frames: BURST_FRAMES,
        observed_frames: observed_frames.min(BURST_FRAMES),
        elapsed_ms: elapsed.as_millis(),
        source_frames_per_second: BURST_FRAMES as f64 / elapsed_seconds,
        data_loss_observed: observed_frames != BURST_FRAMES || snapshot.dropped_chunks != 0,
        max_snapshot_ms,
        render_updates,
        full_repaints,
        partial_repaints,
        changed_rows,
        max_parse_frame_ms,
        bounded_repaint,
    })
}

fn run_scrollback(
    manager: &TerminalManager,
    workspace: &Path,
    peak: &mut ProcSample,
) -> VibexResult<ScrollbackStress> {
    let session = create_session(manager, workspace, "scrollback", 24, 100)?;
    let command = format!(
        "python3 -c \"import sys;w=sys.stdout.write;[w('VIBEX_SCROLL_%05d\\n'%i) for i in range({SCROLLBACK_LINES})];w('VIBEX_SCROLL_DONE\\n');sys.stdout.flush()\"\n"
    );
    manager.write_bytes(&session.id, command.as_bytes())?;
    wait_for_marker(
        manager,
        &session.id,
        b"VIBEX_SCROLL_DONE",
        Duration::from_secs(30),
        peak,
    )?;
    let snapshot = manager.raw_snapshot(&session.id)?;
    let mut backend = TerminalSurfaceBackend::new(session.rows, session.cols);
    backend.lifecycle_mut().activate(1)?;
    backend.sync(&snapshot)?;
    let frame = backend.frame();
    let metrics = backend.resource_metrics();
    let bounded = frame.history_lines <= TERMINAL_SCROLLBACK_LINES
        && frame.history_lines >= SCROLLBACK_LINES.saturating_sub(1_000)
        && metrics.resident_bytes <= TERMINAL_MODEL_BUDGET_BYTES
        && snapshot.dropped_chunks == 0;
    Ok(ScrollbackStress {
        requested_lines: SCROLLBACK_LINES,
        observed_history_lines: frame.history_lines,
        frame_rows: frame.rows,
        frame_columns: frame.columns,
        model_resident_bytes: metrics.resident_bytes,
        model_budget_bytes: metrics.budget_bytes,
        bounded,
    })
}

fn run_resize(
    manager: &TerminalManager,
    workspace: &Path,
    peak: &mut ProcSample,
) -> VibexResult<ResizeStress> {
    let session = create_session(manager, workspace, "resize", 24, 80)?;
    let rows = 42;
    let columns = 132;
    let resized = manager.resize(&TerminalResizeRequest {
        terminal_id: session.id.clone(),
        rows,
        cols: columns,
    })?;
    let command = b"printf 'VIBEX_REFLOW_DONE\\n'\n";
    manager.write_bytes(&session.id, command)?;
    wait_for_marker(
        manager,
        &session.id,
        b"VIBEX_REFLOW_DONE",
        Duration::from_secs(5),
        peak,
    )?;
    Ok(ResizeStress {
        requested_rows: rows,
        requested_columns: columns,
        observed_rows: resized.rows,
        observed_columns: resized.cols,
        reflow_marker_observed: true,
    })
}

fn run_sequence_rebuild(
    manager: &TerminalManager,
    workspace: &Path,
    peak: &mut ProcSample,
) -> VibexResult<SequenceRebuildStress> {
    let session = create_session(manager, workspace, "sequence", 24, 80)?;
    manager.write_bytes(&session.id, b"printf 'VIBEX_SEQUENCE_DONE\\n'\n")?;
    wait_for_marker(
        manager,
        &session.id,
        b"VIBEX_SEQUENCE_DONE",
        Duration::from_secs(5),
        peak,
    )?;
    thread::sleep(Duration::from_millis(50));
    let snapshot = manager.raw_snapshot(&session.id)?;
    let mut backend = TerminalSurfaceBackend::new(session.rows, session.cols);
    backend.lifecycle_mut().activate(1)?;
    backend.sync(&snapshot)?;
    let settled = manager.raw_snapshot_from(&session.id, backend.next_sequence())?;
    backend.sync(&settled)?;
    let empty = manager.raw_snapshot_from(&session.id, backend.next_sequence())?;
    manager.write_bytes(&session.id, b"printf 'VIBEX_INCREMENTAL_DONE\\n'\n")?;
    wait_for_marker(
        manager,
        &session.id,
        b"VIBEX_INCREMENTAL_DONE",
        Duration::from_secs(5),
        peak,
    )?;
    let full_after_incremental = manager.raw_snapshot(&session.id)?;
    let incremental = manager.raw_snapshot_from(&session.id, backend.next_sequence())?;
    let incremental_bytes = incremental.retained_bytes;
    backend.sync(&incremental)?;
    let incremental_text = backend
        .frame()
        .cells
        .into_iter()
        .map(|cell| cell.text)
        .collect::<String>();
    let rebuild_marker = b"VIBEX_REBUILT".to_vec();
    let mut gapped = full_after_incremental.clone();
    gapped.chunks = vec![TerminalRawOutputChunk {
        sequence: full_after_incremental.next_sequence + 1,
        data: rebuild_marker.clone(),
    }];
    gapped.next_sequence = full_after_incremental.next_sequence + 2;
    gapped.retained_bytes = rebuild_marker.len();
    gapped.dropped_chunks = full_after_incremental.dropped_chunks + 1;
    let outcome = backend.sync(&gapped)?;
    let rebuilt_text = backend
        .frame()
        .cells
        .into_iter()
        .map(|cell| cell.text)
        .collect::<String>();
    Ok(SequenceRebuildStress {
        empty_incremental_snapshot: empty.chunks.is_empty() && empty.retained_bytes == 0,
        full_retained_bytes: full_after_incremental.retained_bytes,
        incremental_bytes,
        incremental_snapshot_bounded: incremental_bytes > 0
            && incremental_bytes < full_after_incremental.retained_bytes,
        gap_injected: true,
        rebuild_observed: outcome.rebuilt,
        data_loss_observed: !incremental_text.contains("VIBEX_INCREMENTAL_DONE")
            || !rebuilt_text.contains("VIBEX_REBUILT"),
    })
}

fn run_lifecycle(
    manager: &TerminalManager,
    workspace: &Path,
    peak: &mut ProcSample,
) -> VibexResult<LifecycleStress> {
    let iterations = 100;
    let mut session = create_session(manager, workspace, "lifecycle", 24, 80)?;
    let mut creates = 1;
    let mut restores = 0;
    let mut kills = 0;
    let mut statuses_valid = true;
    for index in 0..iterations {
        let marker = format!("VIBEX_LIFE_{index:02}");
        manager.write_bytes(&session.id, format!("printf '{marker}\\n'\n").as_bytes())?;
        wait_for_marker(
            manager,
            &session.id,
            marker.as_bytes(),
            Duration::from_secs(5),
            peak,
        )?;
        let killed = manager.kill(&session.id)?;
        statuses_valid &= killed.status == TerminalStatus::Killed;
        kills += 1;
        session = manager.restore(workspace, killed)?;
        restores += 1;
        creates += 1;
        statuses_valid &= session.status == TerminalStatus::Running;
    }
    let final_killed = manager.kill(&session.id)?;
    kills += 1;
    statuses_valid &= final_killed.status == TerminalStatus::Killed;
    let all_sessions_closed = manager.list(&session.workspace_id)?.is_empty();
    Ok(LifecycleStress {
        create_count: creates,
        restore_count: restores,
        kill_count: kills,
        all_sessions_closed,
        statuses_valid,
    })
}

fn run_soak(
    manager: &TerminalManager,
    workspace: &Path,
    requested_seconds: u64,
    peak: &mut ProcSample,
) -> VibexResult<SoakStress> {
    let session = create_session(manager, workspace, "soak", 24, 100)?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(requested_seconds);
    let mut activity_ticks = 0_u64;
    let mut snapshots = 0_u64;
    let mut sequence_gaps = 0_u64;
    let mut raw_dropped_chunks = 0_u64;
    let mut max_tick_ms = 0_u128;
    let mut render_updates = 0_u64;
    let mut full_repaints = 1_u64;
    let mut partial_repaints = 0_u64;
    let mut backend = TerminalSurfaceBackend::new(session.rows, session.cols);
    backend.lifecycle_mut().activate(1)?;
    let initial_snapshot = manager.raw_snapshot_from(&session.id, backend.next_sequence())?;
    let initial_outcome = backend.sync(&initial_snapshot)?;
    sequence_gaps += u64::from(initial_outcome.gap_detected);
    raw_dropped_chunks = raw_dropped_chunks.max(initial_outcome.dropped_chunks);
    let initial_frame = backend.frame();
    let mut cache = TerminalFrameCache::new(&initial_frame);
    while Instant::now() < deadline {
        let tick_started = Instant::now();
        let command = format!(
            "printf '\\033[2K\\033[HVIBEX_SOAK_TICK_%08d' {}\n",
            activity_ticks.saturating_add(1)
        );
        manager.write_bytes(&session.id, command.as_bytes())?;

        let output_deadline = Instant::now() + Duration::from_millis(200);
        let mut update_observed = false;
        while Instant::now() < output_deadline {
            let snapshot = manager.raw_snapshot_from(&session.id, backend.next_sequence())?;
            let outcome = backend.sync(&snapshot)?;
            sequence_gaps += u64::from(outcome.gap_detected);
            raw_dropped_chunks = raw_dropped_chunks.max(outcome.dropped_chunks);
            snapshots += 1;
            if outcome.ingested_chunks > 0 {
                let update = cache.apply(&backend.frame());
                render_updates += 1;
                full_repaints += u64::from(update.full_repaint);
                partial_repaints += u64::from(!update.full_repaint);
                update_observed = true;
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        if !update_observed {
            return Err(stress_error(
                "terminal_soak_activity_timeout",
                "terminal soak output did not reach the parser",
            ));
        }
        activity_ticks += 1;
        max_tick_ms = max_tick_ms.max(tick_started.elapsed().as_millis());
        *peak = merge_peak(*peak, proc_sample());
        let tick_remaining = Duration::from_millis(240).saturating_sub(tick_started.elapsed());
        let soak_remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(soak_remaining.min(tick_remaining));
    }
    let observed_seconds = started.elapsed().as_secs();
    Ok(SoakStress {
        requested_seconds,
        observed_seconds,
        activity_ticks,
        snapshots,
        sequence_gaps,
        raw_dropped_chunks,
        max_tick_ms,
        render_updates,
        full_repaints,
        partial_repaints,
        completed_requested_duration: observed_seconds >= requested_seconds,
    })
}

fn create_session(
    manager: &TerminalManager,
    workspace: &Path,
    title: &str,
    rows: u16,
    cols: u16,
) -> VibexResult<TerminalSession> {
    let session = manager.create(
        workspace,
        TerminalCreateRequest {
            workspace_id: WorkspaceId::new(),
            title: Some(title.to_string()),
            shell: Some(shell_path()),
            cwd: Some(workspace.display().to_string()),
            rows,
            cols,
        },
    )?;
    #[cfg(not(target_os = "windows"))]
    {
        manager.write_bytes(&session.id, b"stty -echo -opost\n")?;
        thread::sleep(Duration::from_millis(50));
    }
    Ok(session)
}

fn close_all(manager: &TerminalManager, peak: &mut ProcSample) -> VibexResult<()> {
    let report = manager.shutdown_all()?;
    if !report.failures.is_empty() {
        return Err(stress_error(
            "terminal_shutdown_failed",
            "terminal shutdown left active sessions",
        ));
    }
    thread::sleep(Duration::from_millis(50));
    *peak = merge_peak(*peak, proc_sample());
    wait_for_no_children(None);
    Ok(())
}

fn wait_for_marker(
    manager: &TerminalManager,
    terminal_id: &vibex_core::TerminalId,
    marker: &[u8],
    timeout: Duration,
    peak: &mut ProcSample,
) -> VibexResult<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let snapshot = manager.raw_snapshot(terminal_id)?;
        *peak = merge_peak(*peak, proc_sample());
        if snapshot.dropped_chunks != 0 {
            return Err(stress_error(
                "terminal_raw_output_dropped",
                "terminal raw output ring dropped data",
            ));
        }
        if find_subslice(&flatten_snapshot(&snapshot), marker).is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(stress_error(
        "terminal_marker_timeout",
        "terminal stress marker timed out",
    ))
}

fn extract_between(snapshot: &TerminalRawSnapshot, begin: &[u8], end: &[u8]) -> Option<Vec<u8>> {
    let bytes = flatten_snapshot(snapshot);
    let start = find_subslice(&bytes, begin)? + begin.len();
    let finish = find_subslice(&bytes[start..], end)? + start;
    Some(bytes[start..finish].to_vec())
}

fn flatten_snapshot(snapshot: &TerminalRawSnapshot) -> Vec<u8> {
    snapshot
        .chunks
        .iter()
        .flat_map(|chunk| chunk.data.iter().copied())
        .collect()
}

fn deterministic_fixture() -> Vec<u8> {
    (0..TEN_MIB).map(|index| (index % 251) as u8).collect()
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn count_marker(bytes: &[u8], marker: &[u8]) -> usize {
    bytes
        .windows(marker.len())
        .filter(|window| *window == marker)
        .count()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn stress_workspace() -> VibexResult<PathBuf> {
    let path = std::env::temp_dir().join(format!("vibex-terminal-stress-{}", std::process::id()));
    fs::create_dir_all(&path).map_err(|error| {
        stress_error(
            "terminal_stress_workspace_failed",
            &format!("workspace setup failed: {error}"),
        )
    })?;
    Ok(path)
}

fn shell_path() -> String {
    #[cfg(target_os = "windows")]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        "/bin/sh".to_string()
    }
}

fn proc_sample() -> ProcSample {
    #[cfg(target_os = "linux")]
    {
        ProcSample {
            rss_bytes: read_rss_bytes(),
            fd_count: fs::read_dir("/proc/self/fd")
                .ok()
                .map(|entries| entries.count()),
            child_count: Some(child_pids().len()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        ProcSample::default()
    }
}

#[cfg(target_os = "linux")]
fn read_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let value = line
            .strip_prefix("VmRSS:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?;
        Some(value.saturating_mul(1024))
    })
}

#[cfg(target_os = "linux")]
fn child_pids() -> BTreeSet<u32> {
    let parent = std::process::id();
    let mut children = BTreeSet::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return children;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let is_child = status.lines().any(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<u32>().ok())
                == Some(parent)
        });
        if is_child {
            children.insert(pid);
        }
    }
    children
}

fn merge_peak(left: ProcSample, right: ProcSample) -> ProcSample {
    ProcSample {
        rss_bytes: max_opt(left.rss_bytes, right.rss_bytes),
        fd_count: max_opt(left.fd_count, right.fd_count),
        child_count: max_opt(left.child_count, right.child_count),
    }
}

fn max_opt<T: Ord + Copy>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn resource_report(
    baseline: ProcSample,
    peak: ProcSample,
    final_sample: ProcSample,
) -> ProcessResourceStress {
    let rss_growth_bytes = match (baseline.rss_bytes, final_sample.rss_bytes) {
        (Some(start), Some(end)) => Some(end.saturating_sub(start)),
        _ => None,
    };
    let fd_leak_observed = match (baseline.fd_count, final_sample.fd_count) {
        (Some(start), Some(end)) => end > start.saturating_add(2),
        _ => false,
    };
    let child_leak_observed = match (baseline.child_count, final_sample.child_count) {
        (Some(start), Some(end)) => end > start,
        _ => false,
    };
    ProcessResourceStress {
        procfs_available: baseline.rss_bytes.is_some() && baseline.fd_count.is_some(),
        baseline_rss_bytes: baseline.rss_bytes,
        peak_rss_bytes: peak.rss_bytes,
        final_rss_bytes: final_sample.rss_bytes,
        rss_growth_bytes,
        baseline_fd_count: baseline.fd_count,
        peak_fd_count: peak.fd_count,
        final_fd_count: final_sample.fd_count,
        baseline_child_count: baseline.child_count,
        peak_child_count: peak.child_count,
        final_child_count: final_sample.child_count,
        fd_leak_observed,
        child_leak_observed,
        rss_budget_bytes: 64 * 1024 * 1024,
    }
}

fn wait_for_no_children(expected: Option<usize>) {
    #[cfg(target_os = "linux")]
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if child_pids().len() <= expected.unwrap_or(0) {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

fn stress_error(code: &'static str, message: &str) -> VibexError {
    VibexError::process(code, message)
}

fn platform_name() -> &'static str {
    std::env::consts::OS
}

fn architecture_name() -> &'static str {
    std::env::consts::ARCH
}
