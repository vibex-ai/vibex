use gpui::{
    AnyElement, App, BorderStyle, Element, Hsla, InteractiveElement, Interactivity, IntoElement,
    ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement, StyleRefinement,
    Styled, Window, WindowAppearance, div, px,
};
use gpui_base::Selectable;
use gpui_component::{
    Icon, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    empty::Empty,
    scroll::ScrollbarAxis,
};
use vibex_desktop_runtime::validate_external_open_url;

use crate::platform::open_external_url;

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

/// Pins an overlay scrollbar to its scroll viewport.
///
/// `ScrollableElement::scrollbar` adds the bar as one more *child* of the
/// element it is called on. When that element is the scroll region itself, the
/// bar ends up inside the node whose content is being moved: GPUI applies the
/// region's scroll offset to every descendant during prepaint, so the bar
/// travels with the rows it is supposed to be tracking. The thumb still resizes
/// correctly, because it reads the handle rather than its own bounds, but the
/// whole bar slides off the top of the viewport as soon as the reader scrolls.
///
/// This helper keeps the scroller as the real scroll area and gives the bar a
/// stationary home: an outer box that copies the scroller's own layout style
/// (`flex_1`, `size_full`, `align_self`, …) so the region still occupies the
/// same slot, and the scroller fills that box. The bar is attached to the outer
/// box, which never scrolls, so it stays put.
///
/// The box is a flex container because the scroller's percentage height then
/// resolves against a definite parent. That matters for content-sized panels:
/// a plain block whose only child is `h_full` collapses to zero, which is how
/// the previous attempts at this fix lost their height.
pub fn pinned_scrollbar<E>(
    scroller: E,
    handle: &(impl gpui_base::ScrollbarHandle + Clone),
    axis: ScrollbarAxis,
) -> AnyElement
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    PinnedScrollbarBox::new(scroller, handle, axis).into_any_element()
}

/// The box [`pinned_scrollbar`] wraps a scroller in.
///
/// The scroller stays the real scroll area, which is why every builder call
/// made *after* `.pinned_scrollbar(...)` is forwarded to it rather than to the
/// outer box. A call site keeps chaining as if the call had not happened — the
/// padding, the gap, the hover target and the rows all describe the list — so
/// forwarding is what makes the one-line swap a faithful replacement for the
/// original chain.
///
/// Forwarding the children is not a convenience. Appending them to the outer
/// box instead puts the new rows *beside* the scroll region rather than inside
/// it: the region keeps only the children it had at the call, those rows paint
/// below the clipped viewport, and the tracked handle never grows, so the list
/// stops scrolling exactly when it has the most to show. That is the defect
/// this box exists to prevent.
pub struct PinnedScrollbarBox<E> {
    /// The stationary viewport: the scroller's layout slot plus the bar.
    outer: gpui::Stateful<gpui::Div>,
    scroller: E,
    bar: Option<gpui_base::Scrollbar>,
}

impl<E> PinnedScrollbarBox<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    #[track_caller]
    fn new(
        mut scroller: E,
        handle: &(impl gpui_base::ScrollbarHandle + Clone),
        axis: ScrollbarAxis,
    ) -> Self {
        let source = scroller.style().clone();
        let outer_style = StyleRefinement {
            size: source.size,
            min_size: source.min_size,
            max_size: source.max_size,
            flex_grow: source.flex_grow,
            flex_shrink: source.flex_shrink,
            flex_basis: source.flex_basis,
            align_self: source.align_self,
            ..Default::default()
        };

        // One id per call site, so two pinned regions under the same parent do
        // not share element state.
        let site = std::panic::Location::caller();
        let id = gpui::ElementId::CodeLocation(*site);
        let outer = div()
            .id(id.clone())
            .relative()
            .flex()
            .min_h_0()
            .min_w_0()
            .refine_style(&outer_style);
        let bar = gpui_base::Scrollbar::new(handle)
            .id(id)
            .axis(axis)
            .viewport_from_layout();
        Self {
            outer,
            scroller: scroller.flex_1().min_h_0().min_w_0(),
            bar: Some(bar),
        }
    }
}

