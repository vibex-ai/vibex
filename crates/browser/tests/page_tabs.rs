//! Page-opened tabs, tab closing and screencast restart, against the system
//! Chrome.
//!
//! These behaviours only exist in the CDP target lifecycle, so they are checked
//! against a real browser. The test skips itself when the machine has none —
//! that is the environment where the whole feature is explicitly unavailable.

use std::time::{Duration, Instant};

use vibex_browser::{
    BrowserInput, BrowserService, BrowserServiceConfig, BrowserServiceEvent, BrowserSessionKey,
};
use vibex_core::BrowserTabOwner;

/// A page whose whole viewport is a `target="_blank"` link.
const PAGE: &str = "data:text/html,<a id=l href='about:blank' target='_blank' \
style='position:fixed;inset:0;display:block'>open</a>";

#[tokio::test]
async fn a_page_opened_tab_is_adopted_and_closed_like_any_other() {
    let home = tempfile::tempdir().expect("temp home");
    let service = BrowserService::new(BrowserServiceConfig::new(home.path()));
    let session_id = match service
        .ensure_session(BrowserSessionKey::Anonymous, None)
        .await
    {
        Ok(session_id) => session_id,
        Err(error) => {
            eprintln!("skipping the browser tab test: no usable browser ({error})");
            return;
        }
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
            .subscribe_frames(&first)
            .await
            .expect("the screencast restarts");
    }

    // Clicking the full-viewport link opens a tab of its own.
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

    let (opened_session, opened) = wait_for_opened_tab(&mut events).await;
    assert_eq!(
        opened_session, session_id,
        "the page-opened tab belongs to the opener's session"
    );
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
    let session_id = match service
        .ensure_session(BrowserSessionKey::Anonymous, None)
        .await
    {
        Ok(session_id) => session_id,
        Err(error) => {
            eprintln!("skipping the browser tab test: no usable browser ({error})");
            return;
        }
    };
    let page = format!("http://127.0.0.1:{port}/");
    let mut events = service.subscribe();
    let opener = service
        .create_tab(&session_id, Some(&page), BrowserTabOwner::User)
        .await
        .expect("a tab");
    service.set_viewport(&opener, 800, 600, 1.0).await.ok();
    service.subscribe_frames(&opener).await.ok();
    tokio::time::sleep(Duration::from_millis(700)).await;

    // The top half opens a tab; the bottom half navigates in this one.
    click_at(&service, &opener, 150.0).await;
    assert!(
        wait_for_opened_tab(&mut events).await.0 == session_id,
        "the popup belongs to the opener's session"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    click_at(&service, &opener, 500.0).await;
    tokio::time::sleep(Duration::from_millis(700)).await;

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
    assert!(
        url.ends_with("/landed"),
        "the opener ignored the click after its page opened a tab: {url}"
    );
    service.shutdown().await;
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
