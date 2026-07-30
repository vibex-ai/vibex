use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, Render, RenderImage, ScrollHandle,
    StatefulInteractiveElement as _, Task, WeakEntity, Window, div, img, prelude::*, px,
    uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, IconName, Selectable as _, Sizable as _,
    StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use image::{Frame, RgbaImage};
use serde::Serialize;
use vibex_content::{
    ContentResourceMetrics, PdfCancellationToken, PdfDocumentMetadata, PdfPageBitmap,
};
use vibex_core::{VibexError, VibexResult};

use crate::pdf_worker::{
    IsolatedPdfExecution, PDF_WORKER_TIMEOUT, PdfWorkerDisposition, PdfWorkerFailure,
    PdfWorkerFaultMode, released_worker_resources, run_isolated_pdf_request,
};

const PDF_UI_IMAGE_PAGE_LIMIT: usize = 3;
const PDF_UI_IMAGE_BUDGET_BYTES: usize = 72 * 1024 * 1024;
const PDF_FIT_HORIZONTAL_CHROME: f32 = 228.0;
const PDF_MIN_TARGET_WIDTH: u16 = 64;
const PDF_MAX_TARGET_WIDTH: u16 = 2_048;
const PDF_ZOOM_STEP: u16 = 25;
const PDF_MIN_ZOOM_PERCENT: u16 = 50;
const PDF_MAX_ZOOM_PERCENT: u16 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PdfZoomMode {
    FitWidth,
    Percent(u16),
}

