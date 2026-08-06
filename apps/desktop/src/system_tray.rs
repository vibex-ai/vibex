use std::io::Cursor;
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext as _, BorrowAppContext as _, Bounds, Entity, Global, QuitMode,
    Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions, px, size,
};
use gpui_component::{Root, TitleBar};
use image::ImageReader;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
#[cfg(not(target_os = "linux"))]
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

use crate::app::VibexWorkbench;
use crate::locale::{self, ResolvedLocale};
use crate::{DEFAULT_HEIGHT, DEFAULT_WIDTH, MIN_HEIGHT, MIN_WIDTH};

const TRAY_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TRAY_ICON_BYTES: &[u8] = include_bytes!("../assets/app-icons/tray-icon.png");
const OPEN_ID: &str = "vibex-tray-open";
const NEW_SESSION_ID: &str = "vibex-tray-new-session";
const CONFIG_CENTER_ID: &str = "vibex-tray-config-center";
const SETTINGS_ID: &str = "vibex-tray-settings";
const QUIT_ID: &str = "vibex-tray-quit";

struct TrayMenuItems {
    open: MenuItem,
    new_session: MenuItem,
    config_center: MenuItem,
    settings: MenuItem,
    quit: MenuItem,
}

pub(crate) struct SystemTray {
    _tray_icon: TrayIcon,
    items: TrayMenuItems,
    workbench: Entity<VibexWorkbench>,
    application_id: String,
    visible_window: Option<AnyWindowHandle>,
    last_window_bounds: WindowBounds,
    locale: ResolvedLocale,
    quitting: bool,
}

impl Global for SystemTray {}

impl SystemTray {
    fn new(
        workbench: Entity<VibexWorkbench>,
        application_id: String,
        window: &mut Window,
        locale: ResolvedLocale,
    ) -> Result<Self, String> {
        let strings = locale::strings(locale);
        let items = TrayMenuItems {
            open: MenuItem::with_id(OPEN_ID, strings.tray_open_vibex, true, None),
            new_session: MenuItem::with_id(NEW_SESSION_ID, strings.sidebar_new_session, true, None),
            config_center: MenuItem::with_id(
                CONFIG_CENTER_ID,
                strings.sidebar_providers,
                true,
                None,
            ),
            settings: MenuItem::with_id(SETTINGS_ID, strings.settings, true, None),
            quit: MenuItem::with_id(QUIT_ID, strings.tray_quit_vibex, true, None),
        };
        let separator = PredefinedMenuItem::separator();
        let menu = Menu::with_items(&[
            &items.open,
            &items.new_session,
            &items.config_center,
            &items.settings,
            &separator,
            &items.quit,
        ])
        .map_err(|error| format!("failed to create Vibex tray menu: {error}"))?;
        let icon = load_tray_icon()?;
        let tray_icon = TrayIconBuilder::new()
            .with_id("vibex-system-tray")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(true)
            .with_tooltip("Vibex")
            .with_icon(icon)
            .build()
            .map_err(|error| format!("failed to create Vibex tray icon: {error}"))?;

        Ok(Self {
            _tray_icon: tray_icon,
            items,
            workbench,
            application_id,
            visible_window: Some(window.window_handle()),
            last_window_bounds: window.window_bounds(),
            locale,
            quitting: false,
        })
    }

    fn update_locale(&mut self, locale: ResolvedLocale) {
        if self.locale == locale {
            return;
        }
        let strings = locale::strings(locale);
        self.items.open.set_text(strings.tray_open_vibex);
        self.items.new_session.set_text(strings.sidebar_new_session);
        self.items.config_center.set_text(strings.sidebar_providers);
        self.items.settings.set_text(strings.settings);
        self.items.quit.set_text(strings.tray_quit_vibex);
        self.locale = locale;
    }

    fn close_visible_window(&mut self, window: &mut Window, cx: &mut App) -> bool {
        if self.quitting || self.visible_window != Some(window.window_handle()) {
            return true;
        }

        let close_to_tray = self
            .workbench
            .read(cx)
            .ui_state()
            .desktop_behavior
            .close_to_tray;
        if !close_to_tray {
            cx.set_quit_mode(QuitMode::LastWindowClosed);
            self.quitting = true;
            return true;
        }

        self.last_window_bounds = window.window_bounds();
        self.workbench.update(cx, |workbench, cx| {
            workbench.prepare_for_window_close(window, cx)
        });
        self.visible_window = None;
        true
    }

