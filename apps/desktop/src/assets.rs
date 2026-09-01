use std::{
    borrow::Cow,
    future::Future,
    sync::{Arc, OnceLock},
};

use gpui::{
    AnyElement, App, Asset, AssetSource, Hsla, ImageCacheError, IntoElement, Pixels, RenderImage,
    Result as GpuiResult, SharedString, Styled as _, Window, img,
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
const IBM_PLEX_SANS_REGULAR: &[u8] =
    include_bytes!("../../../vendor/zed/assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf");
const IBM_PLEX_SANS_ITALIC: &[u8] =
    include_bytes!("../../../vendor/zed/assets/fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf");
const IBM_PLEX_SANS_SEMIBOLD: &[u8] =
    include_bytes!("../../../vendor/zed/assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf");
const IBM_PLEX_SANS_SEMIBOLD_ITALIC: &[u8] =
    include_bytes!("../../../vendor/zed/assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf");
const LILEX_REGULAR: &[u8] =
    include_bytes!("../../../vendor/zed/assets/fonts/lilex/Lilex-Regular.ttf");
const LILEX_ITALIC: &[u8] =
    include_bytes!("../../../vendor/zed/assets/fonts/lilex/Lilex-Italic.ttf");
const LILEX_BOLD: &[u8] = include_bytes!("../../../vendor/zed/assets/fonts/lilex/Lilex-Bold.ttf");
const LILEX_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../vendor/zed/assets/fonts/lilex/Lilex-BoldItalic.ttf");
const WQY_MICROHEI: &[u8] =
    include_bytes!("../../mobile/assets/fonts/wqy-microhei/wqy-microhei.ttc");
const BUNDLED_GPUI_FONT_ASSETS: &[(&str, &[u8])] = &[
    (
        "fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf",
        IBM_PLEX_SANS_REGULAR,
    ),
    (
        "fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf",
        IBM_PLEX_SANS_ITALIC,
    ),
    (
        "fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf",
        IBM_PLEX_SANS_SEMIBOLD,
    ),
    (
        "fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf",
        IBM_PLEX_SANS_SEMIBOLD_ITALIC,
    ),
    ("fonts/lilex/Lilex-Regular.ttf", LILEX_REGULAR),
    ("fonts/lilex/Lilex-Italic.ttf", LILEX_ITALIC),
    ("fonts/lilex/Lilex-Bold.ttf", LILEX_BOLD),
    ("fonts/lilex/Lilex-BoldItalic.ttf", LILEX_BOLD_ITALIC),
    ("fonts/wqy-microhei/wqy-microhei.ttc", WQY_MICROHEI),
];
const APP_ICON: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/app-icons/icon.png"
));

pub fn window_icon() -> Result<Arc<image::RgbaImage>, String> {
    static WINDOW_ICON: OnceLock<Result<Arc<image::RgbaImage>, String>> = OnceLock::new();

    WINDOW_ICON
        .get_or_init(|| {
            let image = image::load_from_memory_with_format(APP_ICON, image::ImageFormat::Png)
                .map_err(|error| format!("failed to decode Vibex window icon: {error}"))?
                .resize_exact(256, 256, image::imageops::FilterType::Lanczos3)
                .into_rgba8();
            Ok(Arc::new(image))
        })
        .clone()
}

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
        "icons/vibex/crosshair.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/crosshair.svg"
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
        "icons/vibex/clock.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/clock.svg"
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
        "icons/vibex/wifi-outlined.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/wifi-outlined.svg"
        )),
    ),
    (
        "icons/vibex/arrow-to-top.svg",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/arrow-to-top.svg"
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

const PROJECT_LOGO_ASSETS: &[(&str, &[u8])] = &[
    bundled_icon_asset!("folder.svg"),
    bundled_icon_asset!("briefcase.svg"),
    bundled_icon_asset!("box.svg"),
    bundled_icon_asset!("globe.svg"),
    bundled_icon_asset!("server.svg"),
    bundled_icon_asset!("cpu.svg"),
    bundled_icon_asset!("layers.svg"),
    bundled_icon_asset!("braces.svg"),
    bundled_icon_asset!("rocket.svg"),
    bundled_icon_asset!("wrench.svg"),
    bundled_icon_asset!("gift.svg"),
    bundled_icon_asset!("chart-column.svg"),
    bundled_icon_asset!("palette.svg"),
    bundled_icon_asset!("gauge.svg"),
    bundled_icon_asset!("workflow.svg"),
    bundled_icon_asset!("package.svg"),
];

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
    bundled_icon_asset!("rectangle-outline.svg"),
    bundled_icon_asset!("circle-outline.svg"),
    bundled_icon_asset!("mosaic.svg"),
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
    bundled_icon_asset!("agents/amp-acp.svg"),
    bundled_icon_asset!("agents/antigravity.svg"),
    bundled_icon_asset!("agents/auggie.svg"),
    bundled_icon_asset!("agents/cline.svg"),
    bundled_icon_asset!("agents/codebuddy-code.svg"),
    bundled_icon_asset!("agents/codewhale.svg"),
    bundled_icon_asset!("agents/crow-cli.svg"),
    bundled_icon_asset!("agents/cursor.svg"),
    bundled_icon_asset!("agents/deepagents.svg"),
    bundled_icon_asset!("agents/deepseek-harness.svg"),
    bundled_icon_asset!("agents/devin.svg"),
    bundled_icon_asset!("agents/dimcode.svg"),
    bundled_icon_asset!("agents/dirac.svg"),
    bundled_icon_asset!("agents/factory-droid.svg"),
    bundled_icon_asset!("agents/glm-acp-agent.svg"),
    bundled_icon_asset!("agents/goose.svg"),
    bundled_icon_asset!("agents/grok.svg"),
    bundled_icon_asset!("agents/hermes.svg"),
    bundled_icon_asset!("agents/junie.svg"),
    bundled_icon_asset!("agents/kilo.svg"),
    bundled_icon_asset!("agents/kimi.svg"),
    bundled_icon_asset!("agents/kiro.svg"),
    bundled_icon_asset!("agents/minion-code.svg"),
    bundled_icon_asset!("agents/mistral-vibe.svg"),
    bundled_icon_asset!("agents/nova.svg"),
    bundled_icon_asset!("agents/pi.svg"),
    bundled_icon_asset!("agents/poolside.svg"),
    bundled_icon_asset!("agents/qoder.svg"),
    bundled_icon_asset!("agents/stakpak.svg"),
    bundled_icon_asset!("agents/vtcode.svg"),
];

// OpenRouter model provider marks are derived from the copied provider/model slug.
// A keyword is the slug prefix before the first '-', '_', or whitespace;
// dots remain part of a keyword (for example, `qwen3.5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ModelBrandRule {
    keyword: &'static str,
    asset: BrandAsset,
}

