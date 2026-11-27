//! Native `nsh` terminal grid component backed by [`Emulator`] (`alacritty_terminal`).
//!
//! Designed specifically for mobile touch + IME ergonomics:
//! - Tapping anywhere on text OR empty cells focuses the single external IME
//!   input (`on_tap`) without blocking vertical scrollback or long-press text
//!   selection/copy (`selectable(true)` + `keyboard_taps(KeyboardTaps::Always)`).
//! - Ignores degenerate (`0x0` / `< 2x2`) hidden layout bounds so switching
//!   Workbench tabs or swiping pages never resizes or corrupts the screen.
//! - Disables platform `allow_font_scaling` on terminal cells so `measure_text`
//!   cell width matches rendered glyph advances 1:1 even when Android system
//!   font scaling is > 1.0.
//! - Isolates wide characters, Nerd Font PUA icons, box-drawing symbols, and
//!   fallback glyphs whose measured advance differs from `cols * cell_width`
//!   into exact cell-positioned segments so subsequent columns never drift.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::Duration,
};

use alacritty_terminal::vte::ansi::Rgb;
use nsh_core::{
    color::Rgba,
    element::Style,
    prelude::*,
    time::{Instant, now},
};

use super::emulator::{
    BgSpan, CellColor, CellStyle, DEFAULT_SCROLLBACK, Emulator, EmulatorEvent, EmulatorModes,
    RowCell,
};
use crate::theme;

/// How long a subsequent resize (e.g. animated keyboard height change) settles
/// before resizing the emulator and notifying the PTY.
const RESIZE_SETTLE: Duration = Duration::from_millis(60);

thread_local! {
    /// Cached glyph advances per `(css_font, grapheme)` for the engine thread.
    static ADVANCES: RefCell<HashMap<(String, String), f32>> = RefCell::new(HashMap::new());
    /// Live font revision signals for mounted terminal grids, bumped when
    /// bundled fonts finish registering asynchronously at startup.
    static FONT_REVISIONS: RefCell<Vec<Signal<u64>>> = const { RefCell::new(Vec::new()) };
}

/// Clears cached glyph advances and prompts any mounted terminal grid to
/// re-measure its cell width (called after bundled fonts register at startup).
pub fn notify_terminal_fonts_ready() {
    ADVANCES.with_borrow_mut(HashMap::clear);
    FONT_REVISIONS.with_borrow(|signals| {
        for &rev in signals {
            rev.update(|v| *v = v.wrapping_add(1));
        }
    });
}

/// Props of [`Terminal`].
#[derive(Clone, Copy, PartialEq, Props)]
pub struct TerminalProps {
    /// Scrollback history lines kept above the active viewport.
    #[prop(default = DEFAULT_SCROLLBACK as u32)]
    pub scrollback: Prop<u32>,
    /// Monospace font family (or CSS fallback list).
    pub font_family: Prop<Option<SharedString>>,
    /// Cell font size in points.
    #[prop(default = Points(13.0))]
    pub font_size: Prop<Points>,
    /// Row height in points.
    #[prop(default = Points(18.0))]
    pub line_height: Prop<Points>,
    /// Screen-reader label for the terminal grid.
    pub accessibility_label: Prop<Option<SharedString>>,
    /// Style of the block cursor drawn at the active cell.
    #[prop(default)]
    pub cursor_style: Prop<Style>,
    /// Live IME composition text (underlined at the cursor cell while composing).
    #[prop(default)]
    pub composing: Prop<Option<SharedString>>,
    /// Active Find-in-terminal query (case-insensitive highlight on matching cells).
    #[prop(default)]
    pub find_query: Prop<Option<SharedString>>,
}

/// Events emitted by [`Terminal`].
#[derive(Debug, Clone, PartialEq, Event)]
pub enum TerminalEvent {
    /// Emulator-generated replies for the PTY (e.g. DSR cursor position or OSC
    /// color queries).
    #[args(data)]
    Data(SharedString),
    /// The viewport's cell dimensions changed to a valid (`>= 2x2`) grid.
    #[args(cols, rows)]
    Resize(u32, u32),
    /// The program updated the terminal window title.
    #[args(title)]
    TitleChange(SharedString),
    /// The user tapped the terminal grid (text or empty cell) to focus input.
    Tap,
    /// The program rang the bell.
    Bell,
}

