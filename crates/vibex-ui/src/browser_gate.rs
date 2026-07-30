use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, Font, FontFallbacks,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Role, ScrollAnchor,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Placement, Root, Theme, ThemeMode, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};
use serde::{Deserialize, Serialize};

use crate::{
    CODE_TYPOGRAPHY, DARK_TOKENS, GpuiColorToken, INTERFACE_TYPOGRAPHY, LIGHT_TOKENS, RADII,
};

pub use crate::shell::{MEDIUM_MIN_WIDTH, ShellKind as ViewportClass, WIDE_MIN_WIDTH};
pub const BROWSER_GATE_SCHEMA_VERSION: &str = "vibex-browser-gate.v1";
pub const CJK_FALLBACK_FONT_FAMILY: &str = "WenQuanYi Micro Hei";

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeAreaInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardSource {
    Capacitor,
    VisualViewport,
    #[default]
    None,
}

impl KeyboardSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Capacitor => "capacitor",
            Self::VisualViewport => "visual_viewport",
            Self::None => "none",
        }
    }
}

impl SafeAreaInsets {
    fn normalized(self) -> Self {
        Self {
            top: normalized_dimension(self.top, 0.0, 256.0),
            right: normalized_dimension(self.right, 0.0, 256.0),
            bottom: normalized_dimension(self.bottom, 0.0, 256.0),
            left: normalized_dimension(self.left, 0.0, 256.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserHostSnapshot {
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub device_pixel_ratio: f32,
    pub visible: bool,
    pub focused: bool,
    pub dark_mode: bool,
    pub fullscreen: bool,
    pub keyboard_visible: bool,
    pub keyboard_inset: f32,
    pub keyboard_source: KeyboardSource,
    pub safe_area: SafeAreaInsets,
    pub storage_status: ProbeStatus,
    pub network_status: ProbeStatus,
    pub resume_count: u32,
    pub last_sequence: u64,
}

impl Default for BrowserHostSnapshot {
    fn default() -> Self {
        Self {
            viewport_width: 360.0,
            viewport_height: 800.0,
            device_pixel_ratio: 1.0,
            visible: true,
            focused: true,
            dark_mode: false,
            fullscreen: false,
            keyboard_visible: false,
            keyboard_inset: 0.0,
            keyboard_source: KeyboardSource::None,
            safe_area: SafeAreaInsets::default(),
            storage_status: ProbeStatus::Pending,
            network_status: ProbeStatus::Pending,
            resume_count: 0,
            last_sequence: 0,
        }
    }
}

impl BrowserHostSnapshot {
    pub fn viewport_class(&self) -> ViewportClass {
        ViewportClass::from_width(self.viewport_width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Pending,
    Passed,
    Failed,
    Unsupported,
}

impl ProbeStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserHostEventEnvelope {
    pub sequence: u64,
    #[serde(flatten)]
    pub event: BrowserHostEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserHostEvent {
    Viewport {
        width: f32,
        height: f32,
        device_pixel_ratio: f32,
        keyboard_visible: bool,
        keyboard_inset: f32,
        keyboard_source: KeyboardSource,
        safe_area: SafeAreaInsets,
    },
    Visibility {
        visible: bool,
    },
    Focus {
        focused: bool,
    },
    Appearance {
        dark_mode: bool,
    },
    Fullscreen {
        fullscreen: bool,
    },
    StorageProbe {
        status: ProbeStatus,
    },
    NetworkProbe {
        status: ProbeStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApplyHostEvent {
    Applied,
    IgnoredStale { last_sequence: u64 },
}

impl BrowserHostSnapshot {
    pub fn apply(&mut self, envelope: BrowserHostEventEnvelope) -> ApplyHostEvent {
        if envelope.sequence <= self.last_sequence {
            return ApplyHostEvent::IgnoredStale {
                last_sequence: self.last_sequence,
            };
        }

        let was_visible = self.visible;
        match envelope.event {
            BrowserHostEvent::Viewport {
                width,
                height,
                device_pixel_ratio,
                keyboard_visible,
                keyboard_inset,
                keyboard_source,
                safe_area,
            } => {
                self.viewport_width = normalized_dimension(width, 1.0, 16_384.0);
                self.viewport_height = normalized_dimension(height, 1.0, 16_384.0);
                self.device_pixel_ratio = normalized_dimension(device_pixel_ratio, 0.5, 8.0);
                let keyboard_inset = normalized_dimension(keyboard_inset, 0.0, 4_096.0)
                    .min(self.viewport_height * 0.9);
                self.keyboard_visible = keyboard_visible && keyboard_inset > 0.0;
                self.keyboard_inset = if self.keyboard_visible {
                    keyboard_inset
                } else {
                    0.0
                };
                self.keyboard_source = if self.keyboard_visible {
                    keyboard_source
                } else {
                    KeyboardSource::None
                };
                self.safe_area = safe_area.normalized();
            }
            BrowserHostEvent::Visibility { visible } => self.visible = visible,
            BrowserHostEvent::Focus { focused } => self.focused = focused,
            BrowserHostEvent::Appearance { dark_mode } => self.dark_mode = dark_mode,
            BrowserHostEvent::Fullscreen { fullscreen } => self.fullscreen = fullscreen,
            BrowserHostEvent::StorageProbe { status } => self.storage_status = status,
            BrowserHostEvent::NetworkProbe { status } => self.network_status = status,
        }
        if !was_visible && self.visible {
            self.resume_count = self.resume_count.saturating_add(1);
        }
        self.last_sequence = envelope.sequence;
        ApplyHostEvent::Applied
    }
}

fn normalized_dimension(value: f32, minimum: f32, maximum: f32) -> f32 {
    if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        minimum
    }
}

pub fn semantic_token(name: &str, dark: bool) -> Option<GpuiColorToken> {
    let tokens = if dark { DARK_TOKENS } else { LIGHT_TOKENS };
    tokens.iter().copied().find(|token| token.name == name)
}

pub fn semantic_color(name: &str, dark: bool) -> gpui::Hsla {
    let token = semantic_token(name, dark)
        .unwrap_or_else(|| panic!("missing generated GPUI semantic token: {name}"));
    gpui::Hsla {
        a: token.alpha,
        ..gpui::rgb(token.rgb).into()
    }
}

pub fn apply_browser_gate_theme(cx: &mut App) {
    Theme::sync_system_appearance(None, cx);
    apply_browser_gate_theme_tokens(cx);
}

pub fn apply_browser_gate_theme_mode(dark: bool, cx: &mut App) {
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    apply_browser_gate_theme_tokens(cx);
}

fn apply_browser_gate_theme_tokens(cx: &mut App) {
    let dark = Theme::global(cx).is_dark();
    let theme = Theme::global_mut(cx);
    theme.font_family = INTERFACE_TYPOGRAPHY.family.to_string().into();
    theme.font_size = px(INTERFACE_TYPOGRAPHY.size_px);
    theme.mono_font_family = "Lilex".into();
    theme.mono_font_size = px(CODE_TYPOGRAPHY.size_px);
    theme.radius = px(RADII.control_px);
    theme.radius_lg = px(RADII.large_px);
    theme.background = semantic_color("background", dark);
    theme.foreground = semantic_color("foreground", dark);
    theme.primary = semantic_color("primary", dark);
    theme.primary_foreground = semantic_color("primary-foreground", dark);
    theme.secondary = semantic_color("secondary", dark);
    theme.secondary_foreground = semantic_color("secondary-foreground", dark);
    theme.muted = semantic_color("muted", dark);
    theme.muted_foreground = semantic_color("muted-foreground", dark);
    theme.border = semantic_color("border", dark);
    theme.input = semantic_color("input", dark);
    theme.ring = semantic_color("ring", dark);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalDisposition {
    Pending,
    Approved,
    Denied,
}

pub struct BrowserGateView {
    host: BrowserHostSnapshot,
    composer: Entity<InputState>,
    page_scroll: ScrollHandle,
    composer_anchor: ScrollAnchor,
    scroll: ScrollHandle,
    approval: ApprovalDisposition,
    keyboard_reveal_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct DialogLayout {
    width: f32,
    max_height: f32,
    margin_top: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SheetLayout {
    placement: Placement,
    size: f32,
}

fn dialog_layout(host: &BrowserHostSnapshot) -> DialogLayout {
    let available_width =
        (host.viewport_width - host.safe_area.left - host.safe_area.right - 32.0).max(1.0);
    let available_height =
        (host.viewport_height - host.safe_area.top - host.safe_area.bottom - 24.0).max(128.0);
    DialogLayout {
        width: 448.0_f32.min(available_width),
        max_height: available_height,
        margin_top: (12.0 + host.safe_area.top).min((host.viewport_height - 64.0).max(0.0)),
    }
}

fn sheet_layout(host: &BrowserHostSnapshot) -> SheetLayout {
    if host.viewport_class() == ViewportClass::Compact {
        let available_height =
            (host.viewport_height - host.safe_area.top - host.safe_area.bottom - 16.0).max(1.0);
        SheetLayout {
            placement: Placement::Bottom,
            size: (host.viewport_height * 0.55)
                .clamp(200.0, 360.0)
                .min(available_height),
        }
    } else {
        SheetLayout {
            placement: Placement::Right,
            size: 360.0_f32.min((host.viewport_width - 32.0).max(1.0)),
        }
    }
}

impl BrowserGateView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Type here to verify composition and paste")
                .default_value("GPUI-WASM gate")
        });
        let page_scroll = ScrollHandle::new();
        Self {
            host: BrowserHostSnapshot::default(),
            composer,
            composer_anchor: ScrollAnchor::for_handle(page_scroll.clone()),
            page_scroll,
            scroll: ScrollHandle::new(),
            approval: ApprovalDisposition::Pending,
            keyboard_reveal_pending: false,
        }
    }

    pub fn apply_host_event(
        &mut self,
        envelope: BrowserHostEventEnvelope,
        cx: &mut Context<Self>,
    ) -> ApplyHostEvent {
        let was_keyboard_visible = self.host.keyboard_visible;
        let result = self.host.apply(envelope);
        if result == ApplyHostEvent::Applied {
            if !was_keyboard_visible && self.host.keyboard_visible {
                self.keyboard_reveal_pending = true;
            } else if !self.host.keyboard_visible {
                self.keyboard_reveal_pending = false;
            }
            cx.notify();
        }
        result
    }

    pub fn host_snapshot(&self) -> &BrowserHostSnapshot {
        &self.host
    }

    pub fn composer_value(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    pub fn approval_label(&self) -> &'static str {
        match self.approval {
            ApprovalDisposition::Pending => "pending",
            ApprovalDisposition::Approved => "approved",
            ApprovalDisposition::Denied => "denied",
        }
    }

    pub fn page_scroll_offset(&self) -> [f32; 2] {
        let offset = self.page_scroll.offset();
        [f32::from(offset.x), f32::from(offset.y)]
    }

    pub fn page_scroll_max_offset(&self) -> [f32; 2] {
        let offset = self.page_scroll.max_offset();
        [f32::from(offset.x), f32::from(offset.y)]
    }

    pub fn timeline_scroll_offset(&self) -> [f32; 2] {
        let offset = self.scroll.offset();
        [f32::from(offset.x), f32::from(offset.y)]
    }

    pub fn timeline_scroll_max_offset(&self) -> [f32; 2] {
        let offset = self.scroll.max_offset();
        [f32::from(offset.x), f32::from(offset.y)]
    }

    pub fn dialog_layout_metrics(&self) -> [f32; 3] {
        let layout = dialog_layout(&self.host);
        [layout.width, layout.max_height, layout.margin_top]
    }

    pub fn sheet_layout_metrics(&self) -> (&'static str, f32) {
        let layout = sheet_layout(&self.host);
        let placement = match layout.placement {
            Placement::Top => "top",
            Placement::Bottom => "bottom",
            Placement::Left => "left",
            Placement::Right => "right",
        };
        (placement, layout.size)
    }

    fn open_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = dialog_layout(&self.host);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .width(px(layout.width))
                .max_h(px(layout.max_height))
                .margin_top(px(layout.margin_top))
                .title("Permission details")
                .child("This GPUI dialog uses the shared desktop component layer.")
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new()
                                .child(Button::new("gate-dialog-cancel").outline().label("Cancel")),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("gate-dialog-confirm")
                                    .primary()
                                    .label("Confirm"),
                            ),
                        ),
                )
        });
    }

    fn open_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = sheet_layout(&self.host);
        window.open_sheet_at(layout.placement, cx, move |sheet, _, _| {
            sheet
                .title("Gate diagnostics")
                .size(px(layout.size))
                .resizable(layout.placement != Placement::Bottom)
                .child("Host lifecycle, safe-area, storage and network results are normalized before the View consumes them.")
                .footer(
                    Button::new("gate-sheet-close")
                        .outline()
                        .label("Close")
                        .on_click(|_, window, cx| window.close_sheet(cx)),
                )
        });
    }

