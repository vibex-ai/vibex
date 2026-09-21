use gpui::{
    App, BorderStyle, InteractiveElement, Interactivity, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled as _, Window, WindowAppearance,
    px,
};
use gpui_component::{
    Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    empty::Empty,
};
use vibex_desktop_runtime::validate_external_open_url;

use crate::platform::open_external_url;

#[derive(IntoElement)]
struct AccessibleButton(Button);

impl InteractiveElement for AccessibleButton {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.0.interactivity()
    }
}

impl StatefulInteractiveElement for AccessibleButton {}

impl RenderOnce for AccessibleButton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.0
    }
}

pub fn button_with_aria_label(button: Button, label: impl Into<SharedString>) -> impl IntoElement {
    AccessibleButton(button).aria_label(label)
}

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
}
