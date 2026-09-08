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
    "icons/loader-circle.svg",
    "icons/refresh.svg",
    "icons/scan-line.svg",
    "icons/send.svg",
    "icons/stop.svg",
    "icons/clock.svg",
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
    "icons/copy.svg",
    "icons/git-branch.svg",
    "icons/wifi-outlined.svg",
    "icons/arrow-to-top.svg",
    "icons/download.svg",
    "icons/upload.svg",
    "icons/rotate-ccw.svg",
    "icons/undo.svg",
    "icons/chevrons-down-up.svg",
    "icons/file-code.svg",
    "icons/coffee.svg",
    "icons/file-braces.svg",
    "icons/file-spreadsheet.svg",
    "icons/audio-lines.svg",
    "icons/file-video-camera.svg",
    "icons/file-symlink.svg",
    "icons/file-cog.svg",
    "icons/file-lock.svg",
    "icons/file-key.svg",
    "icons/file-type.svg",
    "icons/file-text.svg",
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
    "icons/sun.svg",
    "icons/moon.svg",
    "icons/monitor.svg",
    "icons/check.svg",
    "icons/minus.svg",
    "icons/workflow.svg",
    "icons/package.svg",
    "icons/log-out.svg",
    "icons/ellipsis-vertical.svg",
    "icons/chevrons-right-left.svg",
    "icons/chevrons-left-right.svg",
    "icons/pencil.svg",
    "icons/brain.svg",
    "icons/file-plus.svg",
    "icons/file-archive.svg",
    "icons/trash-2.svg",
    "icons/plug-zap.svg",
    "icons/user.svg",
    "icons/bot.svg",
    "icons/square-terminal.svg",
    "icons/book-open.svg",
    "icons/openai.svg",
    "icons/claude.svg",
    "icons/opencode.svg",
    "icons/gemini.svg",
    "icons/qwen.svg",
    "icons/copilot.svg",
    "icons/agents/antigravity.svg",
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

// Brand marks bundled from the desktop asset pack under the same `icons/vibex/`
// paths the desktop asset source serves, so model and Agent brand lookups
// resolve identically on the phone.
const VIBEX_BRAND_ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/claude.svg",
        include_bytes!("../../desktop/assets/icons/claude.svg"),
    ),
    (
        "icons/copilot.svg",
        include_bytes!("../../desktop/assets/icons/copilot.svg"),
    ),
    (
        "icons/gemini.svg",
        include_bytes!("../../desktop/assets/icons/gemini.svg"),
    ),
    (
        "icons/openai.svg",
        include_bytes!("../../desktop/assets/icons/openai.svg"),
    ),
    (
        "icons/opencode.svg",
        include_bytes!("../../desktop/assets/icons/opencode.svg"),
    ),
    (
        "icons/qwen.svg",
        include_bytes!("../../desktop/assets/icons/qwen.svg"),
    ),
    (
        "icons/agents/deepseek-harness.svg",
        include_bytes!("../../desktop/assets/icons/agents/deepseek-harness.svg"),
    ),
    (
        "icons/agents/glm-acp-agent.svg",
        include_bytes!("../../desktop/assets/icons/agents/glm-acp-agent.svg"),
    ),
    (
        "icons/agents/grok.svg",
        include_bytes!("../../desktop/assets/icons/agents/grok.svg"),
    ),
    (
        "icons/agents/hermes.svg",
        include_bytes!("../../desktop/assets/icons/agents/hermes.svg"),
    ),
    (
        "icons/agents/kimi.svg",
        include_bytes!("../../desktop/assets/icons/agents/kimi.svg"),
    ),
    (
        "icons/agents/mistral-vibe.svg",
        include_bytes!("../../desktop/assets/icons/agents/mistral-vibe.svg"),
    ),
    (
        "icons/agents/poolside.svg",
        include_bytes!("../../desktop/assets/icons/agents/poolside.svg"),
    ),
];

