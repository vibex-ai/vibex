//! One chrome for every modal surface.
//!
//! Every popup in the interface — the command palette, the runtime picker, the
//! approval card, a text view, a confirmation — is drawn through
//! [`render_modal`]. The point is not code reuse for its own sake: it is that
//! the reader learns one shape. A modal always has the same border, the same
//! title placement, the same close affordance in the same corner, the same
//! inner padding, and a footer whose hints are always bottom-aligned and
//! centered. A surface that drew its own box would quietly drift.
//!
//! Sizing is a preset rather than a per-call rectangle, so a modal declares
//! *how important it is* (a palette, a picker, a full text view) and the
//! geometry follows. [`ModalSizing::compact`] gives the margins back on a short
//! or narrow terminal, which is the one case where the box would otherwise eat
//! the content.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::glyphs::{self, GlyphTier};
use crate::text::{display_width, truncate_to_width};
use crate::theme::{GlyphMode, TuiTheme};

/// The narrowest a modal may be before it stops being usable.
pub const MIN_MODAL_WIDTH: u16 = 20;
/// The shortest a modal may be before it stops being usable.
pub const MIN_MODAL_HEIGHT: u16 = 6;
/// Columns the close affordance reserves on the top border.
pub const CLOSE_WIDTH: u16 = 5;
/// Columns between footer hints.
pub const HINT_SEPARATOR: &str = "  |  ";

/// The border glyphs a terminal can render, as a reusable block.
pub fn border_block(theme: &TuiTheme) -> Block<'static> {
    let block = Block::default().borders(Borders::ALL);
    match theme.glyphs() {
        GlyphMode::Unicode => block.border_type(BorderType::Rounded),
        GlyphMode::Ascii => block
            .border_type(BorderType::Plain)
            .border_set(border::Set {
                top_left: "+",
                top_right: "+",
                bottom_left: "+",
                bottom_right: "+",
                horizontal_top: "-",
                horizontal_bottom: "-",
                vertical_left: "|",
                vertical_right: "|",
            }),
    }
}

/// How a modal sizes itself against the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModalSizing {
    /// Preferred width as a percentage of the terminal.
    pub width_percent: u16,
    pub max_width: u16,
    pub min_width: u16,
    /// Rows kept free above and below the popup.
    pub v_margin: u16,
    /// Columns of padding inside the border.
    pub h_pad: u16,
    /// Rows of padding between the top border and the content.
    pub v_pad: u16,
    /// Rows the footer reserves even when it has nothing to say.
    pub footer_rows: u16,
}

impl ModalSizing {
    pub const fn new(
        width_percent: u16,
        max_width: u16,
        min_width: u16,
        v_margin: u16,
        h_pad: u16,
        v_pad: u16,
        footer_rows: u16,
    ) -> Self {
        Self {
            width_percent,
            max_width,
            min_width,
            v_margin,
            h_pad,
            v_pad,
            footer_rows,
        }
    }

    /// The command palette: narrow, because it holds short labels.
    pub const fn palette() -> Self {
        Self::new(50, 80, 44, 4, 2, 1, 2)
    }

    /// A short question or a single field.
    pub const fn prompt() -> Self {
        Self::new(50, 64, 40, 4, 2, 1, 2)
    }

    /// A list picker: the runtime picker, a value chooser.
    pub const fn picker() -> Self {
        Self::new(65, 120, 48, 4, 2, 1, 2)
    }

    /// A dense, information-heavy surface: help, settings.
    pub const fn large() -> Self {
        Self::new(80, 110, 44, 3, 2, 1, 2)
    }

    /// A read-only document: as much of the screen as the margins allow.
    pub const fn document() -> Self {
        Self::new(96, 140, 44, 2, 2, 0, 1)
    }

    /// An urgent card: approvals, questions. It owns the eye, not the screen.
    pub const fn card() -> Self {
        Self::new(70, 80, 44, 4, 2, 1, 2)
    }

    /// Give the margins back on a small terminal.
    pub const fn compact(mut self) -> Self {
        self.v_margin = 0;
        self.h_pad = 1;
        self.v_pad = 0;
        self
    }

    pub const fn with_footer_rows(mut self, rows: u16) -> Self {
        self.footer_rows = rows;
        self
    }
}