/// Imperative API exposed via `ComponentRef<Terminal>`.
pub struct TerminalApi {
    state: TermState,
}

impl TerminalApi {
    /// Feeds raw PTY bytes directly into `alacritty_terminal`'s VTE parser so
    /// multi-byte UTF-8 characters or escape sequences split across frames
    /// never corrupt.
    pub fn write_bytes(&self, data: &[u8]) {
        self.state.write(data);
    }

    /// Current terminal input/screen modes (`APP_CURSOR`, `BRACKETED_PASTE`, etc.).
    pub fn modes(&self) -> EmulatorModes {
        self.state.emu.borrow().modes()
    }
}

#[methods]
impl TerminalApi {
    /// Writes UTF-8 program output into the terminal emulator.
    pub fn write(&self, data: String) {
        self.state.write(data.as_bytes());
    }

    /// Resets the terminal screen and clears scrollback history.
    pub fn reset(&self) {
        self.state.reset();
    }

    /// Plain text across scrollback and viewport, one line per row.
    pub fn text(&self) -> String {
        self.state.emu.borrow().plain_text()
    }

    /// Requests keyboard focus via the terminal's tap handler.
    pub fn focus(&self) {
        self.state.cx.emit(TerminalEvent::Tap);
    }
}

type PendingResize = ((u16, u16), Instant);

#[derive(Clone)]
struct TermState {
    emu: Rc<RefCell<Emulator>>,
    /// Last applied valid layout size (`None` before first non-empty layout).
    size: Rc<Cell<Option<(u16, u16)>>>,
    /// Debounced pending resize target and timestamp.
    pending: Rc<Cell<Option<PendingResize>>>,
    rev: Signal<u64>,
    cx: Scope<TerminalEvent>,
}

impl TermState {
    fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let events = self.emu.borrow_mut().write(bytes);
        self.rev.update(|r| *r = r.wrapping_add(1));
        for event in events {
            match event {
                EmulatorEvent::Title(title) => {
                    self.cx.emit(TerminalEvent::TitleChange(title.into()));
                }
                EmulatorEvent::ResetTitle => {
                    self.cx
                        .emit(TerminalEvent::TitleChange(SharedString::new_static("")));
                }
                EmulatorEvent::PtyWrite(data) => {
                    self.cx.emit(TerminalEvent::Data(data.into()));
                }
                EmulatorEvent::ColorRequest(index, formatter) => {
                    if let Some(rgb) = color_for_query(index) {
                        let reply = formatter(rgb);
                        if !reply.is_empty() {
                            self.cx.emit(TerminalEvent::Data(reply.into()));
                        }
                    }
                }
                EmulatorEvent::Bell => {
                    self.cx.emit(TerminalEvent::Bell);
                }
            }
        }
    }

    fn reset(&self) {
        self.emu.borrow_mut().reset();
        self.rev.update(|r| *r = r.wrapping_add(1));
    }

    fn request_resize(&self, cols: u16, rows: u16) {
        if cols < 2 || rows < 2 {
            return;
        }
        let next = (cols, rows);
        if self.size.get() == Some(next) {
            self.pending.set(None);
            return;
        }
        // Apply the first real layout immediately so the initial shell output
        // and PTY size match the phone screen right away.
        if self.size.get().is_none() {
            self.apply_resize(next);
            return;
        }
        let at = now();
        self.pending.set(Some((next, at)));
        let state = self.clone();
        set_timeout(RESIZE_SETTLE, move || {
            if state.pending.get() == Some((next, at)) {
                state.pending.set(None);
                state.apply_resize(next);
            }
        });
    }

    fn cancel_pending_resize(&self) {
        self.pending.set(None);
    }

    fn apply_resize(&self, size: (u16, u16)) {
        self.size.set(Some(size));
        if self.emu.borrow_mut().resize(size.0, size.1) {
            self.rev.update(|r| *r = r.wrapping_add(1));
        }
        self.cx
            .emit(TerminalEvent::Resize(u32::from(size.0), u32::from(size.1)));
    }
}

