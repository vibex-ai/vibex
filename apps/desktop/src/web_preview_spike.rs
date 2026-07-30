use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    Context, FocusHandle, IntoElement, KeyDownEvent, Render, Task, Window, div, prelude::*, px, rgb,
};
use gpui_component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use serde::Serialize;
use wry::{
    PageLoadEvent, Rect, WebView, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize},
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

#[cfg(target_os = "linux")]
use wry::raw_window_handle::{HandleError, WindowHandle, XlibWindowHandle};

const INITIAL_BOUNDS: SurfaceBounds = SurfaceBounds {
    x: 80,
    y: 104,
    width: 820,
    height: 520,
};
const RESIZED_BOUNDS: SurfaceBounds = SurfaceBounds {
    x: 120,
    y: 132,
    width: 760,
    height: 470,
};
const TICK_INTERVAL: Duration = Duration::from_millis(40);
const RESIZED_HOLD: Duration = Duration::from_millis(1_500);
const HIDDEN_HOLD: Duration = Duration::from_millis(850);
const SHOWN_HOLD: Duration = Duration::from_millis(850);

const FIXTURE_HTML: &str = r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    * { box-sizing: border-box; }
    html, body { margin: 0; min-height: 100%; }
    body { background: #167f68; color: #f7fbf9; font: 16px sans-serif; }
    header { position: sticky; top: 0; padding: 24px; background: #172a3a; }
    h1 { margin: 0 0 14px; font-size: 24px; }
    input { width: min(520px, 100%); padding: 12px; border: 2px solid #f2c14e; background: #fff; color: #111; }
    main { min-height: 1800px; padding: 28px; background: repeating-linear-gradient(0deg, #167f68 0 120px, #116b59 120px 240px); }
    .marker { width: 180px; height: 80px; background: #f2c14e; color: #172a3a; display: grid; place-items: center; font-weight: 700; }
  </style>
</head>
<body tabindex="0">
  <header><h1>Vibex Web Preview</h1><input id="fixture-input" autocomplete="off"></header>
  <main><div class="marker">Embedded surface</div></main>
  <script>
    const send = value => window.ipc.postMessage(value);
    const input = document.getElementById('fixture-input');
    input.addEventListener('input', () => send(`input:${new TextEncoder().encode(input.value).length}`));
    document.addEventListener('keydown', event => {
      if (event.key === 'j') {
        event.preventDefault();
        window.scrollBy({ top: 420, behavior: 'instant' });
      }
    });
    window.addEventListener('scroll', () => {
      if (window.scrollY > 40) send(`scroll:${Math.round(window.scrollY)}`);
    }, { passive: true });
    window.addEventListener('load', () => send('ready'));
  </script>
</body>
</html>"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SurfaceBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Default)]
struct BrowserSignals {
    page_load_finished: u64,
    ready_messages: u64,
    input_bytes: u64,
    scroll_y: i64,
    web_process_terminated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebPreviewRunReport {
    schema_version: &'static str,
    status: &'static str,
    platform: &'static str,
    architecture: &'static str,
    backend: &'static str,
    parent_handle: &'static str,
    native_child_created: bool,
    lifecycle: LifecycleObservation,
    privacy: PrivacyObservation,
    failure: Option<FailureObservation>,
    limitations: Vec<&'static str>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleObservation {
    page_load_finished_count: u64,
    ipc_ready_observed: bool,
    keyboard_input_observed: bool,
    input_bytes: u64,
    scroll_observed: bool,
    scroll_y: i64,
    initial_bounds: Option<SurfaceBounds>,
    resized_bounds: Option<SurfaceBounds>,
    bounds_round_trip: bool,
    hidden: bool,
    shown_after_hidden: bool,
    web_process_terminated: bool,
    reload_finished: bool,
    focus_return_key_observed: bool,
    raw_window_move_observed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyObservation {
    inline_fixture: bool,
    raw_input_stored: bool,
    url_stored: bool,
    profile_path_stored: bool,
}

#[derive(Debug, Clone, Serialize)]
struct FailureObservation {
    code: &'static str,
}

trait NativeSurfaceHost {
    fn set_bounds(&mut self, bounds: SurfaceBounds) -> Result<bool, ()>;
    fn set_visible(&self, visible: bool) -> Result<(), ()>;
    fn focus(&self) -> Result<(), ()>;
    fn focus_parent(&self) -> Result<(), ()>;
    fn evaluate_script(&self, script: &str) -> Result<(), ()>;
    fn reload(&self) -> Result<(), ()>;
    fn terminate_web_process(&self) -> bool;
}

struct WryNativeSurfaceHost {
    webview: WebView,
    bounds: SurfaceBounds,
}

impl NativeSurfaceHost for WryNativeSurfaceHost {
    fn set_bounds(&mut self, bounds: SurfaceBounds) -> Result<bool, ()> {
        self.webview
            .set_bounds(wry_bounds(bounds))
            .map_err(|_| ())?;
        self.bounds = bounds;
        let actual = self.webview.bounds().map_err(|_| ())?;
        let position = actual.position.to_logical::<i32>(1.0);
        let size = actual.size.to_logical::<u32>(1.0);
        Ok(position.x == bounds.x
            && position.y == bounds.y
            && size.width == bounds.width
            && size.height == bounds.height)
    }

    fn set_visible(&self, visible: bool) -> Result<(), ()> {
        self.webview.set_visible(visible).map_err(|_| ())
    }

    fn focus(&self) -> Result<(), ()> {
        self.webview.focus().map_err(|_| ())
    }

    fn focus_parent(&self) -> Result<(), ()> {
        self.webview.focus_parent().map_err(|_| ())
    }

    fn evaluate_script(&self, script: &str) -> Result<(), ()> {
        self.webview.evaluate_script(script).map_err(|_| ())
    }

    fn reload(&self) -> Result<(), ()> {
        self.webview.reload().map_err(|_| ())
    }

    fn terminate_web_process(&self) -> bool {
        terminate_web_process(&self.webview)
    }
}

pub struct WebPreviewSpikeView {
    output: PathBuf,
    progress_output: PathBuf,
    phase: &'static str,
    phase_started: Instant,
    backend: &'static str,
    parent_handle: &'static str,
    host: Option<WryNativeSurfaceHost>,
    signals: Arc<Mutex<BrowserSignals>>,
    lifecycle: LifecycleObservation,
    focus_handle: FocusHandle,
    report: Option<WebPreviewRunReport>,
    creation_failure: Option<&'static str>,
    _tick_task: Option<Task<()>>,
    quit_task: Option<Task<()>>,
}

impl WebPreviewSpikeView {
    pub fn new(output: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let progress_output = output.with_extension("progress.json");
        let signals = Arc::new(Mutex::new(BrowserSignals::default()));
        let parent_handle = raw_handle_kind(window);
        let backend = backend_name(parent_handle);
        let host = create_host(window, signals.clone());
        let (host, creation_failure, phase) = match host {
            Ok(host) => (Some(host), None, "loading"),
            Err(code) => (None, Some(code), "unsupported"),
        };
        let mut view = Self {
            output,
            progress_output,
            phase,
            phase_started: Instant::now(),
            backend,
            parent_handle,
            host,
            signals,
            lifecycle: LifecycleObservation {
                initial_bounds: (phase == "loading").then_some(INITIAL_BOUNDS),
                ..LifecycleObservation::default()
            },
            focus_handle: cx.focus_handle().tab_stop(true),
            report: None,
            creation_failure,
            _tick_task: None,
            quit_task: None,
        };
        let _ = view.write_progress();
        view._tick_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK_INTERVAL).await;
                let done = match this.update_in(cx, |this, window, cx| this.tick(window, cx)) {
                    Ok(done) => done,
                    Err(_) => true,
                };
                if done {
                    break;
                }
            }
        }));
        view
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        pump_platform_events();
        if self.report.is_some() {
            return true;
        }
        if self.creation_failure.is_some() {
            self.finish(false, cx);
            return true;
        }
        let signals = self
            .signals
            .lock()
            .map(|signals| signals.clone())
            .unwrap_or_default();
        self.lifecycle.page_load_finished_count = signals.page_load_finished;
        self.lifecycle.ipc_ready_observed = signals.ready_messages > 0;
        self.lifecycle.keyboard_input_observed = signals.input_bytes > 0;
        self.lifecycle.input_bytes = signals.input_bytes;
        self.lifecycle.scroll_observed = signals.scroll_y > 40;
        self.lifecycle.scroll_y = signals.scroll_y;
        self.lifecycle.web_process_terminated = signals.web_process_terminated;

        match self.phase {
            "loading" if signals.page_load_finished > 0 && signals.ready_messages > 0 => {
                let focused = self.host.as_ref().is_some_and(|host| {
                    host.focus().is_ok()
                        && host
                            .evaluate_script("document.getElementById('fixture-input').focus()")
                            .is_ok()
                });
                if focused {
                    self.transition("input_ready");
                }
            }
            "input_ready" => {
                if signals.input_bytes > 0 {
                    let ready = self
                        .host
                        .as_ref()
                        .is_some_and(|host| host.evaluate_script("document.body.focus()").is_ok());
                    if ready {
                        self.transition("scroll_ready");
                    }
                } else if let Some(host) = self.host.as_ref() {
                    let _ = host.focus();
                    let _ =
                        host.evaluate_script("document.getElementById('fixture-input').focus()");
                }
            }
            "scroll_ready" => {
                if signals.scroll_y > 40 {
                    if let Some(host) = self.host.as_mut()
                        && let Ok(round_trip) = host.set_bounds(RESIZED_BOUNDS)
                    {
                        self.lifecycle.resized_bounds = Some(RESIZED_BOUNDS);
                        self.lifecycle.bounds_round_trip = round_trip;
                        self.transition("resized");
                    }
                } else if let Some(host) = self.host.as_ref() {
                    let _ = host.focus();
                    let _ = host.evaluate_script("document.body.focus()");
                }
            }
            "resized" if self.phase_started.elapsed() >= RESIZED_HOLD => {
                if self
                    .host
                    .as_ref()
                    .is_some_and(|host| host.set_visible(false).is_ok())
                {
                    self.lifecycle.hidden = true;
                    self.transition("hidden");
                }
            }
            "hidden" if self.phase_started.elapsed() >= HIDDEN_HOLD => {
                if self
                    .host
                    .as_ref()
                    .is_some_and(|host| host.set_visible(true).is_ok())
                {
                    self.lifecycle.shown_after_hidden = true;
                    self.transition("shown");
                }
            }
            "shown" if self.phase_started.elapsed() >= SHOWN_HOLD => {
                if self
                    .host
                    .as_ref()
                    .is_some_and(NativeSurfaceHost::terminate_web_process)
                {
                    self.transition("crashing");
                }
            }
            "crashing" if signals.web_process_terminated => {
                if self.host.as_ref().is_some_and(|host| host.reload().is_ok()) {
                    self.transition("reloading");
                }
            }
            "reloading" if signals.page_load_finished >= 2 => {
                self.lifecycle.reload_finished = true;
                if self
                    .host
                    .as_ref()
                    .is_some_and(|host| host.focus_parent().is_ok())
                {
                    self.focus_handle.focus(window, cx);
                    self.transition("focus_return_ready");
                }
            }
            "focus_return_ready" if self.lifecycle.focus_return_key_observed => {
                self.finish(true, cx);
                return true;
            }
            _ => {}
        }
        cx.notify();
        false
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.phase == "focus_return_ready" && event.keystroke.key == "f8" {
            self.lifecycle.focus_return_key_observed = true;
            let _ = self.write_progress();
            cx.notify();
        }
    }

    fn transition(&mut self, phase: &'static str) {
        self.phase = phase;
        self.phase_started = Instant::now();
        let _ = self.write_progress();
    }

    fn finish(&mut self, passed: bool, cx: &mut Context<Self>) {
        let failure = if passed {
            None
        } else {
            Some(FailureObservation {
                code: self
                    .creation_failure
                    .unwrap_or("web-preview-evidence-incomplete"),
            })
        };
        let report = WebPreviewRunReport {
            schema_version: "web-preview-run.v1",
            status: if passed { "passed" } else { "blocked" },
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            backend: self.backend,
            parent_handle: self.parent_handle,
            native_child_created: self.host.is_some(),
            lifecycle: self.lifecycle.clone(),
            privacy: PrivacyObservation {
                inline_fixture: true,
                raw_input_stored: false,
                url_stored: false,
                profile_path_stored: false,
            },
            failure,
            limitations: if passed {
                vec![
                    "The X11 spike uses WebKitGTK from the host and does not prove native Wayland child embedding.",
                    "Window movement and overlay pixels are validated by the external capture harness.",
                ]
            } else {
                vec![
                    "Wry 0.55.1 supports Linux raw-handle child WebViews on X11 only.",
                    "A GTK companion toplevel is not accepted as an embedded Wayland surface.",
                ]
            },
        };
        self.phase = report.status;
        if write_report(&self.output, &report).is_err() {
            self.phase = "failed";
        }
        self.report = Some(report);
        self.quit_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(hold_ms()))
                .await;
            let _ = cx.update(|cx| cx.quit());
        }));
        let _ = self.write_progress();
        cx.notify();
    }

    fn write_progress(&self) -> std::io::Result<()> {
        let signals = self
            .signals
            .lock()
            .map(|signals| signals.clone())
            .unwrap_or_default();
        write_json(
            &self.progress_output,
            &serde_json::json!({
                "schemaVersion": "web-preview-progress.v1",
                "phase": self.phase,
                "backend": self.backend,
                "parentHandle": self.parent_handle,
                "nativeChildCreated": self.host.is_some(),
                "pageLoadFinishedCount": signals.page_load_finished,
                "inputObserved": signals.input_bytes > 0,
                "inputBytes": signals.input_bytes,
                "scrollObserved": signals.scroll_y > 40,
                "webProcessTerminated": signals.web_process_terminated,
                "bounds": self.host.as_ref().map(|host| host.bounds),
                "rawTextStored": false
            }),
        )
    }
}

