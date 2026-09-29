//! The panel's own browser-shell replacements, against the system Chrome.
//!
//! Find-in-page, the hover cursor and downloads exist only as CDP calls into a
//! live page, so a unit test of the script string proves nothing about whether
//! Chrome accepts it or what it does to the DOM. A machine with no browser
//! skips the tests — that is the environment where the whole feature is
//! explicitly unavailable. Every other failure is a defect and fails the test.

use std::time::{Duration, Instant};

use vibex_browser::{BrowserService, BrowserServiceConfig, BrowserServiceEvent, BrowserSessionKey};
use vibex_core::{BrowserExecutionSource, BrowserTabOwner, WorkspaceId};

/// A page with three known hits, a link and a download link.
///
/// Served over loopback rather than as a `data:` URL: a `#` inside a data URL
/// starts its fragment and silently truncates the document, which is how the
/// first version of this fixture lost the links it was testing.
fn serve_page() -> u16 {
    use std::io::{Read, Write};

    const PAGE: &str = "<title>panel</title>\
<p id=first>alpha needle one</p>\
<p id=second>beta needle two</p>\
<p id=third>needle three</p>\
<a id=link href='/nowhere' style='position:fixed;bottom:0;left:0;width:200px;height:40px;display:block'>a link</a>\
<a id=dl href='/download' download='re:port?.txt' \
style='position:fixed;bottom:0;right:0;width:200px;height:40px;display:block'>save file</a>\
<input id=up type=file style='position:fixed;top:200px;left:300px;width:200px;height:40px'>";
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let request = String::from_utf8_lossy(&buffer);
            let response = if request.contains("GET /download") {
                let body = "hello";
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
Content-Disposition: attachment; filename=\"re:port?.txt\"\r\n\
Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    PAGE.len(),
                    PAGE
                )
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    port
}

type Fixture = (
    BrowserService,
    tempfile::TempDir,
    vibex_core::BrowserSessionId,
    vibex_core::BrowserTabId,
);