/// Mobile terminal grid component.
#[component]
pub fn Terminal(props: TerminalProps, cx: Scope<TerminalEvent>) -> impl IntoElement {
    let scrollback = props.scrollback.get_untracked() as usize;
    let state = TermState {
        emu: Rc::new(RefCell::new(Emulator::new(80, 24, scrollback))),
        size: Rc::new(Cell::new(None)),
        pending: Rc::new(Cell::new(None)),
        rev: signal(0),
        cx,
    };
    cx.methods(TerminalApi {
        state: state.clone(),
    });

    let font_rev = signal(0u64);
    FONT_REVISIONS.with_borrow_mut(|list| list.push(font_rev));
    cleanup(move || {
        FONT_REVISIONS.with_borrow_mut(|list| list.retain(|s| *s != font_rev));
    });

    let family = move || {
        props
            .font_family
            .get()
            .unwrap_or_else(|| SharedString::from(theme::TERMINAL_FONT_FAMILY))
    };
    let font = computed(move || {
        font_rev.get();
        font_css(&family(), props.font_size.get())
    });
    let cell_width = computed(move || {
        font_rev.get();
        let css = font_css(&family(), props.font_size.get());
        let measured = measure_text(&css, "0000000000").0 / 10.0;
        Points(if measured.is_finite() && measured > 0.5 {
            measured
        } else {
            (props.font_size.get().0 * 0.6).max(4.0)
        })
    });

    let count = {
        let state = state.clone();
        move || {
            state.rev.get();
            state.emu.borrow().line_count()
        }
    };

    let on_tap = Rc::new(move || cx.emit(TerminalEvent::Tap));
    let row_state = state.clone();
    let row_tap = on_tap.clone();
    let lh_init = props.line_height.get_untracked();
    let rows = virtual_list(count, move |at| {
        render_row(
            row_state.clone(),
            props,
            font,
            cell_width,
            at,
            row_tap.clone(),
        )
    })
    .alignment(ListAlignment::Bottom)
    .follow_end(px(lh_init.0 * 2.0))
    .estimated_item_size(lh_init)
    .selectable(true)
    .keyboard_taps(KeyboardTaps::Always);

    let layout_state = state.clone();
    let outer_tap = on_tap.clone();
    view()
        .overflow_hidden()
        .font_family(family)
        .text_size(move || Length::from(props.font_size.get()))
        .text_color(theme::foreground)
        .label(move || props.accessibility_label.get().unwrap_or_default())
        .focusable(false)
        .on_press(move || outer_tap())
        .child(
            rows.flex_1()
                .min_h(px(0.0))
                .on_layout(move |bounds: &Bounds<Points>| {
                    let cw = cell_width.get_untracked().0;
                    let lh = props.line_height.get_untracked().0;
                    // Ignore hidden / collapsed (`Display::None` -> `0x0`) frames
                    // so switching Workbench tabs or swiping pages never resizes
                    // the emulator to `1x1` and truncates screen content.
                    if cw <= 0.0
                        || lh <= 0.0
                        || bounds.size.width.0 < cw * 2.0
                        || bounds.size.height.0 < lh * 2.0
                    {
                        layout_state.cancel_pending_resize();
                        return;
                    }
                    let cols = (bounds.size.width.0 / cw).floor().clamp(2.0, 500.0) as u16;
                    let rows = (bounds.size.height.0 / lh).floor().clamp(2.0, 500.0) as u16;
                    layout_state.request_resize(cols, rows);
                }),
        )
}

