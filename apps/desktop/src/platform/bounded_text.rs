//! Linux-only bounded font stack.
//!
//! `gpui-pre-linux` builds its text system with `CosmicTextSystem::new`, which
//! enumerates every font installed on the machine. Vibex deliberately keeps the
//! process font database bounded — the bundled families plus the few system
//! faces the UI actually asks for — so this module keeps that policy in the
//! application by wrapping the platform's text system instead of patching GPUI.
//!
//! The wrapper also resolves the generic families the UI names directly
//! (`monospace`, `sans-serif`) to bundled families: a bounded database has no
//! fontconfig behind it to answer them, so `font_id` would otherwise fail.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use futures_channel::oneshot;
use gpui::{
    Action, ActivityGuard, AnyWindowHandle, AppLifecyclePhase, BackgroundExecutor, Bounds,
    ClipboardItem, ClipboardReadError, CursorStyle, DevicePixels, Font, FontFallbacks, FontId,
    FontMetrics, FontRun, ForegroundExecutor, GlyphId, Hsla, Keymap, LineLayout, Menu, MenuItem,
    OwnedMenu, PathPromptOptions, Pixels, Platform, PlatformDisplay, PlatformGestures,
    PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem, PlatformWindow,
    RenderGlyphParams, ScreenCaptureSource, Size, SystemNotification, SystemNotificationResponse,
    Task, TextRenderingMode, ThermalState, WindowAppearance, WindowButtonLayout, WindowParams,
};
use gpui_wgpu::CosmicTextSystem;
use smallvec::SmallVec;

/// The family GPUI falls back to when a requested family is unavailable.
const SYSTEM_FONT_FALLBACK: &str = "IBM Plex Sans";
/// What the UI means by `monospace`.
const BUNDLED_CODE_FAMILY: &str = "Lilex";
/// System families the UI relies on that are not bundled: emoji and the
/// symbol/fallback coverage DejaVu provides.
const REQUIRED_SYSTEM_FONT_FAMILIES: &[&str] = &["Noto Color Emoji", "DejaVu Sans"];

/// Wraps the platform's text system so the font database stays bounded and the
/// generic family names resolve to bundled families.
struct BoundedTextSystem {
    inner: CosmicTextSystem,
}

impl BoundedTextSystem {
    /// Resolves the family names the UI passes in for generic families. Mirrors
    /// what the bundled font set can actually answer: Lilex is the bundled
    /// monospace face, and the system fallback is the bundled sans face.
    fn mapped_family(name: &str) -> &str {
        if name.eq_ignore_ascii_case("monospace") {
            BUNDLED_CODE_FAMILY
        } else if name.eq_ignore_ascii_case("sans-serif") {
            SYSTEM_FONT_FALLBACK
        } else {
            name
        }
    }

    fn mapped_font(font: &Font) -> Font {
        let mut mapped = font.clone();
        mapped.family = Self::mapped_family(&font.family).into();
        if let Some(fallbacks) = font.fallbacks.as_ref() {
            mapped.fallbacks = Some(FontFallbacks::from_fonts(
                fallbacks
                    .fallback_list()
                    .iter()
                    .map(|family| Self::mapped_family(family).to_string())
                    .collect(),
            ));
        }
        mapped
    }
}

impl PlatformTextSystem for BoundedTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.inner.add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        self.inner.all_font_names()
    }

    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        self.inner.font_id(&Self::mapped_font(descriptor))
    }

    fn prewarm_fonts(&self, font_ids: &[FontId]) {
        self.inner.prewarm_fonts(font_ids)
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        self.inner.font_metrics(font_id)
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        self.inner.typographic_bounds(font_id, glyph_id)
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.inner.advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.inner.glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.inner.glyph_raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.inner.rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.inner.layout_line(text, font_size, runs)
    }

    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        self.inner.recommended_rendering_mode(font_id, font_size)
    }

    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.inner.glyph_dilation_for_color(color)
    }
}

/// Delegates every [`Platform`] call to the real Linux platform, replacing only
/// the text system.
struct BoundedFontPlatform {
    inner: Rc<dyn Platform>,
    text_system: Arc<dyn PlatformTextSystem>,
}

