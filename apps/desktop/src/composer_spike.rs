use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui::{
    ClipboardItem, Context, Entity, EntityInputHandler, ExternalPaths, Focusable as _, IntoElement,
    Render, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use serde::Serialize;

const DEFAULT_HOLD_MS: u64 = 3_000;
const CLIPBOARD_FIXTURE: &str = "second /fi fixture-paste";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComposerRunReport {
    schema_version: &'static str,
    status: &'static str,
    input: InputObservation,
    attachments: AttachmentObservation,
    suggestions: SuggestionObservation,
    focus: FocusObservation,
    failure: Option<ComposerFailure>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct InputObservation {
    native_input_handler: bool,
    multiline: bool,
    accessibility_role: &'static str,
    composition_observed: bool,
    marked_frame_count: u64,
    cjk_commit_observed: bool,
    shift_enter_observed: bool,
    enter_submit_observed: bool,
    paste_observed: bool,
    selection_observed: bool,
    undo_observed: bool,
    redo_observed: bool,
    final_text_bytes: u64,
    raw_text_stored: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentObservation {
    inline_image_token_rendered: bool,
    drop_adapter_fixture_accepted: bool,
    native_file_drop_event_observed: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SuggestionObservation {
    trigger_observed: bool,
    menu_rendered: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct FocusObservation {
    focus_event_count: u64,
    blur_event_count: u64,
    focused_after_submit: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComposerFailure {
    code: String,
}

pub struct ComposerSpikeView {
    input: Entity<InputState>,
    output: PathBuf,
    progress_output: PathBuf,
    phase: &'static str,
    composition_observed: bool,
    composition_active: bool,
    marked_frame_count: u64,
    cjk_commit_observed: bool,
    shift_enter_events: u64,
    submit_events: u64,
    paste_observed: bool,
    selection_observed: bool,
    full_selection_active: bool,
    undo_observed: bool,
    redo_observed: bool,
    suggestion_observed: bool,
    drop_adapter_fixture_accepted: bool,
    native_file_drop_event_observed: bool,
    focus_events: u64,
    blur_events: u64,
    report: Option<ComposerRunReport>,
    quit_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ComposerSpikeView {
    pub fn new(output: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let progress_output = output.with_extension("progress.json");
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(3, 8)
                .submit_on_enter(true)
                .placeholder("Message the Agent")
        });
        let subscriptions = vec![
            cx.observe(&input, |this, input, cx| {
                this.observe_input(&input, cx);
                cx.notify();
            }),
            cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                match event {
                    InputEvent::PressEnter { shift: true, .. } => {
                        this.shift_enter_events = this.shift_enter_events.saturating_add(1);
                    }
                    InputEvent::PressEnter { shift: false, .. } => {
                        this.submit_events = this.submit_events.saturating_add(1);
                        this.phase = "submitted";
                    }
                    InputEvent::Focus => {
                        this.focus_events = this.focus_events.saturating_add(1);
                    }
                    InputEvent::Blur => {
                        this.blur_events = this.blur_events.saturating_add(1);
                    }
                    InputEvent::Change => {}
                }
                cx.notify();
            }),
        ];

        let mut fixture_paths = ExternalPaths::default();
        fixture_paths.0.push(PathBuf::from("fixture-image.png"));
        let drop_adapter_fixture_accepted = accepts_image_drop(&fixture_paths);
        let input_to_focus = input.clone();
        cx.on_next_frame(window, move |this, window, cx| {
            input_to_focus.update(cx, |input, cx| input.focus(window, cx));
            this.phase = "ready";
            let _ = write_progress(&this.progress_output, true, false, false, false, 0);
            cx.notify();
        });

        Self {
            input,
            output,
            progress_output,
            phase: "starting",
            composition_observed: false,
            composition_active: false,
            marked_frame_count: 0,
            cjk_commit_observed: false,
            shift_enter_events: 0,
            submit_events: 0,
            paste_observed: false,
            selection_observed: false,
            full_selection_active: false,
            undo_observed: false,
            redo_observed: false,
            suggestion_observed: false,
            drop_adapter_fixture_accepted,
            native_file_drop_event_observed: false,
            focus_events: 0,
            blur_events: 0,
            report: None,
            quit_task: None,
            _subscriptions: subscriptions,
        }
    }

    fn observe_input(&mut self, input: &Entity<InputState>, cx: &mut Context<Self>) {
        let state = input.read(cx);
        let value = state.value().to_string();
        self.cjk_commit_observed |= value.chars().any(is_cjk);
        self.suggestion_observed |= value.contains("/fi");
        let selected_range = state.selected_range();
        self.full_selection_active =
            !value.is_empty() && selected_range.start == 0 && selected_range.end == value.len();
        let has_paste = value.contains("fixture-paste");
        if has_paste {
            if self.undo_observed {
                self.redo_observed = true;
            } else {
                self.paste_observed = true;
            }
        } else if self.paste_observed {
            self.undo_observed = true;
        }
        self.selection_observed |= self.redo_observed && self.full_selection_active;
        let _ = write_progress(
            &self.progress_output,
            true,
            self.composition_observed,
            self.composition_active,
            self.full_selection_active,
            value.len(),
        );
    }

    fn observe_composition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let marked = self.input.update(cx, |input, cx| {
            EntityInputHandler::marked_text_range(input, window, cx)
        });
        if marked.is_some() {
            let first_marked_frame = !self.composition_observed;
            let composition_started = !self.composition_active;
            self.composition_observed = true;
            self.composition_active = true;
            self.marked_frame_count = self.marked_frame_count.saturating_add(1);
            self.phase = "composing";
            if first_marked_frame || composition_started {
                let _ = write_progress(
                    &self.progress_output,
                    true,
                    true,
                    true,
                    self.full_selection_active,
                    self.input.read(cx).value().len(),
                );
            }
        } else if self.composition_observed && self.phase == "composing" {
            self.composition_active = false;
            self.phase = "committed";
            cx.write_to_clipboard(ClipboardItem::new_string(CLIPBOARD_FIXTURE.to_string()));
            let _ = write_progress(
                &self.progress_output,
                true,
                true,
                false,
                self.full_selection_active,
                self.input.read(cx).value().len(),
            );
        }
    }

    fn maybe_finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submit_events == 0 || self.report.is_some() {
            return;
        }
        let state = self.input.read(cx);
        let value = state.value().to_string();
        let focused_after_submit = state.focus_handle(cx).is_focused(window);
        let complete = self.composition_observed
            && self.marked_frame_count > 0
            && self.cjk_commit_observed
            && self.shift_enter_events > 0
            && value.contains('\n')
            && self.paste_observed
            && self.selection_observed
            && self.undo_observed
            && self.redo_observed
            && self.suggestion_observed
            && self.drop_adapter_fixture_accepted
            && focused_after_submit;
        let report = ComposerRunReport {
            schema_version: "composer-run.v1",
            status: if complete { "passed" } else { "failed" },
            input: InputObservation {
                native_input_handler: true,
                multiline: value.contains('\n'),
                accessibility_role: "multiline_text_input",
                composition_observed: self.composition_observed,
                marked_frame_count: self.marked_frame_count,
                cjk_commit_observed: self.cjk_commit_observed,
                shift_enter_observed: self.shift_enter_events > 0,
                enter_submit_observed: self.submit_events > 0,
                paste_observed: self.paste_observed,
                selection_observed: self.selection_observed,
                undo_observed: self.undo_observed,
                redo_observed: self.redo_observed,
                final_text_bytes: value.len() as u64,
                raw_text_stored: false,
            },
            attachments: AttachmentObservation {
                inline_image_token_rendered: true,
                drop_adapter_fixture_accepted: self.drop_adapter_fixture_accepted,
                native_file_drop_event_observed: self.native_file_drop_event_observed,
            },
            suggestions: SuggestionObservation {
                trigger_observed: self.suggestion_observed,
                menu_rendered: self.suggestion_observed,
            },
            focus: FocusObservation {
                focus_event_count: self.focus_events,
                blur_event_count: self.blur_events,
                focused_after_submit,
            },
            failure: (!complete).then(|| ComposerFailure {
                code: "composer_evidence_incomplete".to_string(),
            }),
            limitations: vec![
                "The image token uses a sanitized adapter fixture; no user file content is retained.",
                "Native Wayland file drag-and-drop remains unproven unless nativeFileDropEventObserved is true.",
            ],
        };
        self.phase = report.status;
        if write_report(&self.output, &report).is_err() {
            self.phase = "failed";
        }
        self.report = Some(report);
        self.quit_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(hold_ms()))
                .await;
            cx.update(|cx| cx.quit());
        }));
        cx.notify();
    }
}