async fn service_with_page() -> Option<Fixture> {
    let home = tempfile::tempdir().expect("temp home");
    let service = BrowserService::new(BrowserServiceConfig::new(home.path()));
    let session_id = match service
        .ensure_session(BrowserSessionKey::Anonymous, None)
        .await
    {
        Ok(session_id) => session_id,
        Err(error) if error.is_browser_missing() => {
            eprintln!("skipping the browser panel test: no usable browser ({error})");
            return None;
        }
        Err(error) => panic!("the embedded browser did not start: {error}"),
    };
    let url = format!("http://127.0.0.1:{}/", serve_page());
    let tab = service
        .create_tab(&session_id, Some(&url), BrowserTabOwner::User)
        .await
        .expect("a tab");
    service
        .set_viewport(&tab, 900, 600, 1.0)
        .await
        .expect("a viewport");
    // The page has to be parsed, laid out and — for the name lookups — present
    // in the accessibility tree before any of these tests means anything. A
    // fixed sleep is not enough when four browsers start at once: the tree is
    // built a frame or two behind layout, and the download test then looked for
    // a link that was on screen but not yet in the tree. The wait uses the same
    // path the tests do — `browser_find` resolves through the AX snapshot, as
    // `browser_click_by_name` does.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let found = service
            .call_tool(
                &tool_context(&session_id),
                "browser_find",
                &serde_json::json!({ "tab_id": tab.as_str(), "query": "save file" }),
            )
            .await;
        if !found.is_error && reported_matches(&found.text).is_some_and(|count| count > 0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the fixture page never reached the accessibility tree: {}",
            found.text
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Some((service, home, session_id, tab))
}

/// The match count `browser_find` reports, or `None` when it did not answer with
/// a count at all.
fn reported_matches(text: &str) -> Option<usize> {
    text.split("Found ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[tokio::test]
async fn find_in_page_counts_and_steps_through_the_hits() {
    let Some((service, home, session_id, tab)) = service_with_page().await else {
        return;
    };
    let _home = home;

    let (total, current) = service
        .find_in_page(&tab, "needle", true)
        .await
        .expect("the first search runs");
    assert_eq!(total, 3, "every hit on the page is counted");
    assert_eq!(current, 1, "a fresh search starts at the first hit");

    let (total, current) = service
        .find_in_page(&tab, "needle", true)
        .await
        .expect("the second search runs");
    assert_eq!((total, current), (3, 2), "the same query steps forward");
    let (_, current) = service
        .find_in_page(&tab, "needle", false)
        .await
        .expect("the third search runs");
    assert_eq!(current, 1, "backwards steps to the previous hit");

    // A miss is not an error and does not inherit the previous count.
    let (total, current) = service
        .find_in_page(&tab, "absent-needle", true)
        .await
        .expect("a search with no hits still answers");
    assert_eq!((total, current), (0, 0));

    service.clear_find_in_page(&tab).await;
    // The page is left as it was found: no marker attribute, no injected style.
    let leftover = service
        .call_tool(
            &tool_context(&session_id),
            "browser_evaluate",
            &serde_json::json!({
                "tab_id": tab.as_str(),
                "script": "document.querySelectorAll('[data-vibex-find-active]').length \
                     + document.querySelectorAll('#vibex-find-style').length"
            }),
        )
        .await;
    assert!(!leftover.is_error, "the page answers: {}", leftover.text);
    assert!(
        leftover.text.contains('0'),
        "clearing the search removes its marks: {}",
        leftover.text
    );
    service.shutdown().await;
}

#[tokio::test]
async fn a_query_with_no_hits_says_so_without_touching_the_page() {
    let Some((service, home, _session_id, tab)) = service_with_page().await else {
        return;
    };
    let _home = home;
    let (total, current) = service
        .find_in_page(&tab, "nothing-matches-this", true)
        .await
        .expect("the search runs");
    assert_eq!((total, current), (0, 0));
    service.shutdown().await;
}

#[tokio::test]
async fn the_hovered_element_names_the_cursor_the_page_wants() {
    let Some((service, home, _session_id, tab)) = service_with_page().await else {
        return;
    };
    let _home = home;

    // The link sits in the bottom-left 200x40 box of a 900x600 viewport.
    let over_link = service
        .cursor_at(&tab, 60.0, 580.0)
        .await
        .expect("the probe answers");
    assert_eq!(over_link, "pointer", "a link asks for a pointing hand");

    let over_text = service
        .cursor_at(&tab, 20.0, 20.0)
        .await
        .expect("the probe answers");
    assert!(
        over_text == "auto" || over_text == "text",
        "a paragraph gets the page's own default, got {over_text}"
    );

    // A point outside the viewport has no element; that is not an error.
    let outside = service
        .cursor_at(&tab, -10.0, -10.0)
        .await
        .expect("the probe answers");
    assert_eq!(outside, "auto");
    service.shutdown().await;
}

#[tokio::test]
async fn an_allowed_download_lands_under_a_sanitized_name() {
    let Some((service, home, session_id, tab)) = service_with_page().await else {
        return;
    };
    let downloads = service.downloads_dir().await;
    service.set_downloads_enabled(true).await;

    // The link carries a hostile suggested name; the runtime decides the one
    // that reaches the disk.
    let clicked = service
        .call_tool(
            &tool_context(&session_id),
            "browser_click_by_name",
            &serde_json::json!({
                "tab_id": tab.as_str(),
                "name": "save file",
                "role": "link",
            }),
        )
        .await;
    assert!(
        !clicked.is_error,
        "the download link is clicked: {}",
        clicked.text
    );

    // Chrome writes the file under the guid it reported (`allowAndName`), and
    // the runtime renames it once `Browser.downloadProgress` says completed, so
    // both names exist on disk for a moment. Waiting for the first file to
    // appear raced that rename and asserted on Chrome's guid; the wait is for
    // the sanitized name, and the directory is checked afterwards too, so a
    // leftover guid beside it would still fail.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut landed = None;
    while Instant::now() < deadline {
        let renamed = std::fs::read_dir(&downloads)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .find(|name| name == "re_port_.txt");
        if let Some(name) = renamed {
            landed = Some(name);
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let landed =
        landed.expect("the download lands under the runtime's sanitized name, not Chrome's guid");
    assert_eq!(
        landed, "re_port_.txt",
        "path separators and reserved characters never reach the disk"
    );
    let remaining = std::fs::read_dir(&downloads)
        .expect("the download directory")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        remaining,
        vec!["re_port_.txt".to_string()],
        "the guid Chrome named is renamed away, not left beside the sanitized file"
    );
    assert!(
        downloads.starts_with(home.path()),
        "the write stays inside the runtime's data directory"
    );
    assert!(
        !downloads.starts_with(home.path().join("workspace")),
        "a page cannot drop a file into a project directory"
    );
    service.shutdown().await;
}

/// A tool context for the calls these tests borrow from the agent surface.
fn tool_context(session_id: &vibex_core::BrowserSessionId) -> vibex_browser::BrowserToolContext {
    vibex_browser::BrowserToolContext {
        session_id: session_id.clone(),
        agent_session_id: None,
        workspace_id: None,
        authorized_roots: Vec::new(),
        tier: vibex_core::BrowserToolTier::Fine,
        approved_origins: Vec::new(),
    }
}

/// Waits for the next download event, so the assertions below never race the
/// browser's own progress reports.
async fn next_download(
    events: &mut tokio::sync::broadcast::Receiver<vibex_browser::BrowserServiceEvent>,
    deadline: Instant,
) -> Option<vibex_browser::BrowserDownload> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Ok(vibex_browser::BrowserServiceEvent::Download(download))) => {
                return Some((*download).clone());
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => return None,
        }
    }
}

/// Clicks the fixture page's download link.
async fn click_download_link(
    service: &BrowserService,
    session_id: &vibex_core::BrowserSessionId,
    tab: &vibex_core::BrowserTabId,
) {
    let clicked = service
        .call_tool(
            &tool_context(session_id),
            "browser_click_by_name",
            &serde_json::json!({
                "tab_id": tab.as_str(),
                "name": "save file",
                "role": "link",
            }),
        )
        .await;
    assert!(
        !clicked.is_error,
        "the download link is clicked: {}",
        clicked.text
    );
}

/// A download the reader allowed reports its progress and where it landed.
///
/// The panel has no browser shelf to fall back on: without these events a
/// download was a silent write to a directory the reader never saw.
#[tokio::test]
async fn an_allowed_download_reports_progress_and_its_saved_path() {
    let Some((service, home, session_id, tab)) = service_with_page().await else {
        return;
    };
    let _home = home;
    service.set_downloads_enabled(true).await;
    let mut events = service.subscribe();

    click_download_link(&service, &session_id, &tab).await;

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut started = None;
    let mut completed = None;
    while let Some(download) = next_download(&mut events, deadline).await {
        match download.state {
            vibex_browser::BrowserDownloadState::InProgress if started.is_none() => {
                started = Some(download);
            }
            vibex_browser::BrowserDownloadState::Completed => {
                completed = Some(download);
                break;
            }
            _ => {}
        }
    }
    let started = started.expect("a download announces that it started");
    assert_eq!(
        started.file_name, "re_port_.txt",
        "the runtime names the file"
    );
    assert_eq!(
        started.tab_id, tab,
        "the download belongs to the tab that started it"
    );
    let completed = completed.expect("a download announces that it finished");
    let path = completed
        .path
        .as_ref()
        .expect("a completed download names the file on disk");
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("re_port_.txt")
    );
    assert!(path.is_file(), "the announced path is the file that landed");
    assert_eq!(
        completed.total_bytes, 5,
        "the size Chrome reported travels with the event"
    );
    assert_eq!(completed.received_bytes, 5);
    assert_eq!(
        completed.directory,
        service.downloads_dir().await,
        "the panel is told which folder to open"
    );
    service.shutdown().await;
}

