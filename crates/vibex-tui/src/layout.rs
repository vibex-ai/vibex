//! Screen composition.
//!
//! The interface is a **vertical stack of full-width bands**, not a row of
//! boxed panes. Everything lives in one column:
//!
//! ```text
//!   ┌─ outer padding (1 row top and bottom, 2 columns each side) ─┐
//!   │ <cwd>                            <seat> │ <context> │ …    │  status
//!   │                                                            │
//!   │  transcript — no border, its own left edge is the margin    │  fills
//!   │                                                            │
//!   │ ⠹ running · read src/net/upload.rs          12s ⇣1.2k      │  turn status
//!   │ ╭─ title ────────────────────────────────────────╮         │
//!   │ │ ❯ draft                                        │         │  prompt
//!   │ ╰─ agent · model ────────────────────────────────╯         │
//!   │ Ctrl+P Commands │ ? Help │ Ctrl+Q Quit                      │  shortcuts
//!   └────────────────────────────────────────────────────────────┘
//! ```
//!
//! Why a stack rather than panes: a terminal is a fixed grid, and every border
//! spent on dividing it is two columns or two rows the content does not get. A
//! transcript is the thing being read, so it takes the full width and the
//! glyphs inside it -- the prompt mark, the work bullet, the heading colour --
//! do the work a frame would.
//! Navigation that would otherwise be a permanent sidebar becomes a full-screen
//! view or an overlay, which is also where it can show enough to be useful.
//!
//! The module is pure data: it computes rectangles and nothing else, so the
//! composition can be asserted at any terminal size without rendering.

use ratatui::layout::{Constraint, Layout, Rect};

/// Rows at or below which optional bands are dropped.
///
/// A short terminal must not lose the transcript or the prompt to chrome; on a
/// 16-row screen the banner and the follow-up row are worth less than two more
/// lines of conversation.
pub const SHORT_TERMINAL_ROWS: u16 = 16;

/// The transcript never shrinks below this many rows.
pub const SCROLLBACK_MIN_ROWS: u16 = 5;

/// Outer padding, in rows above and below the whole stack.
pub const OUTER_VPAD: u16 = 1;
/// Outer padding, in columns left and right of the whole stack.
pub const OUTER_HPAD: u16 = 2;

/// What the caller wants on screen this frame. A zero height means "hidden".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BandRequest {
    /// The requested content height of each optional band, top to bottom.
    pub tasks: u16,
    pub todo: u16,
    pub queue: u16,
    /// The running-turn line above the prompt.
    pub turn_status: u16,
    /// A transient message row (mode switch, tip).
    pub banner: u16,
    /// The collapsible panel above the composer: what the session is running.
    pub dock: u16,
    /// The composer's total height, borders included.
    pub prompt: u16,
    /// Rows between the transcript and the composer.
    pub prompt_gap: u16,
    /// The key hint bar. Always present unless the caller sets it to zero.
    pub shortcuts: u16,
    /// A denser status row under the composer.
    pub status_line: u16,
}

/// The rectangles the frame is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bands {
    /// Everything inside the outer padding.
    pub content: Rect,
    pub status: Rect,
    pub tasks: Rect,
    pub todo: Rect,
    pub scrollback: Rect,
    /// The transcript's right gutter, where the turn rail and scrollbar live.
    pub gutter: Rect,
    pub queue: Rect,
    pub turn_status: Rect,
    pub banner: Rect,
    pub dock: Rect,
    pub prompt: Rect,
    pub status_line: Rect,
    pub shortcuts: Rect,
}

impl Bands {
    /// Whether a band has any area, so callers can skip its renderer.
    pub fn is_visible(rect: Rect) -> bool {
        rect.width > 0 && rect.height > 0
    }
}

/// Columns the turn rail owns.
pub const GUTTER_WIDTH: u16 = 2;

/// Below this transcript width the gutter is returned to the content.
///
/// The rail is a navigator; when the transcript is narrow, two more columns of
/// prose are worth more than a map of it.
pub const MIN_TRANSCRIPT_FOR_GUTTER: u16 = 52;

