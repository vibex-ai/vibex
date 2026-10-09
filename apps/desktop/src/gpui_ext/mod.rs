use gpui::{
    App, BorderStyle, ClipboardItem, InteractiveElement, Interactivity, IntoElement,
    ParentElement as _, Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled,
    Window, WindowAppearance, div, px,
};
use gpui_base::Selectable;
use gpui_component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    empty::Empty,
    notification::{Notification, NotificationType},
};
use vibex_desktop_runtime::validate_external_open_url;

use crate::{locale, platform::open_external_url};

/// An icon button that carries an accessible name.
///
/// GPUI attaches `aria_label` in the stateful layer, which `Button` does not
/// implement, so a button that shows only an icon is wrapped here. The wrapper
/// forwards the interactivity a trigger needs — including its selected state,
/// which is what tells a Popover the panel it owns is open.
#[derive(IntoElement)]
pub struct AccessibleButton(Button);

impl InteractiveElement for AccessibleButton {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.0.interactivity()
    }
}

impl StatefulInteractiveElement for AccessibleButton {}

impl Selectable for AccessibleButton {
    fn selected(mut self, selected: bool) -> Self {
        self.0 = self.0.selected(selected);
        self
    }

    fn is_selected(&self) -> bool {
        self.0.is_selected()
    }

    fn open(mut self, open: bool) -> Self {
        self.0 = self.0.open(open);
        self
    }

    fn is_open(&self) -> bool {
        self.0.is_open()
    }
}

impl RenderOnce for AccessibleButton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.0
    }
}

pub fn button_with_aria_label(button: Button, label: impl Into<SharedString>) -> AccessibleButton {
    AccessibleButton(button).aria_label(label)
}

/// The trailing inset a scroll region reserves for the overlay scrollbar.
///
/// `overflow_y_scrollbar()` positions the bar against the scroll container's
/// *padding* box, so the container's own trailing padding is the bar's lane.
/// The bar's ink reaches this far in from that edge: gpui-base insets the thumb
/// by 4 px and widens it to 8 px while it is hovered or dragged, so 12 px is
/// the widest it ever gets. A container that insets its content by less than
/// this lets the bar cover its last column, which is what a scroll region must
/// never do.
///
/// Fixed pixels rather than `rem`, deliberately: this is not product spacing,
/// it is the width of a piece of kit geometry that does not scale with the base
/// font. If the design system ever changes the thumb's inset or width, this
/// value has to change with it.
pub const SCROLLBAR_GUTTER: Pixels = px(12.0);

/// Reserves [`SCROLLBAR_GUTTER`] on a scroll region's trailing edge.
///
/// Apply it after any other padding so the gutter wins on the trailing side:
/// `.p_2().scroll_gutter()` keeps 8 px on three sides and widens the right one
/// to the bar's lane. A region whose own padding already reaches the gutter —
/// `.p_3()`, `.p_4()`, `.px_6()` — needs no call; adding one there would double
/// the trailing inset and pull the region off its alignment spine.
pub trait ScrollGutter: Styled + Sized {
    fn scroll_gutter(self) -> Self {
        self.pr(SCROLLBAR_GUTTER)
    }
}

impl<T: Styled> ScrollGutter for T {}