impl Render for ComposerSpikeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.observe_composition(window, cx);
        self.maybe_finish(window, cx);
        let show_suggestion = self.suggestion_observed;
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .h(px(48.0))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .px_5()
                    .child(div().font_semibold().child("Composer native input"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.phase),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_end()
                    .p_8()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(820.0))
                            .gap_2()
                            .when(show_suggestion, |this| {
                                this.child(
                                    v_flex()
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .bg(cx.theme().popover)
                                        .p_2()
                                        .child(
                                            h_flex()
                                                .gap_3()
                                                .px_3()
                                                .py_2()
                                                .child(div().font_semibold().child("/files"))
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child("Attach workspace files"),
                                                ),
                                        ),
                                )
                            })
                            .child(
                                v_flex()
                                    .id("composer-drop-target")
                                    .gap_3()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().input)
                                    .p_3()
                                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                                        if accepts_image_drop(paths) {
                                            this.native_file_drop_event_observed = true;
                                            cx.notify();
                                        }
                                    }))
                                    .child(
                                        h_flex().gap_2().text_sm().child(
                                            div()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(cx.theme().border)
                                                .px_2()
                                                .py_1()
                                                .child("Image: fixture-image.png"),
                                        ),
                                    )
                                    .child(Input::new(&self.input).appearance(false).h_full())
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .items_center()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Rime / native clipboard"),
                                            )
                                            .child(
                                                Button::new("composer-send")
                                                    .primary()
                                                    .label("Send"),
                                            ),
                                    ),
                            ),
                    ),
            )
    }
}