impl Platform for BoundedFontPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.inner.background_executor()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.inner.foreground_executor()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        self.inner.run(on_finish_launching)
    }

    fn quit(&self) {
        self.inner.quit()
    }

    fn restart(&self, binary_path: Option<PathBuf>, arguments: Vec<std::ffi::OsString>) {
        self.inner.restart(binary_path, arguments)
    }

    fn activate(&self, ignoring_other_apps: bool) {
        self.inner.activate(ignoring_other_apps)
    }

    fn hide(&self) {
        self.inner.hide()
    }

    fn hide_other_apps(&self) {
        self.inner.hide_other_apps()
    }

    fn unhide_other_apps(&self) {
        self.inner.unhide_other_apps()
    }

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        self.inner.displays()
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.inner.primary_display()
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.inner.active_window()
    }

    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        self.inner.window_stack()
    }

    fn is_screen_capture_supported(&self) -> bool {
        self.inner.is_screen_capture_supported()
    }

    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<anyhow::Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
        self.inner.screen_capture_sources()
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        self.inner.open_window(handle, options)
    }

    fn window_appearance(&self) -> WindowAppearance {
        self.inner.window_appearance()
    }

    fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        self.inner.set_window_appearance(appearance)
    }

    fn button_layout(&self) -> Option<WindowButtonLayout> {
        self.inner.button_layout()
    }

    fn open_url(&self, url: &str) {
        self.inner.open_url(url)
    }

    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.inner.on_open_urls(callback)
    }

    fn register_url_scheme(&self, url: &str) -> Task<Result<()>> {
        self.inner.register_url_scheme(url)
    }

    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        self.inner.prompt_for_paths(options)
    }

    fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        self.inner.prompt_for_new_path(directory, suggested_name)
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        self.inner.can_select_mixed_files_and_dirs()
    }

    fn reveal_path(&self, path: &Path) {
        self.inner.reveal_path(path)
    }

    fn open_with_system(&self, path: &Path) {
        self.inner.open_with_system(path)
    }

    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        self.inner.on_quit(callback)
    }

    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_reopen(callback)
    }

    fn on_system_sleep(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_system_sleep(callback)
    }

    fn on_system_wake(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_system_wake(callback)
    }

    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        self.inner.on_app_lifecycle(callback)
    }

    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_memory_warning(callback)
    }

    fn gestures(&self) -> Option<Rc<dyn PlatformGestures>> {
        self.inner.gestures()
    }

    fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap) {
        self.inner.set_menus(menus, keymap)
    }

    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        self.inner.get_menus()
    }

    fn set_dock_menu(&self, menu: Vec<MenuItem>, keymap: &Keymap) {
        self.inner.set_dock_menu(menu, keymap)
    }

    fn perform_dock_menu_action(&self, action: usize) {
        self.inner.perform_dock_menu_action(action)
    }

    fn add_recent_document(&self, path: &Path) {
        self.inner.add_recent_document(path)
    }

    fn update_jump_list(
        &self,
        menus: Vec<MenuItem>,
        entries: Vec<SmallVec<[PathBuf; 2]>>,
    ) -> Task<Vec<SmallVec<[PathBuf; 2]>>> {
        self.inner.update_jump_list(menus, entries)
    }

    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        self.inner.on_app_menu_action(callback)
    }

    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_will_open_app_menu(callback)
    }

    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        self.inner.on_validate_app_menu_command(callback)
    }

    fn thermal_state(&self) -> ThermalState {
        self.inner.thermal_state()
    }

    fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_thermal_state_change(callback)
    }

    fn prevent_idle_sleep(&self, reason: &str) -> Task<Result<ActivityGuard>> {
        self.inner.prevent_idle_sleep(reason)
    }

    fn set_app_identity(&self, identifier: &str, name: &str) {
        self.inner.set_app_identity(identifier, name)
    }

    fn show_system_notification(&self, notification: SystemNotification) {
        self.inner.show_system_notification(notification)
    }

    fn dismiss_system_notification(&self, tag: &str) {
        self.inner.dismiss_system_notification(tag)
    }

    fn on_system_notification_response(
        &self,
        callback: Box<dyn FnMut(SystemNotificationResponse)>,
    ) {
        self.inner.on_system_notification_response(callback)
    }

    fn compositor_name(&self) -> &'static str {
        self.inner.compositor_name()
    }

    fn app_path(&self) -> Result<PathBuf> {
        self.inner.app_path()
    }

    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        self.inner.path_for_auxiliary_executable(name)
    }

    fn set_cursor_style(&self, style: CursorStyle) {
        self.inner.set_cursor_style(style)
    }

    fn hide_cursor_until_mouse_moves(&self) {
        self.inner.hide_cursor_until_mouse_moves()
    }

    fn is_cursor_visible(&self) -> bool {
        self.inner.is_cursor_visible()
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        self.inner.should_auto_hide_scrollbars()
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.inner.read_from_clipboard()
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        self.inner.write_to_clipboard(item)
    }

    fn read_from_clipboard_async(&self) -> Task<Result<Option<ClipboardItem>, ClipboardReadError>> {
        self.inner.read_from_clipboard_async()
    }

    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.inner.read_from_primary()
    }

    fn write_to_primary(&self, item: ClipboardItem) {
        self.inner.write_to_primary(item)
    }

    fn write_credentials(&self, url: &str, username: &str, password: &[u8]) -> Task<Result<()>> {
        self.inner.write_credentials(url, username, password)
    }

    fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        self.inner.read_credentials(url)
    }

    fn delete_credentials(&self, url: &str) -> Task<Result<()>> {
        self.inner.delete_credentials(url)
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        self.inner.keyboard_layout()
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        self.inner.keyboard_mapper()
    }

    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_keyboard_layout_change(callback)
    }
}

