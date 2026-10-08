//! Viewport layout and bounded screencast frames, against the system browser.

use std::time::Duration;

use vibex_browser::{BrowserService, BrowserServiceConfig, BrowserSessionKey, BrowserToolContext};
use vibex_core::{BrowserCaptureQuality, BrowserTabOwner, BrowserToolTier};

#[tokio::test]
async fn screencast_preserves_the_viewport_on_tall_wide_and_hidpi_displays() {
    let home = tempfile::tempdir().expect("temp home");
    let service = BrowserService::new(BrowserServiceConfig::new(home.path()));
    let session_id = match service
        .ensure_session(BrowserSessionKey::Anonymous, None)
        .await
    {
        Ok(session_id) => session_id,
        Err(error) if error.is_browser_missing() => {
            eprintln!("skipping the viewport test: no usable browser ({error})");
            return;
        }
        Err(error) => panic!("the embedded browser did not start: {error}"),
    };
    let tab = service
        .create_tab(
            &session_id,
            Some("data:text/html,<body style='margin:0;background:lightblue'>viewport"),
            BrowserTabOwner::User,
        )
        .await
        .expect("a tab");
    let context = BrowserToolContext {
        session_id,
        agent_session_id: None,
        workspace_id: None,
        authorized_roots: Vec::new(),
        tier: BrowserToolTier::Fine,
        approved_origins: Vec::new(),
    };
    let mut observations = Vec::new();
    for quality in [BrowserCaptureQuality::Standard, BrowserCaptureQuality::High] {
        let mut frames = service
            .subscribe_frames(&tab, quality)
            .await
            .expect("the screencast starts");
        for (width, height, scale) in [
            (960, 640, 1.0),
            (900, 2000, 1.0),
            (3000, 900, 1.0),
            (800, 1200, 2.0),
            (1000, 1800, 1.5),
        ] {
            service
                .set_viewport(&tab, width, height, scale)
                .await
                .expect("the viewport resizes while streaming");
            let outcome = service
                .call_tool(
                    &context,
                    "browser_evaluate",
                    &serde_json::json!({
                        "tab_id": tab.as_str(),
                        "script": "[innerWidth, innerHeight, devicePixelRatio]",
                    }),
                )
                .await;
            let frame = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let frame = frames.next().await.expect("a live frame stream");
                    if frame.format == quality.frame_format()
                        && frame.metadata.device_width == width as f64
                        && frame.metadata.device_height == height as f64
                    {
                        break frame;
                    }
                }
            })
            .await
            .expect("a frame with the resized viewport metadata");
            let image = image::load_from_memory(&frame.bytes).expect("a decodable frame");
            observations.push((width, height, scale, outcome, image.width(), image.height()));
        }
    }
    service.shutdown().await;

    for (width, height, scale, outcome, frame_width, frame_height) in observations {
        assert!(!outcome.is_error, "viewport query failed: {}", outcome.text);
        let metrics: Vec<f64> = serde_json::from_str(
            outcome
                .text
                .strip_prefix("Script result (JSON):\n")
                .expect("a script result"),
        )
        .expect("page viewport metrics");
        assert_eq!(metrics, [width as f64, height as f64, scale]);
        assert!(
            frame_width <= 2560 && frame_height <= 1600,
            "{width}x{height} at {scale}x encoded an oversized {frame_width}x{frame_height} frame"
        );
        // Integer encoder dimensions can round by one pixel, but must not
        // change the page's aspect ratio or crop either edge.
        assert!(
            (frame_width as f64 / frame_height as f64 - width as f64 / height as f64).abs()
                <= 1.0 / frame_height as f64,
            "{width}x{height} at {scale}x changed ratio to {frame_width}x{frame_height}"
        );
    }
}
