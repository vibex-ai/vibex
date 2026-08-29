use std::borrow::Cow;

use gpui::{App, AssetSource, Result, SharedString};

const IBM_PLEX_SANS_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf");
const IBM_PLEX_SANS_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf");
const IBM_PLEX_SANS_SEMIBOLD: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf");
const IBM_PLEX_SANS_SEMIBOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf");
const WQY_MICROHEI: &[u8] = include_bytes!("../assets/fonts/wqy-microhei/wqy-microhei.ttc");

const ICONS: &[&str] = &[
    "brand/logo.svg",
    "icons/vibex-mark.svg",
    "icons/menu.svg",
    "icons/plus.svg",
    "icons/circle-plus.svg",
    "icons/search.svg",
    "icons/list-checks.svg",
    "icons/sliders-horizontal.svg",
    "icons/refresh.svg",
    "icons/scan-line.svg",
    "icons/send.svg",
    "icons/stop.svg",
    "icons/zap.svg",
    "icons/x.svg",
    "icons/settings.svg",
    "icons/activity.svg",
    "icons/server.svg",
    "icons/chevron-right.svg",
    "icons/chevron-left.svg",
    "icons/chevron-down.svg",
    "icons/message-square.svg",
    "icons/pin.svg",
    "icons/crosshair.svg",
    "icons/grip-vertical.svg",
    "icons/folder.svg",
    "icons/folder-open.svg",
    "icons/triangle-alert.svg",
    "icons/git-branch.svg",
    "icons/image.svg",
    "icons/boxes.svg",
    "icons/code-xml.svg",
    "icons/file-terminal.svg",
    "icons/database.svg",
    "icons/hash.svg",
    "icons/book-open-text.svg",
    "icons/sparkles.svg",
    "icons/briefcase.svg",
    "icons/box.svg",
    "icons/globe.svg",
    "icons/cpu.svg",
    "icons/layers.svg",
    "icons/braces.svg",
    "icons/rocket.svg",
    "icons/wrench.svg",
    "icons/gift.svg",
    "icons/chart-column.svg",
    "icons/palette.svg",
    "icons/gauge.svg",
    "icons/workflow.svg",
    "icons/package.svg",
    "icons/log-out.svg",
    "icons/ellipsis-vertical.svg",
    "icons/chevrons-right-left.svg",
    "icons/chevrons-left-right.svg",
    "icons/pencil.svg",
    "icons/file-archive.svg",
    "icons/trash-2.svg",
    "icons/openai.svg",
    "icons/claude.svg",
    "icons/opencode.svg",
    "icons/gemini.svg",
    "icons/qwen.svg",
    "icons/copilot.svg",
    "icons/agents/amp-acp.svg",
    "icons/agents/auggie.svg",
    "icons/agents/cline.svg",
    "icons/agents/codebuddy-code.svg",
    "icons/agents/codewhale.svg",
    "icons/agents/crow-cli.svg",
    "icons/agents/cursor.svg",
    "icons/agents/deepagents.svg",
    "icons/agents/deepseek-harness.svg",
    "icons/agents/devin.svg",
    "icons/agents/dimcode.svg",
    "icons/agents/dirac.svg",
    "icons/agents/factory-droid.svg",
    "icons/agents/glm-acp-agent.svg",
    "icons/agents/goose.svg",
    "icons/agents/grok.svg",
    "icons/agents/hermes.svg",
    "icons/agents/junie.svg",
    "icons/agents/kilo.svg",
    "icons/agents/kimi.svg",
    "icons/agents/kiro.svg",
    "icons/agents/minion-code.svg",
    "icons/agents/mistral-vibe.svg",
    "icons/agents/nova.svg",
    "icons/agents/pi.svg",
    "icons/agents/poolside.svg",
    "icons/agents/qoder.svg",
    "icons/agents/stakpak.svg",
    "icons/agents/vtcode.svg",
];

pub struct MobileAssets;

pub fn load_fonts(cx: &mut App) -> Result<()> {
    cx.text_system().add_fonts(vec![
        Cow::Borrowed(IBM_PLEX_SANS_REGULAR),
        Cow::Borrowed(IBM_PLEX_SANS_ITALIC),
        Cow::Borrowed(IBM_PLEX_SANS_SEMIBOLD),
        Cow::Borrowed(IBM_PLEX_SANS_SEMIBOLD_ITALIC),
        Cow::Borrowed(WQY_MICROHEI),
    ])
}

