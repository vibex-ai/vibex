//! Records what the developer FPS HUD measures into the diagnostics folder.
//!
//! gpui-fps publishes its readings to the screen and exposes no accessor for
//! them, so this records the same measurements from the same sources the HUD
//! reads: GPUI's own frame trace, filtered to the workbench window exactly as
//! the HUD filters it, plus this process' CPU and resident memory. While the
//! Developer switch is on, one line per [`SAMPLE_INTERVAL`] is appended to
//! `diagnostics/fps-monitor.jsonl` — the folder the Data & Diagnostics cleanup
//! removes and the storage usage report counts as diagnostic data.
//!
//! The HUD grades a rolling window of the last frames, so two readings half a
//! second apart describe mostly the same frames. Each line here summarizes one
//! interval instead, which keeps consecutive lines independent enough to be
//! summed and averaged by whoever reads the file later.

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui::{FrameEvent, FrameTimingCollector, WindowId};
use serde::Serialize;
use vibex_core::unix_timestamp_ms;

/// The diagnostics directory the runtime home holds the samples in.
const DIAGNOSTICS_DIRECTORY: &str = "diagnostics";
/// The file samples are appended to, inside [`DIAGNOSTICS_DIRECTORY`].
pub const FPS_MONITOR_LOG_FILE: &str = "fps-monitor.jsonl";
/// The one rotated file kept beside it, so the newest samples survive a
/// rotation.
pub const FPS_MONITOR_ROTATED_LOG_FILE: &str = "fps-monitor.1.jsonl";
/// How often an interval is summarized into a line.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// The frame budget the HUD grades against — one frame at 60Hz — so
/// `droppedPercent` means what the HUD's `DROP` row means.
pub const FRAME_BUDGET: Duration = Duration::from_nanos(16_666_667);
/// Stamped into every line so a later reader can tell formats apart.
pub const SAMPLE_SCHEMA_VERSION: u32 = 1;
/// Roughly ten hours of continuous HUD time: a forgotten HUD rotates rather
/// than filling the disk.
const LOG_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Where the developer HUD's samples are recorded for a runtime home.
pub fn fps_monitor_log_path(home: &Path) -> PathBuf {
    home.join(DIAGNOSTICS_DIRECTORY).join(FPS_MONITOR_LOG_FILE)
}

/// One interval of the FPS HUD's measurements.
///
/// Every field is what the HUD's corresponding row would have shown over the
/// same interval, except that the frame statistics are computed over the
/// interval rather than over the HUD's rolling window.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FpsMonitorSample {
    pub schema_version: u32,
    pub recorded_at_ms: i64,
    /// Frames this window drew inside the interval.
    pub frames: u32,
    /// Frames it presented inside the interval.
    pub presented: u32,
    /// Presented frames per second, derived the way the HUD derives it.
    pub fps: f32,
    /// Mean time inside `Window::draw`, in milliseconds.
    pub frame_millis: f32,
    /// The slow tail: the draw time 95% of the interval's frames came in at or
    /// under, in milliseconds.
    pub p95_millis: f32,
    /// The slowest frame of the interval, in milliseconds.
    pub worst_millis: f32,
    /// Share of the interval's frames that overran [`FRAME_BUDGET`].
    pub dropped_percent: f32,
    /// Mean invalidations coalesced into one frame.
    pub invalidations: f32,
    /// This process' CPU on the scale 100 is one saturated logical core, the
    /// same figure the HUD's `CPU` row shows.
    pub cpu_percent: Option<f32>,
    /// Resident set size. The HUD's `MEM` row reads the responsible-memory
    /// counter its platform module keeps private, so this is the same order of
    /// magnitude rather than always the same number.
    pub resident_memory_bytes: Option<u64>,
}

/// Drains GPUI's frame trace and summarizes each interval into a line.
pub struct FpsMonitorRecorder {
    path: PathBuf,
    collector: FrameTimingCollector,
    window_id: WindowId,
    interval: IntervalFrames,
    resources: Option<ProcessResources>,
}

