use std::borrow::Cow;

use gpui::{
    AnyElement, App, AssetSource, Hsla, IntoElement, Pixels, Result as GpuiResult, SharedString,
    Styled as _, img,
};
use gpui_component::{Icon, IconName};
use gpui_component_assets::Assets as ComponentAssets;
use sha2::{Digest, Sha256};

const INTER_PACKAGE_ROOT: &str = "../../../node_modules/.pnpm/@fontsource-variable+inter@5.2.8/node_modules/@fontsource-variable/inter";
const INTER_LATIN: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../node_modules/.pnpm/@fontsource-variable+inter@5.2.8/node_modules/@fontsource-variable/inter/files/inter-latin-wght-normal.woff2"
));
const INTER_LATIN_EXT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../node_modules/.pnpm/@fontsource-variable+inter@5.2.8/node_modules/@fontsource-variable/inter/files/inter-latin-ext-wght-normal.woff2"
));

const VIBEX_ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/vibex/vibex-mark.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/vibex-mark.svg"
        )),
    ),
    (
        "icons/vibex/openai.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/openai.svg"
        )),
    ),
    (
        "icons/vibex/claude.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/claude.svg"
        )),
    ),
    (
        "icons/vibex/opencode.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/opencode.svg"
        )),
    ),
    (
        "icons/vibex/gemini.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/gemini.svg"
        )),
    ),
    (
        "icons/vibex/qwen.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/qwen.svg"
        )),
    ),
    (
        "icons/vibex/copilot.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/copilot.svg"
        )),
    ),
    (
        "icons/vibex/database.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/database.svg"
        )),
    ),
    (
        "icons/vibex/image.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/image.svg"
        )),
    ),
    (
        "icons/vibex/download.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/download.svg"
        )),
    ),
    (
        "icons/vibex/upload.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/upload.svg"
        )),
    ),
    (
        "icons/vibex/brain.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/brain.svg"
        )),
    ),
    (
        "icons/vibex/shield-alert.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/shield-alert.svg"
        )),
    ),
    (
        "icons/vibex/sparkles.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/sparkles.svg"
        )),
    ),
    (
        "icons/vibex/list-checks.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/list-checks.svg"
        )),
    ),
    (
        "icons/vibex/chevrons-right-left.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/chevrons-right-left.svg"
        )),
    ),
    (
        "icons/vibex/chevrons-left-right.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/chevrons-left-right.svg"
        )),
    ),
    (
        "icons/vibex/import.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/import.svg"
        )),
    ),
    (
        "icons/vibex/plug-zap.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/plug-zap.svg"
        )),
    ),
    (
        "icons/vibex/grip-vertical.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/grip-vertical.svg"
        )),
    ),
    (
        "icons/vibex/activity.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/activity.svg"
        )),
    ),
    (
        "icons/vibex/boxes.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/boxes.svg"
        )),
    ),
    (
        "icons/vibex/library.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/library.svg"
        )),
    ),
    (
        "icons/vibex/pin.svg",
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/pin.svg")),
    ),
    (
        "icons/vibex/pin-filled.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/pin-filled.svg"
        )),
    ),
    (
        "icons/vibex/pencil.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/pencil.svg"
        )),
    ),
    (
        "icons/vibex/corner-down-right.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/corner-down-right.svg"
        )),
    ),
    (
        "icons/vibex/send.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/send.svg"
        )),
    ),
    (
        "icons/vibex/monitor.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/monitor.svg"
        )),
    ),
    (
        "icons/vibex/rotate-ccw.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/rotate-ccw.svg"
        )),
    ),
    (
        "icons/vibex/trash-2.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/trash-2.svg"
        )),
    ),
    (
        "icons/vibex/zap.svg",
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/zap.svg")),
    ),
    (
        "icons/vibex/message-square.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/message-square.svg"
        )),
    ),
    (
        "icons/vibex/git-branch.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/git-branch.svg"
        )),
    ),
    (
        "icons/vibex/puzzle.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/puzzle.svg"
        )),
    ),
    (
        "icons/vibex/file-code.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/file-code.svg"
        )),
    ),
    (
        "icons/vibex/file-braces.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/file-braces.svg"
        )),
    ),
    (
        "icons/vibex/book-open-text.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/book-open-text.svg"
        )),
    ),
    (
        "icons/vibex/file-archive.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/file-archive.svg"
        )),
    ),
    (
        "icons/vibex/file-terminal.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/file-terminal.svg"
        )),
    ),
    (
        "icons/vibex/code-xml.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/code-xml.svg"
        )),
    ),
    (
        "icons/vibex/hash.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/hash.svg"
        )),
    ),
    (
        "icons/vibex/chevrons-right.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/chevrons-right.svg"
        )),
    ),
    (
        "icons/vibex/chevrons-down-up.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/chevrons-down-up.svg"
        )),
    ),
    (
        "icons/vibex/files.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/files.svg"
        )),
    ),
];