const VIBEX_MODEL_PROVIDER_ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/model-providers/aion-labs.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/aion-labs.svg"),
    ),
    (
        "icons/model-providers/alibaba.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/alibaba.svg"),
    ),
    (
        "icons/model-providers/amazon.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/amazon.svg"),
    ),
    (
        "icons/model-providers/anthracite-org.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/anthracite-org.svg"),
    ),
    (
        "icons/model-providers/arcee-ai.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/arcee-ai.svg"),
    ),
    (
        "icons/model-providers/baai.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/baai.svg"),
    ),
    (
        "icons/model-providers/baidu.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/baidu.svg"),
    ),
    (
        "icons/model-providers/black-forest-labs.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/black-forest-labs.svg"),
    ),
    (
        "icons/model-providers/bytedance-seed.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/bytedance-seed.svg"),
    ),
    (
        "icons/model-providers/bytedance.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/bytedance.svg"),
    ),
    (
        "icons/model-providers/canopylabs.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/canopylabs.svg"),
    ),
    (
        "icons/model-providers/cognitivecomputations.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/cognitivecomputations.svg"),
    ),
    (
        "icons/model-providers/cohere.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/cohere.svg"),
    ),
    (
        "icons/model-providers/deepgram.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/deepgram.svg"),
    ),
    (
        "icons/model-providers/dots-studio.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/dots-studio.svg"),
    ),
    (
        "icons/model-providers/fish-audio.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/fish-audio.svg"),
    ),
    (
        "icons/model-providers/gryphe.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/gryphe.svg"),
    ),
    (
        "icons/model-providers/hexgrad.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/hexgrad.svg"),
    ),
    (
        "icons/model-providers/heygen.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/heygen.svg"),
    ),
    (
        "icons/model-providers/ibm-granite.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/ibm-granite.svg"),
    ),
    (
        "icons/model-providers/inception.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/inception.svg"),
    ),
    (
        "icons/model-providers/inclusionai.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/inclusionai.svg"),
    ),
    (
        "icons/model-providers/intfloat.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/intfloat.svg"),
    ),
    (
        "icons/model-providers/krea.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/krea.svg"),
    ),
    (
        "icons/model-providers/kwaipilot.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/kwaipilot.svg"),
    ),
    (
        "icons/model-providers/kwaivgi.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/kwaivgi.svg"),
    ),
    (
        "icons/model-providers/liquid.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/liquid.svg"),
    ),
    (
        "icons/model-providers/mai.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/mai.svg"),
    ),
    (
        "icons/model-providers/mancer.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/mancer.svg"),
    ),
    (
        "icons/model-providers/meituan.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/meituan.svg"),
    ),
    (
        "icons/model-providers/meta-llama.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/meta-llama.svg"),
    ),
    (
        "icons/model-providers/meta.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/meta.svg"),
    ),
    (
        "icons/model-providers/microsoft.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/microsoft.svg"),
    ),
    (
        "icons/model-providers/minimax.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/minimax.svg"),
    ),
    (
        "icons/model-providers/morph.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/morph.svg"),
    ),
    (
        "icons/model-providers/nex-agi.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/nex-agi.svg"),
    ),
    (
        "icons/model-providers/nvidia.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/nvidia.svg"),
    ),
    (
        "icons/model-providers/openrouter.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/openrouter.svg"),
    ),
    (
        "icons/model-providers/perceptron.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/perceptron.svg"),
    ),
    (
        "icons/model-providers/perplexity.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/perplexity.svg"),
    ),
    (
        "icons/model-providers/recraft.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/recraft.svg"),
    ),
    (
        "icons/model-providers/rekaai.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/rekaai.svg"),
    ),
    (
        "icons/model-providers/relace.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/relace.svg"),
    ),
    (
        "icons/model-providers/runway.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/runway.svg"),
    ),
    (
        "icons/model-providers/sakana.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/sakana.svg"),
    ),
    (
        "icons/model-providers/sao10k.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/sao10k.svg"),
    ),
    (
        "icons/model-providers/sentence-transformers.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/sentence-transformers.svg"),
    ),
    (
        "icons/model-providers/sesame.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/sesame.svg"),
    ),
    (
        "icons/model-providers/sourceful.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/sourceful.svg"),
    ),
    (
        "icons/model-providers/stepfun.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/stepfun.svg"),
    ),
    (
        "icons/model-providers/tencent.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/tencent.svg"),
    ),
    (
        "icons/model-providers/thedrummer.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/thedrummer.svg"),
    ),
    (
        "icons/model-providers/thenlper.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/thenlper.svg"),
    ),
    (
        "icons/model-providers/thinkingmachines.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/thinkingmachines.svg"),
    ),
    (
        "icons/model-providers/undi95.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/undi95.svg"),
    ),
    (
        "icons/model-providers/upstage.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/upstage.svg"),
    ),
    (
        "icons/model-providers/voyageai.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/voyageai.svg"),
    ),
    (
        "icons/model-providers/writer.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/writer.svg"),
    ),
    (
        "icons/model-providers/xiaomi.svg",
        include_bytes!("../../desktop/assets/icons/model-providers/xiaomi.svg"),
    ),
];

