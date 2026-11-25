//! The brand table is a port of the desktop's, and these are the two things the
//! porting can get wrong: which mark an identity resolves to, and whether a
//! themed mark comes out in the caller's colour rather than black.

use super::*;

/// The priority the desktop established: a brand with a mark of its own beats
/// the catalogue, and a mark matched by a command beats the Agent's own id.
#[test]
fn a_brand_of_its_own_is_tested_before_the_catalogue() {
    // "opencode gemini" is OpenCode, not Gemini: the inline order is the
    // desktop's, and it is what a custom Agent whose command is `opencode`
    // and whose preset is `gemini` resolves to.
    assert_eq!(
        agent_mark("custom-acp Custom Agent opencode gemini"),
        Some(mark!("opencode.svg", false))
    );
    assert_eq!(agent_mark("gemini"), Some(mark!("gemini.svg", false)));
    assert_eq!(
        agent_mark("GitHub Copilot"),
        Some(mark!("copilot.svg", true))
    );
    assert_eq!(agent_mark("OpenAI Codex"), Some(mark!("openai.svg", true)));
    assert_eq!(agent_mark("Claude Code"), Some(mark!("claude.svg", false)));
    assert_eq!(agent_mark("tongyi-qwen"), Some(mark!("qwen.svg", false)));
}

/// A catalogue needle matches anywhere in the joined identity, which is what
/// lets an id and a label name the same Agent without the caller choosing one.
#[test]
fn a_catalogue_needle_matches_the_joined_identity() {
    assert_eq!(
        agent_mark(&identity(&["deepseek-harness", "DeepSeek Harness"])),
        Some(mark!("agents/deepseek-harness.svg", false))
    );
    assert_eq!(
        agent_mark("zcode"),
        Some(mark!("agents/glm-acp-agent.svg", true))
    );
}

/// An Agent nothing matches wears no mark, and the caller answers with its name
/// instead of a blank: this is the fallback the whole feature rests on.
#[test]
fn an_unknown_agent_has_no_mark() {
    assert_eq!(agent_mark("custom acp"), None);
    assert_eq!(agent_mark(""), None);
}

/// Every mark the table names must actually be bundled, or a matched Agent
/// would fail to draw at the last moment.
#[test]
fn every_catalogue_mark_is_bundled() {
    let mut checked = 0;
    for (_, mark) in CATALOG_AGENT_BRANDS {
        assert!(
            mark_bytes(mark.path).is_some(),
            "the catalogue names {} but it is not bundled",
            mark.path
        );
        checked += 1;
    }
    assert!(checked > 0, "the catalogue should not be empty");
}

/// A themed mark is drawn in the caller's colour; a colored mark is left alone.
/// This is the one distinction the raster path makes, so it is asserted on the
/// text the renderer is handed rather than on pixels.
#[test]
fn a_themed_mark_resolves_current_color_and_a_colored_one_does_not() {
    let themed = mark!("agents/cursor.svg", true);
    let source = mark_svg(themed, Some(0x11_2233)).unwrap();
    assert!(
        source.contains("#112233"),
        "themed marks take the caller's colour"
    );
    assert!(
        !source.contains("currentColor"),
        "no unresolved keyword is left"
    );

    let colored = mark!("agents/deepseek-harness.svg", false);
    let source = mark_svg(colored, Some(0x11_2233)).unwrap();
    assert!(
        !source.contains("#112233"),
        "a colored mark keeps its own brand colours"
    );
}

/// The point of the feature: a mark becomes a picture the terminal can draw.
#[cfg(feature = "raster")]
#[test]
fn a_mark_rasters_to_a_square_png() {
    let png = mark_png(mark!("agents/deepseek-harness.svg", false), 48, None)
        .expect("a bundled mark should render");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    let decoded = image::load_from_memory(&png).expect("the bytes are a PNG");
    assert_eq!((decoded.width(), decoded.height()), (48, 48));
}

/// A themed mark rastered without a colour has nothing to draw it in, and says
/// so rather than producing a black blob the caller cannot tell from a mark.
#[cfg(feature = "raster")]
#[test]
fn a_themed_mark_without_a_colour_is_no_picture() {
    assert_eq!(mark_png(mark!("agents/cursor.svg", true), 32, None), None);
}