/// Style calls after the pin belong to the list, not to its viewport.
impl<E> Styled for PinnedScrollbarBox<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    fn style(&mut self) -> &mut StyleRefinement {
        self.scroller.style()
    }
}

/// Interactivity after the pin belongs to the list too: a hover or a wheel
/// handler added by a call site is about the rows.
impl<E> InteractiveElement for PinnedScrollbarBox<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    fn interactivity(&mut self) -> &mut Interactivity {
        self.scroller.interactivity()
    }
}

impl<E> StatefulInteractiveElement for PinnedScrollbarBox<E> where
    E: InteractiveElement + Styled + ParentElement + Element + 'static
{
}

impl<E> ParentElement for PinnedScrollbarBox<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.scroller.extend(elements);
    }
}

impl<E> IntoElement for PinnedScrollbarBox<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    type Element = gpui::ViewElement<Self>;

    fn into_element(self) -> Self::Element {
        gpui::ViewElement::new(self)
    }
}

impl<E> RenderOnce for PinnedScrollbarBox<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    fn render(mut self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let scroller = self.scroller;
        let mut outer = self.outer.child(scroller);
        if let Some(bar) = self.bar.take() {
            // The overlay is a sibling placed after the scroller, so it paints
            // over the rows. `inset_0` pins it to the viewport while
            // `viewport_from_layout` makes it measure against the same box,
            // which is what keeps the thumb fixed as the content moves.
            outer = outer.child(
                div()
                    .absolute()
                    .inset_0()
                    .debug_selector(|| "scrollbar-overlay".to_string())
                    .child(bar),
            );
        }
        outer
    }
}

/// [`pinned_scrollbar`] as a builder method, so a call site swaps one line.
///
/// Replacing `.vertical_scrollbar(&handle)` with `.pinned_scrollbar(&handle,
/// ScrollbarAxis::Vertical)` keeps the rest of the chain — and its indentation —
/// untouched, which is what makes the fix readable in review: the diff shows the
/// bar moving out of the scroller, not a re-indented block.
///
/// The returned [`PinnedScrollbarBox`] forwards `child`/`children` to the
/// scroller, so a chain can keep adding rows after it and they land in the
/// region the bar tracks.
pub trait PinnedScrollbar: InteractiveElement + Styled + ParentElement + Sized {
    /// The same bar [`pinned_scrollbar`] draws, pinned to this element's box.
    fn pinned_scrollbar<H>(self, handle: &H, axis: ScrollbarAxis) -> PinnedScrollbarBox<Self>
    where
        H: gpui_base::ScrollbarHandle + Clone,
        Self: Element + 'static,
    {
        PinnedScrollbarBox::new(self, handle, axis)
    }
}

impl<E> PinnedScrollbar for E where E: InteractiveElement + Styled + ParentElement {}

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