/// Compute the frame.
///
/// Optional bands are dropped in reverse order of usefulness as the terminal
/// shortens: the banner first, then the tasks and todo rows. The transcript, the
/// composer and the hint bar are never dropped.
pub fn compute(area: Rect, request: BandRequest) -> Bands {
    let short = area.height <= SHORT_TERMINAL_ROWS;
    let outer_vpad = if area.height == 0 { 0 } else { OUTER_VPAD };
    let hpad = OUTER_HPAD.min(area.width / 4);

    let content = Rect {
        x: area.x.saturating_add(hpad),
        y: area.y.saturating_add(outer_vpad),
        width: area.width.saturating_sub(hpad.saturating_mul(2)),
        height: area.height.saturating_sub(outer_vpad.saturating_mul(2)),
    };

    // A short terminal gives up its optional rows before it gives up any of the
    // transcript or the composer.
    let banner = if short { 0 } else { request.banner };
    // The dock is a convenience panel, so a short terminal gives its rows back
    // to the transcript before anything essential is dropped.
    let dock = if short { 0 } else { request.dock };
    let tasks = if short { 0 } else { request.tasks };
    let todo = if short { 0 } else { request.todo };
    let turn_status = request.turn_status;
    let queue = request.queue;
    let shortcuts = request.shortcuts;

    // A blank row above each optional band, so bands read as separate.
    let gap = |height: u16| -> u16 {
        if height > 0 && content.height > 0 {
            1
        } else {
            0
        }
    };

    let mut constraints = vec![Constraint::Length(1)]; // status
    for height in [tasks, todo] {
        if height > 0 {
            constraints.push(Constraint::Length(gap(height)));
            constraints.push(Constraint::Length(height));
        }
    }
    constraints.push(Constraint::Length(gap(1)));
    constraints.push(Constraint::Min(SCROLLBACK_MIN_ROWS));
    for height in [queue, turn_status, banner, dock] {
        if height > 0 {
            constraints.push(Constraint::Length(1));
            constraints.push(Constraint::Length(height));
        }
    }
    if request.prompt_gap > 0 {
        constraints.push(Constraint::Length(request.prompt_gap));
    }
    constraints.push(Constraint::Length(request.prompt));
    let status_line = if short { 0 } else { request.status_line };
    if status_line > 0 {
        constraints.push(Constraint::Length(status_line));
    }
    constraints.push(Constraint::Length(shortcuts));

    let chunks = Layout::vertical(constraints).split(content);
    let mut next = chunks.iter().copied();
    let mut take = || next.next().unwrap_or_default();
    // A constraint that was not pushed has no chunk, so the reader walks the
    // same sequence the builder did.
    let status = take();
    let tasks_rect = if tasks > 0 {
        take();
        take()
    } else {
        Rect::default()
    };
    let todo_rect = if todo > 0 {
        take();
        take()
    } else {
        Rect::default()
    };
    take(); // the gap above the transcript
    let scrollback = take();
    let queue_rect = if queue > 0 {
        take();
        take()
    } else {
        Rect::default()
    };
    let turn_status_rect = if turn_status > 0 {
        take();
        take()
    } else {
        Rect::default()
    };
    let banner_rect = if banner > 0 {
        take();
        take()
    } else {
        Rect::default()
    };
    let dock_rect = if dock > 0 {
        take();
        take()
    } else {
        Rect::default()
    };
    if request.prompt_gap > 0 {
        take();
    }
    let prompt = take();
    let status_line_rect = if status_line > 0 {
        take()
    } else {
        Rect::default()
    };
    let shortcuts_rect = take();

    // The gutter is taken from the transcript's right edge, and only when there
    // is width to spare: on a narrow terminal the transcript needs the columns
    // more than the navigator does.
    let rail_columns = if scrollback.width >= MIN_TRANSCRIPT_FOR_GUTTER {
        GUTTER_WIDTH
    } else {
        0
    };
    let gutter = Rect {
        x: scrollback.x + scrollback.width.saturating_sub(rail_columns),
        y: scrollback.y,
        width: rail_columns,
        height: scrollback.height,
    };
    let scrollback = Rect {
        width: scrollback.width.saturating_sub(rail_columns),
        ..scrollback
    };

    Bands {
        content,
        status,
        tasks: tasks_rect,
        todo: todo_rect,
        scrollback,
        gutter,
        queue: queue_rect,
        turn_status: turn_status_rect,
        banner: banner_rect,
        dock: dock_rect,
        prompt,
        status_line: status_line_rect,
        shortcuts: shortcuts_rect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> BandRequest {
        BandRequest {
            turn_status: 1,
            prompt: 4,
            prompt_gap: 1,
            shortcuts: 1,
            ..BandRequest::default()
        }
    }

    #[test]
    fn the_bands_stack_without_overlapping() {
        let bands = compute(Rect::new(0, 0, 120, 40), request());
        let order = [
            bands.status,
            bands.scrollback,
            bands.turn_status,
            bands.prompt,
            bands.shortcuts,
        ];
        for pair in order.windows(2) {
            assert!(
                pair[0].bottom() <= pair[1].y,
                "{:?} overlaps {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn the_stack_fills_the_content_area_exactly() {
        let area = Rect::new(0, 0, 120, 40);
        let bands = compute(area, request());
        // The last band ends on the content's last row, so nothing is wasted at
        // the bottom and nothing is pushed off.
        assert_eq!(bands.shortcuts.bottom(), bands.content.bottom());
        assert_eq!(bands.status.y, bands.content.y);
        // The transcript absorbs the slack.
        assert!(bands.scrollback.height > 10);
    }

    #[test]
    fn the_dock_sits_directly_above_the_composer() {
        let bands = compute(
            Rect::new(0, 0, 120, 40),
            BandRequest {
                turn_status: 1,
                banner: 1,
                dock: 4,
                prompt: 4,
                prompt_gap: 1,
                shortcuts: 1,
                ..BandRequest::default()
            },
        );
        // Banner, then dock, then the gap, then the prompt.
        assert_eq!(bands.dock.y, bands.banner.bottom() + 1);
        assert_eq!(bands.prompt.y, bands.dock.bottom() + 1);
        assert!(bands.dock.height >= 1);
    }

    #[test]
    fn a_short_terminal_drops_optional_bands_before_the_transcript() {
        let tall = compute(
            Rect::new(0, 0, 120, 40),
            BandRequest {
                tasks: 3,
                todo: 4,
                banner: 1,
                ..request()
            },
        );
        assert!(Bands::is_visible(tall.tasks));
        assert!(Bands::is_visible(tall.todo));
        assert!(Bands::is_visible(tall.banner));

        let short = compute(
            Rect::new(0, 0, 120, SHORT_TERMINAL_ROWS),
            BandRequest {
                tasks: 3,
                todo: 4,
                banner: 1,
                ..request()
            },
        );
        assert!(!Bands::is_visible(short.tasks));
        assert!(!Bands::is_visible(short.todo));
        assert!(!Bands::is_visible(short.banner));
        // The three bands that matter survive.
        assert!(short.scrollback.height >= SCROLLBACK_MIN_ROWS);
        assert!(Bands::is_visible(short.prompt));
        assert!(Bands::is_visible(short.shortcuts));
    }

    #[test]
    fn the_transcript_keeps_its_minimum_at_every_size() {
        for height in [17u16, 20, 24, 40, 60] {
            let bands = compute(
                Rect::new(0, 0, 120, height),
                BandRequest {
                    tasks: 2,
                    todo: 2,
                    queue: 2,
                    turn_status: 1,
                    banner: 1,
                    dock: 4,
                    prompt: 4,
                    prompt_gap: 1,
                    shortcuts: 1,
                    status_line: 1,
                },
            );
            assert!(
                bands.scrollback.height >= SCROLLBACK_MIN_ROWS,
                "at {height} rows the transcript got {}",
                bands.scrollback.height
            );
        }
    }

    #[test]
    fn the_gutter_is_taken_from_the_transcript_and_returned_when_narrow() {
        let wide = compute(Rect::new(0, 0, 120, 40), request());
        assert_eq!(wide.gutter.width, GUTTER_WIDTH);
        assert_eq!(wide.gutter.right(), wide.scrollback.right() + GUTTER_WIDTH);
        assert_eq!(wide.scrollback.right(), wide.gutter.x);

        let narrow = compute(Rect::new(0, 0, 48, 40), request());
        assert_eq!(narrow.gutter.width, 0);
        // The freed columns go back to the transcript rather than being lost.
        assert_eq!(narrow.scrollback.right(), narrow.content.right());
    }

    #[test]
    fn outer_padding_shrinks_on_a_narrow_terminal_instead_of_eating_it() {
        // Two columns of padding each side is right on a wide screen and absurd
        // on a 30-column one.
        let narrow = compute(Rect::new(0, 0, 24, 40), request());
        assert!(narrow.content.width >= 12);
        assert!(narrow.content.x >= 2);
        assert!(narrow.content.right() <= 22);
    }

    #[test]
    fn a_zero_sized_terminal_produces_zero_rects_rather_than_panicking() {
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 1, 1),
            Rect::new(0, 0, 10, 3),
        ] {
            let bands = compute(area, request());
            assert!(bands.content.width <= area.width);
            assert!(bands.content.height <= area.height);
        }
    }

    #[test]
    fn every_visible_band_sits_inside_the_content_area() {
        let bands = compute(
            Rect::new(3, 5, 100, 30),
            BandRequest {
                tasks: 2,
                todo: 2,
                queue: 2,
                turn_status: 1,
                banner: 1,
                dock: 3,
                status_line: 1,
                prompt: 4,
                prompt_gap: 1,
                shortcuts: 1,
            },
        );
        for rect in [
            bands.status,
            bands.tasks,
            bands.todo,
            bands.scrollback,
            bands.gutter,
            bands.queue,
            bands.turn_status,
            bands.banner,
            bands.dock,
            bands.prompt,
            bands.status_line,
            bands.shortcuts,
        ] {
            if !Bands::is_visible(rect) {
                continue;
            }
            assert!(rect.x >= bands.content.x, "{rect:?} escapes left");
            assert!(
                rect.right() <= bands.content.right(),
                "{rect:?} escapes right"
            );
            assert!(rect.y >= bands.content.y, "{rect:?} escapes top");
            assert!(
                rect.bottom() <= bands.content.bottom(),
                "{rect:?} escapes bottom"
            );
        }
    }
}
