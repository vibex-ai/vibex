// The model and provider marks the desktop's pickers draw.
//
// Migrated from the desktop's own asset tables so a model wears one logo in
// every client. A client that renders SVG itself reads the paths here and
// draws the file from its own icon pack; the table is the shared truth about
// which model belongs to which brand.
//
// This file is `include!`d, so it carries no inner doc comments and no `mod`.

const fn colored_model_asset(path: &'static str) -> Mark {
    Mark {
        path,
        uses_current_color: false,
    }
}

const fn themed_model_asset(path: &'static str) -> Mark {
    Mark {
        path,
        uses_current_color: true,
    }
}

/// One model-name keyword and the mark it picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelBrandRule {
    pub keyword: &'static str,
    pub asset: Mark,
}

pub const OPENROUTER_PROVIDER_BRANDS: &[(&str, Mark)] = &[
    (
        "aion-labs",
        colored_model_asset("model-providers/aion-labs.svg"),
    ),
    (
        "alibaba",
        colored_model_asset("model-providers/alibaba.svg"),
    ),
    (
        "amazon",
        colored_model_asset("model-providers/amazon.svg"),
    ),
    (
        "anthracite-org",
        colored_model_asset("model-providers/anthracite-org.svg"),
    ),
    (
        "anthropic",
        colored_model_asset("model-providers/anthropic.svg"),
    ),
    (
        "arcee-ai",
        colored_model_asset("model-providers/arcee-ai.svg"),
    ),
    (
        "baai",
        themed_model_asset("model-providers/baai.svg"),
    ),
    (
        "baidu",
        colored_model_asset("model-providers/baidu.svg"),
    ),
    (
        "black-forest-labs",
        colored_model_asset("model-providers/black-forest-labs.svg"),
    ),
    (
        "bytedance",
        colored_model_asset("model-providers/bytedance.svg"),
    ),
    (
        "bytedance-seed",
        colored_model_asset("model-providers/bytedance-seed.svg"),
    ),
    (
        "canopylabs",
        colored_model_asset("model-providers/canopylabs.svg"),
    ),
    (
        "cognitivecomputations",
        colored_model_asset("model-providers/cognitivecomputations.svg"),
    ),
    (
        "cohere",
        colored_model_asset("model-providers/cohere.svg"),
    ),
    (
        "deepgram",
        themed_model_asset("model-providers/deepgram.svg"),
    ),
    (
        "deepseek",
        colored_model_asset("model-providers/deepseek.svg"),
    ),
    (
        "deepseek-ai",
        colored_model_asset("model-providers/deepseek.svg"),
    ),
    (
        "dots-studio",
        colored_model_asset("model-providers/dots-studio.svg"),
    ),
    (
        "fish-audio",
        colored_model_asset("model-providers/fish-audio.svg"),
    ),
    (
        "google",
        colored_model_asset("model-providers/google.svg"),
    ),
    (
        "gryphe",
        colored_model_asset("model-providers/gryphe.svg"),
    ),
    (
        "hexgrad",
        colored_model_asset("model-providers/hexgrad.svg"),
    ),
    (
        "heygen",
        colored_model_asset("model-providers/heygen.svg"),
    ),
    (
        "ibm-granite",
        themed_model_asset("model-providers/ibm-granite.svg"),
    ),
    (
        "inception",
        themed_model_asset("model-providers/inception.svg"),
    ),
    (
        "inclusionai",
        colored_model_asset("model-providers/inclusionai.svg"),
    ),
    (
        "intfloat",
        colored_model_asset("model-providers/intfloat.svg"),
    ),
    (
        "krea",
        colored_model_asset("model-providers/krea.svg"),
    ),
    (
        "kwaipilot",
        colored_model_asset("model-providers/kwaipilot.svg"),
    ),
    (
        "kwaivgi",
        colored_model_asset("model-providers/kwaivgi.svg"),
    ),
    (
        "liquid",
        themed_model_asset("model-providers/liquid.svg"),
    ),
    (
        "mancer",
        colored_model_asset("model-providers/mancer.svg"),
    ),
    (
        "meituan",
        colored_model_asset("model-providers/meituan.svg"),
    ),
    (
        "meta",
        colored_model_asset("model-providers/meta.svg"),
    ),
    (
        "meta-llama",
        colored_model_asset("model-providers/meta-llama.svg"),
    ),
    (
        "microsoft",
        colored_model_asset("model-providers/microsoft.svg"),
    ),
    (
        "minimax",
        colored_model_asset("model-providers/minimax.svg"),
    ),
    (
        "mistralai",
        colored_model_asset("model-providers/mistral.svg"),
    ),
    (
        "moonshotai",
        themed_model_asset("model-providers/kimi.svg"),
    ),
    (
        "morph",
        colored_model_asset("model-providers/morph.svg"),
    ),
    (
        "nex-agi",
        themed_model_asset("model-providers/nex-agi.svg"),
    ),
    (
        "nousresearch",
        themed_model_asset("icons/vibex/agents/hermes.svg"),
    ),
    (
        "nvidia",
        colored_model_asset("model-providers/nvidia.svg"),
    ),
    (
        "openai",
        themed_model_asset("model-providers/openai.svg"),
    ),
    (
        "openrouter",
        colored_model_asset("model-providers/openrouter.svg"),
    ),
    (
        "perceptron",
        colored_model_asset("model-providers/perceptron.svg"),
    ),
    (
        "perplexity",
        colored_model_asset("model-providers/perplexity.svg"),
    ),
    (
        "poolside",
        themed_model_asset("icons/vibex/agents/poolside.svg"),
    ),
    (
        "qwen",
        colored_model_asset("model-providers/qwen.svg"),
    ),
    (
        "recraft",
        themed_model_asset("model-providers/recraft.svg"),
    ),
    (
        "rekaai",
        colored_model_asset("model-providers/rekaai.svg"),
    ),
    (
        "relace",
        colored_model_asset("model-providers/relace.svg"),
    ),
    (
        "runway",
        colored_model_asset("model-providers/runway.svg"),
    ),
    (
        "sakana",
        colored_model_asset("model-providers/sakana.svg"),
    ),
    (
        "sao10k",
        colored_model_asset("model-providers/sao10k.svg"),
    ),
    (
        "sentence-transformers",
        colored_model_asset("model-providers/sentence-transformers.svg"),
    ),
    (
        "sesame",
        colored_model_asset("model-providers/sesame.svg"),
    ),
    (
        "sourceful",
        colored_model_asset("model-providers/sourceful.svg"),
    ),
    (
        "stepfun",
        colored_model_asset("model-providers/stepfun.svg"),
    ),
    (
        "tencent",
        colored_model_asset("model-providers/tencent.svg"),
    ),
    (
        "thedrummer",
        colored_model_asset("model-providers/thedrummer.svg"),
    ),
    (
        "thenlper",
        colored_model_asset("model-providers/thenlper.svg"),
    ),
    (
        "thinkingmachines",
        colored_model_asset("model-providers/thinkingmachines.svg"),
    ),
    (
        "undi95",
        colored_model_asset("model-providers/undi95.svg"),
    ),
    (
        "upstage",
        colored_model_asset("model-providers/upstage.svg"),
    ),
    (
        "voyageai",
        colored_model_asset("model-providers/voyageai.svg"),
    ),
    (
        "writer",
        themed_model_asset("model-providers/writer.svg"),
    ),
    (
        "x-ai",
        themed_model_asset("model-providers/xai.svg"),
    ),
    (
        "xiaomi",
        colored_model_asset("model-providers/xiaomi.svg"),
    ),
    (
        "z-ai",
        themed_model_asset("model-providers/zhipu.svg"),
    ),
    (
        "groq",
        themed_model_asset("model-providers/groq.svg"),
    ),
    (
        "nebius",
        themed_model_asset("model-providers/nebius.svg"),
    ),
    (
        "sambanova",
        themed_model_asset("model-providers/sambanova.svg"),
    ),
    (
        "novita",
        colored_model_asset("model-providers/novita.svg"),
    ),
    (
        "deepinfra",
        colored_model_asset("model-providers/deepinfra.svg"),
    ),
    (
        "hyperbolic",
        colored_model_asset("model-providers/hyperbolic.svg"),
    ),
    (
        "cerebras",
        colored_model_asset("model-providers/cerebras.svg"),
    ),
    (
        "ai21",
        colored_model_asset("model-providers/ai21.svg"),
    ),
    (
        "baichuan",
        themed_model_asset("model-providers/baichuan.svg"),
    ),
    (
        "yi",
        colored_model_asset("model-providers/yi.svg"),
    ),
    (
        "iflytek",
        colored_model_asset("model-providers/iflytek.svg"),
    ),
    (
        "modelscope",
        colored_model_asset("model-providers/modelscope.svg"),
    ),
    (
        "volcengine",
        colored_model_asset("model-providers/volcengine.svg"),
    ),
    (
        "aihubmix",
        colored_model_asset("model-providers/aihubmix.svg"),
    ),
    (
        "vercel",
        themed_model_asset("model-providers/vercel.svg"),
    ),
    (
        "moonshot-ai",
        themed_model_asset("model-providers/kimi.svg"),
    ),
    (
        "moonshotai-cn",
        themed_model_asset("model-providers/kimi.svg"),
    ),
    (
        "z-ai",
        themed_model_asset("model-providers/zhipu.svg"),
    ),
    (
        "zai",
        themed_model_asset("model-providers/zhipu.svg"),
    ),
    (
        "zhipuai",
        themed_model_asset("model-providers/zhipu.svg"),
    ),
    (
        "xai",
        themed_model_asset("model-providers/xai.svg"),
    ),
    (
        "x-ai",
        themed_model_asset("model-providers/xai.svg"),
    ),
    (
        "mistral",
        colored_model_asset("model-providers/mistral.svg"),
    ),
    (
        "mistralai",
        colored_model_asset("model-providers/mistral.svg"),
    ),
    (
        "moonshotai-cn",
        themed_model_asset("model-providers/kimi.svg"),
    ),
    (
        "kimi-code-plan-cn",
        themed_model_asset("model-providers/kimi.svg"),
    ),
    (
        "kimi-code-plan-global",
        themed_model_asset("model-providers/kimi.svg"),
    ),
    (
        "zhipuai-coding-plan",
        themed_model_asset("model-providers/zhipu.svg"),
    ),
    (
        "minimax-cn",
        colored_model_asset("model-providers/minimax.svg"),
    ),
    (
        "minimax-coding-plan",
        colored_model_asset("model-providers/minimax.svg"),
    ),
    (
        "minimax-cn-coding-plan",
        colored_model_asset("model-providers/minimax.svg"),
    ),
    (
        "stepfun-ai",
        colored_model_asset("model-providers/stepfun.svg"),
    ),
    (
        "stepfun-ai-step-plan",
        colored_model_asset("model-providers/stepfun.svg"),
    ),
    (
        "stepfun-step-plan",
        colored_model_asset("model-providers/stepfun.svg"),
    ),
    (
        "xiaomi-token-plan-cn",
        colored_model_asset("model-providers/xiaomi.svg"),
    ),
    (
        "xiaomi-token-plan-ams",
        colored_model_asset("model-providers/xiaomi.svg"),
    ),
    (
        "xiaomi-token-plan-sgp",
        colored_model_asset("model-providers/xiaomi.svg"),
    ),
    (
        "tencent-tokenhub",
        colored_model_asset("model-providers/tencent.svg"),
    ),
    (
        "tencent-token-plan",
        colored_model_asset("model-providers/tencent.svg"),
    ),
    (
        "tencent-coding-plan",
        colored_model_asset("model-providers/tencent.svg"),
    ),
    (
        "volcengine-coding-plan",
        colored_model_asset("model-providers/bytedance.svg"),
    ),
    (
        "alibaba-coding-plan-cn",
        colored_model_asset("model-providers/alibaba.svg"),
    ),
    (
        "siliconflow-cn",
        colored_model_asset("model-providers/siliconflow.svg"),
    ),
    (
        "siliconflow",
        colored_model_asset("model-providers/siliconflow.svg"),
    ),
    (
        "togetherai",
        colored_model_asset("model-providers/together.svg"),
    ),
    (
        "fireworks-ai",
        colored_model_asset("model-providers/fireworks.svg"),
    ),
    (
        "github",
        themed_model_asset("model-providers/github.svg"),
    ),
    (
        "opencode",
        themed_model_asset("model-providers/opencode.svg"),
    ),
    (
        "opencode-go",
        themed_model_asset("model-providers/opencode.svg"),
    ),
    (
        "opencode-zen",
        themed_model_asset("model-providers/opencode.svg"),
    ),
    (
        "clinepass",
        themed_model_asset("model-providers/cline.svg"),
    ),
    (
        "commandcode",
        themed_model_asset("model-providers/commandcode.svg"),
    ),
    (
        "novita-ai",
        colored_model_asset("model-providers/novita.svg"),
    ),
    (
        "reka",
        colored_model_asset("model-providers/rekaai.svg"),
    ),
    (
        "anyrouter",
        themed_model_asset("model-providers/anyrouter.svg"),
    ),
    (
        "opendesign",
        themed_model_asset("model-providers/opendesign.svg"),
    ),
    (
        "open-design",
        themed_model_asset("model-providers/opendesign.svg"),
    ),
    (
        "ollama",
        themed_model_asset("model-providers/ollama.svg"),
    ),
    (
        "lmstudio",
        themed_model_asset("model-providers/lmstudio.svg"),
    ),
    (
        "lm-studio",
        themed_model_asset("model-providers/lmstudio.svg"),
    ),
    (
        "magpie",
        themed_model_asset("model-providers/magpie.svg"),
    ),
];