impl AssetSource for MobileAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "brand/logo.svg" => Some(include_bytes!("../assets/brand/logo.svg")),
            "icons/vibex-mark.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/vibex-mark.svg"))
            }
            "icons/menu.svg" => Some(include_bytes!("../assets/icons/menu.svg")),
            "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
            "icons/circle-plus.svg" => Some(include_bytes!("../assets/icons/circle-plus.svg")),
            "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
            "icons/list-checks.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/list-checks.svg"))
            }
            "icons/sliders-horizontal.svg" => {
                Some(include_bytes!("../assets/icons/sliders-horizontal.svg"))
            }
            "icons/refresh.svg" => Some(include_bytes!("../assets/icons/refresh.svg")),
            "icons/scan-line.svg" => Some(include_bytes!("../assets/icons/scan-line.svg")),
            "icons/send.svg" => Some(include_bytes!("../assets/icons/send.svg")),
            "icons/stop.svg" => Some(include_bytes!("../assets/icons/stop.svg")),
            "icons/zap.svg" => Some(include_bytes!("../../desktop/assets/icons/zap.svg")),
            "icons/x.svg" => Some(include_bytes!("../assets/icons/x.svg")),
            "icons/settings.svg" => Some(include_bytes!("../assets/icons/settings.svg")),
            "icons/activity.svg" => Some(include_bytes!("../assets/icons/activity.svg")),
            "icons/server.svg" => Some(include_bytes!("../assets/icons/server.svg")),
            "icons/chevron-right.svg" => Some(include_bytes!("../assets/icons/chevron-right.svg")),
            "icons/chevron-left.svg" => Some(include_bytes!("../assets/icons/chevron-left.svg")),
            "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
            "icons/message-square.svg" => {
                Some(include_bytes!("../assets/icons/message-square.svg"))
            }
            "icons/pin.svg" => Some(include_bytes!("../assets/icons/pin.svg")),
            "icons/crosshair.svg" => Some(include_bytes!("../assets/icons/crosshair.svg")),
            "icons/grip-vertical.svg" => Some(include_bytes!("../assets/icons/grip-vertical.svg")),
            "icons/folder.svg" => Some(include_bytes!("../assets/icons/folder.svg")),
            "icons/folder-open.svg" => Some(include_bytes!("../assets/icons/folder-open.svg")),
            "icons/triangle-alert.svg" => {
                Some(include_bytes!("../assets/icons/triangle-alert.svg"))
            }
            "icons/git-branch.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/git-branch.svg"))
            }
            "icons/image.svg" => Some(include_bytes!("../../desktop/assets/icons/image.svg")),
            "icons/boxes.svg" => Some(include_bytes!("../../desktop/assets/icons/boxes.svg")),
            "icons/code-xml.svg" => Some(include_bytes!("../../desktop/assets/icons/code-xml.svg")),
            "icons/file-terminal.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/file-terminal.svg"
            )),
            "icons/database.svg" => Some(include_bytes!("../../desktop/assets/icons/database.svg")),
            "icons/hash.svg" => Some(include_bytes!("../../desktop/assets/icons/hash.svg")),
            "icons/book-open-text.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/book-open-text.svg"
            )),
            "icons/sparkles.svg" => Some(include_bytes!("../../desktop/assets/icons/sparkles.svg")),
            "icons/briefcase.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/briefcase.svg"))
            }
            "icons/box.svg" => Some(include_bytes!("../../desktop/assets/icons/box.svg")),
            "icons/globe.svg" => Some(include_bytes!("../../desktop/assets/icons/globe.svg")),
            "icons/cpu.svg" => Some(include_bytes!("../../desktop/assets/icons/cpu.svg")),
            "icons/layers.svg" => Some(include_bytes!("../../desktop/assets/icons/layers.svg")),
            "icons/braces.svg" => Some(include_bytes!("../../desktop/assets/icons/braces.svg")),
            "icons/rocket.svg" => Some(include_bytes!("../../desktop/assets/icons/rocket.svg")),
            "icons/wrench.svg" => Some(include_bytes!("../../desktop/assets/icons/wrench.svg")),
            "icons/gift.svg" => Some(include_bytes!("../../desktop/assets/icons/gift.svg")),
            "icons/chart-column.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/chart-column.svg"
            )),
            "icons/palette.svg" => Some(include_bytes!("../../desktop/assets/icons/palette.svg")),
            "icons/gauge.svg" => Some(include_bytes!("../../desktop/assets/icons/gauge.svg")),
            "icons/workflow.svg" => Some(include_bytes!("../../desktop/assets/icons/workflow.svg")),
            "icons/package.svg" => Some(include_bytes!("../../desktop/assets/icons/package.svg")),
            "icons/log-out.svg" => Some(include_bytes!("../assets/icons/log-out.svg")),
            "icons/ellipsis-vertical.svg" => {
                Some(include_bytes!("../assets/icons/ellipsis-vertical.svg"))
            }
            // The collapse/expand chevrons are the reviewed desktop marks so the
            // sessions toolbar reads identically on both shells.
            "icons/chevrons-right-left.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/chevrons-right-left.svg"
            )),
            "icons/chevrons-left-right.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/chevrons-left-right.svg"
            )),
            "icons/pencil.svg" => Some(include_bytes!("../../desktop/assets/icons/pencil.svg")),
            "icons/file-archive.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/file-archive.svg"
            )),
            "icons/trash-2.svg" => Some(include_bytes!("../../desktop/assets/icons/trash-2.svg")),
            // Reuse the reviewed desktop provider marks so the compact client
            // and desktop sidebar show the same Agent identity.
            "icons/openai.svg" => Some(include_bytes!("../../desktop/assets/icons/openai.svg")),
            "icons/claude.svg" => Some(include_bytes!("../../desktop/assets/icons/claude.svg")),
            "icons/opencode.svg" => Some(include_bytes!("../../desktop/assets/icons/opencode.svg")),
            "icons/gemini.svg" => Some(include_bytes!("../../desktop/assets/icons/gemini.svg")),
            "icons/qwen.svg" => Some(include_bytes!("../../desktop/assets/icons/qwen.svg")),
            "icons/copilot.svg" => Some(include_bytes!("../../desktop/assets/icons/copilot.svg")),
            "icons/agents/amp-acp.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/amp-acp.svg"
            )),
            "icons/agents/auggie.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/auggie.svg"
            )),
            "icons/agents/cline.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/cline.svg"
            )),
            "icons/agents/codebuddy-code.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/codebuddy-code.svg"
            )),
            "icons/agents/codewhale.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/codewhale.svg"
            )),
            "icons/agents/crow-cli.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/crow-cli.svg"
            )),
            "icons/agents/cursor.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/cursor.svg"
            )),
            "icons/agents/deepagents.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/deepagents.svg"
            )),
            "icons/agents/deepseek-harness.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/deepseek-harness.svg"
            )),
            "icons/agents/devin.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/devin.svg"
            )),
            "icons/agents/dimcode.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/dimcode.svg"
            )),
            "icons/agents/dirac.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/dirac.svg"
            )),
            "icons/agents/factory-droid.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/factory-droid.svg"
            )),
            "icons/agents/glm-acp-agent.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/glm-acp-agent.svg"
            )),
            "icons/agents/goose.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/goose.svg"
            )),
            "icons/agents/grok.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/agents/grok.svg"))
            }
            "icons/agents/hermes.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/hermes.svg"
            )),
            "icons/agents/junie.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/junie.svg"
            )),
            "icons/agents/kilo.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/agents/kilo.svg"))
            }
            "icons/agents/kimi.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/agents/kimi.svg"))
            }
            "icons/agents/kiro.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/agents/kiro.svg"))
            }
            "icons/agents/minion-code.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/minion-code.svg"
            )),
            "icons/agents/mistral-vibe.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/mistral-vibe.svg"
            )),
            "icons/agents/nova.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/agents/nova.svg"))
            }
            "icons/agents/pi.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/agents/pi.svg"))
            }
            "icons/agents/poolside.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/poolside.svg"
            )),
            "icons/agents/qoder.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/qoder.svg"
            )),
            "icons/agents/stakpak.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/stakpak.svg"
            )),
            "icons/agents/vtcode.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/vtcode.svg"
            )),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|item| item.starts_with(path))
            .map(|item| SharedString::from(*item))
            .collect())
    }
}