const OPENROUTER_PROVIDER_BRANDS: &[(&str, BrandAsset)] = &[
    (
        "aion-labs",
        colored_model_asset("icons/vibex/model-providers/aion-labs.svg"),
    ),
    (
        "alibaba",
        colored_model_asset("icons/vibex/model-providers/alibaba.svg"),
    ),
    (
        "amazon",
        colored_model_asset("icons/vibex/model-providers/amazon.svg"),
    ),
    (
        "anthracite-org",
        colored_model_asset("icons/vibex/model-providers/anthracite-org.svg"),
    ),
    ("anthropic", colored_model_asset("icons/vibex/claude.svg")),
    (
        "arcee-ai",
        colored_model_asset("icons/vibex/model-providers/arcee-ai.svg"),
    ),
    (
        "baai",
        colored_model_asset("icons/vibex/model-providers/baai.svg"),
    ),
    (
        "baidu",
        colored_model_asset("icons/vibex/model-providers/baidu.svg"),
    ),
    (
        "black-forest-labs",
        colored_model_asset("icons/vibex/model-providers/black-forest-labs.svg"),
    ),
    (
        "bytedance",
        colored_model_asset("icons/vibex/model-providers/bytedance.svg"),
    ),
    (
        "bytedance-seed",
        colored_model_asset("icons/vibex/model-providers/bytedance-seed.svg"),
    ),
    (
        "canopylabs",
        colored_model_asset("icons/vibex/model-providers/canopylabs.svg"),
    ),
    (
        "cognitivecomputations",
        colored_model_asset("icons/vibex/model-providers/cognitivecomputations.svg"),
    ),
    (
        "cohere",
        colored_model_asset("icons/vibex/model-providers/cohere.svg"),
    ),
    (
        "deepgram",
        themed_model_asset("icons/vibex/model-providers/deepgram.svg"),
    ),
    (
        "deepseek",
        colored_model_asset("icons/vibex/agents/deepseek-harness.svg"),
    ),
    (
        "deepseek-ai",
        colored_model_asset("icons/vibex/agents/deepseek-harness.svg"),
    ),
    (
        "dots-studio",
        colored_model_asset("icons/vibex/model-providers/dots-studio.svg"),
    ),
    (
        "fish-audio",
        colored_model_asset("icons/vibex/model-providers/fish-audio.svg"),
    ),
    ("google", colored_model_asset("icons/vibex/gemini.svg")),
    (
        "gryphe",
        colored_model_asset("icons/vibex/model-providers/gryphe.svg"),
    ),
    (
        "hexgrad",
        colored_model_asset("icons/vibex/model-providers/hexgrad.svg"),
    ),
    (
        "heygen",
        colored_model_asset("icons/vibex/model-providers/heygen.svg"),
    ),
    (
        "ibm-granite",
        themed_model_asset("icons/vibex/model-providers/ibm-granite.svg"),
    ),
    (
        "inception",
        themed_model_asset("icons/vibex/model-providers/inception.svg"),
    ),
    (
        "inclusionai",
        colored_model_asset("icons/vibex/model-providers/inclusionai.svg"),
    ),
    (
        "intfloat",
        colored_model_asset("icons/vibex/model-providers/intfloat.svg"),
    ),
    (
        "krea",
        colored_model_asset("icons/vibex/model-providers/krea.svg"),
    ),
    (
        "kwaipilot",
        colored_model_asset("icons/vibex/model-providers/kwaipilot.svg"),
    ),
    (
        "kwaivgi",
        colored_model_asset("icons/vibex/model-providers/kwaivgi.svg"),
    ),
    (
        "liquid",
        colored_model_asset("icons/vibex/model-providers/liquid.svg"),
    ),
    (
        "mancer",
        colored_model_asset("icons/vibex/model-providers/mancer.svg"),
    ),
    (
        "meituan",
        colored_model_asset("icons/vibex/model-providers/meituan.svg"),
    ),
    (
        "meta",
        colored_model_asset("icons/vibex/model-providers/meta.svg"),
    ),
    (
        "meta-llama",
        colored_model_asset("icons/vibex/model-providers/meta-llama.svg"),
    ),
    (
        "microsoft",
        colored_model_asset("icons/vibex/model-providers/microsoft.svg"),
    ),
    (
        "minimax",
        colored_model_asset("icons/vibex/model-providers/minimax.svg"),
    ),
    (
        "mistralai",
        colored_model_asset("icons/vibex/agents/mistral-vibe.svg"),
    ),
    (
        "moonshotai",
        colored_model_asset("icons/vibex/agents/kimi.svg"),
    ),
    (
        "morph",
        colored_model_asset("icons/vibex/model-providers/morph.svg"),
    ),
    (
        "nex-agi",
        themed_model_asset("icons/vibex/model-providers/nex-agi.svg"),
    ),
    (
        "nousresearch",
        themed_model_asset("icons/vibex/agents/hermes.svg"),
    ),
    (
        "nvidia",
        colored_model_asset("icons/vibex/model-providers/nvidia.svg"),
    ),
    ("openai", themed_model_asset("icons/vibex/openai.svg")),
    (
        "openrouter",
        colored_model_asset("icons/vibex/model-providers/openrouter.svg"),
    ),
    (
        "perceptron",
        colored_model_asset("icons/vibex/model-providers/perceptron.svg"),
    ),
    (
        "perplexity",
        colored_model_asset("icons/vibex/model-providers/perplexity.svg"),
    ),
    (
        "poolside",
        themed_model_asset("icons/vibex/agents/poolside.svg"),
    ),
    ("qwen", colored_model_asset("icons/vibex/qwen.svg")),
    (
        "recraft",
        themed_model_asset("icons/vibex/model-providers/recraft.svg"),
    ),
    (
        "rekaai",
        colored_model_asset("icons/vibex/model-providers/rekaai.svg"),
    ),
    (
        "relace",
        colored_model_asset("icons/vibex/model-providers/relace.svg"),
    ),
    (
        "runway",
        colored_model_asset("icons/vibex/model-providers/runway.svg"),
    ),
    (
        "sakana",
        colored_model_asset("icons/vibex/model-providers/sakana.svg"),
    ),
    (
        "sao10k",
        colored_model_asset("icons/vibex/model-providers/sao10k.svg"),
    ),
    (
        "sentence-transformers",
        colored_model_asset("icons/vibex/model-providers/sentence-transformers.svg"),
    ),
    (
        "sesame",
        colored_model_asset("icons/vibex/model-providers/sesame.svg"),
    ),
    (
        "sourceful",
        colored_model_asset("icons/vibex/model-providers/sourceful.svg"),
    ),
    (
        "stepfun",
        colored_model_asset("icons/vibex/model-providers/stepfun.svg"),
    ),
    (
        "tencent",
        colored_model_asset("icons/vibex/model-providers/tencent.svg"),
    ),
    (
        "thedrummer",
        colored_model_asset("icons/vibex/model-providers/thedrummer.svg"),
    ),
    (
        "thenlper",
        colored_model_asset("icons/vibex/model-providers/thenlper.svg"),
    ),
    (
        "thinkingmachines",
        colored_model_asset("icons/vibex/model-providers/thinkingmachines.svg"),
    ),
    (
        "undi95",
        colored_model_asset("icons/vibex/model-providers/undi95.svg"),
    ),
    (
        "upstage",
        colored_model_asset("icons/vibex/model-providers/upstage.svg"),
    ),
    (
        "voyageai",
        colored_model_asset("icons/vibex/model-providers/voyageai.svg"),
    ),
    (
        "writer",
        colored_model_asset("icons/vibex/model-providers/writer.svg"),
    ),
    ("x-ai", themed_model_asset("icons/vibex/agents/grok.svg")),
    (
        "xiaomi",
        colored_model_asset("icons/vibex/model-providers/xiaomi.svg"),
    ),
    (
        "z-ai",
        themed_model_asset("icons/vibex/agents/glm-acp-agent.svg"),
    ),
];

