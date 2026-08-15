use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId, InspectorElementId,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div,
    fill, point, prelude::*, px, relative, rgba, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::theme;

actions!(
    mobile_text_input,
    [
        Backspace,
        Delete,
        Enter,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Paste,
        Cut,
        Copy
    ]
);

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Vec<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_line_height: Option<Pixels>,
    selecting: bool,
    multiline: bool,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: Vec::new(),
            last_bounds: None,
            last_line_height: None,
            selecting: false,
            multiline: false,
        }
    }

    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_text(&mut self, value: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = value.into();
        self.selected_range = self.content.len()..self.content.len();
        self.marked_range = None;
        cx.notify();
    }

    pub fn take(&mut self, cx: &mut Context<Self>) -> String {
        let value = self.content.to_string();
        self.set_text("", cx);
        value
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = clamp_offset_to_boundary(&self.content, offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = clamp_offset_to_boundary(&self.content, offset);
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        offset_from_utf16(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        offset_to_utf16(&self.content, offset)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        range_from_utf16(&self.content, range)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line_height)) = (self.last_bounds, self.last_line_height) else {
            return 0;
        };
        let ranges = line_ranges(&self.content);
        let line_index = if position.y <= bounds.top() {
            0
        } else {
            (((position.y - bounds.top()).as_f32() / line_height.as_f32()).floor() as usize)
                .min(ranges.len().saturating_sub(1))
        };
        let range = &ranges[line_index];
        let Some(line) = self.last_layout.get(line_index) else {
            return range.start;
        };
        let local = if position.x <= bounds.left() {
            0
        } else {
            line.closest_index_for_x(position.x - bounds.left())
                .min(range.len())
        };
        clamp_offset_to_boundary(&self.content, range.start + local)
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selected_range.is_empty() {
            self.previous_boundary(self.cursor_offset())
        } else {
            self.selected_range.start
        };
        self.move_to(offset, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selected_range.is_empty() {
            self.next_boundary(self.cursor_offset())
        } else {
            self.selected_range.end
        };
        self.move_to(offset, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = if self.selected_range.is_empty() {
            self.cursor_offset()
        } else {
            self.selected_range.start
        };
        self.move_to(self.vertical_offset(cursor, -1), cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = if self.selected_range.is_empty() {
            self.cursor_offset()
        } else {
            self.selected_range.end
        };
        self.move_to(self.vertical_offset(cursor, 1), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical_offset(self.cursor_offset(), -1), cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.vertical_offset(self.cursor_offset(), 1), cx);
    }

    fn vertical_offset(&self, offset: usize, delta: isize) -> usize {
        if !self.multiline {
            return offset;
        }
        let ranges = line_ranges(&self.content);
        let (line_index, range) = line_range_for_offset(&ranges, offset);
        let target_index = line_index
            .saturating_add_signed(delta)
            .min(ranges.len().saturating_sub(1));
        if target_index == line_index {
            return offset;
        }
        let target = &ranges[target_index];
        let local = match (
            self.last_layout.get(line_index),
            self.last_layout.get(target_index),
        ) {
            (Some(line), Some(target_line)) => target_line
                .closest_index_for_x(line.x_for_index(offset.saturating_sub(range.start)))
                .min(target.len()),
            _ => offset.saturating_sub(range.start).min(target.len()),
        };
        clamp_offset_to_boundary(&self.content, target.start + local)
    }

    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let previous = self.previous_boundary(self.cursor_offset());
            if previous == self.cursor_offset() {
                window.play_system_bell();
                return;
            }
            self.select_to(previous, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if next == self.cursor_offset() {
                window.play_system_bell();
                return;
            }
            self.select_to(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        if !self.selected_range.is_empty() {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        window.show_soft_keyboard();
        self.selecting = true;
        let offset = self.index_for_position(event.position);
        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_for_position(event.position), cx);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }
}

fn offset_from_utf16(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in text.chars() {
        if utf16 >= offset {
            break;
        }
        utf16 += character.len_utf16();
        utf8 += character.len_utf8();
    }
    utf8
}

fn offset_to_utf16(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in text.chars() {
        if utf8 >= offset {
            break;
        }
        utf8 += character.len_utf8();
        utf16 += character.len_utf16();
    }
    utf16
}

fn range_from_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    offset_from_utf16(text, range.start)..offset_from_utf16(text, range.end)
}

fn clamp_offset_to_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn clamp_range_to_text(text: &str, range: Range<usize>) -> Range<usize> {
    let start = clamp_offset_to_boundary(text, range.start);
    let end = clamp_offset_to_boundary(text, range.end).max(start);
    start..end
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        self.content.get(range).map(ToOwned::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        let range = clamp_range_to_text(&self.content, range);
        let normalized = normalize_inserted_text(text, self.multiline);
        self.content = format!(
            "{}{}{}",
            &self.content[..range.start],
            normalized,
            &self.content[range.end..]
        )
        .into();
        let cursor = range.start + normalized.len();
        self.selected_range = cursor..cursor;
        self.marked_range = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let base = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        let base = clamp_range_to_text(&self.content, base);
        let normalized = normalize_inserted_text(text, self.multiline);
        self.content = format!(
            "{}{}{}",
            &self.content[..base.start],
            normalized,
            &self.content[base.end..]
        )
        .into();
        let marked = base.start..base.start + normalized.len();
        self.marked_range = (!normalized.is_empty()).then_some(marked.clone());
        self.selected_range = marked.end..marked.end;
        self.selection_reversed = false;
        if let Some(selected) = selected_utf16 {
            let selected = range_from_utf16(&normalized, &selected);
            self.selected_range = base.start + selected.start..base.start + selected.end;
        }
        window.refresh();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line_height = self.last_line_height?;
        let range = self.range_from_utf16(&range_utf16);
        let ranges = line_ranges(&self.content);
        let (start_line, start_range) = line_range_for_offset(&ranges, range.start);
        let (end_line, end_range) = line_range_for_offset(&ranges, range.end);
        let start_x = self
            .last_layout
            .get(start_line)?
            .x_for_index(range.start.saturating_sub(start_range.start));
        let end_x = self
            .last_layout
            .get(end_line)?
            .x_for_index(range.end.saturating_sub(end_range.start));
        Some(Bounds::from_corners(
            point(
                if start_line == end_line {
                    bounds.left() + start_x
                } else {
                    bounds.left()
                },
                bounds.top() + line_height * start_line as f32,
            ),
            point(
                if start_line == end_line {
                    bounds.left() + end_x
                } else {
                    bounds.right()
                },
                bounds.top() + line_height * (end_line + 1) as f32,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_position(point)))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }
}

fn normalize_inserted_text(text: &str, multiline: bool) -> String {
    if multiline {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.replace(['\r', '\n'], " ")
    }
}

fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if character == '\n' {
            ranges.push(start..index);
            start = index + 1;
        }
    }
    ranges.push(start..text.len());
    ranges
}

fn line_range_for_offset(ranges: &[Range<usize>], offset: usize) -> (usize, &Range<usize>) {
    let index = ranges
        .iter()
        .position(|range| offset <= range.end)
        .unwrap_or_else(|| ranges.len().saturating_sub(1));
    (index, &ranges[index])
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    lines: Vec<ShapedLine>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    line_height: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let input = self.input.read(cx);
        let line_count = if input.multiline {
            line_ranges(&input.content).len()
        } else {
            1
        };
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = (window.line_height() * line_count as f32).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let input = self.input.read(cx);
        let ranges = line_ranges(&input.content);
        let line_height = window.line_height();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let lines = ranges
            .iter()
            .enumerate()
            .map(|(line_index, range)| {
                let (text, color, global_start) = if input.content.is_empty() {
                    (input.placeholder.clone(), theme::text_muted(), 0)
                } else {
                    (
                        SharedString::from(input.content[range.clone()].to_string()),
                        style.color,
                        range.start,
                    )
                };
                let base = TextRun {
                    len: text.len(),
                    font: style.font(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let runs = if !input.content.is_empty() {
                    if let Some(marked) = input.marked_range.as_ref() {
                        let marked_start =
                            marked.start.saturating_sub(global_start).min(text.len());
                        let marked_end = marked.end.saturating_sub(global_start).min(text.len());
                        if marked_start < marked_end {
                            [
                                TextRun {
                                    len: marked_start,
                                    ..base.clone()
                                },
                                TextRun {
                                    len: marked_end - marked_start,
                                    underline: Some(UnderlineStyle {
                                        color: Some(base.color),
                                        thickness: px(1.0),
                                        wavy: false,
                                    }),
                                    ..base.clone()
                                },
                                TextRun {
                                    len: text.len() - marked_end,
                                    ..base
                                },
                            ]
                            .into_iter()
                            .filter(|run| run.len > 0)
                            .collect()
                        } else {
                            vec![base]
                        }
                    } else {
                        vec![base]
                    }
                } else {
                    vec![base]
                };
                let _ = line_index;
                window
                    .text_system()
                    .shape_line(text, font_size, &runs, None)
            })
            .collect::<Vec<_>>();

        let cursor = if input.selected_range.is_empty() {
            let (line_index, range) = line_range_for_offset(&ranges, input.cursor_offset());
            let cursor_x =
                lines[line_index].x_for_index(input.cursor_offset().saturating_sub(range.start));
            Some(fill(
                Bounds::new(
                    point(
                        bounds.left() + cursor_x,
                        bounds.top() + line_height * line_index as f32,
                    ),
                    size(px(1.5), line_height),
                ),
                theme::text_primary(),
            ))
        } else {
            None
        };
        let mut selections = Vec::new();
        if !input.selected_range.is_empty() {
            for (line_index, range) in ranges.iter().enumerate() {
                let start = input.selected_range.start.max(range.start).min(range.end);
                let end = input.selected_range.end.max(range.start).min(range.end);
                let includes_newline = range.end < input.content.len()
                    && input.selected_range.start <= range.end
                    && input.selected_range.end > range.end;
                if start < end || includes_newline {
                    let left = lines[line_index].x_for_index(start.saturating_sub(range.start));
                    let mut right = lines[line_index].x_for_index(end.saturating_sub(range.start));
                    if includes_newline {
                        right += px(6.0);
                    }
                    selections.push(fill(
                        Bounds::from_corners(
                            point(
                                bounds.left() + left,
                                bounds.top() + line_height * line_index as f32,
                            ),
                            point(
                                bounds.left() + right,
                                bounds.top() + line_height * (line_index + 1) as f32,
                            ),
                        ),
                        rgba(0x61afef44),
                    ));
                }
            }
        }
        PrepaintState {
            lines,
            cursor,
            selections,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        for selection in state.selections.drain(..) {
            window.paint_quad(selection);
        }
        for (line_index, line) in state.lines.iter().enumerate() {
            line.paint(
                point(
                    bounds.left(),
                    bounds.top() + state.line_height * line_index as f32,
                ),
                state.line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            )
            .expect("failed to paint mobile text input");
        }
        if focus.is_focused(window)
            && let Some(cursor) = state.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = state.lines.clone();
            input.last_bounds = Some(bounds);
            input.last_line_height = Some(state.line_height);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let multiline = self.multiline;
        div()
            .id(("mobile-text-input", cx.entity_id()))
            .key_context("MobileTextInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .w_full()
            .px_3()
            .flex()
            .when(multiline, |input| {
                input
                    .h_full()
                    .py_2()
                    .items_start()
                    .overflow_y_scroll()
                    .font_family("IBM Plex Mono")
                    .text_size(px(theme::FONT_CAPTION))
            })
            .when(!multiline, |input| {
                input
                    .h(px(theme::TOUCH_TARGET))
                    .items_center()
                    .overflow_hidden()
                    .text_size(px(theme::FONT_HEADING))
            })
            .text_color(theme::text_primary())
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_ranges_map_across_emoji_and_cjk() {
        let text = "A😀中Z";
        assert_eq!(range_from_utf16(text, &(1..3)), 1..5);
        assert_eq!(range_from_utf16(text, &(3..4)), 5..8);
        assert_eq!(offset_to_utf16(text, 8), 4);
    }

    #[test]
    fn composition_selection_is_relative_to_inserted_text() {
        let original = "prefix ";
        let base = original.len();
        let inserted = "A😀中";
        let selected = range_from_utf16(inserted, &(1..3));

        assert_eq!(base + selected.start..base + selected.end, 8..12);
    }

    #[test]
    fn placeholder_offsets_cannot_escape_the_empty_document() {
        assert_eq!(clamp_offset_to_boundary("", 18), 0);
        assert_eq!(clamp_range_to_text("", 18..18), 0..0);
    }

    #[test]
    fn stale_offsets_clamp_to_a_utf8_boundary() {
        assert_eq!(clamp_offset_to_boundary("A中", usize::MAX), 4);
        assert_eq!(clamp_offset_to_boundary("A中", 3), 1);
        assert_eq!(clamp_range_to_text("A中", 3..99), 1..4);
    }

    #[test]
    fn multiline_input_preserves_newlines_and_normalizes_crlf() {
        assert_eq!(normalize_inserted_text("a\r\nb\rc", true), "a\nb\nc");
        assert_eq!(normalize_inserted_text("a\r\nb\nc", false), "a  b c");
    }

    #[test]
    fn line_ranges_include_empty_trailing_line() {
        assert_eq!(line_ranges("a\n中\n"), vec![0..1, 2..5, 6..6]);
        let ranges = line_ranges("a\n中\n");
        assert_eq!(line_range_for_offset(&ranges, 1).0, 0);
        assert_eq!(line_range_for_offset(&ranges, 2).0, 1);
        assert_eq!(line_range_for_offset(&ranges, 6).0, 2);
    }
}
