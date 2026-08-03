//! Typed ACP protocol layer with bounded raw-envelope preservation (P2-01).
//!
//! Plan references: §7.1 (typed stable core), §7.2 (operation matrix),
//! §7.3 (raw envelope preservation), §7.4 (version identity), §25.1 (tests).
//!
//! Design rules enforced here:
//! - Stable-core outbound messages (`initialize`, `session/new`,
//!   `session/load`, `session/prompt`, `session/cancel`) are built through the
//!   official typed ACP v1 schema instead of hand-written field strings.
//! - Method name strings are centralized in [`AcpOperation::method`].
//! - Inbound traffic is classified through [`decode_incoming`], which keeps a
//!   redacted, bounded [`BoundedRawAcpEnvelope`] next to the typed
//!   interpretation instead of replacing the original message.
//! - `method-not-found` maps to a single-operation downgrade signal
//!   ([`AcpProtocolError::MethodNotFound`]), never a connection failure.
//!
//! ## Known quirks between the fixed official schema and the current Vibex
//! wire behavior (wire behavior wins; do NOT change the wire to match the
//! schema):
//!
//! 1. `initialize.clientCapabilities` on the current wire carries the
//!    adapter-extension keys `auth`, `mcpServers` and `meta` (plain `meta`,
//!    not the spec's `_meta`). The stable
//!    [`agent_client_protocol_schema::v1::ClientCapabilities`] surface cannot
//!    represent them, so [`build_initialize_params`] serializes the typed base
//!    and then splices these extension keys back in.
//! 2. `session/new.mcpServers` on the current wire uses the Vibex descriptor
//!    shape `{id, name, transport, command|url, args}` while
//!    the official [`agent_client_protocol_schema::v1::McpServer`] wire shape
//!    is different. The builder keeps the local descriptor serialization and
//!    splices it into the typed request base.
//! 3. `session/load` reuses the same local `mcpServers` quirk (2).
//! 4. Inbound decoding is deliberately tolerant: the runtime accepts looser
//!    shapes than the fixed schema (e.g. permissive permission-request and
//!    `session/update` payloads from real adapters). [`decode_incoming`]
//!    therefore classifies by method through the operation matrix and keeps
//!    the raw params authoritative; strict typed enforcement of inbound
//!    bodies is deferred to the compatibility registry (P2-02+).

use std::fmt;
use std::path::Path;

use agent_client_protocol_schema::{
    ProtocolVersion,
    v1::{
        CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities,
        Implementation, InitializeRequest, LoadSessionRequest, NewSessionRequest, PromptRequest,
        ResumeSessionRequest, SessionId, SetSessionConfigOptionRequest, SetSessionModeRequest,
    },
};
use serde_json::{Value, json};

/// Default bound applied to preserved raw envelopes before they enter logs,
/// diagnostics storage or the timeline.
pub(crate) const DEFAULT_RAW_ENVELOPE_LIMIT_BYTES: usize = 8 * 1024;
const ACP_METHOD_METADATA_LIMIT: usize = 128;

/// Wire value of the JSON-RPC `method not found` error code.
pub(crate) const JSON_RPC_METHOD_NOT_FOUND: i64 = -32601;

// ---------------------------------------------------------------------------
// Operation matrix (§7.2)
// ---------------------------------------------------------------------------

/// Every ACP operation Vibex knows about, standard or adapter extension.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AcpOperation {
    Initialize,
    Authenticate,
    SessionNew,
    SessionPrompt,
    SessionCancel,
    SessionLoad,
    SessionList,
    SessionSetMode,
    SessionResume,
    SessionFork,
    SessionSetConfigOption,
    SessionSetModel,
    SessionUpdate,
    PermissionRequest,
    FsReadTextFile,
    FsWriteTextFile,
    TerminalCreate,
    TerminalKill,
    TerminalRelease,
    TerminalOutput,
    TerminalWaitForExit,
    /// Adapter-private method (for example `_claude/*`).
    Extension(String),
}

impl AcpOperation {
    /// Canonical wire method name. Single source of truth for standard ACP
    /// method strings; hand-written method literals are not allowed outside
    /// this function.
    pub fn method(&self) -> &str {
        match self {
            Self::Initialize => "initialize",
            Self::Authenticate => "authenticate",
            Self::SessionNew => "session/new",
            Self::SessionPrompt => "session/prompt",
            Self::SessionCancel => "session/cancel",
            Self::SessionLoad => "session/load",
            Self::SessionList => "session/list",
            Self::SessionSetMode => "session/set_mode",
            Self::SessionResume => "session/resume",
            Self::SessionFork => "session/fork",
            Self::SessionSetConfigOption => "session/set_config_option",
            Self::SessionSetModel => "session/set_model",
            Self::SessionUpdate => "session/update",
            Self::PermissionRequest => "session/request_permission",
            Self::FsReadTextFile => "fs/read_text_file",
            Self::FsWriteTextFile => "fs/write_text_file",
            Self::TerminalCreate => "terminal/create",
            Self::TerminalKill => "terminal/kill",
            Self::TerminalRelease => "terminal/release",
            Self::TerminalOutput => "terminal/output",
            Self::TerminalWaitForExit => "terminal/wait_for_exit",
            Self::Extension(method) => method,
        }
    }

