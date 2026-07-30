use std::{collections::HashSet, error::Error, path::Path};

use serde::Serialize;
use sha2::{Digest, Sha256};
use vibex_content::{
    ContentResourceMetrics, ContentSurfacePhase, PDFIUM_RENDER_VERSION, PDFIUM_VERSION,
    PdfCancellationToken, PdfDocumentController, PdfPageBitmap, PdfViewportRequest, PdfiumEngine,
    read_pdf_source,
};

const EXPECTED_PAGE_COUNT: usize = 12;
const FIT_WIDTH: u16 = 960;
const ZOOM_WIDTH: u16 = 1_440;
const CONTROLLER_CACHE_PAGE_LIMIT: usize = 3;
const CONTROLLER_CACHE_BUDGET_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfControllerRunReport {
    schema_version: &'static str,
    status: &'static str,
    platform: &'static str,
    architecture: &'static str,
    engine: PdfControllerEngineEvidence,
    fixture: PdfControllerFixtureEvidence,
    encrypted_fixture: PdfControllerEncryptedFixtureEvidence,
    large_inputs: PdfControllerLargeInputEvidence,
    opening: PdfControllerOpenEvidence,
    viewport: PdfControllerViewportEvidence,
    cache: PdfControllerCacheEvidence,
    cancellation: PdfControllerCancellationEvidence,
    failures: PdfControllerFailureEvidence,
    close: PdfControllerCloseEvidence,
    privacy: PdfControllerPrivacyEvidence,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerEngineEvidence {
    backend: &'static str,
    wrapper_version: &'static str,
    pdfium_version: &'static str,
    binding: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerFixtureEvidence {
    bytes: usize,
    sha256: String,
    page_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerEncryptedFixtureEvidence {
    bytes: usize,
    sha256: String,
    page_count: usize,
    missing_password_error_code: String,
    incorrect_password_error_code: String,
    failed_open_cleared_document: bool,
    failed_open_cleared_cache: bool,
    correct_password_opened: bool,
    correct_password_rendered: bool,
    load_failures_after_password_attempts: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerLargeInputEvidence {
    oversized_source_bytes: u64,
    oversized_source_error_code: String,
    too_many_pages_fixture: PdfControllerFixtureEvidence,
    too_many_pages_error_code: String,
    too_many_pages_cleared_document: bool,
    too_many_pages_cleared_cache: bool,
    extreme_page_fixture: PdfControllerFixtureEvidence,
    extreme_page_render_error_code: String,
    extreme_page_render_requests: u64,
    extreme_page_cache_empty: bool,
    load_failures_after_large_attempts: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerOpenEvidence {
    lifecycle_active: bool,
    metadata_complete: bool,
    positive_page_dimensions: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerViewportEvidence {
    fit: PdfControllerRenderEvidence,
    zoom_150_percent: PdfControllerRenderEvidence,
    overscan_page_indexes: Vec<usize>,
    aspect_ratio_preserved: bool,
    distinct_zoom_output: bool,
    repeated_viewport_reused_cache: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerRenderEvidence {
    page_index: usize,
    width: u32,
    height: u32,
    rgba_bytes: usize,
    rgba_sha256: String,
    sampled_unique_colors: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerCacheEvidence {
    page_limit: usize,
    budget_bytes: usize,
    rendered_page_requests: u64,
    evictions: u64,
    resident_pages: usize,
    resident_bytes: usize,
    budget_respected: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerCancellationEvidence {
    pre_cancelled_render_rejected: bool,
    error_code: String,
    cancelled_requests: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerFailureEvidence {
    source_size_error_code: String,
    corrupt_error_code: String,
    lifecycle_error: bool,
    failed_reload_cleared_document: bool,
    failed_reload_cleared_cache: bool,
    load_failures: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerCloseEvidence {
    lifecycle_closed: bool,
    document_released: bool,
    cache_released: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfControllerPrivacyEvidence {
    diagnostics_contain_fixture_path: bool,
    diagnostics_contain_document_sentinel: bool,
    diagnostics_contain_page_content: bool,
    diagnostics_contain_password: bool,
}

pub fn run_pdf_controller(
    library_path: impl AsRef<Path>,
    fixture_path: impl AsRef<Path>,
    encrypted_fixture_path: impl AsRef<Path>,
    too_many_pages_fixture_path: impl AsRef<Path>,
    extreme_page_fixture_path: impl AsRef<Path>,
    oversized_source_path: impl AsRef<Path>,
    encrypted_fixture_password: &str,
) -> Result<PdfControllerRunReport, Box<dyn Error>> {
    let engine = PdfiumEngine::bind(library_path)?;
    let fixture = read_pdf_source(fixture_path.as_ref())?;
    let fixture_sha256 = digest(&fixture);
    let encrypted_fixture = read_pdf_source(encrypted_fixture_path.as_ref())?;
    let encrypted_fixture_sha256 = digest(&encrypted_fixture);
    let too_many_pages_fixture = read_pdf_source(too_many_pages_fixture_path.as_ref())?;
    let too_many_pages_fixture_sha256 = digest(&too_many_pages_fixture);
    let extreme_page_fixture = read_pdf_source(extreme_page_fixture_path.as_ref())?;
    let extreme_page_fixture_sha256 = digest(&extreme_page_fixture);
    let mut controller = PdfDocumentController::with_cache_budget(
        CONTROLLER_CACHE_PAGE_LIMIT,
        CONTROLLER_CACHE_BUDGET_BYTES,
    )?;

    controller.activate(1)?;
    let metadata = controller.open(&engine, fixture.clone(), None, 1)?;
    let metadata_complete = metadata.page_count == EXPECTED_PAGE_COUNT
        && metadata.pages.len() == EXPECTED_PAGE_COUNT
        && metadata
            .pages
            .iter()
            .enumerate()
            .all(|(index, page)| page.page_index == index);
    let positive_page_dimensions = metadata
        .pages
        .iter()
        .all(|page| page.width_points > 0.0 && page.height_points > 0.0);
    let lifecycle_active = controller.lifecycle().phase() == ContentSurfacePhase::Active;
    if !metadata_complete || !positive_page_dimensions || !lifecycle_active {
        return Err("PDF controller returned incomplete page metadata".into());
    }

    let fit_pages = controller.render_viewport(
        &engine,
        None,
        PdfViewportRequest {
            first_visible_page: 0,
            last_visible_page: 0,
            target_width: FIT_WIDTH,
        },
        &PdfCancellationToken::default(),
    )?;
    let overscan_page_indexes = fit_pages
        .iter()
        .map(|page| page.page_index)
        .collect::<Vec<_>>();
    if overscan_page_indexes != vec![0, 1] {
        return Err("PDF controller viewport did not include the bounded page overscan".into());
    }
    let fit = render_evidence(page(&fit_pages, 0)?);
    let rendered_after_first_fit = controller.diagnostics().rendered_page_requests;
    controller.render_viewport(
        &engine,
        None,
        PdfViewportRequest {
            first_visible_page: 0,
            last_visible_page: 0,
            target_width: FIT_WIDTH,
        },
        &PdfCancellationToken::default(),
    )?;
    let repeated_viewport_reused_cache =
        controller.diagnostics().rendered_page_requests == rendered_after_first_fit;

    let zoom_pages = controller.render_viewport(
        &engine,
        None,
        PdfViewportRequest {
            first_visible_page: 0,
            last_visible_page: 0,
            target_width: ZOOM_WIDTH,
        },
        &PdfCancellationToken::default(),
    )?;
    let zoom = render_evidence(page(&zoom_pages, 0)?);
    let expected_zoom_height =
        (fit.height as f64 * f64::from(ZOOM_WIDTH) / f64::from(FIT_WIDTH)).round();
    let aspect_ratio_preserved = (zoom.height as f64 - expected_zoom_height).abs() <= 1.0;
    let distinct_zoom_output = zoom.rgba_sha256 != fit.rgba_sha256
        && zoom.rgba_bytes > fit.rgba_bytes
        && zoom.width == u32::from(ZOOM_WIDTH);
    if !repeated_viewport_reused_cache || !aspect_ratio_preserved || !distinct_zoom_output {
        return Err("PDF controller cache reuse or fit/zoom semantics failed".into());
    }

    for page_index in 0..EXPECTED_PAGE_COUNT {
        controller.render_viewport(
            &engine,
            None,
            PdfViewportRequest {
                first_visible_page: page_index,
                last_visible_page: page_index,
                target_width: FIT_WIDTH,
            },
            &PdfCancellationToken::default(),
        )?;
    }
    let cache_diagnostics = controller.diagnostics();
    if cache_diagnostics.resources.evictions == 0
        || !cache_diagnostics.resources.is_within_budget()
        || cache_diagnostics.resources.resident_items > CONTROLLER_CACHE_PAGE_LIMIT
    {
        return Err("PDF controller did not enforce decoded-page LRU eviction".into());
    }

    let cancellation = PdfCancellationToken::default();
    cancellation.cancel();
    let cancellation_error = controller
        .render_viewport(
            &engine,
            None,
            PdfViewportRequest {
                first_visible_page: 0,
                last_visible_page: 0,
                target_width: FIT_WIDTH,
            },
            &cancellation,
        )
        .expect_err("pre-cancelled PDF render must fail");
    if cancellation_error.code != "pdf_render_cancelled" {
        return Err("PDF controller returned the wrong cancellation error".into());
    }

    controller.activate(2)?;
    let source_size_error = controller
        .open(&engine, Vec::new(), None, 2)
        .expect_err("empty PDF input must fail");
    let source_failure_cleared_document = controller.metadata().is_none();
    let source_failure_cleared_cache = controller.diagnostics().resources.resident_items == 0;
    if source_size_error.code != "pdf_source_size_invalid"
        || !source_failure_cleared_document
        || !source_failure_cleared_cache
        || controller.lifecycle().phase() != ContentSurfacePhase::Error
    {
        return Err("PDF source-size failure retained stale controller state".into());
    }

    controller.activate(3)?;
    controller.open(&engine, fixture.clone(), None, 3)?;
    controller.activate(4)?;
    let corrupt_error = controller
        .open(
            &engine,
            b"%PDF-1.7\nvibex-pdf-document-sentinel".to_vec(),
            None,
            4,
        )
        .expect_err("corrupt PDF input must fail");
    let failure_diagnostics = controller.diagnostics();
    let failed_reload_cleared_document =
        source_failure_cleared_document && controller.metadata().is_none();
    let failed_reload_cleared_cache =
        source_failure_cleared_cache && failure_diagnostics.resources.resident_items == 0;
    if corrupt_error.code != "pdf_document_corrupt"
        || !failed_reload_cleared_document
        || !failed_reload_cleared_cache
        || controller.lifecycle().phase() != ContentSurfacePhase::Error
        || failure_diagnostics.load_failures != 2
    {
        return Err("PDF corrupt reload did not fail closed with a typed error".into());
    }

    controller.activate(5)?;
    let missing_password_error = controller
        .open(&engine, encrypted_fixture.clone(), None, 5)
        .expect_err("encrypted PDF without a password must fail");
    let missing_password_cleared_document = controller.metadata().is_none();
    let missing_password_cleared_cache = controller.diagnostics().resources.resident_items == 0;

    let incorrect_password = format!("{encrypted_fixture_password}-incorrect");
    controller.activate(6)?;
    let incorrect_password_error = controller
        .open(
            &engine,
            encrypted_fixture.clone(),
            Some(&incorrect_password),
            6,
        )
        .expect_err("encrypted PDF with an incorrect password must fail");
    let encrypted_failure_diagnostics = controller.diagnostics();
    let encrypted_failed_open_cleared_document =
        missing_password_cleared_document && controller.metadata().is_none();
    let encrypted_failed_open_cleared_cache = missing_password_cleared_cache
        && encrypted_failure_diagnostics.resources.resident_items == 0;
    if missing_password_error.code != "pdf_password_required"
        || incorrect_password_error.code != "pdf_password_required"
        || !encrypted_failed_open_cleared_document
        || !encrypted_failed_open_cleared_cache
        || controller.lifecycle().phase() != ContentSurfacePhase::Error
        || encrypted_failure_diagnostics.load_failures != 4
    {
        return Err("PDF password failures did not fail closed with a typed error".into());
    }

    let diagnostics_json = serde_json::to_string(&encrypted_failure_diagnostics)?;
    let fixture_path_text = fixture_path.as_ref().to_string_lossy();
    let encrypted_fixture_path_text = encrypted_fixture_path.as_ref().to_string_lossy();
    let privacy = PdfControllerPrivacyEvidence {
        diagnostics_contain_fixture_path: diagnostics_json.contains(fixture_path_text.as_ref())
            || diagnostics_json.contains(encrypted_fixture_path_text.as_ref()),
        diagnostics_contain_document_sentinel: diagnostics_json
            .contains("vibex-pdf-document-sentinel"),
        diagnostics_contain_page_content: diagnostics_json.contains("pageContent")
            || diagnostics_json.contains("documentText"),
        diagnostics_contain_password: diagnostics_json.contains(encrypted_fixture_password)
            || diagnostics_json.contains(&incorrect_password),
    };
    if privacy.diagnostics_contain_fixture_path
        || privacy.diagnostics_contain_document_sentinel
        || privacy.diagnostics_contain_page_content
        || privacy.diagnostics_contain_password
    {
        return Err(
            "PDF controller diagnostics leaked document identity, content, or password".into(),
        );
    }

    controller.activate(7)?;
    let encrypted_metadata = controller.open(
        &engine,
        encrypted_fixture.clone(),
        Some(encrypted_fixture_password),
        7,
    )?;
    let correct_password_opened = encrypted_metadata.page_count == 1;
    let encrypted_pages = controller.render_viewport(
        &engine,
        Some(encrypted_fixture_password),
        PdfViewportRequest {
            first_visible_page: 0,
            last_visible_page: 0,
            target_width: FIT_WIDTH,
        },
        &PdfCancellationToken::default(),
    )?;
    let encrypted_page = page(&encrypted_pages, 0)?;
    let correct_password_rendered = encrypted_page.width == u32::from(FIT_WIDTH)
        && encrypted_page.height > 0
        && encrypted_page.rgba.len()
            == encrypted_page.width as usize * encrypted_page.height as usize * 4;
    if !correct_password_opened || !correct_password_rendered {
        return Err("PDF controller did not open and render the encrypted fixture".into());
    }

    let oversized_source_bytes = std::fs::metadata(oversized_source_path.as_ref())?.len();
    let oversized_source_error = read_pdf_source(oversized_source_path.as_ref())
        .expect_err("oversized sparse PDF source must fail before allocation");
    if oversized_source_error.code != "pdf_source_size_invalid" {
        return Err("oversized PDF source returned the wrong typed error".into());
    }

    controller.activate(8)?;
    let too_many_pages_error = controller
        .open(&engine, too_many_pages_fixture.clone(), None, 8)
        .expect_err("PDF fixture with more than 10,000 pages must fail");
    let too_many_pages_diagnostics = controller.diagnostics();
    let too_many_pages_cleared_document = controller.metadata().is_none();
    let too_many_pages_cleared_cache = too_many_pages_diagnostics.resources.resident_items == 0;
    if too_many_pages_error.code != "pdf_page_count_unsupported"
        || !too_many_pages_cleared_document
        || !too_many_pages_cleared_cache
        || too_many_pages_diagnostics.load_failures != 5
        || controller.lifecycle().phase() != ContentSurfacePhase::Error
    {
        return Err("PDF page-count limit did not fail closed".into());
    }

    controller.activate(9)?;
    let extreme_metadata = controller.open(&engine, extreme_page_fixture.clone(), None, 9)?;
    if extreme_metadata.page_count != 1 {
        return Err("extreme-page PDF fixture did not open as one page".into());
    }
    let rendered_before_extreme = controller.diagnostics().rendered_page_requests;
    let extreme_page_render_error = controller
        .render_viewport(
            &engine,
            None,
            PdfViewportRequest {
                first_visible_page: 0,
                last_visible_page: 0,
                target_width: FIT_WIDTH,
            },
            &PdfCancellationToken::default(),
        )
        .expect_err("extreme-page PDF render must fail before native allocation");
    let extreme_page_diagnostics = controller.diagnostics();
    let extreme_page_render_requests = extreme_page_diagnostics
        .rendered_page_requests
        .saturating_sub(rendered_before_extreme);
    let extreme_page_cache_empty = extreme_page_diagnostics.resources.resident_items == 0
        && extreme_page_diagnostics.resources.resident_bytes == 0;
    if extreme_page_render_error.code != "pdf_page_exceeds_cache_budget"
        || extreme_page_render_requests != 0
        || !extreme_page_cache_empty
        || controller.lifecycle().phase() != ContentSurfacePhase::Active
    {
        return Err("extreme PDF page was not rejected before native rendering".into());
    }

    controller.close(9)?;
    let close_diagnostics = controller.diagnostics();
    let close = PdfControllerCloseEvidence {
        lifecycle_closed: controller.lifecycle().phase() == ContentSurfacePhase::Closed,
        document_released: controller.metadata().is_none() && !close_diagnostics.document_loaded,
        cache_released: close_diagnostics.resources.resident_items == 0
            && close_diagnostics.resources.resident_bytes == 0,
    };
    if !close.lifecycle_closed || !close.document_released || !close.cache_released {
        return Err("PDF controller close did not release document resources".into());
    }

    Ok(PdfControllerRunReport {
        schema_version: "vibex-pdf-controller-run.v1",
        status: "passed",
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        engine: PdfControllerEngineEvidence {
            backend: "pdfium-render",
            wrapper_version: PDFIUM_RENDER_VERSION,
            pdfium_version: PDFIUM_VERSION,
            binding: "explicit-dynamic-library",
        },
        fixture: PdfControllerFixtureEvidence {
            bytes: fixture.len(),
            sha256: fixture_sha256,
            page_count: EXPECTED_PAGE_COUNT,
        },
        encrypted_fixture: PdfControllerEncryptedFixtureEvidence {
            bytes: encrypted_fixture.len(),
            sha256: encrypted_fixture_sha256,
            page_count: 1,
            missing_password_error_code: missing_password_error.code,
            incorrect_password_error_code: incorrect_password_error.code,
            failed_open_cleared_document: encrypted_failed_open_cleared_document,
            failed_open_cleared_cache: encrypted_failed_open_cleared_cache,
            correct_password_opened,
            correct_password_rendered,
            load_failures_after_password_attempts: encrypted_failure_diagnostics.load_failures,
        },
        large_inputs: PdfControllerLargeInputEvidence {
            oversized_source_bytes,
            oversized_source_error_code: oversized_source_error.code,
            too_many_pages_fixture: PdfControllerFixtureEvidence {
                bytes: too_many_pages_fixture.len(),
                sha256: too_many_pages_fixture_sha256,
                page_count: 10_001,
            },
            too_many_pages_error_code: too_many_pages_error.code,
            too_many_pages_cleared_document,
            too_many_pages_cleared_cache,
            extreme_page_fixture: PdfControllerFixtureEvidence {
                bytes: extreme_page_fixture.len(),
                sha256: extreme_page_fixture_sha256,
                page_count: 1,
            },
            extreme_page_render_error_code: extreme_page_render_error.code,
            extreme_page_render_requests,
            extreme_page_cache_empty,
            load_failures_after_large_attempts: too_many_pages_diagnostics.load_failures,
        },
        opening: PdfControllerOpenEvidence {
            lifecycle_active,
            metadata_complete,
            positive_page_dimensions,
        },
        viewport: PdfControllerViewportEvidence {
            fit,
            zoom_150_percent: zoom,
            overscan_page_indexes,
            aspect_ratio_preserved,
            distinct_zoom_output,
            repeated_viewport_reused_cache,
        },
        cache: cache_evidence(
            cache_diagnostics.resources,
            cache_diagnostics.rendered_page_requests,
        ),
        cancellation: PdfControllerCancellationEvidence {
            pre_cancelled_render_rejected: true,
            error_code: cancellation_error.code,
            cancelled_requests: controller.diagnostics().cancelled_requests,
        },
        failures: PdfControllerFailureEvidence {
            source_size_error_code: source_size_error.code,
            corrupt_error_code: corrupt_error.code,
            lifecycle_error: true,
            failed_reload_cleared_document,
            failed_reload_cleared_cache,
            load_failures: failure_diagnostics.load_failures,
        },
        close,
        privacy,
        limitations: vec![
            "this controller run is headless and does not claim GPUI page controls or physical input",
            "PDFium native-call crash and hard-timeout isolation remain open and are not claimed by typed error tests",
        ],
    })
}

fn page(pages: &[PdfPageBitmap], page_index: usize) -> Result<&PdfPageBitmap, Box<dyn Error>> {
    pages
        .iter()
        .find(|page| page.page_index == page_index)
        .ok_or_else(|| format!("rendered viewport omitted page {page_index}").into())
}

fn render_evidence(bitmap: &PdfPageBitmap) -> PdfControllerRenderEvidence {
    PdfControllerRenderEvidence {
        page_index: bitmap.page_index,
        width: bitmap.width,
        height: bitmap.height,
        rgba_bytes: bitmap.rgba.len(),
        rgba_sha256: digest(&bitmap.rgba),
        sampled_unique_colors: sampled_unique_colors(&bitmap.rgba),
    }
}

fn cache_evidence(
    resources: ContentResourceMetrics,
    rendered_page_requests: u64,
) -> PdfControllerCacheEvidence {
    PdfControllerCacheEvidence {
        page_limit: resources.budget_items,
        budget_bytes: resources.budget_bytes,
        rendered_page_requests,
        evictions: resources.evictions,
        resident_pages: resources.resident_items,
        resident_bytes: resources.resident_bytes,
        budget_respected: resources.is_within_budget(),
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sampled_unique_colors(rgba: &[u8]) -> usize {
    let pixels = rgba.len() / 4;
    let stride = (pixels / 4_096).max(1);
    rgba.chunks_exact(4)
        .step_by(stride)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<HashSet<_>>()
        .len()
}