impl Render for WebPreviewSpikeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self
            .host
            .as_ref()
            .map(|host| host.bounds)
            .unwrap_or(INITIAL_BOUNDS);
        div()
            .id("web-preview-root")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .absolute()
                    .left(px(bounds.x as f32))
                    .top(px(bounds.y as f32))
                    .w(px(bounds.width as f32))
                    .h(px(bounds.height as f32))
                    .bg(rgb(0xff007a)),
            )
            .child(
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .h(px(52.0))
                            .flex_none()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .px_5()
                            .child(div().font_semibold().child("Web Preview"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(self.phase),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .p_5()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.backend),
                    ),
            )
    }
}

fn create_host(
    window: &Window,
    signals: Arc<Mutex<BrowserSignals>>,
) -> Result<WryNativeSurfaceHost, &'static str> {
    let ipc_signals = signals.clone();
    let load_signals = signals.clone();
    let builder = WebViewBuilder::new()
        .with_html(FIXTURE_HTML)
        .with_bounds(wry_bounds(INITIAL_BOUNDS))
        .with_focused(false)
        .with_ipc_handler(move |request| observe_ipc(request.body(), &ipc_signals))
        .with_on_page_load_handler(move |event, _| {
            if matches!(event, PageLoadEvent::Finished)
                && let Ok(mut signals) = load_signals.lock()
            {
                signals.page_load_finished = signals.page_load_finished.saturating_add(1);
            }
        });
    let webview =
        build_child_webview(builder, window).map_err(|_| match raw_handle_kind(window) {
            "wayland" => "wry-wayland-child-unsupported",
            _ => "wry-child-creation-failed",
        })?;
    attach_web_process_observer(&webview, signals);
    Ok(WryNativeSurfaceHost {
        webview,
        bounds: INITIAL_BOUNDS,
    })
}