    /// Reverse lookup used by inbound dispatch; unknown methods become
    /// [`AcpOperation::Extension`].
    pub(crate) fn from_method(method: &str) -> Self {
        match method {
            "initialize" => Self::Initialize,
            "authenticate" => Self::Authenticate,
            "session/new" => Self::SessionNew,
            "session/prompt" => Self::SessionPrompt,
            "session/cancel" => Self::SessionCancel,
            "session/load" => Self::SessionLoad,
            "session/list" => Self::SessionList,
            "session/set_mode" => Self::SessionSetMode,
            "session/resume" => Self::SessionResume,
            "session/fork" => Self::SessionFork,
            "session/set_config_option" => Self::SessionSetConfigOption,
            "session/set_model" => Self::SessionSetModel,
            "session/update" => Self::SessionUpdate,
            "session/request_permission" => Self::PermissionRequest,
            "fs/read_text_file" => Self::FsReadTextFile,
            "fs/write_text_file" => Self::FsWriteTextFile,
            "terminal/create" => Self::TerminalCreate,
            "terminal/kill" => Self::TerminalKill,
            "terminal/release" => Self::TerminalRelease,
            "terminal/output" => Self::TerminalOutput,
            "terminal/wait_for_exit" => Self::TerminalWaitForExit,
            other => Self::Extension(other.to_string()),
        }
    }
}

/// Protocol-level stability class recorded for every operation (§7.2).
///
/// Consumed by the Compatibility Registry (P2-02) and session-config
/// negotiation (P3-02); until those land only tests exercise the matrix.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpOperationStability {
    /// Supported by the fixed schema version; typed encode/decode always.
    StableCore,
    /// Standard operation, but only callable after the initialize result
    /// (or negotiated capability snapshot) declares support.
    CapabilityGated,
    /// Versioned / unstable operation; typed preferred, versioned raw
    /// request when the SDK lacks the surface. Encoding selection happens
    /// through negotiation (P3-02/P3-03), never by assumption.
    VersionedUnstable,
    /// Adapter-private method requiring an exact adapter compatibility
    /// identity match and a dedicated extension codec.
    AdapterExtension,
}

/// Wire encoding strategy attached to an operation support entry.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpWireEncoding {
    Typed,
    VersionedRaw,
    ExtensionCodec,
}

/// Where a support decision came from. The baseline matrix only uses
/// `FixedSchema`; runtime negotiation (P2-02/P3-02) overrides `source`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySource {
    /// Static boundary derived from the pinned schema version.
    FixedSchema,
    /// Conservative fallback when no stronger evidence exists.
    ConservativeDefault,
    /// Declared by a Provider Profile.
    DeclaredProfile,
    /// Declared by an exact-version compatibility descriptor.
    VersionedRegistry,
    /// Declared by the initialize result or another negotiated runtime result.
    NegotiatedRuntime,
    /// Observed while invoking the live runtime (e.g. method-not-found).
    ObservedRuntime,
}

/// Per-operation support record (§7.2 recommended shape).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpOperationSupport {
    pub operation: AcpOperation,
    pub stability: AcpOperationStability,
    pub encoding: AcpWireEncoding,
    pub source: CapabilitySource,
    pub protocol_version: Option<String>,
    pub adapter_compatibility_identity: String,
}

#[allow(dead_code)]
fn support(
    operation: AcpOperation,
    stability: AcpOperationStability,
    encoding: AcpWireEncoding,
) -> AcpOperationSupport {
    AcpOperationSupport {
        operation,
        stability,
        encoding,
        source: CapabilitySource::FixedSchema,
        protocol_version: Some(ProtocolVersion::V1.to_string()),
        adapter_compatibility_identity: "generic-acp".to_string(),
    }
}

