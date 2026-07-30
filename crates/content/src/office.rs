use std::{
    io::{Cursor, Read},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use quick_xml::{
    Reader,
    events::{BytesRef, Event},
};
use serde::Serialize;
use vibex_core::{VibexError, VibexResult};
use zip::ZipArchive;

use crate::{
    ContentResourceMetrics, ContentSurfaceKind, ContentSurfaceLifecycle, ContentSurfaceOrigin,
    GenerationDisposition,
};

pub const OFFICE_MAX_ARCHIVE_ENTRIES: usize = 512;
pub const OFFICE_MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
pub const OFFICE_MAX_DECODED_BYTES: usize = 32 * 1024 * 1024;
pub const OFFICE_MAX_COMPRESSION_RATIO: u64 = 100;
pub const OFFICE_XML_DEPTH_LIMIT: usize = 96;
pub const OFFICE_TEXT_LIMIT: usize = 256 * 1024;
pub const OFFICE_SHEET_ROW_LIMIT: usize = 80;
pub const OFFICE_SHEET_COLUMN_LIMIT: usize = 20;
pub const OFFICE_PPTX_SLIDE_LIMIT: usize = 200;
pub const OFFICE_DEFAULT_PARSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OfficeFileKind {
    Docx,
    Xlsx,
    Ods,
    Pptx,
    LegacyDoc,
    LegacyXls,
    LegacyPpt,
    Unsupported,
}

impl OfficeFileKind {
    pub fn from_path(path: impl AsRef<Path>) -> Self {
        match path
            .as_ref()
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref()
        {
            Some("docx") => Self::Docx,
            Some("xlsx") => Self::Xlsx,
            Some("ods") => Self::Ods,
            Some("pptx") => Self::Pptx,
            Some("doc") => Self::LegacyDoc,
            Some("xls") => Self::LegacyXls,
            Some("ppt") => Self::LegacyPpt,
            _ => Self::Unsupported,
        }
    }

    pub fn supported(self) -> bool {
        matches!(self, Self::Docx | Self::Xlsx | Self::Ods | Self::Pptx)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficeUnsupportedDocument {
    pub kind: OfficeFileKind,
    pub reason_code: String,
    pub system_open_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficeTextDocument {
    pub kind: OfficeFileKind,
    pub title: Option<String>,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficeSheetDocument {
    pub kind: OfficeFileKind,
    pub sheet_name: String,
    pub rows: Vec<Vec<String>>,
    pub truncated_rows: bool,
    pub truncated_columns: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficePresentationDocument {
    pub kind: OfficeFileKind,
    pub slides: Vec<OfficeSlideText>,
    pub truncated_slides: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficeSlideText {
    pub slide_index: usize,
    pub text: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OfficeDocumentModel {
    Text(OfficeTextDocument),
    Sheet(OfficeSheetDocument),
    Presentation(OfficePresentationDocument),
    Unsupported(OfficeUnsupportedDocument),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficeParserDiagnostics {
    pub parser: &'static str,
    pub kind: OfficeFileKind,
    pub archive_entries: usize,
    pub decoded_bytes: usize,
    pub rejected_entries: u64,
    pub cancelled_requests: u64,
    pub timed_out_requests: u64,
    pub document_loaded: bool,
    pub resources: ContentResourceMetrics,
}

#[derive(Debug, Default)]
struct ArchiveStats {
    entries: usize,
    decoded_bytes: usize,
    rejected_entries: u64,
}

#[derive(Debug, Clone, Default)]
pub struct OfficeCancellationToken(Arc<AtomicBool>);

impl OfficeCancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct OfficeParseGuard<'a> {
    cancellation: &'a OfficeCancellationToken,
    deadline: Instant,
}

impl<'a> OfficeParseGuard<'a> {
    fn new(cancellation: &'a OfficeCancellationToken, timeout: Duration) -> Self {
        Self {
            cancellation,
            deadline: Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(Instant::now),
        }
    }

    fn check(&self) -> VibexResult<()> {
        if self.cancellation.is_cancelled() {
            return Err(VibexError::conflict(
                "office_parse_cancelled",
                "Office document parsing was cancelled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(VibexError::process(
                "office_parse_timeout",
                "Office document parsing exceeded its time budget",
            ));
        }
        Ok(())
    }
}

pub struct OfficeDocumentController {
    lifecycle: ContentSurfaceLifecycle,
    diagnostics: OfficeParserDiagnostics,
    model: Option<OfficeDocumentModel>,
}

impl OfficeDocumentController {
    pub fn new() -> Self {
        Self {
            lifecycle: ContentSurfaceLifecycle::restored(
                ContentSurfaceKind::Office,
                ContentSurfaceOrigin::Preview,
            ),
            diagnostics: OfficeParserDiagnostics {
                parser: "quick-xml+zip",
                kind: OfficeFileKind::Unsupported,
                archive_entries: 0,
                decoded_bytes: 0,
                rejected_entries: 0,
                cancelled_requests: 0,
                timed_out_requests: 0,
                document_loaded: false,
                resources: ContentResourceMetrics {
                    resident_items: 0,
                    resident_bytes: 0,
                    budget_items: OFFICE_MAX_ARCHIVE_ENTRIES,
                    budget_bytes: OFFICE_MAX_DECODED_BYTES,
                    evictions: 0,
                },
            },
            model: None,
        }
    }

    pub fn lifecycle(&self) -> &ContentSurfaceLifecycle {
        &self.lifecycle
    }

    pub fn activate(&mut self, generation: u64) -> VibexResult<GenerationDisposition> {
        self.lifecycle.activate(generation)
    }

    pub fn model(&self) -> Option<&OfficeDocumentModel> {
        self.model.as_ref()
    }

    pub fn diagnostics(&self) -> &OfficeParserDiagnostics {
        &self.diagnostics
    }

    pub fn open(
        &mut self,
        path: impl AsRef<Path>,
        bytes: Vec<u8>,
        generation: u64,
    ) -> VibexResult<&OfficeDocumentModel> {
        let cancellation = OfficeCancellationToken::default();
        self.open_with_control(
            path,
            bytes,
            generation,
            &cancellation,
            OFFICE_DEFAULT_PARSE_TIMEOUT,
        )
    }

    pub fn open_with_control(
        &mut self,
        path: impl AsRef<Path>,
        bytes: Vec<u8>,
        generation: u64,
        cancellation: &OfficeCancellationToken,
        timeout: Duration,
    ) -> VibexResult<&OfficeDocumentModel> {
        if generation != self.lifecycle.activation_generation() {
            return Err(VibexError::conflict(
                "office_activation_stale",
                "Office activation changed before the document opened",
            ));
        }
        let kind = OfficeFileKind::from_path(path.as_ref());
        self.diagnostics.kind = kind;
        self.diagnostics.archive_entries = 0;
        self.diagnostics.decoded_bytes = 0;
        self.diagnostics.rejected_entries = 0;
        self.diagnostics.document_loaded = false;
        self.diagnostics.resources.resident_items = 0;
        self.diagnostics.resources.resident_bytes = 0;
        self.model = None;
        if !kind.supported() {
            self.model = Some(OfficeDocumentModel::Unsupported(
                OfficeUnsupportedDocument {
                    kind,
                    reason_code: match kind {
                        OfficeFileKind::LegacyDoc
                        | OfficeFileKind::LegacyXls
                        | OfficeFileKind::LegacyPpt => "office_legacy_format_unsupported",
                        _ => "office_format_unsupported",
                    }
                    .to_string(),
                    system_open_available: true,
                },
            ));
            self.diagnostics.document_loaded = true;
            self.lifecycle.finish_load(generation)?;
            return Ok(self.model.as_ref().unwrap());
        }
        if bytes.is_empty() || bytes.len() > OFFICE_MAX_DECODED_BYTES {
            let error = VibexError::validation(
                "office_source_size_invalid",
                "Office document is empty or exceeds the source size limit",
            );
            self.lifecycle.failed(generation, &error.code)?;
            return Err(error);
        }
        self.lifecycle.begin_load(generation)?;
        let guard = OfficeParseGuard::new(cancellation, timeout);
        let parsed: VibexResult<(OfficeDocumentModel, ArchiveStats)> = (|| {
            guard.check()?;
            let mut archive = open_office_archive(bytes)?;
            let mut stats = validate_archive(&mut archive, &guard)?;
            let model = match kind {
                OfficeFileKind::Docx => {
                    OfficeDocumentModel::Text(parse_docx(&mut archive, &mut stats, &guard)?)
                }
                OfficeFileKind::Xlsx => {
                    OfficeDocumentModel::Sheet(parse_xlsx(&mut archive, &mut stats, &guard)?)
                }
                OfficeFileKind::Ods => {
                    OfficeDocumentModel::Sheet(parse_ods(&mut archive, &mut stats, &guard)?)
                }
                OfficeFileKind::Pptx => {
                    OfficeDocumentModel::Presentation(parse_pptx(&mut archive, &mut stats, &guard)?)
                }
                _ => unreachable!("unsupported Office kind returned before archive parsing"),
            };
            Ok((model, stats))
        })();
        let (model, stats) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                match error.code.as_str() {
                    "office_parse_cancelled" => {
                        self.diagnostics.cancelled_requests =
                            self.diagnostics.cancelled_requests.saturating_add(1);
                    }
                    "office_parse_timeout" => {
                        self.diagnostics.timed_out_requests =
                            self.diagnostics.timed_out_requests.saturating_add(1);
                    }
                    _ => {}
                }
                self.lifecycle.failed(generation, &error.code)?;
                return Err(error);
            }
        };
        self.diagnostics.archive_entries = stats.entries;
        self.diagnostics.decoded_bytes = stats.decoded_bytes;
        self.diagnostics.rejected_entries = stats.rejected_entries;
        self.diagnostics.document_loaded = true;
        self.diagnostics.resources = ContentResourceMetrics {
            resident_items: stats.entries,
            resident_bytes: stats.decoded_bytes,
            budget_items: OFFICE_MAX_ARCHIVE_ENTRIES,
            budget_bytes: OFFICE_MAX_DECODED_BYTES,
            evictions: stats.rejected_entries,
        };
        self.model = Some(model);
        self.lifecycle.finish_load(generation)?;
        Ok(self.model.as_ref().unwrap())
    }

    pub fn close(&mut self, generation: u64) -> VibexResult<()> {
        self.lifecycle.close(generation)?;
        self.model = None;
        self.diagnostics.document_loaded = false;
        self.diagnostics.resources.resident_items = 0;
        self.diagnostics.resources.resident_bytes = 0;
        Ok(())
    }
}

impl Default for OfficeDocumentController {
    fn default() -> Self {
        Self::new()
    }
}

fn open_office_archive(bytes: Vec<u8>) -> VibexResult<ZipArchive<Cursor<Vec<u8>>>> {
    ZipArchive::new(Cursor::new(bytes)).map_err(|_| {
        VibexError::validation("office_zip_invalid", "Office document archive is invalid")
    })
}

fn validate_archive<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<ArchiveStats> {
    guard.check()?;
    if archive.is_empty() || archive.len() > OFFICE_MAX_ARCHIVE_ENTRIES {
        return Err(VibexError::validation(
            "office_archive_entry_count_invalid",
            "Office archive has an unsupported number of entries",
        ));
    }
    let mut stats = ArchiveStats {
        entries: archive.len(),
        ..ArchiveStats::default()
    };
    for index in 0..archive.len() {
        guard.check()?;
        let file = archive.by_index(index).map_err(|_| {
            VibexError::validation(
                "office_archive_entry_invalid",
                "Office archive entry is invalid",
            )
        })?;
        if file.enclosed_name().is_none() || file.name().contains('\0') {
            return Err(VibexError::validation(
                "office_archive_path_invalid",
                "Office archive entry path is unsafe",
            ));
        }
        if file.size() > OFFICE_MAX_ENTRY_BYTES {
            return Err(VibexError::capability(
                "office_archive_entry_too_large",
                "Office archive entry exceeds the decoded size limit",
            ));
        }
        if file.compressed_size() > 0
            && file.size() / file.compressed_size().max(1) > OFFICE_MAX_COMPRESSION_RATIO
        {
            return Err(VibexError::capability(
                "office_archive_zip_bomb",
                "Office archive compression ratio is unsafe",
            ));
        }
        stats.decoded_bytes = stats.decoded_bytes.saturating_add(file.size() as usize);
        if stats.decoded_bytes > OFFICE_MAX_DECODED_BYTES {
            return Err(VibexError::capability(
                "office_archive_decoded_size_exceeded",
                "Office archive exceeds the decoded byte budget",
            ));
        }
    }
    Ok(stats)
}

fn read_archive_text<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    stats: &mut ArchiveStats,
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<String> {
    guard.check()?;
    let file = archive.by_name(name).map_err(|_| {
        VibexError::validation(
            "office_required_part_missing",
            "Office document is missing a required part",
        )
    })?;
    if file.size() > OFFICE_MAX_ENTRY_BYTES {
        stats.rejected_entries = stats.rejected_entries.saturating_add(1);
        return Err(VibexError::capability(
            "office_part_too_large",
            "Office document part exceeds the decoded size limit",
        ));
    }
    let mut text = String::new();
    file.take(OFFICE_MAX_ENTRY_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| {
            VibexError::validation(
                "office_part_encoding_unsupported",
                "Office document part is not valid UTF-8 XML",
            )
        })?;
    guard.check()?;
    if text.len() as u64 > OFFICE_MAX_ENTRY_BYTES {
        return Err(VibexError::capability(
            "office_part_too_large",
            "Office document part exceeds the decoded size limit",
        ));
    }
    Ok(text)
}

fn read_optional_archive_text<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    stats: &mut ArchiveStats,
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<Option<String>> {
    guard.check()?;
    if !archive.file_names().any(|candidate| candidate == name) {
        return Ok(None);
    }
    read_archive_text(archive, name, stats, guard).map(Some)
}

fn extract_text_nodes(
    xml: &str,
    local_names: &[&[u8]],
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<Vec<String>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut capture = false;
    let mut output = Vec::new();
    loop {
        guard.check()?;
        match reader.read_event_into(&mut buf).map_err(|_| {
            VibexError::validation("office_xml_malformed", "Office XML is malformed")
        })? {
            Event::Start(event) => {
                depth += 1;
                if depth > OFFICE_XML_DEPTH_LIMIT {
                    return Err(VibexError::capability(
                        "office_xml_depth_exceeded",
                        "Office XML exceeds the nesting depth limit",
                    ));
                }
                capture = local_names
                    .iter()
                    .any(|name| local_name(event.name().as_ref()) == *name);
            }
            Event::Empty(event) => {
                if local_names
                    .iter()
                    .any(|name| local_name(event.name().as_ref()) == *name)
                {
                    output.push(String::new());
                }
            }
            Event::Text(event) if capture => {
                let text = event.decode().map_err(|_| {
                    VibexError::validation(
                        "office_xml_text_encoding_unsupported",
                        "Office XML text encoding is unsupported",
                    )
                })?;
                push_limited_text(&mut output, text.as_ref());
            }
            Event::GeneralRef(reference) if capture => {
                push_limited_text(&mut output, &decode_xml_reference(&reference)?);
            }
            Event::GeneralRef(reference) => {
                decode_xml_reference(&reference)?;
            }
            Event::DocType(_) => {
                return Err(VibexError::capability(
                    "office_xml_doctype_unsupported",
                    "Office XML document types are unsupported",
                ));
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                capture = false;
            }
            Event::Eof if depth != 0 => {
                return Err(VibexError::validation(
                    "office_xml_malformed",
                    "Office XML is malformed",
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(output
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .collect())
}

fn push_limited_text(output: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let current: usize = output.iter().map(String::len).sum();
    if current >= OFFICE_TEXT_LIMIT {
        return;
    }
    let remaining = OFFICE_TEXT_LIMIT - current;
    let mut end = value.len().min(remaining);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    if end > 0 {
        output.push(value[..end].to_string());
    }
}

fn local_name(name: &[u8]) -> &[u8] {
    name.iter()
        .rposition(|byte| *byte == b':')
        .map_or(name, |index| &name[index + 1..])
}

fn decode_xml_reference(reference: &BytesRef<'_>) -> VibexResult<String> {
    if let Some(value) = reference.resolve_char_ref().map_err(|_| {
        VibexError::validation(
            "office_xml_entity_unsupported",
            "Office XML contains an unsupported entity reference",
        )
    })? {
        return Ok(value.to_string());
    }
    let name = reference.decode().map_err(|_| {
        VibexError::validation(
            "office_xml_text_encoding_unsupported",
            "Office XML text encoding is unsupported",
        )
    })?;
    match name.as_ref() {
        "amp" => Ok("&".to_string()),
        "lt" => Ok("<".to_string()),
        "gt" => Ok(">".to_string()),
        "apos" => Ok("'".to_string()),
        "quot" => Ok("\"".to_string()),
        _ => Err(VibexError::capability(
            "office_xml_entity_unsupported",
            "Office XML contains an unsupported entity reference",
        )),
    }
}

fn parse_docx<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    stats: &mut ArchiveStats,
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<OfficeTextDocument> {
    let xml = read_archive_text(archive, "word/document.xml", stats, guard)?;
    let paragraphs = extract_text_nodes(&xml, &[b"t"], guard)?;
    Ok(OfficeTextDocument {
        kind: OfficeFileKind::Docx,
        title: None,
        paragraphs,
    })
}

fn parse_xlsx<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    stats: &mut ArchiveStats,
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<OfficeSheetDocument> {
    let shared_strings = read_optional_archive_text(archive, "xl/sharedStrings.xml", stats, guard)?
        .map(|xml| extract_text_nodes(&xml, &[b"t"], guard))
        .transpose()?
        .unwrap_or_default();
    let sheet_xml = read_archive_text(archive, "xl/worksheets/sheet1.xml", stats, guard)?;
    let rows = parse_xlsx_sheet(&sheet_xml, &shared_strings, guard)?;
    Ok(OfficeSheetDocument {
        kind: OfficeFileKind::Xlsx,
        sheet_name: "Sheet1".to_string(),
        truncated_rows: rows.len() >= OFFICE_SHEET_ROW_LIMIT,
        truncated_columns: rows
            .iter()
            .any(|row| row.len() >= OFFICE_SHEET_COLUMN_LIMIT),
        rows,
    })
}

fn parse_xlsx_sheet(
    xml: &str,
    shared_strings: &[String],
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<Vec<Vec<String>>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut rows = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut in_cell = false;
    let mut current_cell_shared = false;
    let mut in_value = false;
    let mut cell_value = String::new();
    let mut depth = 0usize;
    loop {
        guard.check()?;
        match reader.read_event_into(&mut buf).map_err(|_| {
            VibexError::validation("office_xml_malformed", "Office XML is malformed")
        })? {
            Event::Start(event) => {
                depth += 1;
                if depth > OFFICE_XML_DEPTH_LIMIT {
                    return Err(VibexError::capability(
                        "office_xml_depth_exceeded",
                        "Office XML exceeds the nesting depth limit",
                    ));
                }
                match local_name(event.name().as_ref()) {
                    b"row" => current_row = Vec::new(),
                    b"c" => {
                        in_cell = true;
                        current_cell_shared = event.attributes().flatten().any(|attribute| {
                            local_name(attribute.key.as_ref()) == b"t"
                                && attribute.value.as_ref() == b"s"
                        });
                        cell_value.clear();
                    }
                    b"v" | b"t" if in_cell => in_value = true,
                    _ => {}
                }
            }
            Event::Text(event) if in_value => {
                let text = event.decode().map_err(|_| {
                    VibexError::validation(
                        "office_xml_text_encoding_unsupported",
                        "Office XML text encoding is unsupported",
                    )
                })?;
                cell_value.push_str(text.as_ref());
            }
            Event::GeneralRef(reference) if in_value => {
                cell_value.push_str(&decode_xml_reference(&reference)?);
            }
            Event::GeneralRef(reference) => {
                decode_xml_reference(&reference)?;
            }
            Event::DocType(_) => {
                return Err(VibexError::capability(
                    "office_xml_doctype_unsupported",
                    "Office XML document types are unsupported",
                ));
            }
            Event::End(event) => {
                match local_name(event.name().as_ref()) {
                    b"v" | b"t" => in_value = false,
                    b"c" => {
                        let value = if current_cell_shared {
                            cell_value
                                .parse::<usize>()
                                .ok()
                                .and_then(|index| shared_strings.get(index).cloned())
                                .unwrap_or_default()
                        } else {
                            cell_value.clone()
                        };
                        if current_row.len() < OFFICE_SHEET_COLUMN_LIMIT {
                            current_row.push(value);
                        }
                        in_cell = false;
                    }
                    b"row" if rows.len() < OFFICE_SHEET_ROW_LIMIT => {
                        rows.push(current_row.clone());
                    }
                    _ => {}
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof if depth != 0 => {
                return Err(VibexError::validation(
                    "office_xml_malformed",
                    "Office XML is malformed",
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(rows)
}

fn parse_ods<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    stats: &mut ArchiveStats,
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<OfficeSheetDocument> {
    let xml = read_archive_text(archive, "content.xml", stats, guard)?;
    let mut rows = Vec::new();
    let mut current = Vec::new();
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_cell = false;
    let mut depth = 0usize;
    loop {
        guard.check()?;
        match reader.read_event_into(&mut buf).map_err(|_| {
            VibexError::validation("office_xml_malformed", "Office XML is malformed")
        })? {
            Event::Start(event) => {
                depth += 1;
                if depth > OFFICE_XML_DEPTH_LIMIT {
                    return Err(VibexError::capability(
                        "office_xml_depth_exceeded",
                        "Office XML exceeds the nesting depth limit",
                    ));
                }
                match local_name(event.name().as_ref()) {
                    b"table-row" => current = Vec::new(),
                    b"table-cell" => in_cell = true,
                    _ => {}
                }
            }
            Event::Text(event) if in_cell && current.len() < OFFICE_SHEET_COLUMN_LIMIT => {
                let text = event.decode().map_err(|_| {
                    VibexError::validation(
                        "office_xml_text_encoding_unsupported",
                        "Office XML text encoding is unsupported",
                    )
                })?;
                if !text.trim().is_empty() {
                    current.push(text.trim().to_string());
                }
            }
            Event::GeneralRef(reference)
                if in_cell && current.len() < OFFICE_SHEET_COLUMN_LIMIT =>
            {
                let value = decode_xml_reference(&reference)?;
                if !value.is_empty() {
                    current.push(value);
                }
            }
            Event::GeneralRef(reference) => {
                decode_xml_reference(&reference)?;
            }
            Event::DocType(_) => {
                return Err(VibexError::capability(
                    "office_xml_doctype_unsupported",
                    "Office XML document types are unsupported",
                ));
            }
            Event::End(event) => {
                match local_name(event.name().as_ref()) {
                    b"table-cell" => in_cell = false,
                    b"table-row" if rows.len() < OFFICE_SHEET_ROW_LIMIT => {
                        rows.push(current.clone());
                    }
                    _ => {}
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof if depth != 0 => {
                return Err(VibexError::validation(
                    "office_xml_malformed",
                    "Office XML is malformed",
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(OfficeSheetDocument {
        kind: OfficeFileKind::Ods,
        sheet_name: "Sheet1".to_string(),
        truncated_rows: rows.len() >= OFFICE_SHEET_ROW_LIMIT,
        truncated_columns: rows
            .iter()
            .any(|row| row.len() >= OFFICE_SHEET_COLUMN_LIMIT),
        rows,
    })
}

fn parse_pptx<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    stats: &mut ArchiveStats,
    guard: &OfficeParseGuard<'_>,
) -> VibexResult<OfficePresentationDocument> {
    let mut slide_names = Vec::new();
    for index in 0..archive.len() {
        guard.check()?;
        let file = archive.by_index(index).map_err(|_| {
            VibexError::validation(
                "office_archive_entry_invalid",
                "Office archive entry is invalid",
            )
        })?;
        let name = file.name().to_string();
        if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
            slide_names.push(name);
        }
    }
    slide_names.sort_by_key(|name| slide_number(name));
    let truncated_slides = slide_names.len() > OFFICE_PPTX_SLIDE_LIMIT;
    let mut slides = Vec::new();
    for (slide_index, name) in slide_names
        .into_iter()
        .take(OFFICE_PPTX_SLIDE_LIMIT)
        .enumerate()
    {
        guard.check()?;
        let xml = read_archive_text(archive, &name, stats, guard)?;
        slides.push(OfficeSlideText {
            slide_index,
            text: extract_text_nodes(&xml, &[b"t"], guard)?,
        });
    }
    Ok(OfficePresentationDocument {
        kind: OfficeFileKind::Pptx,
        slides,
        truncated_slides,
    })
}

fn slide_number(name: &str) -> usize {
    name.rsplit('/')
        .next()
        .and_then(|file| file.strip_prefix("slide"))
        .and_then(|rest| rest.strip_suffix(".xml"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use crate::ContentSurfacePhase;

    use super::*;

    fn zip_with(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut bytes);
            for (name, body) in entries {
                zip.start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
                )
                .unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        bytes.into_inner()
    }

    fn zip_with_bytes(entries: &[(&str, &[u8], CompressionMethod)]) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut bytes);
            for (name, body, method) in entries {
                zip.start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(*method),
                )
                .unwrap();
                zip.write_all(body).unwrap();
            }
            zip.finish().unwrap();
        }
        bytes.into_inner()
    }

    #[test]
    fn docx_extracts_plain_text_without_storing_source_bytes_in_diagnostics() {
        let bytes = zip_with(&[(
            "word/document.xml",
            r#"<w:document><w:p><w:r><w:t>Hello</w:t></w:r></w:p><w:p><w:r><w:t>世界</w:t></w:r></w:p></w:document>"#,
        )]);
        let mut controller = OfficeDocumentController::new();
        controller.lifecycle.activate(1).unwrap();
        let model = controller.open("sample.docx", bytes, 1).unwrap();
        let OfficeDocumentModel::Text(document) = model else {
            panic!("expected text document")
        };
        assert_eq!(document.paragraphs, vec!["Hello", "世界"]);
        let json = serde_json::to_string(controller.diagnostics()).unwrap();
        assert!(!json.contains("Hello"));
        assert!(!json.contains("sample.docx"));
    }

    #[test]
    fn xlsx_limits_first_sheet_to_current_parity_bounds() {
        let shared = r#"<sst><si><t>A</t></si><si><t>B</t></si></sst>"#;
        let sheet = r#"<worksheet><sheetData><row><c t="s"><v>0</v></c><c><v>42</v></c><c t="s"><v>1</v></c></row></sheetData></worksheet>"#;
        let mut controller = OfficeDocumentController::new();
        controller.lifecycle.activate(1).unwrap();
        let model = controller
            .open(
                "book.xlsx",
                zip_with(&[
                    ("xl/sharedStrings.xml", shared),
                    ("xl/worksheets/sheet1.xml", sheet),
                ]),
                1,
            )
            .unwrap();
        let OfficeDocumentModel::Sheet(sheet) = model else {
            panic!("expected sheet")
        };
        assert_eq!(sheet.rows[0], vec!["A", "42", "B"]);
        assert!(!sheet.truncated_rows);
    }

    #[test]
    fn pptx_extracts_ordered_slide_text() {
        let mut controller = OfficeDocumentController::new();
        controller.lifecycle.activate(1).unwrap();
        let bytes = zip_with(&[
            ("ppt/slides/slide2.xml", "<p:sld><a:t>Second</a:t></p:sld>"),
            ("ppt/slides/slide1.xml", "<p:sld><a:t>First</a:t></p:sld>"),
        ]);
        let model = controller.open("deck.pptx", bytes, 1).unwrap();
        let OfficeDocumentModel::Presentation(deck) = model else {
            panic!("expected presentation")
        };
        assert_eq!(deck.slides[0].text, vec!["First"]);
        assert_eq!(deck.slides[1].text, vec!["Second"]);
    }

    #[test]
    fn legacy_formats_return_explicit_unsupported_state() {
        let mut controller = OfficeDocumentController::new();
        controller.lifecycle.activate(1).unwrap();
        let model = controller.open("legacy.doc", vec![1, 2, 3], 1).unwrap();
        let OfficeDocumentModel::Unsupported(unsupported) = model else {
            panic!("expected unsupported")
        };
        assert_eq!(unsupported.reason_code, "office_legacy_format_unsupported");
        assert!(unsupported.system_open_available);
    }

    #[test]
    fn close_releases_the_parsed_model_and_resident_resource_metrics() {
        let bytes = zip_with(&[(
            "word/document.xml",
            "<w:document><w:p><w:r><w:t>close sentinel</w:t></w:r></w:p></w:document>",
        )]);
        let mut controller = OfficeDocumentController::new();
        controller.activate(1).unwrap();
        controller.open("sample.docx", bytes, 1).unwrap();
        assert!(controller.diagnostics().resources.resident_items > 0);
        assert!(controller.diagnostics().resources.resident_bytes > 0);

        controller.close(1).unwrap();

        assert!(controller.model().is_none());
        assert!(!controller.diagnostics().document_loaded);
        assert_eq!(controller.diagnostics().resources.resident_items, 0);
        assert_eq!(controller.diagnostics().resources.resident_bytes, 0);
    }

    #[test]
    fn unsafe_archive_paths_and_depth_are_rejected() {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut bytes);
            zip.start_file("../escape.xml", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        }
        let mut archive = open_office_archive(bytes.into_inner()).unwrap();
        let cancellation = OfficeCancellationToken::default();
        let guard = OfficeParseGuard::new(&cancellation, Duration::from_secs(1));
        assert_eq!(
            validate_archive(&mut archive, &guard).unwrap_err().code,
            "office_archive_path_invalid"
        );

        let deep = format!(
            "{}text{}",
            "<a>".repeat(OFFICE_XML_DEPTH_LIMIT + 1),
            "</a>".repeat(OFFICE_XML_DEPTH_LIMIT + 1)
        );
        assert_eq!(
            extract_text_nodes(&deep, &[b"a"], &guard).unwrap_err().code,
            "office_xml_depth_exceeded"
        );
        assert_eq!(
            extract_text_nodes(
                "<!DOCTYPE a [<!ENTITY x 'secret'>]><a>&x;</a>",
                &[b"a"],
                &guard,
            )
            .unwrap_err()
            .code,
            "office_xml_doctype_unsupported"
        );
        assert_eq!(
            extract_text_nodes("<a>&unknown;</a>", &[b"a"], &guard)
                .unwrap_err()
                .code,
            "office_xml_entity_unsupported"
        );
        assert_eq!(
            extract_text_nodes("<a>A&amp;B&#x4E16;</a>", &[b"a"], &guard)
                .unwrap()
                .join(""),
            "A&B世"
        );
    }

    #[test]
    fn cancellation_and_timeout_fail_closed_with_redacted_diagnostics() {
        let bytes = zip_with(&[(
            "word/document.xml",
            "<w:document><w:t>vibex-office-secret</w:t></w:document>",
        )]);
        let mut controller = OfficeDocumentController::new();
        controller.lifecycle.activate(1).unwrap();
        let cancellation = OfficeCancellationToken::default();
        cancellation.cancel();
        let cancelled = controller
            .open_with_control(
                "secret.docx",
                bytes.clone(),
                1,
                &cancellation,
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(cancelled.code, "office_parse_cancelled");
        assert_eq!(controller.lifecycle.phase(), ContentSurfacePhase::Error);
        assert_eq!(controller.diagnostics.cancelled_requests, 1);

        controller.lifecycle.activate(2).unwrap();
        let timeout = controller
            .open_with_control(
                "secret.docx",
                bytes,
                2,
                &OfficeCancellationToken::default(),
                Duration::ZERO,
            )
            .unwrap_err();
        assert_eq!(timeout.code, "office_parse_timeout");
        assert_eq!(controller.lifecycle.phase(), ContentSurfacePhase::Error);
        assert_eq!(controller.diagnostics.timed_out_requests, 1);

        let json = serde_json::to_string(controller.diagnostics()).unwrap();
        assert!(!json.contains("vibex-office-secret"));
        assert!(!json.contains("secret.docx"));
    }

    #[test]
    fn malformed_encoding_and_source_size_are_rejected_without_content_leakage() {
        let mut controller = OfficeDocumentController::new();
        controller.lifecycle.activate(1).unwrap();
        let malformed = controller
            .open(
                "malformed.docx",
                zip_with(&[("word/document.xml", "<w:document><w:t>secret")]),
                1,
            )
            .unwrap_err();
        assert_eq!(malformed.code, "office_xml_malformed");

        controller.lifecycle.activate(2).unwrap();
        let invalid_xml = [0xff, 0xfe, 0xfd];
        let invalid_encoding = controller
            .open(
                "encoding.docx",
                zip_with_bytes(&[("word/document.xml", &invalid_xml, CompressionMethod::Stored)]),
                2,
            )
            .unwrap_err();
        assert_eq!(invalid_encoding.code, "office_part_encoding_unsupported");

        controller.lifecycle.activate(3).unwrap();
        let invalid_shared_strings = controller
            .open(
                "encoding.xlsx",
                zip_with_bytes(&[
                    (
                        "xl/sharedStrings.xml",
                        &invalid_xml,
                        CompressionMethod::Stored,
                    ),
                    (
                        "xl/worksheets/sheet1.xml",
                        b"<worksheet><sheetData/></worksheet>",
                        CompressionMethod::Stored,
                    ),
                ]),
                3,
            )
            .unwrap_err();
        assert_eq!(
            invalid_shared_strings.code,
            "office_part_encoding_unsupported"
        );

        controller.lifecycle.activate(4).unwrap();
        let oversized = controller
            .open("oversized.docx", vec![0; OFFICE_MAX_DECODED_BYTES + 1], 4)
            .unwrap_err();
        assert_eq!(oversized.code, "office_source_size_invalid");
        assert_eq!(controller.lifecycle.phase(), ContentSurfacePhase::Error);
        let json = serde_json::to_string(controller.diagnostics()).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("malformed.docx"));
        assert!(!json.contains("encoding.docx"));
    }

    #[test]
    fn entry_count_entry_size_and_zip_bomb_limits_are_enforced() {
        let mut too_many = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut too_many);
            for index in 0..=OFFICE_MAX_ARCHIVE_ENTRIES {
                zip.start_file(
                    format!("word/part-{index}.xml"),
                    SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
                )
                .unwrap();
            }
            zip.finish().unwrap();
        }
        let mut controller = OfficeDocumentController::new();
        controller.lifecycle.activate(1).unwrap();
        assert_eq!(
            controller
                .open("many.docx", too_many.into_inner(), 1)
                .unwrap_err()
                .code,
            "office_archive_entry_count_invalid"
        );

        let oversized_part = vec![0; OFFICE_MAX_ENTRY_BYTES as usize + 1];
        controller.lifecycle.activate(2).unwrap();
        assert_eq!(
            controller
                .open(
                    "large.docx",
                    zip_with_bytes(&[(
                        "word/document.xml",
                        &oversized_part,
                        CompressionMethod::Deflated,
                    )]),
                    2,
                )
                .unwrap_err()
                .code,
            "office_archive_entry_too_large"
        );

        let compressible = vec![b'x'; 1024 * 1024];
        controller.lifecycle.activate(3).unwrap();
        assert_eq!(
            controller
                .open(
                    "bomb.docx",
                    zip_with_bytes(&[(
                        "word/document.xml",
                        &compressible,
                        CompressionMethod::Deflated,
                    )]),
                    3,
                )
                .unwrap_err()
                .code,
            "office_archive_zip_bomb"
        );
    }

    #[test]
    fn multibyte_text_limit_is_enforced_in_bytes_on_character_boundaries() {
        let text = "😀".repeat(OFFICE_TEXT_LIMIT / 4 + 32);
        let xml = format!("<w:document><w:t>{text}</w:t></w:document>");
        let mut controller = OfficeDocumentController::new();
        controller.lifecycle.activate(1).unwrap();
        let model = controller
            .open("emoji.docx", zip_with(&[("word/document.xml", &xml)]), 1)
            .unwrap();
        let OfficeDocumentModel::Text(document) = model else {
            panic!("expected text document")
        };
        assert_eq!(document.paragraphs.len(), 1);
        assert!(document.paragraphs[0].len() <= OFFICE_TEXT_LIMIT);
        assert!(document.paragraphs[0].is_char_boundary(document.paragraphs[0].len()));
    }
}
