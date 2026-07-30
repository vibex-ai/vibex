use std::{fs, path::PathBuf, process::Command};

use gpui::{
    AnyElement, Context, IntoElement, Render, WeakEntity, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, button::Button, h_flex,
    scroll::ScrollableElement as _, v_flex,
};
use vibex_content::{
    OfficeDocumentController, OfficeDocumentModel, OfficeFileKind, OfficePresentationDocument,
    OfficeSheetDocument, OfficeTextDocument,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum OfficeSurfacePhase {
    Empty,
    Loading,
    Ready(OfficeDocumentModel),
    Error { code: String, message: String },
    Closed,
}

pub struct OfficeSurface {
    controller: OfficeDocumentController,
    document_path: Option<PathBuf>,
    generation: u64,
    phase: OfficeSurfacePhase,
    note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficePhysicalObservation {
    pub ready: bool,
    pub closed: bool,
    pub kind: &'static str,
    pub visible_items: usize,
    pub resident_items: usize,
    pub resident_bytes: usize,
    pub system_open_available: bool,
}

impl OfficeSurface {
    pub fn physical_observation(&self) -> OfficePhysicalObservation {
        let (ready, closed, kind, visible_items, system_open_available) = match &self.phase {
            OfficeSurfacePhase::Ready(OfficeDocumentModel::Text(document)) => {
                (true, false, "docx", document.paragraphs.len(), true)
            }
            OfficeSurfacePhase::Ready(OfficeDocumentModel::Sheet(document)) => (
                true,
                false,
                match document.kind {
                    OfficeFileKind::Ods => "ods",
                    _ => "xlsx",
                },
                document.rows.len(),
                true,
            ),
            OfficeSurfacePhase::Ready(OfficeDocumentModel::Presentation(document)) => {
                (true, false, "pptx", document.slides.len(), true)
            }
            OfficeSurfacePhase::Ready(OfficeDocumentModel::Unsupported(document)) => (
                true,
                false,
                "unsupported",
                0,
                document.system_open_available,
            ),
            OfficeSurfacePhase::Closed => (false, true, "closed", 0, false),
            _ => (false, false, "unavailable", 0, self.document_path.is_some()),
        };
        let resources = self.controller.diagnostics().resources;
        OfficePhysicalObservation {
            ready,
            closed,
            kind,
            visible_items,
            resident_items: resources.resident_items,
            resident_bytes: resources.resident_bytes,
            system_open_available,
        }
    }

    pub fn physical_close(&mut self, cx: &mut Context<Self>) {
        self.close_document(cx);
    }

    pub fn new(document_path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut surface = Self {
            controller: OfficeDocumentController::new(),
            document_path: None,
            generation: 0,
            phase: OfficeSurfacePhase::Empty,
            note: None,
        };
        if let Some(path) = document_path {
            surface.open_document(path, cx);
        }
        surface
    }

    fn open_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.generation = self.generation.saturating_add(1).max(1);
        let generation = self.generation;
        self.document_path = Some(path.clone());
        self.phase = OfficeSurfacePhase::Loading;
        let load = cx.background_spawn(async move { load_office_document(path, generation) });
        cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let (controller, phase) = load.await;
            let _ = entity.update(cx, |this, cx| {
                // A newer open (generation) or an explicit close/retry (phase)
                // supersedes this load; drop the stale result.
                if this.generation != generation
                    || !matches!(this.phase, OfficeSurfacePhase::Loading)
                {
                    return;
                }
                this.controller = controller;
                if matches!(phase, OfficeSurfacePhase::Ready(_)) {
                    this.note = None;
                }
                this.phase = phase;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.document_path.clone() {
            self.open_document(path, cx);
        }
    }

    fn close_document(&mut self, cx: &mut Context<Self>) {
        let _ = self.controller.close(self.generation);
        self.document_path = None;
        self.phase = OfficeSurfacePhase::Closed;
        self.note = Some("Office document closed and parsed content released".into());
        cx.notify();
    }

    fn open_in_system(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document_path.as_ref() else {
            return;
        };
        let result = if cfg!(target_os = "macos") {
            Command::new("open").arg(path).spawn()
        } else if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", "start", ""])
                .arg(path)
                .spawn()
        } else {
            Command::new("xdg-open").arg(path).spawn()
        };
        self.note = Some(match result {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                "System open requested".into()
            }
            Err(error) => format!("System open failed: {error}"),
        });
        cx.notify();
    }

    fn render_model(&self, model: &OfficeDocumentModel, cx: &mut Context<Self>) -> AnyElement {
        match model {
            OfficeDocumentModel::Text(document) => self.render_text(document, cx),
            OfficeDocumentModel::Sheet(sheet) => self.render_sheet(sheet, cx),
            OfficeDocumentModel::Presentation(deck) => self.render_presentation(deck, cx),
            OfficeDocumentModel::Unsupported(document) => v_flex()
                .gap_2()
                .p_4()
                .child(
                    div()
                        .font_semibold()
                        .child("Legacy Office format is not previewable"),
                )
                .child(div().text_sm().child(document.reason_code.clone()))
                .child(
                    div()
                        .text_sm()
                        .child("Use the explicit system-open action if needed."),
                )
                .into_any_element(),
        }
    }

    fn render_text(&self, document: &OfficeTextDocument, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_3()
            .p_4()
            .child(
                div().text_lg().font_semibold().child(
                    document
                        .title
                        .clone()
                        .unwrap_or_else(|| "DOCX document".into()),
                ),
            )
            .children(document.paragraphs.iter().cloned().map(|paragraph| {
                div()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(paragraph)
            }))
            .into_any_element()
    }

    fn render_sheet(&self, sheet: &OfficeSheetDocument, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .min_h_0()
            .gap_2()
            .p_3()
            .child(
                div()
                    .flex_none()
                    .font_semibold()
                    .child(format!("First sheet · {}", sheet.sheet_name)),
            )
            .child(
                uniform_list(
                    "office-sheet-rows",
                    sheet.rows.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        let OfficeSurfacePhase::Ready(OfficeDocumentModel::Sheet(sheet)) =
                            &this.phase
                        else {
                            return Vec::new();
                        };
                        range
                            .filter_map(|row_index| {
                                sheet.rows.get(row_index).map(|row| (row_index, row))
                            })
                            .map(|(row_index, row)| {
                                h_flex().h(px(30.0)).children(row.iter().enumerate().map(
                                    |(column_index, value)| {
                                        div()
                                            .w(px(132.0))
                                            .h_full()
                                            .flex_none()
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .px_2()
                                            .py_1()
                                            .text_xs()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .child(if value.is_empty() {
                                                format!("R{}C{}", row_index + 1, column_index + 1)
                                            } else {
                                                value.clone()
                                            })
                                    },
                                ))
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn render_presentation(
        &self,
        deck: &OfficePresentationDocument,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .gap_3()
            .p_4()
            .children(deck.slides.iter().map(|slide| {
                v_flex()
                    .gap_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .child(
                        div()
                            .font_semibold()
                            .child(format!("Slide {}", slide.slide_index + 1)),
                    )
                    .children(
                        slide
                            .text
                            .iter()
                            .cloned()
                            .map(|text| div().text_sm().child(text)),
                    )
            }))
            .into_any_element()
    }
}

impl Render for OfficeSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_open = self.document_path.is_some();
        let can_retry = matches!(self.phase, OfficeSurfacePhase::Error { .. });
        v_flex()
            .size_full()
            .bg(cx.theme().background)
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
                        div()
                            .font_semibold()
                            .child("Bounded read-only Office preview"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("office-retry")
                                    .label("Retry")
                                    .small()
                                    .disabled(!can_retry)
                                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                            )
                            .child(
                                Button::new("office-system-open")
                                    .label("Open in system")
                                    .small()
                                    .disabled(!can_open)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_in_system(cx)),
                                    ),
                            )
                            .child(
                                Button::new("office-close")
                                    .label("Close")
                                    .small()
                                    .disabled(!can_open)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.close_document(cx)),
                                    ),
                            ),
                    ),
            )
            .child({
                let body = match &self.phase {
                    OfficeSurfacePhase::Empty => div()
                        .p_4()
                        .child("No Office document selected")
                        .into_any_element(),
                    OfficeSurfacePhase::Loading => div()
                        .p_4()
                        .child("Loading Office document…")
                        .into_any_element(),
                    OfficeSurfacePhase::Ready(model) => self.render_model(model, cx),
                    OfficeSurfacePhase::Error { code, message } => v_flex()
                        .gap_2()
                        .p_4()
                        .child(div().font_semibold().child("Office preview failed"))
                        .child(div().text_sm().child(code.clone()))
                        .child(div().text_sm().child(message.clone()))
                        .into_any_element(),
                    OfficeSurfacePhase::Closed => div()
                        .p_4()
                        .child("Office preview closed")
                        .into_any_element(),
                };
                // Sheets scroll via their own uniform_list; wrapping them in the
                // outer scrollbar would defeat row virtualization.
                if matches!(
                    &self.phase,
                    OfficeSurfacePhase::Ready(OfficeDocumentModel::Sheet(_))
                ) {
                    v_flex().flex_1().min_h_0().child(body).into_any_element()
                } else {
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(body)
                        .into_any_element()
                }
            })
            .when_some(self.note.clone(), |this, note| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(note),
                )
            })
    }
}

/// Runs off the UI thread: reads and parses the document into a fresh
/// controller so large files never block rendering.
fn load_office_document(
    path: PathBuf,
    generation: u64,
) -> (OfficeDocumentController, OfficeSurfacePhase) {
    let mut controller = OfficeDocumentController::new();
    if let Err(error) = controller.activate(generation) {
        return (
            controller,
            OfficeSurfacePhase::Error {
                code: error.code,
                message: error.message,
            },
        );
    }
    let bytes = if OfficeFileKind::from_path(&path).supported() {
        match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return (
                    controller,
                    OfficeSurfacePhase::Error {
                        code: "office_source_read_failed".into(),
                        message: format!("Office document could not be read: {error}"),
                    },
                );
            }
        }
    } else {
        Vec::new()
    };
    let phase = match controller.open(&path, bytes, generation) {
        Ok(model) => OfficeSurfacePhase::Ready(model.clone()),
        Err(error) => OfficeSurfacePhase::Error {
            code: error.code,
            message: error.message,
        },
    };
    (controller, phase)
}
