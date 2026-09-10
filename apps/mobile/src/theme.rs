//! Design tokens for the native mobile client.
//!
//! The palette resolves from the shared desktop token source
//! (`crates/vibex-ui` generated tokens) at render time through an appearance
//! mode, so the phone reads as the same product as the desktop shell in both
//! dark and light. Views must go through these accessors instead of
//! hardcoding colors.

use std::sync::atomic::{AtomicU8, Ordering};

use gpui::{App, Hsla, Rgba, Window, hsla, px, rgb};
use gpui_component::{Theme, ThemeMode as ComponentThemeMode};
use vibex_ui::{DARK_TOKENS, GpuiColorToken, LIGHT_TOKENS};

// ---------------------------------------------------------------------------
// Appearance mode
// ---------------------------------------------------------------------------

/// The mobile appearance preference. `System` resolves against the platform
/// at render time so the phone flips with the OS dark-mode toggle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppearanceMode {
    Light,
    Dark,
    #[default]
    System,
}

impl AppearanceMode {
    pub fn all() -> [AppearanceMode; 3] {
        [
            AppearanceMode::System,
            AppearanceMode::Light,
            AppearanceMode::Dark,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            AppearanceMode::System => "System",
            AppearanceMode::Light => "Light",
            AppearanceMode::Dark => "Dark",
        }
    }

    pub fn from_storage(value: &str) -> Option<AppearanceMode> {
        match value {
            "light" => Some(AppearanceMode::Light),
            "dark" => Some(AppearanceMode::Dark),
            "system" => Some(AppearanceMode::System),
            _ => None,
        }
    }

    pub fn to_storage(self) -> &'static str {
        match self {
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
            AppearanceMode::System => "system",
        }
    }
}

// Encoding fits the small persistence payload: index into `AppearanceMode::all`.
const MODE_SYSTEM: u8 = 0;
const MODE_LIGHT: u8 = 1;
const MODE_DARK: u8 = 2;

static APPEARANCE_MODE: AtomicU8 = AtomicU8::new(MODE_SYSTEM);

/// Applies the persisted appearance preference for this process. Called once
/// at startup before the first frame and whenever the user changes the
/// setting; every color accessor reads from here.
pub fn set_appearance_mode(mode: AppearanceMode) {
    let encoded = match mode {
        AppearanceMode::System => MODE_SYSTEM,
        AppearanceMode::Light => MODE_LIGHT,
        AppearanceMode::Dark => MODE_DARK,
    };
    APPEARANCE_MODE.store(encoded, Ordering::Relaxed);
}

pub fn appearance_mode() -> AppearanceMode {
    match APPEARANCE_MODE.load(Ordering::Relaxed) {
        MODE_LIGHT => AppearanceMode::Light,
        MODE_DARK => AppearanceMode::Dark,
        _ => AppearanceMode::System,
    }
}

// The System preference resolves against the platform appearance, which the
// app refreshes from its window-appearance observer (and once per frame while
// rendering, so a fresh launch lands on the right side before the first
// observer tick). Dark is the safe default for a cold read.
static SYSTEM_DARK: AtomicU8 = AtomicU8::new(1);

/// Records the platform appearance for the System preference. Called by the
/// app shell whenever the window appearance is known to have changed.
pub fn set_system_dark(dark: bool) {
    SYSTEM_DARK.store(dark as u8, Ordering::Relaxed);
}

/// Whether the window is currently dark. A stored Light/Dark preference wins;
/// the System preference tracks the platform appearance the same way the
/// desktop shell resolves `ThemeMode::System`.
pub fn is_dark() -> bool {
    match appearance_mode() {
        AppearanceMode::Light => false,
        AppearanceMode::Dark => true,
        AppearanceMode::System => SYSTEM_DARK.load(Ordering::Relaxed) == 1,
    }
}

/// Whether the stored preference can still flip at runtime (System mode).
pub fn follows_system() -> bool {
    appearance_mode() == AppearanceMode::System
}

// ---------------------------------------------------------------------------
// Shared token resolution
// ---------------------------------------------------------------------------