fn render_row(
    state: TermState,
    props: TerminalProps,
    font: Memo<String>,
    cell_width: Memo<Points>,
    at: Position,
    on_tap: Rc<dyn Fn()>,
) -> impl IntoElement {
    let row = {
        let state = state.clone();
        computed(move || {
            state.rev.get();
            state.emu.borrow().row(at.index())
        })
    };

    let caret_col = {
        let state = state.clone();
        computed(move || {
            state.rev.get();
            let (cur_row, cur_col) = state.emu.borrow().cursor_position();
            (at.index() == cur_row).then_some(cur_col)
        })
    };

    let composing_here = computed(move || {
        caret_col.get()?;
        props.composing.get().filter(|text| !text.is_empty())
    });

    let cursor_col = {
        let state = state.clone();
        computed(move || {
            if props.composing.get().is_some_and(|t| !t.is_empty()) {
                return None;
            }
            state.rev.get();
            let (cur_row, cur_col) = state.emu.borrow().cursor()?;
            (at.index() == cur_row).then_some(cur_col)
        })
    };

    let row_tap = on_tap.clone();
    let text_tap = on_tap.clone();

    view()
        .w_full()
        .h(move || Length::from(props.line_height.get()))
        .justify_center()
        // Tapping any blank area or empty cell on the row focuses the IME.
        .on_press(move || row_tap())
        // Layer 1: Cell background boxes (fills full row height).
        .child(dynamic(move || {
            let cw = cell_width.get().0;
            let is_dark = dark();
            let snapshot = row.get();
            let mut boxes: Vec<AnyElement> = Vec::with_capacity(snapshot.backgrounds.len());
            for span in &snapshot.backgrounds {
                if let Some(bg) = resolve_bg_span(span, is_dark) {
                    boxes.push(
                        view()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(cw * span.col as f32))
                            .w(px(cw * span.cols as f32))
                            .bg(bg)
                            .into_element(),
                    );
                }
            }
            // Find-in-terminal match highlights.
            if let Some(query) = props.find_query.get() {
                let needle = query.trim().to_lowercase();
                if !needle.is_empty() && !snapshot.plain.is_empty() {
                    for (start_col, span_cols) in find_match_columns(&snapshot.cells, &needle) {
                        boxes.push(
                            view()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(cw * start_col as f32))
                                .w(px(cw * span_cols as f32))
                                .bg(theme::accent_yellow().alpha(if is_dark { 0.36 } else { 0.42 }))
                                .rounded(px(2.0))
                                .into_element(),
                        );
                    }
                }
            }
            view()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right_0()
                .pointer_events(PointerEvents::None)
                .children(boxes)
        }))
        // Layer 2: Cell-aligned text segments.
        .child(dynamic(move || {
            let cw = cell_width.get().0;
            let font_name = font.get();
            let is_dark = dark();
            let snapshot = row.get();
            let segments = build_segments(&snapshot.cells, cw, |g| glyph_advance(&font_name, g));
            if segments.is_empty() {
                let tap = text_tap.clone();
                return text("")
                    .allow_font_scaling(false)
                    .select_text()
                    .on_press(move || tap())
                    .into_element();
            }
            view()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right_0()
                .justify_center()
                .children(
                    segments
                        .into_iter()
                        .enumerate()
                        .map(|(i, segment)| {
                            let tap = text_tap.clone();
                            text("")
                                .allow_font_scaling(false)
                                .select_text()
                                .when(i > 0, |t| t.continues_text(true))
                                .absolute()
                                .left(px(segment.x))
                                .when(segment.spacing != 0.0, |t| {
                                    t.letter_spacing(px(segment.spacing))
                                })
                                // On Android (`TextBox.forwardPress`), a press listener on
                                // selectable text fires on taps while long-press cancels it
                                // and hands off to `SelectableView` for word selection/copy.
                                .on_press(move || tap())
                                .children(
                                    segment
                                        .runs
                                        .into_iter()
                                        .map(|run| render_run(run, is_dark))
                                        .collect::<Vec<_>>(),
                                )
                                .into_element()
                        })
                        .collect::<Vec<_>>(),
                )
                .into_element()
        }))
        // Layer 3: Live IME composition preview at the cursor cell.
        .child(when(
            move || composing_here.with(Option::is_some),
            move || {
                view()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(move || {
                        Length::from(cell_width.get() * caret_col.get().unwrap_or(0) as f32)
                    })
                    .justify_center()
                    .bg(theme::terminal_surface)
                    .pointer_events(PointerEvents::None)
                    .child(
                        text(move || composing_here.get().unwrap_or_default())
                            .allow_font_scaling(false)
                            .underline()
                            .text_color(theme::foreground),
                    )
            },
        ))
        // Layer 4: Cursor block.
        .child(when(
            move || cursor_col.with(Option::is_some),
            move || {
                view()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(move || {
                        Length::from(cell_width.get() * cursor_col.get().unwrap_or(0) as f32)
                    })
                    .w(move || Length::from(cell_width.get()))
                    .pointer_events(PointerEvents::None)
                    .inline_style(props.cursor_style)
            },
        ))
}

#[derive(Debug, Clone, PartialEq)]
struct StyledRun {
    text: String,
    style: CellStyle,
}

