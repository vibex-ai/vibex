//! Built-in Provider presets.
//!
//! A preset answers the three questions a user otherwise has to research
//! before adding a Provider: which address to call, which wire protocol that
//! address speaks, and which model ids it offers. The table ships with the
//! binary — it is code, not remotely updated data — because the address and
//! protocol combination decides whether a projection can work at all.
//!
//! Two consequences are part of the contract:
//!
//! - Creating from a preset writes `provider_options["wireApi"]` explicitly.
//!   The legacy editor otherwise falls back to "the first protocol that has a
//!   URL", which silently re-labels an Anthropic preset as Chat Completions
//!   the next time the editor opens.
//! - A preset's model list may age. Users can add a model id by hand; the
//!   preset is never silently rewritten from the network.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ModelProviderHeaderVariable, ProviderModelWireApi};

/// The Provider option that carries a Profile's request headers.
const HEADER_OVERRIDE_OPTION_KEY: &str = "headerOverrideJson";

/// One request header a Provider created from a preset declares up front.
///
/// Most presets declare none. A header belongs here only when the vendor's
/// gateway refuses a request without it, so creating the Provider from the
/// preset is enough to make it work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPresetHeader {
    pub name: String,
    pub value: ProviderPresetHeaderValue,
}

/// What a preset's request header carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ProviderPresetHeaderValue {
    /// A fixed value the vendor documents.
    Literal(String),
    /// A value the runtime fills from the session it serves.
    Variable(ModelProviderHeaderVariable),
}

/// One regional spelling of a Provider's endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPresetRegion {
    pub id: String,
    pub display_name: String,
    /// Endpoints this region replaces, keyed by wire protocol id.
    pub endpoints: BTreeMap<String, String>,
}

/// A note the preset editor shows above the model list.
///
/// Caveats are copy, not switches: they explain a limitation the preset's
/// data cannot express, and no runtime branch keys off them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPresetCaveat {
    /// The vendor's free models only answer a specific first-party client, so
    /// the preset lists paid models only.
    FreeTierNeedsClient,
    /// The plan's models are absent from the vendor's model-list endpoint and
    /// come from the plan description instead.
    ModelListExcludesPlan,
    /// Models are grouped by wire protocol rather than one flat list.
    ProtocolPerModel,
}

impl ProviderPresetCaveat {
    pub const fn id(self) -> &'static str {
        match self {
            Self::FreeTierNeedsClient => "free_tier_needs_client",
            Self::ModelListExcludesPlan => "model_list_excludes_plan",
            Self::ProtocolPerModel => "protocol_per_model",
        }
    }

    /// The message the editor shows for this caveat.
    pub const fn message(self) -> &'static str {
        match self {
            Self::FreeTierNeedsClient => {
                "Free models only work inside the vendor's own client; only paid models are listed here."
            }
            Self::ModelListExcludesPlan => {
                "Plan models are not in the vendor's model list; these come from the plan description."
            }
            Self::ProtocolPerModel => {
                "Claude models use the Anthropic API; other models use the OpenAI APIs."
            }
        }
    }
}