/// Static operation boundary for the pinned schema version (§7.2 table).
///
/// Runtime negotiation results (initialize response, adapter descriptor,
/// contract tests) override `source`/`encoding` per process instance in
/// later phases; this baseline never authorizes calling capability-gated or
/// unstable operations by itself.
#[allow(dead_code)] // consumed by P2-02 Compatibility Registry
pub fn baseline_operation_matrix() -> Vec<AcpOperationSupport> {
    use AcpOperation as Op;
    use AcpOperationStability as St;
    use AcpWireEncoding as Enc;
    vec![
        // Stable core: fixed schema support, always typed.
        support(Op::Initialize, St::StableCore, Enc::Typed),
        support(Op::SessionNew, St::StableCore, Enc::Typed),
        support(Op::SessionPrompt, St::StableCore, Enc::Typed),
        support(Op::SessionCancel, St::StableCore, Enc::Typed),
        support(Op::SessionUpdate, St::StableCore, Enc::Typed),
        support(Op::PermissionRequest, St::StableCore, Enc::Typed),
        support(Op::FsReadTextFile, St::StableCore, Enc::Typed),
        support(Op::FsWriteTextFile, St::StableCore, Enc::Typed),
        support(Op::TerminalCreate, St::StableCore, Enc::Typed),
        support(Op::TerminalKill, St::StableCore, Enc::Typed),
        support(Op::TerminalRelease, St::StableCore, Enc::Typed),
        support(Op::TerminalOutput, St::StableCore, Enc::Typed),
        support(Op::TerminalWaitForExit, St::StableCore, Enc::Typed),
        // Capability-gated standard: initialize must declare support;
        // method-not-found downgrades only this capability.
        support(Op::SessionLoad, St::CapabilityGated, Enc::Typed),
        support(Op::SessionResume, St::CapabilityGated, Enc::Typed),
        support(Op::SessionSetMode, St::CapabilityGated, Enc::Typed),
        support(Op::SessionList, St::CapabilityGated, Enc::Typed),
        // Versioned / unstable: typed preferred, versioned raw fallback; the
        // actual encoding choice is negotiated (P3-02/P3-03).
        support(Op::SessionFork, St::VersionedUnstable, Enc::VersionedRaw),
        support(
            Op::SessionSetConfigOption,
            St::VersionedUnstable,
            Enc::VersionedRaw,
        ),
        support(
            Op::SessionSetModel,
            St::VersionedUnstable,
            Enc::VersionedRaw,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Error taxonomy (§7.2 / design §4)
// ---------------------------------------------------------------------------

/// Unified protocol error taxonomy. `MethodNotFound` is a single-operation
/// downgrade signal: callers mark the operation unsupported and keep the
/// connection alive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // variants constructed incrementally (P2-03 transport, P3-02 negotiation)
pub(crate) enum AcpProtocolError {
    /// The agent answered `-32601`; only `operation` is downgraded.
    MethodNotFound { operation: AcpOperation },
    /// The agent proved that the native resource needed by a restore
    /// operation no longer exists. Callers may continue the bounded restore
    /// chain without treating every provider error as recoverable.
    ResourceNotFound { operation: AcpOperation },
    /// A response arrived but did not carry the shape the operation
    /// requires (e.g. `session/new` without `sessionId`).
    InvalidResponseShape {
        operation: AcpOperation,
        detail: String,
    },
    /// The negotiated protocol version does not match expectations.
    ProtocolVersionMismatch,
    /// A payload could not be decoded at all.
    DecodeFailure,
    /// The transport (process stdio) closed underneath the request.
    TransportClosed,
    /// The agent reported a JSON-RPC error other than method-not-found.
    AgentReportedError {
        code: String,
        message_redacted: String,
    },
}

impl AcpProtocolError {
    /// Stable identifier used in structured error diagnostics.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::MethodNotFound { .. } => "method_not_found",
            Self::ResourceNotFound { .. } => "resource_not_found",
            Self::InvalidResponseShape { .. } => "invalid_response_shape",
            Self::ProtocolVersionMismatch => "protocol_version_mismatch",
            Self::DecodeFailure => "decode_failure",
            Self::TransportClosed => "transport_closed",
            Self::AgentReportedError { .. } => "agent_reported_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcpRpcFailureDataKind {
    Other,
    ResourceNotFound,
}

pub(crate) fn classify_rpc_failure_data(data: Option<&Value>) -> AcpRpcFailureDataKind {
    if data.is_some_and(|data| rpc_failure_data_contains_missing_resource(data, 0)) {
        AcpRpcFailureDataKind::ResourceNotFound
    } else {
        AcpRpcFailureDataKind::Other
    }
}

fn rpc_failure_data_contains_missing_resource(value: &Value, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    match value {
        Value::String(value) => {
            let normalized = value
                .chars()
                .take(512)
                .collect::<String>()
                .to_ascii_lowercase();
            [
                "no rollout found for thread id",
                "no rollout found for thread",
                "session not found",
                "unknown session",
                "thread not found",
                "unknown thread",
            ]
            .iter()
            .any(|needle| normalized.contains(needle))
        }
        Value::Array(values) => values
            .iter()
            .take(16)
            .any(|value| rpc_failure_data_contains_missing_resource(value, depth + 1)),
        Value::Object(values) => values
            .values()
            .take(16)
            .any(|value| rpc_failure_data_contains_missing_resource(value, depth + 1)),
        _ => false,
    }
}

/// Classify a JSON-RPC error response for an outbound operation.
/// `-32601` becomes the per-operation downgrade signal.
pub(crate) fn classify_rpc_failure(
    operation: AcpOperation,
    code: &str,
    redacted_message: &str,
    data_kind: AcpRpcFailureDataKind,
) -> AcpProtocolError {
    if code
        .trim()
        .parse::<i64>()
        .map(|code| code == JSON_RPC_METHOD_NOT_FOUND)
        .unwrap_or(false)
    {
        AcpProtocolError::MethodNotFound { operation }
    } else if data_kind == AcpRpcFailureDataKind::ResourceNotFound
        && matches!(
            operation,
            AcpOperation::SessionResume | AcpOperation::SessionLoad
        )
    {
        AcpProtocolError::ResourceNotFound { operation }
    } else {
        AcpProtocolError::AgentReportedError {
            code: code.to_string(),
            message_redacted: redacted_message.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Bounded raw envelope + redaction (§7.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Outgoing capture lands with P2-03 process registry
pub(crate) enum AcpMessageDirection {
    Incoming,
    Outgoing,
}

/// Redacted, size-bounded copy of a wire message kept next to its typed
/// interpretation. Request id, method and native session id survive so
/// replay / compatibility triage stays possible; unknown non-sensitive
/// fields are preserved inside `redacted_body`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct BoundedRawAcpEnvelope {
    pub direction: AcpMessageDirection,
    pub method: Option<String>,
    pub request_id: Option<Value>,
    pub native_session_id: Option<String>,
    /// Redacted JSON body, truncated to the byte limit when needed.
    pub redacted_body: String,
    pub truncated: bool,
    /// Byte length of the redacted body before truncation.
    pub byte_len: usize,
}

impl fmt::Debug for BoundedRawAcpEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedRawAcpEnvelope")
            .field("direction", &self.direction)
            .field("method", &self.method)
            .field("has_request_id", &self.request_id.is_some())
            .field(
                "native_session_id_hash",
                &self
                    .native_session_id
                    .as_deref()
                    .map(crate::session_attachment_registry::redact_native_id),
            )
            .field("truncated", &self.truncated)
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

impl BoundedRawAcpEnvelope {
    pub(crate) fn capture(
        direction: AcpMessageDirection,
        message: &Value,
        limit_bytes: usize,
    ) -> Self {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .map(safe_method_metadata);
        let request_id = message.get("id").cloned();
        let native_session_id = message
            .get("params")
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let redacted = redact_envelope(message);
        let serialized = serde_json::to_string(&redacted).unwrap_or_else(|_| "{}".to_string());
        let byte_len = serialized.len();
        let (redacted_body, truncated) = if byte_len > limit_bytes {
            let mut cut = limit_bytes;
            while cut > 0 && !serialized.is_char_boundary(cut) {
                cut -= 1;
            }
            (serialized[..cut].to_string(), true)
        } else {
            (serialized, false)
        };
        Self {
            direction,
            method,
            request_id,
            native_session_id,
            redacted_body,
            truncated,
            byte_len,
        }
    }

    fn retain_metadata_only(&mut self) {
        let source_truncated = self.truncated;
        self.redacted_body = json!({
            "method": self.method.as_deref(),
            "hasRequestId": self.request_id.is_some(),
            "hasNativeSessionId": self.native_session_id.is_some(),
            "payloadRedacted": true,
            "sourceTruncated": source_truncated,
            "byteLen": self.byte_len,
        })
        .to_string();
        self.truncated = true;
    }
}

/// Redact a wire message before it may enter logs, diagnostics storage or
/// the timeline: sensitive key values are replaced and absolute home paths
/// inside strings are masked. Unknown non-sensitive fields are preserved.
pub(crate) fn redact_envelope(value: &Value) -> Value {
    let home = std::env::var("HOME").ok().filter(|home| home.len() > 1);
    redact_envelope_value(value, home.as_deref())
}

fn redact_envelope_value(value: &Value, home: Option<&str>) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    if envelope_key_is_sensitive(key) {
                        (key.clone(), Value::String("[redacted]".to_string()))
                    } else {
                        (key.clone(), redact_envelope_value(value, home))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_envelope_value(item, home))
                .collect(),
        ),
        Value::String(text) => {
            let masked = match home {
                Some(home) if text.contains(home) => text.replace(home, "~"),
                _ => text.clone(),
            };
            if crate::looks_sensitive(&masked) {
                Value::String("[redacted]".to_string())
            } else {
                Value::String(masked)
            }
        }
        other => other.clone(),
    }
}

pub(crate) fn safe_method_metadata(method: &str) -> String {
    let method = method.trim();
    if method.is_empty()
        || method.len() > ACP_METHOD_METADATA_LIMIT
        || !method.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/' | ':')
        })
    {
        "invalid_method".to_string()
    } else {
        method.to_string()
    }
}

fn envelope_key_is_sensitive(key: &str) -> bool {
    let key = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    key.contains("apikey")
        || key.contains("token")
        || key.contains("authorization")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("credential")
        || matches!(
            key.as_str(),
            "env"
                | "headers"
                | "prompt"
                | "content"
                | "rawinput"
                | "rawoutput"
                | "rawpayload"
                | "payload"
                | "sessionid"
                | "nativesessionid"
        )
}

// ---------------------------------------------------------------------------
// Typed decode with raw preservation (§7.3)
// ---------------------------------------------------------------------------

/// A standard (matrix-known) inbound message. The raw `params` stay
/// authoritative for the runtime handlers (quirk 5 in the module docs);
/// `operation` is the typed interpretation of the method.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StandardAcpMessage {
    pub operation: AcpOperation,
    pub request_id: Option<Value>,
    pub params: Value,
}

