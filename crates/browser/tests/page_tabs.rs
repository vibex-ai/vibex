//! Page-opened tabs, tab closing, screencast restart and a browser restart,
//! against the system Chrome.
//!
//! These behaviours only exist in the CDP target lifecycle, so they are checked
//! against a real browser. A machine that has none skips them — that is the
//! environment where the whole feature is explicitly unavailable. Any other
//! failure is a defect and fails the test: a broken transport reported as "no
//! browser" is how the Windows panel shipped unable to open a single tab.

use std::time::{Duration, Instant};

use vibex_browser::{
    BrowserInput, BrowserService, BrowserServiceConfig, BrowserServiceEvent, BrowserSessionKey,
};
use vibex_core::{BrowserCaptureQuality, BrowserTabOwner};

/// Opens a session, or skips when this machine has no browser to open one in.
async fn session_or_skip(service: &BrowserService) -> Option<vibex_core::BrowserSessionId> {
    match service
        .ensure_session(BrowserSessionKey::Anonymous, None)
        .await
    {
        Ok(session_id) => Some(session_id),
        Err(error) if error.is_browser_missing() => {
            eprintln!("skipping the browser tab test: no usable browser ({error})");
            None
        }
        Err(error) => panic!("the embedded browser did not start: {error}"),
    }
}

/// A page whose whole viewport is a `target="_blank"` link.
const PAGE: &str = "data:text/html,<a id=l href='about:blank' target='_blank' \
style='position:fixed;inset:0;display:block'>open</a>";

#[tokio::test]
async fn a_page_opened_tab_is_adopted_and_closed_like_any_other() {
    let home = tempfile::tempdir().expect("temp home");
    let service = BrowserService::new(BrowserServiceConfig::new(home.path()));
    let Some(session_id) = session_or_skip(&service).await else {
        return;
    };
    let mut events = service.subscribe();
    let first = service
        .create_tab(&session_id, Some(PAGE), BrowserTabOwner::User)
        .await
        .expect("a tab");
    service
        .set_viewport(&first, 900, 600, 1.0)
        .await
        .expect("a viewport");
    // Starting the screencast again on a live tab must not fail with
    // "Screencast is already active": reopening a panel tab takes this path.
    for _ in 0..2 {
        service
            .subscribe_frames(&first, BrowserCaptureQuality::Standard)
            .await
            .expect("the screencast restarts");
    }

    // Clicking the full-viewport link opens a tab of its own.
    wait_for_link_at(&service, &first, 100.0).await;
    for input in [
        BrowserInput::MouseDown {
            x: 100.0,
            y: 100.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        },
        BrowserInput::MouseUp {
            x: 100.0,
            y: 100.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        },
    ] {
        service
            .dispatch_input(&first, input)
            .await
            .expect("the click reaches the page");
    }

    // The tab this test created announces itself first; the popup is the one
    // after it.
    let (created_session, created_tab) = wait_for_opened_tab(&mut events).await;
    assert_eq!(created_session, session_id);
    assert_eq!(created_tab, first);
    let (opened_session, opened) = wait_for_opened_tab(&mut events).await;
    assert_eq!(
        opened_session, session_id,
        "the page-opened tab belongs to the opener's session"
    );
    assert_ne!(opened, first, "the popup is a tab of its own");
    let snapshot = service
        .session_snapshot(&session_id)
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot.session.tabs.len(),
        2,
        "the page-opened tab is a tab of its own"
    );

    service.close_tab(&opened).await.expect("close");
    let snapshot = service
        .session_snapshot(&session_id)
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot.session.tabs.len(),
        1,
        "closing a tab removes it from the session"
    );
    service.shutdown().await;
}

async fn wait_for_opened_tab(
    events: &mut tokio::sync::broadcast::Receiver<BrowserServiceEvent>,
) -> (vibex_core::BrowserSessionId, vibex_core::BrowserTabId) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            Ok(Ok(BrowserServiceEvent::TabOpened { session_id, tab_id })) => {
                return (session_id, tab_id);
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }
    panic!("the click never opened a tab");
}

