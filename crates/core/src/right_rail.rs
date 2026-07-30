use serde::{Deserialize, Serialize};

use crate::ids::RightRailPluginId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightRailPluginKind {
    System,
    Web,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightRailSystemPluginKey {
    Files,
    Git,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightRailPluginStatus {
    Enabled,
    Disabled,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightRailWebPluginUaMode {
    Desktop,
    Mobile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailPlugin {
    pub id: RightRailPluginId,
    pub kind: RightRailPluginKind,
    pub system_key: Option<RightRailSystemPluginKey>,
    pub builtin_key: Option<String>,
    pub display_name: String,
    pub url: Option<String>,
    pub logo: Option<String>,
    pub desktop_user_agent: Option<String>,
    pub mobile_user_agent: Option<String>,
    pub ua_mode: Option<RightRailWebPluginUaMode>,
    pub status: RightRailPluginStatus,
    pub order_index: i64,
    pub data_directory: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailPluginCreateRequest {
    pub display_name: String,
    pub url: String,
    pub logo: Option<String>,
    pub desktop_user_agent: Option<String>,
    pub mobile_user_agent: Option<String>,
    pub ua_mode: RightRailWebPluginUaMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailPluginUpdateRequest {
    pub id: RightRailPluginId,
    pub display_name: Option<String>,
    pub url: Option<String>,
    pub logo: Option<String>,
    pub clear_logo: bool,
    pub desktop_user_agent: Option<String>,
    pub clear_desktop_user_agent: bool,
    pub mobile_user_agent: Option<String>,
    pub clear_mobile_user_agent: bool,
    pub ua_mode: Option<RightRailWebPluginUaMode>,
    pub status: Option<RightRailPluginStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailPluginDeleteRequest {
    pub id: RightRailPluginId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailPluginReorderRequest {
    pub ordered_plugin_ids: Vec<RightRailPluginId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightRailIframeEmbedStatus {
    Supported,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailIframeEmbedCheckRequest {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailIframeEmbedCheckResponse {
    pub status: RightRailIframeEmbedStatus,
    pub blocking_header: Option<String>,
    pub blocking_value: Option<String>,
    pub final_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebviewBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebviewOpenRequest {
    pub plugin_id: RightRailPluginId,
    pub activation_seq: u32,
    pub url: String,
    pub user_agent: Option<String>,
    pub bounds: RightRailWebviewBounds,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebviewNavigateRequest {
    pub plugin_id: RightRailPluginId,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebviewSetBoundsRequest {
    pub plugin_id: RightRailPluginId,
    pub bounds: RightRailWebviewBounds,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebviewShowRequest {
    pub plugin_id: RightRailPluginId,
    pub activation_seq: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebviewHideRequest {
    pub plugin_id: RightRailPluginId,
    pub activation_seq: u32,
    pub deactivate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RightRailWebviewCloseRequest {
    pub plugin_id: RightRailPluginId,
}