#[derive(Debug, Clone, PartialEq)]
struct TextSegment {
    x: f32,
    spacing: f32,
    runs: Vec<StyledRun>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SegmentKind {
    /// Normal single-column monospace cells (exact `cw` advance).
    Inline,
    /// Wide cells (e.g. CJK) sharing an identical advance, spaced to `2 * cw`.
    Wide(u32),
    /// Standalone cell-positioned glyph (PUA / Nerd Font / emoji / fallback
    /// whose advance differs from `width * cw`).
    Pinned,
}

/// Groups a row's cells into visually positioned segments so every run lands
/// on its exact grid column even when fallback fonts, Nerd Font PUA icons,
/// box-drawing glyphs, or wide CJK/emoji characters have non-grid advances.
fn build_segments(
    cells: &[RowCell],
    cw: f32,
    mut advance: impl FnMut(&str) -> f32,
) -> Vec<TextSegment> {
    let mut out: Vec<(SegmentKind, usize, TextSegment)> = Vec::new();
    for cell in cells {
        let cell_span = cell.width.max(1);
        let (kind, x, spacing) = classify_cell(cell, cw, &mut advance);

        let can_merge = if let Some((prev_kind, prev_end_col, _)) = out.last() {
            *prev_end_col == cell.col
                && matches!(
                    (*prev_kind, kind),
                    (SegmentKind::Inline, SegmentKind::Inline)
                )
                || (*prev_end_col == cell.col
                    && matches!((*prev_kind, kind), (SegmentKind::Wide(a), SegmentKind::Wide(b)) if a == b))
        } else {
            false
        };

        if can_merge {
            let (_, end_col, seg) = out.last_mut().expect("checked");
            *end_col = cell.col + cell_span;
            if let Some(last_run) = seg.runs.last_mut()
                && last_run.style == cell.style
            {
                last_run.text.push_str(&cell.text);
            } else {
                seg.runs.push(StyledRun {
                    text: cell.text.clone(),
                    style: cell.style,
                });
            }
        } else {
            out.push((
                kind,
                cell.col + cell_span,
                TextSegment {
                    x,
                    spacing,
                    runs: vec![StyledRun {
                        text: cell.text.clone(),
                        style: cell.style,
                    }],
                },
            ));
        }
    }
    out.into_iter().map(|(_, _, seg)| seg).collect()
}

fn classify_cell(
    cell: &RowCell,
    cw: f32,
    advance: &mut impl FnMut(&str) -> f32,
) -> (SegmentKind, f32, f32) {
    let col_x = cell.col as f32 * cw;
    // Fast path: plain ASCII printable characters & spaces always match the
    // monospace cell width in a true monospace font.
    if cell.width == 1 && cell.text.len() == 1 && cell.text.as_bytes()[0].is_ascii() {
        return (SegmentKind::Inline, col_x, 0.0);
    }

    let target = cell.width.max(1) as f32 * cw;
    let adv = advance(&cell.text);

    if cell.width == 2 && !is_pua_or_special(&cell.text) && adv > 0.0 {
        let gap = (target - adv).max(0.0);
        let key = (adv * 16.0).round() as u32;
        return (SegmentKind::Wide(key), col_x + gap * 0.5, gap);
    }

    // Single-column non-ASCII glyph: keep inline ONLY if it's not a PUA/symbol
    // icon and its measured advance closely matches `cw`. Otherwise pin it to
    // its exact column so any fallback/tofu/icon glyph never shifts what follows.
    if cell.width == 1
        && !is_pua_or_special(&cell.text)
        && adv > 0.0
        && (adv - cw).abs() <= cw * 0.06
    {
        return (SegmentKind::Inline, col_x, 0.0);
    }

    let offset = ((target - adv).max(0.0)) * 0.5;
    (SegmentKind::Pinned, col_x + offset, 0.0)
}

/// Characters that frequently come from a fallback / Nerd Font / emoji font
/// (PUA, Powerline, box-drawing, Dingbats, ZWJ clusters) and should be pinned
/// to their own cell column so they can never cause drift in following runs.
fn is_pua_or_special(grapheme: &str) -> bool {
    if grapheme.chars().count() > 1 {
        return true;
    }
    let Some(ch) = grapheme.chars().next() else {
        return false;
    };
    matches!(
        ch as u32,
        0x2500..=0x27BF   // Box Drawing, Block Elements, Geometric Shapes, Misc Symbols, Dingbats
        | 0xE000..=0xF8FF // BMP Private Use Area (Nerd Font / Powerline)
        | 0xF0000..=0x10FFFF // Supplementary PUA (Nerd Font Material Design icons, etc.)
    )
}

fn find_match_columns(cells: &[RowCell], needle_lower: &str) -> Vec<(usize, usize)> {
    if cells.is_empty() || needle_lower.is_empty() {
        return Vec::new();
    }
    // Map each char in the lowered line back to its cell `(col, width)`.
    let mut lowered = String::with_capacity(cells.len());
    let mut byte_to_cell: Vec<(usize, usize)> = Vec::with_capacity(cells.len());
    for cell in cells {
        for ch in cell.text.chars() {
            for low in ch.to_lowercase() {
                let start_len = lowered.len();
                lowered.push(low);
                let added = lowered.len() - start_len;
                for _ in 0..added {
                    byte_to_cell.push((cell.col, cell.width.max(1)));
                }
            }
        }
    }
    let mut matches = Vec::new();
    let mut offset = 0usize;
    while let Some(pos) = lowered[offset..].find(needle_lower) {
        let start_byte = offset + pos;
        let end_byte = start_byte + needle_lower.len();
        if let (Some(&(start_col, _)), Some(&(end_col, end_w))) = (
            byte_to_cell.get(start_byte),
            byte_to_cell.get(end_byte.saturating_sub(1)),
        ) {
            let span = (end_col + end_w).saturating_sub(start_col).max(1);
            matches.push((start_col, span));
        }
        offset = end_byte;
    }
    matches
}

fn render_run(run: StyledRun, dark: bool) -> AnyElement {
    let style = run.style;
    let fg = resolve_fg(&style, dark);
    let mut el = text(run.text).allow_font_scaling(false).text_color(fg);
    if style.bold {
        el = el.font_weight(FontWeight::BOLD);
    }
    if style.italic {
        el = el.italic();
    }
    if style.underline {
        el = el.underline();
    }
    if style.strikethrough {
        el = el.line_through();
    }
    if style.hidden {
        el = el.opacity(0.0);
    } else if style.dim {
        el = el.opacity(0.6);
    }
    el.into_element()
}

/// Dark-mode 16-color ANSI palette (crisp, legible on dark `terminal_surface`).
const ANSI_DARK: [u32; 16] = [
    0x1e1e24, 0xf15c67, 0x34d399, 0xf5c451, 0x60a5fa, 0xc084fc, 0x22d3ee, 0xe5e7eb, 0x6b7280,
    0xfb7185, 0x4ade80, 0xfde047, 0x93c5fd, 0xd8b4fe, 0x67e8f9, 0xffffff,
];

/// Light-mode 16-color ANSI palette (high contrast on light `terminal_surface`).
const ANSI_LIGHT: [u32; 16] = [
    0x1f2328, 0xcf222e, 0x116329, 0x9a6700, 0x0969da, 0x8250df, 0x0e7490, 0x57606a, 0x6e7781,
    0xa40e26, 0x1a7f37, 0xbf8700, 0x218bff, 0xa475f9, 0x1b7c83, 0x24292f,
];

fn ansi_rgb(index: u8, dark_ground: bool) -> Hsla {
    let palette = if dark_ground { ANSI_DARK } else { ANSI_LIGHT };
    nsh_core::color::rgb(palette[usize::from(index.min(15))]).into()
}

fn indexed_rgb(n: u8, dark_ground: bool) -> Hsla {
    let hex = match n {
        0..=15 => {
            return ansi_rgb(n, dark_ground);
        }
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + u32::from(v) * 40 };
            (level(n / 36) << 16) | (level((n / 6) % 6) << 8) | level(n % 6)
        }
        _ => {
            let v = 8 + u32::from(n - 232) * 10;
            (v << 16) | (v << 8) | v
        }
    };
    nsh_core::color::rgb(hex).into()
}

