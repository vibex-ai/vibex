//! Viewport layout and native-resolution screencast frames, against the system browser.

use std::time::Duration;

use image::GenericImageView as _;
use vibex_browser::{
    BrowserInput, BrowserService, BrowserServiceConfig, BrowserSessionKey, BrowserToolContext,
    BrowserToolOutcome,
};
use vibex_core::{BrowserCaptureQuality, BrowserTabOwner, BrowserToolTier};

// Distinct corners catch a larger capture surface that crops or stretches only
// part of the page. All frames stay in memory; no screenshots are written.
const PAGE: &str = "data:text/html,<style>body{margin:0;background:white}\
i{position:fixed;width:40px;height:40px}</style>\
<i style='left:0;top:0;background:red'></i>\
<i style='right:0;top:0;background:lime'></i>\
<i style='left:0;bottom:0;background:blue'></i>\
<i style='right:0;bottom:0;background:black'></i>\
<div style='position:fixed;left:0;top:80px;width:40px;height:16px;\
background:repeating-linear-gradient(90deg,black 0 .5px,white .5px 1px)'></div>\
<script>document.onmousedown=e=>window.lastClick=[e.clientX,e.clientY]</script>";

#[tokio::test]
async fn screencast_preserves_layout_and_resolution_on_tall_wide_and_hidpi_displays() {
    check_viewports(Vec::new()).await;
}

#[tokio::test]
async fn screencast_preserves_layout_and_resolution_on_a_scaled_browser_host() {
    check_viewports(vec!["--force-device-scale-factor=2".to_string()]).await;
}

async fn check_viewports(extra_flags: Vec<String>) {
    let home = tempfile::tempdir().expect("temp home");
    let mut config = BrowserServiceConfig::new(home.path());
    config.extra_flags = extra_flags;
    let service = BrowserService::new(config);
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
        .create_tab(&session_id, Some(PAGE), BrowserTabOwner::User)
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
            (1280, 800, 1.0),
            (960, 640, 1.0),
            (900, 2000, 1.0),
            (3000, 900, 1.0),
            (800, 1200, 2.0),
            (800, 1200, 1.0),
            (1000, 1800, 1.5),
            (1001, 1801, 1.25),
            (1920, 1080, 2.0),
            (1080, 1920, 2.0),
        ] {
            let expected_size = (
                (width as f64 * scale).round() as u32,
                (height as f64 * scale).round() as u32,
            );
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
            let mut last_size = None;
            let mut last_corners = None;
            let mut last_detail = None;
            let captured = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let frame = frames.next().await.expect("a live frame stream");
                    let decoded = image::load_from_memory(&frame.bytes).expect("a decodable frame");
                    last_size = Some((decoded.width(), decoded.height()));
                    // A frame already being encoded during resize may arrive
                    // first. Require the full display resolution to settle;
                    // checking only metadata or an upper bound misses blur.
                    if frame.format == quality.frame_format()
                        && decoded.width().abs_diff(expected_size.0) <= 1
                        && decoded.height().abs_diff(expected_size.1) <= 1
                    {
                        let corners = corner_pixels(&decoded);
                        let complete = corners_match(&corners);
                        last_corners = Some(corners);
                        // At 2x, the half-CSS-pixel stripes must retain their
                        // one-device-pixel contrast. Larger image dimensions
                        // alone would let an upscaled low-resolution raster pass.
                        let sharp = scale != 2.0 || has_native_pixel_detail(&decoded);
                        last_detail = Some(sharp);
                        if complete && sharp {
                            break frame;
                        }
                    }
                }
            })
            .await;
            let frame = match captured {
                Ok(captured) => captured,
                Err(_) => {
                    service.shutdown().await;
                    panic!(
                        "{quality:?}: {width}x{height} at {scale}x expected {expected_size:?}, last frame was {last_size:?}, corners {last_corners:?}, native detail {last_detail:?}"
                    );
                }
            };
            // The surface converts a point three quarters across the painted
            // frame using these metadata fields, not the encoded pixel count.
            let x = frame.metadata.device_width * 0.75 / frame.metadata.page_scale_factor;
            let y = frame.metadata.device_height * 0.75 / frame.metadata.page_scale_factor;
            for input in [
                BrowserInput::MouseDown {
                    x,
                    y,
                    button: "left".to_string(),
                    click_count: 1,
                    modifiers: 0,
                },
                BrowserInput::MouseUp {
                    x,
                    y,
                    button: "left".to_string(),
                    click_count: 1,
                    modifiers: 0,
                },
            ] {
                service.dispatch_input(&tab, input).await.expect("a click");
            }
            let click = service
                .call_tool(
                    &context,
                    "browser_evaluate",
                    &serde_json::json!({
                        "tab_id": tab.as_str(),
                        "script": "window.lastClick",
                    }),
                )
                .await;
            observations.push((
                quality,
                (width, height, scale),
                outcome,
                click,
                frame.metadata,
            ));
        }
    }
    service.shutdown().await;

    for (quality, (width, height, scale), outcome, click, metadata) in observations {
        assert_eq!(
            numeric_result(&outcome),
            [width as f64, height as f64, scale],
            "{quality:?}: {width}x{height} at {scale}x must preserve CSS layout"
        );
        assert_eq!(
            (metadata.device_width, metadata.device_height),
            (width as f64, height as f64),
            "published metadata must stay in the page's input coordinate system"
        );
        assert_eq!(
            numeric_result(&click),
            [
                (width as f64 * 0.75).floor(),
                (height as f64 * 0.75).floor()
            ],
            "{quality:?}: {width}x{height} at {scale}x must not move a click"
        );
    }
}

fn corner_pixels(image: &image::DynamicImage) -> [image::Rgba<u8>; 4] {
    [
        image.get_pixel(8, 8),
        image.get_pixel(image.width() - 9, 8),
        image.get_pixel(8, image.height() - 9),
        image.get_pixel(image.width() - 9, image.height() - 9),
    ]
}

fn corners_match(corners: &[image::Rgba<u8>; 4]) -> bool {
    corners
        .iter()
        .zip([[255, 0, 0], [0, 255, 0], [0, 0, 255], [0, 0, 0]])
        .all(|(actual, expected)| {
            actual.0[..3]
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.abs_diff(expected) <= 12)
        })
}

fn has_native_pixel_detail(image: &image::DynamicImage) -> bool {
    (8..24).all(|x| {
        let value = image.get_pixel(x, 168).0[0];
        if x % 2 == 0 { value < 40 } else { value > 215 }
    })
}

fn numeric_result(outcome: &BrowserToolOutcome) -> Vec<f64> {
    assert!(!outcome.is_error, "page query failed: {}", outcome.text);
    serde_json::from_str(
        outcome
            .text
            .strip_prefix("Script result (JSON):\n")
            .expect("a script result"),
    )
    .expect("numeric page metrics")
}