    fn render_status_row(
        &self,
        label: &'static str,
        value: SharedString,
        compact: bool,
        cx: &App,
    ) -> impl IntoElement {
        let mut row = h_flex().justify_between().gap_3();
        if compact {
            row = row.flex_col().items_start().gap_0();
        }
        let mut value = div()
            .min_w_0()
            .font_family(cx.theme().mono_font_family.clone())
            .child(value);
        if compact {
            value = value.w_full().text_sm();
        }
        row.py_1()
            .child(div().text_color(cx.theme().muted_foreground).child(label))
            .child(value)
    }

    fn render_permission_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let disposition = match self.approval {
            ApprovalDisposition::Pending => "Needs approval",
            ApprovalDisposition::Approved => "Approved",
            ApprovalDisposition::Denied => "Denied",
        };
        v_flex()
            .id("gate-permission-card")
            .role(Role::Group)
            .aria_label("Permission request: run cargo test")
            .gap_3()
            .p_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(semantic_color("card", cx.theme().is_dark()))
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Run cargo test"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(disposition),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Agent requests permission to execute the workspace test suite."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("gate-deny")
                            .outline()
                            .label("Deny")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.approval = ApprovalDisposition::Denied;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("gate-approve")
                            .primary()
                            .label("Allow")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.approval = ApprovalDisposition::Approved;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn render_timeline(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = (0..48).map(|index| {
            let (title, detail) = match index % 4 {
                0 => (
                    "Agent",
                    "Checked the workspace contract and prepared a focused change.",
                ),
                1 => ("Tool", "cargo test -p vibex-ui --locked"),
                2 => ("Git", "2 files changed, no unrelated paths staged"),
                _ => ("System", "Remote timeline sequence is authoritative"),
            };
            h_flex()
                .id(("gate-timeline-row", index as usize))
                .role(Role::ListItem)
                .aria_label(SharedString::from(format!("{title}: {detail}")))
                .gap_3()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .w(px(56.0))
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                )
        });

