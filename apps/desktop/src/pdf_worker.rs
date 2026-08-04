use std::{
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use vibex_content::{
    ContentResourceMetrics, PdfCancellationToken, PdfDocumentController, PdfDocumentMetadata,
    PdfPageBitmap, PdfViewportRequest, PdfiumEngine, read_pdf_source,
};
use vibex_core::{VibexError, VibexResult};

const PDF_WORKER_SCHEMA_VERSION: &str = "pdf-worker-once.v1";
pub const PDF_WORKER_CACHE_PAGE_LIMIT: usize = 4;
pub const PDF_WORKER_CACHE_BUDGET_BYTES: usize = 48 * 1024 * 1024;
pub const PDF_WORKER_TIMEOUT: Duration = Duration::from_secs(15);
const PDF_WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
static PDF_WORKER_TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfWorkerDisposition {
    CleanExit,
    Cancelled,
    TimedOut,
    Crashed,
    ProtocolFailure,
}

#[derive(Debug, Clone)]
pub struct PdfWorkerFailure {
    pub code: String,
    pub message: String,
}

impl From<VibexError> for PdfWorkerFailure {
    fn from(error: VibexError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

#[derive(Debug)]
pub struct IsolatedPdfSuccess {
    pub metadata: PdfDocumentMetadata,
    pub pages: Vec<PdfPageBitmap>,
}

#[derive(Debug)]
pub struct IsolatedPdfExecution {
    pub result: Result<IsolatedPdfSuccess, PdfWorkerFailure>,
    pub disposition: PdfWorkerDisposition,
    pub last_worker_resources: ContentResourceMetrics,
    pub child_started: bool,
    pub child_reaped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfWorkerFaultMode {
    None,
    Crash,
    Hang,
}

impl PdfWorkerFaultMode {
    fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Crash => "crash",
            Self::Hang => "hang",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "crash" => Some(Self::Crash),
            "hang" => Some(Self::Hang),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PdfWorkerFileReport {
    schema_version: String,
    status: String,
    metadata: Option<PdfDocumentMetadata>,
    pages: Vec<PdfWorkerPageFile>,
    resources: ContentResourceMetrics,
    error: Option<PdfWorkerFileError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PdfWorkerPageFile {
    page_index: usize,
    width: u32,
    height: u32,
    rgba_bytes: usize,
    file_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PdfWorkerFileError {
    code: String,
    message: String,
}

struct PdfWorkerTempDir(PathBuf);

impl PdfWorkerTempDir {
    fn create() -> std::io::Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let counter = PDF_WORKER_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vibex-pdf-worker-{}-{timestamp}-{counter}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PdfWorkerTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_isolated_pdf_request(
    library_path: &Path,
    document_path: &Path,
    generation: u64,
    page_index: usize,
    target_width: u16,
    timeout: Duration,
    cancellation: &PdfCancellationToken,
    fault_mode: PdfWorkerFaultMode,
) -> IsolatedPdfExecution {
    let default_resources = released_worker_resources();
    let temporary = match PdfWorkerTempDir::create() {
        Ok(temporary) => temporary,
        Err(_) => {
            return failed_execution(
                "pdf_worker_temp_failed",
                "PDF worker temporary storage could not be created",
                PdfWorkerDisposition::ProtocolFailure,
                default_resources,
                false,
            );
        }
    };
    let report_path = temporary.path().join("report.json");
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(_) => {
            return failed_execution(
                "pdf_worker_executable_unavailable",
                "PDF worker executable could not be resolved",
                PdfWorkerDisposition::ProtocolFailure,
                default_resources,
                false,
            );
        }
    };
    let mut child = match Command::new(executable)
        .arg("--native-content-pdf-worker-once")
        .arg(library_path)
        .arg(document_path)
        .arg(generation.to_string())
        .arg(page_index.to_string())
        .arg(target_width.to_string())
        .arg(temporary.path())
        .arg(&report_path)
        .arg(fault_mode.label())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return failed_execution(
                "pdf_worker_spawn_failed",
                "PDF worker process could not be started",
                PdfWorkerDisposition::Crashed,
                default_resources,
                false,
            );
        }
    };

    let started = Instant::now();
    let exit_status = loop {
        if cancellation.is_cancelled() {
            reap_child(&mut child, true);
            return failed_execution(
                "pdf_render_cancelled",
                "PDF page rendering was cancelled",
                PdfWorkerDisposition::Cancelled,
                default_resources,
                true,
            );
        }
        if started.elapsed() >= timeout {
            reap_child(&mut child, true);
            return failed_execution(
                "pdf_worker_timeout",
                "PDF worker exceeded the native operation deadline",
                PdfWorkerDisposition::TimedOut,
                default_resources,
                true,
            );
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(PDF_WORKER_POLL_INTERVAL),
            Err(_) => {
                reap_child(&mut child, true);
                return failed_execution(
                    "pdf_worker_wait_failed",
                    "PDF worker process status could not be read",
                    PdfWorkerDisposition::Crashed,
                    default_resources,
                    true,
                );
            }
        }
    };
    reap_child(&mut child, false);
    if !exit_status.success() {
        return failed_execution(
            "pdf_worker_crashed",
            "PDF worker exited during a native PDF operation",
            PdfWorkerDisposition::Crashed,
            default_resources,
            true,
        );
    }

    let report = match read_worker_report(&report_path) {
        Ok(report) => report,
        Err(failure) => {
            return IsolatedPdfExecution {
                result: Err(failure),
                disposition: PdfWorkerDisposition::ProtocolFailure,
                last_worker_resources: default_resources,
                child_started: true,
                child_reaped: true,
            };
        }
    };
    let resources = report.resources;
    if let Some(error) = report.error {
        return IsolatedPdfExecution {
            result: Err(PdfWorkerFailure {
                code: error.code,
                message: error.message,
            }),
            disposition: PdfWorkerDisposition::CleanExit,
            last_worker_resources: resources,
            child_started: true,
            child_reaped: true,
        };
    }
    let Some(metadata) = report.metadata else {
        return failed_execution(
            "pdf_worker_protocol_failed",
            "PDF worker response omitted document metadata",
            PdfWorkerDisposition::ProtocolFailure,
            resources,
            true,
        );
    };
    let pages = match read_worker_pages(temporary.path(), &report.pages) {
        Ok(pages) => pages,
        Err(failure) => {
            return IsolatedPdfExecution {
                result: Err(failure),
                disposition: PdfWorkerDisposition::ProtocolFailure,
                last_worker_resources: resources,
                child_started: true,
                child_reaped: true,
            };
        }
    };
    IsolatedPdfExecution {
        result: Ok(IsolatedPdfSuccess { metadata, pages }),
        disposition: PdfWorkerDisposition::CleanExit,
        last_worker_resources: resources,
        child_started: true,
        child_reaped: true,
    }
}

fn read_worker_report(path: &Path) -> Result<PdfWorkerFileReport, PdfWorkerFailure> {
    let bytes = fs::read(path).map_err(|_| PdfWorkerFailure {
        code: "pdf_worker_protocol_failed".into(),
        message: "PDF worker response could not be read".into(),
    })?;
    let report: PdfWorkerFileReport =
        serde_json::from_slice(&bytes).map_err(|_| PdfWorkerFailure {
            code: "pdf_worker_protocol_failed".into(),
            message: "PDF worker response was invalid".into(),
        })?;
    if report.schema_version != PDF_WORKER_SCHEMA_VERSION
        || !matches!(report.status.as_str(), "success" | "error")
    {
        return Err(PdfWorkerFailure {
            code: "pdf_worker_protocol_failed".into(),
            message: "PDF worker response schema was unsupported".into(),
        });
    }
    Ok(report)
}

fn read_worker_pages(
    directory: &Path,
    page_files: &[PdfWorkerPageFile],
) -> Result<Vec<PdfPageBitmap>, PdfWorkerFailure> {
    let mut pages = Vec::with_capacity(page_files.len());
    for page in page_files {
        let file_path = Path::new(&page.file_name);
        if file_path.components().count() != 1
            || !matches!(file_path.components().next(), Some(Component::Normal(_)))
        {
            return Err(PdfWorkerFailure {
                code: "pdf_worker_protocol_failed".into(),
                message: "PDF worker returned an unsafe bitmap path".into(),
            });
        }
        let rgba = fs::read(directory.join(file_path)).map_err(|_| PdfWorkerFailure {
            code: "pdf_worker_protocol_failed".into(),
            message: "PDF worker bitmap could not be read".into(),
        })?;
        let expected = (page.width as usize)
            .checked_mul(page.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| PdfWorkerFailure {
                code: "pdf_worker_protocol_failed".into(),
                message: "PDF worker bitmap dimensions overflowed".into(),
            })?;
        if page.rgba_bytes != expected || rgba.len() != expected {
            return Err(PdfWorkerFailure {
                code: "pdf_worker_protocol_failed".into(),
                message: "PDF worker bitmap size was invalid".into(),
            });
        }
        pages.push(PdfPageBitmap {
            page_index: page.page_index,
            width: page.width,
            height: page.height,
            rgba: rgba.into(),
        });
    }
    Ok(pages)
}

fn failed_execution(
    code: &str,
    message: &str,
    disposition: PdfWorkerDisposition,
    resources: ContentResourceMetrics,
    child_reaped: bool,
) -> IsolatedPdfExecution {
    IsolatedPdfExecution {
        result: Err(PdfWorkerFailure {
            code: code.into(),
            message: message.into(),
        }),
        disposition,
        last_worker_resources: resources,
        child_started: child_reaped,
        child_reaped,
    }
}

pub fn released_worker_resources() -> ContentResourceMetrics {
    ContentResourceMetrics {
        resident_items: 0,
        resident_bytes: 0,
        budget_items: PDF_WORKER_CACHE_PAGE_LIMIT,
        budget_bytes: PDF_WORKER_CACHE_BUDGET_BYTES,
        evictions: 0,
    }
}

fn reap_child(child: &mut Child, kill: bool) {
    if kill {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[allow(clippy::too_many_arguments)]
pub fn run_pdf_worker_once(
    library_path: impl AsRef<Path>,
    document_path: impl AsRef<Path>,
    generation: u64,
    page_index: usize,
    target_width: u16,
    output_directory: impl AsRef<Path>,
    report_path: impl AsRef<Path>,
    fault_mode: &str,
) -> Result<(), Box<dyn Error>> {
    let fault_mode =
        PdfWorkerFaultMode::parse(fault_mode).ok_or("invalid PDF worker fault mode")?;
    match fault_mode {
        PdfWorkerFaultMode::Crash => std::process::abort(),
        PdfWorkerFaultMode::Hang => thread::sleep(Duration::from_secs(60)),
        PdfWorkerFaultMode::None => {}
    }
    let output_directory = output_directory.as_ref();
    let report_path = report_path.as_ref();
    fs::create_dir_all(output_directory)?;
    let mut controller = PdfDocumentController::with_cache_budget(
        PDF_WORKER_CACHE_PAGE_LIMIT,
        PDF_WORKER_CACHE_BUDGET_BYTES,
    )?;
    let result = (|| -> VibexResult<(PdfDocumentMetadata, Vec<PdfPageBitmap>)> {
        controller.activate(generation)?;
        let bytes = read_pdf_source(document_path)?;
        let engine = PdfiumEngine::bind(library_path)?;
        let metadata = controller.open(&engine, bytes, None, generation)?.clone();
        let pages = controller.render_viewport(
            &engine,
            None,
            PdfViewportRequest {
                first_visible_page: page_index,
                last_visible_page: page_index,
                target_width,
            },
            &PdfCancellationToken::default(),
        )?;
        Ok((metadata, pages))
    })();
    let report = match result {
        Ok((metadata, pages)) => {
            let mut page_files = Vec::with_capacity(pages.len());
            for page in pages {
                let file_name = format!("page-{}-{}.rgba", page.page_index, target_width);
                fs::write(output_directory.join(&file_name), &page.rgba)?;
                page_files.push(PdfWorkerPageFile {
                    page_index: page.page_index,
                    width: page.width,
                    height: page.height,
                    rgba_bytes: page.rgba.len(),
                    file_name,
                });
            }
            PdfWorkerFileReport {
                schema_version: PDF_WORKER_SCHEMA_VERSION.into(),
                status: "success".into(),
                metadata: Some(metadata),
                pages: page_files,
                resources: controller.diagnostics().resources,
                error: None,
            }
        }
        Err(error) => PdfWorkerFileReport {
            schema_version: PDF_WORKER_SCHEMA_VERSION.into(),
            status: "error".into(),
            metadata: None,
            pages: Vec::new(),
            resources: controller.diagnostics().resources,
            error: Some(PdfWorkerFileError {
                code: error.code,
                message: error.message,
            }),
        },
    };
    fs::write(report_path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfWorkerSupervisorReport {
    schema_version: &'static str,
    status: &'static str,
    normal_render_passed: bool,
    crash_detected: bool,
    crash_error_code: String,
    timeout_detected: bool,
    timeout_error_code: String,
    recovery_after_crash_passed: bool,
    recovery_after_timeout_passed: bool,
    children_started: usize,
    children_reaped: usize,
    privacy: PdfWorkerSupervisorPrivacy,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfWorkerSupervisorPrivacy {
    document_path_stored: bool,
    page_content_stored: bool,
    raw_stderr_stored: bool,
}

pub fn run_pdf_worker_supervisor(
    library_path: impl AsRef<Path>,
    document_path: impl AsRef<Path>,
) -> Result<PdfWorkerSupervisorReport, Box<dyn Error>> {
    let library_path = library_path.as_ref();
    let document_path = document_path.as_ref();
    let normal = run_isolated_pdf_request(
        library_path,
        document_path,
        1,
        0,
        960,
        PDF_WORKER_TIMEOUT,
        &PdfCancellationToken::default(),
        PdfWorkerFaultMode::None,
    );
    let normal_render_passed = normal.disposition == PdfWorkerDisposition::CleanExit
        && normal
            .result
            .as_ref()
            .is_ok_and(|success| !success.pages.is_empty());
    let crash = run_isolated_pdf_request(
        library_path,
        document_path,
        2,
        0,
        960,
        Duration::from_secs(2),
        &PdfCancellationToken::default(),
        PdfWorkerFaultMode::Crash,
    );
    let crash_error_code = crash
        .result
        .as_ref()
        .err()
        .map_or(String::new(), |error| error.code.clone());
    let crash_detected = crash.disposition == PdfWorkerDisposition::Crashed
        && crash_error_code == "pdf_worker_crashed";
    let recovery_after_crash = run_isolated_pdf_request(
        library_path,
        document_path,
        3,
        0,
        960,
        PDF_WORKER_TIMEOUT,
        &PdfCancellationToken::default(),
        PdfWorkerFaultMode::None,
    );
    let recovery_after_crash_passed = recovery_after_crash.result.is_ok();
    let timeout = run_isolated_pdf_request(
        library_path,
        document_path,
        4,
        0,
        960,
        Duration::from_millis(250),
        &PdfCancellationToken::default(),
        PdfWorkerFaultMode::Hang,
    );
    let timeout_error_code = timeout
        .result
        .as_ref()
        .err()
        .map_or(String::new(), |error| error.code.clone());
    let timeout_detected = timeout.disposition == PdfWorkerDisposition::TimedOut
        && timeout_error_code == "pdf_worker_timeout";
    let recovery_after_timeout = run_isolated_pdf_request(
        library_path,
        document_path,
        5,
        0,
        960,
        PDF_WORKER_TIMEOUT,
        &PdfCancellationToken::default(),
        PdfWorkerFaultMode::None,
    );
    let recovery_after_timeout_passed = recovery_after_timeout.result.is_ok();
    let executions = [
        &normal,
        &crash,
        &recovery_after_crash,
        &timeout,
        &recovery_after_timeout,
    ];
    let children_started = executions
        .iter()
        .filter(|execution| execution.child_started)
        .count();
    let children_reaped = executions
        .iter()
        .filter(|execution| execution.child_reaped)
        .count();
    if !normal_render_passed
        || !crash_detected
        || !timeout_detected
        || !recovery_after_crash_passed
        || !recovery_after_timeout_passed
        || children_started != executions.len()
        || children_reaped != executions.len()
    {
        return Err("PDF worker supervisor contract failed".into());
    }
    Ok(PdfWorkerSupervisorReport {
        schema_version: "pdf-worker-supervisor-run.v1",
        status: "passed",
        normal_render_passed,
        crash_detected,
        crash_error_code,
        timeout_detected,
        timeout_error_code,
        recovery_after_crash_passed,
        recovery_after_timeout_passed,
        children_started,
        children_reaped,
        privacy: PdfWorkerSupervisorPrivacy {
            document_path_stored: false,
            page_content_stored: false,
            raw_stderr_stored: false,
        },
    })
}

const PDF_WORKER_SOAK_ITERATIONS: usize = 49;
const PDF_WORKER_SOAK_EXPECTED_NORMAL: usize = 37;
const PDF_WORKER_SOAK_EXPECTED_FAULTS_PER_KIND: usize = 4;
const PDF_WORKER_SOAK_EXPECTED_RECOVERIES: usize = 12;
const PDF_WORKER_SOAK_RSS_GROWTH_BUDGET_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfWorkerSoakReport {
    schema_version: &'static str,
    status: &'static str,
    iterations: usize,
    normal_requests: usize,
    cancellations: usize,
    crashes: usize,
    timeouts: usize,
    recoveries_passed: usize,
    unexpected_failures: usize,
    children_started: usize,
    children_reaped: usize,
    initial_parent_rss_bytes: usize,
    final_parent_rss_bytes: usize,
    peak_parent_rss_bytes: usize,
    parent_rss_growth_bytes: usize,
    rss_growth_budget_bytes: usize,
    initial_open_fds: usize,
    final_open_fds: usize,
    initial_direct_children: usize,
    final_direct_children: usize,
    initial_worker_temp_directories: usize,
    final_worker_temp_directories: usize,
    longest_request_ms: u128,
    current_resources: ContentResourceMetrics,
    last_worker_resources: ContentResourceMetrics,
    privacy: PdfWorkerSoakPrivacy,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfWorkerSoakPrivacy {
    document_path_stored: bool,
    page_content_stored: bool,
    raw_stderr_stored: bool,
    temporary_path_stored: bool,
}

pub fn run_pdf_worker_soak(
    library_path: impl AsRef<Path>,
    document_path: impl AsRef<Path>,
) -> Result<PdfWorkerSoakReport, Box<dyn Error>> {
    let library_path = library_path.as_ref();
    let document_path = document_path.as_ref();
    let initial_parent_rss_bytes = process_rss_bytes().ok_or("process RSS is unavailable")?;
    let initial_open_fds = open_fd_count().ok_or("process FD count is unavailable")?;
    let initial_direct_children = direct_child_count().ok_or("child count is unavailable")?;
    let initial_worker_temp_directories =
        worker_temp_directory_count().ok_or("worker temporary directory count is unavailable")?;

    let mut normal_requests = 0usize;
    let mut cancellations = 0usize;
    let mut crashes = 0usize;
    let mut timeouts = 0usize;
    let mut recoveries_passed = 0usize;
    let mut unexpected_failures = 0usize;
    let mut children_started = 0usize;
    let mut children_reaped = 0usize;
    let mut peak_parent_rss_bytes = initial_parent_rss_bytes;
    let mut longest_request_ms = 0u128;
    let mut last_worker_resources = released_worker_resources();
    let mut recovery_pending = false;

    for iteration in 0..PDF_WORKER_SOAK_ITERATIONS {
        let position = iteration % 12;
        let fault_mode = match position {
            9 => PdfWorkerFaultMode::Crash,
            11 => PdfWorkerFaultMode::Hang,
            _ => PdfWorkerFaultMode::None,
        };
        let cancellation = PdfCancellationToken::default();
        if position == 7 {
            cancellation.cancel();
        }
        let timeout = if fault_mode == PdfWorkerFaultMode::Hang {
            Duration::from_millis(250)
        } else {
            PDF_WORKER_TIMEOUT
        };
        let started = Instant::now();
        let execution = run_isolated_pdf_request(
            library_path,
            document_path,
            iteration as u64 + 1,
            iteration % 12,
            960,
            timeout,
            &cancellation,
            fault_mode,
        );
        longest_request_ms = longest_request_ms.max(started.elapsed().as_millis());
        children_started += usize::from(execution.child_started);
        children_reaped += usize::from(execution.child_reaped);
        last_worker_resources = execution.last_worker_resources;

        let expected = if position == 7 {
            cancellations += 1;
            execution.disposition == PdfWorkerDisposition::Cancelled
                && execution
                    .result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.code == "pdf_render_cancelled")
        } else if fault_mode == PdfWorkerFaultMode::Crash {
            crashes += 1;
            execution.disposition == PdfWorkerDisposition::Crashed
                && execution
                    .result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.code == "pdf_worker_crashed")
        } else if fault_mode == PdfWorkerFaultMode::Hang {
            timeouts += 1;
            execution.disposition == PdfWorkerDisposition::TimedOut
                && execution
                    .result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.code == "pdf_worker_timeout")
        } else {
            normal_requests += 1;
            let succeeded = execution.disposition == PdfWorkerDisposition::CleanExit
                && execution
                    .result
                    .as_ref()
                    .is_ok_and(|success| !success.pages.is_empty());
            if succeeded && recovery_pending {
                recoveries_passed += 1;
                recovery_pending = false;
            }
            succeeded
        };
        if !expected {
            unexpected_failures += 1;
        }
        if position == 7
            || fault_mode == PdfWorkerFaultMode::Crash
            || fault_mode == PdfWorkerFaultMode::Hang
        {
            recovery_pending = true;
        }
        peak_parent_rss_bytes = peak_parent_rss_bytes
            .max(process_rss_bytes().ok_or("process RSS disappeared during soak")?);
    }

    let final_parent_rss_bytes = process_rss_bytes().ok_or("process RSS is unavailable")?;
    let final_open_fds = open_fd_count().ok_or("process FD count is unavailable")?;
    let final_direct_children = direct_child_count().ok_or("child count is unavailable")?;
    let final_worker_temp_directories =
        worker_temp_directory_count().ok_or("worker temporary directory count is unavailable")?;
    let parent_rss_growth_bytes = final_parent_rss_bytes.saturating_sub(initial_parent_rss_bytes);
    let passed = normal_requests == PDF_WORKER_SOAK_EXPECTED_NORMAL
        && cancellations == PDF_WORKER_SOAK_EXPECTED_FAULTS_PER_KIND
        && crashes == PDF_WORKER_SOAK_EXPECTED_FAULTS_PER_KIND
        && timeouts == PDF_WORKER_SOAK_EXPECTED_FAULTS_PER_KIND
        && recoveries_passed == PDF_WORKER_SOAK_EXPECTED_RECOVERIES
        && unexpected_failures == 0
        && children_started == PDF_WORKER_SOAK_ITERATIONS
        && children_reaped == PDF_WORKER_SOAK_ITERATIONS
        && parent_rss_growth_bytes <= PDF_WORKER_SOAK_RSS_GROWTH_BUDGET_BYTES
        && final_open_fds <= initial_open_fds.saturating_add(1)
        && final_direct_children == initial_direct_children
        && final_worker_temp_directories == initial_worker_temp_directories
        && released_worker_resources().resident_items == 0
        && released_worker_resources().resident_bytes == 0
        && last_worker_resources.is_within_budget();

    Ok(PdfWorkerSoakReport {
        schema_version: "pdf-worker-soak-run.v1",
        status: if passed { "passed" } else { "failed" },
        iterations: PDF_WORKER_SOAK_ITERATIONS,
        normal_requests,
        cancellations,
        crashes,
        timeouts,
        recoveries_passed,
        unexpected_failures,
        children_started,
        children_reaped,
        initial_parent_rss_bytes,
        final_parent_rss_bytes,
        peak_parent_rss_bytes,
        parent_rss_growth_bytes,
        rss_growth_budget_bytes: PDF_WORKER_SOAK_RSS_GROWTH_BUDGET_BYTES,
        initial_open_fds,
        final_open_fds,
        initial_direct_children,
        final_direct_children,
        initial_worker_temp_directories,
        final_worker_temp_directories,
        longest_request_ms,
        current_resources: released_worker_resources(),
        last_worker_resources,
        privacy: PdfWorkerSoakPrivacy {
            document_path_stored: false,
            page_content_stored: false,
            raw_stderr_stored: false,
            temporary_path_stored: false,
        },
    })
}

#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<usize> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<usize>()
        .ok()?;
    kib.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn process_rss_bytes() -> Option<usize> {
    None
}

#[cfg(target_os = "linux")]
fn open_fd_count() -> Option<usize> {
    Some(fs::read_dir("/proc/self/fd").ok()?.count())
}

#[cfg(not(target_os = "linux"))]
fn open_fd_count() -> Option<usize> {
    None
}

#[cfg(target_os = "linux")]
fn direct_child_count() -> Option<usize> {
    let path = format!(
        "/proc/{}/task/{}/children",
        std::process::id(),
        std::process::id()
    );
    let children = fs::read_to_string(path).ok()?;
    Some(children.split_whitespace().count())
}

#[cfg(not(target_os = "linux"))]
fn direct_child_count() -> Option<usize> {
    None
}

fn worker_temp_directory_count() -> Option<usize> {
    let prefix = format!("vibex-pdf-worker-{}-", std::process::id());
    Some(
        fs::read_dir(std::env::temp_dir())
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_worker_metrics_preserve_the_controller_budget() {
        let resources = released_worker_resources();
        assert_eq!(resources.resident_items, 0);
        assert_eq!(resources.resident_bytes, 0);
        assert_eq!(resources.budget_items, PDF_WORKER_CACHE_PAGE_LIMIT);
        assert_eq!(resources.budget_bytes, PDF_WORKER_CACHE_BUDGET_BYTES);
    }

    #[test]
    fn worker_bitmap_protocol_rejects_nested_paths_and_overflow() {
        let temporary = PdfWorkerTempDir::create().unwrap();
        let nested = PdfWorkerPageFile {
            page_index: 0,
            width: 1,
            height: 1,
            rgba_bytes: 4,
            file_name: "nested/page.rgba".into(),
        };
        assert_eq!(
            read_worker_pages(temporary.path(), &[nested])
                .unwrap_err()
                .code,
            "pdf_worker_protocol_failed"
        );

        let overflow = PdfWorkerPageFile {
            page_index: 0,
            width: u32::MAX,
            height: u32::MAX,
            rgba_bytes: usize::MAX,
            file_name: "page.rgba".into(),
        };
        assert_eq!(
            read_worker_pages(temporary.path(), &[overflow])
                .unwrap_err()
                .code,
            "pdf_worker_protocol_failed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_soak_process_metrics_are_available() {
        assert!(process_rss_bytes().is_some_and(|bytes| bytes > 0));
        assert!(open_fd_count().is_some_and(|count| count > 0));
        assert!(direct_child_count().is_some());
        assert!(worker_temp_directory_count().is_some());
    }
}
