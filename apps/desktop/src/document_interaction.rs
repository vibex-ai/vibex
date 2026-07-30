use std::{fs, path::PathBuf, time::Duration};

use gpui::{
    Context, Entity, FocusHandle, IntoElement, KeyDownEvent, Render, Subscription, Task,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, StyledExt as _, h_flex,
    input::{InputEvent, InputState},
    v_flex,
};
use serde::Serialize;

use crate::{office_surface::OfficeSurface, pdf_surface::PdfSurface};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentInteractionReport {
    schema_version: &'static str,
    status: &'static str,
    pdf: PdfObservation,
    office: OfficeObservation,
    privacy: PrivacyObservation,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PdfObservation {
    page_count: usize,
    current_page: usize,
    zoom_label: String,
    rendered_pages: usize,
    next_page_command_observed: bool,
    zoom_command_observed: bool,
    worker_active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OfficeObservation {
    initial_kind: &'static str,
    initial_visible_items: usize,
    initial_resident_items: usize,
    initial_resident_bytes: usize,
    system_open_available: bool,
    close_command_observed: bool,
    final_resident_items: usize,
    final_resident_bytes: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyObservation {
    document_paths_stored: bool,
    pdf_content_stored: bool,
    office_content_stored: bool,
}

pub struct DocumentInteractionWorkbench {
    pdf: Entity<PdfSurface>,
    office: Entity<OfficeSurface>,
    command: Entity<InputState>,
    focus: FocusHandle,
    output: PathBuf,
    progress: PathBuf,
    initial_office: Option<crate::office_surface::OfficePhysicalObservation>,
    next_page_command_observed: bool,
    zoom_command_observed: bool,
    close_command_observed: bool,
    note: String,
    poll_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl DocumentInteractionWorkbench {
    pub fn new(
        library: PathBuf,
        pdf_path: PathBuf,
        office_path: PathBuf,
        output: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let progress = output.with_extension("progress.json");
        let pdf = cx.new(|cx| PdfSurface::new(Ok(library), Some(pdf_path), None, window, cx));
        let office = cx.new(|cx| OfficeSurface::new(Some(office_path), cx));
        let command = cx.new(|cx| {
            InputState::new(window, cx)
                .submit_on_enter(true)
                .placeholder("Commands: pdf-next, pdf-zoom-in, office-close")
        });
        let mut this = Self {
            pdf,
            office,
            command,
            focus: cx.focus_handle(),
            output,
            progress,
            initial_office: None,
            next_page_command_observed: false,
            zoom_command_observed: false,
            close_command_observed: false,
            note: "Waiting for bounded PDF and Office models".into(),
            poll_task: None,
            _subscriptions: Vec::new(),
        };
        this._subscriptions.push(cx.subscribe_in(
            &this.command,
            window,
            |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    this.submit_command(window, cx);
                }
            },
        ));
        cx.on_next_frame(window, move |this, window, cx| {
            this.focus.focus(window, cx);
            this.write_progress(false);
            cx.notify();
        });
        this.poll_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let alive = entity
                    .update(cx, |this, cx| {
                        this.observe(cx);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        }));
        this
    }

    fn submit_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.command.read(cx).value().trim().to_string();
        self.command
            .update(cx, |input, cx| input.set_value("", window, cx));
        match command.as_str() {
            "pdf-next" => {
                self.next_page_command_observed = true;
                self.pdf.update(cx, |pdf, cx| pdf.physical_next_page(cx));
            }
            "pdf-zoom-in" => {
                self.zoom_command_observed = true;
                self.pdf.update(cx, |pdf, cx| pdf.physical_zoom_in(cx));
            }
            "office-close" => {
                self.close_command_observed = true;
                self.office
                    .update(cx, |office, cx| office.physical_close(cx));
            }
            _ => self.note = "Unknown command; use the listed bounded interaction commands".into(),
        }
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "n" => {
                self.next_page_command_observed = true;
                self.pdf.update(cx, |pdf, cx| pdf.physical_next_page(cx));
            }
            "z" => {
                self.zoom_command_observed = true;
                self.pdf.update(cx, |pdf, cx| pdf.physical_zoom_in(cx));
            }
            "c" => {
                self.close_command_observed = true;
                self.office
                    .update(cx, |office, cx| office.physical_close(cx));
            }
            _ => return,
        }
        cx.notify();
    }

    fn observe(&mut self, cx: &mut Context<Self>) {
        let pdf = self.pdf.read(cx).physical_observation();
        let office = self.office.read(cx).physical_observation();
        if self.initial_office.is_none() && office.ready {
            self.initial_office = Some(office.clone());
        }
        let ready = pdf.ready && self.initial_office.is_some();
        if ready {
            self.note = "Ready for physical keyboard interaction".into();
        }
        self.write_progress(ready);
        if self.next_page_command_observed
            && self.zoom_command_observed
            && self.close_command_observed
            && pdf.ready
            && pdf.current_page == 1
            && pdf.zoom_label == "125%"
            && office.closed
        {
            self.write_report(&pdf, &office);
            self.note = "PDF page/zoom and Office close interactions passed".into();
        }
    }

    fn write_progress(&self, ready: bool) {
        let value = serde_json::json!({
            "schemaVersion": "document-interaction-progress.v1",
            "ready": ready,
            "nextPageCommandObserved": self.next_page_command_observed,
            "zoomCommandObserved": self.zoom_command_observed,
            "officeCloseCommandObserved": self.close_command_observed,
        });
        if let Some(parent) = self.progress.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
            let _ = fs::write(&self.progress, bytes);
        }
    }

    fn write_report(
        &self,
        pdf: &crate::pdf_surface::PdfPhysicalObservation,
        office: &crate::office_surface::OfficePhysicalObservation,
    ) {
        let Some(initial) = self.initial_office.as_ref() else {
            return;
        };
        let report = DocumentInteractionReport {
            schema_version: "document-interaction-run.v1",
            status: "passed",
            pdf: PdfObservation {
                page_count: pdf.page_count,
                current_page: pdf.current_page,
                zoom_label: pdf.zoom_label.clone(),
                rendered_pages: pdf.rendered_pages,
                next_page_command_observed: self.next_page_command_observed,
                zoom_command_observed: self.zoom_command_observed,
                worker_active: pdf.worker_active,
            },
            office: OfficeObservation {
                initial_kind: initial.kind,
                initial_visible_items: initial.visible_items,
                initial_resident_items: initial.resident_items,
                initial_resident_bytes: initial.resident_bytes,
                system_open_available: initial.system_open_available,
                close_command_observed: self.close_command_observed,
                final_resident_items: office.resident_items,
                final_resident_bytes: office.resident_bytes,
            },
            privacy: PrivacyObservation {
                document_paths_stored: false,
                pdf_content_stored: false,
                office_content_stored: false,
            },
        };
        if let Some(parent) = self.output.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&report) {
            let _ = fs::write(&self.output, bytes);
        }
    }
}

impl Render for DocumentInteractionWorkbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("document-interaction-workbench")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .h(px(52.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .font_semibold()
                            .child("Physical PDF / Office Interaction"),
                    )
                    .child(div().text_sm().child(self.note.clone())),
            )
            .child(
                div()
                    .h(px(44.0))
                    .flex_none()
                    .px_3()
                    .py_1()
                    .child("Physical shortcuts: N = next page, Z = zoom in, C = close Office"),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w_3_5()
                            .h_full()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .child(self.pdf.clone()),
                    )
                    .child(div().flex_1().h_full().min_w_0().child(self.office.clone())),
            )
    }
}
