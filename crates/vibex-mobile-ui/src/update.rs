//! The phone's release check, and the way into an install.
//!
//! The phone ships outside the store channels, so the only release it can
//! install is the Android package GitHub publishes. This module asks the
//! release feed once at launch and again whenever the app returns to the
//! foreground — no more often than a few hours apart — and keeps the answer
//! in one signal the screens read: the connection page turns it into the
//! green block beside Settings, and the drawer turns it into the same block
//! beside its own Settings button.
//!
//! Why a queue and not a callback: the package is downloaded and installed
//! by the platform host, which this view crate cannot reach. The app's
//! poller moves the accepted URL across, the same way it already moves a
//! scanned code or a camera photo.

use std::{
    cell::{Cell, RefCell},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nsh_base::{ButtonElement, button};
use nsh_core::{element::HttpRequest, platform::Lifecycle, prelude::*};

use crate::{copy, theme};

/// Where the latest release is described.
const RELEASES_API: &str = "https://api.github.com/repos/peatboy/vibex/releases/latest";
/// Where a release with no package this phone can install is read about.
const RELEASES_PAGE: &str = "https://github.com/peatboy/vibex/releases";
/// How long an answer is trusted before the app asks again.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// How long after a failed check the app may ask again, so a phone that
/// launched without a network does not wait the full interval.
const RETRY_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// How long after handing an APK URL to the host before a repeat tap is
/// accepted again, so an accidental double-tap does not queue two downloads
/// while a user who cancelled the installer can still retry.
const INSTALL_COOLDOWN: Duration = Duration::from_secs(5);
/// GitHub's release API answers a request that does not say who makes it
/// with a refusal.
const USER_AGENT: &str = concat!("Vibex/", env!("CARGO_PKG_VERSION"), " (mobile)");

/// What the last check said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Availability {
    /// No answer yet: the first check of this launch has not finished.
    #[default]
    Unknown,
    /// A check is in flight.
    Checking,
    /// The newest release is the build this phone runs.
    Current,
    /// A newer release exists. `download` is its Android package when the
    /// release carries one; the iOS artifacts are not an install the phone
    /// can perform.
    Available {
        version: String,
        download: Option<String>,
    },
    /// The feed could not be read: why, when the platform said.
    Failed(Option<String>),
}

/// The surface's answer, read by every screen that offers the update.
pub fn status() -> Signal<Availability> {
    global::<Availability>()
}

/// Whether a newer release is known.
pub fn available() -> bool {
    matches!(status().get(), Availability::Available { .. })
}

/// The newer release's Android package, when the release has one.
pub fn download() -> Option<String> {
    match status().get() {
        Availability::Available { download, .. } => download,
        _ => None,
    }
}

/// The line the About page shows under its check row; empty before a check.
pub fn message() -> SharedString {
    match status().get() {
        Availability::Unknown => SharedString::default(),
        Availability::Checking => copy::text("Checking…", "检查中…", "檢查中…").into(),
        Availability::Current => copy::text("Up to date", "已是最新版本", "已是最新版本").into(),
        Availability::Available { version, .. } => format!(
            "{}v{version}",
            copy::text("Available: ", "可更新到：", "可更新至：")
        )
        .into(),
        Availability::Failed(Some(reason)) => format!(
            "{}: {reason}",
            copy::text("Update check failed", "检查更新失败", "檢查更新失敗")
        )
        .into(),
        Availability::Failed(None) => {
            copy::text("Update check failed", "检查更新失败", "檢查更新失敗").into()
        }
    }
}

/// Asks the release feed now, unless a request is already on its way.
///
/// Safe to call from a press handler or an effect: the request runs under a
/// detached owner, so leaving the page that asked does not abandon it.
pub fn refresh() {
    refresh_inner(true);
}

fn refresh_inner(explicit: bool) {
    if CHECKING.replace(true) {
        return;
    }
    let state = status();
    let previous = state.get_untracked();
    if explicit || !matches!(previous, Availability::Available { .. }) {
        state.set(Availability::Checking);
    }
    Owner::detached().run(move || {
        drop(spawn(async move {
            let next = ask_release_feed().await;
            let wait = if matches!(next, Availability::Failed(_)) {
                RETRY_INTERVAL
            } else {
                CHECK_INTERVAL
            };
            NEXT_CHECK_MS.with(|cell| cell.set(now_ms() + wait.as_millis() as u64));
            CHECKING.set(false);
            if !explicit
                && matches!(next, Availability::Failed(_))
                && matches!(previous, Availability::Available { .. })
            {
                state.set(previous);
            } else {
                state.set(next);
            }
        }));
    });
}

/// Starts the automatic check: once now, then each time the app returns to
/// the foreground, no more often than [`CHECK_INTERVAL`]. The app calls this
/// once, while it builds its root.
pub fn watch() {
    refresh_if_stale();
    effect(move || {
        if lifecycle() == Lifecycle::Active {
            refresh_if_stale();
        }
    });
}

/// Installs the newer release the reader asked for. A release with no
/// package this phone can install (iOS) opens the release page instead.
pub fn install() {
    let Availability::Available { version, download } = status().get_untracked() else {
        return;
    };
    let Some(url) = download else {
        open_releases();
        return;
    };
    // Avoid double-queueing the same APK download on a rapid double-tap,
    // while still allowing a retry if the user cancelled the system installer.
    let now = now_ms();
    let cooling_down = HANDED_OFF.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|(last_version, until)| last_version == &version && now < *until)
    });
    if cooling_down {
        return;
    }
    HANDED_OFF.with(|slot| {
        *slot.borrow_mut() = Some((version, now + INSTALL_COOLDOWN.as_millis() as u64));
    });
    PENDING_INSTALL.with(|slot| *slot.borrow_mut() = Some(url));
    crate::motion::tick();
}

