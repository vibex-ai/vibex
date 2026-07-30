use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use vibex_core::{
    TerminalCreateRequest, TerminalResizeRequest, TerminalStatus, VibexError, VibexResult,
    WorkspaceId,
};

use crate::{TerminalEmulator, TerminalGridPoint, TerminalManager};

const RAW_FIXTURE_BYTES: usize = 10 * 1024 * 1024;
const RAW_OBSERVATION_CAPACITY: usize = 12 * 1024 * 1024;
const RAW_BEGIN: &[u8] = b"\x1eVIBEX_RAW_BEGIN\x1f";
const RAW_END: &[u8] = b"\x1eVIBEX_RAW_END\x1f";
const INVALID_UTF8: &[u8] = b"\xff\xfe";
const CJK_FIXTURE: &[u8] = b"VIBEX_CJK:\xe4\xbd\xa0\xe5\xa5\xbd";
const EXPECTED_ROWS: u16 = 42;
const EXPECTED_COLUMNS: u16 = 132;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFeasibilityRun {
    schema_version: &'static str,
    status: &'static str,
    platform: &'static str,
    architecture: &'static str,
    engine: EngineRun,
    pty: PtyRun,
    emulator: EmulatorRun,
    throughput: ThroughputRun,
    raw_text_stored: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EngineRun {
    name: &'static str,
    version: &'static str,
    integration: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PtyRun {
    backend: &'static str,
    windows_conpty_exercised: bool,
    raw_bytes_observed: bool,
    invalid_utf8_observed: bool,
    cjk_observed: bool,
    resize_requested: TerminalDimensions,
    resize_observed: bool,
    input_bytes_written: usize,
    process_exited: bool,
    raw_dropped_chunks: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorRun {
    cjk_cells_observed: bool,
    selection_copy_observed: bool,
    alternate_screen_entered: bool,
    primary_screen_restored: bool,
    resize_observed: bool,
    ingested_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThroughputRun {
    fixture_bytes: usize,
    fixture_sha256: String,
    elapsed_ms: u128,
    mebibytes_per_second: f64,
    data_loss_observed: bool,
}

#[derive(Serialize)]
struct TerminalDimensions {
    rows: u16,
    columns: u16,
}

pub fn run_terminal_feasibility() -> VibexResult<TerminalFeasibilityRun> {
    let manager = TerminalManager::with_raw_observation_capacity(128, RAW_OBSERVATION_CAPACITY);
    let session = manager.create(
        std::env::temp_dir(),
        TerminalCreateRequest {
            workspace_id: WorkspaceId::new(),
            title: Some("terminal-feasibility".to_string()),
            shell: None,
            cwd: None,
            rows: 24,
            cols: 80,
        },
    )?;

    manager.write_bytes(&session.id, echo_disable_command())?;
    thread::sleep(Duration::from_millis(150));
    manager.resize(&TerminalResizeRequest {
        terminal_id: session.id.clone(),
        rows: EXPECTED_ROWS,
        cols: EXPECTED_COLUMNS,
    })?;

    let command = fixture_command();
    let started = Instant::now();
    manager.write_bytes(&session.id, command.as_bytes())?;
    wait_for_text_marker(
        &manager,
        &session.id,
        "\u{1e}VIBEX_DONE\u{1f}",
        Duration::from_secs(30),
    )?;
    let elapsed = started.elapsed();

    let raw_snapshot = manager.raw_snapshot(&session.id)?;
    ensure(
        raw_snapshot.dropped_chunks == 0,
        "raw observation buffer dropped chunks",
    )?;
    let raw = raw_snapshot
        .chunks
        .iter()
        .flat_map(|chunk| chunk.data.iter().copied())
        .collect::<Vec<_>>();
    let raw_start = find_subslice(&raw, RAW_BEGIN)
        .map(|offset| offset + RAW_BEGIN.len())
        .ok_or_else(|| {
            feasibility_error(
                "terminal_raw_start_missing",
                "raw fixture start marker was not observed",
            )
        })?;
    let raw_end = find_subslice(&raw[raw_start..], RAW_END)
        .map(|offset| raw_start + offset)
        .ok_or_else(|| {
            feasibility_error(
                "terminal_raw_end_missing",
                "raw fixture end marker was not observed",
            )
        })?;
    let fixture = &raw[raw_start..raw_end];
    ensure(
        fixture.len() == RAW_FIXTURE_BYTES && fixture.iter().all(|byte| *byte == 0),
        "10 MiB raw fixture changed in transit",
    )?;
    let suffix = &raw[raw_end + RAW_END.len()..];
    let invalid_utf8_observed = find_subslice(suffix, INVALID_UTF8).is_some();
    let cjk_observed = find_subslice(suffix, CJK_FIXTURE).is_some();
    let expected_size = format!("VIBEX_SIZE:{EXPECTED_ROWS} {EXPECTED_COLUMNS}");
    let resize_observed = find_subslice(suffix, expected_size.as_bytes()).is_some();
    ensure(
        invalid_utf8_observed,
        "non-UTF-8 PTY bytes were not preserved",
    )?;
    ensure(cjk_observed, "CJK PTY bytes were not preserved")?;
    ensure(
        resize_observed,
        "child process did not observe the requested PTY size",
    )?;

    manager.write_bytes(&session.id, exit_command())?;
    let process_exited = wait_for_exit(&manager, &session.id, Duration::from_secs(5))?;
    ensure(process_exited, "PTY shell did not exit cleanly")?;

    let emulator = exercise_emulator()?;
    let digest = Sha256::digest(fixture);
    let elapsed_seconds = elapsed.as_secs_f64().max(f64::EPSILON);
    Ok(TerminalFeasibilityRun {
        schema_version: "vibex-terminal-feasibility-run.v1",
        status: "passed",
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        engine: EngineRun {
            name: "alacritty_terminal",
            version: "0.26.0",
            integration: "termy-compatible-bounded-core-fallback",
        },
        pty: PtyRun {
            backend: native_pty_backend(),
            windows_conpty_exercised: cfg!(target_os = "windows"),
            raw_bytes_observed: true,
            invalid_utf8_observed,
            cjk_observed,
            resize_requested: TerminalDimensions {
                rows: EXPECTED_ROWS,
                columns: EXPECTED_COLUMNS,
            },
            resize_observed,
            input_bytes_written: command.len(),
            process_exited,
            raw_dropped_chunks: raw_snapshot.dropped_chunks,
        },
        emulator,
        throughput: ThroughputRun {
            fixture_bytes: fixture.len(),
            fixture_sha256: format!("{digest:x}"),
            elapsed_ms: elapsed.as_millis(),
            mebibytes_per_second: 10.0 / elapsed_seconds,
            data_loss_observed: false,
        },
        raw_text_stored: false,
    })
}

fn exercise_emulator() -> VibexResult<EmulatorRun> {
    let mut emulator = TerminalEmulator::new(4, 40);
    emulator.advance("primary 中文".as_bytes());
    let cjk_cells_observed = emulator.visible_text().contains("中文");
    let selected = emulator.select_text(
        TerminalGridPoint { row: 0, column: 0 },
        TerminalGridPoint { row: 0, column: 6 },
    )?;
    let selection_copy_observed = selected == "primary";
    emulator.advance(b"\x1b[?1049h");
    let alternate_screen_entered = emulator.alternate_screen_active();
    emulator.advance(b"alternate");
    emulator.advance(b"\x1b[?1049l");
    let primary_screen_restored =
        !emulator.alternate_screen_active() && emulator.visible_text().contains("primary");
    emulator.resize(30, 100);
    let resize_observed = (emulator.rows(), emulator.columns()) == (30, 100);
    ensure(
        cjk_cells_observed,
        "Alacritty model did not retain CJK cells",
    )?;
    ensure(
        selection_copy_observed,
        "Alacritty selection did not reproduce the selected text",
    )?;
    ensure(
        alternate_screen_entered,
        "Alacritty model did not enter alternate screen",
    )?;
    ensure(
        primary_screen_restored,
        "Alacritty model did not restore primary screen",
    )?;
    ensure(resize_observed, "Alacritty model did not resize")?;
    Ok(EmulatorRun {
        cjk_cells_observed,
        selection_copy_observed,
        alternate_screen_entered,
        primary_screen_restored,
        resize_observed,
        ingested_bytes: emulator.ingested_bytes(),
    })
}

fn wait_for_text_marker(
    manager: &TerminalManager,
    terminal_id: &vibex_core::TerminalId,
    marker: &str,
    timeout: Duration,
) -> VibexResult<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let snapshot = manager.snapshot(terminal_id)?;
        if snapshot
            .chunks
            .iter()
            .any(|chunk| chunk.data.contains(marker))
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(feasibility_error(
        "terminal_fixture_timeout",
        "timed out waiting for terminal fixture output",
    ))
}

fn wait_for_exit(
    manager: &TerminalManager,
    terminal_id: &vibex_core::TerminalId,
    timeout: Duration,
) -> VibexResult<bool> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if manager.snapshot(terminal_id)?.session.status == TerminalStatus::Exited {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(false)
}

fn fixture_command() -> String {
    let script = format!(
        "import os,sys;w=sys.stdout.buffer.write;w(b'\\x1eVIBEX_RAW_BEGIN\\x1f');w(bytes({RAW_FIXTURE_BYTES}));w(b'\\x1eVIBEX_RAW_END\\x1f\\xff\\xfeVIBEX_CJK:\\xe4\\xbd\\xa0\\xe5\\xa5\\xbd');s=os.get_terminal_size();w(f'VIBEX_SIZE:{{s.lines}} {{s.columns}}'.encode());w(b'\\x1eVIBEX_DONE\\x1f');sys.stdout.flush()"
    );
    format!("{} -c \"{}\"{}", python_command(), script, line_ending())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn ensure(condition: bool, message: &'static str) -> VibexResult<()> {
    if condition {
        Ok(())
    } else {
        Err(feasibility_error("terminal_feasibility_failed", message))
    }
}

fn feasibility_error(code: &'static str, message: &'static str) -> VibexError {
    VibexError::process(code, message)
}

#[cfg(target_os = "windows")]
fn native_pty_backend() -> &'static str {
    "portable-pty-conpty"
}

#[cfg(not(target_os = "windows"))]
fn native_pty_backend() -> &'static str {
    "portable-pty-openpty"
}

#[cfg(target_os = "windows")]
fn python_command() -> &'static str {
    "python"
}

#[cfg(not(target_os = "windows"))]
fn python_command() -> &'static str {
    "python3"
}

#[cfg(target_os = "windows")]
fn echo_disable_command() -> &'static [u8] {
    b"@echo off\r\n"
}

#[cfg(not(target_os = "windows"))]
fn echo_disable_command() -> &'static [u8] {
    b"stty -echo\n"
}

#[cfg(target_os = "windows")]
fn exit_command() -> &'static [u8] {
    b"exit\r\n"
}

#[cfg(not(target_os = "windows"))]
fn exit_command() -> &'static [u8] {
    b"exit\n"
}

#[cfg(target_os = "windows")]
fn line_ending() -> &'static str {
    "\r\n"
}

#[cfg(not(target_os = "windows"))]
fn line_ending() -> &'static str {
    "\n"
}