/// One step above the desktop lookup helper: a missing token is a build-time
/// contract with `tokens.json`, so a typo surfaces as a panic at first paint
/// instead of a silent wrong color.
pub fn semantic_color(name: &str, dark: bool) -> Hsla {
    let tokens: &[GpuiColorToken] = if dark { DARK_TOKENS } else { LIGHT_TOKENS };
    let token = tokens
        .iter()
        .copied()
        .find(|token| token.name == name)
        .unwrap_or_else(|| panic!("missing generated GPUI semantic token: {name}"));
    Hsla {
        a: token.alpha,
        ..rgb(token.rgb).into()
    }
}

fn tone(name: &str) -> Hsla {
    semantic_color(name, is_dark())
}

// ---------------------------------------------------------------------------
// Palette (dark values kept as the legacy fallback contract for tests)
// ---------------------------------------------------------------------------

/// Flattens a translucent token (for example the desktop's 10% white border)
/// over an opaque surface so callers that need a solid fill keep the same
/// contrast the alpha produced on the given background. `Rgba::blend`
/// composites its argument on top of the receiver, so the surface receives
/// the token. The blend must stay in `Rgba`: a `u32` round-trip through
/// `gpui::rgb` would reinterpret the `0xRRGGBBAA` layout as `0x00RRGGBB` and
/// turn the alpha byte into a full-blue channel.
fn flatten(token: Rgba, over: Rgba) -> Rgba {
    over.blend(token)
}

fn opaque(name: &str, surface: &str) -> Hsla {
    let dark = is_dark();
    let token = semantic_color(name, dark);
    if token.a >= 1.0 {
        return token;
    }
    let over = semantic_color(surface, dark);
    let flattened = flatten(Rgba::from(token), Rgba::from(over).alpha(1.0));
    flattened.into()
}

// ---------------------------------------------------------------------------
// Spacing and typography
// ---------------------------------------------------------------------------

pub const SPACING_XS: f32 = 4.0;
pub const SPACING_SM: f32 = 8.0;
pub const SPACING_MD: f32 = 12.0;
pub const SPACING_LG: f32 = 16.0;
pub const SPACING_XL: f32 = 20.0;

pub const FONT_APP_TITLE: f32 = 28.0;
pub const FONT_HEADING: f32 = 13.0;
pub const FONT_BODY: f32 = 12.0;
pub const FONT_DETAIL: f32 = 12.0;
pub const FONT_CAPTION: f32 = 11.0;
/// Metadata that must not compete with the caption it annotates (timestamps,
/// workspace paths, row counts).
pub const FONT_MICRO: f32 = 10.0;

// The sidebar preserves the desktop hierarchy at one denser mobile type step
// so a full project tree remains scannable on a phone.
pub const FONT_SIDEBAR_TITLE: f32 = 15.0;
pub const FONT_SIDEBAR_ROW: f32 = 13.0;
pub const FONT_SIDEBAR_META: f32 = 11.0;

pub const ICON_MD: f32 = 18.0;
pub const ICON_SM: f32 = 16.0;
pub const ICON_STATUS: f32 = 6.0;

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// The compact top bar stays below the native status-bar inset supplied by GPUI.
pub const HEADER_HEIGHT: f32 = 40.0;
pub const HEADER_BUTTON_SIZE: f32 = 40.0;
/// Drawer and overlay headers have a little more room for the compact action
/// cluster while the session page keeps the shorter workbench header.
pub const DRAWER_HEADER_HEIGHT: f32 = 52.0;
/// Minimum tappable edge for controls that are not header buttons.
pub const TOUCH_TARGET: f32 = 44.0;
/// Caps centered panel content so tablets do not stretch a phone layout.
pub const CARD_WIDTH: f32 = 300.0;

pub const RADIUS_CARD: f32 = 8.0;
pub const RADIUS_CONTROL: f32 = 6.0;

pub const DRAWER_ROW_HEIGHT: f32 = 52.0;
pub const SIDEBAR_ROW_HEIGHT: f32 = 44.0;

// ---------------------------------------------------------------------------
// Sidebar tree geometry
//
// These mirror the desktop's `SIDEBAR_*` constants in `apps/desktop/src/app.rs`
// so the phone renders the same project tree: same icon slots, same indents,
// same folder guides, same worktree card. Only the row height is the phone's
// own, because every desktop row is a hover target while every phone row is a
// touch target.
// ---------------------------------------------------------------------------