#[derive(Clone, PartialEq)]
pub(crate) enum AcpDecodedPayload {
    Standard(StandardAcpMessage),
    Extension {
        method: String,
        params: Value,
    },
    Malformed {
        error: AcpProtocolError,
        detail: String,
    },
}

impl fmt::Debug for AcpDecodedPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standard(message) => formatter
                .debug_struct("Standard")
                .field("operation", &message.operation)
                .field("has_request_id", &message.request_id.is_some())
                .finish(),
            Self::Extension { method, .. } => formatter
                .debug_struct("Extension")
                .field("method", &safe_method_metadata(method))
                .field("params", &"<redacted>")
                .finish(),
            Self::Malformed { error, .. } => formatter
                .debug_struct("Malformed")
                .field("error_code", &error.kind())
                .finish(),
        }
    }
}

/// Typed interpretation plus the preserved raw envelope (§7.3).
#[derive(Clone, PartialEq)]
pub(crate) struct DecodedAcpMessage {
    pub raw: BoundedRawAcpEnvelope,
    pub decoded: AcpDecodedPayload,
}

impl fmt::Debug for DecodedAcpMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedAcpMessage")
            .field("raw", &self.raw)
            .field("decoded", &self.decoded)
            .finish()
    }
}

/// Decode a method-bearing inbound message (request or notification).
/// Unknown methods are classified as `Extension`; structurally invalid
/// messages become `Malformed` with a taxonomy entry. Raw preservation is
/// unconditional.
pub(crate) fn decode_incoming(message: &Value) -> DecodedAcpMessage {
    let mut raw = BoundedRawAcpEnvelope::capture(
        AcpMessageDirection::Incoming,
        message,
        DEFAULT_RAW_ENVELOPE_LIMIT_BYTES,
    );
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        raw.retain_metadata_only();
        return DecodedAcpMessage {
            raw,
            decoded: AcpDecodedPayload::Malformed {
                error: AcpProtocolError::DecodeFailure,
                detail: "message carries no method".to_string(),
            },
        };
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let request_id = message.get("id").cloned();
    let decoded = match AcpOperation::from_method(method) {
        AcpOperation::Extension(method) => {
            raw.retain_metadata_only();
            AcpDecodedPayload::Extension { method, params }
        }
        operation => AcpDecodedPayload::Standard(StandardAcpMessage {
            operation,
            request_id,
            params,
        }),
    };
    DecodedAcpMessage { raw, decoded }
}