macro_rules! bundled_icon_asset {
    ($path:literal) => {
        (
            concat!("icons/vibex/", $path),
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/", $path)),
        )
    };
}

const FILE_INTEGRATION_ASSETS: &[(&str, &[u8])] = &[
    bundled_icon_asset!("audio-lines.svg"),
    bundled_icon_asset!("clipboard-paste.svg"),
    bundled_icon_asset!("coffee.svg"),
    bundled_icon_asset!("file-cog.svg"),
    bundled_icon_asset!("file-key.svg"),
    bundled_icon_asset!("file-lock.svg"),
    bundled_icon_asset!("file-plus.svg"),
    bundled_icon_asset!("file-symlink.svg"),
    bundled_icon_asset!("file-spreadsheet.svg"),
    bundled_icon_asset!("file-text.svg"),
    bundled_icon_asset!("file-type.svg"),
    bundled_icon_asset!("file-video-camera.svg"),
    bundled_icon_asset!("folder-plus.svg"),
    bundled_icon_asset!("scissors.svg"),
    bundled_icon_asset!("open-tools/clion.svg"),
    bundled_icon_asset!("open-tools/cursor.svg"),
    bundled_icon_asset!("open-tools/goland.svg"),
    bundled_icon_asset!("open-tools/intellij-idea.svg"),
    bundled_icon_asset!("open-tools/jetbrains.svg"),
    bundled_icon_asset!("open-tools/phpstorm.svg"),
    bundled_icon_asset!("open-tools/pycharm.svg"),
    bundled_icon_asset!("open-tools/rider.svg"),
    bundled_icon_asset!("open-tools/sublime-text.svg"),
    bundled_icon_asset!("open-tools/visual-studio-code.svg"),
    bundled_icon_asset!("open-tools/webstorm.svg"),
    bundled_icon_asset!("open-tools/windsurf.svg"),
    bundled_icon_asset!("open-tools/xcode.svg"),
    bundled_icon_asset!("open-tools/zed.svg"),
];

const AGENT_BRAND_ASSETS: &[(&str, &[u8])] = &[
    bundled_icon_asset!("agents/agoragentic-acp.svg"),
    bundled_icon_asset!("agents/amp-acp.svg"),
    bundled_icon_asset!("agents/auggie.svg"),
    bundled_icon_asset!("agents/autohand.svg"),
    bundled_icon_asset!("agents/cline.svg"),
    bundled_icon_asset!("agents/codebuddy-code.svg"),
    bundled_icon_asset!("agents/codewhale.svg"),
    bundled_icon_asset!("agents/cortex-code.svg"),
    bundled_icon_asset!("agents/corust-agent.svg"),
    bundled_icon_asset!("agents/crow-cli.svg"),
    bundled_icon_asset!("agents/cursor.svg"),
    bundled_icon_asset!("agents/deepagents.svg"),
    bundled_icon_asset!("agents/devin.svg"),
    bundled_icon_asset!("agents/dimcode.svg"),
    bundled_icon_asset!("agents/dirac.svg"),
    bundled_icon_asset!("agents/factory-droid.svg"),
    bundled_icon_asset!("agents/fast-agent.svg"),
    bundled_icon_asset!("agents/glm-acp-agent.svg"),
    bundled_icon_asset!("agents/goose.svg"),
    bundled_icon_asset!("agents/grok.svg"),
    bundled_icon_asset!("agents/hermes.png"),
    bundled_icon_asset!("agents/junie.svg"),
    bundled_icon_asset!("agents/kilo.svg"),
    bundled_icon_asset!("agents/kimi.svg"),
    bundled_icon_asset!("agents/kiro.svg"),
    bundled_icon_asset!("agents/minion-code.svg"),
    bundled_icon_asset!("agents/mistral-vibe.svg"),
    bundled_icon_asset!("agents/nova.svg"),
    bundled_icon_asset!("agents/poolside.svg"),
    bundled_icon_asset!("agents/qoder.svg"),
    bundled_icon_asset!("agents/sigit.svg"),
    bundled_icon_asset!("agents/stakpak.svg"),
    bundled_icon_asset!("agents/vtcode.svg"),
];