/// Horizontal padding of the row list (the desktop's `px_4`).
pub const SIDEBAR_LIST_PADDING: f32 = 16.0;
/// Leading slot that carries a project logo, a folder mark, or a worktree
/// status dot.
pub const SIDEBAR_ICON_SLOT: f32 = 30.0;
/// The slot hangs left of the list padding so its glyph optically aligns with
/// the section heading above the list.
pub const SIDEBAR_ICON_SLOT_OVERHANG: f32 = 8.0;
/// Gap between the leading slot and the row title.
pub const SIDEBAR_ICON_TITLE_GAP: f32 = 4.0;
/// Indent of a project's own sessions in the compact hierarchy.
pub const SIDEBAR_PROJECT_SESSION_INDENT: f32 = 12.0;
/// Indent of the sessions inside a worktree card.
pub const SIDEBAR_WORKSPACE_SESSION_INDENT: f32 = 20.0;
/// Indent of the rows filed inside a folder.
pub const SIDEBAR_FOLDER_CHILD_INDENT: f32 = 18.0;
/// Where a folder draws the guide line down its child column.
pub const SIDEBAR_FOLDER_GUIDE_OFFSET: f32 = 7.0;
/// A session row starts its content past the card overhang instead of hanging
/// into it the way project and worktree rows do.
pub const SIDEBAR_SESSION_CONTENT_INSET: f32 = 8.0;
/// Trailing metadata column of a session row (time and status). Keeping it
/// compact leaves more room for the session title.
pub const SIDEBAR_SESSION_META_WIDTH: f32 = 48.0;
/// Corner radius of a row's fill.
pub const SIDEBAR_ROW_RADIUS: f32 = 8.0;
/// Corner radius of the worktree card that wraps a worktree and its sessions.
pub const SIDEBAR_CARD_RADIUS: f32 = 10.0;
/// Right inset of the worktree card (the desktop's `pr(4)`).
pub const SIDEBAR_CARD_INSET: f32 = 4.0;
/// Agent mark on a session row.
pub const SIDEBAR_AGENT_LOGO_SIZE: f32 = 14.0;
/// Project and folder marks.
pub const SIDEBAR_PROJECT_LOGO_SIZE: f32 = 16.0;
/// Worktree and session status dot.
pub const SIDEBAR_STATUS_DOT: f32 = 8.0;
/// Desktop's compact spinner size for an active workspace or session.
pub const SIDEBAR_STATUS_ICON_SIZE: f32 = 12.0;
/// Unread-completion dot, which is deliberately smaller than a status dot.
pub const SIDEBAR_UNREAD_DOT: f32 = 7.0;

/// Indent per tree level, matching the desktop's nested sidebar spacing.
pub const SIDEBAR_INDENT: f32 = 14.0;
/// Width of the trailing sidebar menu column. Row movement uses a long press on
/// the body, so no separate drag affordance is reserved.
pub const SIDEBAR_ACTION_WIDTH: f32 = 34.0;
/// The menu is layered over the row wrapper, so row content needs only the
/// small visual inset before its icon rather than the whole menu hitbox width.
pub const SIDEBAR_ACTION_CONTENT_INSET: f32 = 8.0;
pub const DRAWER_ACTION_HEIGHT: f32 = 40.0;
pub const DRAWER_SECTION_HEIGHT: f32 = 40.0;
/// Horizontal travel before a page pan may be recognized as a drawer swipe.
/// Deliberately above the platform touch slop so ordinary taps and slow
/// vertical scrolls on the session screen never open a side page, while a
/// normal swipe still arms after roughly a centimeter of travel.
pub const DRAWER_DRAG_THRESHOLD: f32 = 12.0;
/// A pan whose vertical travel reaches this fraction of its horizontal travel
/// belongs to list scrolling, not to a drawer swipe.
pub const DRAWER_VERTICAL_CANCEL_RATIO: f32 = 1.6;
/// Fraction of the viewport a page must travel before settling on the next page.
pub const DRAWER_SNAP_TRAVEL_RATIO: f32 = 0.15;
/// Release-adjacent movement in the intended direction commits the transition.
pub const DRAWER_SNAP_COMMIT_DIRECTION_THRESHOLD: f32 = 2.0;
/// Reversing a transition requires a much clearer release-adjacent movement.
pub const DRAWER_SNAP_REVERSE_DIRECTION_THRESHOLD: f32 = 28.0;
/// A swipe only commits when its release velocity points at the target with at
/// least this magnitude (logical px/s), so slow drags rely on travel instead.
pub const DRAWER_SWIPE_VELOCITY: f32 = 600.0;
/// Ratio between horizontal and vertical release velocity for a fling to count
/// as a horizontal swipe.
pub const DRAWER_SWIPE_STRAIGHTNESS: f32 = 1.5;
pub const DRAWER_BACKDROP_OPACITY: f32 = 0.4;
pub const DRAWER_OPEN_ANIMATION_MS: u64 = 160;
pub const DRAWER_CLOSE_ANIMATION_MS: u64 = 100;