impl PdfZoomMode {
    fn label(self) -> String {
        match self {
            Self::FitWidth => "Fit width".into(),
            Self::Percent(percent) => format!("{percent}%"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PdfSurfacePhase {
    Empty,
    Loading,
    Ready,
    Rendering,
    Error { code: String, message: String },
    Closed,
}

impl PdfSurfacePhase {
    fn label(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Rendering => "rendering",
            Self::Error { .. } => "error",
            Self::Closed => "closed",
        }
    }
}

#[derive(Clone)]
struct PdfRenderedPage {
    page_index: usize,
    width: u32,
    height: u32,
    image: Arc<RenderImage>,
}

enum PdfWorkerValue {
    Loaded {
        metadata: PdfDocumentMetadata,
        pages: Vec<PdfRenderedPage>,
    },
    Rendered {
        pages: Vec<PdfRenderedPage>,
    },
}

struct PdfWorkerOutcome {
    result: Result<PdfWorkerValue, PdfSurfaceError>,
    disposition: PdfWorkerDisposition,
    last_worker_resources: ContentResourceMetrics,
    child_started: bool,
    child_reaped: bool,
}

#[derive(Debug)]
struct PdfSurfaceError {
    code: String,
    message: String,
}

impl From<VibexError> for PdfSurfaceError {
    fn from(error: VibexError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

impl From<PdfWorkerFailure> for PdfSurfaceError {
    fn from(error: PdfWorkerFailure) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

#[derive(Debug, Default)]
struct PdfWorkerProcessMetrics {
    children_started: usize,
    children_reaped: usize,
    clean_exits: usize,
    cancellations: usize,
    timeouts: usize,
    crashes: usize,
    protocol_failures: usize,
    last_disposition: Option<PdfWorkerDisposition>,
}

impl PdfWorkerProcessMetrics {
    fn record(&mut self, outcome: &PdfWorkerOutcome) {
        self.children_started += usize::from(outcome.child_started);
        self.children_reaped += usize::from(outcome.child_reaped);
        match outcome.disposition {
            PdfWorkerDisposition::CleanExit => self.clean_exits += 1,
            PdfWorkerDisposition::Cancelled => self.cancellations += 1,
            PdfWorkerDisposition::TimedOut => self.timeouts += 1,
            PdfWorkerDisposition::Crashed => self.crashes += 1,
            PdfWorkerDisposition::ProtocolFailure => self.protocol_failures += 1,
        }
        self.last_disposition = Some(outcome.disposition);
    }

    fn report(&self, current_processes: usize) -> PdfWorkerProcessReport {
        PdfWorkerProcessReport {
            current_processes,
            children_started: self.children_started,
            children_reaped: self.children_reaped,
            clean_exits: self.clean_exits,
            cancellations: self.cancellations,
            timeouts: self.timeouts,
            crashes: self.crashes,
            protocol_failures: self.protocol_failures,
            last_disposition: self.last_disposition,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfSurfaceRunReport {
    schema_version: &'static str,
    status: &'static str,
    page_count: usize,
    current_page: usize,
    target_width: u16,
    rendered_page_indexes: Vec<usize>,
    zoom_mode: String,
    controls: PdfSurfaceControlReport,
    resources: ContentResourceMetrics,
    last_worker_resources: ContentResourceMetrics,
    worker_processes: PdfWorkerProcessReport,
    ui_images: ContentResourceMetrics,
    error: Option<PdfSurfaceErrorReport>,
    privacy: PdfSurfacePrivacyReport,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfWorkerProcessReport {
    current_processes: usize,
    children_started: usize,
    children_reaped: usize,
    clean_exits: usize,
    cancellations: usize,
    timeouts: usize,
    crashes: usize,
    protocol_failures: usize,
    last_disposition: Option<PdfWorkerDisposition>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfSurfaceErrorReport {
    code: String,
    retry_available: bool,
    explicit_system_open_available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfSurfaceControlReport {
    page_list: bool,
    scrolling: bool,
    zoom: bool,
    fit_width: bool,
    loading: bool,
    typed_error: bool,
    retry: bool,
    explicit_system_open: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfSurfacePrivacyReport {
    document_path_stored: bool,
    page_content_stored: bool,
    password_stored: bool,
}

pub struct PdfSurface {
    library_path: Result<PathBuf, PdfSurfaceError>,
    document_path: Option<PathBuf>,
    output: Option<PathBuf>,
    report_written: bool,
    phase: PdfSurfacePhase,
    metadata: Option<PdfDocumentMetadata>,
    rendered_pages: Vec<PdfRenderedPage>,
    last_worker_resources: ContentResourceMetrics,
    worker_processes: PdfWorkerProcessMetrics,
    current_page: usize,
    zoom_mode: PdfZoomMode,
    current_target_width: u16,
    viewport_width: f32,
    document_generation: u64,
    request_generation: u64,
    pending_render: Option<(usize, u16)>,
    render_cancellation: PdfCancellationToken,
    worker_task: Option<Task<()>>,
    picker_task: Option<Task<()>>,
    resize_task: Option<Task<()>>,
    viewport_scroll: ScrollHandle,
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfPhysicalObservation {
    pub ready: bool,
    pub page_count: usize,
    pub current_page: usize,
    pub zoom_label: String,
    pub rendered_pages: usize,
    pub worker_active: bool,
}

impl PdfSurface {
    pub fn physical_observation(&self) -> PdfPhysicalObservation {
        let target_width_settled = self.metadata.as_ref().is_some_and(|metadata| {
            self.current_target_width
                == target_width_for(
                    self.zoom_mode,
                    self.viewport_width,
                    metadata.pages.get(self.current_page),
                )
        });
        PdfPhysicalObservation {
            ready: self.phase == PdfSurfacePhase::Ready
                && self.worker_task.is_none()
                && self.resize_task.is_none()
                && self.pending_render.is_none()
                && target_width_settled,
            page_count: self
                .metadata
                .as_ref()
                .map_or(0, |metadata| metadata.page_count),
            current_page: self.current_page,
            zoom_label: self.zoom_mode.label(),
            rendered_pages: self.rendered_pages.len(),
            worker_active: self.worker_task.is_some(),
        }
    }

    pub fn physical_next_page(&mut self, cx: &mut Context<Self>) {
        self.request_page(self.current_page.saturating_add(1), cx);
    }

    pub fn physical_zoom_in(&mut self, cx: &mut Context<Self>) {
        self.zoom_in(cx);
    }

    pub fn new(
        library_path: Result<PathBuf, VibexError>,
        initial_document: Option<PathBuf>,
        output: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let library_path = library_path.map_err(Into::into);
        let viewport_width = f32::from(window.viewport_size().width);
        let mut this = Self {
            library_path,
            document_path: None,
            output,
            report_written: false,
            phase: PdfSurfacePhase::Empty,
            metadata: None,
            rendered_pages: Vec::new(),
            last_worker_resources: released_worker_resources(),
            worker_processes: PdfWorkerProcessMetrics::default(),
            current_page: 0,
            zoom_mode: PdfZoomMode::FitWidth,
            current_target_width: fit_target_width(viewport_width),
            viewport_width,
            document_generation: 0,
            request_generation: 0,
            pending_render: None,
            render_cancellation: PdfCancellationToken::default(),
            worker_task: None,
            picker_task: None,
            resize_task: None,
            viewport_scroll: ScrollHandle::new(),
            note: None,
        };
        if let Some(path) = initial_document {
            cx.on_next_frame(window, move |this, _, cx| this.begin_load(path, cx));
        } else if let Err(error) = &this.library_path {
            this.phase = PdfSurfacePhase::Error {
                code: error.code.clone(),
                message: error.message.clone(),
            };
        }
        this
    }

    fn choose_document(&mut self, cx: &mut Context<Self>) {
        if self.picker_task.is_some() || self.worker_task.is_some() {
            return;
        }
        let picker = gpui_tokio::Tokio::spawn(cx, async move {
            rfd::AsyncFileDialog::new()
                .set_title("Open PDF")
                .add_filter("PDF documents", &["pdf"])
                .pick_file()
                .await
        });
        self.picker_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let selection = picker.await.ok().flatten();
            let _ = entity.update(cx, |this, cx| {
                this.picker_task = None;
                if let Some(selection) = selection {
                    this.begin_load(selection.path().to_path_buf(), cx);
                }
                cx.notify();
            });
        }));
    }

    fn begin_load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.worker_task.is_some() {
            return;
        }
        let library_path = match &self.library_path {
            Ok(path) => path.clone(),
            Err(error) => {
                self.phase = PdfSurfacePhase::Error {
                    code: error.code.clone(),
                    message: error.message.clone(),
                };
                cx.notify();
                return;
            }
        };
        if let Err(error) = validate_pdf_path(&path) {
            let error = PdfSurfaceError::from(error);
            self.phase = PdfSurfacePhase::Error {
                code: error.code,
                message: error.message,
            };
            cx.notify();
            return;
        }

        self.render_cancellation.cancel();
        self.render_cancellation = PdfCancellationToken::default();
        self.resize_task = None;
        self.pending_render = None;
        self.request_generation = self.request_generation.saturating_add(1).max(1);
        self.document_generation = self.document_generation.saturating_add(1).max(1);
        self.document_path = Some(path.clone());
        self.report_written = false;
        self.phase = PdfSurfacePhase::Loading;
        self.metadata = None;
        self.rendered_pages.clear();
        self.last_worker_resources = released_worker_resources();
        self.current_page = 0;
        self.zoom_mode = PdfZoomMode::FitWidth;
        let target_width = fit_target_width(self.viewport_width);
        self.current_target_width = target_width;
        self.note = None;

        let document_generation = self.document_generation;
        let request_generation = self.request_generation;
        let cancellation = self.render_cancellation.clone();
        let worker = cx.background_spawn(async move {
            isolated_worker_outcome(
                run_isolated_pdf_request(
                    &library_path,
                    &path,
                    document_generation,
                    0,
                    target_width,
                    PDF_WORKER_TIMEOUT,
                    &cancellation,
                    PdfWorkerFaultMode::None,
                ),
                true,
                0,
            )
        });
        self.attach_worker(worker, request_generation, cx);
        cx.notify();
    }

    fn request_page(&mut self, page_index: usize, cx: &mut Context<Self>) {
        let Some(metadata) = self.metadata.as_ref() else {
            return;
        };
        let page_index = page_index.min(metadata.page_count.saturating_sub(1));
        self.current_page = page_index;
        let target_width = target_width_for(
            self.zoom_mode,
            self.viewport_width,
            metadata.pages.get(page_index),
        );
        self.request_render(page_index, target_width, cx);
    }

    fn set_zoom(&mut self, zoom_mode: PdfZoomMode, cx: &mut Context<Self>) {
        if self.metadata.is_none() {
            return;
        }
        self.zoom_mode = zoom_mode;
        self.request_page(self.current_page, cx);
    }

    fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let current = match self.zoom_mode {
            PdfZoomMode::FitWidth => 100,
            PdfZoomMode::Percent(percent) => percent,
        };
        self.set_zoom(
            PdfZoomMode::Percent(
                current
                    .saturating_sub(PDF_ZOOM_STEP)
                    .max(PDF_MIN_ZOOM_PERCENT),
            ),
            cx,
        );
    }

    fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let current = match self.zoom_mode {
            PdfZoomMode::FitWidth => 100,
            PdfZoomMode::Percent(percent) => percent,
        };
        self.set_zoom(
            PdfZoomMode::Percent(
                current
                    .saturating_add(PDF_ZOOM_STEP)
                    .min(PDF_MAX_ZOOM_PERCENT),
            ),
            cx,
        );
    }

    fn request_render(&mut self, page_index: usize, target_width: u16, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.saturating_add(1).max(1);
        self.current_target_width = target_width;
        self.phase = PdfSurfacePhase::Rendering;
        self.render_cancellation.cancel();
        self.render_cancellation = PdfCancellationToken::default();
        if self.worker_task.is_some() {
            self.pending_render = Some((page_index, target_width));
            cx.notify();
            return;
        }
        self.pending_render = None;
        self.start_render(page_index, target_width, self.request_generation, cx);
    }

    fn start_render(
        &mut self,
        page_index: usize,
        target_width: u16,
        request_generation: u64,
        cx: &mut Context<Self>,
    ) {
        let library_path = match &self.library_path {
            Ok(path) => path.clone(),
            Err(error) => {
                self.phase = PdfSurfacePhase::Error {
                    code: error.code.clone(),
                    message: error.message.clone(),
                };
                return;
            }
        };
        let Some(document_path) = self.document_path.clone() else {
            self.phase = PdfSurfacePhase::Error {
                code: "pdf_path_missing".into(),
                message: "PDF document path is unavailable".into(),
            };
            return;
        };
        let document_generation = self.document_generation;
        let cancellation = self.render_cancellation.clone();
        let worker = cx.background_spawn(async move {
            isolated_worker_outcome(
                run_isolated_pdf_request(
                    &library_path,
                    &document_path,
                    document_generation,
                    page_index,
                    target_width,
                    PDF_WORKER_TIMEOUT,
                    &cancellation,
                    PdfWorkerFaultMode::None,
                ),
                false,
                page_index,
            )
        });
        self.attach_worker(worker, request_generation, cx);
    }

    fn attach_worker(
        &mut self,
        worker: Task<PdfWorkerOutcome>,
        request_generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.worker_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = worker.await;
            let _ = entity.update(cx, |this, cx| {
                this.worker_task = None;
                this.worker_processes.record(&outcome);
                this.last_worker_resources = outcome.last_worker_resources;
                if request_generation == this.request_generation {
                    this.apply_worker_result(outcome.result);
                }
                if let Some((page_index, target_width)) = this.pending_render.take() {
                    this.start_render(page_index, target_width, this.request_generation, cx);
                } else {
                    this.maybe_write_report();
                }
                cx.notify();
            });
        }));
    }

    fn apply_worker_result(&mut self, result: Result<PdfWorkerValue, PdfSurfaceError>) {
        match result {
            Ok(PdfWorkerValue::Loaded { metadata, pages }) => {
                self.metadata = Some(metadata);
                self.rendered_pages = pages;
                self.phase = PdfSurfacePhase::Ready;
                self.scroll_to_current_page();
            }
            Ok(PdfWorkerValue::Rendered { pages }) => {
                self.rendered_pages = pages;
                self.phase = PdfSurfacePhase::Ready;
                self.scroll_to_current_page();
            }
            Err(error) if error.code == "pdf_render_cancelled" && self.pending_render.is_some() => {
            }
            Err(error) => {
                self.phase = PdfSurfacePhase::Error {
                    code: error.code,
                    message: error.message,
                };
            }
        }
    }

    fn scroll_to_current_page(&self) {
        self.viewport_scroll.set_offset(Default::default());
    }

    fn schedule_fit_resize(&mut self, cx: &mut Context<Self>) {
        if self.zoom_mode != PdfZoomMode::FitWidth || self.metadata.is_none() {
            return;
        }
        let target_width = fit_target_width(self.viewport_width);
        if target_width == self.current_target_width {
            return;
        }
        let page_index = self.current_page;
        self.resize_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let _ = entity.update(cx, |this, cx| {
                this.resize_task = None;
                if this.zoom_mode == PdfZoomMode::FitWidth
                    && fit_target_width(this.viewport_width) == target_width
                {
                    this.request_render(page_index, target_width, cx);
                } else {
                    cx.notify();
                }
            });
        }));
    }

    fn update_viewport_width(&mut self, viewport_width: f32, cx: &mut Context<Self>) {
        if (self.viewport_width - viewport_width).abs() < 0.5 {
            return;
        }
        self.viewport_width = viewport_width;
        self.schedule_fit_resize(cx);
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.document_path.clone() {
            self.begin_load(path, cx);
        }
    }

    fn close_document(&mut self, cx: &mut Context<Self>) {
        self.render_cancellation.cancel();
        self.request_generation = self.request_generation.saturating_add(1).max(1);
        self.metadata = None;
        self.rendered_pages.clear();
        self.document_path = None;
        self.pending_render = None;
        self.phase = PdfSurfacePhase::Closed;
        self.note = Some("PDF document closed; active worker cancellation requested".into());
        cx.notify();
    }

    fn open_in_system(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document_path.as_ref() else {
            return;
        };
        match validate_pdf_path(path).and_then(|_| {
            spawn_system_open(path)
                .map(|mut child| {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                })
                .map_err(|_| {
                    VibexError::process(
                        "pdf_system_open_failed",
                        "PDF document could not be opened in the system application",
                    )
                })
        }) {
            Ok(()) => self.note = Some("Requested the system PDF application".into()),
            Err(error) => {
                self.phase = PdfSurfacePhase::Error {
                    code: error.code,
                    message: error.message,
                }
            }
        }
        cx.notify();
    }

    fn maybe_write_report(&mut self) {
        if self.report_written {
            return;
        }
        let Some(output) = self.output.as_ref() else {
            return;
        };
        let (status, page_count, error) = match &self.phase {
            PdfSurfacePhase::Ready => {
                let Some(metadata) = self.metadata.as_ref() else {
                    return;
                };
                ("ready", metadata.page_count, None)
            }
            PdfSurfacePhase::Error { code, .. } => (
                "error",
                0,
                Some(PdfSurfaceErrorReport {
                    code: code.clone(),
                    retry_available: self.document_path.is_some(),
                    explicit_system_open_available: self.document_path.is_some(),
                }),
            ),
            _ => return,
        };
        let report = PdfSurfaceRunReport {
            schema_version: "pdf-surface-run.v1",
            status,
            page_count,
            current_page: self.current_page,
            target_width: self.current_target_width,
            rendered_page_indexes: self
                .rendered_pages
                .iter()
                .map(|page| page.page_index)
                .collect(),
            zoom_mode: self.zoom_mode.label(),
            controls: PdfSurfaceControlReport {
                page_list: true,
                scrolling: true,
                zoom: true,
                fit_width: true,
                loading: true,
                typed_error: true,
                retry: true,
                explicit_system_open: true,
            },
            resources: released_worker_resources(),
            last_worker_resources: self.last_worker_resources,
            worker_processes: self.worker_processes.report(0),
            ui_images: ui_image_metrics(&self.rendered_pages),
            error,
            privacy: PdfSurfacePrivacyReport {
                document_path_stored: false,
                page_content_stored: false,
                password_stored: false,
            },
            limitations: vec![
                "This report proves isolated-worker UI state, not native pixels or physical input.",
                "Per-request worker isolation trades controller-cache reuse for crash containment.",
                "Search, annotations, forms, and editing remain out of scope.",
            ],
        };
        if write_report(output, &report).is_ok() {
            self.report_written = true;
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.worker_task.is_some() || self.picker_task.is_some();
        let has_document = self.document_path.is_some();
        let page_count = self
            .metadata
            .as_ref()
            .map_or(0, |metadata| metadata.page_count);
        let current_page = self.current_page;
        let zoom_mode = self.zoom_mode;
        h_flex()
            .h(px(44.0))
            .flex_none()
            .items_center()
            .justify_between()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("pdf-choose")
                            .small()
                            .icon(IconName::File)
                            .label("Open PDF")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_document(cx))),
                    )
                    .child(
                        Button::new("pdf-system-open")
                            .small()
                            .outline()
                            .icon(IconName::ExternalLink)
                            .label("System open")
                            .disabled(!has_document)
                            .on_click(cx.listener(|this, _, _, cx| this.open_in_system(cx))),
                    )
                    .child(
                        Button::new("pdf-close")
                            .small()
                            .ghost()
                            .label("Close")
                            .disabled(!has_document)
                            .on_click(cx.listener(|this, _, _, cx| this.close_document(cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("pdf-previous")
                            .small()
                            .ghost()
                            .icon(IconName::ChevronLeft)
                            .disabled(page_count == 0 || current_page == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_page(this.current_page.saturating_sub(1), cx)
                            })),
                    )
                    .child(div().min_w(px(74.0)).text_center().text_sm().child(
                        if page_count == 0 {
                            "— / —".into()
                        } else {
                            format!("{} / {page_count}", current_page + 1)
                        },
                    ))
                    .child(
                        Button::new("pdf-next")
                            .small()
                            .ghost()
                            .icon(IconName::ChevronRight)
                            .disabled(page_count == 0 || current_page + 1 >= page_count)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_page(this.current_page.saturating_add(1), cx)
                            })),
                    )
                    .child(
                        Button::new("pdf-zoom-out")
                            .small()
                            .ghost()
                            .icon(IconName::Minus)
                            .disabled(page_count == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.zoom_out(cx))),
                    )
                    .child(
                        Button::new("pdf-fit-width")
                            .small()
                            .outline()
                            .label(zoom_mode.label())
                            .selected(zoom_mode == PdfZoomMode::FitWidth)
                            .disabled(page_count == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_zoom(PdfZoomMode::FitWidth, cx)
                            })),
                    )
                    .child(
                        Button::new("pdf-zoom-in")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .disabled(page_count == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.zoom_in(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_page_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let page_count = self
            .metadata
            .as_ref()
            .map_or(0, |metadata| metadata.page_count);
        let selected = self.current_page;
        v_flex()
            .w(px(150.0))
            .h_full()
            .flex_none()
            .min_h_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .h(px(32.0))
                    .flex_none()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child("PAGES"),
            )
            .child(
                uniform_list(
                    "pdf-page-list",
                    page_count,
                    cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|page_index| {
                                Button::new(format!("pdf-page-{page_index}"))
                                    .ghost()
                                    .compact()
                                    .w_full()
                                    .justify_start()
                                    .selected(page_index == selected)
                                    .label(format!("Page {}", page_index + 1))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.request_page(page_index, cx)
                                    }))
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn render_document(&self, cx: &mut Context<Self>) -> AnyElement {
        let pages = self.rendered_pages.clone();
        let current_page = self.current_page;
        let rendering = self.phase == PdfSurfacePhase::Rendering;
        v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .bg(cx.theme().muted.opacity(0.35))
            .child(
                v_flex()
                    .id("pdf-page-scroll")
                    .size_full()
                    .min_h_0()
                    .items_center()
                    .gap_4()
                    .track_scroll(&self.viewport_scroll)
                    .overflow_y_scrollbar()
                    .p_4()
                    .children(
                        pages
                            .into_iter()
                            .filter(move |page| page.page_index == current_page)
                            .map(|page| {
                                v_flex()
                                    .id(format!("pdf-rendered-page-{}", page.page_index))
                                    .flex_none()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("Page {}", page.page_index + 1)),
                                    )
                                    .child(
                                        div().rounded_sm().bg(gpui::white()).shadow_md().child(
                                            img(page.image)
                                                .w(px(page.width as f32))
                                                .h(px(page.height as f32)),
                                        ),
                                    )
                            }),
                    ),
            )
            .when(rendering, |this| {
                this.child(
                    div()
                        .absolute()
                        .top_3()
                        .right_3()
                        .rounded_md()
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .px_3()
                        .py_2()
                        .text_sm()
                        .child("Rendering visible pages…"),
                )
            })
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.phase {
            PdfSurfacePhase::Ready | PdfSurfacePhase::Rendering => h_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_page_list(cx))
                .child(self.render_document(cx))
                .into_any_element(),
            PdfSurfacePhase::Loading => centered_state(
                "Loading PDF…",
                "Reading metadata and rendering the visible page off the GPUI foreground thread.",
                cx,
            ),
            PdfSurfacePhase::Error { code, message } => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .p_6()
                .child(
                    div()
                        .text_lg()
                        .font_semibold()
                        .child("PDF preview unavailable"),
                )
                .child(
                    div()
                        .max_w(px(560.0))
                        .text_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{code}: {message}")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("pdf-retry")
                                .small()
                                .primary()
                                .label("Retry")
                                .disabled(
                                    self.document_path.is_none() || self.worker_task.is_some(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                        )
                        .child(
                            Button::new("pdf-error-system-open")
                                .small()
                                .outline()
                                .label("System open")
                                .disabled(self.document_path.is_none())
                                .on_click(cx.listener(|this, _, _, cx| this.open_in_system(cx))),
                        ),
                )
                .into_any_element(),
            PdfSurfacePhase::Closed => centered_state(
                "PDF closed",
                "Decoded pages and document metadata were released.",
                cx,
            ),
            PdfSurfacePhase::Empty => centered_state(
                "Open a PDF document",
                "Vibex renders visible pages through the bounded native PDFium controller.",
                cx,
            ),
        }
    }
}