/// Asks fontconfig which file backs `family`, refusing a substitution: a
/// fallback match would silently pull in a different face than requested.
fn resolved_system_font_data(family: &str) -> Option<Vec<u8>> {
    let output = Command::new("fc-match")
        .args(["--format=%{family}\t%{file}\n", "--", family])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (matched_family, path) = stdout.lines().next()?.split_once('\t')?;
    let family_matches = matched_family
        .split(',')
        .any(|candidate| candidate.trim().eq_ignore_ascii_case(family.trim()));
    if !family_matches {
        return None;
    }

    std::fs::read(path.trim()).ok()
}

fn bounded_text_system() -> BoundedTextSystem {
    let text_system = CosmicTextSystem::new_without_system_fonts(SYSTEM_FONT_FALLBACK);

    let mut fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(crate::assets::IBM_PLEX_SANS_REGULAR),
        Cow::Borrowed(crate::assets::IBM_PLEX_SANS_ITALIC),
        Cow::Borrowed(crate::assets::IBM_PLEX_SANS_SEMIBOLD),
        Cow::Borrowed(crate::assets::IBM_PLEX_SANS_SEMIBOLD_ITALIC),
        Cow::Borrowed(crate::assets::LILEX_REGULAR),
        Cow::Borrowed(crate::assets::LILEX_ITALIC),
        Cow::Borrowed(crate::assets::LILEX_BOLD),
        Cow::Borrowed(crate::assets::LILEX_BOLD_ITALIC),
    ];
    for family in REQUIRED_SYSTEM_FONT_FAMILIES {
        match resolved_system_font_data(family) {
            Some(data) => fonts.push(Cow::Owned(data)),
            None => {
                tracing::debug!("fontconfig could not resolve required font family {family:?}")
            }
        }
    }

    if let Err(error) = text_system.add_fonts(fonts) {
        tracing::warn!("failed to load the bounded Linux font set: {error:#}");
    }

    BoundedTextSystem { inner: text_system }
}

/// The Linux platform with Vibex's bounded font stack installed.
pub fn platform() -> Rc<dyn Platform> {
    Rc::new(BoundedFontPlatform {
        inner: gpui_platform::current_platform(false),
        text_system: Arc::new(bounded_text_system()),
    })
}