// Longer prefixes handle model names whose first keyword is shared by providers.
const OPENROUTER_MODEL_BRAND_PREFIXES: &[(&str, BrandAsset)] = &[
    (
        "microsoft/mai",
        colored_model_asset("icons/vibex/model-providers/mai.svg"),
    ),
    (
        "deepgram/flux",
        themed_model_asset("icons/vibex/model-providers/deepgram.svg"),
    ),
    (
        "deepgram/nova",
        themed_model_asset("icons/vibex/model-providers/deepgram.svg"),
    ),
    (
        "cohere/rerank",
        colored_model_asset("icons/vibex/model-providers/cohere.svg"),
    ),
    (
        "voyageai/rerank",
        colored_model_asset("icons/vibex/model-providers/voyageai.svg"),
    ),
    (
        "nvidia/llama",
        colored_model_asset("icons/vibex/model-providers/nvidia.svg"),
    ),
    (
        "meta-llama/llama",
        colored_model_asset("icons/vibex/model-providers/meta-llama.svg"),
    ),
    (
        "flux-tts",
        themed_model_asset("icons/vibex/model-providers/deepgram.svg"),
    ),
    (
        "nova-3",
        themed_model_asset("icons/vibex/model-providers/deepgram.svg"),
    ),
    (
        "rerank-4",
        colored_model_asset("icons/vibex/model-providers/cohere.svg"),
    ),
    (
        "rerank-v3.5",
        colored_model_asset("icons/vibex/model-providers/cohere.svg"),
    ),
    (
        "llama-nemotron",
        colored_model_asset("icons/vibex/model-providers/nvidia.svg"),
    ),
];