// ---------------------------------------------------------------------------
// Accessors
// ---------------------------------------------------------------------------

pub fn bg_primary() -> Hsla {
    tone("background")
}

pub fn bg_card() -> Hsla {
    tone("card")
}

pub fn bg_card_dim() -> Hsla {
    // Dense lists need the dimmed fill flattened over the page background so
    // nested rows keep one consistent contrast step in both appearances.
    opaque("secondary", "background")
}

pub fn bg_popover() -> Hsla {
    tone("popover")
}

pub fn bg_composer() -> Hsla {
    tone("composer-surface")
}

pub fn text_primary() -> Hsla {
    tone("foreground")
}

pub fn text_secondary() -> Hsla {
    // The desktop sidebar fades one foreground; the phone keeps the ladder
    // explicit. Secondary text is 78% of the foreground in both appearances.
    let mut color = tone("foreground");
    color.a = 0.78;
    color
}

pub fn text_muted() -> Hsla {
    tone("muted-foreground")
}

pub fn border_default() -> Hsla {
    opaque("border", "background")
}

pub fn border_subtle() -> Hsla {
    opaque("sidebar-border", "background")
}

pub fn border_input() -> Hsla {
    opaque("input", "background")
}

pub fn row_pressed_bg() -> Hsla {
    // Desktop parity (`theme::hover_wash`): dark paints a soft-white wash,
    // light the tone-flipped soft-black.
    let (luminance, alpha) = if is_dark() {
        (0.92, 0.11)
    } else {
        (0.10, 0.11)
    };
    hsla(0.0, 0.0, luminance, alpha)
}

pub fn row_active_bg() -> Hsla {
    // Desktop parity (`theme::active_wash`).
    let (luminance, alpha) = if is_dark() {
        (0.92, 0.16)
    } else {
        (0.10, 0.16)
    };
    hsla(0.0, 0.0, luminance, alpha)
}

pub fn accent() -> Hsla {
    tone("accent")
}

pub fn accent_foreground() -> Hsla {
    tone("accent-foreground")
}

pub fn primary() -> Hsla {
    tone("primary")
}

pub fn primary_foreground() -> Hsla {
    tone("primary-foreground")
}

/// Status and chart accents stay identical across both appearances, matching
/// the desktop right-rail tones.
pub fn accent_green() -> Hsla {
    tone("chart-2")
}

pub fn accent_blue() -> Hsla {
    tone("chart-category-1")
}

pub fn accent_yellow() -> Hsla {
    tone("warning")
}

pub fn accent_red() -> Hsla {
    tone("destructive")
}

pub fn accent_dim() -> Hsla {
    tone("muted-foreground")
}

pub fn accent_purple() -> Hsla {
    tone("chart-category-8")
}

/// Git change statuses share the desktop right-rail tones exactly.
pub fn status_added() -> Hsla {
    tone("right-rail-status-added")
}

pub fn status_modified() -> Hsla {
    tone("right-rail-status-modified")
}

pub fn status_untracked() -> Hsla {
    tone("right-rail-status-untracked")
}

/// Teal accent used for agent-account markers in the session-settings cascade.
pub fn accent_chart3() -> Hsla {
    tone("chart-3")
}

pub fn sidebar_drop_bg() -> Hsla {
    let mut color = accent_blue();
    color.a = 0.18;
    color
}