impl FpsMonitorRecorder {
    /// A recorder that only sees frames drawn from this point on.
    pub fn new(window_id: WindowId, path: PathBuf) -> Self {
        Self {
            path,
            collector: FrameTimingCollector::new(),
            window_id,
            interval: IntervalFrames::default(),
            resources: ProcessResources::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The next line to append, or `None` when the window drew nothing since
    /// the previous call — an idle workbench has no interval worth recording.
    pub fn sample_line(&mut self) -> Option<String> {
        self.interval.drain(&mut self.collector, self.window_id);
        if self.interval.draws.is_empty() {
            // Whatever this interval saw belongs to it, not to the next one.
            self.interval.clear();
            return None;
        }
        let (cpu_percent, resident_memory_bytes) = self
            .resources
            .as_mut()
            .map(ProcessResources::read)
            .unwrap_or((None, None));
        let summary = self.interval.take_summary();
        serde_json::to_string(&FpsMonitorSample {
            schema_version: SAMPLE_SCHEMA_VERSION,
            recorded_at_ms: unix_timestamp_ms(),
            frames: summary.frames,
            presented: summary.presented,
            fps: summary.fps,
            frame_millis: summary.frame_millis,
            p95_millis: summary.p95_millis,
            worst_millis: summary.worst_millis,
            dropped_percent: summary.dropped_percent,
            invalidations: summary.invalidations,
            cpu_percent,
            resident_memory_bytes,
        })
        .ok()
    }
}

/// Appends one recorded line, rotating the log once it outgrows
/// [`LOG_MAX_BYTES`].
pub fn append_sample_line(path: &Path, line: &str) -> io::Result<()> {
    append_sample_line_with_limit(path, line, LOG_MAX_BYTES)
}

fn append_sample_line_with_limit(path: &Path, line: &str, limit: u64) -> io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let over_limit = fs::metadata(path)
        .map(|metadata| metadata.len() >= limit)
        .unwrap_or(false);
    if over_limit {
        rotate_log(path)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()
}

fn rotate_log(path: &Path) -> io::Result<()> {
    let Some(rotated) = path
        .file_name()
        .map(|_| path.with_file_name(FPS_MONITOR_ROTATED_LOG_FILE))
    else {
        return Ok(());
    };
    match fs::remove_file(&rotated) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(path, rotated)
}

/// The frames one interval collected, and what they add up to.
#[derive(Default)]
struct IntervalFrames {
    draws: Vec<Duration>,
    invalidations: u64,
    present_times: Vec<Instant>,
}

impl IntervalFrames {
    fn drain(&mut self, collector: &mut FrameTimingCollector, window_id: WindowId) {
        for event in collector.collect_unseen() {
            match event {
                FrameEvent::Draw(timing) if timing.window_id == window_id => {
                    self.draws.push(timing.draw_duration());
                    self.invalidations = self.invalidations.saturating_add(timing.invalidations);
                }
                FrameEvent::Present(timing) if timing.window_id == window_id => {
                    self.present_times.push(timing.present_end);
                }
                FrameEvent::Draw(_) | FrameEvent::Present(_) => {}
            }
        }
    }

    fn take_summary(&mut self) -> FrameSummary {
        let summary = summarize_frames(
            &self.draws,
            self.invalidations,
            &self.present_times,
            FRAME_BUDGET,
        );
        self.clear();
        summary
    }

    fn clear(&mut self) {
        self.draws.clear();
        self.invalidations = 0;
        self.present_times.clear();
    }
}

/// What one interval's frames add up to, using the HUD's definitions.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FrameSummary {
    frames: u32,
    presented: u32,
    fps: f32,
    frame_millis: f32,
    p95_millis: f32,
    worst_millis: f32,
    dropped_percent: f32,
    invalidations: f32,
}

fn summarize_frames(
    draws: &[Duration],
    invalidations: u64,
    present_times: &[Instant],
    budget: Duration,
) -> FrameSummary {
    if draws.is_empty() {
        return FrameSummary {
            frames: 0,
            presented: present_times.len() as u32,
            fps: 0.,
            frame_millis: 0.,
            p95_millis: 0.,
            worst_millis: 0.,
            dropped_percent: 0.,
            invalidations: 0.,
        };
    }

    let total: Duration = draws.iter().sum();
    let mean = total / draws.len() as u32;
    let worst = draws.iter().max().copied().unwrap_or_default();
    let over_budget = draws.iter().filter(|draw| **draw > budget).count();

    FrameSummary {
        frames: draws.len() as u32,
        presented: present_times.len() as u32,
        fps: presented_rate(present_times),
        frame_millis: millis(mean),
        p95_millis: millis(percentile_draw(draws, 0.95)),
        worst_millis: millis(worst),
        dropped_percent: over_budget as f32 / draws.len() as f32 * 100.,
        invalidations: invalidations as f32 / draws.len() as f32,
    }
}

fn millis(duration: Duration) -> f32 {
    duration.as_secs_f32() * 1000.
}

/// The HUD's rate: `n` frames span `n - 1` intervals, so the rate comes from
/// the elapsed span rather than from the count.
fn presented_rate(present_times: &[Instant]) -> f32 {
    let (Some(oldest), Some(newest)) = (present_times.first(), present_times.last()) else {
        return 0.;
    };
    let span = newest.duration_since(*oldest).as_secs_f32();
    if present_times.len() < 2 || span <= 0. {
        return 0.;
    }
    (present_times.len() - 1) as f32 / span
}

/// The draw time 95% of the frames came in at or under, ranked nearest rather
/// than interpolated so every number in the log is a frame that was drawn.
fn percentile_draw(draws: &[Duration], percentile: f32) -> Duration {
    let mut sorted = draws.to_vec();
    sorted.sort_unstable();
    let last = sorted.len() - 1;
    let rank = (percentile.clamp(0., 1.) * last as f32).round() as usize;
    sorted[rank.min(last)]
}

/// This process' CPU and memory, read the way the HUD's sampler reads them.
struct ProcessResources {
    system: sysinfo::System,
    pid: sysinfo::Pid,
}

impl ProcessResources {
    fn new() -> Option<Self> {
        let pid = sysinfo::get_current_pid().ok()?;
        let mut resources = Self {
            system: sysinfo::System::new(),
            pid,
        };
        // The first refresh only establishes the baseline: `cpu_usage` is a
        // delta against the previous refresh and reads zero until then.
        resources.refresh();
        Some(resources)
    }