/// The file a page's chooser is given is the file the page receives.
///
/// The panel's own "Choose file" button ends in this same call: a headless
/// browser has no native dialog, so `DOM.setFileInputFiles` is the only way a
/// human's answer ever reaches the `<input type=file>`.
#[tokio::test]
async fn a_chosen_file_reaches_the_pages_file_input() {
    let Some((service, home, session_id, tab)) = service_with_page().await else {
        return;
    };
    let chosen = home.path().join("chosen.txt");
    std::fs::write(&chosen, b"picked by the reader").expect("the chosen file");
    let mut events = service.subscribe();

    // The page opens its chooser; the runtime intercepts it and the panel's
    // card is what answers instead of a native dialog. The click travels as a
    // real mouse event: a scripted `click()` has no user activation, and Chrome
    // refuses to open a chooser without one.
    for input in [
        vibex_browser::BrowserInput::MouseDown {
            x: 400.0,
            y: 220.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        },
        vibex_browser::BrowserInput::MouseUp {
            x: 400.0,
            y: 220.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        },
    ] {
        service
            .dispatch_input(&tab, input)
            .await
            .expect("the click reaches the page");
    }
    service.resume_agent_operations(&session_id).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut announced = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Ok(vibex_browser::BrowserServiceEvent::FileChooserOpened(opened))) => {
                assert_eq!(opened, tab);
                announced = true;
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    assert!(announced, "the page's file chooser reaches the panel");

    service
        .resolve_file_chooser(&tab, std::slice::from_ref(&chosen))
        .await
        .expect("the chosen path is applied to the page");
    let files = service
        .call_tool(
            &tool_context(&session_id),
            "browser_evaluate",
            &serde_json::json!({
                "tab_id": tab.as_str(),
                "script": "document.getElementById('up').files.length",
            }),
        )
        .await;
    assert!(!files.is_error, "the page answers: {}", files.text);
    assert!(
        files.text.contains('1'),
        "the page's input holds the chosen file: {}",
        files.text
    );
    service.shutdown().await;
}