/// A client with icons of its own gets the same rasterizer: the bytes it
/// carries are turned into a square PNG in the colour it asks for, without the
/// brand table being involved at all.
#[cfg(feature = "raster")]
#[test]
fn any_svg_becomes_a_square_png() {
    let svg = themed_svg(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M4 4h16v16H4z" fill="currentColor"/></svg>"#,
        Some(0x33_99ff),
    )
    .expect("a colour to draw in");
    let png = svg_png(&svg, 40).expect("a well-formed SVG should render");
    let decoded = image::load_from_memory(&png).expect("the bytes are a PNG");
    assert_eq!((decoded.width(), decoded.height()), (40, 40));
    // The colour asked for is the colour drawn, which is the whole reason the
    // keyword is rewritten before the parse.
    let pixel = decoded.to_rgba8().get_pixel(20, 20).0;
    assert_eq!(&pixel[..3], &[0x33, 0x99, 0xff]);
}

/// SVG text that is not drawable is `None` rather than a panic, so a caller
/// falls back to a glyph instead of taking the frame down.
#[cfg(feature = "raster")]
#[test]
fn undrawable_svg_is_refused() {
    assert_eq!(svg_png("not an svg at all", 32), None);
    assert_eq!(svg_png("", 32), None);
    assert_eq!(svg_png("<svg/>", 0), None);
}

/// Every mark named by the model and provider tables must also resolve through
/// `mark_bytes`, so a matched model or provider never silently drops back to a
/// generic glyph.
#[test]
fn every_model_and_provider_mark_is_bundled() {
    for (name, mark) in OPENROUTER_PROVIDER_BRANDS {
        assert!(
            mark_bytes(mark.path).is_some(),
            "provider brand {name} names {} but it is not bundled",
            mark.path
        );
    }
    for (prefix, mark) in OPENROUTER_MODEL_BRAND_PREFIXES {
        assert!(
            mark_bytes(mark.path).is_some(),
            "model prefix {prefix} names {} but it is not bundled",
            mark.path
        );
    }
    for rule in OPENROUTER_MODEL_BRANDS {
        assert!(
            mark_bytes(rule.asset.path).is_some(),
            "model rule {} names {} but it is not bundled",
            rule.keyword,
            rule.asset.path
        );
    }
    for sample in [
        "openrouter",
        "deepseek",
        "deepseek-chat",
        "gpt-5",
        "claude-4",
    ] {
        let mark = model_mark(sample).expect("known brand should resolve");
        assert!(
            mark_bytes(mark.path).is_some(),
            "{sample} resolved to {} which is not bundled",
            mark.path
        );
    }
}

/// The Antigravity mark must avoid `<mask>` and `<filter>` because mobile's
/// `nsh` SVG renderer skips both, which turns a masked blur stack into a solid
/// colourful rectangle.
#[test]
fn antigravity_mark_uses_no_masks_or_filters() {
    let source = mark_svg(mark!("agents/antigravity.svg", false), None)
        .expect("antigravity mark should be bundled");
    assert!(
        !source.contains("<mask"),
        "antigravity.svg must not rely on <mask> elements"
    );
    assert!(
        !source.contains("<filter"),
        "antigravity.svg must not rely on <filter> elements"
    );
}

#[cfg(feature = "raster")]
#[test]
fn antigravity_mark_rasters_as_an_arch_with_transparent_corners() {
    let png = mark_png(mark!("agents/antigravity.svg", false), 64, None)
        .expect("antigravity mark should rasterize");
    let image = image::load_from_memory(&png)
        .expect("the bytes are a PNG")
        .to_rgba8();
    assert_eq!(image.get_pixel(0, 0).0[3], 0, "top-left corner stays transparent");
    assert_eq!(image.get_pixel(63, 0).0[3], 0, "top-right corner stays transparent");
    assert_eq!(
        image.get_pixel(32, 60).0[3],
        0,
        "inner arch cutout stays transparent"
    );
    assert!(
        image.get_pixel(31, 16).0[3] > 200,
        "arch body near the apex is opaque"
    );
}