#[cfg(target_os = "linux")]
struct XlibParentWindow {
    window: std::os::raw::c_ulong,
    visual_id: std::os::raw::c_ulong,
}

#[cfg(target_os = "linux")]
impl HasWindowHandle for XlibParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let mut handle = XlibWindowHandle::new(self.window);
        handle.visual_id = self.visual_id;
        Ok(unsafe { WindowHandle::borrow_raw(handle.into()) })
    }
}

fn build_child_webview(builder: WebViewBuilder<'_>, window: &Window) -> wry::Result<WebView> {
    #[cfg(target_os = "linux")]
    if let Ok(handle) = HasWindowHandle::window_handle(window)
        && let RawWindowHandle::Xcb(xcb) = handle.as_raw()
    {
        let parent = XlibParentWindow {
            window: xcb.window.get().into(),
            visual_id: xcb
                .visual_id
                .map(|visual_id| visual_id.get().into())
                .unwrap_or(0),
        };
        return builder.build_as_child(&parent);
    }
    builder.build_as_child(window)
}

fn observe_ipc(body: &str, signals: &Arc<Mutex<BrowserSignals>>) {
    let Ok(mut signals) = signals.lock() else {
        return;
    };
    if body == "ready" {
        signals.ready_messages = signals.ready_messages.saturating_add(1);
    } else if let Some(bytes) = body
        .strip_prefix("input:")
        .and_then(|value| value.parse().ok())
    {
        signals.input_bytes = bytes;
    } else if let Some(scroll_y) = body
        .strip_prefix("scroll:")
        .and_then(|value| value.parse().ok())
    {
        signals.scroll_y = scroll_y;
    }
}