/// A ready-to-use Provider definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPreset {
    pub id: String,
    pub display_name: String,
    /// The matching provider id in the models.dev catalogue, when the vendor is
    /// listed there.
    ///
    /// The two ids differ for some vendors (`google-gemini` is `google` in the
    /// catalogue), so the mapping is explicit rather than derived from `id`.
    /// `None` means the catalogue has nothing to suggest for this Provider.
    pub catalog_id: Option<String>,
    /// Endpoint per wire protocol. One preset may serve several protocols
    /// through different addresses.
    pub endpoints: BTreeMap<String, String>,
    pub website: Option<String>,
    pub keys_url: Option<String>,
    /// A hint for the Key field, such as a required prefix.
    pub key_hint: Option<String>,
    /// The service runs on this machine and needs no credential.
    pub no_key: bool,
    pub regions: Vec<ProviderPresetRegion>,
    /// Only model ids with this prefix belong to the plan.
    ///
    /// A shipped preset never carries a model list of its own: the ids come from
    /// the models.dev catalogue entry `catalog_id` names, so a vendor's new model
    /// reaches the picker on the next catalogue refresh instead of waiting for
    /// a release. This prefix is the one thing the catalogue cannot express,
    /// because a plan and its vendor's metered API share an entry. A
    /// user-defined template is the exception to "no list of its own"; see
    /// [`Self::models`].
    pub only: Option<String>,
    pub endpoint_hint: Option<String>,
    /// Static price description, when the preset is a paid plan rather than a
    /// metered API. This is vendor copy, not a computed amount.
    pub plan_note: Option<String>,
    pub caveats: Vec<ProviderPresetCaveat>,
    /// Request headers a Provider created from this preset declares.
    ///
    /// Declared in preset order so the editor shows the vendor's own ordering;
    /// an empty list leaves the header editor untouched.
    #[serde(default)]
    pub headers: Vec<ProviderPresetHeader>,
    /// The model ids this preset carries of its own.
    ///
    /// A shipped preset leaves this empty: its ids come from the models.dev
    /// entry `catalog_id` names, so a vendor's new model reaches the picker on
    /// the next catalogue refresh. A user-defined template is the exception —
    /// nothing else knows the model list the saved Provider was configured
    /// with, so the template carries it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// The saved Provider a user-defined template was built from.
    ///
    /// `None` for every preset that ships with the binary. The value is what a
    /// removal names: the record survives, so the answer has to be remembered
    /// against the record rather than against the template's own id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_provider_id: Option<String>,
    /// The bundled icon key the saved record carried, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The picker kind the saved record declared: `vendor`, `relay`, or `local`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl ProviderPreset {
    /// Whether this preset is one the user's own saved Providers produced.
    ///
    /// A user-defined template sits under its own heading in the library and is
    /// the only kind a reader can remove. It carries no credential, so creating
    /// from it still asks for a Key.
    pub fn is_user_defined(&self) -> bool {
        self.source_provider_id.is_some()
    }

    /// The address for one wire protocol, with the region already applied.
    pub fn endpoint_for(&self, wire_api: ProviderModelWireApi) -> Option<&str> {
        self.endpoints
            .get(wire_api.wire_protocol_id())
            .map(String::as_str)
    }

    /// The protocol a newly created Provider should default to.
    ///
    /// The order is deliberate: Chat Completions is the widest-compatible
    /// surface, so a preset that offers it should start there, and a preset
    /// that only offers one protocol obviously starts with that one.
    pub fn default_wire_api(&self) -> Option<ProviderModelWireApi> {
        [
            ProviderModelWireApi::OpenaiChatCompletions,
            ProviderModelWireApi::AnthropicMessages,
            ProviderModelWireApi::OpenaiResponses,
            ProviderModelWireApi::GoogleGenerativeAi,
        ]
        .into_iter()
        .find(|wire_api| self.endpoint_for(*wire_api).is_some())
    }

    /// The protocol a new Provider for `agent_id` should start on.
    ///
    /// A preset may publish several addresses, but an Agent can only speak the
    /// interfaces its projection descriptor declares. Starting on the preset's
    /// own first protocol made a Claude Provider open on Chat Completions — an
    /// interface Claude does not implement — and the save then failed with
    /// `agent_model_interface_unsupported` after the user had already filled in
    /// the rest of the form. The preset's preference order is kept among the
    /// protocols the Agent actually supports.
    ///
    /// An Agent with no registered descriptor (a shared record, or one whose
    /// projection is not known here) keeps the preset's own default: nothing
    /// says it cannot speak it.
    pub fn wire_api_for_agent(&self, agent_id: &crate::AgentId) -> Option<ProviderModelWireApi> {
        let supported = crate::AgentProviderProjectionRegistry::builtin()
            .map(|registry| {
                registry
                    .descriptors_for_agent(agent_id)
                    .flat_map(|descriptor| descriptor.model_interfaces.iter())
                    .map(|interface| interface.wire_protocol_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if supported.is_empty() {
            return self.default_wire_api();
        }
        [
            ProviderModelWireApi::OpenaiChatCompletions,
            ProviderModelWireApi::AnthropicMessages,
            ProviderModelWireApi::OpenaiResponses,
            ProviderModelWireApi::GoogleGenerativeAi,
        ]
        .into_iter()
        .find(|wire_api| {
            self.endpoint_for(*wire_api).is_some()
                && supported
                    .iter()
                    .any(|protocol| protocol.as_str() == wire_api.wire_protocol_id())
        })
        .or_else(|| self.default_wire_api())
    }

    /// Applies a region's endpoint overrides.
    pub fn with_region(&self, region_id: Option<&str>) -> Self {
        let Some(region_id) = region_id else {
            return self.clone();
        };
        let Some(region) = self.regions.iter().find(|region| region.id == region_id) else {
            return self.clone();
        };
        let mut preset = self.clone();
        preset.endpoints.extend(
            region
                .endpoints
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        preset
    }

    /// The model ids this preset is allowed to offer, out of the catalogue's
    /// own list.
    ///
    /// `only` is the one filter the catalogue cannot express: a plan and its
    /// vendor's metered API share an entry, so the prefix is what keeps a
    /// Token Plan from offering models its key cannot spend on. Everything
    /// else is the catalogue's answer.
    pub fn offered_models(&self, catalogue: &[String]) -> Vec<String> {
        catalogue
            .iter()
            .filter(|model| {
                self.only
                    .as_deref()
                    .is_none_or(|prefix| model.starts_with(prefix))
            })
            .cloned()
            .collect()
    }

    /// The `headerOverrideJson` value a Provider created from this preset
    /// starts with.
    ///
    /// The shape is the same one the Provider editors read and write: a JSON
    /// object keyed by header name, whose value is a literal string or a
    /// `{"variable": ...}` object. `None` means the preset declares no header,
    /// so the editor leaves the user's own rows alone.
    pub fn header_override(&self) -> Option<String> {
        if self.headers.is_empty() {
            return None;
        }
        let mut object = serde_json::Map::new();
        for header in &self.headers {
            let value = match &header.value {
                ProviderPresetHeaderValue::Literal(value) => {
                    serde_json::Value::String(value.clone())
                }
                ProviderPresetHeaderValue::Variable(variable) => {
                    serde_json::json!({ "variable": variable.id() })
                }
            };
            object.insert(header.name.clone(), value);
        }
        Some(serde_json::Value::Object(object).to_string())
    }
}

impl ProviderPreset {
    /// A draft Provider Profile this preset pre-fills.
    ///
    /// The result is display-shaped: it has no id worth persisting and no
    /// Secret, and it exists so an editor can show the addresses, protocol and
    /// models the reader would otherwise have to research. The protocol is
    /// written into the Provider options because that is the field the
    /// projection reads back; leaving it implicit lets an editor fall back to
    /// "the first protocol that has a URL".
    ///
    /// `models` is the catalogue's list for this preset, already filtered by
    /// [`Self::offered_models`]. A preset does not carry one of its own, so a
    /// caller that has no catalogue passes an empty slice and gets a draft
    /// whose model list the editor fills after the first fetch.
    pub fn draft_profile(
        &self,
        agent_id: &crate::AgentId,
        models: &[String],
    ) -> crate::ProviderProfile {
        let mut profile = crate::ProviderProfile::local_default(crate::ProviderKind::Acp);
        profile.id = crate::ProviderProfileId::parse(format!(
            "provider_preset_{}",
            self.id.replace(['/', ' '], "-")
        ))
        .unwrap_or(profile.id);
        profile.agent_id = agent_id.clone();
        profile.display_name = self.display_name.clone();
        profile.status = crate::ProviderProfileStatus::Enabled;
        // The draft belongs to one Agent, so its protocol has to be one that
        // Agent can speak; the preset's own default is only the fallback.
        let wire_api = self.wire_api_for_agent(agent_id);
        profile.base_url = wire_api
            .and_then(|wire_api| self.endpoint_for(wire_api))
            .map(str::to_string);
        profile.default_model = models.first().cloned();
        profile.configured_models = models
            .iter()
            .map(|model| crate::ProviderConfiguredModel {
                id: model.clone(),
                display_name: None,
                enabled: true,
                wire_api,
                capabilities: crate::ProviderModelCapabilities::default(),
            })
            .collect();
        let mut options = crate::ProviderOptions::empty();
        if let Some(wire_api) = wire_api {
            options.entries.push(crate::ProviderBindingMetadata {
                key: "wireApi".to_string(),
                value: wire_api.wire_protocol_id().to_string(),
            });
        }
        if let Some(catalog_id) = self.catalog_id.as_deref() {
            options.entries.push(crate::ProviderBindingMetadata {
                key: "catalogId".to_string(),
                value: catalog_id.to_string(),
            });
        }
        for (protocol, url) in &self.endpoints {
            if let Some(wire_api) = crate::ProviderModelWireApi::from_wire_protocol_id(protocol) {
                options.entries.push(crate::ProviderBindingMetadata {
                    key: wire_api.protocol_base_url_option_key(),
                    value: url.clone(),
                });
            }
        }
        if let Some(website) = self.website.as_deref() {
            options.entries.push(crate::ProviderBindingMetadata {
                key: "websiteUrl".to_string(),
                value: website.to_string(),
            });
        }
        if let Some(headers) = self.header_override() {
            options.entries.push(crate::ProviderBindingMetadata {
                key: HEADER_OVERRIDE_OPTION_KEY.to_string(),
                value: headers,
            });
        }
        profile.provider_options = options;
        profile
    }
}

/// The first batch of built-in presets.
pub const PROVIDER_PRESET_ANTHROPIC: &str = "anthropic";
pub const PROVIDER_PRESET_OPENAI: &str = "openai";
pub const PROVIDER_PRESET_GOOGLE_GEMINI: &str = "google-gemini";
pub const PROVIDER_PRESET_DEEPSEEK: &str = "deepseek";
pub const PROVIDER_PRESET_MOONSHOT_KIMI: &str = "moonshot-kimi";
pub const PROVIDER_PRESET_ZHIPU_GLM: &str = "zhipu-glm";
pub const PROVIDER_PRESET_OPENROUTER: &str = "openrouter";
pub const PROVIDER_PRESET_XAI: &str = "xai";
pub const PROVIDER_PRESET_MISTRAL: &str = "mistral";
pub const PROVIDER_PRESET_GROQ: &str = "groq";
pub const PROVIDER_PRESET_SILICONFLOW: &str = "siliconflow";
pub const PROVIDER_PRESET_OLLAMA: &str = "ollama";

/// A gateway that fronts every upstream Provider behind one address.
///
/// Magpie listens on loopback by default and can also be deployed on another
/// machine, where it requires a gateway key. It speaks the OpenAI, Anthropic
/// and Gemini wire protocols itself, so one preset covers every Agent. Its
/// model ids are `provider/model` and depend on the Providers configured
/// inside Magpie, so the preset ships no list and the catalogue is fetched
/// from the gateway.
pub const PROVIDER_PRESET_MAGPIE: &str = "magpie";

/// Third-party plans that resell open coding models.
pub const PROVIDER_PRESET_OPENCODE_GO: &str = "opencode-go";
pub const PROVIDER_PRESET_CLINEPASS: &str = "clinepass";
pub const PROVIDER_PRESET_COMMANDCODE: &str = "commandcode";
/// A vendor's second region, or a subscription sold beside its metered API.
///
/// These carry their own id because they answer at a different address with a
/// different key: a plan's key is refused at the pay-as-you-go host, and a
/// China host is refused an international key.
pub const PROVIDER_PRESET_MOONSHOT_KIMI_CN: &str = "moonshot-kimi-cn";
pub const PROVIDER_PRESET_KIMI_CODE_CN: &str = "kimi-code-cn";
pub const PROVIDER_PRESET_ZAI: &str = "zai";
pub const PROVIDER_PRESET_ZHIPU_CODING_PLAN: &str = "zhipu-coding-plan";
pub const PROVIDER_PRESET_MINIMAX_CN: &str = "minimax-cn";
pub const PROVIDER_PRESET_MINIMAX_CODING_PLAN: &str = "minimax-coding-plan";
pub const PROVIDER_PRESET_STEPFUN_GLOBAL: &str = "stepfun-global";
pub const PROVIDER_PRESET_STEPFUN_STEP_PLAN: &str = "stepfun-step-plan";
pub const PROVIDER_PRESET_STEPFUN_STEP_PLAN_CN: &str = "stepfun-step-plan-cn";
pub const PROVIDER_PRESET_MODELSCOPE: &str = "modelscope";
pub const PROVIDER_PRESET_OPENCODE_ZEN: &str = "opencode-zen";
pub const PROVIDER_PRESET_XIAOMI: &str = "xiaomi";
pub const PROVIDER_PRESET_XIAOMI_TOKEN_PLAN: &str = "xiaomi-token-plan";
pub const PROVIDER_PRESET_XIAOMI_TOKEN_PLAN_AMS: &str = "xiaomi-token-plan-ams";
pub const PROVIDER_PRESET_XIAOMI_TOKEN_PLAN_SGP: &str = "xiaomi-token-plan-sgp";
pub const PROVIDER_PRESET_TENCENT_TOKENHUB: &str = "tencent-tokenhub";
pub const PROVIDER_PRESET_TENCENT_TOKEN_PLAN: &str = "tencent-token-plan";
pub const PROVIDER_PRESET_TENCENT_CODING_PLAN: &str = "tencent-coding-plan";
pub const PROVIDER_PRESET_VOLCENGINE_CODING_PLAN: &str = "volcengine-coding-plan";
pub const PROVIDER_PRESET_ALIBABA_CODING_PLAN: &str = "alibaba-coding-plan";
pub const PROVIDER_PRESET_ANYROUTER: &str = "anyrouter";
pub const PROVIDER_PRESET_OPEN_DESIGN: &str = "opendesign";

fn endpoints(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(protocol, url)| ((*protocol).to_string(), (*url).to_string()))
        .collect()
}

fn preset(id: &str, display_name: &str, endpoint_entries: &[(&str, &str)]) -> ProviderPreset {
    ProviderPreset {
        id: id.to_string(),
        display_name: display_name.to_string(),
        catalog_id: None,
        endpoints: endpoints(endpoint_entries),
        website: None,
        keys_url: None,
        key_hint: None,
        no_key: false,
        regions: Vec::new(),
        endpoint_hint: None,
        only: None,
        plan_note: None,
        caveats: Vec::new(),
        headers: Vec::new(),
        models: Vec::new(),
        source_provider_id: None,
        icon: None,
        category: None,
    }
}

/// Builds a minimal preset for tests outside this module.
#[cfg(test)]
pub(crate) fn preset_for_test(
    id: &str,
    display_name: &str,
    endpoint_entries: &[(&str, &str)],
) -> ProviderPreset {
    preset(id, display_name, endpoint_entries)
}

/// The header OpenCode's gateways route a request by.
///
/// The session id is filled by the runtime, not typed by the user, so a preset
/// that points at either OpenCode gateway ships the row already bound.
fn opencode_session_headers() -> Vec<ProviderPresetHeader> {
    vec![ProviderPresetHeader {
        name: "x-opencode-session".to_string(),
        value: ProviderPresetHeaderValue::Variable(ModelProviderHeaderVariable::SessionId),
    }]
}

/// Every preset the build ships, in display order.
pub fn provider_presets() -> Vec<ProviderPreset> {
    use ProviderModelWireApi::{
        AnthropicMessages, GoogleGenerativeAi, OpenaiChatCompletions, OpenaiResponses,
    };
    let chat = OpenaiChatCompletions.wire_protocol_id();
    let responses = OpenaiResponses.wire_protocol_id();
    let messages = AnthropicMessages.wire_protocol_id();
    let gemini = GoogleGenerativeAi.wire_protocol_id();

    let mut presets = vec![
        ProviderPreset {
            catalog_id: Some("anthropic".to_string()),
            website: Some("https://www.anthropic.com".to_string()),
            keys_url: Some("https://console.anthropic.com/settings/keys".to_string()),
            key_hint: Some("starts with sk-ant-".to_string()),
            endpoint_hint: Some("Uses the Anthropic Messages API".to_string()),
            ..preset(
                PROVIDER_PRESET_ANTHROPIC,
                "Anthropic",
                &[(messages, "https://api.anthropic.com")],
            )
        },
        ProviderPreset {
            catalog_id: Some("openai".to_string()),
            website: Some("https://openai.com".to_string()),
            keys_url: Some("https://platform.openai.com/api-keys".to_string()),
            key_hint: Some("starts with sk-".to_string()),
            ..preset(
                PROVIDER_PRESET_OPENAI,
                "OpenAI",
                &[
                    (chat, "https://api.openai.com/v1"),
                    (responses, "https://api.openai.com/v1"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("google".to_string()),
            website: Some("https://ai.google.dev".to_string()),
            keys_url: Some("https://aistudio.google.com/apikey".to_string()),
            ..preset(
                PROVIDER_PRESET_GOOGLE_GEMINI,
                "Google Gemini",
                &[(gemini, "https://generativelanguage.googleapis.com")],
            )
        },
        ProviderPreset {
            catalog_id: Some("deepseek".to_string()),
            website: Some("https://www.deepseek.com".to_string()),
            keys_url: Some("https://platform.deepseek.com/api_keys".to_string()),
            key_hint: Some("starts with sk-".to_string()),
            ..preset(
                PROVIDER_PRESET_DEEPSEEK,
                "DeepSeek",
                &[
                    (chat, "https://api.deepseek.com/v1"),
                    (messages, "https://api.deepseek.com/anthropic"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("moonshotai".to_string()),
            website: Some("https://platform.moonshot.cn".to_string()),
            keys_url: Some("https://platform.moonshot.cn/console/api-keys".to_string()),
            ..preset(
                PROVIDER_PRESET_MOONSHOT_KIMI,
                "Moonshot Kimi",
                &[
                    (chat, "https://api.moonshot.cn/v1"),
                    (messages, "https://api.moonshot.cn/anthropic"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("zhipuai".to_string()),
            website: Some("https://open.bigmodel.cn".to_string()),
            keys_url: Some("https://open.bigmodel.cn/usercenter/apikeys".to_string()),
            regions: vec![ProviderPresetRegion {
                id: "global".to_string(),
                display_name: "International".to_string(),
                endpoints: endpoints(&[(chat, "https://api.z.ai/api/paas/v4")]),
            }],
            ..preset(
                PROVIDER_PRESET_ZHIPU_GLM,
                "Zhipu GLM",
                &[(chat, "https://open.bigmodel.cn/api/paas/v4")],
            )
        },
        ProviderPreset {
            catalog_id: Some("openrouter".to_string()),
            website: Some("https://openrouter.ai".to_string()),
            keys_url: Some("https://openrouter.ai/keys".to_string()),
            ..preset(
                PROVIDER_PRESET_OPENROUTER,
                "OpenRouter",
                &[
                    (chat, "https://openrouter.ai/api/v1"),
                    (responses, "https://openrouter.ai/api/v1"),
                    (messages, "https://openrouter.ai/api"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("xai".to_string()),
            website: Some("https://x.ai".to_string()),
            keys_url: Some("https://console.x.ai".to_string()),
            key_hint: Some("starts with xai-".to_string()),
            ..preset(
                PROVIDER_PRESET_XAI,
                "xAI",
                &[(chat, "https://api.x.ai/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("mistral".to_string()),
            website: Some("https://mistral.ai".to_string()),
            keys_url: Some("https://console.mistral.ai/api-keys".to_string()),
            ..preset(
                PROVIDER_PRESET_MISTRAL,
                "Mistral",
                &[(chat, "https://api.mistral.ai/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("groq".to_string()),
            website: Some("https://groq.com".to_string()),
            keys_url: Some("https://console.groq.com/keys".to_string()),
            key_hint: Some("starts with gsk_".to_string()),
            ..preset(
                PROVIDER_PRESET_GROQ,
                "Groq",
                &[(chat, "https://api.groq.com/openai/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("siliconflow".to_string()),
            website: Some("https://cloud.siliconflow.cn".to_string()),
            keys_url: Some("https://cloud.siliconflow.cn/account/ak".to_string()),
            regions: vec![ProviderPresetRegion {
                id: "global".to_string(),
                display_name: "International".to_string(),
                endpoints: endpoints(&[(chat, "https://api.siliconflow.com/v1")]),
            }],
            ..preset(
                PROVIDER_PRESET_SILICONFLOW,
                "SiliconFlow",
                &[(chat, "https://api.siliconflow.cn/v1")],
            )
        },
        ProviderPreset {
            website: Some("https://ollama.com".to_string()),
            no_key: true,
            endpoint_hint: Some("A model server on this machine; no API key is needed".to_string()),
            ..preset(
                PROVIDER_PRESET_OLLAMA,
                "Ollama",
                &[(chat, "http://127.0.0.1:11434/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("cohere".to_string()),
            website: Some("https://cohere.com".to_string()),
            keys_url: Some("https://dashboard.cohere.com/api-keys".to_string()),
            ..preset(
                "cohere",
                "Cohere",
                &[(chat, "https://api.cohere.ai/compatibility/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("togetherai".to_string()),
            website: Some("https://www.together.ai".to_string()),
            keys_url: Some("https://api.together.ai/settings/api-keys".to_string()),
            ..preset(
                "together",
                "Together AI",
                &[(chat, "https://api.together.xyz/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("fireworks-ai".to_string()),
            website: Some("https://fireworks.ai".to_string()),
            keys_url: Some("https://fireworks.ai/account/api-keys".to_string()),
            ..preset(
                "fireworks",
                "Fireworks AI",
                &[(chat, "https://api.fireworks.ai/inference/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("perplexity".to_string()),
            website: Some("https://www.perplexity.ai".to_string()),
            keys_url: Some("https://www.perplexity.ai/settings/api".to_string()),
            ..preset(
                "perplexity",
                "Perplexity",
                &[(chat, "https://api.perplexity.ai")],
            )
        },
        ProviderPreset {
            catalog_id: Some("cerebras".to_string()),
            website: Some("https://www.cerebras.ai".to_string()),
            keys_url: Some("https://cloud.cerebras.ai".to_string()),
            ..preset(
                "cerebras",
                "Cerebras",
                &[(chat, "https://api.cerebras.ai/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("deepinfra".to_string()),
            website: Some("https://deepinfra.com".to_string()),
            keys_url: Some("https://deepinfra.com/dash/api_keys".to_string()),
            ..preset(
                "deepinfra",
                "DeepInfra",
                &[(chat, "https://api.deepinfra.com/v1/openai")],
            )
        },
        ProviderPreset {
            website: Some("https://hyperbolic.xyz".to_string()),
            keys_url: Some("https://app.hyperbolic.xyz/settings".to_string()),
            ..preset(
                "hyperbolic",
                "Hyperbolic",
                &[(chat, "https://api.hyperbolic.xyz/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("nebius".to_string()),
            website: Some("https://nebius.com".to_string()),
            keys_url: Some("https://studio.nebius.com/settings/api-keys".to_string()),
            ..preset(
                "nebius",
                "Nebius",
                &[(chat, "https://api.studio.nebius.com/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("novita-ai".to_string()),
            website: Some("https://novita.ai".to_string()),
            keys_url: Some("https://novita.ai/settings/key-management".to_string()),
            ..preset(
                "novita",
                "Novita AI",
                &[(chat, "https://api.novita.ai/v3/openai")],
            )
        },
        ProviderPreset {
            catalog_id: Some("sambanova".to_string()),
            website: Some("https://sambanova.ai".to_string()),
            keys_url: Some("https://cloud.sambanova.ai/apis".to_string()),
            ..preset(
                "sambanova",
                "SambaNova",
                &[(chat, "https://api.sambanova.ai/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("minimax".to_string()),
            website: Some("https://www.minimax.io".to_string()),
            keys_url: Some(
                "https://platform.minimax.io/user-center/basic-information/interface-key"
                    .to_string(),
            ),
            ..preset(
                "minimax",
                "MiniMax",
                &[(chat, "https://api.minimax.io/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("alibaba".to_string()),
            website: Some("https://www.alibabacloud.com/product/modelstudio".to_string()),
            keys_url: Some("https://bailian.console.alibabacloud.com".to_string()),
            ..preset(
                "qwen",
                "Qwen (DashScope)",
                &[(chat, "https://dashscope.aliyuncs.com/compatible-mode/v1")],
            )
        },
        ProviderPreset {
            website: Some("https://cloud.baidu.com/product/wenxinworkshop".to_string()),
            keys_url: Some("https://console.bce.baidu.com/iam/#/iam/apikey/list".to_string()),
            ..preset(
                "qianfan",
                "Baidu Qianfan",
                &[(chat, "https://qianfan.baidubce.com/v2")],
            )
        },
        ProviderPreset {
            catalog_id: Some("volcengine".to_string()),
            website: Some("https://www.volcengine.com/product/ark".to_string()),
            keys_url: Some("https://console.volcengine.com/ark".to_string()),
            ..preset(
                "doubao",
                "ByteDance Doubao",
                &[(chat, "https://ark.cn-beijing.volces.com/api/v3")],
            )
        },
        ProviderPreset {
            website: Some("https://cloud.tencent.com/product/hunyuan".to_string()),
            keys_url: Some("https://console.cloud.tencent.com/hunyuan".to_string()),
            ..preset(
                "hunyuan",
                "Tencent Hunyuan",
                &[(chat, "https://api.hunyuan.cloud.tencent.com/v1")],
            )
        },
        ProviderPreset {
            website: Some("https://xinghuo.xfyun.cn".to_string()),
            keys_url: Some("https://console.xfyun.cn/services/bmxh".to_string()),
            ..preset(
                "spark",
                "iFlytek Spark",
                &[(chat, "https://spark-api-open.xf-yun.com/v1")],
            )
        },
        ProviderPreset {
            website: Some("https://www.lingyiwanwu.com".to_string()),
            keys_url: Some("https://platform.lingyiwanwu.com/apikeys".to_string()),
            ..preset(
                "yi",
                "01.AI Yi",
                &[(chat, "https://api.lingyiwanwu.com/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("stepfun".to_string()),
            website: Some("https://www.stepfun.com".to_string()),
            keys_url: Some("https://platform.stepfun.com/interface-key".to_string()),
            ..preset(
                "stepfun",
                "StepFun",
                &[(chat, "https://api.stepfun.com/v1")],
            )
        },
        ProviderPreset {
            website: Some("https://www.baichuan-ai.com".to_string()),
            keys_url: Some("https://platform.baichuan-ai.com/console/apikey".to_string()),
            ..preset(
                "baichuan",
                "Baichuan",
                &[(chat, "https://api.baichuan-ai.com/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("upstage".to_string()),
            website: Some("https://upstage.ai".to_string()),
            keys_url: Some("https://console.upstage.ai/api-keys".to_string()),
            ..preset(
                "upstage",
                "Upstage",
                &[(chat, "https://api.upstage.ai/v1")],
            )
        },
        ProviderPreset {
            website: Some("https://www.reka.ai".to_string()),
            keys_url: Some("https://platform.reka.ai".to_string()),
            ..preset(
                "reka",
                "Reka",
                &[(chat, "https://api.reka.ai/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("ai21".to_string()),
            website: Some("https://www.ai21.com".to_string()),
            keys_url: Some("https://studio.ai21.com/account/api-key".to_string()),
            ..preset(
                "ai21",
                "AI21 Labs",
                &[(chat, "https://api.ai21.com/studio/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("nvidia".to_string()),
            website: Some("https://build.nvidia.com".to_string()),
            keys_url: Some("https://build.nvidia.com/settings/api-keys".to_string()),
            ..preset(
                "nvidia",
                "NVIDIA NIM",
                &[(chat, "https://integrate.api.nvidia.com/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("github-copilot".to_string()),
            website: Some("https://github.com/marketplace/models".to_string()),
            keys_url: Some("https://github.com/settings/tokens".to_string()),
            ..preset(
                "github-models",
                "GitHub Models",
                &[(chat, "https://models.inference.ai.azure.com")],
            )
        },
        ProviderPreset {
            catalog_id: Some("aihubmix".to_string()),
            website: Some("https://aihubmix.com".to_string()),
            keys_url: Some("https://aihubmix.com/token".to_string()),
            ..preset(
                "aihubmix",
                "AiHubMix",
                &[(chat, "https://aihubmix.com/v1")],
            )
        },
        ProviderPreset {
            catalog_id: Some("vercel".to_string()),
            website: Some("https://vercel.com/ai-gateway".to_string()),
            keys_url: Some("https://vercel.com/dashboard/ai-gateway".to_string()),
            ..preset(
                "vercel-gateway",
                "Vercel AI Gateway",
                &[(chat, "https://ai-gateway.vercel.sh/v1")],
            )
        },
        ProviderPreset {
            website: Some("https://lmstudio.ai".to_string()),
            no_key: true,
            endpoint_hint: Some("A model server on this machine; no API key is needed".to_string()),
            ..preset(
                "lmstudio",
                "LM Studio",
                &[(chat, "http://127.0.0.1:1234/v1")],
            )
        },
        ProviderPreset {
            website: Some("https://usemagpie.ai".to_string()),
            key_hint: Some(
                "magpie on this machine, or a gateway key for a remote Magpie".to_string(),
            ),
            endpoint_hint: Some(
                "Runs on this machine by default; a remote Magpie needs its address and gateway key. Model ids are provider/model"
                    .to_string(),
            ),
            ..preset(
                PROVIDER_PRESET_MAGPIE,
                "Magpie",
                &[
                    (chat, "http://127.0.0.1:3425/v1"),
                    (responses, "http://127.0.0.1:3425/v1"),
                    (messages, "http://127.0.0.1:3425"),
                    (gemini, "http://127.0.0.1:3425"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("opencode-go".to_string()),
            plan_note: Some("$10/month".to_string()),
            website: Some("https://opencode.ai/docs/go".to_string()),
            keys_url: Some("https://opencode.ai/auth".to_string()),
            headers: opencode_session_headers(),
            ..preset(
                PROVIDER_PRESET_OPENCODE_GO,
                "OpenCode Go",
                &[
                    (chat, "https://opencode.ai/zen/go/v1"),
                    (responses, "https://opencode.ai/zen/go/v1"),
                    (messages, "https://opencode.ai/zen/go"),
                ],
            )
        },
        ProviderPreset {
            plan_note: Some("$9.99/month".to_string()),
            catalog_id: Some("cline-pass".to_string()),
            website: Some("https://docs.cline.bot/getting-started/clinepass".to_string()),
            keys_url: Some("https://app.cline.bot".to_string()),
            // The vendor's own `/models` answers with its metered catalogue
            // only, never the plan's ids. The catalogue entry carries both, so
            // `only` is what keeps a metered id from being mistaken for a
            // covered one.
            only: Some("cline-pass/".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_CLINEPASS,
                "ClinePass",
                &[(chat, "https://api.cline.bot/api/v1")],
            )
        },
        // The second region of a vendor whose two hosts take different keys.
        ProviderPreset {
            catalog_id: Some("moonshotai-cn".to_string()),
            website: Some("https://platform.moonshot.cn".to_string()),
            keys_url: Some("https://platform.moonshot.cn/console/api-keys".to_string()),
            ..preset(
                PROVIDER_PRESET_MOONSHOT_KIMI_CN,
                "Moonshot Kimi (China)",
                &[
                    (chat, "https://api.moonshot.cn/v1"),
                    (messages, "https://api.moonshot.cn/anthropic"),
                ],
            )
        },
        // A membership's own endpoint: it serves the coding models alone and
        // refuses a metered key, so it is its own record rather than a region
        // of the API preset above it.
        ProviderPreset {
            catalog_id: Some("kimi-code-plan-cn".to_string()),
            website: Some("https://www.kimi.com/code".to_string()),
            keys_url: Some("https://www.kimi.com/code/console".to_string()),
            plan_note: Some("Membership".to_string()),
            ..preset(
                PROVIDER_PRESET_KIMI_CODE_CN,
                "Kimi Code (China)",
                &[
                    (chat, "https://api.kimi.com/coding/v1"),
                    (messages, "https://api.kimi.com/coding"),
                ],
            )
        },
        // The same vendor's international host. The catalogue names it apart
        // from the China one because its key and its model line differ.
        ProviderPreset {
            catalog_id: Some("zai".to_string()),
            website: Some("https://z.ai".to_string()),
            keys_url: Some("https://z.ai/manage-apikey/apikey-list".to_string()),
            ..preset(
                PROVIDER_PRESET_ZAI,
                "Z.ai",
                &[
                    (chat, "https://api.z.ai/api/paas/v4"),
                    (messages, "https://api.z.ai/api/anthropic"),
                ],
            )
        },
        // A subscription served at its own path: its key is refused at the
        // pay-as-you-go address, and the vendor's model list answers with the
        // metered models only, so the plan's ids are written here.
        ProviderPreset {
            catalog_id: Some("zhipuai-coding-plan".to_string()),
            website: Some("https://docs.bigmodel.cn/cn/coding-plan/overview".to_string()),
            keys_url: Some("https://open.bigmodel.cn/usercenter/proj-mgmt/apikeys".to_string()),
            plan_note: Some("Coding Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_ZHIPU_CODING_PLAN,
                "Zhipu GLM Coding Plan",
                &[
                    (chat, "https://open.bigmodel.cn/api/coding/paas/v4"),
                    (responses, "https://open.bigmodel.cn/api/v1"),
                    (messages, "https://open.bigmodel.cn/api/anthropic"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("minimax-cn".to_string()),
            website: Some("https://platform.minimaxi.com".to_string()),
            keys_url: Some(
                "https://platform.minimaxi.com/user-center/basic-information/interface-key"
                    .to_string(),
            ),
            ..preset(
                PROVIDER_PRESET_MINIMAX_CN,
                "MiniMax (China)",
                &[
                    (chat, "https://api.minimaxi.com/v1"),
                    (messages, "https://api.minimaxi.com/anthropic"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("minimax-coding-plan".to_string()),
            website: Some("https://platform.minimax.io/docs/token-plan/intro".to_string()),
            keys_url: Some(
                "https://platform.minimax.io/user-center/basic-information/interface-key"
                    .to_string(),
            ),
            plan_note: Some("Token Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_MINIMAX_CODING_PLAN,
                "MiniMax Token Plan",
                &[
                    (chat, "https://api.minimax.io/v1"),
                    (messages, "https://api.minimax.io/anthropic"),
                ],
            )
        },
        // StepFun's two hosts and its subscription. The plan's key is refused
        // at the pay-as-you-go addresses and the other way round, so the plan
        // is a preset of its own rather than a second address on one record.
        ProviderPreset {
            catalog_id: Some("stepfun-ai".to_string()),
            website: Some("https://platform.stepfun.ai".to_string()),
            keys_url: Some("https://platform.stepfun.ai/interface-key".to_string()),
            ..preset(
                PROVIDER_PRESET_STEPFUN_GLOBAL,
                "StepFun (Global)",
                &[
                    (chat, "https://api.stepfun.ai/v1"),
                    (messages, "https://api.stepfun.ai"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("stepfun-ai-step-plan".to_string()),
            website: Some(
                "https://platform.stepfun.ai/docs/en/step-plan/integrations/reasoning-api"
                    .to_string(),
            ),
            keys_url: Some("https://platform.stepfun.ai/interface-key".to_string()),
            plan_note: Some("Step Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_STEPFUN_STEP_PLAN,
                "StepFun Step Plan (Global)",
                &[
                    (chat, "https://api.stepfun.ai/step_plan/v1"),
                    (messages, "https://api.stepfun.ai/step_plan"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("stepfun-step-plan".to_string()),
            website: Some(
                "https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api"
                    .to_string(),
            ),
            keys_url: Some("https://platform.stepfun.com/interface-key".to_string()),
            plan_note: Some("Step Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_STEPFUN_STEP_PLAN_CN,
                "StepFun Step Plan (China)",
                &[
                    (chat, "https://api.stepfun.com/step_plan/v1"),
                    (messages, "https://api.stepfun.com/step_plan"),
                ],
            )
        },
        // An inference aggregator: one token reaches many vendors' open
        // weights, and its ids keep the vendor as a path prefix.
        ProviderPreset {
            catalog_id: Some("modelscope".to_string()),
            website: Some("https://modelscope.cn".to_string()),
            keys_url: Some("https://modelscope.cn/my/myaccesstoken".to_string()),
            ..preset(
                PROVIDER_PRESET_MODELSCOPE,
                "ModelScope",
                &[
                    (chat, "https://api-inference.modelscope.cn/v1"),
                    (responses, "https://api-inference.modelscope.cn/v1"),
                ],
            )
        },
        // The same relay's open-model console, which serves a keyless tier:
        // its free models answer without one, and a key adds the paid ones.
        ProviderPreset {
            catalog_id: Some("opencode".to_string()),
            website: Some("https://opencode.ai/docs/zen".to_string()),
            keys_url: Some("https://opencode.ai/auth".to_string()),
            key_hint: Some("optional: free models need no key".to_string()),
            headers: opencode_session_headers(),
            ..preset(
                PROVIDER_PRESET_OPENCODE_ZEN,
                "OpenCode Zen",
                &[
                    (chat, "https://opencode.ai/zen/v1"),
                    (responses, "https://opencode.ai/zen/v1"),
                    (messages, "https://opencode.ai/zen"),
                ],
            )
        },
        // Xiaomi sells a metered API and a regional subscription, each on its
        // own host: a plan key is spent at the plan's host alone.
        ProviderPreset {
            catalog_id: Some("xiaomi".to_string()),
            website: Some("https://platform.xiaomimimo.com".to_string()),
            keys_url: Some("https://platform.xiaomimimo.com/#/console/api-keys".to_string()),
            ..preset(
                PROVIDER_PRESET_XIAOMI,
                "Xiaomi MiMo",
                &[
                    (chat, "https://api.xiaomimimo.com/v1"),
                    (responses, "https://api.xiaomimimo.com/v1"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("xiaomi-token-plan-cn".to_string()),
            website: Some("https://platform.xiaomimimo.com".to_string()),
            keys_url: Some("https://platform.xiaomimimo.com/token-plan".to_string()),
            plan_note: Some("Token Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_XIAOMI_TOKEN_PLAN,
                "Xiaomi Token Plan (China)",
                &[
                    (chat, "https://token-plan-cn.xiaomimimo.com/v1"),
                    (responses, "https://token-plan-cn.xiaomimimo.com/v1"),
                    (messages, "https://token-plan-cn.xiaomimimo.com/anthropic"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("xiaomi-token-plan-ams".to_string()),
            website: Some("https://platform.xiaomimimo.com".to_string()),
            keys_url: Some("https://platform.xiaomimimo.com/token-plan".to_string()),
            plan_note: Some("Token Plan".to_string()),
            ..preset(
                PROVIDER_PRESET_XIAOMI_TOKEN_PLAN_AMS,
                "Xiaomi Token Plan (Europe)",
                &[
                    (chat, "https://token-plan-ams.xiaomimimo.com/v1"),
                    (responses, "https://token-plan-ams.xiaomimimo.com/v1"),
                    (messages, "https://token-plan-ams.xiaomimimo.com/anthropic"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("xiaomi-token-plan-sgp".to_string()),
            website: Some("https://platform.xiaomimimo.com".to_string()),
            keys_url: Some("https://platform.xiaomimimo.com/token-plan".to_string()),
            plan_note: Some("Token Plan".to_string()),
            ..preset(
                PROVIDER_PRESET_XIAOMI_TOKEN_PLAN_SGP,
                "Xiaomi Token Plan (Singapore)",
                &[
                    (chat, "https://token-plan-sgp.xiaomimimo.com/v1"),
                    (responses, "https://token-plan-sgp.xiaomimimo.com/v1"),
                    (messages, "https://token-plan-sgp.xiaomimimo.com/anthropic"),
                ],
            )
        },
        // Tencent's three answers. TokenHub is the metered gateway; the two
        // plans are served at paths of their own and refuse a TokenHub key.
        ProviderPreset {
            catalog_id: Some("tencent-tokenhub".to_string()),
            website: Some("https://cloud.tencent.com/document/product/1823/130050".to_string()),
            keys_url: Some("https://console.cloud.tencent.com/tokenhub/apikey".to_string()),
            ..preset(
                PROVIDER_PRESET_TENCENT_TOKENHUB,
                "Tencent TokenHub",
                &[
                    (chat, "https://tokenhub.tencentmaas.com/v1"),
                    (responses, "https://tokenhub.tencentmaas.com/v1"),
                    (messages, "https://tokenhub.tencentmaas.com"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("tencent-token-plan".to_string()),
            website: Some("https://cloud.tencent.com/document/product/1823/130060".to_string()),
            keys_url: Some("https://console.cloud.tencent.com/tokenhub/tokenplan".to_string()),
            plan_note: Some("Token Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_TENCENT_TOKEN_PLAN,
                "Tencent Token Plan",
                &[
                    (chat, "https://api.lkeap.cloud.tencent.com/plan/v3"),
                    (
                        messages,
                        "https://api.lkeap.cloud.tencent.com/plan/anthropic",
                    ),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("tencent-coding-plan".to_string()),
            website: Some("https://cloud.tencent.com/document/product/1772/128947".to_string()),
            keys_url: Some("https://console.cloud.tencent.com/tokenhub/codingplan".to_string()),
            plan_note: Some("Coding Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_TENCENT_CODING_PLAN,
                "Tencent Coding Plan",
                &[
                    (chat, "https://api.lkeap.cloud.tencent.com/coding/v3"),
                    (
                        messages,
                        "https://api.lkeap.cloud.tencent.com/coding/anthropic",
                    ),
                ],
            )
        },
        // The Ark Coding Plan's quota is spent at /api/coding/v3 alone: a key
        // sent to the metered /api/v3 is billed separately and never draws on
        // the plan, so the two must not share a record.
        ProviderPreset {
            catalog_id: Some("volcengine-coding-plan".to_string()),
            website: Some("https://www.volcengine.com/docs/82379/1928261".to_string()),
            keys_url: Some("https://ark.volcengine.com/region:cn-beijing/apikey".to_string()),
            plan_note: Some("Coding Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_VOLCENGINE_CODING_PLAN,
                "Volcengine Ark Coding Plan",
                &[
                    (chat, "https://ark.cn-beijing.volces.com/api/coding/v3"),
                    (responses, "https://ark.cn-beijing.volces.com/api/coding/v3"),
                    (messages, "https://ark.cn-beijing.volces.com/api/coding"),
                ],
            )
        },
        // Alibaba's coding plan: its own host and its own key, which the
        // DashScope endpoint the Qwen preset uses will not accept.
        ProviderPreset {
            catalog_id: Some("alibaba-coding-plan-cn".to_string()),
            website: Some("https://help.aliyun.com/zh/model-studio/coding-plan".to_string()),
            keys_url: Some("https://bailian.console.aliyun.com".to_string()),
            plan_note: Some("Coding Plan".to_string()),
            caveats: vec![ProviderPresetCaveat::ModelListExcludesPlan],
            ..preset(
                PROVIDER_PRESET_ALIBABA_CODING_PLAN,
                "Alibaba Coding Plan (China)",
                &[
                    (chat, "https://coding.dashscope.aliyuncs.com/v1"),
                    (responses, "https://coding.dashscope.aliyuncs.com/v1"),
                    (
                        messages,
                        "https://coding.dashscope.aliyuncs.com/apps/anthropic",
                    ),
                ],
            )
        },
        // A relay: one key answers for several vendors' models, over both the
        // OpenAI and the Anthropic protocols.
        ProviderPreset {
            website: Some("https://anyrouter.top".to_string()),
            keys_url: Some("https://anyrouter.top".to_string()),
            ..preset(
                PROVIDER_PRESET_ANYROUTER,
                "AnyRouter",
                &[
                    (chat, "http://anyrouter.top/v1"),
                    (responses, "http://anyrouter.top/v1"),
                    (messages, "http://anyrouter.top"),
                ],
            )
        },
        ProviderPreset {
            website: Some("https://open-design.ai".to_string()),
            keys_url: Some("https://amr-link.open-design.ai".to_string()),
            ..preset(
                PROVIDER_PRESET_OPEN_DESIGN,
                "OpenDesign",
                &[
                    (chat, "https://amr-link.open-design.ai/v1"),
                    (messages, "https://amr-link.open-design.ai/v1"),
                ],
            )
        },
        ProviderPreset {
            catalog_id: Some("commandcode".to_string()),
            website: Some("https://commandcode.ai/docs/provider".to_string()),
            keys_url: Some("https://commandcode.ai/settings/keys".to_string()),
            // One account, two protocol families: the Claude models answer the
            // Messages API alone, everything else the OpenAI ones. Which
            // address a model calls is decided by the protocol it was added
            // with, so the preset has to carry all three.
            caveats: vec![ProviderPresetCaveat::ProtocolPerModel],
            ..preset(
                PROVIDER_PRESET_COMMANDCODE,
                "Command Code",
                &[
                    (chat, "https://api.commandcode.ai/provider/v1"),
                    (responses, "https://api.commandcode.ai/provider/v1"),
                    (messages, "https://api.commandcode.ai/provider"),
                ],
            )
        },
    ];
    presets.retain(|preset| {
        preset
            .default_wire_api()
            .is_some_and(|api| preset.endpoint_for(api).is_some())
    });
    presets
}

/// Looks one preset up by id.
pub fn provider_preset(id: &str) -> Option<ProviderPreset> {
    provider_presets()
        .into_iter()
        .find(|preset| preset.id == id.trim())
}

/// The id a saved Provider's user-defined template is addressed by.
///
/// The prefix keeps a template apart from the shipped table, so a saved record
/// whose display name matches a vendor cannot shadow that vendor's preset.
pub const USER_PROVIDER_PRESET_PREFIX: &str = "user:";

/// The id of the template a saved Provider is offered as.
pub fn user_provider_preset_id(provider_id: &str) -> String {
    format!("{USER_PROVIDER_PRESET_PREFIX}{}", provider_id.trim())
}

/// Builds the template a saved Provider offers in the preset library.
///
/// This is a projection, not a copy: the template carries the record's name,
/// its addresses, its models and its advisory metadata, and deliberately no
/// credential. A Provider created from it starts with an empty Key field, which
/// is the reminder that the saved record's Key stays with the saved record.
///
/// `None` when the record holds nothing a template could fill: a Provider with
/// no API address would open a form the user still has to complete from
/// scratch, which is what the "Custom" tile is for.
pub fn user_provider_preset(profile: &crate::ModelProviderProfile) -> Option<ProviderPreset> {
    let mut endpoints = BTreeMap::new();
    for endpoint in profile
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.kind == crate::ModelProviderEndpointKind::Api)
    {
        // A protocol-specific endpoint is the one a template can reproduce;
        // the unmarked fallback would compete with it for the same key.
        let Some(protocol) = endpoint.wire_protocol_id.as_deref() else {
            continue;
        };
        if crate::ProviderModelWireApi::from_wire_protocol_id(protocol).is_some() {
            endpoints.insert(protocol.to_string(), endpoint.url.clone());
        }
    }
    if endpoints.is_empty() {
        // A record created from a single address keeps it as the unmarked
        // fallback. That address is still the one to offer, under the protocol
        // the record starts on.
        let wire_api = profile
            .configured_models
            .iter()
            .find_map(|model| {
                model
                    .metadata
                    .iter()
                    .find(|entry| entry.key == "wireApi")
                    .and_then(|entry| {
                        crate::ProviderModelWireApi::from_wire_protocol_id(&entry.value)
                    })
            })
            .or(Some(crate::ProviderModelWireApi::OpenaiChatCompletions))?;
        let url = profile
            .endpoints
            .iter()
            .find(|endpoint| endpoint.kind == crate::ModelProviderEndpointKind::Api)
            .map(|endpoint| endpoint.url.clone())?;
        endpoints.insert(wire_api.wire_protocol_id().to_string(), url);
    }
    let models = profile
        .configured_models
        .iter()
        .filter(|model| model.enabled)
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    let headers = profile
        .headers
        .iter()
        .filter_map(|header| {
            let value = match &header.value {
                crate::ModelProviderHeaderValue::NonSecretLiteral(value) => {
                    ProviderPresetHeaderValue::Literal(value.clone())
                }
                crate::ModelProviderHeaderValue::DynamicVariable(variable) => {
                    ProviderPresetHeaderValue::Variable(*variable)
                }
                // A Secret reference has no value a template may carry: the
                // Secret belongs to the record, and a copy would be a second
                // copy of the credential.
                crate::ModelProviderHeaderValue::SecretReference(_) => return None,
            };
            Some(ProviderPresetHeader {
                name: header.name.clone(),
                value,
            })
        })
        .collect::<Vec<_>>();
    let website = profile.metadata.website_url.clone();
    Some(ProviderPreset {
        id: user_provider_preset_id(profile.id.as_str()),
        display_name: profile.display_name.clone(),
        catalog_id: profile.metadata.catalog_id.clone(),
        endpoints,
        website,
        keys_url: None,
        key_hint: None,
        // A template carries no credential, so the Key field stays visible and
        // empty: the Provider made from it is a new account, not a second name
        // for the saved one.
        no_key: false,
        regions: Vec::new(),
        only: None,
        endpoint_hint: profile
            .primary_api_endpoint()
            .map(|endpoint| endpoint.url.clone()),
        plan_note: None,
        caveats: Vec::new(),
        headers,
        models,
        source_provider_id: Some(profile.id.as_str().to_string()),
        icon: profile.metadata.icon.clone(),
        category: profile.metadata.category.clone(),
    })
}

/// Every user-defined template the saved Providers produce, in display order.
///
/// A template is offered for every visible saved record, whether or not an
/// Agent binds it: the library is where a Provider is looked up, and a record
/// that exists is a starting point even before it is in use. Deleted records
/// are left out by the caller's own list, so this function has no filter of its
/// own.
///
/// A record created *from* a template is left out on purpose. It is a saved
/// Provider in every other sense — Agents bind it, the list draws it, its Key
/// works — but listing it here would make each copy the seed of the next one,
/// so a library of near-identical rows would grow out of a single duplicate.
pub fn user_provider_presets(profiles: &[crate::ModelProviderProfile]) -> Vec<ProviderPreset> {
    let mut presets = profiles
        .iter()
        .filter(|profile| profile.deleted_at_ms.is_none())
        .filter(|profile| profile.metadata.preset_source_provider_id.is_none())
        .filter_map(user_provider_preset)
        .collect::<Vec<_>>();
    presets.sort_by(|left, right| {
        left.display_name
            .to_lowercase()
            .cmp(&right.display_name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    presets
}

/// Looks a user-defined template up in the saved Providers it came from.
pub fn user_provider_preset_for(
    profiles: &[crate::ModelProviderProfile],
    preset_id: &str,
) -> Option<ProviderPreset> {
    let preset_id = preset_id.trim();
    user_provider_presets(profiles)
        .into_iter()
        .find(|preset| preset.id == preset_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_batch_is_complete_and_unique() {
        let presets = provider_presets();
        // A range rather than one number: the table grows as vendors appear and
        // shrinks when one folds. What must hold is that it stays a curated
        // library — neither accidentally empty nor a dump of every catalogue
        // entry — and that every entry below is addressable and unique.
        assert!(
            (60..=120).contains(&presets.len()),
            "the curated picker stays a curated library, got {}",
            presets.len()
        );
        let mut ids = presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), presets.len(), "preset ids must be unique");
        for preset in &presets {
            assert!(
                preset.default_wire_api().is_some(),
                "preset {} has no usable endpoint",
                preset.id
            );
        }
    }

    #[test]
    fn the_curated_picker_covers_the_top_vendors() {
        for id in [
            "anthropic",
            "openai",
            "google-gemini",
            "deepseek",
            "moonshot-kimi",
            "zhipu-glm",
            "openrouter",
            "xai",
            "mistral",
            "groq",
            "siliconflow",
            "cohere",
            "together",
            "fireworks",
            "perplexity",
            "cerebras",
            "deepinfra",
            "hyperbolic",
            "nebius",
            "novita",
            "sambanova",
            "minimax",
            "qwen",
            "qianfan",
            "doubao",
            "hunyuan",
            "spark",
            "yi",
            "stepfun",
            "baichuan",
            "upstage",
            "reka",
            "ai21",
            "nvidia",
            "github-models",
        ] {
            assert!(
                provider_preset(id).is_some(),
                "the curated picker must include {id}"
            );
        }
    }

    /// Every vendor the catalogue lists under a second region or a second
    /// plan ships as its own preset.
    ///
    /// These are not aliases of the first preset: a China host refuses an
    /// international key, and a plan's key is refused at the metered address.
    /// The catalogue therefore names each one, and a preset that pointed all of
    /// them at the first host would fail at the first request rather than at
    /// the picker.
    #[test]
    fn every_secondary_region_and_plan_has_its_own_preset() {
        for (preset_id, catalog_id) in [
            (PROVIDER_PRESET_MOONSHOT_KIMI_CN, "moonshotai-cn"),
            (PROVIDER_PRESET_KIMI_CODE_CN, "kimi-code-plan-cn"),
            (PROVIDER_PRESET_ZAI, "zai"),
            (PROVIDER_PRESET_ZHIPU_CODING_PLAN, "zhipuai-coding-plan"),
            (PROVIDER_PRESET_MINIMAX_CN, "minimax-cn"),
            (PROVIDER_PRESET_MINIMAX_CODING_PLAN, "minimax-coding-plan"),
            (PROVIDER_PRESET_STEPFUN_GLOBAL, "stepfun-ai"),
            (PROVIDER_PRESET_STEPFUN_STEP_PLAN, "stepfun-ai-step-plan"),
            (PROVIDER_PRESET_STEPFUN_STEP_PLAN_CN, "stepfun-step-plan"),
            (PROVIDER_PRESET_MODELSCOPE, "modelscope"),
            (PROVIDER_PRESET_OPENCODE_ZEN, "opencode"),
            (PROVIDER_PRESET_XIAOMI, "xiaomi"),
            (PROVIDER_PRESET_XIAOMI_TOKEN_PLAN, "xiaomi-token-plan-cn"),
            (
                PROVIDER_PRESET_XIAOMI_TOKEN_PLAN_AMS,
                "xiaomi-token-plan-ams",
            ),
            (
                PROVIDER_PRESET_XIAOMI_TOKEN_PLAN_SGP,
                "xiaomi-token-plan-sgp",
            ),
            (PROVIDER_PRESET_TENCENT_TOKENHUB, "tencent-tokenhub"),
            (PROVIDER_PRESET_TENCENT_TOKEN_PLAN, "tencent-token-plan"),
            (PROVIDER_PRESET_TENCENT_CODING_PLAN, "tencent-coding-plan"),
            (
                PROVIDER_PRESET_VOLCENGINE_CODING_PLAN,
                "volcengine-coding-plan",
            ),
            (
                PROVIDER_PRESET_ALIBABA_CODING_PLAN,
                "alibaba-coding-plan-cn",
            ),
        ] {
            let preset = provider_preset(preset_id)
                .unwrap_or_else(|| panic!("{preset_id} must ship as its own preset"));
            assert_eq!(
                preset.catalog_id.as_deref(),
                Some(catalog_id),
                "{preset_id} must point at the catalogue entry it answers for"
            );
            assert!(
                preset.default_wire_api().is_some(),
                "{preset_id} must carry a usable endpoint"
            );
        }
    }

    /// A plan's record says what its vendor's list cannot.
    ///
    /// A plan preset names a catalogue entry and says why its list needs a
    /// caveat, but it does not carry the ids itself.
    ///
    /// The plans are served at their own paths and vendors list the metered
    /// models rather than the covered ones, so the caveat explains what the
    /// catalogue cannot express. The ids still come from the catalogue entry:
    /// writing them here would be the table this change removed.
    #[test]
    fn every_plan_preset_points_at_a_catalogue_entry_and_explains_itself() {
        for preset_id in [
            PROVIDER_PRESET_ZHIPU_CODING_PLAN,
            PROVIDER_PRESET_MINIMAX_CODING_PLAN,
            PROVIDER_PRESET_STEPFUN_STEP_PLAN,
            PROVIDER_PRESET_STEPFUN_STEP_PLAN_CN,
            PROVIDER_PRESET_XIAOMI_TOKEN_PLAN,
            PROVIDER_PRESET_TENCENT_TOKEN_PLAN,
            PROVIDER_PRESET_TENCENT_CODING_PLAN,
            PROVIDER_PRESET_VOLCENGINE_CODING_PLAN,
            PROVIDER_PRESET_ALIBABA_CODING_PLAN,
        ] {
            let preset = provider_preset(preset_id).expect("the plan ships");
            assert!(
                preset.catalog_id.is_some(),
                "{preset_id} must name the catalogue entry its models come from"
            );
            assert!(
                !preset.caveats.is_empty(),
                "{preset_id} must say what its vendor's list leaves out"
            );
        }
    }

    #[test]
    fn catalog_ids_follow_the_catalogue_aliases() {
        for (preset_id, catalog_id) in [
            (PROVIDER_PRESET_ANTHROPIC, "anthropic"),
            (PROVIDER_PRESET_OPENAI, "openai"),
            // The preset id keeps the product name; the catalogue uses the
            // vendor name.
            (PROVIDER_PRESET_GOOGLE_GEMINI, "google"),
            (PROVIDER_PRESET_DEEPSEEK, "deepseek"),
            (PROVIDER_PRESET_MOONSHOT_KIMI, "moonshotai"),
            (PROVIDER_PRESET_ZHIPU_GLM, "zhipuai"),
            (PROVIDER_PRESET_OPENROUTER, "openrouter"),
            (PROVIDER_PRESET_XAI, "xai"),
            (PROVIDER_PRESET_MISTRAL, "mistral"),
            (PROVIDER_PRESET_GROQ, "groq"),
            (PROVIDER_PRESET_SILICONFLOW, "siliconflow"),
            (PROVIDER_PRESET_COMMANDCODE, "commandcode"),
            (PROVIDER_PRESET_OPENCODE_GO, "opencode-go"),
            (PROVIDER_PRESET_CLINEPASS, "cline-pass"),
        ] {
            assert_eq!(
                provider_preset(preset_id).unwrap().catalog_id.as_deref(),
                Some(catalog_id),
                "{preset_id} must point at its models.dev provider"
            );
        }
        // Ollama's own server is local and unlisted, so a suggestion must
        // never be attempted for it.
        for preset_id in [PROVIDER_PRESET_OLLAMA] {
            assert!(
                provider_preset(preset_id).unwrap().catalog_id.is_none(),
                "{preset_id} has no models.dev provider"
            );
        }
    }

    #[test]
    fn catalog_ids_are_unique_and_slug_shaped() {
        let catalog_ids = provider_presets()
            .into_iter()
            .filter_map(|preset| preset.catalog_id)
            .collect::<Vec<_>>();
        assert!(catalog_ids.len() >= 10);
        let mut unique = catalog_ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            catalog_ids.len(),
            "two presets must not share a catalogue id"
        );
        for catalog_id in catalog_ids {
            assert!(!catalog_id.trim().is_empty());
            assert_eq!(
                catalog_id.to_ascii_lowercase(),
                catalog_id,
                "a catalogue id is a lowercase slug"
            );
            assert!(
                !catalog_id.chars().any(char::is_whitespace),
                "a catalogue id must not contain whitespace"
            );
        }
    }

    #[test]
    fn the_common_vendors_ship_with_their_official_endpoints() {
        let xai = provider_preset(PROVIDER_PRESET_XAI).unwrap();
        assert_eq!(xai.display_name, "xAI");
        assert_eq!(
            xai.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.x.ai/v1")
        );
        let mistral = provider_preset(PROVIDER_PRESET_MISTRAL).unwrap();
        assert_eq!(
            mistral.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.mistral.ai/v1")
        );
        let groq = provider_preset(PROVIDER_PRESET_GROQ).unwrap();
        assert_eq!(
            groq.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.groq.com/openai/v1")
        );
        let siliconflow = provider_preset(PROVIDER_PRESET_SILICONFLOW).unwrap();
        assert_eq!(
            siliconflow.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.siliconflow.cn/v1")
        );
        let global = siliconflow.with_region(Some("global"));
        assert_eq!(
            global.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.siliconflow.com/v1")
        );
        // The vendor serves an OpenAI-compatible surface on both regions.
        for preset in [xai, mistral, groq, siliconflow] {
            assert!(!preset.no_key);
            assert!(preset.keys_url.is_some());
            // The model list is not the preset's to carry: a vendor the
            // catalogue knows names its entry, and the ids come from there.
            assert!(preset.catalog_id.is_some());
        }
    }

    #[test]
    fn a_preset_draft_carries_its_catalog_id() {
        let agent_id = crate::AgentId::parse("codex").unwrap();
        let deepseek = provider_preset(PROVIDER_PRESET_DEEPSEEK).unwrap();
        let draft = deepseek.draft_profile(&agent_id, &[]);
        assert!(
            draft
                .provider_options
                .entries
                .iter()
                .any(|entry| { entry.key == "catalogId" && entry.value == "deepseek" })
        );
        let ollama = provider_preset(PROVIDER_PRESET_OLLAMA).unwrap();
        let draft = ollama.draft_profile(&agent_id, &[]);
        assert!(
            !draft
                .provider_options
                .entries
                .iter()
                .any(|entry| entry.key == "catalogId"),
            "a preset without a catalogue id writes no option"
        );
    }

    /// Creating an OpenCode Provider from its preset already carries the
    /// header its gateway routes by, in the shape the editors persist.
    #[test]
    fn opencode_presets_preset_the_session_header() {
        let agent_id = crate::AgentId::parse("opencode").unwrap();
        for preset_id in [PROVIDER_PRESET_OPENCODE_GO, PROVIDER_PRESET_OPENCODE_ZEN] {
            let preset = provider_preset(preset_id).expect("the OpenCode preset ships");
            assert_eq!(
                preset.headers,
                opencode_session_headers(),
                "{preset_id} must declare the session header"
            );

            let raw = preset
                .header_override()
                .unwrap_or_else(|| panic!("{preset_id} must write a header override"));
            let stored: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(stored["x-opencode-session"]["variable"], "session_id");

            // The draft is the profile a preset-driven creation starts from,
            // so the header has to ride in its options rather than only in the
            // preset's own field.
            let draft = preset.draft_profile(&agent_id, &[]);
            let option = draft
                .provider_options
                .entries
                .iter()
                .find(|entry| entry.key == "headerOverrideJson")
                .unwrap_or_else(|| panic!("{preset_id} draft carries no header option"));
            let stored: serde_json::Value = serde_json::from_str(&option.value).unwrap();
            assert_eq!(stored["x-opencode-session"]["variable"], "session_id");
        }

        // Every other preset stays out of the user's header rows.
        let deepseek = provider_preset(PROVIDER_PRESET_DEEPSEEK).unwrap();
        assert!(deepseek.headers.is_empty());
        assert!(deepseek.header_override().is_none());
        assert!(
            !deepseek
                .draft_profile(&agent_id, &[])
                .provider_options
                .entries
                .iter()
                .any(|entry| entry.key == "headerOverrideJson")
        );
    }

    #[test]
    fn deepseek_offers_both_its_apis_and_starts_on_chat() {
        let preset = provider_preset(PROVIDER_PRESET_DEEPSEEK).unwrap();
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.deepseek.com/v1")
        );
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::AnthropicMessages),
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(
            preset.default_wire_api(),
            Some(ProviderModelWireApi::OpenaiChatCompletions)
        );
    }

    /// The protocol a preset starts on follows the Agent it is created for.
    ///
    /// OpenCode Go publishes Chat, Responses and Messages, and its own order
    /// starts on Chat. Claude speaks only Messages, so a draft built from the
    /// preset for Claude has to start there; otherwise the saved record pins a
    /// protocol the Agent cannot use and the save fails with
    /// `agent_model_interface_unsupported`.
    #[test]
    fn a_preset_starts_on_a_protocol_the_agent_speaks() {
        let opencode_go = provider_preset(PROVIDER_PRESET_OPENCODE_GO).unwrap();
        assert_eq!(
            opencode_go.default_wire_api(),
            Some(ProviderModelWireApi::OpenaiChatCompletions)
        );

        let claude = crate::AgentId::parse("claude").unwrap();
        assert_eq!(
            opencode_go.wire_api_for_agent(&claude),
            Some(ProviderModelWireApi::AnthropicMessages)
        );
        let draft = opencode_go.draft_profile(&claude, &[]);
        assert_eq!(
            draft.base_url.as_deref(),
            opencode_go.endpoint_for(ProviderModelWireApi::AnthropicMessages)
        );
        assert!(draft.provider_options.entries.iter().any(|entry| {
            entry.key == "wireApi"
                && entry.value == ProviderModelWireApi::AnthropicMessages.wire_protocol_id()
        }));

        // Every Model the draft starts with is stamped with that same
        // protocol, so the saved record does not disagree with its option.
        let deepseek = provider_preset(PROVIDER_PRESET_DEEPSEEK).unwrap();
        let catalogue = vec!["deepseek-v4-pro".to_string(), "deepseek-flash".to_string()];
        let draft = deepseek.draft_profile(&claude, &catalogue);
        assert_eq!(draft.default_model.as_deref(), Some("deepseek-v4-pro"));
        assert!(!draft.configured_models.is_empty());
        assert!(
            draft
                .configured_models
                .iter()
                .all(|model| model.wire_api == Some(ProviderModelWireApi::AnthropicMessages))
        );

        // An Agent that speaks Chat keeps the preset's own default.
        let harness = crate::AgentId::parse("deepseek-harness").unwrap();
        assert_eq!(
            opencode_go.wire_api_for_agent(&harness),
            Some(ProviderModelWireApi::OpenaiChatCompletions)
        );

        // A preset whose only address the Agent cannot speak still keeps a
        // protocol instead of dropping it; the binding check reports it later.
        let chat_only = preset(
            "chat-only",
            "Chat only",
            &[(
                crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                "https://chat.example.invalid",
            )],
        );
        assert_eq!(
            chat_only.wire_api_for_agent(&claude),
            Some(ProviderModelWireApi::OpenaiChatCompletions)
        );
    }

    #[test]
    fn local_presets_need_no_key() {
        let preset = provider_preset(PROVIDER_PRESET_OLLAMA).unwrap();
        assert!(preset.no_key);
        assert!(preset.keys_url.is_none());
    }

    /// Magpie is one address in front of every upstream, so its preset has to
    /// cover each wire protocol an Agent may speak, and it has to ask for a Key:
    /// the default is a tokenless local gateway, but a Magpie on another
    /// machine authenticates with a gateway key.
    ///
    /// Its model ids are `provider/model` and depend on the Providers the user
    /// configured inside Magpie, so the preset names no catalogue entry and the
    /// list is fetched from the gateway once the Provider exists.
    #[test]
    fn magpie_is_a_gateway_for_every_wire_protocol_and_takes_a_key() {
        let preset = provider_preset(PROVIDER_PRESET_MAGPIE).expect("the preset ships");
        assert!(!preset.no_key);
        assert!(preset.keys_url.is_none());
        assert!(preset.key_hint.is_some());
        assert!(
            preset.catalog_id.is_none(),
            "Magpie's models come from the gateway, not the catalogue"
        );
        assert!(
            preset.offered_models(&[]).is_empty(),
            "a preset with no catalogue answer offers nothing"
        );
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("http://127.0.0.1:3425/v1")
        );
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::OpenaiResponses),
            Some("http://127.0.0.1:3425/v1")
        );
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::AnthropicMessages),
            Some("http://127.0.0.1:3425")
        );
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::GoogleGenerativeAi),
            Some("http://127.0.0.1:3425")
        );
    }

    #[test]
    fn region_override_rewrites_only_named_endpoints() {
        let preset = provider_preset(PROVIDER_PRESET_ZHIPU_GLM).unwrap();
        let global = preset.with_region(Some("global"));
        assert_eq!(
            global.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.z.ai/api/paas/v4")
        );
        let missing = preset.with_region(Some("nowhere"));
        assert_eq!(missing.endpoints, preset.endpoints);
    }

    #[test]
    fn offered_models_keep_only_the_plan_prefix_in_catalogue_order() {
        let mut preset = preset("example", "Example", &[]);
        preset.only = Some("plan-".to_string());
        // The catalogue's order is kept; ids outside the plan are dropped.
        assert_eq!(
            preset.offered_models(&[
                "plan-b".to_string(),
                "other-model".to_string(),
                "plan-a".to_string(),
            ]),
            vec!["plan-b".to_string(), "plan-a".to_string()]
        );
        // A catalogue answer that names no plan id offers nothing: there is no
        // private list to fall back to.
        assert!(
            preset
                .offered_models(&["other-model".to_string()])
                .is_empty()
        );
        // A preset with no prefix takes the catalogue's whole answer.
        let open = crate::provider_presets::preset_for_test("open", "Open", &[]);
        assert_eq!(
            open.offered_models(&["a".to_string(), "b".to_string()]),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn preset_payload_has_no_money_fields_beyond_static_plan_copy() {
        for preset in provider_presets() {
            let encoded = serde_json::to_string(&preset).unwrap();
            // `planNote` is vendor copy and is allowed; computed fields are not.
            let lower = encoded.to_lowercase();
            assert!(!lower.contains("\"cost\""));
            assert!(!lower.contains("\"price\""));
            assert!(!lower.contains("\"currency\""));
        }
    }

    /// A preset's id is the only handle a surface has on it, so the lookup has
    /// to return that same preset — which also catches two presets sharing an id,
    /// because the second one would never come back.
    #[test]
    fn every_preset_is_addressable_by_its_own_id() {
        for preset in provider_presets() {
            assert_eq!(
                provider_preset(&preset.id).as_ref(),
                Some(&preset),
                "{} is not addressable by its own id",
                preset.id
            );
        }
    }

    /// A plan that resells models says what its vendor's list cannot.
    #[test]
    fn a_plan_preset_carries_what_its_vendor_list_omits() {
        let cline = provider_preset(PROVIDER_PRESET_CLINEPASS).unwrap();
        assert_eq!(cline.only.as_deref(), Some("cline-pass/"));
        assert_eq!(
            cline.caveats,
            vec![ProviderPresetCaveat::ModelListExcludesPlan]
        );
        // A catalogue entry that lists the whole plan answers in full, and the
        // vendor's metered ids are dropped: a ClinePass key cannot spend them.
        let catalogue = vec![
            "gpt-5".to_string(),
            "cline-pass/glm-5.3".to_string(),
            "cline-pass/kimi-k3".to_string(),
        ];
        assert_eq!(
            cline.offered_models(&catalogue),
            vec![
                "cline-pass/glm-5.3".to_string(),
                "cline-pass/kimi-k3".to_string()
            ]
        );
        // A catalogue answer that names none of the plan's ids leaves the list
        // empty rather than falling back to a table the binary would carry.
        assert!(
            cline
                .offered_models(&["gpt-5".to_string(), "claude-sonnet-5".to_string()])
                .is_empty()
        );

        let command = provider_preset(PROVIDER_PRESET_COMMANDCODE).unwrap();
        assert_eq!(
            command.caveats,
            vec![ProviderPresetCaveat::ProtocolPerModel]
        );
        // One account, two protocol families: the Messages address is not the
        // OpenAI one, which is exactly why the preset carries both.
        assert_eq!(
            command.endpoint_for(ProviderModelWireApi::AnthropicMessages),
            Some("https://api.commandcode.ai/provider")
        );
        assert_eq!(
            command.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://api.commandcode.ai/provider/v1")
        );

        let go = provider_preset(PROVIDER_PRESET_OPENCODE_GO).unwrap();
        assert!(go.plan_note.is_some());
        assert!(go.keys_url.is_some());
        assert!(go.caveats.is_empty(), "a plain plan needs no warning");
        // Three protocols are served by two addresses, which is the shape the
        // plan shares with a vendor that offers both API families.
        assert_eq!(
            go.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            go.endpoint_for(ProviderModelWireApi::OpenaiResponses)
        );
        assert_ne!(
            go.endpoint_for(ProviderModelWireApi::AnthropicMessages),
            go.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions)
        );
    }

    /// AnyRouter answers the Anthropic and OpenAI families at different root
    /// paths, so a preset that shares one address between them sends one
    /// protocol to the other's endpoint.
    #[test]
    fn anyrouter_keeps_the_anthropic_and_openai_roots_apart() {
        let anyrouter = provider_preset(PROVIDER_PRESET_ANYROUTER).unwrap();
        assert_eq!(
            anyrouter.endpoint_for(ProviderModelWireApi::AnthropicMessages),
            Some("http://anyrouter.top")
        );
        assert_eq!(
            anyrouter.endpoint_for(ProviderModelWireApi::OpenaiResponses),
            Some("http://anyrouter.top/v1")
        );
    }

    /// A saved Provider becomes a template that fills the same fields and
    /// carries no credential.
    #[test]
    fn a_saved_provider_becomes_a_keyless_template() {
        let profile = crate::ModelProviderProfile {
            display_name: "House relay".to_string(),
            vendor_hint: Some("acme".to_string()),
            endpoints: vec![
                crate::ModelProviderEndpoint {
                    id: "api".to_string(),
                    kind: crate::ModelProviderEndpointKind::Api,
                    url: "https://relay.example.com/v1".to_string(),
                    wire_protocol_id: Some(
                        crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    ),
                },
                crate::ModelProviderEndpoint {
                    id: "api-anthropic".to_string(),
                    kind: crate::ModelProviderEndpointKind::Api,
                    url: "https://relay.example.com/anthropic".to_string(),
                    wire_protocol_id: Some(crate::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string()),
                },
                crate::ModelProviderEndpoint {
                    id: "models".to_string(),
                    kind: crate::ModelProviderEndpointKind::ModelCatalog,
                    url: "https://relay.example.com/models".to_string(),
                    wire_protocol_id: None,
                },
            ],
            configured_models: vec![
                crate::ModelProviderCatalogEntry {
                    id: "gpt-5".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                },
                crate::ModelProviderCatalogEntry {
                    id: "retired-model".to_string(),
                    display_name: None,
                    enabled: false,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                },
            ],
            headers: vec![crate::ModelProviderHeaderReference {
                name: "X-Relay".to_string(),
                value: crate::ModelProviderHeaderValue::NonSecretLiteral("on".to_string()),
                redacted_hint: String::new(),
            }],
            metadata: crate::ModelProviderMetadata {
                website_url: Some("https://relay.example.com".to_string()),
                category: Some("relay".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let preset = user_provider_preset(&profile).expect("a saved Provider is a template");
        assert!(preset.is_user_defined());
        assert_eq!(preset.display_name, "House relay");
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::OpenaiChatCompletions),
            Some("https://relay.example.com/v1")
        );
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::AnthropicMessages),
            Some("https://relay.example.com/anthropic")
        );
        assert_eq!(preset.models, vec!["gpt-5".to_string()]);
        assert_eq!(preset.category.as_deref(), Some("relay"));
        // No credential crosses into the template, in any field.
        assert!(preset.keys_url.is_none());
        assert!(preset.key_hint.is_none());
        assert_eq!(
            preset.source_provider_id.as_deref(),
            Some(profile.id.as_str())
        );
        let raw = serde_json::to_string(&preset).unwrap();
        assert!(!raw.contains("apiKey"));
    }

    /// A record whose only address is the unmarked fallback still becomes a
    /// template, under the protocol it starts on.
    #[test]
    fn a_single_address_record_still_becomes_a_template() {
        let profile = crate::ModelProviderProfile {
            display_name: "Work relay".to_string(),
            endpoints: vec![crate::ModelProviderEndpoint {
                id: "api".to_string(),
                kind: crate::ModelProviderEndpointKind::Api,
                url: "https://work.example.com/v1".to_string(),
                wire_protocol_id: None,
            }],
            configured_models: vec![crate::ModelProviderCatalogEntry {
                id: "claude-sonnet-4".to_string(),
                display_name: None,
                enabled: true,
                metadata: vec![crate::ProviderBindingMetadata {
                    key: "wireApi".to_string(),
                    value: crate::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string(),
                }],
                capabilities: Default::default(),
            }],
            ..Default::default()
        };
        let preset = user_provider_preset(&profile).expect("the fallback address is enough");
        assert_eq!(
            preset.endpoint_for(ProviderModelWireApi::AnthropicMessages),
            Some("https://work.example.com/v1")
        );
    }

    /// A record with no address has nothing a template could prefill.
    #[test]
    fn a_record_without_an_address_yields_no_template() {
        let profile = crate::ModelProviderProfile {
            display_name: "Empty".to_string(),
            ..Default::default()
        };
        assert!(user_provider_preset(&profile).is_none());
    }

    /// Templates are ordered by name, and an id addresses exactly one of them.
    #[test]
    fn user_templates_are_ordered_and_addressable() {
        let make = |name: &str| crate::ModelProviderProfile {
            display_name: name.to_string(),
            endpoints: vec![crate::ModelProviderEndpoint {
                id: "api".to_string(),
                kind: crate::ModelProviderEndpointKind::Api,
                url: format!("https://{name}.example.com/v1"),
                wire_protocol_id: Some(crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string()),
            }],
            ..Default::default()
        };
        let profiles = vec![make("zeta"), make("alpha")];
        let presets = user_provider_presets(&profiles);
        assert_eq!(
            presets
                .iter()
                .map(|preset| preset.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        assert!(
            user_provider_preset_for(&profiles, &presets[0].id).is_some(),
            "a template is found by the id it advertises"
        );
        assert!(
            presets
                .iter()
                .all(|preset| preset.id.starts_with(USER_PROVIDER_PRESET_PREFIX)),
            "a template can never shadow a shipped preset"
        );
    }

    /// A record created from a template is not a template itself.
    ///
    /// Without this, every copy would seed the next one and the library would
    /// grow a chain of near-identical rows out of a single duplicate.
    #[test]
    fn a_provider_copied_from_a_template_is_not_a_template() {
        let original = crate::ModelProviderProfile {
            display_name: "House relay".to_string(),
            endpoints: vec![crate::ModelProviderEndpoint {
                id: "api".to_string(),
                kind: crate::ModelProviderEndpointKind::Api,
                url: "https://relay.example.com/v1".to_string(),
                wire_protocol_id: Some(crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string()),
            }],
            ..Default::default()
        };
        let copy = crate::ModelProviderProfile {
            display_name: "House relay copy".to_string(),
            endpoints: original.endpoints.clone(),
            metadata: crate::ModelProviderMetadata {
                preset_source_provider_id: Some(original.id.as_str().to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let presets = user_provider_presets(&[original.clone(), copy.clone()]);
        assert_eq!(
            presets
                .iter()
                .map(|preset| preset.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["House relay"],
            "only the Provider the reader configured is a template"
        );
        assert!(
            user_provider_preset_for(
                std::slice::from_ref(&copy),
                &user_provider_preset_id(copy.id.as_str()),
            )
            .is_none(),
            "the copy is never addressed as a template either"
        );

        // Removing the marker — what an older record simply never had — is the
        // only thing that makes the copy a template again.
        let mut adopted = copy;
        adopted.metadata.preset_source_provider_id = None;
        assert_eq!(user_provider_presets(&[adopted]).len(), 1);
    }
}
