use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use vibex_core::{VibexError, VibexResult};

use crate::{
    ContentResourceMetrics, ContentSurfaceKind, ContentSurfaceLifecycle, ContentSurfaceOrigin,
    GenerationDisposition,
};

pub const PDFIUM_VERSION: &str = "7881";
pub const PDFIUM_RENDER_VERSION: &str = "0.9.3";
pub const DEFAULT_PDF_CACHE_BUDGET_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_PDF_CACHE_PAGE_LIMIT: usize = 12;
pub const MAX_PDF_SOURCE_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_PDF_PAGES: usize = 10_000;
pub const PDF_PAGE_OVERSCAN: usize = 1;

pub fn read_pdf_source(path: impl AsRef<Path>) -> VibexResult<Vec<u8>> {
    let path = path.as_ref();
    let metadata = std::fs::metadata(path).map_err(|_| {
        VibexError::storage(
            "pdf_source_metadata_failed",
            "PDF document metadata could not be read",
        )
        .with_recovery_hint("Choose the document again or use the system-open action")
    })?;
    if metadata.len() == 0 || metadata.len() > MAX_PDF_SOURCE_BYTES as u64 {
        return Err(VibexError::validation(
            "pdf_source_size_invalid",
            "PDF document is empty or exceeds the source size limit",
        ));
    }
    let file = File::open(path).map_err(|_| {
        VibexError::storage("pdf_source_read_failed", "PDF document could not be read")
            .with_recovery_hint("Choose the document again or use the system-open action")
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_PDF_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            VibexError::storage("pdf_source_read_failed", "PDF document could not be read")
                .with_recovery_hint("Choose the document again or use the system-open action")
        })?;
    if bytes.is_empty() || bytes.len() > MAX_PDF_SOURCE_BYTES {
        return Err(VibexError::validation(
            "pdf_source_size_invalid",
            "PDF document is empty or exceeds the source size limit",
        ));
    }
    Ok(bytes)
}

pub struct PdfiumEngine {
    pdfium: Pdfium,
}

impl PdfiumEngine {
    pub fn discover_library_path() -> VibexResult<PathBuf> {
        let executable = std::env::current_exe().map_err(|_| {
            VibexError::process(
                "pdfium_executable_path_unavailable",
                "Vibex could not resolve its executable path for PDFium discovery",
            )
        })?;
        let mut candidates = packaged_pdfium_candidates(&executable);
        if let Ok(current_dir) = std::env::current_dir() {
            candidates.push(current_dir.join(development_pdfium_relative_path()));
        }
        candidates
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| {
                VibexError::process(
                    "pdfium_library_missing",
                    "PDFium native library is unavailable",
                )
                .with_recovery_hint("Reinstall Vibex or use the system-open action")
            })
    }

    pub fn bind(library_path: impl AsRef<Path>) -> VibexResult<Self> {
        let path = library_path.as_ref();
        if !path.is_file() {
            return Err(VibexError::process(
                "pdfium_library_missing",
                "PDFium native library is unavailable",
            )
            .with_recovery_hint("Reinstall Vibex or use the system-open action"));
        }
        let bindings = Pdfium::bind_to_library(path).map_err(|_| {
            VibexError::process(
                "pdfium_bind_failed",
                "PDFium native library could not be initialized",
            )
            .with_recovery_hint("Use the system-open action and inspect native-content diagnostics")
        })?;
        Ok(Self {
            pdfium: Pdfium::new(bindings),
        })
    }
}