impl Drop for PdfSurface {
    fn drop(&mut self) {
        self.render_cancellation.cancel();
    }
}

impl Render for PdfSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.weak_entity();
        let file_label = self
            .document_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("No document")
            .to_string();
        v_flex()
            .id("pdf-surface")
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .h(px(42.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().font_semibold().child("PDF Preview"))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(file_label),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.phase.label()),
                    ),
            )
            .child(self.render_toolbar(cx))
            .child(self.render_body(cx))
            .child(
                h_flex()
                    .h(px(32.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.note.clone().unwrap_or_else(|| {
                        "Only visible pages plus bounded overscan are decoded".into()
                    }))
                    .child(if self.worker_task.is_some() {
                        "isolated worker active".into()
                    } else {
                        format!(
                            "worker reaped · last peak {} pages / {:.1} MiB",
                            self.last_worker_resources.resident_items,
                            self.last_worker_resources.resident_bytes as f64 / (1024.0 * 1024.0)
                        )
                    }),
            )
            .on_prepaint(move |bounds, _, cx| {
                let viewport_width = f32::from(bounds.size.width);
                let _ = entity.update(cx, |this, cx| {
                    this.update_viewport_width(viewport_width, cx)
                });
            })
    }
}

fn centered_state(
    title: &'static str,
    description: &'static str,
    cx: &mut Context<PdfSurface>,
) -> AnyElement {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .child(div().text_lg().font_semibold().child(title))
        .child(
            div()
                .max_w(px(560.0))
                .text_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
        .into_any_element()
}

fn fit_target_width(viewport_width: f32) -> u16 {
    target_width((viewport_width - PDF_FIT_HORIZONTAL_CHROME).max(f32::from(PDF_MIN_TARGET_WIDTH)))
}

fn target_width_for(
    zoom_mode: PdfZoomMode,
    viewport_width: f32,
    page: Option<&vibex_content::PdfPageMetadata>,
) -> u16 {
    match zoom_mode {
        PdfZoomMode::FitWidth => fit_target_width(viewport_width),
        PdfZoomMode::Percent(percent) => {
            let width_points = page.map_or(612.0, |page| page.width_points);
            target_width(width_points * (96.0 / 72.0) * f32::from(percent) / 100.0)
        }
    }
}

fn target_width(width: f32) -> u16 {
    width.round().clamp(
        f32::from(PDF_MIN_TARGET_WIDTH),
        f32::from(PDF_MAX_TARGET_WIDTH),
    ) as u16
}

fn isolated_worker_outcome(
    execution: IsolatedPdfExecution,
    loading: bool,
    current_page: usize,
) -> PdfWorkerOutcome {
    let result = execution
        .result
        .map_err(PdfSurfaceError::from)
        .and_then(|success| {
            let pages = render_images(success.pages, current_page)?;
            if loading {
                Ok(PdfWorkerValue::Loaded {
                    metadata: success.metadata,
                    pages,
                })
            } else {
                Ok(PdfWorkerValue::Rendered { pages })
            }
        });
    PdfWorkerOutcome {
        result,
        disposition: execution.disposition,
        last_worker_resources: execution.last_worker_resources,
        child_started: execution.child_started,
        child_reaped: execution.child_reaped,
    }
}

fn render_images(
    mut bitmaps: Vec<PdfPageBitmap>,
    current_page: usize,
) -> Result<Vec<PdfRenderedPage>, PdfSurfaceError> {
    bitmaps.sort_by_key(|bitmap| bitmap.page_index.abs_diff(current_page));
    let mut resident_bytes = 0usize;
    let mut pages = Vec::with_capacity(bitmaps.len().min(PDF_UI_IMAGE_PAGE_LIMIT));
    for bitmap in bitmaps {
        if pages.len() >= PDF_UI_IMAGE_PAGE_LIMIT {
            break;
        }
        let next_bytes = resident_bytes.saturating_add(bitmap.rgba.len());
        if next_bytes > PDF_UI_IMAGE_BUDGET_BYTES {
            continue;
        }
        resident_bytes = next_bytes;
        pages.push(render_image(bitmap)?);
    }
    if !pages.iter().any(|page| page.page_index == current_page) {
        return Err(PdfSurfaceError::from(VibexError::capability(
            "pdf_ui_image_budget_exceeded",
            "Visible PDF page exceeds the GPUI image budget",
        )));
    }
    pages.sort_by_key(|page| page.page_index);
    Ok(pages)
}

fn render_image(bitmap: PdfPageBitmap) -> Result<PdfRenderedPage, PdfSurfaceError> {
    let mut bgra = bitmap.rgba.to_vec();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(bitmap.width, bitmap.height, bgra).ok_or_else(|| {
        PdfSurfaceError::from(VibexError::process(
            "pdf_ui_bitmap_size_invalid",
            "PDF page bitmap could not be prepared for GPUI",
        ))
    })?;
    Ok(PdfRenderedPage {
        page_index: bitmap.page_index,
        width: bitmap.width,
        height: bitmap.height,
        image: Arc::new(RenderImage::new(vec![Frame::new(buffer)])),
    })
}

fn ui_image_metrics(pages: &[PdfRenderedPage]) -> ContentResourceMetrics {
    ContentResourceMetrics {
        resident_items: pages.len(),
        resident_bytes: pages.iter().fold(0usize, |bytes, page| {
            bytes.saturating_add(
                (page.width as usize)
                    .saturating_mul(page.height as usize)
                    .saturating_mul(4),
            )
        }),
        budget_items: PDF_UI_IMAGE_PAGE_LIMIT,
        budget_bytes: PDF_UI_IMAGE_BUDGET_BYTES,
        evictions: 0,
    }
}

fn validate_pdf_path(path: &Path) -> VibexResult<()> {
    if !path.is_file() {
        return Err(VibexError::validation(
            "pdf_path_missing",
            "PDF document path does not reference a file",
        ));
    }
    if !is_pdf_path(path) {
        return Err(VibexError::validation(
            "pdf_path_extension_invalid",
            "PDF preview accepts only .pdf documents",
        ));
    }
    Ok(())
}

fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

#[cfg(target_os = "linux")]
fn spawn_system_open(path: &Path) -> std::io::Result<Child> {
    Command::new("xdg-open")
        .arg(path)
        .spawn()
        .or_else(|_| Command::new("gio").arg("open").arg(path).spawn())
}

#[cfg(target_os = "macos")]
fn spawn_system_open(path: &Path) -> std::io::Result<Child> {
    Command::new("open").arg(path).spawn()
}

#[cfg(target_os = "windows")]
fn spawn_system_open(path: &Path) -> std::io::Result<Child> {
    Command::new("rundll32.exe")
        .arg("url.dll,FileProtocolHandler")
        .arg(path)
        .spawn()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn spawn_system_open(_: &Path) -> std::io::Result<Child> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "system PDF open is unsupported on this platform",
    ))
}