// ---------------------------------------------------------------------------
// Stable-core typed builders (§7.1)
// ---------------------------------------------------------------------------

/// Build `initialize` params through the typed schema, then re-apply the
/// adapter-extension capability keys the current wire requires (quirk 1).
pub(crate) fn build_initialize_params(
    read_text_file: bool,
    write_text_file: bool,
    terminal_tools: bool,
    terminal_auth: bool,
    mcp_servers: bool,
) -> Value {
    let mut fs = FileSystemCapabilities::new();
    fs.read_text_file = read_text_file;
    fs.write_text_file = write_text_file;
    let mut capabilities = ClientCapabilities::new();
    capabilities.fs = fs;
    capabilities.terminal = terminal_tools;
    let mut request = InitializeRequest::new(ProtocolVersion::V1);
    request.client_capabilities = capabilities;
    request.client_info = Some(Implementation::new("vibex", env!("CARGO_PKG_VERSION")));

    let mut params = serde_json::to_value(&request).unwrap_or_else(|_| json!({}));
    // Keep this defensive normalization so a schema serialization change
    // cannot silently add a nullable field to Vibex's frozen wire contract.
    if let Some(client_info) = params.get_mut("clientInfo").and_then(Value::as_object_mut)
        && client_info.get("title") == Some(&Value::Null)
    {
        client_info.remove("title");
    }
    // Adapter-extension capability keys not representable in the stable
    // schema's ClientCapabilities.
    if let Some(capabilities) = params
        .get_mut("clientCapabilities")
        .and_then(Value::as_object_mut)
    {
        capabilities.insert("auth".to_string(), json!({ "terminal": terminal_auth }));
        capabilities.insert("mcpServers".to_string(), Value::Bool(mcp_servers));
        capabilities.insert(
            "meta".to_string(),
            json!({
                "terminal_output": terminal_tools,
                "terminal-auth": terminal_auth,
                "mcpServers": mcp_servers
            }),
        );
    }
    params
}

/// Build `session/new` params: typed base (`cwd`) plus the local
/// `mcpServers` descriptor serialization (quirk 2).
pub(crate) fn build_session_new_params(cwd: &Path, mcp_servers: Value) -> Value {
    let request = NewSessionRequest::new(cwd.display().to_string());
    let mut params = serde_json::to_value(&request).unwrap_or_else(|_| json!({}));
    if let Some(object) = params.as_object_mut() {
        object.insert("mcpServers".to_string(), mcp_servers);
    }
    params
}

/// Build `session/load` params (capability-gated; caller must have verified
/// the initialize capability). Same `mcpServers` quirk as `session/new`.
pub(crate) fn build_session_load_params(
    native_session_id: &str,
    cwd: &Path,
    mcp_servers: Value,
) -> Value {
    let request =
        LoadSessionRequest::new(SessionId::new(native_session_id), cwd.display().to_string());
    let mut params = serde_json::to_value(&request).unwrap_or_else(|_| json!({}));
    if let Some(object) = params.as_object_mut() {
        object.insert("mcpServers".to_string(), mcp_servers);
    }
    params
}