/// Every asset path the source serves, so the app can assert its brand
/// lookup tables never reference an unloaded path.
pub fn mobile_bundled_brand_paths() -> impl Iterator<Item = &'static str> {
    ICONS.iter().copied().chain(
        VIBEX_BRAND_ASSETS
            .iter()
            .chain(VIBEX_MODEL_PROVIDER_ASSETS.iter())
            .map(|(asset, _)| *asset),
    )
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
            "icons/loader-circle.svg" => Some(include_bytes!("../assets/icons/loader-circle.svg")),
            "icons/refresh.svg" => Some(include_bytes!("../assets/icons/refresh.svg")),
            "icons/scan-line.svg" => Some(include_bytes!("../assets/icons/scan-line.svg")),
            "icons/send.svg" => Some(include_bytes!("../assets/icons/send.svg")),
            "icons/stop.svg" => Some(include_bytes!("../assets/icons/stop.svg")),
            "icons/clock.svg" => Some(include_bytes!("../../desktop/assets/icons/clock.svg")),
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
            "icons/copy.svg" => Some(include_bytes!("../assets/icons/copy.svg")),
            "icons/wifi-outlined.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/wifi-outlined.svg"
            )),
            "icons/arrow-to-top.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/arrow-to-top.svg"
            )),
            "icons/git-branch.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/git-branch.svg"))
            }
            "icons/download.svg" => Some(include_bytes!("../../desktop/assets/icons/download.svg")),
            "icons/upload.svg" => Some(include_bytes!("../../desktop/assets/icons/upload.svg")),
            "icons/rotate-ccw.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/rotate-ccw.svg"))
            }
            "icons/undo.svg" => Some(include_bytes!("../../../vendor/zed/assets/icons/undo.svg")),
            "icons/chevrons-down-up.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/chevrons-down-up.svg"
            )),
            "icons/file-code.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/file-code.svg"))
            }
            "icons/coffee.svg" => Some(include_bytes!("../../desktop/assets/icons/coffee.svg")),
            "icons/file-braces.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/file-braces.svg"))
            }
            "icons/file-spreadsheet.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/file-spreadsheet.svg"
            )),
            "icons/audio-lines.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/audio-lines.svg"))
            }
            "icons/file-video-camera.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/file-video-camera.svg"
            )),
            "icons/file-symlink.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/file-symlink.svg"
            )),
            "icons/file-cog.svg" => Some(include_bytes!("../../desktop/assets/icons/file-cog.svg")),
            "icons/file-lock.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/file-lock.svg"))
            }
            "icons/file-key.svg" => Some(include_bytes!("../../desktop/assets/icons/file-key.svg")),
            "icons/file-type.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/file-type.svg"))
            }
            "icons/file-text.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/file-text.svg"))
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
            "icons/sun.svg" => Some(include_bytes!("../assets/icons/sun.svg")),
            "icons/moon.svg" => Some(include_bytes!("../assets/icons/moon.svg")),
            "icons/monitor.svg" => Some(include_bytes!("../../desktop/assets/icons/monitor.svg")),
            "icons/check.svg" => Some(include_bytes!("../assets/icons/check.svg")),
            "icons/minus.svg" => Some(include_bytes!("../assets/icons/minus.svg")),
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
            "icons/brain.svg" => Some(include_bytes!("../../desktop/assets/icons/brain.svg")),
            "icons/square-terminal.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/square-terminal.svg"
            )),
            "icons/book-open.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/book-open.svg"))
            }
            "icons/file-plus.svg" => {
                Some(include_bytes!("../../desktop/assets/icons/file-plus.svg"))
            }
            "icons/file-archive.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/file-archive.svg"
            )),
            "icons/trash-2.svg" => Some(include_bytes!("../../desktop/assets/icons/trash-2.svg")),
            "icons/plug-zap.svg" => Some(include_bytes!("../../desktop/assets/icons/plug-zap.svg")),
            "icons/user.svg" => Some(include_bytes!("../assets/icons/user.svg")),
            "icons/bot.svg" => Some(include_bytes!("../assets/icons/bot.svg")),
            // Reuse the reviewed desktop provider marks so the compact client
            // and desktop sidebar show the same Agent identity.
            "icons/openai.svg" => Some(include_bytes!("../../desktop/assets/icons/openai.svg")),
            "icons/claude.svg" => Some(include_bytes!("../../desktop/assets/icons/claude.svg")),
            "icons/opencode.svg" => Some(include_bytes!("../../desktop/assets/icons/opencode.svg")),
            "icons/gemini.svg" => Some(include_bytes!("../../desktop/assets/icons/gemini.svg")),
            "icons/qwen.svg" => Some(include_bytes!("../../desktop/assets/icons/qwen.svg")),
            "icons/copilot.svg" => Some(include_bytes!("../../desktop/assets/icons/copilot.svg")),
            "icons/agents/antigravity.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/agents/antigravity.svg"
            )),
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
        let bytes = bytes.or_else(|| {
            VIBEX_BRAND_ASSETS
                .iter()
                .chain(VIBEX_MODEL_PROVIDER_ASSETS.iter())
                .find(|(asset, _)| *asset == path)
                .map(|(_, bytes)| *bytes)
        });
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .copied()
            .chain(
                VIBEX_BRAND_ASSETS
                    .iter()
                    .chain(VIBEX_MODEL_PROVIDER_ASSETS.iter())
                    .map(|(asset, _)| *asset),
            )
            .filter(|item| item.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}