pub fn workbench_bg() -> Hsla {
    sidebar_bg()
}

pub fn workbench_panel_bg() -> Hsla {
    tone("card")
}

pub fn sidebar_bg() -> Hsla {
    tone("sidebar")
}

pub fn sidebar_selected_bg() -> Hsla {
    opaque("sidebar-accent", "sidebar")
}

pub fn sidebar_text_primary() -> Hsla {
    tone("sidebar-foreground")
}

pub fn sidebar_text_secondary() -> Hsla {
    let mut color = tone("sidebar-foreground");
    color.a = 0.78;
    color
}

pub fn sidebar_text_muted() -> Hsla {
    tone("muted-foreground")
}

/// The desktop's sidebar builds its contrast ladder by fading one foreground
/// colour rather than by swapping palette entries. Rows that copy a desktop
/// opacity go through here so the two trees land on the same shade.
pub fn sidebar_foreground(alpha: f32) -> Hsla {
    let mut color = tone("sidebar-foreground");
    color.a = alpha;
    color
}

pub fn sidebar_card_focus_border() -> Hsla {
    let mut color = tone("sidebar-ring");
    color.a = if is_dark() { 0.42 } else { 0.45 };
    color
}

/// Guide line down a folder's child column.
pub fn sidebar_tree_guide() -> Hsla {
    let mut color = opaque("sidebar-border", "sidebar");
    color.a = 0.70;
    color
}

pub fn backdrop(opacity: f32) -> Hsla {
    hsla(0.0, 0.0, 0.0, opacity)
}

// ---------------------------------------------------------------------------
// gpui-kit component theme
// ---------------------------------------------------------------------------

/// Point the gpui-kit global `Theme` at the same `vibex_ui` tokens the rest of
/// the phone renders from.
///
/// Without this bridge every kit component would paint from gpui-kit's own
/// default palette, which reads as a second design system bolted onto the
/// phone. The mapping mirrors the desktop shell's `theme::apply_appearance`
/// token for token, so a `Sheet`, `Input`, or `Button` renders in the product's
/// colors instead of the framework's.
///
/// Call this on the appearance mode changing and once before the first window
/// paints.
pub fn apply_component_theme(window: Option<&mut Window>, cx: &mut App) {
    let dark = is_dark();
    Theme::change(
        if dark {
            ComponentThemeMode::Dark
        } else {
            ComponentThemeMode::Light
        },
        window,
        cx,
    );

    let theme = Theme::global_mut(cx);
    let token = |name: &str| semantic_color(name, dark);

    // The phone runs one step denser than the desktop shell; keep kit controls
    // on the compact end so they sit in the same rhythm as the hand-built rows.
    theme.font_family = "IBM Plex Sans".into();
    theme.font_size = px(15.0);
    theme.mono_font_size = px(13.0);
    theme.radius = px(RADIUS_CONTROL);
    theme.radius_lg = px(RADIUS_CARD);

    let background = token("background");
    let foreground = token("foreground");
    let secondary = token("secondary");
    let muted = token("muted");
    let muted_foreground = token("muted-foreground");
    let border = token("border");
    let input = token("input");
    let sidebar = token("sidebar");
    let hover = row_pressed_bg();
    let active = row_active_bg();

    theme.background = background;
    theme.tokens.background = background.into();
    theme.foreground = foreground;
    theme.tokens.foreground = foreground.into();
    theme.secondary = secondary;
    theme.tokens.secondary = secondary.into();
    theme.secondary_foreground = token("secondary-foreground");
    theme.tokens.secondary_foreground = theme.secondary_foreground.into();
    theme.muted = muted;
    theme.tokens.muted = muted.into();
    theme.muted_foreground = muted_foreground;
    theme.tokens.muted_foreground = muted_foreground.into();
    theme.primary = token("primary");
    theme.tokens.primary = theme.primary.into();
    theme.primary_foreground = token("primary-foreground");
    theme.tokens.primary_foreground = theme.primary_foreground.into();
    theme.border = border;
    theme.tokens.border = border.into();
    theme.input = input;
    theme.tokens.input = input.into();
    theme.ring = token("ring");
    theme.tokens.ring = theme.ring.into();
    theme.sidebar = sidebar;
    theme.tokens.sidebar = sidebar.into();
    theme.sidebar_foreground = token("sidebar-foreground");
    theme.tokens.sidebar_foreground = theme.sidebar_foreground.into();
    theme.sidebar_border = token("sidebar-border");
    theme.tokens.sidebar_border = theme.sidebar_border.into();
    theme.accent = token("accent");
    theme.tokens.accent = theme.accent.into();
    theme.accent_foreground = token("accent-foreground");
    theme.tokens.accent_foreground = theme.accent_foreground.into();
    // Hover and pressed plates. The phone's washes are translucent by design
    // (`row_pressed_bg`), so the kit picks up the same wash the hand-built rows
    // already paint.
    theme.list_hover = hover;
    theme.tokens.list_hover = hover.into();
    theme.table_hover = hover;
    theme.tokens.table_hover = hover.into();
    theme.secondary_hover = hover;
    theme.tokens.secondary_hover = hover.into();
    theme.button_hover = hover;
    theme.tokens.button_hover = hover.into();
    theme.button_active = active;
    theme.tokens.button_active = active.into();
    theme.primary_hover = token("primary");
    theme.tokens.primary_hover = theme.primary_hover.into();
    theme.primary_active = token("primary");
    theme.tokens.primary_active = theme.primary_active.into();
    // The scrim darkens what is behind it, so light mode runs about half.
    theme.overlay = gpui::black().opacity(if dark { 0.60 } else { 0.32 });
    theme.title_bar = sidebar;
    theme.tokens.title_bar = sidebar.into();
    theme.title_bar_border = token("sidebar-border");
    theme.tokens.title_bar_border = theme.title_bar_border.into();
}