fn wry_bounds(bounds: SurfaceBounds) -> Rect {
    Rect {
        position: LogicalPosition::new(bounds.x, bounds.y).into(),
        size: LogicalSize::new(bounds.width, bounds.height).into(),
    }
}

fn raw_handle_kind(window: &Window) -> &'static str {
    match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Xcb(_)) => "xcb",
        Ok(RawWindowHandle::Xlib(_)) => "xlib",
        Ok(RawWindowHandle::Wayland(_)) => "wayland",
        Ok(RawWindowHandle::Win32(_)) => "win32",
        Ok(RawWindowHandle::AppKit(_)) => "appkit",
        _ => "unsupported",
    }
}

fn backend_name(parent_handle: &str) -> &'static str {
    match parent_handle {
        "xcb" | "xlib" => "wry-webkitgtk-x11-child",
        "wayland" => "wry-webkitgtk-wayland-child-unsupported",
        "win32" => "wry-webview2-child",
        "appkit" => "wry-wkwebview-child",
        _ => "wry-child-unsupported",
    }
}

#[cfg(target_os = "linux")]
fn attach_web_process_observer(webview: &WebView, signals: Arc<Mutex<BrowserSignals>>) {
    use webkit2gtk::WebViewExt as _;
    use wry::WebViewExtUnix as _;

    webview
        .webview()
        .connect_web_process_terminated(move |_, _| {
            if let Ok(mut signals) = signals.lock() {
                signals.web_process_terminated = true;
            }
        });
}