pub const OPENROUTER_MODEL_BRAND_PREFIXES: &[(&str, Mark)] = &[
    (
        "microsoft/mai",
        colored_model_asset("model-providers/mai.svg"),
    ),
    (
        "deepgram/flux",
        themed_model_asset("model-providers/deepgram.svg"),
    ),
    (
        "deepgram/nova",
        themed_model_asset("model-providers/deepgram.svg"),
    ),
    (
        "cohere/rerank",
        colored_model_asset("model-providers/cohere.svg"),
    ),
    (
        "voyageai/rerank",
        colored_model_asset("model-providers/voyageai.svg"),
    ),
    (
        "nvidia/llama",
        colored_model_asset("model-providers/nvidia.svg"),
    ),
    (
        "meta-llama/llama",
        colored_model_asset("model-providers/meta-llama.svg"),
    ),
    (
        "flux-tts",
        themed_model_asset("model-providers/deepgram.svg"),
    ),
    (
        "nova-3",
        themed_model_asset("model-providers/deepgram.svg"),
    ),
    (
        "rerank-4",
        colored_model_asset("model-providers/cohere.svg"),
    ),
    (
        "rerank-v3.5",
        colored_model_asset("model-providers/cohere.svg"),
    ),
    (
        "llama-nemotron",
        colored_model_asset("model-providers/nvidia.svg"),
    ),
];