fn packaged_pdfium_candidates(executable: &Path) -> Vec<PathBuf> {
    let Some(binary_dir) = executable.parent() else {
        return Vec::new();
    };
    #[cfg(target_os = "linux")]
    {
        vec![
            binary_dir.join("../lib/vibex-desktop/pdfium/lib/libpdfium.so"),
            binary_dir.join("pdfium/lib/libpdfium.so"),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            binary_dir.join("../Resources/pdfium/lib/libpdfium.dylib"),
            binary_dir.join("pdfium/lib/libpdfium.dylib"),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        vec![
            binary_dir.join("pdfium/bin/pdfium.dll"),
            binary_dir.join("../Resources/pdfium/bin/pdfium.dll"),
        ]
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

fn development_pdfium_relative_path() -> &'static Path {
    #[cfg(target_os = "linux")]
    {
        Path::new("target/native/pdfium/linux-x86_64/lib/libpdfium.so")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Path::new("target/native/pdfium/macos-x86_64/lib/libpdfium.dylib")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Path::new("target/native/pdfium/macos-aarch64/lib/libpdfium.dylib")
    }
    #[cfg(target_os = "windows")]
    {
        Path::new("target/native/pdfium/windows-x86_64/bin/pdfium.dll")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Path::new("pdfium-unavailable")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfPageMetadata {
    pub page_index: usize,
    pub width_points: f32,
    pub height_points: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfDocumentMetadata {
    pub page_count: usize,
    pub pages: Vec<PdfPageMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PdfPageCacheKey {
    page_index: usize,
    target_width: u16,
}

#[derive(Debug, Clone)]
pub struct PdfPageBitmap {
    pub page_index: usize,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

#[derive(Debug, Clone)]
struct CachedPdfPage {
    bitmap: PdfPageBitmap,
    last_used: u64,
}

#[derive(Debug)]
struct PdfPageCache {
    budget_bytes: usize,
    page_limit: usize,
    resident_bytes: usize,
    clock: u64,
    entries: BTreeMap<PdfPageCacheKey, CachedPdfPage>,
    evictions: u64,
}

impl PdfPageCache {
    fn new(page_limit: usize, budget_bytes: usize) -> VibexResult<Self> {
        if page_limit == 0 || budget_bytes == 0 {
            return Err(VibexError::validation(
                "pdf_cache_budget_invalid",
                "PDF cache budgets must be non-zero",
            ));
        }
        Ok(Self {
            budget_bytes,
            page_limit,
            resident_bytes: 0,
            clock: 0,
            entries: BTreeMap::new(),
            evictions: 0,
        })
    }

    fn get(&mut self, key: PdfPageCacheKey) -> Option<PdfPageBitmap> {
        self.clock = self.clock.saturating_add(1);
        let entry = self.entries.get_mut(&key)?;
        entry.last_used = self.clock;
        Some(entry.bitmap.clone())
    }

    fn insert(&mut self, key: PdfPageCacheKey, bitmap: PdfPageBitmap) -> VibexResult<()> {
        let bytes = bitmap.rgba.len();
        if bytes > self.budget_bytes {
            return Err(VibexError::capability(
                "pdf_page_exceeds_cache_budget",
                "Rendered PDF page exceeds the decoded page cache budget",
            ));
        }
        self.clock = self.clock.saturating_add(1);
        if let Some(previous) = self.entries.remove(&key) {
            self.resident_bytes = self
                .resident_bytes
                .saturating_sub(previous.bitmap.rgba.len());
        }
        self.resident_bytes = self.resident_bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            CachedPdfPage {
                bitmap,
                last_used: self.clock,
            },
        );
        while self.entries.len() > self.page_limit || self.resident_bytes > self.budget_bytes {
            let lru = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
                .ok_or_else(|| {
                    VibexError::process(
                        "pdf_cache_eviction_failed",
                        "PDF cache could not select an eviction candidate",
                    )
                })?;
            let removed = self
                .entries
                .remove(&lru)
                .expect("selected PDF cache entry must exist");
            self.resident_bytes = self
                .resident_bytes
                .saturating_sub(removed.bitmap.rgba.len());
            self.evictions = self.evictions.saturating_add(1);
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.resident_bytes = 0;
    }

    fn metrics(&self) -> ContentResourceMetrics {
        ContentResourceMetrics {
            resident_items: self.entries.len(),
            resident_bytes: self.resident_bytes,
            budget_items: self.page_limit,
            budget_bytes: self.budget_bytes,
            evictions: self.evictions,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PdfCancellationToken(Arc<AtomicBool>);

impl PdfCancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfViewportRequest {
    pub first_visible_page: usize,
    pub last_visible_page: usize,
    pub target_width: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfControllerDiagnostics {
    pub engine: &'static str,
    pub engine_version: &'static str,
    pub wrapper: &'static str,
    pub wrapper_version: &'static str,
    pub document_loaded: bool,
    pub page_count: usize,
    pub rendered_page_requests: u64,
    pub cancelled_requests: u64,
    pub load_failures: u64,
    pub resources: ContentResourceMetrics,
}

pub struct PdfDocumentController {
    lifecycle: ContentSurfaceLifecycle,
    bytes: Option<Arc<[u8]>>,
    metadata: Option<PdfDocumentMetadata>,
    cache: PdfPageCache,
    rendered_page_requests: u64,
    cancelled_requests: u64,
    load_failures: u64,
}

impl PdfDocumentController {
    pub fn new() -> Self {
        Self::with_cache_budget(DEFAULT_PDF_CACHE_PAGE_LIMIT, DEFAULT_PDF_CACHE_BUDGET_BYTES)
            .expect("default PDF cache budgets are valid")
    }

    pub fn with_cache_budget(page_limit: usize, budget_bytes: usize) -> VibexResult<Self> {
        Ok(Self {
            lifecycle: ContentSurfaceLifecycle::restored(
                ContentSurfaceKind::Pdf,
                ContentSurfaceOrigin::Preview,
            ),
            bytes: None,
            metadata: None,
            cache: PdfPageCache::new(page_limit, budget_bytes)?,
            rendered_page_requests: 0,
            cancelled_requests: 0,
            load_failures: 0,
        })
    }

    pub fn lifecycle(&self) -> &ContentSurfaceLifecycle {
        &self.lifecycle
    }

    pub fn activate(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        self.lifecycle.activate(generation)
    }

    pub fn metadata(&self) -> Option<&PdfDocumentMetadata> {
        self.metadata.as_ref()
    }

    pub fn open(
        &mut self,
        engine: &PdfiumEngine,
        bytes: Vec<u8>,
        password: Option<&str>,
        generation: u64,
    ) -> VibexResult<&PdfDocumentMetadata> {
        if generation != self.lifecycle.activation_generation() {
            return Err(VibexError::conflict(
                "pdf_activation_stale",
                "PDF activation changed before the document opened",
            ));
        }
        self.begin_open(generation)?;
        if bytes.is_empty() || bytes.len() > MAX_PDF_SOURCE_BYTES {
            return self.fail_open(
                generation,
                VibexError::validation(
                    "pdf_source_size_invalid",
                    "PDF document is empty or exceeds the source size limit",
                ),
            );
        }
        let bytes: Arc<[u8]> = bytes.into();
        let document = match engine.pdfium.load_pdf_from_byte_slice(&bytes, password) {
            Ok(document) => document,
            Err(error) => {
                return self.fail_open(generation, pdf_load_error(error));
            }
        };
        let page_count = document.pages().len() as usize;
        if page_count == 0 || page_count > MAX_PDF_PAGES {
            return self.fail_open(
                generation,
                VibexError::capability(
                    "pdf_page_count_unsupported",
                    "PDF document has no pages or exceeds the page limit",
                ),
            );
        }
        let mut pages = Vec::with_capacity(page_count);
        for page_index in 0..page_count {
            let page = match document.pages().get(page_index as i32) {
                Ok(page) => page,
                Err(_) => {
                    return self.fail_open(
                        generation,
                        VibexError::process(
                            "pdf_page_metadata_failed",
                            "PDF page metadata could not be read",
                        ),
                    );
                }
            };
            let width_points = page.width().value;
            let height_points = page.height().value;
            if !width_points.is_finite()
                || !height_points.is_finite()
                || width_points <= 0.0
                || height_points <= 0.0
            {
                return self.fail_open(
                    generation,
                    VibexError::validation(
                        "pdf_page_dimensions_invalid",
                        "PDF page dimensions are invalid",
                    ),
                );
            }
            pages.push(PdfPageMetadata {
                page_index,
                width_points,
                height_points,
            });
        }
        drop(document);
        self.lifecycle.finish_load(generation)?;
        self.bytes = Some(bytes);
        self.metadata = Some(PdfDocumentMetadata { page_count, pages });
        Ok(self
            .metadata
            .as_ref()
            .expect("PDF metadata was assigned before returning"))
    }

    pub fn render_viewport(
        &mut self,
        engine: &PdfiumEngine,
        password: Option<&str>,
        request: PdfViewportRequest,
        cancellation: &PdfCancellationToken,
    ) -> VibexResult<Vec<PdfPageBitmap>> {
        let metadata = self.metadata.as_ref().ok_or_else(|| {
            VibexError::conflict("pdf_not_loaded", "PDF document has not been loaded")
        })?;
        if request.first_visible_page > request.last_visible_page
            || request.last_visible_page >= metadata.page_count
            || !(64..=4_096).contains(&request.target_width)
        {
            return Err(VibexError::validation(
                "pdf_viewport_invalid",
                "PDF viewport or target width is invalid",
            ));
        }
        if cancellation.is_cancelled() {
            self.cancelled_requests = self.cancelled_requests.saturating_add(1);
            return Err(pdf_cancelled());
        }
        let start = request.first_visible_page.saturating_sub(PDF_PAGE_OVERSCAN);
        let end = request
            .last_visible_page
            .saturating_add(PDF_PAGE_OVERSCAN)
            .min(metadata.page_count - 1);
        let bytes = self
            .bytes
            .as_ref()
            .expect("loaded PDF metadata always has document bytes")
            .clone();
        let document = engine
            .pdfium
            .load_pdf_from_byte_slice(&bytes, password)
            .map_err(pdf_load_error)?;
        let mut pages = Vec::with_capacity(end - start + 1);
        for page_index in start..=end {
            if cancellation.is_cancelled() {
                self.cancelled_requests = self.cancelled_requests.saturating_add(1);
                return Err(pdf_cancelled());
            }
            let key = PdfPageCacheKey {
                page_index,
                target_width: request.target_width,
            };
            let bitmap = if let Some(bitmap) = self.cache.get(key) {
                bitmap
            } else {
                validate_render_budget(
                    metadata.pages[page_index],
                    request.target_width,
                    self.cache.budget_bytes,
                )?;
                self.rendered_page_requests = self.rendered_page_requests.saturating_add(1);
                let page = document.pages().get(page_index as i32).map_err(|_| {
                    VibexError::process("pdf_page_load_failed", "PDF page could not be loaded")
                })?;
                let bitmap = page
                    .render_with_config(
                        &PdfRenderConfig::new()
                            .set_target_width(i32::from(request.target_width))
                            .render_annotations(false),
                    )
                    .map_err(|_| {
                        VibexError::process(
                            "pdf_page_render_failed",
                            "PDF page could not be rendered",
                        )
                    })?;
                let width = bitmap.width() as u32;
                let height = bitmap.height() as u32;
                let rgba: Arc<[u8]> = bitmap.as_rgba_bytes().into();
                let expected = width as usize * height as usize * 4;
                if rgba.len() != expected {
                    return Err(VibexError::process(
                        "pdf_bitmap_size_invalid",
                        "PDF renderer returned an invalid bitmap size",
                    ));
                }
                let bitmap = PdfPageBitmap {
                    page_index,
                    width,
                    height,
                    rgba,
                };
                if cancellation.is_cancelled() {
                    self.cancelled_requests = self.cancelled_requests.saturating_add(1);
                    return Err(pdf_cancelled());
                }
                self.cache.insert(key, bitmap.clone())?;
                bitmap
            };
            pages.push(bitmap);
        }
        Ok(pages)
    }

    pub fn close(&mut self, generation: u64) -> VibexResult<()> {
        self.lifecycle.close(generation)?;
        self.clear_document();
        Ok(())
    }

    pub fn diagnostics(&self) -> PdfControllerDiagnostics {
        PdfControllerDiagnostics {
            engine: "pdfium",
            engine_version: PDFIUM_VERSION,
            wrapper: "pdfium-render",
            wrapper_version: PDFIUM_RENDER_VERSION,
            document_loaded: self.metadata.is_some(),
            page_count: self
                .metadata
                .as_ref()
                .map_or(0, |metadata| metadata.page_count),
            rendered_page_requests: self.rendered_page_requests,
            cancelled_requests: self.cancelled_requests,
            load_failures: self.load_failures,
            resources: self.cache.metrics(),
        }
    }

    fn begin_open(&mut self, generation: u64) -> VibexResult<()> {
        self.lifecycle.begin_load(generation)?;
        self.clear_document();
        Ok(())
    }

    fn fail_open<T>(&mut self, generation: u64, error: VibexError) -> VibexResult<T> {
        self.load_failures = self.load_failures.saturating_add(1);
        self.lifecycle.failed(generation, &error.code)?;
        Err(error)
    }

    fn clear_document(&mut self) {
        self.bytes = None;
        self.metadata = None;
        self.cache.clear();
    }
}

impl Default for PdfDocumentController {
    fn default() -> Self {
        Self::new()
    }
}

fn pdf_load_error(error: PdfiumError) -> VibexError {
    match error {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            VibexError::capability(
                "pdf_password_required",
                "PDF document is encrypted or the supplied password is incorrect",
            )
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError) => {
            VibexError::capability(
                "pdf_security_unsupported",
                "PDF document security settings are unsupported",
            )
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError) => {
            VibexError::validation(
                "pdf_document_corrupt",
                "PDF document is corrupt or malformed",
            )
        }
        _ => VibexError::process("pdf_load_failed", "PDF document could not be loaded")
            .with_recovery_hint("Use the system-open action or try another document"),
    }
}

fn pdf_cancelled() -> VibexError {
    VibexError::conflict("pdf_render_cancelled", "PDF page rendering was cancelled")
}

fn validate_render_budget(
    page: PdfPageMetadata,
    target_width: u16,
    budget_bytes: usize,
) -> VibexResult<()> {
    let target_height = (f64::from(target_width) * f64::from(page.height_points)
        / f64::from(page.width_points))
    .ceil();
    if !target_height.is_finite() || target_height <= 0.0 || target_height > usize::MAX as f64 {
        return Err(VibexError::validation(
            "pdf_page_dimensions_invalid",
            "PDF page dimensions are invalid",
        ));
    }
    let expected_bytes = usize::from(target_width)
        .checked_mul(target_height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            VibexError::capability(
                "pdf_page_exceeds_cache_budget",
                "Rendered PDF page exceeds the decoded page cache budget",
            )
        })?;
    if expected_bytes > budget_bytes {
        return Err(VibexError::capability(
            "pdf_page_exceeds_cache_budget",
            "Rendered PDF page exceeds the decoded page cache budget",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentSurfacePhase;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn bitmap(page_index: usize, bytes: usize) -> PdfPageBitmap {
        PdfPageBitmap {
            page_index,
            width: 1,
            height: (bytes / 4) as u32,
            rgba: vec![0; bytes].into(),
        }
    }

    #[test]
    fn decoded_page_cache_is_lru_and_byte_bounded() {
        let mut cache = PdfPageCache::new(2, 12).unwrap();
        let key0 = PdfPageCacheKey {
            page_index: 0,
            target_width: 100,
        };
        let key1 = PdfPageCacheKey {
            page_index: 1,
            target_width: 100,
        };
        let key2 = PdfPageCacheKey {
            page_index: 2,
            target_width: 100,
        };
        cache.insert(key0, bitmap(0, 4)).unwrap();
        cache.insert(key1, bitmap(1, 4)).unwrap();
        cache.get(key0).unwrap();
        cache.insert(key2, bitmap(2, 8)).unwrap();
        assert!(cache.get(key0).is_some());
        assert!(cache.get(key1).is_none());
        assert!(cache.metrics().is_within_budget());
        assert_eq!(cache.metrics().evictions, 1);
    }

    #[test]
    fn decoded_page_larger_than_budget_is_rejected() {
        let mut cache = PdfPageCache::new(2, 4).unwrap();
        let error = cache
            .insert(
                PdfPageCacheKey {
                    page_index: 0,
                    target_width: 100,
                },
                bitmap(0, 8),
            )
            .unwrap_err();
        assert_eq!(error.code, "pdf_page_exceeds_cache_budget");
    }

    #[test]
    fn cancellation_is_shared_and_diagnostics_are_content_free() {
        let token = PdfCancellationToken::default();
        let sibling = token.clone();
        sibling.cancel();
        assert!(token.is_cancelled());

        let controller = PdfDocumentController::new();
        let json = serde_json::to_string(&controller.diagnostics()).unwrap();
        assert!(!json.contains("path"));
        assert!(!json.contains("content"));
        assert!(json.contains(PDFIUM_VERSION));
    }

    #[test]
    fn source_and_viewport_limits_reject_unbounded_work_before_native_calls() {
        let controller = PdfDocumentController::new();
        assert!(controller.metadata().is_none());
        assert_eq!(MAX_PDF_SOURCE_BYTES, 256 * 1024 * 1024);
        assert_eq!(MAX_PDF_PAGES, 10_000);
        assert_eq!(PDF_PAGE_OVERSCAN, 1);
    }

    #[test]
    fn new_activation_can_begin_loading_and_releases_previous_document_state() {
        let mut controller = PdfDocumentController::with_cache_budget(2, 16).unwrap();
        controller.activate(7).unwrap();
        controller.bytes = Some(vec![1, 2, 3].into());
        controller.metadata = Some(PdfDocumentMetadata {
            page_count: 1,
            pages: vec![PdfPageMetadata {
                page_index: 0,
                width_points: 10.0,
                height_points: 20.0,
            }],
        });
        controller
            .cache
            .insert(
                PdfPageCacheKey {
                    page_index: 0,
                    target_width: 100,
                },
                bitmap(0, 8),
            )
            .unwrap();

        controller.begin_open(7).unwrap();

        assert_eq!(controller.lifecycle.phase(), ContentSurfacePhase::Loading);
        assert!(controller.bytes.is_none());
        assert!(controller.metadata.is_none());
        assert_eq!(controller.cache.metrics().resident_items, 0);
        assert_eq!(controller.cache.metrics().resident_bytes, 0);
    }

    #[test]
    fn every_open_failure_is_counted_and_moves_the_lifecycle_to_error() {
        let mut controller = PdfDocumentController::new();
        controller.activate(3).unwrap();
        controller.begin_open(3).unwrap();

        let error = controller
            .fail_open::<()>(
                3,
                VibexError::validation("pdf_document_corrupt", "PDF document is corrupt"),
            )
            .unwrap_err();

        assert_eq!(error.code, "pdf_document_corrupt");
        assert_eq!(controller.lifecycle.phase(), ContentSurfacePhase::Error);
        assert_eq!(controller.diagnostics().load_failures, 1);
        assert!(!controller.diagnostics().document_loaded);
    }

    #[test]
    fn native_render_is_rejected_before_allocation_when_estimated_rgba_exceeds_budget() {
        let page = PdfPageMetadata {
            page_index: 0,
            width_points: 100.0,
            height_points: 1_000.0,
        };
        let error = validate_render_budget(page, 1_000, 1_000_000).unwrap_err();
        assert_eq!(error.code, "pdf_page_exceeds_cache_budget");
        validate_render_budget(page, 100, 4_000_000).unwrap();
    }

    #[test]
    fn source_reader_rejects_sparse_oversized_files_before_allocating_their_contents() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "vibex-pdf-source-limit-{}-{nonce}.pdf",
            std::process::id()
        ));
        let file = File::create(&path).unwrap();
        file.set_len(MAX_PDF_SOURCE_BYTES as u64 + 1).unwrap();
        drop(file);

        let error = read_pdf_source(&path).unwrap_err();
        std::fs::remove_file(path).unwrap();

        assert_eq!(error.code, "pdf_source_size_invalid");
    }
}