fn resolve_cell_color(color: CellColor, dark_ground: bool) -> Option<Hsla> {
    match color {
        CellColor::DefaultFg | CellColor::DefaultBg => None,
        CellColor::Ansi(n) => Some(ansi_rgb(n, dark_ground)),
        CellColor::Indexed(n) => Some(indexed_rgb(n, dark_ground)),
        CellColor::Rgb(r, g, b) => Some(
            nsh_core::color::rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)).into(),
        ),
    }
}

fn resolve_bg_span(span: &BgSpan, dark: bool) -> Option<Hsla> {
    if span.inverse {
        Some(resolve_cell_color(span.fg_for_inverse, dark).unwrap_or_else(theme::foreground))
    } else {
        resolve_cell_color(span.color, dark)
    }
}

fn resolve_fg(style: &CellStyle, dark: bool) -> Hsla {
    if style.inverse {
        let effective_bg = resolve_cell_color(style.fg, dark).unwrap_or_else(theme::foreground);
        let bg_is_dark = effective_bg.l < 0.5;
        return resolve_cell_color(style.bg, bg_is_dark).unwrap_or_else(theme::terminal_surface);
    }
    // If the cell sits on an explicit background color (e.g. a colored Starship
    // pill), choose light/dark ANSI foreground colors against THAT background
    // rather than the page appearance so white-on-green pills stay bright.
    let ground_is_dark = resolve_cell_color(style.bg, dark)
        .map(|bg| bg.l < 0.5)
        .unwrap_or(dark);
    resolve_cell_color(style.fg, ground_is_dark).unwrap_or_else(theme::foreground)
}