/// The rectangle a modal occupies in `area`.
pub fn dimensions(area: Rect, sizing: ModalSizing) -> Rect {
    let max_width = area.width.saturating_sub(4).min(sizing.max_width);
    let preferred = (u32::from(area.width) * u32::from(sizing.width_percent) / 100) as u16;
    let width = preferred
        .min(max_width)
        .max(sizing.min_width)
        .min(area.width);
    let height = area
        .height
        .saturating_sub(sizing.v_margin.saturating_mul(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// The close affordance's rectangle on a modal's top border.
///
/// Published separately from [`render_modal`] so the mouse layer can hit-test
/// the same cells without re-deriving the size presets.
pub fn close_rect(popup: Rect) -> Rect {
    Rect {
        x: popup.x + popup.width.saturating_sub(CLOSE_WIDTH + 2),
        y: popup.y,
        width: CLOSE_WIDTH.min(popup.width),
        height: 1,
    }
}

/// One footer hint: the key and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModalHint {
    pub key: &'static str,
    pub label: &'static str,
}

impl ModalHint {
    pub const fn new(key: &'static str, label: &'static str) -> Self {
        Self { key, label }
    }
}

/// Everything the shared chrome draws for one modal.
pub struct ModalChrome<'a> {
    pub title: &'a str,
    pub sizing: ModalSizing,
    pub hints: Vec<ModalHint>,
    /// Whether the top-right close affordance is drawn.
    pub close: bool,
}

impl<'a> ModalChrome<'a> {
    pub fn new(title: &'a str, sizing: ModalSizing) -> Self {
        Self {
            title,
            sizing,
            hints: Vec::new(),
            close: true,
        }
    }

    pub fn hints(mut self, hints: Vec<ModalHint>) -> Self {
        self.hints = hints;
        self
    }

    pub fn close(mut self, close: bool) -> Self {
        self.close = close;
        self
    }
}

/// The regions the chrome resolved, so a caller can hit-test and fill them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModalLayout {
    /// The whole popup, border included.
    pub area: Rect,
    /// Inside the border, horizontal padding and top padding applied.
    pub content: Rect,
    /// The bottom-aligned hint rows, when the modal is tall enough for them.
    pub footer: Option<Rect>,
    /// The close affordance on the top border, for mouse hit-testing.
    pub close: Option<Rect>,
}

/// Draw one modal's chrome and return the regions to fill.
///
/// A modal that cannot be drawn at a usable size renders a single explanatory
/// row instead of a broken box, which is the only honest degradation on a
/// terminal that small.
pub fn render_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    chrome: &ModalChrome<'_>,
    theme: &TuiTheme,
) -> Option<ModalLayout> {
    let popup = dimensions(area, chrome.sizing);
    if popup.width < MIN_MODAL_WIDTH || popup.height < MIN_MODAL_HEIGHT {
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(truncate_to_width(
                chrome.title,
                usize::from(area.width),
                "…",
            ))
            .style(theme.muted())
            .alignment(Alignment::Center),
            area,
        );
        return None;
    }
    frame.render_widget(Clear, popup);

    let close_rect = chrome.close.then(|| close_rect(popup));
    let title_budget = usize::from(popup.width).saturating_sub(if chrome.close { 8 } else { 4 });
    let title = truncate_to_width(chrome.title, title_budget, "…");
    let block = border_block(theme)
        .border_style(theme.focus_style())
        .style(theme.base())
        .title(Span::styled(format!(" {title} "), theme.strong()));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if let Some(close) = close_rect {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("[{0}]", glyphs::ballot_x(GlyphTier::of(theme))),
                theme.muted(),
            ))),
            Rect { width: 3, ..close },
        );
    }

    let hints_width = usize::from(inner.width.saturating_sub(chrome.sizing.h_pad * 2));
    let rows = hint_rows(&chrome.hints, hints_width);
    let footer_height = rows
        .len()
        .max(usize::from(chrome.sizing.footer_rows))
        .min(usize::from(inner.height.saturating_sub(2))) as u16;
    let content = Rect {
        x: inner.x + chrome.sizing.h_pad,
        y: inner.y + chrome.sizing.v_pad,
        width: inner.width.saturating_sub(chrome.sizing.h_pad * 2),
        height: inner
            .height
            .saturating_sub(footer_height)
            .saturating_sub(chrome.sizing.v_pad),
    };
    let mut footer = None;
    if footer_height > 0 {
        let footer_area = Rect {
            x: inner.x + chrome.sizing.h_pad,
            y: inner.y + inner.height.saturating_sub(footer_height),
            width: inner.width.saturating_sub(chrome.sizing.h_pad * 2),
            height: footer_height,
        };
        let visible = rows
            .iter()
            .rev()
            .take(usize::from(footer_height))
            .rev()
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Text::from(visible)), footer_area);
        footer = Some(footer_area);
    }

    Some(ModalLayout {
        area: popup,
        content,
        footer,
        close: close_rect,
    })
}