const OPENROUTER_MODEL_BRANDS: &[ModelBrandRule] = &[
    ModelBrandRule {
        keyword: "mai",
        asset: colored_model_asset("icons/vibex/model-providers/mai.svg"),
    },
    ModelBrandRule {
        keyword: "multilingual",
        asset: colored_model_asset("icons/vibex/model-providers/intfloat.svg"),
    },
    ModelBrandRule {
        keyword: "bodybuilder",
        asset: colored_model_asset("icons/vibex/model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "happyhorse",
        asset: colored_model_asset("icons/vibex/model-providers/alibaba.svg"),
    },
    ModelBrandRule {
        keyword: "paraphrase",
        asset: colored_model_asset("icons/vibex/model-providers/sentence-transformers.svg"),
    },
    ModelBrandRule {
        keyword: "perceptron",
        asset: colored_model_asset("icons/vibex/model-providers/perceptron.svg"),
    },
    ModelBrandRule {
        keyword: "transcribe",
        asset: colored_model_asset("icons/vibex/model-providers/fish-audio.svg"),
    },
    ModelBrandRule {
        keyword: "unslopnemo",
        asset: colored_model_asset("icons/vibex/model-providers/thedrummer.svg"),
    },
    ModelBrandRule {
        keyword: "codestral",
        asset: colored_model_asset("icons/vibex/agents/mistral-vibe.svg"),
    },
    ModelBrandRule {
        keyword: "ministral",
        asset: colored_model_asset("icons/vibex/agents/mistral-vibe.svg"),
    },
    ModelBrandRule {
        keyword: "riverflow",
        asset: colored_model_asset("icons/vibex/model-providers/sourceful.svg"),
    },
    ModelBrandRule {
        keyword: "deepseek",
        asset: colored_model_asset("icons/vibex/agents/deepseek-harness.svg"),
    },
    ModelBrandRule {
        keyword: "devstral",
        asset: colored_model_asset("icons/vibex/agents/mistral-vibe.svg"),
    },
    ModelBrandRule {
        keyword: "mythomax",
        asset: colored_model_asset("icons/vibex/model-providers/gryphe.svg"),
    },
    ModelBrandRule {
        keyword: "nemotron",
        asset: colored_model_asset("icons/vibex/model-providers/nvidia.svg"),
    },
    ModelBrandRule {
        keyword: "parakeet",
        asset: colored_model_asset("icons/vibex/model-providers/nvidia.svg"),
    },
    ModelBrandRule {
        keyword: "seedance",
        asset: colored_model_asset("icons/vibex/model-providers/bytedance.svg"),
    },
    ModelBrandRule {
        keyword: "seedream",
        asset: colored_model_asset("icons/vibex/model-providers/bytedance-seed.svg"),
    },
    ModelBrandRule {
        keyword: "wizardlm",
        asset: colored_model_asset("icons/vibex/model-providers/microsoft.svg"),
    },
    ModelBrandRule {
        keyword: "command",
        asset: colored_model_asset("icons/vibex/model-providers/cohere.svg"),
    },
    ModelBrandRule {
        keyword: "cydonia",
        asset: colored_model_asset("icons/vibex/model-providers/thedrummer.svg"),
    },
    ModelBrandRule {
        keyword: "dolphin",
        asset: colored_model_asset("icons/vibex/model-providers/cognitivecomputations.svg"),
    },
    ModelBrandRule {
        keyword: "granite",
        asset: themed_model_asset("icons/vibex/model-providers/ibm-granite.svg"),
    },
    ModelBrandRule {
        keyword: "hunyuan",
        asset: colored_model_asset("icons/vibex/model-providers/tencent.svg"),
    },
    ModelBrandRule {
        keyword: "inkling",
        asset: colored_model_asset("icons/vibex/model-providers/thinkingmachines.svg"),
    },
    ModelBrandRule {
        keyword: "longcat",
        asset: colored_model_asset("icons/vibex/model-providers/meituan.svg"),
    },
    ModelBrandRule {
        keyword: "mercury",
        asset: themed_model_asset("icons/vibex/model-providers/inception.svg"),
    },
    ModelBrandRule {
        keyword: "minimax",
        asset: colored_model_asset("icons/vibex/model-providers/minimax.svg"),
    },
    ModelBrandRule {
        keyword: "mistral",
        asset: colored_model_asset("icons/vibex/agents/mistral-vibe.svg"),
    },
    ModelBrandRule {
        keyword: "mixtral",
        asset: colored_model_asset("icons/vibex/agents/mistral-vibe.svg"),
    },
    ModelBrandRule {
        keyword: "orpheus",
        asset: colored_model_asset("icons/vibex/model-providers/canopylabs.svg"),
    },
    ModelBrandRule {
        keyword: "palmyra",
        asset: colored_model_asset("icons/vibex/model-providers/writer.svg"),
    },
    ModelBrandRule {
        keyword: "qwen2.5",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "qwen3.5",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "qwen3.6",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "qwen3.7",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "qwen3.8",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "recraft",
        asset: themed_model_asset("icons/vibex/model-providers/recraft.svg"),
    },
    ModelBrandRule {
        keyword: "skyfall",
        asset: colored_model_asset("icons/vibex/model-providers/thedrummer.svg"),
    },
    ModelBrandRule {
        keyword: "trinity",
        asset: colored_model_asset("icons/vibex/model-providers/arcee-ai.svg"),
    },
    ModelBrandRule {
        keyword: "voxtral",
        asset: colored_model_asset("icons/vibex/agents/mistral-vibe.svg"),
    },
    ModelBrandRule {
        keyword: "whisper",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "avatar",
        asset: colored_model_asset("icons/vibex/model-providers/heygen.svg"),
    },
    ModelBrandRule {
        keyword: "claude",
        asset: colored_model_asset("icons/vibex/claude.svg"),
    },
    ModelBrandRule {
        keyword: "flux.2",
        asset: colored_model_asset("icons/vibex/model-providers/black-forest-labs.svg"),
    },
    ModelBrandRule {
        keyword: "fusion",
        asset: colored_model_asset("icons/vibex/model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "gemini",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "hailuo",
        asset: colored_model_asset("icons/vibex/model-providers/minimax.svg"),
    },
    ModelBrandRule {
        keyword: "hermes",
        asset: themed_model_asset("icons/vibex/agents/hermes.svg"),
    },
    ModelBrandRule {
        keyword: "kokoro",
        asset: colored_model_asset("icons/vibex/model-providers/hexgrad.svg"),
    },
    ModelBrandRule {
        keyword: "laguna",
        asset: themed_model_asset("icons/vibex/agents/poolside.svg"),
    },
    ModelBrandRule {
        keyword: "magnum",
        asset: colored_model_asset("icons/vibex/model-providers/anthracite-org.svg"),
    },
    ModelBrandRule {
        keyword: "pareto",
        asset: colored_model_asset("icons/vibex/model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "relace",
        asset: colored_model_asset("icons/vibex/model-providers/relace.svg"),
    },
    ModelBrandRule {
        keyword: "rerank",
        asset: colored_model_asset("icons/vibex/model-providers/voyageai.svg"),
    },
    ModelBrandRule {
        keyword: "sakana",
        asset: colored_model_asset("icons/vibex/model-providers/sakana.svg"),
    },
    ModelBrandRule {
        keyword: "speech",
        asset: colored_model_asset("icons/vibex/model-providers/minimax.svg"),
    },
    ModelBrandRule {
        keyword: "voyage",
        asset: colored_model_asset("icons/vibex/model-providers/voyageai.svg"),
    },
    ModelBrandRule {
        keyword: "weaver",
        asset: colored_model_asset("icons/vibex/model-providers/mancer.svg"),
    },
    ModelBrandRule {
        keyword: "aleph",
        asset: colored_model_asset("icons/vibex/model-providers/runway.svg"),
    },
    ModelBrandRule {
        keyword: "chirp",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "ernie",
        asset: colored_model_asset("icons/vibex/model-providers/baidu.svg"),
    },
    ModelBrandRule {
        keyword: "gemma",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "kling",
        asset: colored_model_asset("icons/vibex/model-providers/kwaivgi.svg"),
    },
    ModelBrandRule {
        keyword: "llama",
        asset: colored_model_asset("icons/vibex/model-providers/meta-llama.svg"),
    },
    ModelBrandRule {
        keyword: "lyria",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "morph",
        asset: colored_model_asset("icons/vibex/model-providers/morph.svg"),
    },
    ModelBrandRule {
        keyword: "multi",
        asset: colored_model_asset("icons/vibex/model-providers/sentence-transformers.svg"),
    },
    ModelBrandRule {
        keyword: "north",
        asset: colored_model_asset("icons/vibex/model-providers/cohere.svg"),
    },
    ModelBrandRule {
        keyword: "qwen3",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "solar",
        asset: colored_model_asset("icons/vibex/model-providers/upstage.svg"),
    },
    ModelBrandRule {
        keyword: "sonar",
        asset: colored_model_asset("icons/vibex/model-providers/perplexity.svg"),
    },
    ModelBrandRule {
        keyword: "aion",
        asset: colored_model_asset("icons/vibex/model-providers/aion-labs.svg"),
    },
    ModelBrandRule {
        keyword: "aura",
        asset: themed_model_asset("icons/vibex/model-providers/deepgram.svg"),
    },
    ModelBrandRule {
        keyword: "auto",
        asset: colored_model_asset("icons/vibex/model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "dots",
        asset: colored_model_asset("icons/vibex/model-providers/dots-studio.svg"),
    },
    ModelBrandRule {
        keyword: "flux",
        asset: colored_model_asset("icons/vibex/model-providers/black-forest-labs.svg"),
    },
    ModelBrandRule {
        keyword: "free",
        asset: colored_model_asset("icons/vibex/model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "fugu",
        asset: colored_model_asset("icons/vibex/model-providers/sakana.svg"),
    },
    ModelBrandRule {
        keyword: "grok",
        asset: themed_model_asset("icons/vibex/agents/grok.svg"),
    },
    ModelBrandRule {
        keyword: "kimi",
        asset: colored_model_asset("icons/vibex/agents/kimi.svg"),
    },
    ModelBrandRule {
        keyword: "krea",
        asset: colored_model_asset("icons/vibex/model-providers/krea.svg"),
    },
    ModelBrandRule {
        keyword: "l3.1",
        asset: colored_model_asset("icons/vibex/model-providers/sao10k.svg"),
    },
    ModelBrandRule {
        keyword: "l3.3",
        asset: colored_model_asset("icons/vibex/model-providers/sao10k.svg"),
    },
    ModelBrandRule {
        keyword: "ling",
        asset: colored_model_asset("icons/vibex/model-providers/inclusionai.svg"),
    },
    ModelBrandRule {
        keyword: "mimo",
        asset: colored_model_asset("icons/vibex/model-providers/xiaomi.svg"),
    },
    ModelBrandRule {
        keyword: "muse",
        asset: colored_model_asset("icons/vibex/model-providers/meta.svg"),
    },
    ModelBrandRule {
        keyword: "nova",
        asset: colored_model_asset("icons/vibex/model-providers/amazon.svg"),
    },
    ModelBrandRule {
        keyword: "pplx",
        asset: colored_model_asset("icons/vibex/model-providers/perplexity.svg"),
    },
    ModelBrandRule {
        keyword: "qwen",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "reka",
        asset: colored_model_asset("icons/vibex/model-providers/rekaai.svg"),
    },
    ModelBrandRule {
        keyword: "remm",
        asset: colored_model_asset("icons/vibex/model-providers/undi95.svg"),
    },
    ModelBrandRule {
        keyword: "s2.1",
        asset: colored_model_asset("icons/vibex/model-providers/fish-audio.svg"),
    },
    ModelBrandRule {
        keyword: "seed",
        asset: colored_model_asset("icons/vibex/model-providers/bytedance-seed.svg"),
    },
    ModelBrandRule {
        keyword: "sora",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "step",
        asset: colored_model_asset("icons/vibex/model-providers/stepfun.svg"),
    },
    ModelBrandRule {
        keyword: "text",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "all",
        asset: colored_model_asset("icons/vibex/model-providers/sentence-transformers.svg"),
    },
    ModelBrandRule {
        keyword: "bge",
        asset: colored_model_asset("icons/vibex/model-providers/baai.svg"),
    },
    ModelBrandRule {
        keyword: "csm",
        asset: colored_model_asset("icons/vibex/model-providers/sesame.svg"),
    },
    ModelBrandRule {
        keyword: "gen",
        asset: colored_model_asset("icons/vibex/model-providers/runway.svg"),
    },
    ModelBrandRule {
        keyword: "glm",
        asset: themed_model_asset("icons/vibex/agents/glm-acp-agent.svg"),
    },
    ModelBrandRule {
        keyword: "gpt",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "gte",
        asset: colored_model_asset("icons/vibex/model-providers/thenlper.svg"),
    },
    ModelBrandRule {
        keyword: "hy3",
        asset: colored_model_asset("icons/vibex/model-providers/tencent.svg"),
    },
    ModelBrandRule {
        keyword: "hy4",
        asset: colored_model_asset("icons/vibex/model-providers/tencent.svg"),
    },
    ModelBrandRule {
        keyword: "kat",
        asset: colored_model_asset("icons/vibex/model-providers/kwaipilot.svg"),
    },
    ModelBrandRule {
        keyword: "lfm",
        asset: colored_model_asset("icons/vibex/model-providers/liquid.svg"),
    },
    ModelBrandRule {
        keyword: "nex",
        asset: themed_model_asset("icons/vibex/model-providers/nex-agi.svg"),
    },
    ModelBrandRule {
        keyword: "phi",
        asset: colored_model_asset("icons/vibex/model-providers/microsoft.svg"),
    },
    ModelBrandRule {
        keyword: "veo",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "wan",
        asset: colored_model_asset("icons/vibex/model-providers/alibaba.svg"),
    },
    ModelBrandRule {
        keyword: "e5",
        asset: colored_model_asset("icons/vibex/model-providers/intfloat.svg"),
    },
    ModelBrandRule {
        keyword: "hy",
        asset: colored_model_asset("icons/vibex/model-providers/tencent.svg"),
    },
    ModelBrandRule {
        keyword: "l3",
        asset: colored_model_asset("icons/vibex/model-providers/sao10k.svg"),
    },
    ModelBrandRule {
        keyword: "o1",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "o3",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "o4",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "s1",
        asset: colored_model_asset("icons/vibex/model-providers/fish-audio.svg"),
    },
    ModelBrandRule {
        keyword: "s2",
        asset: colored_model_asset("icons/vibex/model-providers/fish-audio.svg"),
    },
    ModelBrandRule {
        keyword: "ui",
        asset: colored_model_asset("icons/vibex/model-providers/bytedance.svg"),
    },
];

// Keep model IDs emitted by the built-in Agent integrations on their existing marks.
const MODEL_AGENT_COMPATIBILITY_BRANDS: &[(&str, BrandAsset)] = &[
    ("opencode", colored_model_asset("icons/vibex/opencode.svg")),
    ("codex", themed_model_asset("icons/vibex/openai.svg")),
    ("chatgpt", themed_model_asset("icons/vibex/openai.svg")),
    ("copilot", themed_model_asset("icons/vibex/copilot.svg")),
    ("tongyi", colored_model_asset("icons/vibex/qwen.svg")),
    ("dashscope", colored_model_asset("icons/vibex/qwen.svg")),
];

// Catalog snapshot: 486 unique slugs and 121 keyword families.
macro_rules! bundled_model_provider_asset {
    ($file:literal) => {
        (
            concat!("icons/vibex/model-providers/", $file),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/model-providers/",
                $file
            )),
        )
    };
}

const MODEL_PROVIDER_ASSETS: &[(&str, &[u8])] = &[
    bundled_model_provider_asset!("aion-labs.svg"),
    bundled_model_provider_asset!("alibaba.svg"),
    bundled_model_provider_asset!("amazon.svg"),
    bundled_model_provider_asset!("anthracite-org.svg"),
    bundled_model_provider_asset!("arcee-ai.svg"),
    bundled_model_provider_asset!("baai.svg"),
    bundled_model_provider_asset!("baidu.svg"),
    bundled_model_provider_asset!("black-forest-labs.svg"),
    bundled_model_provider_asset!("bytedance-seed.svg"),
    bundled_model_provider_asset!("bytedance.svg"),
    bundled_model_provider_asset!("canopylabs.svg"),
    bundled_model_provider_asset!("cognitivecomputations.svg"),
    bundled_model_provider_asset!("cohere.svg"),
    bundled_model_provider_asset!("deepgram.svg"),
    bundled_model_provider_asset!("dots-studio.svg"),
    bundled_model_provider_asset!("fish-audio.svg"),
    bundled_model_provider_asset!("gryphe.svg"),
    bundled_model_provider_asset!("hexgrad.svg"),
    bundled_model_provider_asset!("heygen.svg"),
    bundled_model_provider_asset!("ibm-granite.svg"),
    bundled_model_provider_asset!("inception.svg"),
    bundled_model_provider_asset!("inclusionai.svg"),
    bundled_model_provider_asset!("intfloat.svg"),
    bundled_model_provider_asset!("krea.svg"),
    bundled_model_provider_asset!("kwaipilot.svg"),
    bundled_model_provider_asset!("kwaivgi.svg"),
    bundled_model_provider_asset!("liquid.svg"),
    bundled_model_provider_asset!("mai.svg"),
    bundled_model_provider_asset!("mancer.svg"),
    bundled_model_provider_asset!("meituan.svg"),
    bundled_model_provider_asset!("meta-llama.svg"),
    bundled_model_provider_asset!("meta.svg"),
    bundled_model_provider_asset!("microsoft.svg"),
    bundled_model_provider_asset!("minimax.svg"),
    bundled_model_provider_asset!("morph.svg"),
    bundled_model_provider_asset!("nex-agi.svg"),
    bundled_model_provider_asset!("nvidia.svg"),
    bundled_model_provider_asset!("openrouter.svg"),
    bundled_model_provider_asset!("perceptron.svg"),
    bundled_model_provider_asset!("perplexity.svg"),
    bundled_model_provider_asset!("recraft.svg"),
    bundled_model_provider_asset!("rekaai.svg"),
    bundled_model_provider_asset!("relace.svg"),
    bundled_model_provider_asset!("runway.svg"),
    bundled_model_provider_asset!("sakana.svg"),
    bundled_model_provider_asset!("sao10k.svg"),
    bundled_model_provider_asset!("sentence-transformers.svg"),
    bundled_model_provider_asset!("sesame.svg"),
    bundled_model_provider_asset!("sourceful.svg"),
    bundled_model_provider_asset!("stepfun.svg"),
    bundled_model_provider_asset!("tencent.svg"),
    bundled_model_provider_asset!("thedrummer.svg"),
    bundled_model_provider_asset!("thenlper.svg"),
    bundled_model_provider_asset!("thinkingmachines.svg"),
    bundled_model_provider_asset!("undi95.svg"),
    bundled_model_provider_asset!("upstage.svg"),
    bundled_model_provider_asset!("voyageai.svg"),
    bundled_model_provider_asset!("writer.svg"),
    bundled_model_provider_asset!("xiaomi.svg"),
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

const fn themed_agent_asset(path: &'static str) -> BrandAsset {
    BrandAsset {
        path,
        uses_current_color: true,
    }
}

const fn colored_model_asset(path: &'static str) -> BrandAsset {
    colored_agent_asset(path)
}

const fn themed_model_asset(path: &'static str) -> BrandAsset {
    themed_agent_asset(path)
}

const CATALOG_AGENT_BRANDS: &[(&str, BrandAsset)] = &[
    (
        "antigravity",
        colored_agent_asset("icons/vibex/agents/antigravity.svg"),
    ),
    (
        "amp-acp",
        colored_agent_asset("icons/vibex/agents/amp-acp.svg"),
    ),
    (
        "auggie",
        themed_agent_asset("icons/vibex/agents/auggie.svg"),
    ),
    ("cline", themed_agent_asset("icons/vibex/agents/cline.svg")),
    (
        "codebuddy-code",
        colored_agent_asset("icons/vibex/agents/codebuddy-code.svg"),
    ),
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
        themed_agent_asset("icons/vibex/agents/cursor.svg"),
    ),
    (
        "deepagents",
        themed_agent_asset("icons/vibex/agents/deepagents.svg"),
    ),
    (
        "deepseek-harness",
        colored_agent_asset("icons/vibex/agents/deepseek-harness.svg"),
    ),
    ("devin", themed_agent_asset("icons/vibex/agents/devin.svg")),
    (
        "dimcode",
        colored_agent_asset("icons/vibex/agents/dimcode.svg"),
    ),
    ("dirac", themed_agent_asset("icons/vibex/agents/dirac.svg")),
    (
        "factory-droid",
        themed_agent_asset("icons/vibex/agents/factory-droid.svg"),
    ),
    (
        "glm-acp-agent",
        themed_agent_asset("icons/vibex/agents/glm-acp-agent.svg"),
    ),
    (
        "zcode",
        themed_agent_asset("icons/vibex/agents/glm-acp-agent.svg"),
    ),
    ("goose", themed_agent_asset("icons/vibex/agents/goose.svg")),
    ("grok", themed_agent_asset("icons/vibex/agents/grok.svg")),
    (
        "hermes",
        themed_agent_asset("icons/vibex/agents/hermes.svg"),
    ),
    ("junie", colored_agent_asset("icons/vibex/agents/junie.svg")),
    ("kilo", themed_agent_asset("icons/vibex/agents/kilo.svg")),
    ("kiro", colored_agent_asset("icons/vibex/agents/kiro.svg")),
    ("kimi", colored_agent_asset("icons/vibex/agents/kimi.svg")),
    (
        "minion-code",
        themed_agent_asset("icons/vibex/agents/minion-code.svg"),
    ),
    (
        "mistral-vibe",
        colored_agent_asset("icons/vibex/agents/mistral-vibe.svg"),
    ),
    ("nova", themed_agent_asset("icons/vibex/agents/nova.svg")),
    ("pi", themed_agent_asset("icons/vibex/agents/pi.svg")),
    (
        "poolside",
        themed_agent_asset("icons/vibex/agents/poolside.svg"),
    ),
    ("qoder", colored_agent_asset("icons/vibex/agents/qoder.svg")),
    (
        "stakpak",
        themed_agent_asset("icons/vibex/agents/stakpak.svg"),
    ),
    (
        "vtcode",
        themed_agent_asset("icons/vibex/agents/vtcode.svg"),
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

fn model_keyword_matches(model: &str, keyword: &str) -> bool {
    model.starts_with(keyword)
        && model.as_bytes().get(keyword.len()).is_none_or(|character| {
            matches!(
                character,
                b'-' | b'_' | b'.' | b'/' | b':' | b' ' | b'\t' | b'\n' | b'\r'
            )
        })
}

fn model_brand_prefix(model: &str) -> Option<BrandAsset> {
    OPENROUTER_MODEL_BRAND_PREFIXES
        .iter()
        .filter(|(prefix, _)| model_keyword_matches(model, prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, asset)| *asset)
}

fn model_brand_asset(model: &str) -> Option<BrandAsset> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.strip_prefix('~').unwrap_or(&normalized);
    if normalized.is_empty() {
        return None;
    }

    // Explicit model/provider exceptions must win over generic keywords.
    if let Some(asset) = model_brand_prefix(normalized) {
        return Some(asset);
    }

    let (provider, model_name) = normalized
        .split_once('/')
        .map_or((normalized, normalized), |(provider, model_name)| {
            (provider, model_name)
        });
    if let Some((_, asset)) = OPENROUTER_PROVIDER_BRANDS
        .iter()
        .find(|(candidate, _)| *candidate == provider)
    {
        return Some(*asset);
    }

    if let Some(asset) = model_brand_prefix(model_name) {
        return Some(asset);
    }

    if let Some(rule) = OPENROUTER_MODEL_BRANDS
        .iter()
        .filter(|rule| model_keyword_matches(model_name, rule.keyword))
        .max_by_key(|rule| rule.keyword.len())
    {
        return Some(rule.asset);
    }

    if let Some((_, asset)) = MODEL_AGENT_COMPATIBILITY_BRANDS
        .iter()
        .find(|(keyword, _)| model_keyword_matches(model_name, keyword))
    {
        return Some(*asset);
    }

    // Keep the aliases accepted by the original Tauri model selector.
    if ["gpt", "o1", "o3", "o4", "o5"]
        .iter()
        .any(|keyword| model_keyword_matches(model_name, keyword))
    {
        agent_brand_asset("openai")
    } else if ["opus", "sonnet", "haiku"]
        .iter()
        .any(|keyword| model_keyword_matches(model_name, keyword))
    {
        agent_brand_asset("claude")
    } else {
        None
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
        return agent_brand_intrinsic_width(asset.path).map_or_else(
            || img(asset.path).size(size).flex_none().into_any_element(),
            |intrinsic_width| exact_size_polychrome_svg(asset.path, size, intrinsic_width),
        );
    }
    themed_icon(Icon::default().path(asset.path).size(size), current_color)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SizedSvgSource {
    path: &'static str,
    logical_size_bits: u32,
    intrinsic_width: u16,
}

enum SizedSvgAsset {}

impl Asset for SizedSvgAsset {
    type Source = SizedSvgSource;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let asset_source = cx.asset_source().clone();
        let svg_renderer = cx.svg_renderer();
        async move {
            let bytes = asset_source
                .load(source.path)
                .map_err(|error| ImageCacheError::Asset(error.to_string().into()))?
                .ok_or_else(|| {
                    ImageCacheError::Asset(
                        format!("Embedded resource not found: {}", source.path).into(),
                    )
                })?;
            let logical_size = f32::from_bits(source.logical_size_bits);
            svg_renderer
                .render_single_frame(&bytes, logical_size / f32::from(source.intrinsic_width))
                .map_err(Into::into)
        }
    }
}

fn exact_size_polychrome_svg(path: &'static str, size: Pixels, intrinsic_width: u16) -> AnyElement {
    let source = SizedSvgSource {
        path,
        logical_size_bits: f32::from(size).to_bits(),
        intrinsic_width,
    };
    img(move |window: &mut Window, cx: &mut App| window.get_asset::<SizedSvgAsset>(&source, cx))
        .size(size)
        .flex_none()
        .into_any_element()
}

fn agent_brand_intrinsic_width(path: &str) -> Option<u16> {
    match path {
        "icons/vibex/opencode.svg" | "icons/vibex/agents/antigravity.svg" => Some(16),
        "icons/vibex/gemini.svg" => Some(32),
        "icons/vibex/qwen.svg" | "icons/vibex/claude.svg" => Some(256),
        "icons/vibex/agents/crow-cli.svg" | "icons/vibex/agents/kimi.svg" => Some(100),
        "icons/vibex/agents/amp-acp.svg" => Some(28),
        "icons/vibex/agents/codebuddy-code.svg" => Some(40),
        "icons/vibex/agents/codewhale.svg" | "icons/vibex/agents/qoder.svg" => Some(180),
        "icons/vibex/agents/deepseek-harness.svg" => Some(24),
        "icons/vibex/agents/dimcode.svg" => Some(256),
        "icons/vibex/agents/junie.svg" => Some(128),
        "icons/vibex/agents/mistral-vibe.svg" => Some(512),
        "icons/vibex/agents/kiro.svg" => Some(1200),
        _ => None,
    }
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
        if let Some((_, bytes)) = BUNDLED_GPUI_FONT_ASSETS
            .iter()
            .chain(VIBEX_ASSETS.iter())
            .chain(PROJECT_LOGO_ASSETS.iter())
            .chain(FILE_INTEGRATION_ASSETS.iter())
            .chain(AGENT_BRAND_ASSETS.iter())
            .chain(MODEL_PROVIDER_ASSETS.iter())
            .find(|(asset_path, _)| *asset_path == path)
        {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> GpuiResult<Vec<SharedString>> {
        let mut assets = ComponentAssets.list(path)?;
        assets.extend(
            BUNDLED_GPUI_FONT_ASSETS
                .iter()
                .chain(VIBEX_ASSETS.iter())
                .chain(PROJECT_LOGO_ASSETS.iter())
                .chain(FILE_INTEGRATION_ASSETS.iter())
                .chain(AGENT_BRAND_ASSETS.iter())
                .chain(MODEL_PROVIDER_ASSETS.iter())
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
    let mut fonts = vec![Cow::Borrowed(INTER_LATIN), Cow::Borrowed(INTER_LATIN_EXT)];
    #[cfg(not(target_os = "linux"))]
    fonts.extend(
        [
            IBM_PLEX_SANS_REGULAR,
            IBM_PLEX_SANS_ITALIC,
            IBM_PLEX_SANS_SEMIBOLD,
            IBM_PLEX_SANS_SEMIBOLD_ITALIC,
            LILEX_REGULAR,
            LILEX_ITALIC,
            LILEX_BOLD,
            LILEX_BOLD_ITALIC,
        ]
        .into_iter()
        .map(Cow::Borrowed),
    );
    fonts.push(Cow::Borrowed(WQY_MICROHEI));
    cx.text_system()
        .add_fonts(fonts)
        .map_err(|error| format!("failed to load bundled desktop fonts: {error}"))?;
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
    fn window_icon_is_high_contrast_and_rounded() {
        let icon = window_icon().expect("bundled window icon should decode");

        assert_eq!(icon.dimensions(), (256, 256));
        assert_eq!(*icon.get_pixel(0, 0), image::Rgba([0, 0, 0, 0]));
        assert!(icon.pixels().any(|pixel| pixel.0 == [255, 255, 255, 255]));
    }

    #[test]
    fn multicolor_agent_brands_use_polychrome_image_elements() {
        for identity in [
            "Claude Code",
            "Google Gemini",
            "Google Antigravity",
            "OpenCode",
            "Qwen Code",
            "amp-acp",
            "codebuddy-code",
            "codewhale",
            "deepseek-harness",
            "dimcode",
            "kimi",
            "kiro",
            "qoder",
        ] {
            let mut icon = agent_brand_icon(identity, gpui::px(16.0), None);
            assert!(
                icon.downcast_mut::<gpui::Img>().is_some(),
                "{identity} must bypass GPUI's monochrome Icon renderer"
            );
        }
    }

    #[test]
    fn monochrome_agent_brands_follow_the_active_theme() {
        let assets = VibexAssets;

        for identity in [
            "auggie",
            "cline",
            "cursor",
            "deepagents",
            "devin",
            "dirac",
            "factory-droid",
            "glm-acp-agent",
            "zcode",
            "goose",
            "grok",
            "hermes",
            "kilo",
            "minion-code",
            "nova",
            "pi",
            "poolside",
            "stakpak",
            "vtcode",
        ] {
            let asset = agent_brand_asset(identity).unwrap();
            assert!(asset.uses_current_color, "{identity}");
            let bytes = assets.load(asset.path).unwrap().unwrap();
            assert!(
                std::str::from_utf8(&bytes)
                    .unwrap()
                    .contains("currentColor"),
                "{identity} must expose a theme-paintable SVG mask"
            );

            let mut icon = agent_brand_icon(identity, gpui::px(16.0), None);
            assert!(
                icon.downcast_mut::<gpui::Img>().is_none(),
                "{identity} must use GPUI's theme-aware Icon renderer"
            );
        }
    }

    #[test]
    fn zcode_uses_the_glm_agent_brand_asset() {
        assert_eq!(
            agent_brand_asset("zcode"),
            agent_brand_asset("glm-acp-agent")
        );
        assert_eq!(
            agent_brand_asset("ZCode ACP Server"),
            agent_brand_asset("glm-acp-agent")
        );
    }

    #[test]
    fn agent_brand_logo_is_optional_for_text_fallbacks() {
        assert!(agent_brand_logo("OpenCode", gpui::px(16.0), None).is_some());
        assert!(agent_brand_logo("custom acp", gpui::px(16.0), None).is_none());
    }

    #[test]
    fn every_catalog_agent_has_a_loadable_brand_asset() {
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

            let mut icon = agent_brand_icon(entry.id, gpui::px(16.0), None);
            assert_eq!(
                icon.downcast_mut::<gpui::Img>().is_some(),
                !asset.uses_current_color,
                "{} uses the wrong renderer for its brand asset",
                entry.id
            );
            if !asset.uses_current_color && asset.path.ends_with(".svg") {
                assert!(
                    agent_brand_intrinsic_width(asset.path).is_some(),
                    "{} must rasterize at its requested display size",
                    entry.id
                );
            }
        }
    }

    #[test]
    fn every_builtin_project_logo_asset_is_embedded() {
        let assets = VibexAssets;
        for (path, bytes) in PROJECT_LOGO_ASSETS {
            assert!(assets.load(path).unwrap().is_some(), "{path}");
            assert!(bytes.starts_with(b"<svg"), "{path} should be an SVG");
        }
    }

    #[test]
    fn agent_brand_svgs_rasterize_at_requested_2x_size() {
        let assets = VibexAssets;
        let renderer = gpui::SvgRenderer::new(Arc::new(VibexAssets));

        for identity in [
            "OpenCode",
            "amp-acp",
            "codebuddy-code",
            "codewhale",
            "deepseek-harness",
            "crow-cli",
            "dimcode",
            "junie",
            "kimi",
            "kiro",
            "mistral-vibe",
            "qoder",
        ] {
            let asset = agent_brand_asset(identity).unwrap();
            let intrinsic_width = agent_brand_intrinsic_width(asset.path).unwrap();
            let bytes = assets.load(asset.path).unwrap().unwrap();
            let image = renderer
                .render_single_frame(&bytes, 28.0 / f32::from(intrinsic_width))
                .unwrap();

            assert_eq!(u32::from(image.size(0).width), 56, "{identity}");
            assert_eq!(u32::from(image.size(0).height), 56, "{identity}");
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
    fn openrouter_model_keywords_resolve_provider_marks() {
        let expected = |model: &str, path: &str| {
            assert_eq!(
                model_brand_asset(model).map(|asset| asset.path),
                Some(path),
                "{model}"
            );
        };

        expected("hy4-preview", "icons/vibex/model-providers/tencent.svg");
        expected(
            "tencent/hy4-preview",
            "icons/vibex/model-providers/tencent.svg",
        );
        expected(
            "~TENCENT/HY4-PREVIEW",
            "icons/vibex/model-providers/tencent.svg",
        );
        expected("gpt-5", "icons/vibex/openai.svg");
        expected(
            "microsoft/mai-image-2.5",
            "icons/vibex/model-providers/mai.svg",
        );
        expected("opencode-default", "icons/vibex/opencode.svg");
        expected("codex_model_mini", "icons/vibex/openai.svg");

        // Provider-qualified IDs take precedence over shared model keywords.
        expected(
            "deepgram/flux-tts",
            "icons/vibex/model-providers/deepgram.svg",
        );
        expected(
            "deepgram/nova-3",
            "icons/vibex/model-providers/deepgram.svg",
        );
        expected(
            "nvidia/llama-nemotron-rerank-vl-1b-v2",
            "icons/vibex/model-providers/nvidia.svg",
        );
        expected(
            "cohere/rerank-4-pro",
            "icons/vibex/model-providers/cohere.svg",
        );

        // Unqualified IDs retain the catalog's disambiguation rules.
        expected("flux-tts", "icons/vibex/model-providers/deepgram.svg");
        expected("nova-3", "icons/vibex/model-providers/deepgram.svg");
        expected("rerank-4", "icons/vibex/model-providers/cohere.svg");
        expected("llama-nemotron", "icons/vibex/model-providers/nvidia.svg");
    }

    #[test]
    fn model_keyword_matching_requires_a_token_boundary() {
        assert!(model_keyword_matches("gpt-5", "gpt"));
        assert!(model_keyword_matches("flux.2-pro", "flux.2"));
        assert!(!model_keyword_matches("gptish", "gpt"));
        assert!(!model_keyword_matches("notgpt-5", "gpt"));
        assert_eq!(model_brand_asset("gptish"), None);
        assert_eq!(model_brand_asset("vendor/gptish"), None);
        assert_eq!(
            model_brand_asset("vendor/flux-tts").map(|asset| asset.path),
            Some("icons/vibex/model-providers/deepgram.svg")
        );
    }

    #[test]
    fn every_openrouter_brand_rule_points_to_a_registered_asset() {
        let assets = VibexAssets;
        for (provider, asset) in OPENROUTER_PROVIDER_BRANDS {
            assert!(
                assets.load(asset.path).unwrap().is_some(),
                "provider {provider} points to an unregistered asset {}",
                asset.path
            );
        }
        for rule in OPENROUTER_MODEL_BRANDS {
            assert!(
                assets.load(rule.asset.path).unwrap().is_some(),
                "keyword {} points to an unregistered asset {}",
                rule.keyword,
                rule.asset.path
            );
        }
    }

    #[test]
    fn model_provider_svgs_are_path_based_and_rasterizable() {
        let assets = VibexAssets;
        let renderer = gpui::SvgRenderer::new(Arc::new(VibexAssets));

        assert_eq!(MODEL_PROVIDER_ASSETS.len(), 59);
        for (path, bytes) in MODEL_PROVIDER_ASSETS {
            let source = std::str::from_utf8(bytes).expect("provider SVG should be UTF-8");
            assert!(source.contains("<svg"), "{path} should be an SVG");
            assert!(
                source.contains("<path"),
                "{path} should contain vector paths"
            );
            assert!(
                !source.contains("<image"),
                "{path} must not embed a raster image"
            );
            assert!(
                !source.contains("data:image"),
                "{path} must not contain a data URI"
            );
            assert!(assets.load(path).unwrap().is_some(), "{path}");

            let image = renderer
                .render_single_frame(bytes, 1.0)
                .unwrap_or_else(|error| panic!("{path} failed to rasterize: {error}"));
            assert!(u32::from(image.size(0).width) > 0, "{path} has no width");
            assert!(u32::from(image.size(0).height) > 0, "{path} has no height");
        }
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