    fn restore(&mut self, cx: &mut App) {
        if self.quitting {
            return;
        }
        if let Some(visible_window) = self.visible_window
            && activate_existing_window(visible_window, cx)
        {
            return;
        }
        self.visible_window = None;

        let workbench = self.workbench.clone();
        let visible_options =
            match workbench_window_options(self.application_id.clone(), self.last_window_bounds) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("failed to restore Vibex from the system tray: {error}");
                    return;
                }
            };
        let visible_window = match cx.open_window(visible_options, move |window, cx| {
            window.on_window_should_close(cx, handle_window_close);
            cx.new(|cx| Root::new(workbench, window, cx).bordered(false))
        }) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("failed to restore Vibex from the system tray: {error}");
                return;
            }
        };

        let visible_window: AnyWindowHandle = visible_window.into();
        self.visible_window = Some(visible_window);
        let workbench = self.workbench.clone();
        if let Err(error) = visible_window.update(cx, |_, window, cx| {
            workbench.update(cx, |workbench, cx| workbench.bind_to_window(window, cx));
            window.activate_window();
        }) {
            eprintln!("failed to activate the restored Vibex window: {error}");
            self.visible_window = None;
            return;
        }
        cx.activate(true);
    }

    fn with_visible_window(
        &mut self,
        cx: &mut App,
        action: impl FnOnce(&Entity<VibexWorkbench>, &mut Window, &mut App),
    ) {
        self.restore(cx);
        let Some(window_handle) = self.visible_window else {
            return;
        };
        let workbench = self.workbench.clone();
        let _ = window_handle.update(cx, move |_, window, cx| action(&workbench, window, cx));
    }

    fn handle_menu_event(&mut self, event: MenuEvent, cx: &mut App) {
        match event.id.as_ref() {
            OPEN_ID => self.restore(cx),
            NEW_SESSION_ID => self.with_visible_window(cx, |workbench, window, cx| {
                workbench.update(cx, |workbench, cx| {
                    workbench.open_new_session_from_tray(window, cx)
                });
            }),
            CONFIG_CENTER_ID => self.with_visible_window(cx, |workbench, _, cx| {
                workbench.update(cx, |workbench, cx| workbench.open_management_from_tray(cx));
            }),
            SETTINGS_ID => self.with_visible_window(cx, |workbench, window, cx| {
                workbench.update(cx, |workbench, cx| {
                    workbench.open_settings_from_tray(window, cx)
                });
            }),
            QUIT_ID => {
                self.quitting = true;
                cx.quit();
            }
            _ => {}
        }
    }
}

pub(crate) fn initialize(
    workbench: Entity<VibexWorkbench>,
    application_id: String,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if !gtk::is_initialized() {
        gtk::init()
            .map_err(|error| format!("failed to initialize GTK for system tray: {error}"))?;
    }

    let resolved_locale = workbench.read(cx).resolved_locale_for_tray();
    let tray = SystemTray::new(workbench, application_id, window, resolved_locale)?;
    cx.set_global(tray);
    cx.set_quit_mode(QuitMode::Explicit);
    poll_events(cx);
    Ok(())
}

pub(crate) fn handle_window_close(window: &mut Window, cx: &mut App) -> bool {
    if !cx.has_global::<SystemTray>() {
        return true;
    }
    cx.update_global::<SystemTray, _>(|tray, cx| tray.close_visible_window(window, cx))
}

pub(crate) fn update_locale(locale: ResolvedLocale, cx: &mut App) {
    if cx.has_global::<SystemTray>() {
        cx.update_global::<SystemTray, _>(|tray, _| tray.update_locale(locale));
    }
}

fn poll_events(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(TRAY_EVENT_POLL_INTERVAL)
                .await;
            let should_stop = cx.update(|cx| {
                #[cfg(target_os = "linux")]
                while gtk::events_pending() {
                    gtk::main_iteration_do(false);
                }

                if !cx.has_global::<SystemTray>() {
                    return true;
                }
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    cx.update_global::<SystemTray, _>(|tray, cx| tray.handle_menu_event(event, cx));
                }
                #[cfg(not(target_os = "linux"))]
                while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        cx.update_global::<SystemTray, _>(|tray, cx| tray.restore(cx));
                    }
                }
                false
            });
            if should_stop {
                break;
            }
        }
    })
    .detach();
}