pub struct VibexAssets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentBrandAsset {
    pub(crate) path: &'static str,
    pub(crate) uses_current_color: bool,
}

type BrandAsset = AgentBrandAsset;

const fn colored_agent_asset(path: &'static str) -> BrandAsset {
    BrandAsset {
        path,
        uses_current_color: false,
    }
}

const CATALOG_AGENT_BRANDS: &[(&str, BrandAsset)] = &[
    (
        "agoragentic-acp",
        colored_agent_asset("icons/vibex/agents/agoragentic-acp.svg"),
    ),
    (
        "glm-acp-agent",
        colored_agent_asset("icons/vibex/agents/glm-acp-agent.svg"),
    ),
    (
        "codebuddy-code",
        colored_agent_asset("icons/vibex/agents/codebuddy-code.svg"),
    ),
    (
        "factory-droid",
        colored_agent_asset("icons/vibex/agents/factory-droid.svg"),
    ),
    (
        "mistral-vibe",
        colored_agent_asset("icons/vibex/agents/mistral-vibe.svg"),
    ),
    (
        "minion-code",
        colored_agent_asset("icons/vibex/agents/minion-code.svg"),
    ),
    (
        "cortex-code",
        colored_agent_asset("icons/vibex/agents/cortex-code.svg"),
    ),
    (
        "corust-agent",
        colored_agent_asset("icons/vibex/agents/corust-agent.svg"),
    ),
    (
        "amp-acp",
        colored_agent_asset("icons/vibex/agents/amp-acp.svg"),
    ),
    (
        "auggie",
        colored_agent_asset("icons/vibex/agents/auggie.svg"),
    ),
    (
        "autohand",
        colored_agent_asset("icons/vibex/agents/autohand.svg"),
    ),
    ("cline", colored_agent_asset("icons/vibex/agents/cline.svg")),
    (
        "codewhale",
        colored_agent_asset("icons/vibex/agents/codewhale.svg"),
    ),
    (
        "crow-cli",
        colored_agent_asset("icons/vibex/agents/crow-cli.svg"),
    ),
    (
        "cursor",
        colored_agent_asset("icons/vibex/agents/cursor.svg"),
    ),
    (
        "deepagents",
        colored_agent_asset("icons/vibex/agents/deepagents.svg"),
    ),
    ("devin", colored_agent_asset("icons/vibex/agents/devin.svg")),
    (
        "dimcode",
        colored_agent_asset("icons/vibex/agents/dimcode.svg"),
    ),
    ("dirac", colored_agent_asset("icons/vibex/agents/dirac.svg")),
    (
        "fast-agent",
        colored_agent_asset("icons/vibex/agents/fast-agent.svg"),
    ),
    ("goose", colored_agent_asset("icons/vibex/agents/goose.svg")),
    ("grok", colored_agent_asset("icons/vibex/agents/grok.svg")),
    (
        "hermes",
        colored_agent_asset("icons/vibex/agents/hermes.png"),
    ),
    ("junie", colored_agent_asset("icons/vibex/agents/junie.svg")),
    ("kilo", colored_agent_asset("icons/vibex/agents/kilo.svg")),
    ("kiro", colored_agent_asset("icons/vibex/agents/kiro.svg")),
    ("kimi", colored_agent_asset("icons/vibex/agents/kimi.svg")),
    ("nova", colored_agent_asset("icons/vibex/agents/nova.svg")),
    (
        "poolside",
        colored_agent_asset("icons/vibex/agents/poolside.svg"),
    ),
    ("qoder", colored_agent_asset("icons/vibex/agents/qoder.svg")),
    ("sigit", colored_agent_asset("icons/vibex/agents/sigit.svg")),
    (
        "stakpak",
        colored_agent_asset("icons/vibex/agents/stakpak.svg"),
    ),
    (
        "vtcode",
        colored_agent_asset("icons/vibex/agents/vtcode.svg"),
    ),
];

