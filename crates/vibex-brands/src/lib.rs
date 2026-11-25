//! Which mark belongs to which Agent, and the bytes of that mark.
//!
//! Every Vibex client that lists Agents — the desktop workbench, the phone, and
//! the terminal — answers the same question: given whatever the runtime calls an
//! Agent, which brand mark is it? The answer has to be the same everywhere or
//! the same Agent wears two faces, so it lives here once rather than in each
//! client.
//!
//! # The identity is a string, not an id
//!
//! The match is deliberately loose. The runtime's `AgentId` is whatever an
//! Agent definition declared, and a reader's own Agent can be an arbitrary ACP
//! command; the mark is therefore chosen from the Agent's id, its label, its
//! command and its preset joined together — so a custom Agent whose command is
//! `opencode` wears the OpenCode mark, exactly as the desktop decides it. A
//! caller builds that string with [`identity`] rather than joining it itself,
//! because the order and separators are part of the answer.
//!
//! # What a mark is not
//!
//! A mark is either *colored* (it carries its own brand colours) or *themed*
//! (it is drawn in the reader's foreground). That one bit is the whole of the
//! metadata a renderer needs, and it is deliberately also the whole of what
//! crosses into this crate from the desktop's asset table: positions, sizes and
//! intrinsic widths are a renderer's business, not a brand's.
//!
//! # Rasterization is a feature
//!
//! A client that has its own renderer (a GPUI window, an SVG engine) wants only
//! [`agent_mark`]. A client that has to produce pixels — the terminal, whose
//! image protocol wants a PNG — turns on `raster` and calls [`mark_png`].

#![forbid(unsafe_code)]

/// One brand mark, without its bytes.
///
/// `path` is the asset path the desktop uses, relative to its `icons/vibex/`
/// root, so a client that serves its own asset source can look the same mark up
/// there; the bytes are also available through [`mark_bytes`] so a client that
/// has no asset source can carry them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// The asset path, relative to the desktop pack's `icons/vibex/`.
    pub path: &'static str,
    /// Whether the mark is drawn in the reader's own colour rather than in the
    /// colours baked into it.
    pub uses_current_color: bool,
}

macro_rules! mark {
    ($path:literal, $themed:literal) => {
        Mark {
            path: $path,
            uses_current_color: $themed,
        }
    };
}

/// The identity string a mark is matched against.
///
/// The parts are joined with a space, and empty ones are dropped. Every part is
/// optional: an Agent with nothing but an id is still identified.
pub fn identity(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The mark for an identity string, or `None` when nothing matches.
///
/// The order is load-bearing and matches the desktop exactly: a brand that has
/// a mark of its own (OpenCode, Gemini, Qwen, Copilot, Claude, OpenAI) is
/// tested before the catalogue, because "opencode gemini" must be OpenCode
/// while a bare "gemini" must be Gemini.
pub fn agent_mark(identity: &str) -> Option<Mark> {
    let identity = identity.to_ascii_lowercase();
    let inline = if identity.contains("opencode") {
        Some(mark!("opencode.svg", false))
    } else if identity.contains("gemini") {
        Some(mark!("gemini.svg", false))
    } else if identity.contains("qwen")
        || identity.contains("tongyi")
        || identity.contains("dashscope")
    {
        Some(mark!("qwen.svg", false))
    } else if identity.contains("copilot") {
        Some(mark!("copilot.svg", true))
    } else if identity.contains("claude") || identity.contains("anthropic") {
        Some(mark!("claude.svg", false))
    } else if identity.contains("codex")
        || identity.contains("openai")
        || identity.contains("chatgpt")
    {
        Some(mark!("openai.svg", true))
    } else {
        None
    };
    if inline.is_some() {
        return inline;
    }
    CATALOG_AGENT_BRANDS
        .iter()
        .find_map(|(needle, mark)| identity.contains(needle).then_some(*mark))
}

/// The bytes of a mark, by its [`Mark::path`].
pub fn mark_bytes(path: &str) -> Option<&'static [u8]> {
    MARK_ASSETS
        .iter()
        .find_map(|(candidate, bytes)| (*candidate == path).then_some(*bytes))
}

/// Every mark this crate can serve, for a caller that wants to enumerate them.
pub fn marks() -> impl Iterator<Item = (&'static str, &'static [u8])> {
    MARK_ASSETS.iter().copied()
}

