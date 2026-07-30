use std::borrow::Cow;
use std::io::Cursor;
use std::sync::Arc;

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};

use crate::limits::MarkdownLimits;

type SvgRootMetadata<'a> = (
    &'a mut Option<f32>,
    &'a mut Option<f32>,
    &'a mut Option<SvgViewBox>,
    &'a mut Option<f32>,
);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvgViewBox {
    pub min_x: f32,
    pub min_y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvgArtifact {
    pub bytes: Arc<[u8]>,
    pub width_px: f32,
    pub height_px: f32,
    pub view_box: SvgViewBox,
    pub baseline_offset_px: Option<f32>,
    pub element_count: usize,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum SvgPolicyError {
    #[error("SVG exceeds the byte limit")]
    ByteLimit,
    #[error("SVG XML is malformed: {0}")]
    Malformed(String),
    #[error("SVG root element is missing or invalid")]
    InvalidRoot,
    #[error("SVG contains forbidden element <{0}>")]
    ForbiddenElement(String),
    #[error("SVG contains forbidden attribute {0}")]
    ForbiddenAttribute(String),
    #[error("SVG contains an external or unsafe reference")]
    UnsafeReference,
    #[error("SVG dimensions are missing, non-finite, or out of bounds")]
    InvalidDimensions,
    #[error("SVG exceeds its element, depth, path, or text limit")]
    StructureLimit,
    #[error("SVG DTD, entities, or processing instructions are forbidden")]
    ActiveXml,
}

#[derive(Debug, Clone, Copy)]
pub struct SvgPolicy {
    limits: MarkdownLimits,
}

impl Default for SvgPolicy {
    fn default() -> Self {
        Self::new(MarkdownLimits::default())
    }
}

impl SvgPolicy {
    pub fn new(limits: MarkdownLimits) -> Self {
        Self { limits }
    }

    pub fn sanitize(&self, source: &str, id_prefix: &str) -> Result<SvgArtifact, SvgPolicyError> {
        self.sanitize_inner(source, id_prefix, None)
    }

    #[cfg(any(feature = "artifact-engines", test))]
    pub(crate) fn sanitize_with_current_color(
        &self,
        source: &str,
        id_prefix: &str,
        current_color_rgb: u32,
    ) -> Result<SvgArtifact, SvgPolicyError> {
        self.sanitize_inner(source, id_prefix, Some(current_color_rgb & 0x00ff_ffff))
    }

    fn sanitize_inner(
        &self,
        source: &str,
        id_prefix: &str,
        current_color_rgb: Option<u32>,
    ) -> Result<SvgArtifact, SvgPolicyError> {
        if source.len() > self.limits.max_svg_bytes {
            return Err(SvgPolicyError::ByteLimit);
        }
        let prefix = safe_prefix(id_prefix);
        let current_color = current_color_rgb.map(|color| format!("#{color:06x}"));
        let mut reader = Reader::from_str(source);
        reader.config_mut().trim_text(false);
        reader.config_mut().check_end_names = true;
        let mut writer = Writer::new(Cursor::new(Vec::with_capacity(source.len())));
        let mut depth = 0usize;
        let mut elements = 0usize;
        let mut text_bytes = 0usize;
        let mut path_bytes = 0usize;
        let mut root_seen = false;
        let mut root_closed = false;
        let mut in_style = false;
        let mut root_width = None;
        let mut root_height = None;
        let mut root_view_box = None;
        let mut baseline_offset_px = None;

        loop {
            let event = reader
                .read_event()
                .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
            match event {
                Event::Start(start) => {
                    if root_closed {
                        return Err(SvgPolicyError::InvalidRoot);
                    }
                    depth = depth.saturating_add(1);
                    elements = elements.saturating_add(1);
                    if depth > self.limits.max_svg_depth || elements > self.limits.max_svg_elements
                    {
                        return Err(SvgPolicyError::StructureLimit);
                    }
                    let tag = xml_name(start.name().as_ref());
                    if !allowed_tag(&tag) {
                        return Err(SvgPolicyError::ForbiddenElement(tag));
                    }
                    if !root_seen {
                        if depth != 1 || tag != "svg" {
                            return Err(SvgPolicyError::InvalidRoot);
                        }
                        root_seen = true;
                    }
                    in_style = tag == "style";
                    let root = (depth == 1 && tag == "svg").then_some((
                        &mut root_width,
                        &mut root_height,
                        &mut root_view_box,
                        &mut baseline_offset_px,
                    ));
                    let sanitized = self.sanitize_start(
                        &start,
                        &reader,
                        &prefix,
                        &tag,
                        &mut path_bytes,
                        root,
                        current_color.as_deref(),
                    )?;
                    writer
                        .write_event(Event::Start(sanitized))
                        .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                }
                Event::Empty(start) => {
                    if root_closed {
                        return Err(SvgPolicyError::InvalidRoot);
                    }
                    let element_depth = depth.saturating_add(1);
                    if element_depth > self.limits.max_svg_depth {
                        return Err(SvgPolicyError::StructureLimit);
                    }
                    elements = elements.saturating_add(1);
                    if elements > self.limits.max_svg_elements {
                        return Err(SvgPolicyError::StructureLimit);
                    }
                    let tag = xml_name(start.name().as_ref());
                    if !allowed_tag(&tag) {
                        return Err(SvgPolicyError::ForbiddenElement(tag));
                    }
                    if !root_seen {
                        if element_depth != 1 || tag != "svg" {
                            return Err(SvgPolicyError::InvalidRoot);
                        }
                        root_seen = true;
                        root_closed = true;
                    }
                    let root = (element_depth == 1 && tag == "svg").then_some((
                        &mut root_width,
                        &mut root_height,
                        &mut root_view_box,
                        &mut baseline_offset_px,
                    ));
                    let sanitized = self.sanitize_start(
                        &start,
                        &reader,
                        &prefix,
                        &tag,
                        &mut path_bytes,
                        root,
                        current_color.as_deref(),
                    )?;
                    writer
                        .write_event(Event::Empty(sanitized))
                        .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                }
                Event::End(end) => {
                    let tag = xml_name(end.name().as_ref());
                    if depth == 0 {
                        return Err(SvgPolicyError::InvalidRoot);
                    }
                    if tag == "style" {
                        in_style = false;
                    }
                    if depth == 1 {
                        if tag != "svg" {
                            return Err(SvgPolicyError::InvalidRoot);
                        }
                        root_closed = true;
                    }
                    depth = depth.saturating_sub(1);
                    writer
                        .write_event(Event::End(BytesEnd::new(tag)))
                        .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                }
                Event::Text(text) => {
                    if (!root_seen || root_closed)
                        && !text
                            .decode()
                            .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?
                            .trim()
                            .is_empty()
                    {
                        return Err(SvgPolicyError::InvalidRoot);
                    }
                    text_bytes = text_bytes.saturating_add(text.len());
                    if text_bytes > self.limits.max_svg_text_bytes {
                        return Err(SvgPolicyError::StructureLimit);
                    }
                    if in_style {
                        let text = text
                            .decode()
                            .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                        let text = sanitize_css(&text, &prefix)?;
                        writer
                            .write_event(Event::Text(BytesText::new(&text)))
                            .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                    } else {
                        writer
                            .write_event(Event::Text(text.into_owned()))
                            .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                    }
                }
                Event::CData(text) => {
                    if !root_seen || root_closed || !in_style {
                        return Err(SvgPolicyError::ActiveXml);
                    }
                    let text = text
                        .decode()
                        .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                    text_bytes = text_bytes.saturating_add(text.len());
                    if text_bytes > self.limits.max_svg_text_bytes {
                        return Err(SvgPolicyError::StructureLimit);
                    }
                    let text = sanitize_css(&text, &prefix)?;
                    writer
                        .write_event(Event::Text(BytesText::new(&text)))
                        .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                }
                Event::Comment(_) => {}
                Event::Decl(declaration) => {
                    if root_seen || root_closed {
                        return Err(SvgPolicyError::ActiveXml);
                    }
                    writer
                        .write_event(Event::Decl(declaration.into_owned()))
                        .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
                }
                Event::DocType(_) | Event::PI(_) | Event::GeneralRef(_) => {
                    return Err(SvgPolicyError::ActiveXml);
                }
                Event::Eof => break,
            }
        }
        if !root_seen || !root_closed || depth != 0 {
            return Err(SvgPolicyError::InvalidRoot);
        }
        let view_box = root_view_box.ok_or(SvgPolicyError::InvalidDimensions)?;
        validate_view_box(view_box, self.limits.max_svg_dimension)?;
        let width_px = root_width.unwrap_or(view_box.width);
        let height_px = root_height.unwrap_or(view_box.height);
        validate_dimension(width_px, self.limits.max_svg_dimension)?;
        validate_dimension(height_px, self.limits.max_svg_dimension)?;
        validate_pixel_area(width_px, height_px, self.limits.max_svg_pixels)?;
        let bytes = writer.into_inner().into_inner();
        if bytes.len() > self.limits.max_svg_bytes {
            return Err(SvgPolicyError::ByteLimit);
        }
        Ok(SvgArtifact {
            bytes: bytes.into(),
            width_px,
            height_px,
            view_box,
            baseline_offset_px,
            element_count: elements,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn sanitize_start(
        &self,
        start: &BytesStart<'_>,
        reader: &Reader<&[u8]>,
        prefix: &str,
        tag: &str,
        path_bytes: &mut usize,
        mut root: Option<SvgRootMetadata<'_>>,
        current_color: Option<&str>,
    ) -> Result<BytesStart<'static>, SvgPolicyError> {
        let mut output = BytesStart::new(tag.to_string());
        let is_root = root.is_some();
        for attribute in start.attributes().with_checks(true) {
            let attribute =
                attribute.map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
            let name = xml_name(attribute.key.as_ref());
            let lower = name.to_ascii_lowercase();
            if lower.starts_with("on") || !allowed_attribute(&lower) {
                return Err(SvgPolicyError::ForbiddenAttribute(name));
            }
            if is_root && lower == "color" && current_color.is_some() {
                continue;
            }
            let value = attribute
                .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
                .map_err(|error| SvgPolicyError::Malformed(error.to_string()))?;
            let value = sanitize_attribute_value(&lower, &value, prefix)?;
            let value = match current_color {
                Some(current_color) if value.eq_ignore_ascii_case("currentcolor") => {
                    current_color.to_string()
                }
                _ => value,
            };
            if matches!(lower.as_str(), "d" | "points") {
                *path_bytes = path_bytes.saturating_add(value.len());
                if *path_bytes > self.limits.max_svg_path_bytes {
                    return Err(SvgPolicyError::StructureLimit);
                }
            }
            if let Some((width, height, view_box, baseline)) = root.as_mut() {
                match lower.as_str() {
                    "width" => {
                        **width = Some(
                            parse_svg_length(&value).ok_or(SvgPolicyError::InvalidDimensions)?,
                        )
                    }
                    "height" => {
                        **height = Some(
                            parse_svg_length(&value).ok_or(SvgPolicyError::InvalidDimensions)?,
                        )
                    }
                    "viewbox" => {
                        **view_box =
                            Some(parse_view_box(&value).ok_or(SvgPolicyError::InvalidDimensions)?)
                    }
                    "style" => **baseline = parse_vertical_align(&value),
                    _ => {}
                }
            }
            output.push_attribute((name.as_str(), value.as_str()));
        }
        if is_root && let Some(current_color) = current_color {
            output.push_attribute(("color", current_color));
        }
        Ok(output.into_owned())
    }
}

fn xml_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn safe_prefix(prefix: &str) -> String {
    let mut output = prefix
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .take(64)
        .collect::<String>();
    if output.is_empty() {
        output.push_str("vibex-svg");
    }
    output
}

fn allowed_tag(tag: &str) -> bool {
    matches!(
        tag,
        "svg"
            | "g"
            | "defs"
            | "title"
            | "desc"
            | "style"
            | "path"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "text"
            | "tspan"
            | "use"
            | "symbol"
            | "marker"
            | "clipPath"
            | "mask"
            | "linearGradient"
            | "radialGradient"
            | "stop"
            | "filter"
            | "feBlend"
            | "feColorMatrix"
            | "feComponentTransfer"
            | "feFuncA"
            | "feFuncR"
            | "feFuncG"
            | "feFuncB"
            | "feGaussianBlur"
            | "feMerge"
            | "feMergeNode"
            | "feOffset"
            | "feFlood"
            | "feComposite"
            | "feDropShadow"
    )
}

fn allowed_attribute(attribute: &str) -> bool {
    if (attribute.starts_with("data-") || attribute.starts_with("aria-"))
        && attribute
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return true;
    }
    matches!(
        attribute,
        "id" | "class"
            | "style"
            | "xmlns"
            | "xmlns:xlink"
            | "version"
            | "role"
            | "focusable"
            | "aria-hidden"
            | "aria-label"
            | "viewbox"
            | "width"
            | "height"
            | "x"
            | "y"
            | "x1"
            | "y1"
            | "x2"
            | "y2"
            | "cx"
            | "cy"
            | "r"
            | "rx"
            | "ry"
            | "dx"
            | "dy"
            | "d"
            | "points"
            | "transform"
            | "transform-origin"
            | "fill"
            | "fill-opacity"
            | "fill-rule"
            | "stroke"
            | "stroke-width"
            | "stroke-opacity"
            | "stroke-linecap"
            | "stroke-linejoin"
            | "stroke-dasharray"
            | "stroke-dashoffset"
            | "opacity"
            | "color"
            | "font-family"
            | "font-size"
            | "font-style"
            | "font-weight"
            | "text-anchor"
            | "dominant-baseline"
            | "alignment-baseline"
            | "letter-spacing"
            | "word-spacing"
            | "text-decoration"
            | "white-space"
            | "direction"
            | "unicode-bidi"
            | "marker-start"
            | "marker-mid"
            | "marker-end"
            | "clip-path"
            | "clip-rule"
            | "mask"
            | "filter"
            | "offset"
            | "stop-color"
            | "stop-opacity"
            | "gradientunits"
            | "gradienttransform"
            | "spreadmethod"
            | "patternunits"
            | "patterncontentunits"
            | "preserveaspectratio"
            | "refx"
            | "refy"
            | "markerwidth"
            | "markerheight"
            | "markerunits"
            | "orient"
            | "pathlength"
            | "href"
            | "xlink:href"
            | "in"
            | "in2"
            | "result"
            | "type"
            | "values"
            | "stddeviation"
            | "flood-color"
            | "flood-opacity"
            | "operator"
            | "k1"
            | "k2"
            | "k3"
            | "k4"
    )
}

fn sanitize_attribute_value(
    attribute: &str,
    value: &str,
    prefix: &str,
) -> Result<String, SvgPolicyError> {
    if attribute == "xmlns" && value == "http://www.w3.org/2000/svg" {
        return Ok(value.to_string());
    }
    if attribute == "xmlns:xlink" && value == "http://www.w3.org/1999/xlink" {
        return Ok(value.to_string());
    }
    if contains_unsafe_text(value) {
        return Err(SvgPolicyError::UnsafeReference);
    }
    match attribute {
        "id" => Ok(format!("{prefix}-{}", safe_fragment_id(value)?)),
        "href" | "xlink:href" => {
            let fragment = value
                .strip_prefix('#')
                .ok_or(SvgPolicyError::UnsafeReference)?;
            Ok(format!("#{prefix}-{}", safe_fragment_id(fragment)?))
        }
        "style" => sanitize_css(value, prefix),
        _ => rewrite_url_fragments(value, prefix),
    }
}

fn sanitize_css(value: &str, prefix: &str) -> Result<String, SvgPolicyError> {
    if contains_unsafe_text(value)
        || value.to_ascii_lowercase().contains("@import")
        || value.to_ascii_lowercase().contains("@namespace")
        || value.contains('\\')
    {
        return Err(SvgPolicyError::UnsafeReference);
    }
    rewrite_url_fragments(value, prefix)
}

fn contains_unsafe_text(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("javascript:")
        || lower.contains("vbscript:")
        || lower.contains("data:")
        || lower.contains("http:")
        || lower.contains("https:")
        || lower.contains("file:")
        || lower.contains("expression(")
}

fn rewrite_url_fragments(value: &str, prefix: &str) -> Result<String, SvgPolicyError> {
    let mut output = String::with_capacity(value.len() + prefix.len());
    let mut remaining = value;
    while let Some(index) = find_ascii_case_insensitive(remaining, "url(") {
        output.push_str(&remaining[..index]);
        let after = &remaining[index + 4..];
        let close = after.find(')').ok_or(SvgPolicyError::UnsafeReference)?;
        let fragment = after[..close].trim().trim_matches(['\'', '"']);
        let fragment = fragment
            .strip_prefix('#')
            .ok_or(SvgPolicyError::UnsafeReference)?;
        output.push_str("url(#");
        output.push_str(prefix);
        output.push('-');
        output.push_str(&safe_fragment_id(fragment)?);
        output.push(')');
        remaining = &after[close + 1..];
    }
    output.push_str(remaining);
    Ok(output)
}

fn find_ascii_case_insensitive(value: &str, needle: &str) -> Option<usize> {
    value
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn safe_fragment_id(value: &str) -> Result<Cow<'_, str>, SvgPolicyError> {
    if value.is_empty()
        || value.len() > 256
        || !value.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
    {
        return Err(SvgPolicyError::UnsafeReference);
    }
    Ok(Cow::Borrowed(value))
}

fn parse_svg_length(value: &str) -> Option<f32> {
    let value = value.trim();
    let split = value
        .find(|character: char| {
            !character.is_ascii_digit() && !matches!(character, '.' | '-' | '+')
        })
        .unwrap_or(value.len());
    let number = value[..split].parse::<f32>().ok()?;
    let scale = match value[split..].trim().to_ascii_lowercase().as_str() {
        "" | "px" => 1.0,
        "ex" => 8.0,
        "em" | "rem" => 16.0,
        "pt" => 96.0 / 72.0,
        _ => return None,
    };
    Some(number * scale)
}

fn parse_view_box(value: &str) -> Option<SvgViewBox> {
    let values = value
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|value| !value.is_empty())
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (values.len() == 4).then(|| SvgViewBox {
        min_x: values[0],
        min_y: values[1],
        width: values[2],
        height: values[3],
    })
}

fn parse_vertical_align(style: &str) -> Option<f32> {
    style.split(';').find_map(|declaration| {
        let (name, value) = declaration.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("vertical-align")
            .then(|| parse_svg_length(value))
            .flatten()
    })
}

fn validate_dimension(value: f32, maximum: u32) -> Result<(), SvgPolicyError> {
    if value.is_finite() && value > 0.0 && value <= maximum as f32 {
        Ok(())
    } else {
        Err(SvgPolicyError::InvalidDimensions)
    }
}

fn validate_view_box(value: SvgViewBox, maximum: u32) -> Result<(), SvgPolicyError> {
    if value.min_x.is_finite()
        && value.min_y.is_finite()
        && value.width.is_finite()
        && value.height.is_finite()
        && value.width > 0.0
        && value.height > 0.0
        && value.width <= maximum as f32
        && value.height <= maximum as f32
    {
        Ok(())
    } else {
        Err(SvgPolicyError::InvalidDimensions)
    }
}

fn validate_pixel_area(width: f32, height: f32, maximum: u64) -> Result<(), SvgPolicyError> {
    let pixels = f64::from(width) * f64::from(height);
    if pixels.is_finite() && pixels > 0.0 && pixels <= maximum as f64 {
        Ok(())
    } else {
        Err(SvgPolicyError::InvalidDimensions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_rewrites_ids_and_fragment_references() {
        let artifact = SvgPolicy::default()
            .sanitize(
                r##"<svg width="20" height="10" viewBox="0 0 20 10"><defs><clipPath id="clip"><rect width="20" height="10"/></clipPath></defs><g clip-path="url(#clip)"><use href="#clip"/></g></svg>"##,
                "doc-1",
            )
            .unwrap();
        let output = String::from_utf8(artifact.bytes.to_vec()).unwrap();
        assert!(output.contains("id=\"doc-1-clip\""));
        assert!(output.contains("url(#doc-1-clip)"));
        assert!(output.contains("href=\"#doc-1-clip\""));
        assert_eq!(artifact.width_px, 20.0);
    }

    #[test]
    fn sanitizer_resolves_generated_svg_current_color() {
        let artifact = SvgPolicy::default()
            .sanitize_with_current_color(
                r##"<svg color="#000000" width="20" height="10" viewBox="0 0 20 10"><g fill="currentColor" stroke="CURRENTCOLOR"><path d="M0 0L10 10"/></g></svg>"##,
                "math-dark",
                0xfafafa,
            )
            .unwrap();
        let output = String::from_utf8(artifact.bytes.to_vec()).unwrap();

        assert!(output.contains("color=\"#fafafa\""));
        assert!(output.contains("fill=\"#fafafa\""));
        assert!(output.contains("stroke=\"#fafafa\""));
        assert!(!output.to_ascii_lowercase().contains("currentcolor"));
    }

    #[test]
    fn sanitizer_bounds_intrinsic_pixels_without_treating_view_box_units_as_pixels() {
        let artifact = SvgPolicy::default()
            .sanitize(
                r#"<svg width="6ex" height="2ex" viewBox="0 -1500 6000 2000"><path d="M0 0L10 10"/></svg>"#,
                "math",
            )
            .unwrap();
        assert_eq!((artifact.width_px, artifact.height_px), (48.0, 16.0));

        assert!(
            SvgPolicy::default()
                .sanitize(r#"<svg viewBox="0 0 6000 2000"></svg>"#, "oversized")
                .is_err()
        );
    }

    #[test]
    fn sanitizer_rejects_active_xml_external_refs_and_unbounded_dimensions() {
        for source in [
            r#"<!DOCTYPE svg><svg viewBox="0 0 10 10"></svg>"#,
            r#"<svg viewBox="0 0 10 10"><script/></svg>"#,
            r#"<svg viewBox="0 0 10 10"><image href="https://example.com/x"/></svg>"#,
            r#"<svg viewBox="0 0 999999 10"></svg>"#,
            r#"<svg viewBox="0 0 16000 16000"></svg>"#,
            r#"<svg viewBox="0 0 10 10"><foreignObject/></svg>"#,
            r#"<svg width="100%" height="10" viewBox="0 0 10 10"></svg>"#,
            r#"<svg viewBox="0 0 999999 999999"><svg viewBox="0 0 10 10"/></svg>"#,
            r#"<svg viewBox="0 0 10 10"></svg><svg viewBox="0 0 10 10"></svg>"#,
            r#"<svg viewBox="0 0 10 10"><style>.x { fill: URL(https://example.com/x) }</style></svg>"#,
            r#"<svg viewBox="0 0 10 10"><style>.x { fill: u\\72l(#x) }</style></svg>"#,
        ] {
            assert!(
                SvgPolicy::default().sanitize(source, "x").is_err(),
                "{source}"
            );
        }
    }
}