pub const OPENROUTER_MODEL_BRANDS: &[ModelBrandRule] = &[
    ModelBrandRule {
        keyword: "mai",
        asset: colored_model_asset("model-providers/mai.svg"),
    },
    ModelBrandRule {
        keyword: "multilingual",
        asset: colored_model_asset("model-providers/intfloat.svg"),
    },
    ModelBrandRule {
        keyword: "bodybuilder",
        asset: colored_model_asset("model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "happyhorse",
        asset: colored_model_asset("model-providers/alibaba.svg"),
    },
    ModelBrandRule {
        keyword: "paraphrase",
        asset: colored_model_asset("model-providers/sentence-transformers.svg"),
    },
    ModelBrandRule {
        keyword: "perceptron",
        asset: colored_model_asset("model-providers/perceptron.svg"),
    },
    ModelBrandRule {
        keyword: "transcribe",
        asset: colored_model_asset("model-providers/fish-audio.svg"),
    },
    ModelBrandRule {
        keyword: "unslopnemo",
        asset: colored_model_asset("model-providers/thedrummer.svg"),
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
        asset: colored_model_asset("model-providers/sourceful.svg"),
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
        asset: colored_model_asset("model-providers/gryphe.svg"),
    },
    ModelBrandRule {
        keyword: "nemotron",
        asset: colored_model_asset("model-providers/nvidia.svg"),
    },
    ModelBrandRule {
        keyword: "parakeet",
        asset: colored_model_asset("model-providers/nvidia.svg"),
    },
    ModelBrandRule {
        keyword: "seedance",
        asset: colored_model_asset("model-providers/bytedance.svg"),
    },
    ModelBrandRule {
        keyword: "seedream",
        asset: colored_model_asset("model-providers/bytedance-seed.svg"),
    },
    ModelBrandRule {
        keyword: "wizardlm",
        asset: colored_model_asset("model-providers/microsoft.svg"),
    },
    ModelBrandRule {
        keyword: "command",
        asset: colored_model_asset("model-providers/cohere.svg"),
    },
    ModelBrandRule {
        keyword: "cydonia",
        asset: colored_model_asset("model-providers/thedrummer.svg"),
    },
    ModelBrandRule {
        keyword: "dolphin",
        asset: colored_model_asset("model-providers/cognitivecomputations.svg"),
    },
    ModelBrandRule {
        keyword: "granite",
        asset: themed_model_asset("model-providers/ibm-granite.svg"),
    },
    ModelBrandRule {
        keyword: "hunyuan",
        asset: colored_model_asset("model-providers/tencent.svg"),
    },
    ModelBrandRule {
        keyword: "inkling",
        asset: colored_model_asset("model-providers/thinkingmachines.svg"),
    },
    ModelBrandRule {
        keyword: "longcat",
        asset: colored_model_asset("model-providers/meituan.svg"),
    },
    ModelBrandRule {
        keyword: "mercury",
        asset: themed_model_asset("model-providers/inception.svg"),
    },
    ModelBrandRule {
        keyword: "minimax",
        asset: colored_model_asset("model-providers/minimax.svg"),
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
        asset: colored_model_asset("model-providers/canopylabs.svg"),
    },
    ModelBrandRule {
        keyword: "palmyra",
        asset: colored_model_asset("model-providers/writer.svg"),
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
        asset: themed_model_asset("model-providers/recraft.svg"),
    },
    ModelBrandRule {
        keyword: "skyfall",
        asset: colored_model_asset("model-providers/thedrummer.svg"),
    },
    ModelBrandRule {
        keyword: "trinity",
        asset: colored_model_asset("model-providers/arcee-ai.svg"),
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
        asset: colored_model_asset("model-providers/heygen.svg"),
    },
    ModelBrandRule {
        keyword: "claude",
        asset: colored_model_asset("icons/vibex/claude.svg"),
    },
    ModelBrandRule {
        keyword: "flux.2",
        asset: colored_model_asset("model-providers/black-forest-labs.svg"),
    },
    ModelBrandRule {
        keyword: "fusion",
        asset: colored_model_asset("model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "gemini",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "hailuo",
        asset: colored_model_asset("model-providers/minimax.svg"),
    },
    ModelBrandRule {
        keyword: "hermes",
        asset: themed_model_asset("icons/vibex/agents/hermes.svg"),
    },
    ModelBrandRule {
        keyword: "kokoro",
        asset: colored_model_asset("model-providers/hexgrad.svg"),
    },
    ModelBrandRule {
        keyword: "laguna",
        asset: themed_model_asset("icons/vibex/agents/poolside.svg"),
    },
    ModelBrandRule {
        keyword: "magnum",
        asset: colored_model_asset("model-providers/anthracite-org.svg"),
    },
    ModelBrandRule {
        keyword: "pareto",
        asset: colored_model_asset("model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "relace",
        asset: colored_model_asset("model-providers/relace.svg"),
    },
    ModelBrandRule {
        keyword: "rerank",
        asset: colored_model_asset("model-providers/voyageai.svg"),
    },
    ModelBrandRule {
        keyword: "sakana",
        asset: colored_model_asset("model-providers/sakana.svg"),
    },
    ModelBrandRule {
        keyword: "speech",
        asset: colored_model_asset("model-providers/minimax.svg"),
    },
    ModelBrandRule {
        keyword: "voyage",
        asset: colored_model_asset("model-providers/voyageai.svg"),
    },
    ModelBrandRule {
        keyword: "weaver",
        asset: colored_model_asset("model-providers/mancer.svg"),
    },
    ModelBrandRule {
        keyword: "aleph",
        asset: colored_model_asset("model-providers/runway.svg"),
    },
    ModelBrandRule {
        keyword: "chirp",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "ernie",
        asset: colored_model_asset("model-providers/baidu.svg"),
    },
    ModelBrandRule {
        keyword: "gemma",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "kling",
        asset: colored_model_asset("model-providers/kwaivgi.svg"),
    },
    ModelBrandRule {
        keyword: "llama",
        asset: colored_model_asset("model-providers/meta-llama.svg"),
    },
    ModelBrandRule {
        keyword: "lyria",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "morph",
        asset: colored_model_asset("model-providers/morph.svg"),
    },
    ModelBrandRule {
        keyword: "multi",
        asset: colored_model_asset("model-providers/sentence-transformers.svg"),
    },
    ModelBrandRule {
        keyword: "north",
        asset: colored_model_asset("model-providers/cohere.svg"),
    },
    ModelBrandRule {
        keyword: "qwen3",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "solar",
        asset: colored_model_asset("model-providers/upstage.svg"),
    },
    ModelBrandRule {
        keyword: "sonar",
        asset: colored_model_asset("model-providers/perplexity.svg"),
    },
    ModelBrandRule {
        keyword: "aion",
        asset: colored_model_asset("model-providers/aion-labs.svg"),
    },
    ModelBrandRule {
        keyword: "aura",
        asset: themed_model_asset("model-providers/deepgram.svg"),
    },
    ModelBrandRule {
        keyword: "auto",
        asset: colored_model_asset("model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "dots",
        asset: colored_model_asset("model-providers/dots-studio.svg"),
    },
    ModelBrandRule {
        keyword: "flux",
        asset: colored_model_asset("model-providers/black-forest-labs.svg"),
    },
    ModelBrandRule {
        keyword: "free",
        asset: colored_model_asset("model-providers/openrouter.svg"),
    },
    ModelBrandRule {
        keyword: "fugu",
        asset: colored_model_asset("model-providers/sakana.svg"),
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
        asset: colored_model_asset("model-providers/krea.svg"),
    },
    ModelBrandRule {
        keyword: "l3.1",
        asset: colored_model_asset("model-providers/sao10k.svg"),
    },
    ModelBrandRule {
        keyword: "l3.3",
        asset: colored_model_asset("model-providers/sao10k.svg"),
    },
    ModelBrandRule {
        keyword: "ling",
        asset: colored_model_asset("model-providers/inclusionai.svg"),
    },
    ModelBrandRule {
        keyword: "mimo",
        asset: colored_model_asset("model-providers/xiaomi.svg"),
    },
    ModelBrandRule {
        keyword: "muse",
        asset: colored_model_asset("model-providers/meta.svg"),
    },
    ModelBrandRule {
        keyword: "nova",
        asset: colored_model_asset("model-providers/amazon.svg"),
    },
    ModelBrandRule {
        keyword: "pplx",
        asset: colored_model_asset("model-providers/perplexity.svg"),
    },
    ModelBrandRule {
        keyword: "qwen",
        asset: colored_model_asset("icons/vibex/qwen.svg"),
    },
    ModelBrandRule {
        keyword: "reka",
        asset: colored_model_asset("model-providers/rekaai.svg"),
    },
    ModelBrandRule {
        keyword: "remm",
        asset: colored_model_asset("model-providers/undi95.svg"),
    },
    ModelBrandRule {
        keyword: "s2.1",
        asset: colored_model_asset("model-providers/fish-audio.svg"),
    },
    ModelBrandRule {
        keyword: "seed",
        asset: colored_model_asset("model-providers/bytedance-seed.svg"),
    },
    ModelBrandRule {
        keyword: "sora",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "step",
        asset: colored_model_asset("model-providers/stepfun.svg"),
    },
    ModelBrandRule {
        keyword: "text",
        asset: themed_model_asset("icons/vibex/openai.svg"),
    },
    ModelBrandRule {
        keyword: "all",
        asset: colored_model_asset("model-providers/sentence-transformers.svg"),
    },
    ModelBrandRule {
        keyword: "bge",
        asset: colored_model_asset("model-providers/baai.svg"),
    },
    ModelBrandRule {
        keyword: "csm",
        asset: colored_model_asset("model-providers/sesame.svg"),
    },
    ModelBrandRule {
        keyword: "gen",
        asset: colored_model_asset("model-providers/runway.svg"),
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
        asset: colored_model_asset("model-providers/thenlper.svg"),
    },
    ModelBrandRule {
        keyword: "hy3",
        asset: colored_model_asset("model-providers/tencent.svg"),
    },
    ModelBrandRule {
        keyword: "hy4",
        asset: colored_model_asset("model-providers/tencent.svg"),
    },
    ModelBrandRule {
        keyword: "kat",
        asset: colored_model_asset("model-providers/kwaipilot.svg"),
    },
    ModelBrandRule {
        keyword: "lfm",
        asset: colored_model_asset("model-providers/liquid.svg"),
    },
    ModelBrandRule {
        keyword: "nex",
        asset: themed_model_asset("model-providers/nex-agi.svg"),
    },
    ModelBrandRule {
        keyword: "phi",
        asset: colored_model_asset("model-providers/microsoft.svg"),
    },
    ModelBrandRule {
        keyword: "veo",
        asset: colored_model_asset("icons/vibex/gemini.svg"),
    },
    ModelBrandRule {
        keyword: "wan",
        asset: colored_model_asset("model-providers/alibaba.svg"),
    },
    ModelBrandRule {
        keyword: "e5",
        asset: colored_model_asset("model-providers/intfloat.svg"),
    },
    ModelBrandRule {
        keyword: "hy",
        asset: colored_model_asset("model-providers/tencent.svg"),
    },
    ModelBrandRule {
        keyword: "l3",
        asset: colored_model_asset("model-providers/sao10k.svg"),
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
        asset: colored_model_asset("model-providers/fish-audio.svg"),
    },
    ModelBrandRule {
        keyword: "s2",
        asset: colored_model_asset("model-providers/fish-audio.svg"),
    },
    ModelBrandRule {
        keyword: "ui",
        asset: colored_model_asset("model-providers/bytedance.svg"),
    },
];

/// Whether `keyword` names a whole segment of `model`, never a prefix mid-word.
///
/// `xai` must not match inside `xai-something`, nor `openai` inside
/// `openai-compatible`: those name different things, and a prefix match would
/// label them with the wrong brand.
fn keyword_matches(model: &str, keyword: &str) -> bool {
    model.starts_with(keyword)
        && model.as_bytes().get(keyword.len()).is_none_or(|character| {
            matches!(
                character,
                b'-' | b'_' | b'.' | b'/' | b':' | b' ' | b'\t' | b'\n' | b'\r'
            )
        })
}

fn brand_prefix(model: &str) -> Option<Mark> {
    OPENROUTER_MODEL_BRAND_PREFIXES
        .iter()
        .filter(|(prefix, _)| keyword_matches(model, prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, asset)| *asset)
}

/// The mark for a model id, or `None` when no brand owns it.
///
/// The order is load-bearing and is the desktop's: an explicit exception wins
/// over a generic keyword, a provider segment wins over the model name inside
/// it, and the longest matching keyword wins over a shorter one that also
/// matches.
pub fn model_mark(model: &str) -> Option<Mark> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.strip_prefix('~').unwrap_or(&normalized);
    if normalized.is_empty() {
        return None;
    }
    if let Some(mark) = brand_prefix(normalized) {
        return Some(mark);
    }
    let (provider, model_name) = normalized
        .split_once('/')
        .map_or((normalized, normalized), |(provider, name)| {
            (provider, name)
        });
    if let Some((_, asset)) = OPENROUTER_PROVIDER_BRANDS
        .iter()
        .find(|(candidate, _)| *candidate == provider)
    {
        return Some(*asset);
    }
    if let Some(mark) = brand_prefix(model_name) {
        return Some(mark);
    }
    if let Some(rule) = OPENROUTER_MODEL_BRANDS
        .iter()
        .filter(|rule| keyword_matches(model_name, rule.keyword))
        .max_by_key(|rule| rule.keyword.len())
    {
        return Some(rule.asset);
    }
    // The aliases a model can be named by without its brand's own name in it.
    if ["gpt", "o1", "o3", "o4", "o5"]
        .iter()
        .any(|keyword| keyword_matches(model_name, keyword))
    {
        return agent_mark("openai");
    }
    if ["opus", "sonnet", "haiku"]
        .iter()
        .any(|keyword| keyword_matches(model_name, keyword))
    {
        return agent_mark("claude");
    }
    None
}