pub fn is_dark_system_appearance(cx: &App) -> bool {
    matches!(
        cx.window_appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// [`Empty`] paints a dashed border as soon as a border width is set. Vibex's
/// empty-state cards have always used a solid hairline, so keep that look while
/// the component owns the rest of the layout.
pub fn solid_empty_border(mut empty: Empty) -> Empty {
    empty.style().border_style = Some(BorderStyle::Solid);
    empty
}

/// Documentation pages a surface inside the product links to.
pub const DOCS_SELF_HOSTED_SERVER_URL: &str = "https://vibex.peatboy.com/docs/self-hosted-server";
pub const DOCS_REMOTE_MOBILE_URL: &str = "https://vibex.peatboy.com/docs/remote-mobile";

/// The help affordance every surface header shares: one glyph, one hit target.
/// The glyph is a child rather than `Button::icon`, so the frame owns the size
/// and the mark keeps the exact 14px the panel's other header controls use.
const DOCS_HELP_ICON: &str = "icons/vibex/circle-question-mark.svg";
const DOCS_HELP_BUTTON_SIZE: f32 = 24.0;
const DOCS_HELP_GLYPH_SIZE: f32 = 14.0;

/// Test hook for the laid-out message bounds.
const HINT_TEXT_DEBUG_KEY: &str = "hint-notification-text";
/// Element id of the copy button every hint carries.
const HINT_COPY_BUTTON_ID: &str = "hint-copy-button";
/// Test hook for the laid-out copy button.
const HINT_COPY_DEBUG_KEY: &str = "hint-notification-copy";

/// A light hint or error result on the notification layer that copies its whole
/// message in one click.
///
/// The kit paints a notification's `message` as plain text that no selection
/// layer can reach, and a drag across a line that is already fading is a poor
/// way to ask for the error text a user most often needs to paste somewhere
/// else. The message therefore travels as custom content, with one icon button
/// beside it that puts the message on the clipboard whole.
///
/// The surface stays the kit's `Notification`: placement, tone, autohide, and
/// the replace-by-id contract are unchanged.
///
/// The message is deliberately not also set through `Notification::message`,
/// which would paint it a second time under the content. Every hint the
/// workbench pushes is delivered in-app, so there is no system notification
/// body that needs the plain string.
pub fn hint_notification(tone: NotificationType, message: impl Into<SharedString>) -> Notification {
    let message = message.into();
    let copy_value = message.clone();
    let copy_label = locale::text("Copy message", "复制消息", "複製訊息");
    Notification::new()
        .with_type(tone)
        .content(move |_, _, _| {
            div()
                .text_sm()
                .w_full()
                .min_w_0()
                .debug_selector(|| HINT_TEXT_DEBUG_KEY.to_string())
                .child(message.clone())
                .into_any_element()
        })
        .action(move |_, _, _| {
            let copy_value = copy_value.clone();
            Button::new(HINT_COPY_BUTTON_ID)
                .ghost()
                .flex_none()
                .icon(IconName::Copy)
                .debug_selector(|| HINT_COPY_DEBUG_KEY.to_string())
                .accessibility_label(copy_label)
                .tooltip(copy_label)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_value.to_string()));
                    // Clicking the card dismisses the hint, and a copy is not a
                    // decision to close it: the click stops at the button so
                    // the message stays on screen to be read or copied again.
                    cx.stop_propagation();
                })
        })
        // The kit keeps an action toast open because it expects the action to
        // decide what happens next. A hint is still a hint and keeps the
        // lifetime its caller asked for, so the default is restored here and a
        // caller's `.autohide(false)` still wins.
        .autohide(true)
}