#[cfg(not(target_os = "linux"))]
fn attach_web_process_observer(_: &WebView, _: Arc<Mutex<BrowserSignals>>) {}

#[cfg(target_os = "linux")]
fn terminate_web_process(webview: &WebView) -> bool {
    use webkit2gtk::WebViewExt as _;
    use wry::WebViewExtUnix as _;

    webview.webview().terminate_web_process();
    true
}

#[cfg(not(target_os = "linux"))]
fn terminate_web_process(_: &WebView) -> bool {
    false
}

#[cfg(target_os = "linux")]
pub fn init_platform() -> Result<(), &'static str> {
    gtk::init().map_err(|_| "gtk-initialization-failed")
}

#[cfg(not(target_os = "linux"))]
pub fn init_platform() -> Result<(), &'static str> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn pump_platform_events() {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

#[cfg(not(target_os = "linux"))]
fn pump_platform_events() {}

fn write_report(path: &Path, report: &WebPreviewRunReport) -> std::io::Result<()> {
    write_json(path, report)
}

fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    fs::write(path, bytes)
}

fn hold_ms() -> u64 {
    std::env::var("VIBEX_SPIKE_HOLD_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value <= 10_000)
        .unwrap_or(1_500)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_observation_retains_only_counts() {
        let signals = Arc::new(Mutex::new(BrowserSignals::default()));
        observe_ipc("input:14", &signals);
        observe_ipc("scroll:420", &signals);
        let signals = signals.lock().unwrap();
        assert_eq!(signals.input_bytes, 14);
        assert_eq!(signals.scroll_y, 420);
    }

    #[test]
    fn backend_mapping_does_not_promote_wayland_to_embedded() {
        assert_eq!(backend_name("xcb"), "wry-webkitgtk-x11-child");
        assert_eq!(
            backend_name("wayland"),
            "wry-webkitgtk-wayland-child-unsupported"
        );
    }
}