fn accepts_image_drop(paths: &ExternalPaths) -> bool {
    paths.paths().iter().any(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                ["png", "jpg", "jpeg", "gif", "webp"]
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
            })
    })
}

fn is_cjk(character: char) -> bool {
    matches!(character as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

fn write_report(path: &Path, report: &ComposerRunReport) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    fs::write(path, bytes)
}

fn write_progress(
    path: &Path,
    ready: bool,
    composition_observed: bool,
    composition_active: bool,
    full_selection_active: bool,
    value_bytes: usize,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": "composer-progress.v1",
        "ready": ready,
        "compositionObserved": composition_observed,
        "compositionActive": composition_active,
        "fullSelectionActive": full_selection_active,
        "valueBytes": value_bytes,
        "rawTextStored": false
    }))
    .map_err(std::io::Error::other)?;
    fs::write(path, bytes)
}

fn hold_ms() -> u64 {
    std::env::var("VIBEX_SPIKE_HOLD_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value <= 10_000)
        .unwrap_or(DEFAULT_HOLD_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_drop_adapter_accepts_only_supported_extensions() {
        let mut accepted = ExternalPaths::default();
        accepted.0.push(PathBuf::from("fixture.PNG"));
        assert!(accepts_image_drop(&accepted));

        let mut rejected = ExternalPaths::default();
        rejected.0.push(PathBuf::from("fixture.txt"));
        assert!(!accepts_image_drop(&rejected));
    }

    #[test]
    fn cjk_detection_is_bounded_to_cjk_ranges() {
        assert!(is_cjk('\u{4f60}'));
        assert!(!is_cjk('n'));
    }
}
