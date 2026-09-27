//! The panel's own browser-shell replacements, against the system Chrome.
//!
//! Find-in-page, the hover cursor and downloads exist only as CDP calls into a
//! live page, so a unit test of the script string proves nothing about whether
//! Chrome accepts it or what it does to the DOM. The tests skip themselves when
//! the machine has no browser — that is the environment where the whole feature
//! is explicitly unavailable.

use std::time::{Duration, Instant};

use vibex_browser::{BrowserService, BrowserServiceConfig, BrowserSessionKey};
use vibex_core::BrowserTabOwner;

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
style='position:fixed;bottom:0;right:0;width:200px;height:40px;display:block'>save file</a>";
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
        Err(error) => {
            eprintln!("skipping the browser panel test: no usable browser ({error})");
            return None;
        }
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
    // The page has to be laid out before a point probe means anything.
    tokio::time::sleep(Duration::from_millis(300)).await;
    Some((service, home, session_id, tab))
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

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut landed = None;
    while Instant::now() < deadline {
        if let Ok(entries) = std::fs::read_dir(&downloads) {
            let names = entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| !name.ends_with(".crdownload"))
                .collect::<Vec<_>>();
            if let Some(name) = names.into_iter().next() {
                landed = Some(name);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let landed = landed.expect("the download lands in the runtime's own directory");
    assert_eq!(
        landed, "re_port_.txt",
        "path separators and reserved characters never reach the disk"
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
