//! Viewport layout and native-resolution screencast frames, against the system browser.

use std::time::Duration;

use image::GenericImageView as _;
use vibex_browser::{
    BrowserFrameSubscription, BrowserInput, BrowserService, BrowserServiceConfig,
    BrowserSessionKey, BrowserToolContext, BrowserToolOutcome,
};
use vibex_core::{BrowserCaptureQuality, BrowserFrame, BrowserTabOwner, BrowserToolTier};

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
<div role='button' aria-label='Flow target' onclick='window.agentClicks=(window.agentClicks||0)+1'\
 style='position:absolute;left:300px;top:500px;width:40px;height:40px;background:fuchsia'></div>\
<script>document.onmousedown=e=>window.lastClick=[e.clientX,e.clientY];\
onscroll=()=>document.querySelector('i').style.backgroundColor=scrollX||scrollY?'yellow':'red'</script>";

#[tokio::test]
async fn screencast_preserves_layout_and_resolution_on_tall_wide_and_hidpi_displays() {
    check_viewports(Vec::new(), false).await;
}

#[tokio::test]
async fn screencast_preserves_layout_and_resolution_on_a_scaled_browser_host() {
    check_viewports(vec!["--force-device-scale-factor=2".to_string()], false).await;
}

#[tokio::test]
async fn screencast_keeps_scrolled_content_attached_to_the_viewport() {
    check_viewports(Vec::new(), true).await;
}

#[tokio::test]
async fn screencast_keeps_scrolled_content_attached_on_a_scaled_browser_host() {
    check_viewports(vec!["--force-device-scale-factor=2".to_string()], true).await;
}

async fn check_viewports(extra_flags: Vec<String>, scroll: bool) {
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
    let page = if scroll {
        PAGE.replace(
            "body{margin:0;background:white}",
            "html{scrollbar-width:none}body{margin:0;background:white;width:500vw;height:500vh}",
        )
    } else {
        PAGE.to_string()
    };
    let tab = service
        .create_tab(&session_id, Some(&page), BrowserTabOwner::User)
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
                        "script": "scrollTo(0, 0); [innerWidth, innerHeight, devicePixelRatio]",
                    }),
                )
                .await;
            let metrics = numeric_result(&outcome);
            let offsets: &[(f64, f64)] = if scroll {
                &[(200.0, 300.0), (0.0, 0.0)]
            } else {
                &[(0.0, 0.0)]
            };
            let mut previous_offset = (0.0, 0.0);
            for &offset in offsets {
                if offset != previous_offset {
                    service
                        .dispatch_input(
                            &tab,
                            BrowserInput::Wheel {
                                x: width as f64 / 2.0,
                                y: height as f64 / 2.0,
                                delta_x: offset.0 - previous_offset.0,
                                delta_y: offset.1 - previous_offset.1,
                            },
                        )
                        .await
                        .expect("the wheel reaches the page");
                    previous_offset = offset;
                }
                let frame = complete_frame(
                    &service,
                    &mut frames,
                    quality,
                    (width, height, scale),
                    offset,
                )
                .await;
                // Convert a point three quarters across the painted frame as
                // the surface does, using metadata rather than encoded pixels.
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
                            "script": "[...window.lastClick, scrollX, scrollY]",
                        }),
                    )
                    .await;
                observations.push((
                    quality,
                    (width, height, scale),
                    metrics.clone(),
                    click,
                    frame.metadata,
                    offset,
                ));
            }
        }
    }
    // Agent clicks resolve a DOM box before reaching the shared input path.
    let agent_click = service
        .call_tool(
            &context,
            "browser_click_by_name",
            &serde_json::json!({ "tab_id": tab.as_str(), "name": "Flow target" }),
        )
        .await;
    let agent_effect = service
        .call_tool(
            &context,
            "browser_evaluate",
            &serde_json::json!({ "tab_id": tab.as_str(), "script": "[window.agentClicks || 0]" }),
        )
        .await;
    service.shutdown().await;

    assert!(
        !agent_click.is_error,
        "agent click failed: {}",
        agent_click.text
    );
    assert_eq!(
        numeric_result(&agent_effect),
        [1.0],
        "the agent clicked the DOM target"
    );
    for (quality, (width, height, scale), metrics, click, metadata, offset) in observations {
        assert_eq!(
            metrics,
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
                (height as f64 * 0.75).floor(),
                offset.0,
                offset.1,
            ],
            "{quality:?}: {width}x{height} at {scale}x must not move a click"
        );
    }
}

async fn complete_frame(
    service: &BrowserService,
    frames: &mut BrowserFrameSubscription,
    quality: BrowserCaptureQuality,
    (width, height, scale): (u32, u32, f64),
    offset: (f64, f64),
) -> BrowserFrame {
    let expected_size = (
        (width as f64 * scale).round() as u32,
        (height as f64 * scale).round() as u32,
    );
    let mut last_frame = None;
    let captured = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = frames.next().await.expect("a live frame stream");
            let decoded = image::load_from_memory(&frame.bytes).expect("a decodable frame");
            if frame.format != quality.frame_format()
                || decoded.width().abs_diff(expected_size.0) > 1
                || decoded.height().abs_diff(expected_size.1) > 1
            {
                continue;
            }
            let corners = corner_pixels(&decoded);
            let flow = decoded.get_pixel(
                ((320.0 - offset.0) * scale).round() as u32,
                ((520.0 - offset.1) * scale).round() as u32,
            );
            // A scroll listener recolors the first fixed corner. Waiting for
            // that paint catches drift hidden by the earlier compositor frame.
            let complete =
                corners_match(&corners, offset != (0.0, 0.0)) && color_matches(flow, [255, 0, 255]);
            let scrolled = (frame.metadata.scroll_offset_x - offset.0).abs() < 0.5
                && (frame.metadata.scroll_offset_y - offset.1).abs() < 0.5;
            let sharp = scale != 2.0 || has_native_pixel_detail(&decoded);
            last_frame = Some((frame.metadata, corners, flow, sharp));
            if complete && scrolled && sharp {
                break frame;
            }
        }
    })
    .await;
    match captured {
        Ok(frame) => frame,
        Err(_) => {
            service.shutdown().await;
            panic!(
                "{quality:?}: {width}x{height} at {scale}x, scroll {offset:?}, expected {expected_size:?}, last matching frame {last_frame:?}"
            );
        }
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

fn corners_match(corners: &[image::Rgba<u8>; 4], scrolled: bool) -> bool {
    corners
        .iter()
        .zip([
            if scrolled { [255, 255, 0] } else { [255, 0, 0] },
            [0, 255, 0],
            [0, 0, 255],
            [0, 0, 0],
        ])
        .all(|(actual, expected)| color_matches(*actual, expected))
}

fn color_matches(actual: image::Rgba<u8>, expected: [u8; 3]) -> bool {
    actual.0[..3]
        .iter()
        .zip(expected)
        .all(|(actual, expected)| actual.abs_diff(expected) <= 12)
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