/// A quiet, icon-only help button that opens one documentation page.
///
/// `label` names the page for both the tooltip and AccessKit, because the icon
/// alone cannot say what pressing the button opens. The page is a compile-time
/// constant, so the external-open boundary can only reject it by programming
/// error; a header has no error lane, and the button stays available to retry.
pub fn docs_help_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    url: &'static str,
) -> Button {
    let label = label.into();
    let id: SharedString = id.into();
    Button::new(id)
        .small()
        .ghost()
        .compact()
        .flex_none()
        .size(px(DOCS_HELP_BUTTON_SIZE))
        .px_0()
        .tooltip(label.clone())
        .accessibility_label(label)
        .child(
            Icon::default()
                .path(DOCS_HELP_ICON)
                .size(px(DOCS_HELP_GLYPH_SIZE)),
        )
        .on_click(move |_, _, _| {
            let _ = validate_external_open_url(url)
                .and_then(|validated| open_external_url(&validated.url));
        })
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::*;

    /// An icon path that is not in the asset bundle resolves to nothing, with
    /// no error and no placeholder, so a missing registration would leave the
    /// help button invisible rather than broken.
    #[test]
    fn the_docs_help_icon_is_bundled() {
        let assets = crate::assets::VibexAssets;
        assert!(
            assets
                .load(DOCS_HELP_ICON)
                .expect("asset lookup should not fail")
                .is_some(),
            "the docs help button asks for {DOCS_HELP_ICON}, which is not in the asset bundle"
        );
    }

    /// The pages are constants, but they still cross the external-open
    /// boundary on click: a URL the boundary rejects is a button that silently
    /// does nothing.
    #[test]
    fn the_docs_help_urls_pass_the_external_open_boundary() {
        for url in [DOCS_SELF_HOSTED_SERVER_URL, DOCS_REMOTE_MOBILE_URL] {
            assert_eq!(
                validate_external_open_url(url)
                    .expect("a docs page should validate")
                    .url,
                url
            );
        }
    }
    /// Every scroll region has to keep the overlay bar off its own content.
    ///
    /// `overflow_y_scrollbar()` positions the bar against the scroll
    /// container's padding box, so a region whose trailing inset is under
    /// [`SCROLLBAR_GUTTER`] lets the bar cover its last column — a defect that
    /// is invisible in code review and only shows up in a screenshot. This
    /// walks every scroll region in the desktop crate and requires each one to
    /// reserve the gutter itself or to pad its trailing edge by at least that
    /// much already.
    ///
    /// A region whose content carries its own margins is listed in
    /// [`PADDED_BY_THEIR_CONTENT`] with the reason, so the exception is stated
    /// rather than silently skipped.
    #[test]
    fn scroll_regions_reserve_the_scrollbar_gutter() {
        /// Scroll regions whose children inset themselves by at least the
        /// gutter, so the region must not add a second one.
        const PADDED_BY_THEIR_CONTENT: [(&str, &str); 1] = [("office_surface.rs", "render")];

        /// Builder calls that leave at least [`SCROLLBAR_GUTTER`] on the
        /// trailing edge.
        const CLEARS_THE_BAR: [&str; 11] = [
            ".scroll_gutter()",
            ".p_3()",
            ".p_4()",
            ".p_5()",
            ".p_6()",
            ".p_8()",
            ".px_3()",
            ".px_4()",
            ".px_5()",
            ".px_6()",
            ".pr_3()",
        ];

        let mut offenders = Vec::new();
        for (name, source) in SCROLL_REGION_SOURCES {
            for (line, chain) in scroll_region_chains(source) {
                if PADDED_BY_THEIR_CONTENT
                    .iter()
                    .any(|(file, owner)| *file == name && enclosing_fn(source, line) == *owner)
                {
                    continue;
                }
                let reserved = CLEARS_THE_BAR.iter().any(|call| chain.contains(call))
                    || chain_has_wide_literal_padding(&chain);
                if !reserved {
                    offenders.push(format!("{name}:{} in {}", line, enclosing_fn(source, line)));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these scroll regions let the overlay scrollbar cover their content; \
             add `.scroll_gutter()` after their padding:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// Source files that contain scroll regions.
    const SCROLL_REGION_SOURCES: [(&str, &str); 8] = [
        ("app.rs", include_str!("../app.rs")),
        ("code_workbench.rs", include_str!("../code_workbench.rs")),
        (
            "local_history_import.rs",
            include_str!("../local_history_import.rs"),
        ),
        ("management.rs", include_str!("../management.rs")),
        ("native_content.rs", include_str!("../native_content.rs")),
        ("office_surface.rs", include_str!("../office_surface.rs")),
        ("pdf_surface.rs", include_str!("../pdf_surface.rs")),
        ("usage.rs", include_str!("../usage.rs")),
    ];

    /// Every `overflow_y_scrollbar()` builder chain, with its 1-based line.
    ///
    /// A chain is the run of `.method()` continuation lines around the call,
    /// which is where a caller's padding lives. Deliberately not "the whole
    /// statement": a statement can close several lines later and swallow the
    /// next region's padding, which would make this guard pass for the wrong
    /// reason.
    fn scroll_region_chains(source: &str) -> Vec<(usize, String)> {
        let lines: Vec<&str> = source.lines().collect();
        let mut chains = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            // A source-inspection assertion names the call without making one.
            if !line.contains("overflow_y_scrollbar()")
                || line.trim_start().starts_with("//")
                || line.contains("contains(")
            {
                continue;
            }
            let mut start = index;
            while start > 0 {
                let previous = lines[start - 1].trim();
                if previous.starts_with('.') || previous.starts_with("//") {
                    start -= 1;
                } else {
                    break;
                }
            }
            let mut end = index;
            while end + 1 < lines.len() {
                let next = lines[end + 1].trim();
                if next.starts_with('.') || next.starts_with("//") {
                    end += 1;
                } else {
                    break;
                }
            }
            chains.push((index + 1, lines[start..=end].join(" ")));
        }
        chains
    }

    /// The name of the function a line sits in, for a legible failure.
    fn enclosing_fn(source: &str, line: usize) -> String {
        let lines: Vec<&str> = source.lines().collect();
        for candidate in lines[..line.min(lines.len())].iter().rev() {
            let trimmed = candidate.trim_start();
            for prefix in ["fn ", "pub fn ", "pub(crate) fn ", "async fn "] {
                if let Some(rest) = trimmed.strip_prefix(prefix) {
                    return rest.split('(').next().unwrap_or(rest).to_string();
                }
            }
        }
        "<top level>".to_string()
    }

    /// Whether a chain pads its trailing edge with a literal of at least the
    /// gutter, e.g. `.pr(px(16.0))`.
    fn chain_has_wide_literal_padding(chain: &str) -> bool {
        for call in [".p(px(", ".px(px(", ".pr(px("] {
            let mut rest = chain;
            while let Some(at) = rest.find(call) {
                rest = &rest[at + call.len()..];
                let digits: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if digits.parse::<f32>().is_ok_and(|value| value >= 12.0) {
                    return true;
                }
            }
        }
        false
    }
}