#[cfg(test)]
mod tests {
    use super::*;

    // One combined test: the appearance preference lives in a process-global,
    // so parallel test threads would race each other's mode.
    #[test]
    fn appearance_preference_resolves_the_shared_palette() {
        set_appearance_mode(AppearanceMode::Light);
        assert!(!is_dark());
        assert_eq!(
            bg_primary(),
            semantic_color("background", false),
            "light background must resolve from the shared token source"
        );
        assert_eq!(bg_primary().a, 1.0);

        set_appearance_mode(AppearanceMode::Dark);
        assert!(is_dark());
        assert_eq!(
            bg_primary(),
            semantic_color("background", true),
            "dark background must resolve from the shared token source"
        );

        // Dark border is white at 10% over #0d0d0d: flattening lifts the
        // surface to ~#252525 and must stay achromatic. A stray blue channel
        // here means the blend order or a hex round-trip corrupted the
        // channels (the bug that painted every dark-mode divider blue).
        let over = semantic_color("background", true);
        let flattened = flatten(
            Rgba::from(semantic_color("border", true)),
            Rgba::from(over).alpha(1.0),
        );
        assert_eq!(flattened.a, 1.0);
        let lifted = 0.1 + 0.9 * (0x0d as f32 / 255.0);
        for channel in [flattened.r, flattened.g, flattened.b] {
            assert!(
                (channel - lifted).abs() < 1e-3,
                "flattened dark border must stay an achromatic wash"
            );
        }
        assert_eq!(Rgba::from(border_default()), flattened);
        // The opaque accessors never leak alpha.
        assert_eq!(border_default().a, 1.0);
        assert_eq!(border_subtle().a, 1.0);
        assert_eq!(bg_card_dim().a, 1.0);
        assert_eq!(sidebar_selected_bg().a, 1.0);

        // Status tones match the desktop right-rail values.
        assert_eq!(accent_red(), semantic_color("destructive", true));
        assert_eq!(accent_yellow(), semantic_color("warning", true));
        assert_eq!(accent_blue(), semantic_color("chart-category-1", true));

        // Storage encoding round-trips every mode.
        for mode in AppearanceMode::all() {
            set_appearance_mode(mode);
            assert_eq!(appearance_mode(), mode);
            assert_eq!(AppearanceMode::from_storage(mode.to_storage()), Some(mode));
        }
        assert_eq!(AppearanceMode::from_storage("nope"), None);
        set_appearance_mode(AppearanceMode::System);
    }
}