/// The 1px cue a folder tab strip paints along its bottom edge while a tab is
/// dragged over the group.
///
/// The cue is an overlay rather than a border on the strip. A border owns a
/// layout row, and the strip paints its own plate across that row, so the
/// selected tab stops one pixel short of the pane and a hairline of plate
/// separates the two — the tab no longer flows into the surface it opens.
/// Painting the cue over the strip keeps the drop target just as visible
/// without reserving any space.
pub fn tab_strip_drop_cue(color: Hsla) -> AnyElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(1.0))
        .bg(color)
        .into_any_element()
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
        scroll_chains_containing(source, "overflow_y_scrollbar()")
    }

    /// Every builder chain containing `needle`, with the 1-based line where
    /// `needle` sits.
    fn scroll_chains_containing(source: &str, needle: &str) -> Vec<(usize, String)> {
        let lines: Vec<&str> = source.lines().collect();
        let mut chains = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            // A source-inspection assertion names the call without making one.
            if !line.contains(needle)
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

    /// The overlay bar must never be attached to the node it scrolls.
    ///
    /// GPUI applies a scroll region's offset to every descendant during
    /// prepaint. A `vertical_scrollbar`/`scrollbar` call on that same builder
    /// chain therefore puts the bar *inside* the moving content: the thumb's
    /// length stays right, because it reads the handle, but the bar itself
    /// travels upward with the rows and leaves the viewport. The symptom is a
    /// scrollbar that sits at the top (or slides out of sight) while the page
    /// below it scrolls.
    ///
    /// This walks every scroll-region source in the desktop crate and fails on
    /// any chain that both scrolls and carries the bar itself. The pinned form
    /// is [`pinned_scrollbar`], and the `overflow_*_scrollbar()` builders are
    /// fine because `Scrollable` makes the bar a sibling of the scrolling area.
    #[test]
    fn scrollbars_are_never_inside_the_node_they_scroll() {
        /// Every file that owns a scroll region.
        const SOURCES: [(&str, &str); 12] = [
            ("app.rs", include_str!("../app.rs")),
            ("code_workbench.rs", include_str!("../code_workbench.rs")),
            (
                "directory_picker.rs",
                include_str!("../directory_picker.rs"),
            ),
            (
                "local_history_import.rs",
                include_str!("../local_history_import.rs"),
            ),
            ("management.rs", include_str!("../management.rs")),
            (
                "management/provider_workspace/mod.rs",
                include_str!("../management/provider_workspace/mod.rs"),
            ),
            (
                "management/provider_workspace/route_group.rs",
                include_str!("../management/provider_workspace/route_group.rs"),
            ),
            ("native_content.rs", include_str!("../native_content.rs")),
            ("office_surface.rs", include_str!("../office_surface.rs")),
            ("pdf_surface.rs", include_str!("../pdf_surface.rs")),
            ("usage.rs", include_str!("../usage.rs")),
            ("arena/mod.rs", include_str!("../arena/mod.rs")),
        ];

        let mut offenders = Vec::new();
        for (name, source) in SOURCES {
            for needle in [
                ".vertical_scrollbar(",
                ".horizontal_scrollbar(",
                ".scrollbar(",
            ] {
                for (line, chain) in scroll_chains_containing(source, needle) {
                    if chain.contains(".track_scroll(")
                        || chain.contains(".overflow_y_scroll()")
                        || chain.contains(".overflow_x_scroll()")
                        || chain.contains(".overflow_scroll()")
                    {
                        offenders.push(format!(
                            "{name}:{line} in {} attaches a scrollbar and scrolls the same node",
                            enclosing_fn(source, line)
                        ));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these scroll regions attach the overlay bar to the node it scrolls, \
             so the bar rides up with the content; wrap them in \
             `pinned_scrollbar(...)` instead:\n  {}",
            offenders.join("\n  ")
        );
    }

    mod pinned_scrollbar_probe {
        use gpui::{
            Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
            ScrollHandle, StatefulInteractiveElement as _, Styled as _, TestAppContext, Window,
            div, prelude::*, px,
        };
        use gpui_component::v_flex;

        use super::super::{PinnedScrollbar as _, pinned_scrollbar};

        /// A content-sized panel whose body scrolls: the shape that used to let
        /// the bar ride up with the rows.
        struct Probe {
            scroll: ScrollHandle,
        }

        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let rows = (0..20)
                    .map(|ix| {
                        div().h(px(20.0)).flex_none().when(ix == 19, |row| {
                            row.debug_selector(|| "pinned-last-row".to_string())
                        })
                    })
                    .collect::<Vec<_>>();
                let body = v_flex()
                    .id("pinned-probe-scroll")
                    .w_full()
                    .min_h_0()
                    .track_scroll(&self.scroll)
                    .overflow_y_scroll()
                    .children(rows);
                v_flex()
                    .w(px(240.0))
                    .max_h(px(160.0))
                    .min_h_0()
                    .overflow_hidden()
                    .p_2()
                    .child(div().h(px(20.0)).flex_none())
                    .child(pinned_scrollbar(
                        body,
                        &self.scroll,
                        gpui_component::scroll::ScrollbarAxis::Vertical,
                    ))
                    .child(
                        div()
                            .debug_selector(|| "pinned-probe-footer".to_string())
                            .h(px(20.0))
                            .flex_none(),
                    )
            }
        }

        /// The bar stays over the viewport while the content moves under it.
        ///
        /// This is the regression the fix exists for: with the bar attached to
        /// the scrolling node, its overlay travelled from y=28 to y=-92 during
        /// the same wheel event that scrolled the rows.
        #[gpui::test]
        fn the_pinned_bar_stays_put_while_the_content_scrolls(cx: &mut TestAppContext) {
            cx.update(gpui_component::init);
            let (_, cx) = cx.add_window_view(|_, _| Probe {
                scroll: ScrollHandle::new(),
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            cx.run_until_parked();

            let before = cx
                .debug_bounds("scrollbar-overlay")
                .expect("the region should own a scrollbar");
            let row_before = cx
                .debug_bounds("pinned-last-row")
                .expect("the last row should be laid out")
                .origin
                .y;

            cx.simulate_event(gpui::ScrollWheelEvent {
                position: gpui::point(px(40.0), px(40.0)),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-120.0))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });

            let after = cx
                .debug_bounds("scrollbar-overlay")
                .expect("the region should still own a scrollbar");
            let row_after = cx
                .debug_bounds("pinned-last-row")
                .expect("the last row should still be laid out")
                .origin
                .y;

            assert!(
                row_after < row_before,
                "the wheel should move the content: {row_before:?} -> {row_after:?}"
            );
            assert_eq!(
                before.origin, after.origin,
                "the bar must stay on the viewport while the content scrolls"
            );
        }

        /// The shape every call site actually uses: the list keeps chaining
        /// after `.pinned_scrollbar(...)`, so its rows are appended to the
        /// value the builder returns.
        struct ChainedProbe {
            scroll: ScrollHandle,
        }

        impl Render for ChainedProbe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                v_flex()
                    .w(px(240.0))
                    .max_h(px(160.0))
                    .min_h_0()
                    .overflow_hidden()
                    .p_2()
                    .child(
                        v_flex()
                            .id("chained-probe-scroll")
                            .debug_selector(|| "chained-probe-scroll".to_string())
                            .w_full()
                            .min_h_0()
                            .track_scroll(&self.scroll)
                            .overflow_y_scroll()
                            .pinned_scrollbar(
                                &self.scroll,
                                gpui_component::scroll::ScrollbarAxis::Vertical,
                            )
                            .children((0..20).map(|ix| {
                                div().h(px(20.0)).flex_none().when(ix == 19, |row| {
                                    row.debug_selector(|| "chained-last-row".to_string())
                                })
                            })),
                    )
            }
        }

        /// A row appended after the pin belongs to the scroll area.
        ///
        /// The builder used to return the wrapper, so those rows landed beside
        /// the viewport instead of inside it: they painted below the clipping
        /// box, the tracked handle never grew, and the menu could not scroll.
        #[gpui::test]
        fn rows_appended_after_the_pin_scroll_with_the_list(cx: &mut TestAppContext) {
            cx.update(gpui_component::init);
            let (_, cx) = cx.add_window_view(|_, _| ChainedProbe {
                scroll: ScrollHandle::new(),
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            cx.run_until_parked();

            let viewport = cx
                .debug_bounds("chained-probe-scroll")
                .expect("the list should be laid out");
            let row_before = cx
                .debug_bounds("chained-last-row")
                .expect("the appended row should be laid out")
                .origin
                .y;
            assert!(
                cx.debug_bounds("scrollbar-overlay").is_some(),
                "the pinned region should still own its bar"
            );

            cx.simulate_event(gpui::ScrollWheelEvent {
                position: gpui::point(px(40.0), px(40.0)),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-120.0))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });

            let row_after = cx
                .debug_bounds("chained-last-row")
                .expect("the appended row should still be laid out")
                .origin
                .y;
            assert!(
                row_after < row_before,
                "the appended rows must be inside the scroll area: \
                 {row_before:?} -> {row_after:?}"
            );
            assert!(
                row_after >= viewport.origin.y,
                "the last row must not sit above the region that clips it"
            );
        }
    }
}