fn color_for_query(index: usize) -> Option<Rgb> {
    let hsla = match index {
        256 => theme::foreground(),
        257 => theme::terminal_surface(),
        0..=15 => ansi_rgb(index as u8, dark()),
        _ => return None,
    };
    let rgba = Rgba::from(hsla);
    Some(Rgb {
        r: (rgba.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        g: (rgba.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        b: (rgba.b.clamp(0.0, 1.0) * 255.0).round() as u8,
    })
}

fn font_css(family: &str, size: Points) -> String {
    let families = nsh_core::css_font_families(family);
    format!(
        "{}px {}",
        size.0,
        if families.is_empty() {
            "monospace"
        } else {
            &families
        }
    )
}

fn glyph_advance(font: &str, grapheme: &str) -> f32 {
    let key = (font.to_owned(), grapheme.to_owned());
    if let Some(width) = ADVANCES.with_borrow(|cache| cache.get(&key).copied()) {
        return width;
    }
    let width = measure_text(font, grapheme).0;
    ADVANCES.with_borrow_mut(|cache| {
        if cache.len() > 2048 {
            cache.clear();
        }
        cache.insert(key, width);
    });
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(col: usize, width: usize, text: &str) -> RowCell {
        RowCell {
            col,
            width,
            text: text.to_owned(),
            style: CellStyle::default(),
        }
    }

    #[test]
    fn pua_nerd_font_icon_is_pinned_and_never_shifts_following_ascii() {
        let cells = vec![
            cell(0, 1, "\u{e0b0}"), // Powerline PUA icon
            cell(1, 1, " "),
            cell(2, 1, "m"),
            cell(3, 1, "a"),
            cell(4, 1, "i"),
            cell(5, 1, "n"),
        ];
        // Even if the platform reports a wider advance for the PUA/tofu glyph,
        // the ASCII run after it still starts cleanly at col 1 * cw = 8.0.
        let segs = build_segments(&cells, 8.0, |g| if g == "\u{e0b0}" { 12.0 } else { 8.0 });
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].x, 0.0);
        assert_eq!(segs[0].runs[0].text, "\u{e0b0}");
        assert_eq!(segs[1].x, 8.0);
        assert_eq!(segs[1].runs[0].text, " main");
    }

    #[test]
    fn wide_cjk_cells_align_to_two_columns_and_next_ascii_lands_on_grid() {
        let cells = vec![
            cell(0, 1, "a"),
            cell(1, 2, "中"),
            cell(3, 2, "文"),
            cell(5, 1, "b"),
        ];
        let segs = build_segments(&cells, 8.0, |_| 13.0);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].x, 0.0);
        assert_eq!(segs[1].x, 8.0 + 1.5);
        assert_eq!(segs[1].spacing, 3.0);
        assert_eq!(segs[2].x, 40.0);
    }

    #[test]
    fn find_highlights_locate_case_insensitive_matches_by_column() {
        let cells = vec![
            cell(0, 1, "H"),
            cell(1, 1, "e"),
            cell(2, 1, "l"),
            cell(3, 1, "l"),
            cell(4, 1, "o"),
            cell(5, 1, " "),
            cell(6, 2, "中"),
            cell(8, 1, "h"),
            cell(9, 1, "e"),
            cell(10, 1, "l"),
            cell(11, 1, "l"),
            cell(12, 1, "o"),
        ];
        assert_eq!(find_match_columns(&cells, "hello"), vec![(0, 5), (8, 5)]);
    }
}