/// Build `session/resume` through the official v1 schema while retaining the
/// existing exact-version/current-generation capability gate at the caller.
pub(crate) fn build_session_resume_params(
    native_session_id: &str,
    cwd: &Path,
    mcp_servers: Value,
) -> Value {
    let request =
        ResumeSessionRequest::new(SessionId::new(native_session_id), cwd.display().to_string());
    let mut params = serde_json::to_value(&request).unwrap_or_else(|_| json!({}));
    if let Some(object) = params.as_object_mut() {
        object.insert("mcpServers".to_string(), mcp_servers);
    }
    params
}

/// Build `session/prompt` params through the typed `PromptRequest`.
///
/// The runtime assembles content blocks as JSON values; they are decoded into
/// typed [`ContentBlock`]s for validation and re-serialized. If a block does
/// not fit the fixed schema (adapter-specific content), the original blocks
/// pass through unchanged so wire behavior is preserved.
pub(crate) fn build_session_prompt_params(native_session_id: &str, prompt: Vec<Value>) -> Value {
    let typed_blocks: Result<Vec<ContentBlock>, _> = prompt
        .iter()
        .map(|block| serde_json::from_value::<ContentBlock>(block.clone()))
        .collect();
    match typed_blocks {
        Ok(blocks) => {
            let request = PromptRequest::new(SessionId::new(native_session_id), blocks);
            serde_json::to_value(&request)
                .unwrap_or_else(|_| json!({ "sessionId": native_session_id, "prompt": prompt }))
        }
        // Raw preservation gate: never drop adapter-specific content.
        Err(_) => json!({ "sessionId": native_session_id, "prompt": prompt }),
    }
}

/// Build `session/cancel` notification params through the typed schema.
pub(crate) fn build_session_cancel_params(native_session_id: &str) -> Value {
    let notification = CancelNotification::new(SessionId::new(native_session_id));
    serde_json::to_value(&notification)
        .unwrap_or_else(|_| json!({ "sessionId": native_session_id }))
}

/// Typed stable-schema request builder for `session/set_mode`.
pub(crate) fn build_session_set_mode_params(native_session_id: &str, mode_id: &str) -> Value {
    serde_json::to_value(SetSessionModeRequest::new(
        native_session_id.to_string(),
        mode_id.to_string(),
    ))
    .expect("ACP set_mode request is serializable")
}

/// Versioned `session/set_model` request.  ACP adapters in the supported
/// compatibility matrix use this raw envelope even when the pinned schema
/// does not expose a typed request.
pub(crate) fn build_session_set_model_params(native_session_id: &str, model_id: &str) -> Value {
    json!({
        "sessionId": native_session_id,
        "modelId": model_id,
    })
}

/// Typed schema request builder for select-valued `session/set_config_option`.
/// Boolean/string extensions are encoded by the versioned raw path in the
/// session-config planner because the pinned schema feature is intentionally
/// not enabled globally.
pub(crate) fn build_session_set_config_option_params(
    native_session_id: &str,
    config_id: &str,
    value: &str,
) -> Value {
    serde_json::to_value(SetSessionConfigOptionRequest::new(
        native_session_id.to_string(),
        config_id.to_string(),
        value,
    ))
    .expect("ACP set_config_option request is serializable")
}

