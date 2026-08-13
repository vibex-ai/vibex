#![forbid(unsafe_code)]

#[cfg(target_family = "wasm")]
mod mobile {
    use std::borrow::Cow;
    use std::cell::RefCell;
    use std::sync::Arc;

    use gpui::{
        AnyElement, AnyWindowHandle, App, AppContext as _, ApplicationHandle, Entity, IntoElement,
        ParentElement as _, Render, Styled as _, Window, WindowOptions, div,
    };
    use gpui_component::{Root, Theme, WindowExt as _};
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use vibex_backend::{BackendError, BackendErrorKind, BackendResult, DisconnectedBackend};
    use vibex_core::{DeviceId, RemoteAuthProof, RemoteClientType, RemoteDeviceStatus, RequestId};
    use vibex_remote_client::{
        ActiveRemoteRoute, AutoRemoteTransport, AutoRemoteTransportConfig, ClientDeviceIdentity,
        DirectCandidate, DirectWebSocketTransport, PairingClaimRoute, PairingEntryHint,
        RelayClientConfig, RemoteClientConfig, RemoteConnectionState, RemoteCredentialRecord,
        RemoteLifecycleSignal, WebRemoteBackend, claim_pairing_offer,
        claim_pairing_offer_via_relay, pairing_claim_request, parse_pairing_offer_fragment,
        select_pairing_claim_route,
    };
    use vibex_ui::browser_gate::{
        ApplyHostEvent, BROWSER_GATE_SCHEMA_VERSION, BrowserGateView, BrowserHostEvent,
        BrowserHostEventEnvelope, BrowserHostSnapshot, MEDIUM_MIN_WIDTH, apply_browser_gate_theme,
        apply_browser_gate_theme_mode,
    };
    use vibex_ui::{
        AGENT_FILE_GIT_WORKFLOW_SCHEMA_VERSION, AgentFileGitCapabilities, CompactNavigation,
        GlobalDestination, INTERFACE_TYPOGRAPHY, MANAGEMENT_WORKFLOW_SCHEMA_VERSION,
        ManagementWorkflowCapabilities, OverlaySemantic, SessionDestination, ShellKind,
        TERMINAL_RAW_BYTE_BUDGET, TERMINAL_WORKFLOW_SCHEMA_VERSION, TerminalWorkflowCapabilities,
        WORKFLOW_WORKBENCH_SCHEMA_VERSION, WorkbenchConnectionState, WorkflowWorkbenchCommand,
        WorkflowWorkbenchView,
    };
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::future_to_promise;