        div()
            .id("gate-timeline")
            .role(Role::List)
            .aria_label("Agent timeline fixture")
            .relative()
            .h(px(260.0))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .vertical_scrollbar(&self.scroll)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .children(rows)
    }

    fn interface_font(&self, cx: &App) -> Font {
        Font {
            family: cx.theme().font_family.clone(),
            features: Default::default(),
            fallbacks: Some(FontFallbacks::from_fonts(vec![
                CJK_FALLBACK_FONT_FAMILY.to_string(),
            ])),
            weight: Default::default(),
            style: Default::default(),
        }
    }
}

impl Render for BrowserGateView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let interface_font = self.interface_font(cx);
        let viewport_width = f32::from(window.viewport_size().width);
        let viewport = ViewportClass::from_width(viewport_width);
        let compact = viewport == ViewportClass::Compact;
        if self.keyboard_reveal_pending
            && self.composer.read(cx).focus_handle(cx).is_focused(window)
        {
            self.composer_anchor.scroll_to(window, cx);
            self.keyboard_reveal_pending = false;
        }
        let status = v_flex()
            .gap_1()
            .child(self.render_status_row("Shell", viewport.label().into(), compact, cx))
            .child(
                self.render_status_row(
                    "Viewport",
                    format!(
                        "{:.0} x {:.0} @ {:.2}x",
                        self.host.viewport_width,
                        self.host.viewport_height,
                        self.host.device_pixel_ratio
                    )
                    .into(),
                    compact,
                    cx,
                ),
            )
            .child(
                self.render_status_row(
                    "Lifecycle",
                    format!(
                        "{} / {} / resumes {}",
                        if self.host.visible {
                            "visible"
                        } else {
                            "hidden"
                        },
                        if self.host.focused {
                            "focused"
                        } else {
                            "blurred"
                        },
                        self.host.resume_count
                    )
                    .into(),
                    compact,
                    cx,
                ),
            )
            .child(
                self.render_status_row(
                    "Storage / network",
                    format!(
                        "{} / {}",
                        self.host.storage_status.label(),
                        self.host.network_status.label()
                    )
                    .into(),
                    compact,
                    cx,
                ),
            )
            .child(
                self.render_status_row(
                    "Safe area / keyboard",
                    format!(
                        "{:.0},{:.0},{:.0},{:.0} / {:.0}px {}",
                        self.host.safe_area.top,
                        self.host.safe_area.right,
                        self.host.safe_area.bottom,
                        self.host.safe_area.left,
                        self.host.keyboard_inset,
                        self.host.keyboard_source.label()
                    )
                    .into(),
                    compact,
                    cx,
                ),
            );

        let controls = v_flex()
            .gap_3()
            .child(
                div()
                    .id("gate-input-heading")
                    .role(Role::Heading)
                    .aria_level(2)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Input and composition"),
            )
            .child(
                div()
                    .id("gate-composer-anchor")
                    .anchor_scroll(Some(self.composer_anchor.clone()))
                    .child(
                        Input::new(&self.composer)
                            .font(interface_font.clone())
                            .w_full(),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("gate-open-dialog")
                            .outline()
                            .label("Open dialog")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_dialog(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("gate-open-sheet")
                            .outline()
                            .label("Open sheet")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_sheet(window, cx)),
                            ),
                    ),
            )
            .child(self.render_permission_card(cx));

        let mut header = h_flex().justify_between().gap_3();
        if compact {
            header = header.flex_col().items_start().gap_2();
        }
        let header = header
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .id("gate-title")
                            .role(Role::Heading)
                            .aria_level(1)
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Vibex"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("GPUI-WASM cross-platform gate"),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .child(viewport.label()),
            );

        let main = v_flex()
            .id("vibex-browser-gate")
            .role(Role::Application)
            .aria_label("Vibex browser gate")
            .relative()
            .flex_1()
            .w_full()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&self.page_scroll)
            .vertical_scrollbar(&self.page_scroll)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .p_4()
            .pt(px(16.0 + self.host.safe_area.top))
            .pr(px(16.0 + self.host.safe_area.right))
            .pb(px(16.0
                + self.host.safe_area.bottom
                + self.host.keyboard_inset.min(320.0)))
            .pl(px(16.0 + self.host.safe_area.left))
            .gap_4()
            .child(header)
            .child(
                div()
                    .grid()
                    .grid_cols(if compact { 1 } else { 2 })
                    .gap_4()
                    .child(v_flex().gap_4().min_w_0().child(status).child(controls))
                    .child(
                        v_flex()
                            .gap_3()
                            .min_w_0()
                            .child(
                                div()
                                    .id("gate-timeline-heading")
                                    .role(Role::Heading)
                                    .aria_level(2)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Authoritative timeline"),
                            )
                            .child(self.render_timeline(cx)),
                    ),
            );

        v_flex()
            .size_full()
            .font(interface_font)
            .child(main)
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport_event(sequence: u64, width: f32) -> BrowserHostEventEnvelope {
        BrowserHostEventEnvelope {
            sequence,
            event: BrowserHostEvent::Viewport {
                width,
                height: 800.0,
                device_pixel_ratio: 2.0,
                keyboard_visible: false,
                keyboard_inset: 0.0,
                keyboard_source: KeyboardSource::None,
                safe_area: SafeAreaInsets::default(),
            },
        }
    }

    #[test]
    fn breakpoints_match_the_gate_contract() {
        assert_eq!(ViewportClass::from_width(1_100.0), ViewportClass::Wide);
        assert_eq!(ViewportClass::from_width(1_099.0), ViewportClass::Medium);
        assert_eq!(ViewportClass::from_width(760.0), ViewportClass::Medium);
        assert_eq!(ViewportClass::from_width(759.0), ViewportClass::Compact);
        assert_eq!(ViewportClass::from_width(360.0), ViewportClass::Compact);
    }

    #[test]
    fn host_events_are_ordered_and_resume_is_counted_once() {
        let mut snapshot = BrowserHostSnapshot::default();
        assert_eq!(
            snapshot.apply(viewport_event(1, 1_200.0)),
            ApplyHostEvent::Applied
        );
        assert_eq!(snapshot.viewport_class(), ViewportClass::Wide);
        assert_eq!(
            snapshot.apply(BrowserHostEventEnvelope {
                sequence: 2,
                event: BrowserHostEvent::Visibility { visible: false },
            }),
            ApplyHostEvent::Applied
        );
        assert_eq!(
            snapshot.apply(BrowserHostEventEnvelope {
                sequence: 3,
                event: BrowserHostEvent::Visibility { visible: true },
            }),
            ApplyHostEvent::Applied
        );
        assert_eq!(snapshot.resume_count, 1);
        assert_eq!(
            snapshot.apply(viewport_event(2, 360.0)),
            ApplyHostEvent::IgnoredStale { last_sequence: 3 }
        );
        assert_eq!(snapshot.viewport_class(), ViewportClass::Wide);
    }

    #[test]
    fn host_dimensions_are_finite_and_bounded() {
        let mut snapshot = BrowserHostSnapshot::default();
        snapshot.apply(BrowserHostEventEnvelope {
            sequence: 1,
            event: BrowserHostEvent::Viewport {
                width: f32::NAN,
                height: f32::INFINITY,
                device_pixel_ratio: 99.0,
                keyboard_visible: true,
                keyboard_inset: -20.0,
                keyboard_source: KeyboardSource::Capacitor,
                safe_area: SafeAreaInsets {
                    top: 999.0,
                    right: f32::NAN,
                    bottom: 24.0,
                    left: -1.0,
                },
            },
        });
        assert_eq!(snapshot.viewport_width, 1.0);
        assert_eq!(snapshot.viewport_height, 1.0);
        assert_eq!(snapshot.device_pixel_ratio, 8.0);
        assert_eq!(snapshot.keyboard_inset, 0.0);
        assert!(!snapshot.keyboard_visible);
        assert_eq!(snapshot.keyboard_source, KeyboardSource::None);
        assert_eq!(snapshot.safe_area.top, 256.0);
        assert_eq!(snapshot.safe_area.right, 0.0);
        assert_eq!(snapshot.safe_area.bottom, 24.0);
        assert_eq!(snapshot.safe_area.left, 0.0);
    }

    #[test]
    fn keyboard_inset_and_source_are_bounded_by_the_visible_viewport() {
        let mut snapshot = BrowserHostSnapshot::default();
        snapshot.apply(BrowserHostEventEnvelope {
            sequence: 1,
            event: BrowserHostEvent::Viewport {
                width: 360.0,
                height: 490.0,
                device_pixel_ratio: 4.0,
                keyboard_visible: true,
                keyboard_inset: 900.0,
                keyboard_source: KeyboardSource::Capacitor,
                safe_area: SafeAreaInsets::default(),
            },
        });
        assert_eq!(snapshot.keyboard_inset, 441.0);
        assert!(snapshot.keyboard_visible);
        assert_eq!(snapshot.keyboard_source, KeyboardSource::Capacitor);
    }

    #[test]
    fn compact_overlays_stay_inside_the_visible_viewport() {
        let host = BrowserHostSnapshot {
            viewport_width: 360.0,
            viewport_height: 490.0,
            safe_area: SafeAreaInsets {
                top: 24.0,
                bottom: 16.0,
                ..SafeAreaInsets::default()
            },
            ..BrowserHostSnapshot::default()
        };
        assert_eq!(dialog_layout(&host).width, 328.0);
        assert_eq!(dialog_layout(&host).max_height, 426.0);
        assert_eq!(sheet_layout(&host).placement, Placement::Bottom);
        assert_eq!(sheet_layout(&host).size, 269.5);
    }

    #[test]
    fn generated_tokens_drive_the_fixture_palette() {
        assert_eq!(semantic_token("background", false).unwrap().hex, "#ffffff");
        assert_eq!(semantic_token("background", true).unwrap().hex, "#09090b");
        assert_eq!(crate::BORDERS.default_px, 1.0);
        assert_eq!(BROWSER_GATE_SCHEMA_VERSION, "vibex-browser-gate.v1");
    }
}
