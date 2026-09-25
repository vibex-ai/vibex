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