fn write_report(path: &Path, report: &PdfSurfaceRunReport) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_and_percentage_widths_are_bounded_and_deterministic() {
        assert_eq!(fit_target_width(1_200.0), 972);
        assert_eq!(fit_target_width(100.0), PDF_MIN_TARGET_WIDTH);
        assert_eq!(fit_target_width(10_000.0), PDF_MAX_TARGET_WIDTH);
        assert_eq!(
            target_width_for(
                PdfZoomMode::Percent(100),
                1_200.0,
                Some(&vibex_content::PdfPageMetadata {
                    page_index: 0,
                    width_points: 612.0,
                    height_points: 792.0,
                }),
            ),
            816
        );
    }

    #[test]
    fn pdf_path_policy_is_extension_exact_and_case_insensitive() {
        assert!(is_pdf_path(Path::new("document.pdf")));
        assert!(is_pdf_path(Path::new("document.PDF")));
        assert!(!is_pdf_path(Path::new("document.pdf.exe")));
        assert!(!is_pdf_path(Path::new("document")));
    }

    #[test]
    fn rgba_bitmap_is_converted_to_gpui_bgra_without_content_in_state() {
        let page = render_image(PdfPageBitmap {
            page_index: 2,
            width: 1,
            height: 1,
            rgba: vec![10, 20, 30, 255].into(),
        })
        .unwrap();
        assert_eq!(page.page_index, 2);
        assert_eq!(page.image.as_bytes(0), Some([30, 20, 10, 255].as_slice()));
    }

    #[test]
    fn ui_image_budget_prioritizes_the_current_page_and_stays_bounded() {
        let bitmap = |page_index| PdfPageBitmap {
            page_index,
            width: 1,
            height: 1,
            rgba: vec![page_index as u8, 0, 0, 255].into(),
        };
        let pages = render_images(vec![bitmap(0), bitmap(1), bitmap(2), bitmap(3)], 2).unwrap();
        assert_eq!(pages.len(), PDF_UI_IMAGE_PAGE_LIMIT);
        assert!(pages.iter().any(|page| page.page_index == 2));
        assert!(ui_image_metrics(&pages).is_within_budget());
    }
}