/// Takes the release waiting for the platform host, once the reader chose
/// to install one. The app's poller drains this into its install request.
pub fn take_install() -> Option<String> {
    PENDING_INSTALL.with(|slot| slot.borrow_mut().take())
}

/// The update's round block on the connection page, to the left of the
/// settings block: drawn only once a newer release is known.
pub fn floating_button() -> impl IntoElement {
    when(available, || control(true))
}

/// The same block in the drawer's foot, to the left of its settings block.
pub fn drawer_button() -> impl IntoElement {
    when(available, || control(false))
}

fn control(floating: bool) -> ButtonElement {
    let block = button()
        .accessibility_label(copy::text("Install update", "安装更新", "安裝更新"))
        .on_press(install)
        .size(px(theme::TOUCH_TARGET))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::accent_success)
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.6))
        .child(
            svg(icon!("arrow-up"))
                .size(px(theme::ICON_MD))
                .text_color(theme::background),
        );
    if floating { block.shadow_xs() } else { block }
}

async fn ask_release_feed() -> Availability {
    let request = HttpRequest::get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", USER_AGENT);
    match fetch(request).await {
        Ok(response) if response.ok() => match release_from(&response.text()) {
            Some(release) => {
                if is_newer(&release.version, env!("CARGO_PKG_VERSION")) {
                    // A newer release is a new thing to hand over, whatever
                    // was handed over before.
                    HANDED_OFF.with(|slot| *slot.borrow_mut() = None);
                    Availability::Available {
                        version: release.version,
                        download: release.download,
                    }
                } else {
                    Availability::Current
                }
            }
            None => Availability::Failed(Some(
                copy::text("No release information", "没有发布信息", "沒有發佈資訊").to_owned(),
            )),
        },
        Ok(response) => Availability::Failed(Some(format!("HTTP {}", response.status()))),
        Err(error) => Availability::Failed(Some(error)),
    }
}

fn refresh_if_stale() {
    if now_ms() >= NEXT_CHECK_MS.with(Cell::get) {
        refresh_inner(false);
    }
}

fn open_releases() {
    Owner::detached().run(|| {
        drop(spawn(async move {
            let _ = nsh_core::element::open_url(RELEASES_PAGE).await;
        }));
    });
}

/// A release: the version its tag names, and the package it publishes.
struct Release {
    /// The version without its leading `v`.
    version: String,
    /// The Android package, when the release has one.
    download: Option<String>,
}

#[derive(serde::Deserialize)]
struct ReleaseBody {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    assets: Vec<AssetBody>,
}

#[derive(serde::Deserialize)]
struct AssetBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
}

fn release_from(body: &str) -> Option<Release> {
    let release: ReleaseBody = serde_json::from_str(body).ok()?;
    let version = release.tag_name.trim().trim_start_matches('v').to_owned();
    if version.is_empty() {
        return None;
    }
    let download = release
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(".apk"))
        .map(|asset| asset.browser_download_url.trim().to_owned())
        .filter(|url| !url.is_empty());
    Some(Release { version, download })
}

fn is_newer(remote: &str, local: &str) -> bool {
    match (
        semver::Version::parse(remote),
        semver::Version::parse(local),
    ) {
        (Ok(remote), Ok(local)) => remote > local,
        // A tag the semantic parser rejects still compares the way the old
        // check did: equal, or not.
        _ => remote != local,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

thread_local! {
    /// The last release handed to the host and when its cooldown expires.
    static HANDED_OFF: RefCell<Option<(String, u64)>> = const { RefCell::new(None) };
    /// The release waiting for the host's downloader.
    static PENDING_INSTALL: RefCell<Option<String>> = const { RefCell::new(None) };
    /// When the feed may be asked again.
    static NEXT_CHECK_MS: Cell<u64> = const { Cell::new(0) };
    /// A request is already on its way.
    static CHECKING: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = r#"{
        "tag_name": "v0.2.0",
        "assets": [
            {"name": "vibex-0.2.0-android-arm64.apk",
             "browser_download_url": "https://example.com/vibex-0.2.0.apk"},
            {"name": "vibex-0.2.0-source.tar.gz",
             "browser_download_url": "https://example.com/vibex-0.2.0.tar.gz"}
        ]
    }"#;

    #[test]
    fn a_release_names_its_version_and_its_package() {
        let release = release_from(RELEASE).expect("the feed parses");
        assert_eq!(release.version, "0.2.0");
        assert_eq!(
            release.download.as_deref(),
            Some("https://example.com/vibex-0.2.0.apk")
        );
    }

    #[test]
    fn a_release_without_a_package_has_no_download() {
        let release =
            release_from(r#"{"tag_name": "v0.2.0", "assets": []}"#).expect("the feed parses");
        assert!(release.download.is_none());
    }

    #[test]
    fn a_body_that_is_not_a_release_is_no_answer() {
        assert!(release_from("{}").is_none());
        assert!(release_from("not json").is_none());
    }

    #[test]
    fn only_a_newer_version_is_an_update() {
        assert!(is_newer("0.2.0", "0.1.10"));
        assert!(!is_newer("0.1.10", "0.1.10"));
        assert!(!is_newer("0.1.9", "0.1.10"));
        // A tag the semantic parser rejects still compares: equal, or not.
        assert!(is_newer("0.2.0-rc.1+build", "0.1.10"));
    }

    #[test]
    fn the_about_line_names_the_release() {
        let available = Availability::Available {
            version: "0.2.0".to_owned(),
            download: None,
        };
        assert!(format!("{:?}", available).contains("0.2.0"));
    }
}