    fn read(&mut self) -> (Option<f32>, Option<u64>) {
        self.refresh();
        match self.system.process(self.pid) {
            // Left on sysinfo's scale, which is 100 per saturated logical core.
            Some(process) => (Some(process.cpu_usage()), Some(process.memory())),
            None => (None, None),
        }
    }

    fn refresh(&mut self) {
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[self.pid]),
            false,
            sysinfo::ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory(),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gpui::Styled as _;
    use tempfile::tempdir;

    use super::*;

    fn draws(millis: &[u64]) -> Vec<Duration> {
        millis
            .iter()
            .map(|value| Duration::from_millis(*value))
            .collect()
    }

    #[test]
    fn samples_land_in_the_directory_diagnostics_cleanup_owns() {
        let path = fps_monitor_log_path(Path::new("/home/vibex"));
        assert_eq!(path, Path::new("/home/vibex/diagnostics/fps-monitor.jsonl"));
        assert_eq!(
            path.parent().and_then(Path::file_name),
            Some(std::ffi::OsStr::new("diagnostics"))
        );
    }

    #[test]
    fn one_interval_summarizes_with_the_huds_definitions() {
        let summary = summarize_frames(&draws(&[8, 12, 20, 40]), 6, &[], Duration::from_millis(16));
        assert_eq!(summary.frames, 4);
        assert_eq!(summary.frame_millis, 20.0);
        // Nearest rank: the frame 95% of four frames came in at or under.
        assert_eq!(summary.p95_millis, 40.0);
        assert_eq!(summary.worst_millis, 40.0);
        // Two of the four frames overran the 16ms budget.
        assert_eq!(summary.dropped_percent, 50.0);
        assert_eq!(summary.invalidations, 1.5);
    }

    #[test]
    fn a_rate_needs_two_presents_and_reads_the_span_between_them() {
        let start = Instant::now();
        let presents = |offsets: &[u64]| -> Vec<Instant> {
            offsets
                .iter()
                .map(|offset| start + Duration::from_millis(*offset))
                .collect()
        };

        // Three frames over 500ms are two intervals: four frames a second.
        let summary = summarize_frames(&draws(&[4]), 1, &presents(&[0, 250, 500]), FRAME_BUDGET);
        assert_eq!(summary.presented, 3);
        assert_eq!(summary.fps, 4.0);

        let summary = summarize_frames(&draws(&[4]), 1, &presents(&[0]), FRAME_BUDGET);
        assert_eq!(summary.fps, 0.0);
        assert_eq!(summary.presented, 1);
    }