pub(crate) fn agent_brand_asset(identity: &str) -> Option<BrandAsset> {
    let identity = identity.to_ascii_lowercase();
    let asset = if identity.contains("opencode") {
        BrandAsset {
            path: "icons/vibex/opencode.svg",
            uses_current_color: false,
        }
    } else if identity.contains("gemini") {
        BrandAsset {
            path: "icons/vibex/gemini.svg",
            uses_current_color: false,
        }
    } else if identity.contains("qwen")
        || identity.contains("tongyi")
        || identity.contains("dashscope")
    {
        BrandAsset {
            path: "icons/vibex/qwen.svg",
            uses_current_color: false,
        }
    } else if identity.contains("copilot") {
        BrandAsset {
            path: "icons/vibex/copilot.svg",
            uses_current_color: true,
        }
    } else if identity.contains("claude") || identity.contains("anthropic") {
        BrandAsset {
            path: "icons/vibex/claude.svg",
            uses_current_color: false,
        }
    } else if identity.contains("codex")
        || identity.contains("openai")
        || identity.contains("chatgpt")
    {
        BrandAsset {
            path: "icons/vibex/openai.svg",
            uses_current_color: true,
        }
    } else {
        return CATALOG_AGENT_BRANDS
            .iter()
            .find_map(|(needle, asset)| identity.contains(needle).then_some(*asset));
    };
    Some(asset)
}

pub(crate) fn agent_brand_icon(
    identity: &str,
    size: Pixels,
    current_color: Option<Hsla>,
) -> AnyElement {
    agent_brand_logo(identity, size, current_color)
        .unwrap_or_else(|| themed_icon(Icon::new(IconName::Bot).size(size), current_color))
}

pub(crate) fn agent_brand_logo(
    identity: &str,
    size: Pixels,
    current_color: Option<Hsla>,
) -> Option<AnyElement> {
    agent_brand_asset(identity).map(|asset| brand_asset_icon(asset, size, current_color))
}

pub(crate) fn model_brand_icon(
    model: &str,
    size: Pixels,
    current_color: Option<Hsla>,
) -> Option<AnyElement> {
    model_brand_asset(model).map(|asset| brand_asset_icon(asset, size, current_color))
}

fn model_brand_asset(model: &str) -> Option<BrandAsset> {
    let model = model.to_ascii_lowercase();
    let is_openai_model = model
        .split(|character: char| {
            character == '-' || character == '_' || character.is_ascii_whitespace()
        })
        .any(|token| {
            token.starts_with("gpt")
                || ["o1", "o3", "o4", "o5"]
                    .iter()
                    .any(|prefix| token.starts_with(prefix))
        });
    if is_openai_model {
        agent_brand_asset("openai")
    } else if model.contains("opus") || model.contains("sonnet") || model.contains("haiku") {
        agent_brand_asset("claude")
    } else {
        agent_brand_asset(&model)
    }
}