/// Versioned raw config-option envelope.  This is intentionally separate from
/// the select-only typed builder so boolean/string options cannot be silently
/// coerced into a schema type the adapter did not negotiate.
pub(crate) fn build_session_set_config_option_raw_params(
    native_session_id: &str,
    config_id: &str,
    value: Value,
) -> Value {
    json!({
        "sessionId": native_session_id,
        "configId": config_id,
        "value": value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // §25.1: standard request serialization matches the frozen wire shapes.
    #[test]
    fn typed_initialize_params_match_frozen_wire_shape() {
        let params = build_initialize_params(true, true, false, true, true);
        assert_eq!(
            params,
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": true },
                    "terminal": false,
                    "auth": { "terminal": true },
                    "mcpServers": true,
                    "meta": {
                        "terminal_output": false,
                        "terminal-auth": true,
                        "mcpServers": true
                    }
                },
                "clientInfo": {
                    "name": "vibex",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })
        );
    }

    #[test]
    fn typed_session_new_and_load_params_match_frozen_wire_shape() {
        let servers = json!([{
            "id": "filesystem",
            "name": "Filesystem",
            "transport": "stdio",
            "command": "mcp-server-filesystem",
            "args": ["--root", "/tmp/w"]
        }]);
        assert_eq!(
            build_session_new_params(Path::new("/tmp/w"), servers.clone()),
            json!({ "cwd": "/tmp/w", "mcpServers": servers })
        );
        assert_eq!(
            build_session_load_params("native-1", Path::new("/tmp/w"), servers.clone()),
            json!({ "sessionId": "native-1", "cwd": "/tmp/w", "mcpServers": servers })
        );
        let servers = json!([]);
        assert_eq!(
            build_session_resume_params("native-1", Path::new("/tmp/w"), servers.clone()),
            json!({ "sessionId": "native-1", "cwd": "/tmp/w", "mcpServers": servers })
        );
    }

    #[cfg(unix)]
    #[test]
    fn typed_session_paths_preserve_the_existing_lossy_wire_encoding() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let cwd = PathBuf::from(OsString::from_vec(b"/tmp/non-utf8-\xff".to_vec()));
        let expected = cwd.display().to_string();
        assert_eq!(build_session_new_params(&cwd, json!([]))["cwd"], expected);
        assert_eq!(
            build_session_load_params("native-1", &cwd, json!([]))["cwd"],
            expected
        );
        assert_eq!(
            build_session_resume_params("native-1", &cwd, json!([]))["cwd"],
            expected
        );
    }

    #[test]
    fn typed_prompt_params_round_trip_standard_content_blocks() {
        let prompt = vec![
            json!({ "type": "text", "text": "hello" }),
            json!({
                "type": "resource_link",
                "uri": "file:///tmp/notes.txt",
                "name": "notes.txt"
            }),
            json!({ "type": "image", "data": "aGk=", "mimeType": "image/png" }),
        ];
        let params = build_session_prompt_params("native-1", prompt.clone());
        assert_eq!(params["sessionId"], "native-1");
        assert_eq!(params["prompt"], Value::Array(prompt));
    }

    #[test]
    fn prompt_params_preserve_non_schema_content_blocks() {
        let prompt = vec![json!({ "type": "adapter_custom", "blob": "x" })];
        let params = build_session_prompt_params("native-1", prompt.clone());
        assert_eq!(params["prompt"], Value::Array(prompt));
    }

    #[test]
    fn typed_cancel_params_match_frozen_wire_shape() {
        assert_eq!(
            build_session_cancel_params("native-1"),
            json!({ "sessionId": "native-1" })
        );
    }

    #[test]
    fn session_config_request_builders_keep_typed_and_raw_shapes_distinct() {
        assert_eq!(
            build_session_set_mode_params("native-1", "review"),
            json!({ "sessionId": "native-1", "modeId": "review" })
        );
        assert_eq!(
            build_session_set_model_params("native-1", "model-b"),
            json!({ "sessionId": "native-1", "modelId": "model-b" })
        );
        assert_eq!(
            build_session_set_config_option_params("native-1", "effort", "high"),
            json!({ "sessionId": "native-1", "configId": "effort", "value": "high" })
        );
        assert_eq!(
            build_session_set_config_option_raw_params("native-1", "approval", json!(true)),
            json!({ "sessionId": "native-1", "configId": "approval", "value": true })
        );
    }

    // §25.1: operation matrix agrees with the fixed schema boundary.
    #[test]
    fn baseline_matrix_matches_fixed_schema_boundary() {
        let matrix = baseline_operation_matrix();
        let stability = |operation: &AcpOperation| {
            matrix
                .iter()
                .find(|entry| &entry.operation == operation)
                .map(|entry| entry.stability)
                .unwrap_or_else(|| panic!("matrix missing {operation:?}"))
        };
        for operation in [
            AcpOperation::Initialize,
            AcpOperation::SessionNew,
            AcpOperation::SessionPrompt,
            AcpOperation::SessionCancel,
            AcpOperation::SessionUpdate,
            AcpOperation::PermissionRequest,
            AcpOperation::FsReadTextFile,
            AcpOperation::FsWriteTextFile,
            AcpOperation::TerminalCreate,
            AcpOperation::TerminalKill,
            AcpOperation::TerminalRelease,
            AcpOperation::TerminalOutput,
            AcpOperation::TerminalWaitForExit,
        ] {
            assert_eq!(stability(&operation), AcpOperationStability::StableCore);
        }
        assert_eq!(
            stability(&AcpOperation::SessionLoad),
            AcpOperationStability::CapabilityGated
        );
        assert_eq!(
            stability(&AcpOperation::SessionResume),
            AcpOperationStability::CapabilityGated
        );
        assert_eq!(
            stability(&AcpOperation::SessionSetMode),
            AcpOperationStability::CapabilityGated
        );
        for operation in [
            AcpOperation::SessionFork,
            AcpOperation::SessionSetConfigOption,
            AcpOperation::SessionSetModel,
        ] {
            assert_eq!(
                stability(&operation),
                AcpOperationStability::VersionedUnstable
            );
        }
        for entry in &matrix {
            assert_eq!(entry.source, CapabilitySource::FixedSchema);
            assert_eq!(entry.protocol_version.as_deref(), Some("1"));
        }
    }

    #[test]
    fn method_names_round_trip_through_operation_enum() {
        for entry in baseline_operation_matrix() {
            assert_eq!(
                AcpOperation::from_method(entry.operation.method()),
                entry.operation
            );
        }
        assert_eq!(
            AcpOperation::from_method("_claude/private"),
            AcpOperation::Extension("_claude/private".to_string())
        );
    }

    // §25.1: method-not-found downgrades exactly one operation.
    #[test]
    fn method_not_found_maps_to_single_operation_downgrade() {
        let error = classify_rpc_failure(
            AcpOperation::SessionSetModel,
            "-32601",
            "not found",
            AcpRpcFailureDataKind::Other,
        );
        assert_eq!(
            error,
            AcpProtocolError::MethodNotFound {
                operation: AcpOperation::SessionSetModel
            }
        );
        assert_eq!(error.kind(), "method_not_found");
        let other = classify_rpc_failure(
            AcpOperation::SessionSetModel,
            "-32000",
            "boom",
            AcpRpcFailureDataKind::Other,
        );
        assert_eq!(other.kind(), "agent_reported_error");
    }

    #[test]
    fn structured_missing_rollout_is_restore_resource_not_found() {
        let data = json!({
            "details": "no rollout found for thread id 019f0000-0000-0000-0000-000000000000"
        });
        let data_kind = classify_rpc_failure_data(Some(&data));
        assert_eq!(data_kind, AcpRpcFailureDataKind::ResourceNotFound);
        assert_eq!(
            classify_rpc_failure(
                AcpOperation::SessionResume,
                "-32603",
                "Internal error",
                data_kind,
            ),
            AcpProtocolError::ResourceNotFound {
                operation: AcpOperation::SessionResume
            }
        );
        assert_eq!(
            classify_rpc_failure(
                AcpOperation::SessionPrompt,
                "-32603",
                "Internal error",
                data_kind,
            )
            .kind(),
            "agent_reported_error"
        );
    }

    // §25.1: invalid responses have an explicit classification.
    #[test]
    fn invalid_response_shape_is_classified() {
        let error = AcpProtocolError::InvalidResponseShape {
            operation: AcpOperation::SessionNew,
            detail: "sessionId missing".to_string(),
        };
        assert_eq!(error.kind(), "invalid_response_shape");
    }

    // §25.1: raw envelope keeps unknown fields, redacted and bounded.
    #[test]
    fn raw_envelope_preserves_unknown_fields_and_redacts_sensitive_keys() {
        let message = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "native-1",
                "unknownFutureField": { "keep": "me" },
                "apiKey": "sk-super-secret",
                "nested": { "authToken": "abc", "plain": "ok" }
            }
        });
        let envelope = BoundedRawAcpEnvelope::capture(
            AcpMessageDirection::Incoming,
            &message,
            DEFAULT_RAW_ENVELOPE_LIMIT_BYTES,
        );
        assert_eq!(envelope.method.as_deref(), Some("session/update"));
        assert_eq!(envelope.native_session_id.as_deref(), Some("native-1"));
        assert!(!envelope.truncated);
        let body: Value = serde_json::from_str(&envelope.redacted_body).unwrap();
        assert_eq!(body["params"]["unknownFutureField"]["keep"], "me");
        assert_eq!(body["params"]["apiKey"], "[redacted]");
        assert_eq!(body["params"]["nested"]["authToken"], "[redacted]");
        assert_eq!(body["params"]["nested"]["plain"], "ok");
        assert!(!envelope.redacted_body.contains("sk-super-secret"));
    }

    #[test]
    fn raw_envelope_masks_home_paths() {
        // SAFETY: test-scoped env mutation.
        unsafe { std::env::set_var("HOME", "/home/vibex-test-user") };
        let message = json!({
            "method": "fs/read_text_file",
            "params": { "path": "/home/vibex-test-user/project/file.rs" }
        });
        let envelope = redact_envelope(&message);
        assert_eq!(envelope["params"]["path"], "~/project/file.rs");
    }

    #[test]
    fn raw_envelope_truncates_with_marker_and_byte_len() {
        let message = json!({
            "method": "session/update",
            "params": { "sessionId": "native-1", "blob": "x".repeat(64 * 1024) }
        });
        let envelope = BoundedRawAcpEnvelope::capture(AcpMessageDirection::Incoming, &message, 512);
        assert!(envelope.truncated);
        assert!(envelope.redacted_body.len() <= 512);
        assert!(envelope.byte_len > 64 * 1024);
        // Identity fields survive truncation.
        assert_eq!(envelope.method.as_deref(), Some("session/update"));
        assert_eq!(envelope.native_session_id.as_deref(), Some("native-1"));
    }

    // §25.1: typed decode preserves raw and classifies extensions.
    #[test]
    fn decode_incoming_classifies_standard_extension_and_malformed() {
        let standard = decode_incoming(&json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": "native-1", "update": {} }
        }));
        match standard.decoded {
            AcpDecodedPayload::Standard(message) => {
                assert_eq!(message.operation, AcpOperation::SessionUpdate);
                assert_eq!(message.params["sessionId"], "native-1");
            }
            other => panic!("expected standard payload, got {other:?}"),
        }

        let extension = decode_incoming(&json!({
            "jsonrpc": "2.0",
            "method": "_claude/private_stream",
            "params": { "sessionId": "native-1", "blob": true }
        }));
        match extension.decoded {
            AcpDecodedPayload::Extension { method, params } => {
                assert_eq!(method, "_claude/private_stream");
                assert_eq!(params["blob"], true);
            }
            other => panic!("expected extension payload, got {other:?}"),
        }
        assert_eq!(
            extension.raw.method.as_deref(),
            Some("_claude/private_stream")
        );

        let malformed = decode_incoming(&json!({ "jsonrpc": "2.0", "params": {} }));
        match malformed.decoded {
            AcpDecodedPayload::Malformed { error, .. } => {
                assert_eq!(error, AcpProtocolError::DecodeFailure);
            }
            other => panic!("expected malformed payload, got {other:?}"),
        }
    }
}