/// The opener keeps working after its page opened a tab of its own. The tab an
/// agent or a human is looking at must not go deaf because a second one
/// appeared.
#[tokio::test]
async fn the_opener_keeps_receiving_input_after_it_opens_a_tab() {
    let home = tempfile::tempdir().expect("temp home");
    let port = serve_pages();
    let service = BrowserService::new(BrowserServiceConfig::new(home.path()));
    let Some(session_id) = session_or_skip(&service).await else {
        return;
    };
    let page = format!("http://127.0.0.1:{port}/");
    let mut events = service.subscribe();
    let opener = service
        .create_tab(&session_id, Some(&page), BrowserTabOwner::User)
        .await
        .expect("a tab");
    service.set_viewport(&opener, 800, 600, 1.0).await.ok();
    service
        .subscribe_frames(&opener, BrowserCaptureQuality::Standard)
        .await
        .ok();
    // The click is synthesized at a point, so the page has to be laid out before
    // it means anything. A fixed sleep here clicked into an empty document when
    // four browsers started at once, and the popup then never opened.
    wait_for_link_at(&service, &opener, 150.0).await;

    // The tab this test created announces itself first.
    let (created_session, created_tab) = wait_for_opened_tab(&mut events).await;
    assert_eq!(created_session, session_id);
    assert_eq!(created_tab, opener);
    // The top half opens a tab; the bottom half navigates in this one.
    click_at(&service, &opener, 150.0).await;
    let (opened_session, opened_tab) = wait_for_opened_tab(&mut events).await;
    assert_eq!(
        opened_session, session_id,
        "the popup belongs to the opener's session"
    );
    assert_ne!(opened_tab, opener);
    tokio::time::sleep(Duration::from_millis(300)).await;
    click_at(&service, &opener, 500.0).await;

    // A same-tab navigation commits asynchronously: the address is the signal
    // that it did, and waiting a fixed interval raced it under load.
    let deadline = Instant::now() + Duration::from_secs(10);
    let url = loop {
        let snapshot = service
            .session_snapshot(&session_id)
            .await
            .expect("snapshot");
        let url = snapshot
            .session
            .tabs
            .iter()
            .find(|tab| tab.tab_id == opener)
            .map(|tab| tab.url.clone())
            .unwrap_or_default();
        if url.ends_with("/landed") || Instant::now() >= deadline {
            break url;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        url.ends_with("/landed"),
        "the opener ignored the click after its page opened a tab: {url}"
    );
    service.shutdown().await;
}

/// Waits until a link is laid out under the point a test is about to click.
///
/// `cursor_at` reads `getComputedStyle(el).cursor` at the point, so a
/// `pointer` answer is proof that the link is where the click will land — the
/// same question `elementFromPoint` answers for the synthesized click itself.
async fn wait_for_link_at(service: &BrowserService, tab: &vibex_core::BrowserTabId, y: f64) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let cursor = service
            .cursor_at(tab, 60.0, y)
            .await
            .unwrap_or_else(|error| panic!("the cursor probe answers: {error}"));
        if cursor == "pointer" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the fixture page never laid out a link at y={y}: the cursor there is `{cursor}`"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn click_at(service: &BrowserService, tab: &vibex_core::BrowserTabId, y: f64) {
    for input in [
        BrowserInput::MouseDown {
            x: 60.0,
            y,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        },
        BrowserInput::MouseUp {
            x: 60.0,
            y,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        },
    ] {
        service
            .dispatch_input(tab, input)
            .await
            .expect("the click reaches the page");
    }
}

/// Serves a page whose top half opens a tab and whose bottom half navigates in
/// place, plus the two destinations.
fn serve_pages() -> u16 {
    use std::io::{Read, Write};

    const START: &str = "<title>start</title>\
<a href='/popup' target='_blank' style='position:fixed;inset:0 0 50% 0;display:block'>popup</a>\
<a href='/landed' style='position:fixed;inset:50% 0 0 0;display:block'>same tab</a>";
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer);
            let request = String::from_utf8_lossy(&buffer);
            let body = if request.contains("GET /landed") {
                "<title>landed</title>landed"
            } else if request.contains("GET /popup") {
                "<title>popup</title>popup"
            } else {
                START
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    port
}

/// A tab an Agent creates has to be announced too: the panel shows the tabs of
/// a session it knows about, and without the event an Agent's pages lived in a
/// session the panel never followed — which looks exactly like an Agent that
/// lied about opening a browser.
#[tokio::test]
async fn a_tab_created_for_an_agent_is_announced() {
    let home = tempfile::tempdir().expect("temp home");
    let service = BrowserService::new(BrowserServiceConfig::new(home.path()));
    let Some(session_id) = session_or_skip(&service).await else {
        return;
    };
    let mut events = service.subscribe();
    let tab = service
        .create_tab(&session_id, Some("about:blank"), BrowserTabOwner::Agent)
        .await
        .expect("a tab");
    let (announced_session, announced_tab) = wait_for_opened_tab(&mut events).await;
    assert_eq!(announced_session, session_id);
    assert_eq!(announced_tab, tab, "the panel is told which tab appeared");
    service.shutdown().await;
}

/// A second browser on the same profile directory must not be handed the
/// endpoint of the one that was just killed.
///
/// The runtime owns one profile directory per browser family for the life of
/// the install, and a browser it terminated leaves `DevToolsActivePort` behind.
/// The loopback-port transport — the one Windows uses — reads that file to find
/// the endpoint, so the leftover pointed every later run at a dead port and a
/// `/devtools/browser/<uuid>` nobody answered: the browser started, and the tab
/// still never opened.
#[tokio::test]
async fn a_restarted_browser_reopens_a_tab_on_the_same_profile() {
    let home = tempfile::tempdir().expect("temp home");
    let first = BrowserService::new(BrowserServiceConfig::new(home.path()));
    let Some(session_id) = session_or_skip(&first).await else {
        return;
    };
    let tab = first
        .create_tab(&session_id, Some("about:blank"), BrowserTabOwner::User)
        .await
        .expect("a tab in the first browser");
    first.close_tab(&tab).await.expect("close");
    // Killing the browser is what leaves the endpoint file behind.
    first.shutdown().await;

    let second = BrowserService::new(BrowserServiceConfig::new(home.path()));
    let session_id = second
        .ensure_session(BrowserSessionKey::Anonymous, None)
        .await
        .expect("the restarted browser starts");
    let tab = second
        .create_tab(&session_id, Some("about:blank"), BrowserTabOwner::User)
        .await
        .expect("a tab in the restarted browser");
    second.close_tab(&tab).await.expect("close");
    second.shutdown().await;
}