fn activate_existing_window(window_handle: AnyWindowHandle, cx: &mut App) -> bool {
    if window_handle
        .update(cx, |_, window, _| window.activate_window())
        .is_err()
    {
        return false;
    }
    cx.activate(true);
    true
}

fn load_tray_icon() -> Result<Icon, String> {
    let image = ImageReader::new(Cursor::new(TRAY_ICON_BYTES))
        .with_guessed_format()
        .map_err(|error| format!("failed to detect Vibex tray icon format: {error}"))?
        .decode()
        .map_err(|error| format!("failed to decode Vibex tray icon: {error}"))?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height)
        .map_err(|error| format!("failed to load Vibex tray icon pixels: {error}"))
}

fn workbench_window_options(
    application_id: String,
    window_bounds: WindowBounds,
) -> Result<WindowOptions, String> {
    Ok(WindowOptions {
        window_bounds: Some(normalized_window_bounds(window_bounds)),
        titlebar: Some(TitleBar::title_bar_options()),
        app_id: Some(application_id),
        icon: Some(crate::assets::window_icon()?),
        window_min_size: Some(size(px(MIN_WIDTH as f32), px(MIN_HEIGHT as f32))),
        window_decorations: Some(WindowDecorations::Client),
        #[cfg(target_os = "linux")]
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    })
}

fn normalized_window_bounds(bounds: WindowBounds) -> WindowBounds {
    let restore_bounds = bounds.get_bounds();
    if f32::from(restore_bounds.size.width) <= 0.0 || f32::from(restore_bounds.size.height) <= 0.0 {
        WindowBounds::Windowed(Bounds::new(
            restore_bounds.origin,
            size(px(DEFAULT_WIDTH as f32), px(DEFAULT_HEIGHT as f32)),
        ))
    } else {
        bounds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icon_is_high_contrast_and_rounded() {
        let icon = image::load_from_memory_with_format(TRAY_ICON_BYTES, image::ImageFormat::Png)
            .expect("bundled tray icon should decode")
            .into_rgba8();

        assert_eq!(icon.dimensions(), (128, 128));
        assert_eq!(*icon.get_pixel(0, 0), image::Rgba([0, 0, 0, 0]));
        assert!(icon.pixels().any(|pixel| pixel.0 == [255, 255, 255, 255]));
    }

    #[test]
    fn invalid_restore_bounds_fall_back_to_the_desktop_default() {
        let bounds = WindowBounds::Windowed(Bounds::new(
            gpui::point(px(24.0), px(36.0)),
            size(px(0.0), px(0.0)),
        ));

        let normalized = normalized_window_bounds(bounds);

        assert_eq!(
            normalized.get_bounds(),
            Bounds::new(
                gpui::point(px(24.0), px(36.0)),
                size(px(DEFAULT_WIDTH as f32), px(DEFAULT_HEIGHT as f32)),
            )
        );
    }

    #[test]
    fn restored_window_preserves_maximized_state() {
        let restored = Bounds::new(gpui::point(px(40.0), px(60.0)), size(px(1280.0), px(820.0)));

        assert_eq!(
            normalized_window_bounds(WindowBounds::Maximized(restored)),
            WindowBounds::Maximized(restored)
        );
    }

    struct TrayWindowProbe;

    impl gpui::Render for TrayWindowProbe {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    #[gpui::test]
    fn stale_visible_window_handle_does_not_block_restore(cx: &mut gpui::TestAppContext) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| cx.new(|_| TrayWindowProbe))
                .expect("tray window probe should open")
        });
        let handle: AnyWindowHandle = window.into();

        cx.update(|cx| {
            assert!(activate_existing_window(handle, cx));
            handle
                .update(cx, |_, window, _| window.remove_window())
                .expect("tray window probe should close");
            assert!(!activate_existing_window(handle, cx));
        });
    }

    #[gpui::test]
    fn retained_workbench_entity_can_be_rehosted_after_its_window_closes(
        cx: &mut gpui::TestAppContext,
    ) {
        let workbench = cx.update(|cx| cx.new(|_| TrayWindowProbe));
        let first_window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, _| workbench.clone())
                .expect("first tray window probe should open")
        });
        cx.update(|cx| {
            first_window
                .update(cx, |_, window, _| window.remove_window())
                .expect("first tray window probe should close");
        });

        let restored_window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, _| workbench.clone())
                .expect("restored tray window probe should open")
        });

        assert_eq!(
            restored_window
                .entity(cx)
                .expect("restored tray window probe should have a root"),
            workbench
        );
    }
}