/// A mark rendered to a PNG the caller can hand to a terminal.
///
/// `size` is the square the mark is drawn into, in pixels. The mark is fitted
/// into it and centred, so a mark that is not square keeps its proportions
/// rather than being stretched into the box.
///
/// `current_color` is the packed sRGB (`0xrrggbb`) a *themed* mark is drawn in;
/// a colored mark ignores it, because its colours are the brand's.
///
/// `None` when the mark's bytes are missing or are not an SVG this build can
/// draw — a caller falls back to the Agent's name, which is always known.
#[cfg(feature = "raster")]
pub fn mark_png(mark: Mark, size: u32, current_color: Option<u32>) -> Option<Vec<u8>> {
    let svg = mark_svg(mark, current_color)?;
    svg_png(&svg, size)
}

/// A square PNG of any SVG text, for a client that carries its own icons.
///
/// [`mark_png`] answers "what does this Agent look like"; this answers "what
/// does this picture look like", for a caller whose marks are not brands — the
/// terminal's own chrome, whose icons are the desktop pack's. Rasterization is
/// the same in both cases, so it lives here once rather than being copied into
/// every client that has a picture to draw.
#[cfg(feature = "raster")]
pub fn svg_png(svg: &str, size: u32) -> Option<Vec<u8>> {
    use image::ImageEncoder;

    let pixels = svg_rgba(svg, size)?;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, size, size, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(png)
}

/// A mark rendered to straight RGBA pixels, `size` by `size`.
///
/// Separate from [`mark_png`] so a caller with its own PNG encoder — or one
/// that wants to composite the mark itself — does not have to decode what it
/// just encoded.
#[cfg(feature = "raster")]
pub fn mark_rgba(mark: Mark, size: u32, current_color: Option<u32>) -> Option<Vec<u8>> {
    let svg = mark_svg(mark, current_color)?;
    svg_rgba(&svg, size)
}

/// Any SVG text rendered to straight RGBA pixels, `size` by `size`.
///
/// The mark is fitted into the square and centred, so a picture that is not
/// square keeps its proportions rather than being stretched into the box.
#[cfg(feature = "raster")]
pub fn svg_rgba(svg: &str, size: u32) -> Option<Vec<u8>> {
    use resvg::usvg;

    if size == 0 {
        return None;
    }
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg.as_bytes(), &options).ok()?;
    let source = tree.size();
    let (source_width, source_height) = (source.width(), source.height());
    if !source_width.is_finite() || !source_height.is_finite() {
        return None;
    }
    if source_width <= 0.0 || source_height <= 0.0 {
        return None;
    }
    // Fit and centre: the smaller ratio keeps the whole picture inside the
    // square, and the leftover goes to one side each so it sits in the middle of
    // the cell the terminal gives it.
    let target = size as f32;
    let scale = (target / source_width).min(target / source_height);
    let translate_x = (target - source_width * scale) / 2.0;
    let translate_y = (target - source_height * scale) / 2.0;
    let transform =
        resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, translate_x, translate_y);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap.take())
}

/// The SVG text to hand the renderer, with `currentColor` resolved.
///
/// usvg resolves `currentColor` while parsing and leaves no way to set it
/// afterwards, so a themed mark is rewritten before it is parsed. A colored
/// mark is returned untouched: its colours are the brand's, and every mark in
/// the pack that carries a colour of its own never spells `currentColor`, which
/// is what makes the two cases clear-cut rather than a guess.
#[cfg(feature = "raster")]
pub fn mark_svg(mark: Mark, current_color: Option<u32>) -> Option<String> {
    let source = std::str::from_utf8(mark_bytes(mark.path)?).ok()?;
    if !mark.uses_current_color {
        return Some(source.to_string());
    }
    themed_svg(source, current_color)
}

/// SVG text with `currentColor` resolved to `current_color`.
///
/// usvg resolves `currentColor` while parsing and leaves no way to set it
/// afterwards, so a themed picture is rewritten before it is parsed. `None`
/// when there is no colour to resolve to, which is the caller's signal to draw
/// something else rather than a black blob.
#[cfg(feature = "raster")]
pub fn themed_svg(svg: &str, current_color: Option<u32>) -> Option<String> {
    let color = current_color?;
    let hex = format!("#{:06x}", color & 0x00ff_ffff);
    Some(svg.replace("currentColor", &hex))
}

include!("mark_table.rs");
include!("model_table.rs");

#[cfg(test)]
mod tests;