fn open_tool_brand_asset(tool_id: &str) -> Option<BrandAsset> {
    let asset = match tool_id {
        "vscode" | "vscode_insiders" => BrandAsset {
            path: "icons/vibex/open-tools/visual-studio-code.svg",
            uses_current_color: false,
        },
        "cursor" => BrandAsset {
            path: "icons/vibex/open-tools/cursor.svg",
            uses_current_color: true,
        },
        "windsurf" => BrandAsset {
            path: "icons/vibex/open-tools/windsurf.svg",
            uses_current_color: true,
        },
        "zed" => BrandAsset {
            path: "icons/vibex/open-tools/zed.svg",
            uses_current_color: true,
        },
        "intellij" => BrandAsset {
            path: "icons/vibex/open-tools/intellij-idea.svg",
            uses_current_color: false,
        },
        "fleet" | "rustrover" => BrandAsset {
            path: "icons/vibex/open-tools/jetbrains.svg",
            uses_current_color: false,
        },
        "clion" => BrandAsset {
            path: "icons/vibex/open-tools/clion.svg",
            uses_current_color: false,
        },
        "goland" => BrandAsset {
            path: "icons/vibex/open-tools/goland.svg",
            uses_current_color: false,
        },
        "phpstorm" => BrandAsset {
            path: "icons/vibex/open-tools/phpstorm.svg",
            uses_current_color: false,
        },
        "pycharm" => BrandAsset {
            path: "icons/vibex/open-tools/pycharm.svg",
            uses_current_color: false,
        },
        "rider" => BrandAsset {
            path: "icons/vibex/open-tools/rider.svg",
            uses_current_color: false,
        },
        "sublime_text" => BrandAsset {
            path: "icons/vibex/open-tools/sublime-text.svg",
            uses_current_color: false,
        },
        "webstorm" => BrandAsset {
            path: "icons/vibex/open-tools/webstorm.svg",
            uses_current_color: false,
        },
        "xcode" => BrandAsset {
            path: "icons/vibex/open-tools/xcode.svg",
            uses_current_color: false,
        },
        _ => return None,
    };
    Some(asset)
}

pub(crate) fn open_tool_brand_icon(
    tool_id: &str,
    size: Pixels,
    current_color: Hsla,
) -> Option<AnyElement> {
    let asset = open_tool_brand_asset(tool_id)?;
    let current_color = if tool_id == "windsurf" {
        gpui::rgb(0x06b6d4).into()
    } else {
        current_color
    };
    Some(brand_asset_icon(asset, size, Some(current_color)))
}

fn brand_asset_icon(asset: BrandAsset, size: Pixels, current_color: Option<Hsla>) -> AnyElement {
    if !asset.uses_current_color {
        // GPUI's Icon paints SVGs as alpha masks; img preserves the embedded brand colors.
        return img(asset.path).size(size).flex_none().into_any_element();
    }
    themed_icon(Icon::default().path(asset.path).size(size), current_color)
}

fn themed_icon(icon: Icon, color: Option<Hsla>) -> AnyElement {
    if let Some(color) = color {
        icon.text_color(color).into_any_element()
    } else {
        icon.into_any_element()
    }
}

pub(crate) fn file_tree_asset_icon(path: &'static str, size: Pixels, color: Hsla) -> AnyElement {
    Icon::default()
        .path(path)
        .size(size)
        .text_color(color)
        .into_any_element()
}

impl AssetSource for VibexAssets {
    fn load(&self, path: &str) -> GpuiResult<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = VIBEX_ASSETS
            .iter()
            .chain(FILE_INTEGRATION_ASSETS.iter())
            .chain(AGENT_BRAND_ASSETS.iter())
            .find(|(asset_path, _)| *asset_path == path)
        {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> GpuiResult<Vec<SharedString>> {
        let mut assets = ComponentAssets.list(path)?;
        assets.extend(
            VIBEX_ASSETS
                .iter()
                .chain(FILE_INTEGRATION_ASSETS.iter())
                .chain(AGENT_BRAND_ASSETS.iter())
                .filter(|(asset_path, _)| asset_path.starts_with(path))
                .map(|(asset_path, _)| SharedString::from(*asset_path)),
        );
        Ok(assets)
    }
}
pub const INTER_OFL_NOTICE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../node_modules/.pnpm/@fontsource-variable+inter@5.2.8/node_modules/@fontsource-variable/inter/LICENSE"
));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetLoadReport {
    pub family: &'static str,
    pub package_root: &'static str,
    pub font_count: usize,
    pub font_sha256: [String; 2],
    pub notice_sha256: String,
}

pub fn load_fonts(cx: &mut App) -> Result<AssetLoadReport, String> {
    cx.text_system()
        .add_fonts(vec![
            Cow::Borrowed(INTER_LATIN),
            Cow::Borrowed(INTER_LATIN_EXT),
        ])
        .map_err(|error| format!("failed to load bundled Inter Variable fonts: {error}"))?;
    Ok(asset_report())
}