    const MAX_PENDING_EVENTS: usize = 64;
    const MAX_REMOTE_CREDENTIAL_BYTES: usize = 64 * 1024;
    const MAX_REMOTE_COMMAND_BYTES: usize = 8 * 1024;
    const MAX_WORKFLOW_COMMAND_BYTES: usize = 1024;
    const REMOTE_RUNTIME_SCHEMA_VERSION: &str = "vibex-mobile-runtime.v1";
    const REMOTE_CREDENTIAL_SCHEMA_VERSION: &str = "vibex-remote-client-credentials.v1";
    const PAIRING_PREVIEW_SCHEMA_VERSION: &str = "vibex-pairing-preview.v1";
    const PAIRING_CLAIM_SCHEMA_VERSION: &str = "vibex-pairing-claim.v1";
    const INTER_LATIN: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/fonts/inter-latin-wght-normal.ttf"
    ));
    const INTER_LATIN_EXT: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/fonts/inter-latin-ext-wght-normal.ttf"
    ));
    const CJK_FALLBACK: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/fonts/wqy-microhei.ttc"
    ));

    thread_local! {
        static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
        static WINDOW_HANDLE: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
        static ROOT_VIEW: RefCell<Option<Entity<MobileRootView>>> = const { RefCell::new(None) };
        static GATE_VIEW: RefCell<Option<Entity<BrowserGateView>>> = const { RefCell::new(None) };
        static WORKBENCH_VIEW: RefCell<Option<Entity<WorkflowWorkbenchView>>> = const { RefCell::new(None) };
        static HOST_SNAPSHOT: RefCell<BrowserHostSnapshot> = RefCell::new(BrowserHostSnapshot::default());
        static PENDING_EVENTS: RefCell<Vec<BrowserHostEventEnvelope>> = const { RefCell::new(Vec::new()) };
        static RESOLVED_INTERFACE_FONT: RefCell<Option<String>> = const { RefCell::new(None) };
        static LAST_PLATFORM_BACK: RefCell<&'static str> = const { RefCell::new("unhandled") };
        static REMOTE_RUNTIME: RefCell<Option<RemoteRuntime>> = const { RefCell::new(None) };
    }

    struct RemoteRuntime {
        backend: Arc<WebRemoteBackend>,
        expected_server_id: String,
        client_type: RemoteClientType,
        route: RemoteRuntimeRoute,
    }

    enum RemoteRuntimeRoute {
        Direct,
        Auto(AutoRemoteTransport),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    enum MobileRootMode {
        Workbench,
        GateDiagnostics,
    }

    struct MobileRootView {
        gate: Entity<BrowserGateView>,
        workbench: Entity<WorkflowWorkbenchView>,
        mode: MobileRootMode,
    }

    impl MobileRootView {
        fn new(
            gate: Entity<BrowserGateView>,
            workbench: Entity<WorkflowWorkbenchView>,
            gate_diagnostics: bool,
        ) -> Self {
            Self {
                gate,
                workbench,
                mode: if gate_diagnostics {
                    MobileRootMode::GateDiagnostics
                } else {
                    MobileRootMode::Workbench
                },
            }
        }

        fn activate(
            &mut self,
            workbench: Entity<WorkflowWorkbenchView>,
            cx: &mut gpui::Context<Self>,
        ) {
            self.workbench = workbench;
            self.mode = MobileRootMode::Workbench;
            cx.notify();
        }

        fn mode(&self) -> MobileRootMode {
            self.mode
        }
    }

    impl Render for MobileRootView {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let content: AnyElement = match self.mode {
                MobileRootMode::Workbench => self.workbench.clone().into_any_element(),
                MobileRootMode::GateDiagnostics => self.gate.clone().into_any_element(),
            };
            div().size_full().child(content)
        }
    }

    impl RemoteRuntime {
        fn new(
            config: RemoteClientConfig,
            expected_server_id: String,
            route: Option<&MobileRemoteRouteBundle>,
        ) -> BackendResult<Self> {
            let client_type = config.client_type;
            let (backend, route) = if let Some(route) = route {
                let direct_candidates = route
                    .direct_candidates
                    .iter()
                    .enumerate()
                    .map(|(index, url)| DirectCandidate {
                        url: url.clone(),
                        label: format!("direct-{index}"),
                        priority: u8::try_from(index).unwrap_or(u8::MAX),
                    })
                    .collect();
                let relay = route.relay.as_ref().map(|relay| RelayClientConfig {
                    relay_url: relay.url.clone(),
                    room_id: relay.room_id.clone(),
                    local_peer_id: relay.local_peer_id.clone(),
                    pc_peer_id: relay.pc_peer_id.clone(),
                    pc_public_key: Some(relay.pc_public_key.clone()),
                    remote: config.clone(),
                });
                let transport = AutoRemoteTransport::new(AutoRemoteTransportConfig {
                    remote: config,
                    direct_candidates,
                    relay,
                })?;
                (
                    Arc::new(WebRemoteBackend::from_auto(transport.clone())),
                    RemoteRuntimeRoute::Auto(transport),
                )
            } else {
                (
                    Arc::new(WebRemoteBackend::from_direct(
                        DirectWebSocketTransport::new(config)?,
                    )),
                    RemoteRuntimeRoute::Direct,
                )
            };
            Ok(Self {
                backend,
                expected_server_id,
                client_type,
                route,
            })
        }

        fn active_route(&self) -> ActiveRemoteRoute {
            match &self.route {
                RemoteRuntimeRoute::Direct => ActiveRemoteRoute::Direct,
                RemoteRuntimeRoute::Auto(transport) => transport.active_route(),
            }
        }
    }

    #[derive(Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct MobileRemoteCredentialBundle {
        schema_version: String,
        record: RemoteCredentialRecord,
        identity_private_key: String,
        expected_server_id: String,
        client_type: RemoteClientType,
        #[serde(default)]
        allow_insecure_local_dev: bool,
        #[serde(default)]
        route: Option<MobileRemoteRouteBundle>,
    }

    #[derive(Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct MobileRemoteRouteBundle {
        #[serde(default)]
        direct_candidates: Vec<String>,
        relay: Option<MobileRelayCandidate>,
    }

    #[derive(Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct MobileRelayCandidate {
        url: String,
        room_id: vibex_core::RelayRoomId,
        local_peer_id: vibex_core::RelayPeerId,
        pc_peer_id: vibex_core::RelayPeerId,
        pc_public_key: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct PairingClaimOptions {
        display_name: String,
        #[serde(default = "default_mobile_client_type")]
        client_type: RemoteClientType,
        #[serde(default)]
        allow_insecure_local_dev: bool,
        #[serde(default)]
        now_ms: Option<i64>,
        entry_hint: PairingEntryHint,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct DeepLinkResolveCommand {
        notification_id: String,
        opaque_locator: String,
    }

    fn default_mobile_client_type() -> RemoteClientType {
        RemoteClientType::Mobile
    }

    #[derive(Deserialize)]
    #[serde(
        tag = "kind",
        rename_all = "snake_case",
        rename_all_fields = "camelCase"
    )]
    enum RemoteLifecycleCommand {
        VisibilityChanged { visible: bool },
        NetworkChanged,
        ComputerSuspended,
        ComputerResumed,
        AppBackgrounded,
        AppResumed,
    }

    impl From<RemoteLifecycleCommand> for RemoteLifecycleSignal {
        fn from(value: RemoteLifecycleCommand) -> Self {
            match value {
                RemoteLifecycleCommand::VisibilityChanged { visible } => {
                    Self::VisibilityChanged { visible }
                }
                RemoteLifecycleCommand::NetworkChanged => Self::NetworkChanged,
                RemoteLifecycleCommand::ComputerSuspended => Self::ComputerSuspended,
                RemoteLifecycleCommand::ComputerResumed => Self::ComputerResumed,
                RemoteLifecycleCommand::AppBackgrounded => Self::AppBackgrounded,
                RemoteLifecycleCommand::AppResumed => Self::AppResumed,
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(
        tag = "kind",
        rename_all = "snake_case",
        rename_all_fields = "camelCase"
    )]
    enum NavigationCommand {
        EnterSession {
            session_id: String,
        },
        SelectGlobal {
            destination: GlobalDestination,
        },
        SelectSession {
            destination: SessionDestination,
        },
        OpenOverlay {
            semantic: OverlaySemantic,
            shell: ShellKind,
            restore_focus: String,
        },
        CloseOverlay,
        Reset,
    }

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(catch, js_name = __vibexGpuiRuntimeBooted)]
        fn runtime_booted() -> Result<(), JsValue>;

        #[wasm_bindgen(catch, js_name = __vibexGpuiRuntimeFailed)]
        fn runtime_failed(message: &str) -> Result<(), JsValue>;
    }

    #[wasm_bindgen]
    pub fn start(gate_diagnostics: bool) -> Result<(), JsValue> {
        let already_started = APPLICATION.with(|application| application.borrow().is_some());
        if already_started {
            return Err(js_error("GPUI application has already started"));
        }

        gpui_platform::web_init();
        let application = gpui_platform::single_threaded_web();
        let handle = application.run_embedded(move |cx: &mut App| {
            if let Err(error) = cx.text_system().add_fonts(vec![
                Cow::Borrowed(INTER_LATIN),
                Cow::Borrowed(INTER_LATIN_EXT),
                Cow::Borrowed(CJK_FALLBACK),
            ]) {
                let message = format!("failed to load shared Inter fonts: {error:#}");
                let _ = runtime_failed(&message);
                return;
            }
            let font_names = cx.text_system().all_font_names();
            let resolved_font = [INTERFACE_TYPOGRAPHY.family, "Inter"]
                .into_iter()
                .find(|candidate| font_names.iter().any(|font| font == candidate));
            let Some(resolved_font) = resolved_font else {
                let _ = runtime_failed("shared Inter font family was not registered");
                return;
            };
            gpui_component::init(cx);
            apply_browser_gate_theme(cx);
            Theme::global_mut(cx).font_family = resolved_font.to_string().into();
            RESOLVED_INTERFACE_FONT
                .with(|font| *font.borrow_mut() = Some(resolved_font.to_string()));

            let window = cx.open_window(WindowOptions::default(), |window, cx| {
                let gate = cx.new(|cx| BrowserGateView::new(window, cx));
                apply_pending_events(&gate, cx);
                GATE_VIEW.with(|gate_view| *gate_view.borrow_mut() = Some(gate.clone()));
                let host = HOST_SNAPSHOT.with(|snapshot| snapshot.borrow().clone());
                let workbench = cx.new(|cx| {
                    let mut view =
                        WorkflowWorkbenchView::new(DisconnectedBackend::facade(), window, cx);
                    view.apply_browser_host_snapshot(&host, cx);
                    view.set_connection_state(WorkbenchConnectionState::Offline, cx);
                    view
                });
                WORKBENCH_VIEW.with(|slot| *slot.borrow_mut() = Some(workbench.clone()));
                let root_view = cx.new(|_| MobileRootView::new(gate, workbench, gate_diagnostics));
                ROOT_VIEW.with(|slot| *slot.borrow_mut() = Some(root_view.clone()));
                cx.new(|cx| Root::new(root_view, window, cx).bordered(false))
            });

            match window {
                Ok(window) => {
                    WINDOW_HANDLE.with(|handle| *handle.borrow_mut() = Some(window.into()));
                    cx.activate(true);
                    let _ = runtime_booted();
                }
                Err(error) => {
                    let message = format!("failed to open the mobile runtime window: {error:#}");
                    let _ = runtime_failed(&message);
                }
            }
        });

        APPLICATION.with(|application| *application.borrow_mut() = Some(handle));
        Ok(())
    }

    #[wasm_bindgen]
    pub fn host_event(payload: &str) -> Result<String, JsValue> {
        let envelope: BrowserHostEventEnvelope = serde_json::from_str(payload)
            .map_err(|error| js_error(format!("invalid host event: {error}")))?;

        let result = HOST_SNAPSHOT.with(|snapshot| snapshot.borrow_mut().apply(envelope.clone()));
        if result == ApplyHostEvent::Applied {
            if !dispatch_to_view(envelope.clone())? {
                PENDING_EVENTS.with(|pending| {
                    let pending = &mut *pending.borrow_mut();
                    if pending.len() == MAX_PENDING_EVENTS {
                        pending.remove(0);
                    }
                    pending.push(envelope);
                });
            }
        }

        serde_json::to_string(&result)
            .map_err(|error| js_error(format!("failed to serialize host event result: {error}")))
    }

    #[wasm_bindgen]
    pub fn host_snapshot() -> Result<String, JsValue> {
        HOST_SNAPSHOT.with(|snapshot| {
            serde_json::to_string(&*snapshot.borrow())
                .map_err(|error| js_error(format!("failed to serialize host snapshot: {error}")))
        })
    }

    #[wasm_bindgen]
    pub fn gate_contract() -> Result<String, JsValue> {
        serde_json::to_string(&json!({
            "schemaVersion": BROWSER_GATE_SCHEMA_VERSION,
            "runtime": {
                "dispatcher": "single_threaded_web",
                "applicationHandleOwner": "thread_local_runtime",
                "sharedArrayBufferRequired": false
            },
            "breakpoints": {
                "mediumMinWidth": MEDIUM_MIN_WIDTH,
                "compactMinWidth": 0,
                "maximumShell": "medium"
            },
            "network": {
                "maxProbeResponseBytes": 4096,
                "largeResponsePolicy": "stream_or_native_bridge_required"
            },
            "performanceBudgets": {
                "firstFrameMs": 5000,
                "frameP95Ms": 50,
                "inputPresentationMs": 500,
                "timelineRows": 48
            },
            "accessibility": {
                "gpuiSemanticRolesConfigured": true,
                "webAccessibilityAdapterAvailable": false,
                "releaseBlocking": true
            },
            "compatibility": {
                "schemaVersion": "vibex-platform-compat.v1",
                "owner": "apps/mobile-wasm",
                "behaviorGated": true,
                "inputEventFallbackRequired": true,
                "touchScrollFallbackRequired": true,
                "upstreamObservedRevision": "82766871481bfc126f8fd42a3b9f51afc9b2decd",
                "removeWhen": {
                    "inputEventCommittedByGpuiWeb": true,
                    "touchPointerProducesGpuiScrollPhases": true
                }
            }
        }))
        .map_err(|error| js_error(format!("failed to serialize gate contract: {error}")))
    }

    /// Public host contract for the shared remote client.  The actual
    /// credential-bearing backend is created by the Capacitor host bridge;
    /// exposing the state vocabulary here keeps lifecycle/error pages on the
    /// same Rust contract without persisting a socket or session key in UI
    /// state.
    #[wasm_bindgen]
    pub fn remote_contract() -> Result<String, JsValue> {
        serde_json::to_string(&json!({
            "transport": "direct_websocket_v2",
            "relay": {
                "transport": "e2ee_websocket_v2",
                "route": "self_hosted_only",
                "fallback": "direct_probe_then_relay",
                "push": "optional_opaque_provider",
                "deepLink": "short_lived_opaque_locator_authoritative_fetch",
                "officialEndpoint": false
            },
            "secureContext": "https_wss_required_with_loopback_dev_exception",
            "states": [
                RemoteConnectionState::Idle,
                RemoteConnectionState::Resolving,
                RemoteConnectionState::Probing,
                RemoteConnectionState::Connecting,
                RemoteConnectionState::Authenticating,
                RemoteConnectionState::Syncing,
                RemoteConnectionState::Online,
                RemoteConnectionState::Degraded,
                RemoteConnectionState::Reconnecting,
                RemoteConnectionState::Offline,
                RemoteConnectionState::Revoked,
                RemoteConnectionState::Incompatible,
            ],
            "binary": {
                "terminal": "raw_bytes_sequence_generation",
                "file": "chunk_offset_checksum_cancel_bounded"
            },
            "sharedWorkflow": {
                "schemaVersion": AGENT_FILE_GIT_WORKFLOW_SCHEMA_VERSION,
                "workbenchSchemaVersion": WORKFLOW_WORKBENCH_SCHEMA_VERSION,
                "domains": ["agent", "files", "git"],
                "backend": "web_remote_facade",
                "fileEditing": {
                    "maxUtf8Bytes": 1048576,
                    "revisionCas": true,
                    "dangerousMutations": "capability_unavailable"
                },
                "gitReview": {
                    "operations": ["status", "diff", "stage", "unstage", "confirmed_commit"],
                    "pushRevertBranchRewrite": "capability_unavailable"
                }
            },
            "terminalWorkflow": {
                "schemaVersion": TERMINAL_WORKFLOW_SCHEMA_VERSION,
                "rawFrameProtocol": "sequence_generation_rebuild_required",
                "rawByteBudget": TERMINAL_RAW_BYTE_BUDGET,
                "localShell": "never",
                "compactKeyBar": ["Esc", "Ctrl", "Tab", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Enter", "Backspace"],
                "keyboardViewport": "safe_area_keyboard_inset_visible_height",
                "recoveryStates": ["reconnecting", "rebuilding", "closed", "offline"]
            },
            "managementWorkflow": {
                "schemaVersion": MANAGEMENT_WORKFLOW_SCHEMA_VERSION,
                "sections": ["overview", "providers", "health", "relay", "devices"],
                "redactedProviderProfiles": true,
                "profileSelection": "backend_capability_gated",
                "pairing": "short_lived_server_offer",
                "excluded": ["provider_secrets", "mcp_import", "skills_import", "scheduled", "automation", "backup_restore"]
            },
            "hostBridge": {
                "schemaVersion": "vibex-host-services.v1",
                "capabilities": ["safe_area", "keyboard", "lifecycle", "network", "secure_storage", "deep_link", "camera", "qr_scanner", "file_picker", "share", "system_url"],
                "businessState": "rust_only",
                "missingCapabilityPolicy": "unsupported_or_degraded"
            },
            "navigation": {
                "schemaVersion": "vibex-compact-navigation.v1",
                "levels": ["global", "session"],
                "global": ["sessions", "management", "settings"],
                "session": ["agent", "files", "changes", "terminal"],
                "backOrder": ["dialog", "sheet", "compact_navigation", "unhandled"]
            },
            "deployment": {
                "runtimeRole": "capacitor_mobile_runtime",
                "browserHost": "development_and_test_only",
                "capacitorWebDir": "../mobile-wasm/dist"
            },
            "authority": "desktop_runtime"
        }))
        .map_err(|error| js_error(format!("failed to serialize remote contract: {error}")))
    }

    /// Configure the one direct remote runtime.  The payload is a validated,
    /// redacted-at-rest bundle supplied by the host bridge; private material is
    /// never returned by `remote_state` or written to the host diagnostic event
    /// stream.
    #[wasm_bindgen]
    pub fn configure_remote(payload: &str) -> Result<String, JsValue> {
        let bundle = parse_credential_bundle(payload).map_err(|error| backend_js_error(&error))?;
        let expected_server_id = bundle.expected_server_id.clone();
        let config =
            client_config_from_bundle(&bundle).map_err(|error| backend_js_error(&error))?;
        let runtime = RemoteRuntime::new(config, expected_server_id, bundle.route.as_ref())
            .map_err(|error| backend_js_error(&error))?;
        REMOTE_RUNTIME.with(|slot| *slot.borrow_mut() = Some(runtime));
        remote_state_json()
    }

    /// Start the direct WebSocket connection and resolve with a sanitized
    /// runtime snapshot.  A rejected Promise contains a typed BackendError,
    /// never the credential bundle or pairing challenge.
    #[wasm_bindgen]
    pub fn connect_remote() -> js_sys::Promise {
        let backend = REMOTE_RUNTIME.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|runtime| runtime.backend.clone())
        });
        let Some(backend) = backend else {
            return rejected_promise(&BackendError::failed(
                "remote_runtime_not_configured",
                "remote runtime has not been configured",
            ));
        };

        future_to_promise(async move {
            let result = backend.connect().await;
            match result {
                Ok(_) => {
                    activate_workbench(backend.clone())?;
                    remote_state_json().map(|state| JsValue::from_str(&state))
                }
                Err(error) => {
                    let _ = set_workbench_connection(connection_state_for_error(&error));
                    Err(backend_js_error(&error))
                }
            }
        })
    }

    #[wasm_bindgen]
    pub fn disconnect_remote() -> js_sys::Promise {
        let backend = REMOTE_RUNTIME.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|runtime| runtime.backend.clone())
        });
        let Some(backend) = backend else {
            return future_to_promise(async {
                Ok(JsValue::from_str(
                    &remote_state_json().unwrap_or_else(|_| "{}".to_string()),
                ))
            });
        };
        future_to_promise(async move {
            backend
                .disconnect()
                .await
                .map_err(|error| backend_js_error(&error))?;
            set_workbench_connection(WorkbenchConnectionState::Offline)?;
            remote_state_json().map(|state| JsValue::from_str(&state))
        })
    }

    /// Drop the configured runtime after the host has removed this origin's
    /// credential. Callers disconnect first; this function never deletes
    /// storage or affects another paired device.
    #[wasm_bindgen]
    pub fn forget_remote() -> Result<String, JsValue> {
        REMOTE_RUNTIME.with(|slot| *slot.borrow_mut() = None);
        set_workbench_connection(WorkbenchConnectionState::Offline)?;
        remote_state_json()
    }

    #[wasm_bindgen]
    pub fn remote_state() -> Result<String, JsValue> {
        sync_workbench_connection()?;
        remote_state_json()
    }

    /// Apply a lifecycle/network signal to the transport.  Reconnect work is
    /// deliberately owned by the transport, so the host does not implement a
    /// second retry loop.
    #[wasm_bindgen]
    pub fn remote_lifecycle(payload: &str) -> Result<String, JsValue> {
        if payload.len() > MAX_REMOTE_COMMAND_BYTES {
            return Err(js_error("remote lifecycle command is too large"));
        }
        let command: RemoteLifecycleCommand = serde_json::from_str(payload)
            .map_err(|error| js_error(format!("invalid remote lifecycle command: {error}")))?;
        let signal = command.into();
        REMOTE_RUNTIME.with(|slot| {
            let binding = slot.borrow();
            let runtime = binding
                .as_ref()
                .ok_or_else(|| js_error("remote runtime has not been configured"))?;
            runtime.backend.transport().apply_lifecycle_signal(signal);
            Ok::<(), JsValue>(())
        })?;
        sync_workbench_connection()?;
        remote_state_json()
    }

    /// Ask the authenticated PC to resolve an opaque push/deep-link locator.
    /// The mobile host never interprets the locator or invents a session id.
    #[wasm_bindgen]
    pub fn resolve_deep_link(payload: &str) -> js_sys::Promise {
        if payload.len() > MAX_REMOTE_COMMAND_BYTES {
            return js_sys::Promise::reject(&js_error("deep-link resolve command is too large"));
        }
        let command: DeepLinkResolveCommand = match serde_json::from_str(payload) {
            Ok(command) => command,
            Err(_) => {
                return js_sys::Promise::reject(&js_error("deep-link resolve command is invalid"));
            }
        };
        if command.notification_id.trim().is_empty()
            || command.notification_id.len() > 256
            || command
                .notification_id
                .chars()
                .any(|character| character.is_control())
            || command.opaque_locator.trim().is_empty()
            || command.opaque_locator.len() > 512
            || command
                .opaque_locator
                .chars()
                .any(|character| character.is_control())
        {
            return js_sys::Promise::reject(&js_error("deep-link locator is invalid"));
        }
        let agent = REMOTE_RUNTIME.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|runtime| runtime.backend.facade().agent().clone())
        });
        let Some(agent) = agent else {
            return js_sys::Promise::reject(&js_error("remote runtime has not been configured"));
        };
        future_to_promise(async move {
            let resolution = agent
                .resolve_opaque_locator(command.notification_id, command.opaque_locator)
                .await
                .map_err(|error| backend_js_error(&error))?;
            serde_json::to_string(&resolution)
                .map(|value| JsValue::from_str(&value))
                .map_err(|_| js_error("deep-link resolution could not be encoded"))
        })
    }

    #[wasm_bindgen]
    pub fn pairing_preview(fragment: &str, now_ms: f64) -> Result<String, JsValue> {
        let now_ms = normalize_now_ms(now_ms).map_err(|error| backend_js_error(&error))?;
        let offer = parse_pairing_offer_fragment(fragment, now_ms)
            .map_err(|error| backend_js_error(&error))?;
        serde_json::to_string(&json!({
            "schemaVersion": PAIRING_PREVIEW_SCHEMA_VERSION,
            "desktopIdentity": redacted_identity(&offer.summary.server_id),
            "expiresAtMs": offer.summary.expires_at_ms,
            "permissionLevel": offer.summary.permission_level,
            "grantedPermissions": offer.summary.granted_permissions,
            "hasDirectCandidate": offer.summary.direct_candidates.iter().any(|candidate| candidate.transport == vibex_core::RemotePairingTransport::Direct),
            "hasTailnetCandidate": offer.summary.direct_candidates.iter().any(|candidate| candidate.transport == vibex_core::RemotePairingTransport::Tailnet),
            "hasRelayCandidate": offer.summary.relay_candidate.is_some(),
        }))
        .map_err(|error| js_error(format!("failed to serialize pairing preview: {error}")))
    }

    /// Claim a one-shot pairing fragment.  The caller must persist the
    /// returned `credentials` object in Secure Storage before configuring or
    /// connecting the runtime.  No challenge is present in the returned JSON.
    #[wasm_bindgen]
    pub fn claim_pairing_fragment(fragment: &str, options: &str) -> js_sys::Promise {
        let parsed_options: Result<PairingClaimOptions, JsValue> =
            if options.len() > MAX_REMOTE_COMMAND_BYTES {
                Err(js_error("pairing claim options are too large"))
            } else {
                serde_json::from_str(options)
                    .map_err(|error| js_error(format!("invalid pairing claim options: {error}")))
            };
        let parsed_options = match parsed_options {
            Ok(options) => options,
            Err(error) => return js_sys::Promise::reject(&error),
        };
        let now_ms = parsed_options
            .now_ms
            .unwrap_or_else(|| js_sys::Date::now().max(0.0) as i64);
        let offer = match parse_pairing_offer_fragment(fragment, now_ms) {
            Ok(offer) => offer,
            Err(error) => return rejected_promise(&error),
        };
        if parsed_options.client_type != RemoteClientType::Mobile {
            return rejected_promise(&BackendError::unsupported(
                "remote_client_type_unsupported",
                "the mobile runtime only accepts the mobile client type",
            ));
        }

        let provisional_identity = match ClientDeviceIdentity::generate(DeviceId::new()) {
            Ok(identity) => identity,
            Err(error) => return rejected_promise(&error),
        };
        let claim_nonce = RequestId::new().into_string();
        let request = match pairing_claim_request(
            &offer,
            &parsed_options.display_name,
            provisional_identity.public_key_base64(),
            claim_nonce,
        ) {
            Ok(request) => request,
            Err(error) => return rejected_promise(&error),
        };
        let claim_route = match select_pairing_claim_route(&offer, &parsed_options.entry_hint) {
            Ok(route) => route,
            Err(error) => return rejected_promise(&error),
        };
        let client_type = parsed_options.client_type;
        let allow_insecure_local_dev = parsed_options.allow_insecure_local_dev;
        let private_key = provisional_identity.private_key_base64();

        future_to_promise(async move {
            let response = match &claim_route {
                PairingClaimRoute::Direct { claim_base_url, .. } => {
                    claim_pairing_offer(
                        claim_base_url.clone(),
                        request.clone(),
                        allow_insecure_local_dev,
                    )
                    .await
                }
                PairingClaimRoute::Relay(relay) => {
                    claim_pairing_offer_via_relay(
                        relay.url.clone(),
                        relay.relay_room_id.clone().ok_or_else(|| {
                            backend_js_error(&BackendError::failed(
                                "remote_pairing_relay_candidate_invalid",
                                "Relay candidate omitted its room id",
                            ))
                        })?,
                        vibex_core::RelayPeerId::new(),
                        relay.relay_pc_peer_id.clone().ok_or_else(|| {
                            backend_js_error(&BackendError::failed(
                                "remote_pairing_relay_candidate_invalid",
                                "Relay candidate omitted its PC peer id",
                            ))
                        })?,
                        relay.relay_pc_public_key.clone().ok_or_else(|| {
                            backend_js_error(&BackendError::failed(
                                "remote_pairing_relay_candidate_invalid",
                                "Relay candidate omitted its PC identity",
                            ))
                        })?,
                        request.clone(),
                        provisional_identity.clone(),
                        allow_insecure_local_dev,
                    )
                    .await
                }
            }
            .map_err(|error| backend_js_error(&error))?;
            if response.device.status != RemoteDeviceStatus::Active
                || response.device.public_key.as_deref()
                    != Some(request.device_identity_public_key.as_str())
                || response.device_grant_token.trim().is_empty()
            {
                return Err(backend_js_error(&BackendError::failed(
                    "remote_pairing_claim_response_invalid",
                    "pairing claim response did not contain the expected active device grant",
                )));
            }
            let identity = ClientDeviceIdentity::from_private_key_base64(
                response.device.device_id.clone(),
                &private_key,
            )
            .map_err(|error| backend_js_error(&error))?;
            let record = RemoteCredentialRecord {
                server_url: match &claim_route {
                    PairingClaimRoute::Direct { claim_base_url, .. } => claim_base_url.clone(),
                    PairingClaimRoute::Relay(relay) => relay.url.clone(),
                },
                auth: RemoteAuthProof {
                    device_id: response.device.device_id.clone(),
                    auth_token: response.device_grant_token.clone(),
                },
                device_identity_public_key: identity.public_key_base64(),
                server_identity_public_key: Some(offer.summary.server_identity_public_key.clone()),
            };
            let bundle = MobileRemoteCredentialBundle {
                schema_version: REMOTE_CREDENTIAL_SCHEMA_VERSION.to_string(),
                record,
                identity_private_key: identity.private_key_base64(),
                expected_server_id: offer.summary.server_id.clone(),
                client_type,
                allow_insecure_local_dev,
                route: Some(route_bundle_from_offer(&offer)),
            };
            client_config_from_bundle(&bundle).map_err(|error| backend_js_error(&error))?;
            let value = json!({
                "schemaVersion": PAIRING_CLAIM_SCHEMA_VERSION,
                "credentials": bundle,
                "device": {
                    "deviceId": response.device.device_id,
                    "displayName": response.device.display_name,
                    "permissionLevel": response.device.permission_level,
                    "status": response.device.status,
                },
                "serverId": offer.summary.server_id,
                "sessionId": response.session_id,
            });
            serde_json::to_string(&value)
                .map(|value| JsValue::from_str(&value))
                .map_err(|error| js_error(format!("failed to serialize pairing claim: {error}")))
        })
    }

    #[wasm_bindgen]
    pub fn navigation_state() -> Result<String, JsValue> {
        navigation_state_json()
    }

    #[wasm_bindgen]
    pub fn navigation_action(payload: &str) -> Result<String, JsValue> {
        if payload.len() > MAX_REMOTE_COMMAND_BYTES {
            return Err(js_error("navigation command is too large"));
        }
        let command: NavigationCommand = serde_json::from_str(payload)
            .map_err(|error| js_error(format!("invalid navigation command: {error}")))?;
        let view = WORKBENCH_VIEW
            .with(|slot| slot.borrow().clone())
            .ok_or_else(|| js_error("workflow workbench is not connected"))?;
        APPLICATION.with(|application| {
            let application = application.borrow();
            let application = application
                .as_ref()
                .ok_or_else(|| js_error("GPUI application handle is unavailable"))?;
            application.update(|cx| {
                view.update(cx, |view, cx| {
                    match command {
                        NavigationCommand::EnterSession { session_id } => {
                            if session_id.trim().is_empty() || session_id.chars().count() > 256 {
                                return Err(js_error("session id is invalid"));
                            }
                            view.enter_session(&session_id, cx)
                                .map_err(|error| backend_js_error(&error))?;
                        }
                        NavigationCommand::SelectGlobal { destination } => {
                            view.select_global_destination(destination, cx)
                        }
                        NavigationCommand::SelectSession { destination } => {
                            view.select_session_destination(destination, cx)
                        }
                        NavigationCommand::OpenOverlay {
                            semantic,
                            shell,
                            restore_focus,
                        } => {
                            if restore_focus.trim().is_empty()
                                || restore_focus.chars().count() > 256
                            {
                                return Err(js_error("overlay focus target is invalid"));
                            }
                            view.navigation.open_overlay(semantic, shell, restore_focus);
                            cx.notify();
                        }
                        NavigationCommand::CloseOverlay => {
                            view.close_navigation_overlay(cx);
                        }
                        NavigationCommand::Reset => {
                            view.navigation = CompactNavigation::default();
                            cx.notify();
                        }
                    }
                    Ok::<(), JsValue>(())
                })
            })
        })?;
        navigation_state_json()
    }

    fn parse_credential_bundle(payload: &str) -> BackendResult<MobileRemoteCredentialBundle> {
        if payload.is_empty() || payload.len() > MAX_REMOTE_CREDENTIAL_BYTES {
            return Err(BackendError::failed(
                "remote_credentials_invalid",
                "remote credential bundle is empty or exceeds the bounded size",
            ));
        }
        let bundle: MobileRemoteCredentialBundle = serde_json::from_str(payload).map_err(|_| {
            BackendError::failed(
                "remote_credentials_invalid",
                "remote credential bundle is not valid JSON",
            )
        })?;
        if bundle.schema_version != REMOTE_CREDENTIAL_SCHEMA_VERSION
            || bundle.expected_server_id.trim().is_empty()
            || bundle.expected_server_id.chars().count() > 256
            || bundle.identity_private_key.is_empty()
            || bundle.record.server_identity_public_key.is_none()
            || bundle.record.auth.auth_token.trim().is_empty()
            || bundle.record.auth.auth_token.chars().count() > 4096
            || bundle.record.device_identity_public_key.trim().is_empty()
            || bundle.client_type != RemoteClientType::Mobile
        {
            return Err(BackendError::failed(
                "remote_credentials_invalid",
                "remote credential bundle failed its version, identity, or server pin checks",
            ));
        }
        Ok(bundle)
    }

    fn normalize_now_ms(value: f64) -> BackendResult<i64> {
        if !value.is_finite() || value < 0.0 || value > i64::MAX as f64 {
            return Err(BackendError::failed(
                "remote_timestamp_invalid",
                "pairing timestamp is outside the supported range",
            ));
        }
        Ok(value.floor() as i64)
    }

    fn redacted_identity(value: &str) -> String {
        let value = value.trim();
        if value.chars().count() <= 8 {
            return "Desktop".to_string();
        }
        let prefix = value.chars().take(4).collect::<String>();
        let suffix = value
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        format!("{prefix}...{suffix}")
    }

    fn client_config_from_bundle(
        bundle: &MobileRemoteCredentialBundle,
    ) -> BackendResult<RemoteClientConfig> {
        let identity = ClientDeviceIdentity::from_private_key_base64(
            bundle.record.auth.device_id.clone(),
            &bundle.identity_private_key,
        )?;
        if identity.public_key_base64() != bundle.record.device_identity_public_key {
            return Err(BackendError::permission(
                "remote_client_identity_mismatch",
                "stored client identity does not match the remote device grant",
            ));
        }
        let mut config = RemoteClientConfig::from_credentials(bundle.record.clone(), identity)?;
        config.expected_server_id = Some(bundle.expected_server_id.clone());
        config.client_type = bundle.client_type;
        config.client_id = "vibex-mobile".to_string();
        config.allow_insecure_local_dev = bundle.allow_insecure_local_dev && cfg!(debug_assertions);
        config.validate()?;
        Ok(config)
    }

    fn route_bundle_from_offer(offer: &vibex_core::RemotePairingOffer) -> MobileRemoteRouteBundle {
        let direct_candidates = offer
            .summary
            .direct_candidates
            .iter()
            .filter(|candidate| {
                matches!(
                    candidate.transport,
                    vibex_core::RemotePairingTransport::Direct
                        | vibex_core::RemotePairingTransport::Tailnet
                )
            })
            .map(|candidate| candidate.url.clone())
            .collect();
        let relay = offer
            .summary
            .relay_candidate
            .as_ref()
            .and_then(|candidate| {
                Some(MobileRelayCandidate {
                    url: candidate.url.clone(),
                    room_id: candidate.relay_room_id.clone()?,
                    local_peer_id: vibex_core::RelayPeerId::new(),
                    pc_peer_id: candidate.relay_pc_peer_id.clone()?,
                    pc_public_key: candidate.relay_pc_public_key.clone()?,
                })
            });
        MobileRemoteRouteBundle {
            direct_candidates,
            relay,
        }
    }

    fn remote_state_json() -> Result<String, JsValue> {
        let navigation = current_navigation();
        let value = REMOTE_RUNTIME.with(|slot| {
            let binding = slot.borrow();
            let Some(runtime) = binding.as_ref() else {
                return json!({
                    "schemaVersion": REMOTE_RUNTIME_SCHEMA_VERSION,
                    "configured": false,
                    "connection": { "state": RemoteConnectionState::Idle },
                    "navigation": navigation,
                });
            };
            let connection = runtime.backend.connection_state();
            let capabilities = runtime.backend.capability_snapshot();
            let workflow = AgentFileGitCapabilities::from_backend(&capabilities);
            let terminal = TerminalWorkflowCapabilities::from_backend(&capabilities);
            let management = ManagementWorkflowCapabilities::from_backend(&capabilities);
            json!({
                "schemaVersion": REMOTE_RUNTIME_SCHEMA_VERSION,
                "configured": true,
                "clientType": runtime.client_type,
                "expectedServerId": &runtime.expected_server_id,
                "connection": {
                    "state": connection.state,
                    "activeRoute": runtime.active_route(),
                    "sessionEpoch": connection.session_epoch,
                    "reconnectAttempt": connection.reconnect_attempt,
                    "nextRetryAtMs": connection.next_retry_at_ms,
                    "lastErrorCode": &connection.last_error_code,
                },
                "capabilities": capabilities,
                "workflows": {
                    "agentFileGit": workflow,
                    "terminal": {
                        "schemaVersion": terminal.schema_version,
                        "backendRevision": terminal.backend_revision,
                        "domain": terminal.domain,
                    },
                    "management": {
                        "schemaVersion": management.schema_version,
                        "backendRevision": management.backend_revision,
                        "management": management.management,
                        "device": management.device,
                    },
                },
                "navigation": navigation,
            })
        });
        serde_json::to_string(&value)
            .map_err(|error| js_error(format!("failed to serialize remote state: {error}")))
    }

    fn navigation_state_json() -> Result<String, JsValue> {
        let navigation = current_navigation();
        let value = json!({
            "schemaVersion": "vibex-compact-navigation.v1",
            "navigation": navigation,
            "actions": navigation.actions(),
        });
        serde_json::to_string(&value)
            .map_err(|error| js_error(format!("failed to serialize navigation state: {error}")))
    }

    fn rejected_promise(error: &BackendError) -> js_sys::Promise {
        let value = backend_js_error(error);
        js_sys::Promise::reject(&value)
    }

    fn backend_js_error(error: &BackendError) -> JsValue {
        serde_json::to_string(error)
            .map(|value| JsValue::from_str(&value))
            .unwrap_or_else(|_| js_error("remote operation failed"))
    }

    #[wasm_bindgen]
    pub fn workflow_state() -> Result<String, JsValue> {
        let snapshot = current_workbench_snapshot()
            .ok_or_else(|| js_error("workflow workbench is not connected"))?;
        serde_json::to_string(&snapshot)
            .map_err(|error| js_error(format!("failed to serialize workflow state: {error}")))
    }

    #[wasm_bindgen]
    pub fn root_state() -> Result<String, JsValue> {
        let mode = ROOT_VIEW
            .with(|slot| slot.borrow().clone())
            .map(|root| {
                APPLICATION.with(|application| {
                    application
                        .borrow()
                        .as_ref()
                        .map(|application| application.update(|cx| root.read(cx).mode()))
                })
            })
            .flatten()
            .ok_or_else(|| js_error("GPUI root view is not ready"))?;
        serde_json::to_string(&json!({
            "mode": mode,
            "defaultMode": MobileRootMode::Workbench,
            "gateFixtureIsProductSource": false,
        }))
        .map_err(|error| js_error(format!("failed to serialize root state: {error}")))
    }

    #[wasm_bindgen]
    pub fn workflow_action(payload: &str) -> Result<String, JsValue> {
        if payload.is_empty() || payload.len() > MAX_WORKFLOW_COMMAND_BYTES {
            return Err(js_error(
                "workflow command is empty or exceeds the bounded size",
            ));
        }
        let command: WorkflowWorkbenchCommand = serde_json::from_str(payload)
            .map_err(|error| js_error(format!("invalid workflow command: {error}")))?;
        let view = WORKBENCH_VIEW
            .with(|slot| slot.borrow().clone())
            .ok_or_else(|| js_error("workflow workbench is not connected"))?;
        APPLICATION.with(|application| {
            let application = application.borrow();
            let application = application
                .as_ref()
                .ok_or_else(|| js_error("GPUI application handle is unavailable"))?;
            let window_handle = WINDOW_HANDLE
                .with(|handle| *handle.borrow())
                .ok_or_else(|| js_error("GPUI window handle is unavailable"))?;
            application.update(|cx| {
                window_handle
                    .update(cx, |_, window, cx| {
                        view.update(cx, |view, cx| {
                            view.apply_test_command(command, window, cx)
                                .map_err(|error| backend_js_error(&error))
                        })
                    })
                    .map_err(|error| {
                        js_error(format!("failed to dispatch workflow command: {error:#}"))
                    })?
            })
        })?;
        workflow_state()
    }

    #[wasm_bindgen]
    pub fn fixture_state() -> Result<String, JsValue> {
        let view = GATE_VIEW
            .with(|gate_view| gate_view.borrow().clone())
            .ok_or_else(|| js_error("GPUI gate view is not ready"))?;
        APPLICATION.with(|application| {
            let application = application.borrow();
            let application = application
                .as_ref()
                .ok_or_else(|| js_error("GPUI application handle is unavailable"))?;
            application.update(|cx| {
                let overlay = overlay_state(cx)?;
                let view = view.read(cx);
                let dialog_layout = view.dialog_layout_metrics();
                let sheet_layout = view.sheet_layout_metrics();
                serde_json::to_string(&json!({
                    "composerValue": view.composer_value(cx),
                    "approval": view.approval_label(),
                    "host": view.host_snapshot(),
                    "scrollOffsets": {
                        "page": view.page_scroll_offset(),
                        "timeline": view.timeline_scroll_offset()
                    },
                    "scrollMaxOffsets": {
                        "page": view.page_scroll_max_offset(),
                        "timeline": view.timeline_scroll_max_offset()
                    },
                    "overlay": {
                        "dialogActive": overlay.0,
                        "sheetActive": overlay.1,
                        "lastPlatformBack": LAST_PLATFORM_BACK.with(|result| *result.borrow()),
                        "layout": {
                            "dialogWidth": dialog_layout[0],
                            "dialogMaxHeight": dialog_layout[1],
                            "dialogMarginTop": dialog_layout[2],
                            "sheetPlacement": sheet_layout.0,
                            "sheetSize": sheet_layout.1
                        }
                    },
                    "interfaceFont": {
                        "tokenFamily": INTERFACE_TYPOGRAPHY.family,
                        "resolvedFamily": RESOLVED_INTERFACE_FONT.with(|font| font.borrow().clone())
                    },
                    "themeDark": Theme::global(cx).is_dark()
                }))
                .map_err(|error| js_error(format!("failed to serialize fixture state: {error}")))
            })
        })
    }

    #[wasm_bindgen]
    pub fn platform_back() -> Result<String, JsValue> {
        let status = APPLICATION.with(|application| {
            let application = application.borrow();
            let Some(application) = application.as_ref() else {
                return "unhandled";
            };
            let Some(window_handle) = WINDOW_HANDLE.with(|handle| *handle.borrow()) else {
                return "unhandled";
            };
            application.update(|cx| {
                window_handle
                    .update(cx, |_, window, cx| {
                        if window.has_active_dialog(cx) {
                            window.close_dialog(cx);
                            "closed_dialog"
                        } else if window.has_active_sheet(cx) {
                            window.close_sheet(cx);
                            "closed_sheet"
                        } else {
                            let compact_handled = WORKBENCH_VIEW.with(|slot| {
                                slot.borrow().as_ref().is_some_and(|view| {
                                    view.update(cx, |view, cx| view.platform_back(cx))
                                })
                            });
                            if compact_handled {
                                "closed_compact_navigation"
                            } else {
                                "unhandled"
                            }
                        }
                    })
                    .unwrap_or("unhandled")
            })
        });
        LAST_PLATFORM_BACK.with(|result| *result.borrow_mut() = status);
        serde_json::to_string(&json!({ "status": status }))
            .map_err(|error| js_error(format!("failed to serialize platform back result: {error}")))
    }

    fn dispatch_to_view(envelope: BrowserHostEventEnvelope) -> Result<bool, JsValue> {
        let gate = GATE_VIEW.with(|gate_view| gate_view.borrow().clone());
        let workbench = WORKBENCH_VIEW.with(|slot| slot.borrow().clone());
        if gate.is_none() && workbench.is_none() {
            return Ok(false);
        }

        APPLICATION.with(|application| {
            let application = application.borrow();
            let application = application
                .as_ref()
                .ok_or_else(|| js_error("GPUI application handle is unavailable"))?;
            application.update(|cx| {
                if let Some(gate) = gate {
                    apply_event_to_view(&gate, envelope, cx);
                } else if let BrowserHostEvent::Appearance { dark_mode } = &envelope.event {
                    apply_browser_gate_theme_mode(*dark_mode, cx);
                }
                if let Some(workbench) = workbench {
                    let snapshot = HOST_SNAPSHOT.with(|snapshot| snapshot.borrow().clone());
                    workbench.update(cx, |view, cx| {
                        view.apply_browser_host_snapshot(&snapshot, cx)
                    });
                }
            });
            Ok(true)
        })
    }

    fn apply_pending_events(view: &Entity<BrowserGateView>, cx: &mut App) {
        let events =
            PENDING_EVENTS.with(|pending| pending.borrow_mut().drain(..).collect::<Vec<_>>());
        for envelope in events {
            apply_event_to_view(view, envelope, cx);
        }
    }

    fn apply_event_to_view(
        view: &Entity<BrowserGateView>,
        envelope: BrowserHostEventEnvelope,
        cx: &mut App,
    ) {
        if let BrowserHostEvent::Appearance { dark_mode } = &envelope.event {
            apply_browser_gate_theme_mode(*dark_mode, cx);
        }
        view.update(cx, |view, cx| {
            view.apply_host_event(envelope, cx);
        });
    }

    fn activate_workbench(backend: Arc<WebRemoteBackend>) -> Result<(), JsValue> {
        let root = ROOT_VIEW
            .with(|slot| slot.borrow().clone())
            .ok_or_else(|| js_error("GPUI root view is unavailable"))?;
        let window_handle = WINDOW_HANDLE
            .with(|handle| *handle.borrow())
            .ok_or_else(|| js_error("GPUI window handle is unavailable"))?;
        let host = HOST_SNAPSHOT.with(|snapshot| snapshot.borrow().clone());
        let facade = backend.facade();
        APPLICATION.with(|application| {
            let application = application.borrow();
            let application = application
                .as_ref()
                .ok_or_else(|| js_error("GPUI application handle is unavailable"))?;
            application.update(|cx| {
                window_handle
                    .update(cx, |_, window, cx| {
                        let view = cx.new(|cx| WorkflowWorkbenchView::new(facade, window, cx));
                        view.update(cx, |view, cx| {
                            view.apply_browser_host_snapshot(&host, cx);
                            view.start(cx);
                        });
                        root.update(cx, |root, cx| root.activate(view.clone(), cx));
                        WORKBENCH_VIEW.with(|slot| *slot.borrow_mut() = Some(view));
                    })
                    .map_err(|error| {
                        js_error(format!("failed to activate workflow workbench: {error:#}"))
                    })
            })
        })
    }

    fn current_navigation() -> CompactNavigation {
        let Some(view) = WORKBENCH_VIEW.with(|slot| slot.borrow().clone()) else {
            return CompactNavigation::default();
        };
        APPLICATION.with(|application| {
            application
                .borrow()
                .as_ref()
                .map(|application| {
                    application.update(|cx| view.read(cx).navigation_state().clone())
                })
                .unwrap_or_default()
        })
    }

    fn current_workbench_snapshot() -> Option<vibex_ui::WorkflowWorkbenchSnapshot> {
        let view = WORKBENCH_VIEW.with(|slot| slot.borrow().clone())?;
        APPLICATION.with(|application| {
            application
                .borrow()
                .as_ref()
                .map(|application| application.update(|cx| view.read(cx).snapshot()))
        })
    }

    fn set_workbench_connection(state: WorkbenchConnectionState) -> Result<(), JsValue> {
        let Some(view) = WORKBENCH_VIEW.with(|slot| slot.borrow().clone()) else {
            return Ok(());
        };
        APPLICATION.with(|application| {
            let application = application.borrow();
            let application = application
                .as_ref()
                .ok_or_else(|| js_error("GPUI application handle is unavailable"))?;
            application.update(|cx| {
                view.update(cx, |view, cx| view.set_connection_state(state, cx));
            });
            Ok(())
        })
    }

    fn sync_workbench_connection() -> Result<(), JsValue> {
        let state = REMOTE_RUNTIME.with(|slot| {
            slot.borrow().as_ref().map(|runtime| {
                let _ = runtime.backend.capability_snapshot();
                runtime.backend.connection_state().state
            })
        });
        if let Some(state) = state {
            set_workbench_connection(map_connection_state(state))?;
        }
        Ok(())
    }

    fn map_connection_state(state: RemoteConnectionState) -> WorkbenchConnectionState {
        match state {
            RemoteConnectionState::Online => WorkbenchConnectionState::Online,
            RemoteConnectionState::Degraded => WorkbenchConnectionState::Degraded,
            RemoteConnectionState::Revoked => WorkbenchConnectionState::Revoked,
            RemoteConnectionState::Incompatible => WorkbenchConnectionState::Incompatible,
            RemoteConnectionState::Idle | RemoteConnectionState::Offline => {
                WorkbenchConnectionState::Offline
            }
            RemoteConnectionState::Resolving
            | RemoteConnectionState::Probing
            | RemoteConnectionState::Connecting
            | RemoteConnectionState::Authenticating
            | RemoteConnectionState::Syncing
            | RemoteConnectionState::Reconnecting => WorkbenchConnectionState::Reconnecting,
        }
    }

    fn connection_state_for_error(error: &BackendError) -> WorkbenchConnectionState {
        if error.code.contains("incompatible") || error.code.contains("protocol") {
            WorkbenchConnectionState::Incompatible
        } else if error.code.contains("revoked") || error.kind == BackendErrorKind::Permission {
            WorkbenchConnectionState::Revoked
        } else if error.kind == BackendErrorKind::Loading {
            WorkbenchConnectionState::Reconnecting
        } else {
            WorkbenchConnectionState::Offline
        }
    }

    fn overlay_state(cx: &mut App) -> Result<(bool, bool), JsValue> {
        let window_handle = WINDOW_HANDLE
            .with(|handle| *handle.borrow())
            .ok_or_else(|| js_error("GPUI window handle is unavailable"))?;
        window_handle
            .update(cx, |_, window, cx| {
                (window.has_active_dialog(cx), window.has_active_sheet(cx))
            })
            .map_err(|error| js_error(format!("failed to inspect GPUI overlays: {error:#}")))
    }

    fn js_error(message: impl ToString) -> JsValue {
        JsValue::from_str(&message.to_string())
    }
}

#[cfg(target_family = "wasm")]
pub use mobile::*;