/// The panel hears the moment the Agent becomes the driver.
///
/// The tab strip mark and the pause control are read from the session snapshot,
/// and the snapshot only arrives when the runtime says something changed. The
/// flip used to be silent, so a panel that had already drawn its tabs showed
/// the mark on whichever tab happened to be repainted next — switching tabs
/// appeared to fix it.
#[tokio::test]
async fn an_agent_action_announces_the_takeover_once() {
    let Some((service, home, anonymous, _tab)) = service_with_page().await else {
        return;
    };
    let _home = home;
    // The fixture's readiness probe is itself an Agent action, so that session
    // already reports the Agent as the driver.
    assert_eq!(
        service
            .session_snapshot(&anonymous)
            .await
            .expect("snapshot")
            .session
            .execution_source,
        BrowserExecutionSource::Agent,
        "any tool call makes the Agent the driver, not just click and fill"
    );

    // A fresh session starts with the human as the driver.
    let workspace = WorkspaceId::new();
    let session = service
        .ensure_session(
            BrowserSessionKey::Workspace(workspace.clone()),
            Some(workspace),
        )
        .await
        .expect("a session");
    let tab = service
        .create_tab(
            &session,
            Some("data:text/html,<p>needle</p>"),
            BrowserTabOwner::Agent,
        )
        .await
        .expect("a tab");
    assert_eq!(
        service
            .session_snapshot(&session)
            .await
            .expect("snapshot")
            .session
            .execution_source,
        BrowserExecutionSource::User
    );

    let mut events = service.subscribe();
    let found = service
        .call_tool(
            &tool_context(&session),
            "browser_find",
            &serde_json::json!({ "tab_id": tab.as_str(), "query": "needle" }),
        )
        .await;
    assert!(!found.is_error, "the search runs: {}", found.text);

    // The flip reaches the panel, and only the flip: a later action must not
    // repeat it, or every tool call would repaint every tab.
    let mut takeovers = 0;
    let quiet = Instant::now() + Duration::from_millis(700);
    loop {
        let remaining = quiet.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Ok(BrowserServiceEvent::SessionChanged(changed))) if changed == session => {
                takeovers += 1;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert_eq!(takeovers, 1, "the takeover is announced exactly once");
    assert_eq!(
        service
            .session_snapshot(&session)
            .await
            .expect("snapshot")
            .session
            .execution_source,
        BrowserExecutionSource::Agent
    );

    let again = service
        .call_tool(
            &tool_context(&session),
            "browser_find",
            &serde_json::json!({ "tab_id": tab.as_str(), "query": "needle" }),
        )
        .await;
    assert!(!again.is_error, "the second search runs: {}", again.text);
    let mut repeated = 0;
    let quiet = Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = quiet.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Ok(BrowserServiceEvent::SessionChanged(changed))) if changed == session => {
                repeated += 1;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert_eq!(repeated, 0, "the source only moves once");
    service.shutdown().await;
}

/// Human input never pauses the Agent by itself.
///
/// The panel forwards every click, wheel and keystroke; a reader may use the
/// page while the Agent works, and the Agent's next call has to keep working.
/// Pausing is the panel's explicit control, which is the same state
/// `browser_request_help` sets.
#[tokio::test]
async fn human_input_does_not_pause_the_agent() {
    let Some((service, home, session_id, tab)) = service_with_page().await else {
        return;
    };
    let _home = home;
    assert!(!service.agent_operations_aborted(&session_id).await);

    // A click, a wheel and a keystroke, exactly as the panel forwards them.
    for input in [
        // Empty page space: the fixture's download link would start a download
        // and the file input would block on a chooser.
        vibex_browser::BrowserInput::MouseDown {
            x: 620.0,
            y: 420.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        },
        vibex_browser::BrowserInput::MouseUp {
            x: 620.0,
            y: 420.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        },
        vibex_browser::BrowserInput::Wheel {
            x: 400.0,
            y: 300.0,
            delta_x: 0.0,
            delta_y: 120.0,
        },
        vibex_browser::BrowserInput::InsertText {
            text: "typed by the reader".to_string(),
        },
        vibex_browser::BrowserInput::Key {
            event_type: "keyDown".to_string(),
            key: "a".to_string(),
            code: "KeyA".to_string(),
            text: Some("a".to_string()),
            modifiers: 0,
            windows_key_code: 65,
        },
    ] {
        service
            .dispatch_input(&tab, input)
            .await
            .expect("the panel input reaches the page");
    }
    assert!(
        !service.agent_operations_aborted(&session_id).await,
        "touching the page must not pause the Agent"
    );

    // The Agent's next call still runs, without a hand-back.
    let read = service
        .call_tool(
            &tool_context(&session_id),
            "browser_evaluate",
            &serde_json::json!({ "tab_id": tab.as_str(), "script": "1 + 1" }),
        )
        .await;
    assert!(
        !read.is_error,
        "the Agent keeps its turn through human input: {}",
        read.text
    );

    // Only the explicit pause stops it, and the hand-back re-arms it.
    service
        .pause_agent_operations(&tab)
        .await
        .expect("the panel can pause the Agent");
    assert!(service.agent_operations_aborted(&session_id).await);
    let refused = service
        .call_tool(
            &tool_context(&session_id),
            "browser_evaluate",
            &serde_json::json!({ "tab_id": tab.as_str(), "script": "1 + 1" }),
        )
        .await;
    assert!(refused.is_error, "a paused Agent's next call is refused");
    service.resume_agent_operations(&session_id).await;
    assert!(!service.agent_operations_aborted(&session_id).await);
    service.shutdown().await;
}

/// A refused download is announced once, not once per session.
///
/// Setting the policy installs the behaviour at the browser level as well as
/// the per-tab one every tab already carries, and Chrome reports the download
/// on both. Answering both announcements is what put two identical
/// notifications on screen for one refused save.
#[tokio::test]
async fn a_refused_download_is_announced_once() {
    let Some((service, home, session_id, tab)) = service_with_page().await else {
        return;
    };
    let _home = home;
    let downloads = service.downloads_dir().await;
    assert!(!service.downloads_enabled().await);
    // The reader turned downloads on and then off again at some point; that is
    // what leaves both behaviours in force.
    service.set_downloads_enabled(true).await;
    service.set_downloads_enabled(false).await;
    let mut events = service.subscribe();

    click_download_link(&service, &session_id, &tab).await;

    let deadline = Instant::now() + Duration::from_secs(15);
    let blocked = loop {
        let Some(download) = next_download(&mut events, deadline).await else {
            panic!("a refused download still reaches the panel");
        };
        if download.state == vibex_browser::BrowserDownloadState::Blocked {
            break download;
        }
    };
    assert_eq!(blocked.tab_id, tab);
    assert_eq!(blocked.file_name, "re_port_.txt");
    assert!(blocked.path.is_none(), "nothing was written for a refusal");
    assert_eq!(
        blocked.directory, downloads,
        "the refused download still names the folder the button should open"
    );

    // The duplicate announcement, if any, arrives immediately after the first.
    let quiet = Instant::now() + Duration::from_millis(600);
    while let Some(download) = next_download(&mut events, quiet).await {
        assert_ne!(
            download.state,
            vibex_browser::BrowserDownloadState::Blocked,
            "one refused save is one announcement"
        );
    }
    let written = std::fs::read_dir(&downloads)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(written, 0, "a denied download leaves no file behind");
    service.shutdown().await;
}