/// Greedily wrap footer hints into centered rows of `key label` pairs.
fn hint_rows(hints: &[ModalHint], width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for hint in hints {
        let text = format!("{} {}", hint.key, hint.label);
        let width_used = display_width(&text);
        let separator = if current.is_empty() {
            0
        } else {
            display_width(HINT_SEPARATOR)
        };
        if !current.is_empty() && used + separator + width_used > width {
            rows.push(Line::from(std::mem::take(&mut current)).alignment(Alignment::Center));
            used = 0;
        }
        if !current.is_empty() {
            current.push(Span::styled(HINT_SEPARATOR, Style::default()));
            used += display_width(HINT_SEPARATOR);
        }
        current.push(Span::styled(
            hint.key.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        current.push(Span::raw(" "));
        current.push(Span::styled(
            hint.label.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ));
        used += width_used;
    }
    if !current.is_empty() {
        rows.push(Line::from(current).alignment(Alignment::Center));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn theme() -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            vibex_ui::GpuiThemeMode::Dark,
            crate::theme::ColorCapability {
                mode: crate::theme::ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
        )
    }

    #[test]
    fn a_modal_is_centred_and_never_wider_than_the_terminal() {
        for (width, height) in [(80u16, 24u16), (120, 40), (200, 50)] {
            let area = Rect::new(0, 0, width, height);
            let rect = dimensions(area, ModalSizing::picker());
            assert!(rect.width <= width, "{rect:?} escapes {width}");
            assert!(rect.height <= height);
            assert_eq!(rect.x, (width - rect.width) / 2);
        }
    }

    #[test]
    fn the_minimum_width_wins_over_a_narrow_preference() {
        let rect = dimensions(Rect::new(0, 0, 60, 20), ModalSizing::palette());
        assert!(rect.width >= 44);
    }

    #[test]
    fn compact_sizing_gives_the_margins_back() {
        let normal = dimensions(Rect::new(0, 0, 120, 40), ModalSizing::picker());
        let compact = dimensions(Rect::new(0, 0, 120, 40), ModalSizing::picker().compact());
        assert!(compact.height > normal.height);
    }

    #[test]
    fn footer_hints_wrap_and_keep_whole_pairs() {
        let hints = vec![
            ModalHint::new("↑↓", "nav"),
            ModalHint::new("e", "expand"),
            ModalHint::new("/", "search"),
            ModalHint::new("f", "filter"),
            ModalHint::new("d", "delete"),
        ];
        let rows = hint_rows(&hints, 30);
        assert!(rows.len() > 1, "a narrow footer must wrap");
        for row in &rows {
            let text = row
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(!text.starts_with(HINT_SEPARATOR.trim()));
        }
        // One wide row keeps every hint.
        let wide = hint_rows(&hints, 200);
        assert_eq!(wide.len(), 1);
        let joined = wide[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for hint in hints {
            assert!(joined.contains(hint.label), "{joined} lost {}", hint.label);
        }
    }

    #[test]
    fn a_usable_terminal_produces_content_inside_the_border() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let theme = theme();
        let mut layout = None;
        terminal
            .draw(|frame| {
                let chrome = ModalChrome::new("Test", ModalSizing::picker())
                    .hints(vec![ModalHint::new("Esc", "close")]);
                layout = render_modal(frame, frame.area(), &chrome, &theme);
            })
            .expect("frame draws");
        let layout = layout.expect("a 120x40 terminal fits a picker");
        assert!(layout.content.width > 0 && layout.content.height > 0);
        assert!(layout.content.x > layout.area.x);
        assert!(layout.content.bottom() <= layout.area.bottom());
        let footer = layout.footer.expect("a footer is reserved");
        assert_eq!(footer.bottom(), layout.area.bottom() - 1);
        assert!(layout.close.is_some());
    }

    #[test]
    fn an_impossible_terminal_degrades_to_a_title_rather_than_a_broken_box() {
        let backend = TestBackend::new(16, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let theme = theme();
        let mut layout = Some(ModalLayout {
            area: Rect::default(),
            content: Rect::default(),
            footer: None,
            close: None,
        });
        terminal
            .draw(|frame| {
                let chrome = ModalChrome::new("Test", ModalSizing::picker());
                layout = render_modal(frame, frame.area(), &chrome, &theme);
            })
            .expect("frame draws");
        assert!(layout.is_none());
    }
}