    #[test]
    fn an_interval_without_frames_records_nothing() {
        let directory = tempdir().unwrap();
        let path = fps_monitor_log_path(directory.path());
        let mut recorder = FpsMonitorRecorder::new(WindowId::from(0xF0F0), path);
        // No frames have been drawn for this window id, which is what an idle
        // workbench looks like.
        assert!(recorder.sample_line().is_none());
    }

    /// A view that draws an empty frame, which is enough for the frame trace.
    struct DrawProbe;

    impl gpui::Render for DrawProbe {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div().size_full()
        }
    }

    #[gpui::test]
    fn frames_a_live_window_draws_become_recorded_lines(cx: &mut gpui::TestAppContext) {
        let directory = tempdir().unwrap();
        let path = fps_monitor_log_path(directory.path());

        // The same process-wide switch the HUD turns on while it is shown.
        gpui::profiler::set_trace_enabled(true);
        let (_, cx) = cx.add_window_view(|_, _| DrawProbe);
        let window_id = cx.update(|window, _| window.window_handle().window_id());
        let mut recorder = FpsMonitorRecorder::new(window_id, path.clone());

        cx.update(|window, cx| {
            for _ in 0..3 {
                let _ = window.draw(cx);
            }
        });

        let line = recorder
            .sample_line()
            .expect("a window that drew should have an interval to record");
        let sample: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(sample["frames"].as_u64().unwrap_or_default() >= 1, "{line}");
        assert_eq!(sample["schemaVersion"], SAMPLE_SCHEMA_VERSION);
        assert!(sample["frameMillis"].is_number(), "{line}");

        append_sample_line(&path, &line).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);

        gpui::profiler::set_trace_enabled(false);
    }

    #[test]
    fn a_recorded_sample_serializes_its_fields_for_a_later_reader() {
        let sample = FpsMonitorSample {
            schema_version: SAMPLE_SCHEMA_VERSION,
            recorded_at_ms: 1_700_000_000_000,
            frames: 60,
            presented: 59,
            fps: 59.0,
            frame_millis: 8.4,
            p95_millis: 14.1,
            worst_millis: 22.0,
            dropped_percent: 1.7,
            invalidations: 1.0,
            cpu_percent: Some(142.0),
            resident_memory_bytes: Some(88_080_384),
        };
        let line = serde_json::to_string(&sample).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["schemaVersion"], SAMPLE_SCHEMA_VERSION);
        assert_eq!(parsed["recordedAtMs"], 1_700_000_000_000i64);
        assert_eq!(parsed["frameMillis"], 8.4);
        assert_eq!(parsed["droppedPercent"], 1.7);
        assert_eq!(parsed["cpuPercent"], 142.0);
        assert_eq!(parsed["residentMemoryBytes"], 88_080_384u64);
        assert!(line.ends_with('}') && !line.contains('\n'));
    }

    #[test]
    fn appending_creates_the_diagnostics_directory_and_rotates_at_the_limit() {
        let directory = tempdir().unwrap();
        let path = fps_monitor_log_path(directory.path());
        assert!(!path.parent().unwrap().exists());

        append_sample_line_with_limit(&path, r#"{"frames":1}"#, 1024).unwrap();
        append_sample_line_with_limit(&path, r#"{"frames":2}"#, 1024).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\"frames\":1}\n{\"frames\":2}\n"
        );
        assert!(!path.with_file_name(FPS_MONITOR_ROTATED_LOG_FILE).exists());

        // The next line lands past the limit, so the log rotates: the newest
        // line starts a fresh file and the older lines stay beside it.
        append_sample_line_with_limit(&path, r#"{"frames":3}"#, 26).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"frames\":3}\n");
        assert_eq!(
            fs::read_to_string(path.with_file_name(FPS_MONITOR_ROTATED_LOG_FILE)).unwrap(),
            "{\"frames\":1}\n{\"frames\":2}\n"
        );

        // A second rotation replaces the older file instead of growing.
        append_sample_line_with_limit(&path, r#"{"frames":4}"#, 0).unwrap();
        assert_eq!(
            fs::read_to_string(path.with_file_name(FPS_MONITOR_ROTATED_LOG_FILE)).unwrap(),
            "{\"frames\":3}\n"
        );
    }
}
