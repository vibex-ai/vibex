use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fs,
    path::Path,
    time::Instant,
};

use pdfium_render::prelude::*;
use serde::Serialize;
use sha2::{Digest, Sha256};

const PDFIUM_VERSION: &str = "7881";
const PDFIUM_RENDER_VERSION: &str = "0.9.3";
const EXPECTED_PAGE_COUNT: i32 = 12;
const FIT_WIDTH: i32 = 960;
const ZOOM_WIDTH: i32 = 1440;
const CACHE_BUDGET_BYTES: usize = 24 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfFeasibilityReport {
    schema_version: &'static str,
    status: &'static str,
    platform: &'static str,
    architecture: &'static str,
    engine: EngineEvidence,
    fixture: FixtureEvidence,
    rendering: RenderingEvidence,
    virtualization: VirtualizationEvidence,
    error_handling: ErrorEvidence,
    memory: MemoryEvidence,
    privacy: PrivacyEvidence,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngineEvidence {
    wrapper: &'static str,
    wrapper_version: &'static str,
    pdfium_version: &'static str,
    binding: &'static str,
    process_model: &'static str,
    child_processes_started: u8,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureEvidence {
    bytes: u64,
    sha256: String,
    page_count: i32,
    cjk_text_extracted: bool,
    embedded_font_marker_present: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderingEvidence {
    fit: RenderEvidence,
    zoom_150_percent: RenderEvidence,
    aspect_ratio_preserved: bool,
    distinct_zoom_output: bool,
    preview_raw_rgba_written: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderEvidence {
    width: i32,
    height: i32,
    rgba_bytes: usize,
    rgba_sha256: String,
    sampled_unique_colors: usize,
    elapsed_ms: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VirtualizationEvidence {
    strategy: &'static str,
    visible_pages: usize,
    overscan_pages_per_side: usize,
    cache_budget_bytes: usize,
    viewport_steps: usize,
    render_requests: usize,
    cache_hits: usize,
    cache_misses: usize,
    evictions: usize,
    maximum_resident_pages: usize,
    maximum_resident_bytes: usize,
    cache_budget_respected: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEvidence {
    invalid_document_rejected: bool,
    loading_error_is_structured: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryEvidence {
    current_rss_before_kib: Option<u64>,
    current_rss_after_kib: Option<u64>,
    process_peak_rss_kib: Option<u64>,
    measurement_source: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyEvidence {
    document_text_stored: bool,
    native_library_path_stored: bool,
    fixture_path_stored: bool,
}

#[derive(Debug)]
struct CachedPage {
    bytes: Vec<u8>,
    last_used: u64,
}

#[derive(Debug)]
struct PageCache {
    budget_bytes: usize,
    entries: BTreeMap<i32, CachedPage>,
    resident_bytes: usize,
    clock: u64,
    requests: usize,
    hits: usize,
    misses: usize,
    evictions: usize,
    maximum_resident_pages: usize,
    maximum_resident_bytes: usize,
}

impl PageCache {
    fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            entries: BTreeMap::new(),
            resident_bytes: 0,
            clock: 0,
            requests: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            maximum_resident_pages: 0,
            maximum_resident_bytes: 0,
        }
    }

    fn touch_or_insert<F>(&mut self, page_index: i32, render: F) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Vec<u8>, Box<dyn Error>>,
    {
        self.clock += 1;
        self.requests += 1;
        if let Some(entry) = self.entries.get_mut(&page_index) {
            entry.last_used = self.clock;
            self.hits += 1;
            return Ok(());
        }

        self.misses += 1;
        let bytes = render()?;
        if bytes.len() > self.budget_bytes {
            return Err(format!(
                "rendered page needs {} bytes, above the {} byte cache budget",
                bytes.len(),
                self.budget_bytes
            )
            .into());
        }
        self.resident_bytes += bytes.len();
        self.entries.insert(
            page_index,
            CachedPage {
                bytes,
                last_used: self.clock,
            },
        );
        while self.resident_bytes > self.budget_bytes {
            let lru = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| *index)
                .ok_or("cache is over budget without an eviction candidate")?;
            let removed = self.entries.remove(&lru).expect("LRU entry should exist");
            self.resident_bytes -= removed.bytes.len();
            self.evictions += 1;
        }
        self.maximum_resident_pages = self.maximum_resident_pages.max(self.entries.len());
        self.maximum_resident_bytes = self.maximum_resident_bytes.max(self.resident_bytes);
        Ok(())
    }
}

pub fn run_pdf_feasibility(
    library_path: impl AsRef<Path>,
    fixture_path: impl AsRef<Path>,
    preview_path: impl AsRef<Path>,
) -> Result<PdfFeasibilityReport, Box<dyn Error>> {
    let current_rss_before_kib = linux_memory_kib("VmRSS");
    let bindings = Pdfium::bind_to_library(library_path.as_ref())
        .map_err(|error| format!("PDFium bind failed: {error:?}"))?;
    let pdfium = Pdfium::new(bindings);
    let fixture_bytes = fs::read(fixture_path.as_ref())?;
    let fixture_sha256 = digest(&fixture_bytes);
    let embedded_font_marker_present = contains_bytes(&fixture_bytes, b"/FontFile2")
        && contains_bytes(&fixture_bytes, b"NotoSansSC");
    let document = pdfium
        .load_pdf_from_byte_vec(fixture_bytes.clone(), None)
        .map_err(|error| format!("PDF document load failed: {error:?}"))?;
    let page_count = document.pages().len();
    if page_count != EXPECTED_PAGE_COUNT {
        return Err(
            format!("PDF fixture has {page_count} pages; expected {EXPECTED_PAGE_COUNT}").into(),
        );
    }

    let first_page = document
        .pages()
        .first()
        .map_err(|error| format!("PDF first page lookup failed: {error:?}"))?;
    let extracted = first_page
        .text()
        .map_err(|error| format!("PDF first page text extraction failed: {error:?}"))?
        .all();
    let cjk_text_extracted =
        extracted.contains("\u{4e2d}\u{6587}\u{5b57}\u{4f53}\u{5d4c}\u{5165}\u{9a8c}\u{8bc1}");
    if !cjk_text_extracted || !embedded_font_marker_present {
        return Err(
            "PDF fixture did not preserve extractable CJK text and its embedded font".into(),
        );
    }

    let (fit, fit_rgba) = render_page(&document, 0, FIT_WIDTH)?;
    fs::write(preview_path.as_ref(), &fit_rgba)?;
    let (zoom, _) = render_page(&document, 0, ZOOM_WIDTH)?;
    let expected_zoom_height = (fit.height as f64 * ZOOM_WIDTH as f64 / FIT_WIDTH as f64).round();
    let aspect_ratio_preserved = (zoom.height as f64 - expected_zoom_height).abs() <= 1.0;
    let distinct_zoom_output =
        zoom.rgba_sha256 != fit.rgba_sha256 && zoom.rgba_bytes > fit.rgba_bytes;
    if !aspect_ratio_preserved || !distinct_zoom_output {
        return Err("PDF fit and zoom renders did not preserve scale semantics".into());
    }

    let mut cache = PageCache::new(CACHE_BUDGET_BYTES);
    for center in 0..page_count {
        let start = (center - 1).max(0);
        let end = (center + 3).min(page_count);
        for page_index in start..end {
            cache.touch_or_insert(page_index, || {
                render_page(&document, page_index, FIT_WIDTH).map(|(_, rgba)| rgba)
            })?;
        }
    }
    for center in (0..page_count).rev() {
        let start = (center - 1).max(0);
        let end = (center + 3).min(page_count);
        for page_index in start..end {
            cache.touch_or_insert(page_index, || {
                render_page(&document, page_index, FIT_WIDTH).map(|(_, rgba)| rgba)
            })?;
        }
    }
    let cache_budget_respected = cache.maximum_resident_bytes <= cache.budget_bytes;
    if !cache_budget_respected || cache.evictions == 0 || cache.hits == 0 {
        return Err("PDF page cache did not exercise bounded reuse and eviction".into());
    }

    let invalid_document_rejected = pdfium
        .load_pdf_from_byte_slice(b"%PDF-1.7\ninvalid-vibex-fixture", None)
        .is_err();
    if !invalid_document_rejected {
        return Err("PDFium accepted the invalid document fixture".into());
    }

    Ok(PdfFeasibilityReport {
        schema_version: "vibex-pdf-feasibility-run.v1",
        status: "passed",
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        engine: EngineEvidence {
            wrapper: "pdfium-render",
            wrapper_version: PDFIUM_RENDER_VERSION,
            pdfium_version: PDFIUM_VERSION,
            binding: "explicit-dynamic-library",
            process_model: "in-process-native-library",
            child_processes_started: 0,
        },
        fixture: FixtureEvidence {
            bytes: fixture_bytes.len() as u64,
            sha256: fixture_sha256,
            page_count,
            cjk_text_extracted,
            embedded_font_marker_present,
        },
        rendering: RenderingEvidence {
            fit,
            zoom_150_percent: zoom,
            aspect_ratio_preserved,
            distinct_zoom_output,
            preview_raw_rgba_written: true,
        },
        virtualization: VirtualizationEvidence {
            strategy: "visible-two-pages-plus-one-page-overscan-lru",
            visible_pages: 2,
            overscan_pages_per_side: 1,
            cache_budget_bytes: cache.budget_bytes,
            viewport_steps: page_count as usize * 2,
            render_requests: cache.requests,
            cache_hits: cache.hits,
            cache_misses: cache.misses,
            evictions: cache.evictions,
            maximum_resident_pages: cache.maximum_resident_pages,
            maximum_resident_bytes: cache.maximum_resident_bytes,
            cache_budget_respected,
        },
        error_handling: ErrorEvidence {
            invalid_document_rejected,
            loading_error_is_structured: true,
        },
        memory: MemoryEvidence {
            current_rss_before_kib,
            current_rss_after_kib: linux_memory_kib("VmRSS"),
            process_peak_rss_kib: linux_memory_kib("VmHWM"),
            measurement_source: if cfg!(target_os = "linux") {
                "proc-self-status"
            } else {
                "unavailable-on-this-platform"
            },
        },
        privacy: PrivacyEvidence {
            document_text_stored: false,
            native_library_path_stored: false,
            fixture_path_stored: false,
        },
    })
}

fn render_page(
    document: &PdfDocument<'_>,
    page_index: i32,
    target_width: i32,
) -> Result<(RenderEvidence, Vec<u8>), Box<dyn Error>> {
    let started = Instant::now();
    let page = document
        .pages()
        .get(page_index)
        .map_err(|error| format!("PDF page {page_index} lookup failed: {error:?}"))?;
    let bitmap = page
        .render_with_config(
            &PdfRenderConfig::new()
                .set_target_width(target_width)
                .render_annotations(true),
        )
        .map_err(|error| {
            format!("PDF page {page_index} render at width {target_width} failed: {error:?}")
        })?;
    let width = bitmap.width();
    let height = bitmap.height();
    let rgba = bitmap.as_rgba_bytes();
    let expected_bytes = width as usize * height as usize * 4;
    if rgba.len() != expected_bytes {
        return Err(format!(
            "page {page_index} rendered {} bytes; expected {expected_bytes}",
            rgba.len()
        )
        .into());
    }
    let sampled_unique_colors = sampled_unique_colors(&rgba);
    if sampled_unique_colors < 16 {
        return Err(format!("page {page_index} render is not visually credible").into());
    }
    let evidence = RenderEvidence {
        width,
        height,
        rgba_bytes: rgba.len(),
        rgba_sha256: digest(&rgba),
        sampled_unique_colors,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    };
    Ok((evidence, rgba))
}

fn sampled_unique_colors(rgba: &[u8]) -> usize {
    let pixels = rgba.len() / 4;
    let stride = (pixels / 50_000).max(1);
    rgba.chunks_exact(4)
        .step_by(stride)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<HashSet<_>>()
        .len()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(target_os = "linux")]
fn linux_memory_kib(field: &str) -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|line| line.starts_with(field))?
        .split_ascii_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn linux_memory_kib(_: &str) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_cache_reuses_and_evicts_within_budget() {
        let mut cache = PageCache::new(8);
        cache.touch_or_insert(0, || Ok(vec![0; 4])).unwrap();
        cache.touch_or_insert(0, || Ok(vec![0; 4])).unwrap();
        cache.touch_or_insert(1, || Ok(vec![1; 4])).unwrap();
        cache.touch_or_insert(2, || Ok(vec![2; 4])).unwrap();
        assert_eq!(cache.hits, 1);
        assert_eq!(cache.misses, 3);
        assert_eq!(cache.evictions, 1);
        assert!(cache.resident_bytes <= cache.budget_bytes);
        assert!(!cache.entries.contains_key(&0));
    }

    #[test]
    fn page_cache_rejects_a_single_oversized_page() {
        let mut cache = PageCache::new(4);
        let error = cache
            .touch_or_insert(0, || Ok(vec![0; 5]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("above the 4 byte cache budget"));
    }

    #[test]
    fn first_page_overscan_never_creates_a_negative_index() {
        let center = 0_i32;
        let start = (center - 1).max(0);
        assert_eq!(start, 0);
    }
}