pub fn asset_report() -> AssetLoadReport {
    AssetLoadReport {
        family: "Inter Variable",
        package_root: INTER_PACKAGE_ROOT,
        font_count: 2,
        font_sha256: [sha256(INTER_LATIN), sha256(INTER_LATIN_EXT)],
        notice_sha256: sha256(INTER_OFL_NOTICE.as_bytes()),
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multicolor_agent_brands_use_polychrome_image_elements() {
        for identity in [
            "Claude Code",
            "Google Gemini",
            "OpenCode",
            "Qwen Code",
            "Cline",
            "Devin CLI",
            "Kiro CLI",
        ] {
            let mut icon = agent_brand_icon(identity, gpui::px(16.0), None);
            assert!(
                icon.downcast_mut::<gpui::Img>().is_some(),
                "{identity} must bypass GPUI's monochrome Icon renderer"
            );
        }
    }

    #[test]
    fn agent_brand_logo_is_optional_for_text_fallbacks() {
        assert!(agent_brand_logo("OpenCode", gpui::px(16.0), None).is_some());
        assert!(agent_brand_logo("custom acp", gpui::px(16.0), None).is_none());
    }

    #[test]
    fn every_catalog_agent_has_a_loadable_polychrome_brand_asset() {
        let assets = VibexAssets;

        for entry in vibex_core::acp_agent_catalog_entries() {
            let asset = agent_brand_asset(entry.id)
                .unwrap_or_else(|| panic!("{} is missing a brand asset", entry.id));
            assert!(
                assets.load(asset.path).unwrap().is_some(),
                "{} points to an unregistered brand asset: {}",
                entry.id,
                asset.path
            );

            if entry.id != "copilot" {
                let mut icon = agent_brand_icon(entry.id, gpui::px(16.0), None);
                assert!(
                    icon.downcast_mut::<gpui::Img>().is_some(),
                    "{} must use the polychrome image renderer",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn model_brands_match_tauri_model_name_detection() {
        let openai = agent_brand_asset("openai");
        assert_eq!(model_brand_asset("gpt-5.5"), openai);
        assert_eq!(model_brand_asset("o3-mini"), openai);
        assert_eq!(
            model_brand_asset("claude-sonnet-4"),
            agent_brand_asset("claude")
        );
        assert_eq!(
            model_brand_asset("gemini-2.5-pro"),
            agent_brand_asset("gemini")
        );
        assert_eq!(model_brand_asset("custom-model"), None);
    }

    #[test]
    fn open_tool_brands_cover_every_detected_desktop_tool() {
        for tool_id in [
            "vscode",
            "vscode_insiders",
            "cursor",
            "windsurf",
            "zed",
            "intellij",
            "webstorm",
            "pycharm",
            "goland",
            "rustrover",
            "clion",
            "phpstorm",
            "rider",
            "fleet",
            "sublime_text",
            "xcode",
        ] {
            assert!(
                open_tool_brand_asset(tool_id).is_some(),
                "missing {tool_id}"
            );
        }
        assert_eq!(open_tool_brand_asset("unknown"), None);
    }

    #[test]
    fn multicolor_open_tool_brands_bypass_the_icon_mask() {
        for tool_id in ["vscode", "intellij", "clion", "xcode"] {
            let mut icon = open_tool_brand_icon(tool_id, gpui::px(16.0), gpui::black()).unwrap();
            assert!(
                icon.downcast_mut::<gpui::Img>().is_some(),
                "{tool_id} must preserve its embedded colors"
            );
        }
        for tool_id in ["cursor", "windsurf", "zed"] {
            let mut icon = open_tool_brand_icon(tool_id, gpui::px(16.0), gpui::black()).unwrap();
            assert!(icon.downcast_mut::<gpui::Img>().is_none());
        }
    }

    #[test]
    fn bundled_font_and_notice_are_bounded_and_identified() {
        let report = asset_report();
        assert_eq!(report.family, "Inter Variable");
        assert_eq!(report.font_count, 2);
        assert!(report.font_sha256.iter().all(|hash| hash.len() == 64));
        assert_eq!(report.notice_sha256.len(), 64);
        assert!(INTER_OFL_NOTICE.contains("SIL OPEN FONT LICENSE"));
    }
}
