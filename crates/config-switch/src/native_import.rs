use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value as JsonValue;
use toml::Value as TomlValue;
use vibex_core::{
    AcpProcessStrategy, AcpProviderConfig, AcpProviderEnvReference, AcpProviderEnvSource,
    AgentCommandConfig, AgentId, ProviderBindingMetadata, ProviderKind, ProviderNativeConfigFile,
    ProviderNativeConfigFileKind, ProviderNativeConfigFileStatus,
    ProviderNativeImportCreateRequest, ProviderNativeImportCreateResult,
    ProviderNativeImportDiagnostic, ProviderNativeImportItem, ProviderNativeImportItemStatus,
    ProviderNativeImportPreview, ProviderNativeImportPreviewRequest,
    ProviderNativeImportRedactedField, ProviderNativeImportSource, ProviderOptions,
    ProviderProfile, ProviderProfileCreateRequest, ProviderSecretBackend, ProviderSecretKind,
    ProviderSecretReferenceCreateRequest, ProviderSecretSetupState, RequestId, VibexError,
    VibexResult, builtin_agent_definitions, unix_timestamp_ms,
};
use vibex_db::ProviderProfileRepository;

use crate::{
    CODEX_API_KEY_ENV_OPTION_KEY, CODEX_MODEL_PROVIDER_CONFIG_TOML_OPTION_KEY,
    CODEX_MODEL_PROVIDER_ID_OPTION_KEY, ProviderConfigService,
    default_acp_runtime_config_for_agent, option_entry, placeholder_secret, provider_option_value,
    secrets,
};

const CC_SWITCH_NATIVE_SOURCE: &str = "cc-switch";
const CC_SWITCH_DB_PATH_OPTION_KEY: &str = "ccSwitchDbPath";
const CC_SWITCH_PROVIDER_ID_OPTION_KEY: &str = "ccSwitchProviderId";
const CC_SWITCH_APP_TYPE_OPTION_KEY: &str = "ccSwitchAppType";
const CC_SWITCH_WEBSITE_URL_OPTION_KEY: &str = "ccSwitchWebsiteUrl";

#[derive(Debug, Clone)]
struct NativeImportRoots {
    codex_root: Option<PathBuf>,
    claude_root: Option<PathBuf>,
    claude_mcp_path: Option<PathBuf>,
    cc_switch_db_paths: Option<Vec<PathBuf>>,
}

impl Default for NativeImportRoots {
    fn default() -> Self {
        Self {
            codex_root: None,
            claude_root: None,
            claude_mcp_path: None,
            cc_switch_db_paths: None,
        }
    }
}

#[derive(Debug, Clone)]
struct ReadJsonResult {
    value: Option<JsonValue>,
    file: ProviderNativeConfigFile,
}

#[derive(Debug, Clone)]
struct ReadTomlResult {
    value: Option<TomlValue>,
    file: ProviderNativeConfigFile,
}

#[derive(Debug, Clone)]
struct CcSwitchProviderRow {
    provider_id: String,
    app_type: String,
    name: String,
    settings_config: String,
    website_url: Option<String>,
}

#[derive(Debug, Clone)]
struct CcSwitchAgentMapping {
    app_type: String,
    agent_id: AgentId,
    provider_kind: ProviderKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CcSwitchImportIdentity {
    db_path: String,
    provider_id: String,
    app_type: String,
}

#[derive(Debug, Default)]
struct CcSwitchSecretMigration {
    migrated_lookup_key: Option<String>,
    diagnostics: Vec<ProviderNativeImportDiagnostic>,
}

impl ProviderConfigService {
    pub fn preview_native_import(
        &self,
        request: ProviderNativeImportPreviewRequest,
    ) -> VibexResult<ProviderNativeImportPreview> {
        preview_native_import_with_roots(request, NativeImportRoots::default())
    }

    pub fn create_profile_from_import(
        &self,
        request: ProviderNativeImportCreateRequest,
    ) -> VibexResult<ProviderNativeImportCreateResult> {
        self.create_profile_from_import_with_roots(request, NativeImportRoots::default())
    }

    fn create_profile_from_import_with_roots(
        &self,
        request: ProviderNativeImportCreateRequest,
        roots: NativeImportRoots,
    ) -> VibexResult<ProviderNativeImportCreateResult> {
        let preview = preview_native_import_with_roots(request.preview_request, roots)?;
        let item = preview
            .items
            .into_iter()
            .find(|candidate| candidate.import_item_id == request.import_item_id)
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_native_import_item_not_found",
                    "native import item was not found in the current preview",
                )
                .with_diagnostic("importItemId", request.import_item_id.as_str())
            })?;

        if item.status == ProviderNativeImportItemStatus::BlockedByParseError {
            return Err(VibexError::validation(
                "provider_native_import_item_not_importable",
                "native import item is blocked by parse errors",
            )
            .with_diagnostic("importItemId", item.import_item_id.as_str()));
        }

        let source = item.source;
        let mut diagnostics = item.diagnostics.clone();
        if is_cc_switch_import_item(&item)
            && let Some(profile) = self.existing_cc_switch_profile_for_import_item(&item)?
        {
            return Ok(ProviderNativeImportCreateResult {
                profile: self.reconcile_imported_agent_profile(profile)?,
                source,
                diagnostics,
            });
        }
        let mut request = profile_request_from_item(item.clone());
        let secret_migration = if is_cc_switch_import_item(&item) {
            migrate_cc_switch_secret_reference(&item, &mut request)?
        } else {
            CcSwitchSecretMigration::default()
        };
        diagnostics.extend(secret_migration.diagnostics);
        let profile = match self.create_profile(request) {
            Ok(profile) => profile,
            Err(error) => {
                if let Some(lookup_key) = secret_migration.migrated_lookup_key.as_deref() {
                    let _ = secrets::delete_provider_secret(lookup_key);
                }
                return Err(error);
            }
        };
        let profile = self.reconcile_imported_agent_profile(profile)?;
        Ok(ProviderNativeImportCreateResult {
            profile,
            source,
            diagnostics,
        })
    }

    fn existing_cc_switch_profile_for_import_item(
        &self,
        item: &ProviderNativeImportItem,
    ) -> VibexResult<Option<ProviderProfile>> {
        let Some(identity) = cc_switch_import_identity(&item.provider_options) else {
            return Ok(None);
        };
        let Some(agent_id) = item.agent_id.as_ref() else {
            return Ok(None);
        };

        let conn = self.open_connection()?;
        let profiles = ProviderProfileRepository::list_by_agent(&conn, agent_id, true)?;
        Ok(profiles.into_iter().find(|profile| {
            (profile.kind == item.provider_kind
                || (profile.kind == ProviderKind::Acp
                    && matches!(
                        item.provider_kind,
                        ProviderKind::Claude | ProviderKind::Codex
                    )))
                && cc_switch_import_identity(&profile.provider_options).as_ref() == Some(&identity)
        }))
    }

    fn reconcile_imported_agent_profile(
        &self,
        profile: ProviderProfile,
    ) -> VibexResult<ProviderProfile> {
        if !matches!(profile.agent_id.as_str(), "claude" | "codex") {
            return Ok(profile);
        }

        let conn = self.open_connection()?;
        let runtime = default_acp_runtime_config_for_agent(&conn, &profile.agent_id)?;
        drop(conn);
        self.reconcile_agent_acp_runtime(
            profile.agent_id.clone(),
            AgentCommandConfig {
                command: runtime.command,
                args: runtime.args,
            },
        )?;
        self.get_profile(&profile.id)?.ok_or_else(|| {
            VibexError::storage(
                "provider_native_import_profile_readback_failed",
                "failed to read provider profile after ACP runtime reconciliation",
            )
            .with_diagnostic("providerProfileId", profile.id.as_str())
        })
    }
}

fn preview_native_import_with_roots(
    request: ProviderNativeImportPreviewRequest,
    roots: NativeImportRoots,
) -> VibexResult<ProviderNativeImportPreview> {
    let sources = normalize_sources(request.sources);
    let mut files = Vec::new();
    let mut items = Vec::new();
    let mut diagnostics = Vec::new();

    for source in sources.iter().copied() {
        match source {
            ProviderNativeImportSource::Codex => {
                collect_codex_preview(
                    roots.codex_root.clone(),
                    &mut files,
                    &mut items,
                    &mut diagnostics,
                )?;
            }
            ProviderNativeImportSource::Claude => collect_claude_preview(
                roots.claude_root.clone(),
                roots.claude_mcp_path.clone(),
                &mut files,
                &mut items,
                &mut diagnostics,
            )?,
            ProviderNativeImportSource::CcSwitch => collect_cc_switch_preview(
                roots.cc_switch_db_paths.clone(),
                &mut files,
                &mut items,
                &mut diagnostics,
            )?,
        }
    }
    dedupe_native_import_items(&mut items);

    Ok(ProviderNativeImportPreview {
        preview_id: RequestId::new(),
        sources,
        files,
        items,
        diagnostics,
        created_at_ms: unix_timestamp_ms(),
    })
}

fn collect_codex_preview(
    root_override: Option<PathBuf>,
    files: &mut Vec<ProviderNativeConfigFile>,
    items: &mut Vec<ProviderNativeImportItem>,
    diagnostics: &mut Vec<ProviderNativeImportDiagnostic>,
) -> VibexResult<()> {
    let root = root_override.unwrap_or_else(codex_config_root);
    let auth = read_json_file(
        ProviderNativeImportSource::Codex,
        ProviderNativeConfigFileKind::CodexAuthJson,
        root.join("auth.json"),
    );
    let config = read_toml_file(
        ProviderNativeImportSource::Codex,
        ProviderNativeConfigFileKind::CodexConfigToml,
        root.join("config.toml"),
    );
    files.push(auth.file.clone());
    files.push(config.file.clone());
    diagnostics.extend(auth.file.diagnostics.clone());
    diagnostics.extend(config.file.diagnostics.clone());

    let mut auth_redacted_fields = Vec::new();
    let mut auth_secret_references = Vec::new();
    let mut item_diagnostics = Vec::new();

    if let Some(auth_value) = auth.value.as_ref() {
        collect_json_secret_fields(
            auth_value,
            ProviderNativeImportSource::Codex,
            ProviderNativeConfigFileKind::CodexAuthJson,
            &mut auth_redacted_fields,
        );
        if json_has_secret(auth_value) {
            auth_secret_references.push(placeholder_secret(
                ProviderSecretKind::ApiKey,
                "OPENAI_API_KEY",
                "OpenAI API key from Codex native config",
            ));
        }
        add_unknown_json_fields(
            auth_value,
            ProviderNativeImportSource::Codex,
            ProviderNativeConfigFileKind::CodexAuthJson,
            &["OPENAI_API_KEY", "auth_mode"],
            &mut item_diagnostics,
        );
    }

    let config_value = config.value.as_ref();
    if let Some(catalog_path) = config_value.and_then(codex_model_catalog_path) {
        let catalog_path = resolve_neighbor_path(&root, &catalog_path);
        let catalog = read_json_file(
            ProviderNativeImportSource::Codex,
            ProviderNativeConfigFileKind::CodexModelCatalogJson,
            catalog_path,
        );
        diagnostics.extend(catalog.file.diagnostics.clone());
        files.push(catalog.file);
    } else {
        let cache = read_json_file(
            ProviderNativeImportSource::Codex,
            ProviderNativeConfigFileKind::CodexModelsCacheJson,
            root.join("models_cache.json"),
        );
        diagnostics.extend(cache.file.diagnostics.clone());
        files.push(cache.file);
    }

    if auth.value.is_none() && config.value.is_none() {
        if auth.file.status == ProviderNativeConfigFileStatus::Missing
            && config.file.status == ProviderNativeConfigFileStatus::Missing
        {
            diagnostics.push(diagnostic(
                ProviderNativeImportSource::Codex,
                None,
                "provider_native_import_source_missing",
                "no Codex native config files were found",
                vec![option_entry("root", root.display().to_string())],
            ));
        }
        return Ok(());
    }

    let active_provider = config_value.and_then(|value| toml_string(value.get("model_provider")));

    if let Some(value) = config_value {
        add_unknown_toml_fields(
            value,
            ProviderNativeImportSource::Codex,
            ProviderNativeConfigFileKind::CodexConfigToml,
            &[
                "model",
                "model_provider",
                "model_providers",
                "model_catalog_json",
                "model_context_window",
            ],
            &mut item_diagnostics,
        );
    }

    let provider_ids = codex_import_provider_ids(config_value, active_provider.as_deref());
    if provider_ids.is_empty() {
        items.push(codex_import_item(CodexImportItemInput {
            source: ProviderNativeImportSource::Codex,
            agent_id: builtin_agent_id("codex"),
            config_file_kind: ProviderNativeConfigFileKind::CodexConfigToml,
            import_suffix: "codex_config".to_string(),
            native_source: "codex".to_string(),
            root_or_db_path: root.display().to_string(),
            display_prefix: "Codex native".to_string(),
            provider_id: active_provider.clone(),
            account_alias: None,
            provider_name: None,
            provider_table: active_provider
                .as_deref()
                .and_then(|provider_id| codex_provider_table(config_value, provider_id)),
            config_value,
            auth_redacted_fields: auth_redacted_fields.clone(),
            auth_secret_references: auth_secret_references.clone(),
            diagnostics: item_diagnostics.clone(),
            extra_entries: Vec::new(),
            blocked_by_parse_error: false,
        }));
    } else {
        for provider_id in provider_ids {
            let provider_table = codex_provider_table(config_value, &provider_id);
            let provider_name = provider_table.and_then(|table| toml_string(table.get("name")));
            items.push(codex_import_item(CodexImportItemInput {
                source: ProviderNativeImportSource::Codex,
                agent_id: builtin_agent_id("codex"),
                config_file_kind: ProviderNativeConfigFileKind::CodexConfigToml,
                import_suffix: format!(
                    "codex_provider_{}",
                    sanitize_request_id_suffix(&provider_id)
                ),
                native_source: "codex".to_string(),
                root_or_db_path: root.display().to_string(),
                display_prefix: "Codex native".to_string(),
                provider_id: Some(provider_id),
                account_alias: None,
                provider_name,
                provider_table,
                config_value,
                auth_redacted_fields: auth_redacted_fields.clone(),
                auth_secret_references: auth_secret_references.clone(),
                diagnostics: item_diagnostics.clone(),
                extra_entries: Vec::new(),
                blocked_by_parse_error: false,
            }));
        }
    }

    Ok(())
}

struct CodexImportItemInput<'a> {
    source: ProviderNativeImportSource,
    agent_id: AgentId,
    config_file_kind: ProviderNativeConfigFileKind,
    import_suffix: String,
    native_source: String,
    root_or_db_path: String,
    display_prefix: String,
    provider_id: Option<String>,
    account_alias: Option<String>,
    provider_name: Option<String>,
    provider_table: Option<&'a TomlValue>,
    config_value: Option<&'a TomlValue>,
    auth_redacted_fields: Vec<ProviderNativeImportRedactedField>,
    auth_secret_references: Vec<vibex_core::ProviderSecretReferenceCreateRequest>,
    diagnostics: Vec<ProviderNativeImportDiagnostic>,
    extra_entries: Vec<ProviderBindingMetadata>,
    blocked_by_parse_error: bool,
}

fn codex_import_item(input: CodexImportItemInput<'_>) -> ProviderNativeImportItem {
    let mut redacted_fields = input.auth_redacted_fields;
    let mut secret_references = input.auth_secret_references;
    if let Some(table) = input.provider_table {
        collect_toml_secret_fields(
            table,
            input.source,
            input.config_file_kind,
            &mut redacted_fields,
            &mut secret_references,
        );
    }

    let base_url = input
        .provider_table
        .and_then(codex_provider_base_url)
        .or_else(|| {
            input
                .config_value
                .and_then(|value| toml_string(value.get("base_url")))
        });
    let default_model = input
        .provider_table
        .and_then(|table| toml_string(table.get("model")))
        .or_else(|| {
            input
                .config_value
                .and_then(|value| toml_string(value.get("model")))
        });
    let wire_api = input
        .provider_table
        .and_then(|table| toml_string(table.get("wire_api")));
    let status = if input.blocked_by_parse_error {
        ProviderNativeImportItemStatus::BlockedByParseError
    } else if secret_references.is_empty() {
        ProviderNativeImportItemStatus::Partial
    } else {
        ProviderNativeImportItemStatus::NeedsSecretSetup
    };

    let label = input
        .provider_name
        .clone()
        .or_else(|| input.provider_id.clone())
        .unwrap_or_else(|| "config".to_string());
    let display_name = if input.display_prefix.trim().is_empty() {
        label
    } else {
        format!("{} {label}", input.display_prefix)
    };

    let mut entries = vec![option_entry("nativeSource", input.native_source.clone())];
    if input.native_source == CC_SWITCH_NATIVE_SOURCE {
        entries.push(option_entry(
            CC_SWITCH_DB_PATH_OPTION_KEY,
            input.root_or_db_path,
        ));
    } else {
        entries.push(option_entry("nativeRoot", input.root_or_db_path));
    }
    push_option_entry(
        &mut entries,
        "nativeModelProvider",
        input.provider_id.clone(),
    );
    push_option_entry(
        &mut entries,
        CODEX_MODEL_PROVIDER_ID_OPTION_KEY,
        input.provider_id.clone(),
    );
    push_option_entry(&mut entries, "nativeModelProviderName", input.provider_name);
    if let Some(provider_config_toml) = input.provider_table.and_then(redacted_toml_fragment) {
        entries.push(option_entry(
            CODEX_MODEL_PROVIDER_CONFIG_TOML_OPTION_KEY,
            provider_config_toml,
        ));
    }
    if let Some(env_key) = input
        .provider_table
        .and_then(|table| toml_string(table.get("env_key")))
    {
        entries.push(option_entry(CODEX_API_KEY_ENV_OPTION_KEY, env_key));
    }
    push_option_entry(&mut entries, "wireApi", wire_api);
    push_option_entry(
        &mut entries,
        "modelContextWindow",
        input
            .provider_table
            .and_then(|table| table.get("model_context_window"))
            .or_else(|| {
                input
                    .config_value
                    .and_then(|value| value.get("model_context_window"))
            })
            .map(|value| value.to_string()),
    );
    entries.extend(input.extra_entries);

    ProviderNativeImportItem {
        import_item_id: deterministic_request_id(&input.import_suffix),
        source: input.source,
        provider_kind: ProviderKind::Codex,
        agent_id: Some(input.agent_id),
        display_name,
        account_alias: input.account_alias.or(input.provider_id),
        base_url,
        default_model,
        small_model: None,
        large_model: None,
        reasoning_effort: None,
        provider_options: ProviderOptions {
            schema_version: 1,
            entries,
        },
        secret_references,
        status,
        redacted_fields,
        diagnostics: input.diagnostics,
    }
}

fn redacted_toml_fragment(value: &TomlValue) -> Option<String> {
    let mut redacted = value.clone();
    redact_toml_secret_values(&mut redacted);
    toml::to_string(&redacted)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn redact_toml_secret_values(value: &mut TomlValue) {
    let Some(table) = value.as_table_mut() else {
        return;
    };
    let secret_keys: Vec<String> = table
        .keys()
        .filter(|key| is_secret_key(key))
        .cloned()
        .collect();
    for key in secret_keys {
        table.remove(&key);
    }
    for (_key, value) in table.iter_mut() {
        redact_toml_secret_values(value);
    }
}

fn collect_cc_switch_preview(
    db_paths_override: Option<Vec<PathBuf>>,
    files: &mut Vec<ProviderNativeConfigFile>,
    items: &mut Vec<ProviderNativeImportItem>,
    diagnostics: &mut Vec<ProviderNativeImportDiagnostic>,
) -> VibexResult<()> {
    let db_candidates = db_paths_override.unwrap_or_else(cc_switch_db_candidates);
    let display_path = db_candidates
        .first()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "~/.cc-switch/cc-switch.db".to_string());
    let db_path = db_candidates.into_iter().find(|path| path.exists());
    let Some(db_path) = db_path else {
        files.push(config_file(
            ProviderNativeImportSource::CcSwitch,
            ProviderNativeConfigFileKind::CcSwitchDatabase,
            display_path.clone(),
            ProviderNativeConfigFileStatus::Missing,
            Vec::new(),
        ));
        diagnostics.push(diagnostic(
            ProviderNativeImportSource::CcSwitch,
            Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
            "provider_native_import_cc_switch_missing",
            "no CC Switch provider database was found",
            vec![option_entry("path", display_path)],
        ));
        return Ok(());
    };

    let connection = match Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(connection) => connection,
        Err(error) => {
            let diagnostic = diagnostic(
                ProviderNativeImportSource::CcSwitch,
                Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                "provider_native_import_cc_switch_unreadable",
                "failed to open CC Switch provider database",
                vec![
                    option_entry("path", db_path.display().to_string()),
                    option_entry("error", error.to_string()),
                ],
            );
            files.push(config_file(
                ProviderNativeImportSource::CcSwitch,
                ProviderNativeConfigFileKind::CcSwitchDatabase,
                db_path.display().to_string(),
                ProviderNativeConfigFileStatus::Unreadable,
                vec![diagnostic.clone()],
            ));
            diagnostics.push(diagnostic);
            return Ok(());
        }
    };

    let mut statement = match connection.prepare(
        "SELECT id, app_type, name, settings_config, website_url \
         FROM providers ORDER BY rowid ASC",
    ) {
        Ok(statement) => statement,
        Err(error) => {
            let diagnostic = diagnostic(
                ProviderNativeImportSource::CcSwitch,
                Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                "provider_native_import_cc_switch_schema_unmapped",
                "CC Switch provider database schema is not recognized",
                vec![
                    option_entry("path", db_path.display().to_string()),
                    option_entry("error", error.to_string()),
                ],
            );
            files.push(config_file(
                ProviderNativeImportSource::CcSwitch,
                ProviderNativeConfigFileKind::CcSwitchDatabase,
                db_path.display().to_string(),
                ProviderNativeConfigFileStatus::ParseError,
                vec![diagnostic.clone()],
            ));
            diagnostics.push(diagnostic);
            return Ok(());
        }
    };

    let rows = match statement.query_map([], |row| {
        Ok(CcSwitchProviderRow {
            provider_id: row.get::<_, String>(0)?,
            app_type: row.get::<_, String>(1)?,
            name: row.get::<_, String>(2)?,
            settings_config: row.get::<_, String>(3)?,
            website_url: row.get::<_, Option<String>>(4)?,
        })
    }) {
        Ok(rows) => rows,
        Err(error) => {
            diagnostics.push(diagnostic(
                ProviderNativeImportSource::CcSwitch,
                Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                "provider_native_import_cc_switch_read_failed",
                "failed to read CC Switch provider rows",
                vec![
                    option_entry("path", db_path.display().to_string()),
                    option_entry("error", error.to_string()),
                ],
            ));
            return Ok(());
        }
    };

    files.push(config_file(
        ProviderNativeImportSource::CcSwitch,
        ProviderNativeConfigFileKind::CcSwitchDatabase,
        db_path.display().to_string(),
        ProviderNativeConfigFileStatus::Parsed,
        Vec::new(),
    ));

    for row in rows {
        let row = match row {
            Ok(row) => row,
            Err(error) => {
                diagnostics.push(diagnostic(
                    ProviderNativeImportSource::CcSwitch,
                    Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                    "provider_native_import_cc_switch_row_decode_failed",
                    "failed to decode CC Switch provider row",
                    vec![
                        option_entry("path", db_path.display().to_string()),
                        option_entry("error", error.to_string()),
                    ],
                ));
                continue;
            }
        };
        let Some(mapping) = cc_switch_agent_mapping(&row.app_type) else {
            diagnostics.push(diagnostic(
                ProviderNativeImportSource::CcSwitch,
                Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                "provider_native_import_cc_switch_app_type_unmapped",
                "CC Switch provider app type is not mapped to a Vibex Agent yet",
                vec![
                    option_entry("ccSwitchProviderId", row.provider_id),
                    option_entry("ccSwitchAppType", row.app_type),
                ],
            ));
            continue;
        };
        let Some(item) = cc_switch_import_item(&db_path, row, mapping, diagnostics) else {
            continue;
        };
        items.push(item);
    }

    Ok(())
}

fn cc_switch_import_item(
    db_path: &Path,
    row: CcSwitchProviderRow,
    mapping: CcSwitchAgentMapping,
    diagnostics: &mut Vec<ProviderNativeImportDiagnostic>,
) -> Option<ProviderNativeImportItem> {
    let settings = match serde_json::from_str::<JsonValue>(&row.settings_config) {
        Ok(value) => value,
        Err(error) => {
            diagnostics.push(diagnostic(
                ProviderNativeImportSource::CcSwitch,
                Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                "provider_native_import_cc_switch_provider_parse_failed",
                "failed to parse CC Switch provider settings",
                vec![
                    option_entry(CC_SWITCH_PROVIDER_ID_OPTION_KEY, row.provider_id),
                    option_entry(CC_SWITCH_APP_TYPE_OPTION_KEY, row.app_type),
                    option_entry("error", error.to_string()),
                ],
            ));
            return None;
        }
    };

    match mapping.provider_kind {
        ProviderKind::Codex => {
            cc_switch_codex_import_item(db_path, row, mapping, settings, diagnostics)
        }
        ProviderKind::Claude => Some(cc_switch_simple_import_item(
            db_path,
            row,
            mapping,
            settings,
            ProviderSecretKind::AuthToken,
            "ANTHROPIC_API_KEY",
            "Claude auth token from CC Switch provider",
        )),
        ProviderKind::Acp => {
            cc_switch_acp_import_item(db_path, row, mapping, settings, diagnostics)
        }
    }
}

fn cc_switch_codex_import_item(
    db_path: &Path,
    row: CcSwitchProviderRow,
    mapping: CcSwitchAgentMapping,
    settings: JsonValue,
    _diagnostics: &mut Vec<ProviderNativeImportDiagnostic>,
) -> Option<ProviderNativeImportItem> {
    let mut auth_redacted_fields = Vec::new();
    collect_json_secret_fields(
        &settings,
        ProviderNativeImportSource::CcSwitch,
        ProviderNativeConfigFileKind::CcSwitchDatabase,
        &mut auth_redacted_fields,
    );

    let config_text = settings
        .get("config")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut item_diagnostics = Vec::new();
    let mut blocked_by_parse_error = false;
    let config_value = match config_text {
        Some(text) => match text.parse::<TomlValue>() {
            Ok(value) => Some(value),
            Err(error) => {
                blocked_by_parse_error = true;
                item_diagnostics.push(diagnostic(
                    ProviderNativeImportSource::CcSwitch,
                    Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                    "provider_native_import_parse_failed",
                    "failed to parse CC Switch Codex provider TOML config",
                    vec![
                        option_entry(CC_SWITCH_PROVIDER_ID_OPTION_KEY, row.provider_id.clone()),
                        option_entry("error", error.to_string()),
                    ],
                ));
                None
            }
        },
        None => None,
    };
    let active_provider = config_value
        .as_ref()
        .and_then(|value| toml_string(value.get("model_provider")));
    let provider_table = active_provider
        .as_deref()
        .and_then(|provider_id| codex_provider_table(config_value.as_ref(), provider_id));
    let model_provider_id = active_provider.unwrap_or_else(|| row.provider_id.clone());
    let env_key = provider_table
        .and_then(|table| toml_string(table.get("env_key")))
        .or_else(|| cc_switch_secret_env_key(&settings, "OPENAI_API_KEY"))
        .unwrap_or_else(|| "OPENAI_API_KEY".to_string());
    let auth_secret_references = if cc_switch_secret_value(&settings, &env_key).is_some() {
        vec![placeholder_secret(
            ProviderSecretKind::ApiKey,
            env_key,
            "OpenAI API key from CC Switch Codex provider",
        )]
    } else {
        Vec::new()
    };

    let extra_entries = cc_switch_codex_extra_metadata_entries(&row, &mapping);
    let import_suffix = cc_switch_import_suffix(&mapping, &row);

    Some(codex_import_item(CodexImportItemInput {
        source: ProviderNativeImportSource::CcSwitch,
        agent_id: mapping.agent_id,
        config_file_kind: ProviderNativeConfigFileKind::CcSwitchDatabase,
        import_suffix,
        native_source: CC_SWITCH_NATIVE_SOURCE.to_string(),
        root_or_db_path: db_path.display().to_string(),
        display_prefix: String::new(),
        provider_id: Some(model_provider_id),
        account_alias: Some(row.provider_id.clone()),
        provider_name: Some(cc_switch_display_name(&row)),
        provider_table,
        config_value: config_value.as_ref(),
        auth_redacted_fields,
        auth_secret_references,
        diagnostics: item_diagnostics,
        extra_entries,
        blocked_by_parse_error,
    }))
}

fn cc_switch_simple_import_item(
    db_path: &Path,
    row: CcSwitchProviderRow,
    mapping: CcSwitchAgentMapping,
    settings: JsonValue,
    secret_kind: ProviderSecretKind,
    default_secret_key: &str,
    secret_label: &str,
) -> ProviderNativeImportItem {
    let mut redacted_fields = Vec::new();
    collect_json_secret_fields(
        &settings,
        ProviderNativeImportSource::CcSwitch,
        ProviderNativeConfigFileKind::CcSwitchDatabase,
        &mut redacted_fields,
    );
    let secret_references =
        cc_switch_secret_references(&settings, secret_kind, default_secret_key, secret_label);
    let status = if secret_references.is_empty() {
        ProviderNativeImportItemStatus::Partial
    } else {
        ProviderNativeImportItemStatus::NeedsSecretSetup
    };
    let base_url = cc_switch_json_string(
        &settings,
        &["base_url", "baseUrl", "api_base_url", "apiBaseUrl"],
    )
    .or_else(|| cc_switch_json_env_string(&settings, &["ANTHROPIC_BASE_URL"]));
    let default_model =
        cc_switch_json_string(&settings, &["model", "default_model", "defaultModel"]);
    let account_alias = cc_switch_json_string(&settings, &["account", "accountAlias"])
        .or(Some(row.provider_id.clone()));
    let import_item_id = deterministic_request_id(&cc_switch_import_suffix(&mapping, &row));

    ProviderNativeImportItem {
        import_item_id,
        source: ProviderNativeImportSource::CcSwitch,
        provider_kind: mapping.provider_kind,
        agent_id: Some(mapping.agent_id.clone()),
        display_name: cc_switch_display_name(&row),
        account_alias,
        base_url,
        default_model,
        small_model: None,
        large_model: None,
        reasoning_effort: None,
        provider_options: ProviderOptions {
            schema_version: 1,
            entries: cc_switch_metadata_entries(db_path, &row, &mapping),
        },
        secret_references,
        status,
        redacted_fields,
        diagnostics: Vec::new(),
    }
}

fn cc_switch_acp_import_item(
    db_path: &Path,
    row: CcSwitchProviderRow,
    mapping: CcSwitchAgentMapping,
    settings: JsonValue,
    diagnostics: &mut Vec<ProviderNativeImportDiagnostic>,
) -> Option<ProviderNativeImportItem> {
    let env = cc_switch_acp_env_references(&settings, "OPENCODE_AUTH_TOKEN");
    let models = cc_switch_json_string_list(&settings, &["models"])
        .filter(|models| !models.is_empty())
        .or_else(|| {
            cc_switch_json_string(&settings, &["model", "default_model", "defaultModel"])
                .map(|model| vec![model])
        })
        .unwrap_or_default();
    let modes = cc_switch_json_string_list(&settings, &["modes"])
        .filter(|modes| !modes.is_empty())
        .unwrap_or_else(|| vec!["default".to_string()]);
    let features = cc_switch_json_string_list(&settings, &["features"])
        .filter(|features| !features.is_empty())
        .unwrap_or_else(|| {
            vec![
                "agent_messages".to_string(),
                "tool_calls".to_string(),
                "permission_requests".to_string(),
                "slash_commands".to_string(),
                "skills".to_string(),
            ]
        });
    let command = cc_switch_json_string(
        &settings,
        &["command", "cliCommand", "binary", "executable"],
    )
    .unwrap_or_else(|| mapping.agent_id.as_str().to_string());
    let args = cc_switch_json_string_list(&settings, &["args", "arguments"]).unwrap_or_else(|| {
        if mapping.agent_id.as_str() == "opencode" {
            vec!["acp".to_string()]
        } else {
            Vec::new()
        }
    });
    let config = AcpProviderConfig {
        command,
        args,
        env,
        cwd_template: Some("{workspaceRoot}".to_string()),
        process_strategy: AcpProcessStrategy::default(),
        terminal_tools: false,
        terminal_auth: false,
        models,
        modes,
        features,
        disabled_tools: cc_switch_json_string_list(&settings, &["disabledTools", "disabled_tools"])
            .unwrap_or_default(),
    };
    let mut provider_options = match crate::acp_config_to_options(&config) {
        Ok(options) => options,
        Err(error) => {
            diagnostics.push(diagnostic(
                ProviderNativeImportSource::CcSwitch,
                Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                "provider_native_import_cc_switch_acp_config_invalid",
                "CC Switch ACP provider settings could not be converted to a Vibex ACP profile",
                vec![
                    option_entry(CC_SWITCH_PROVIDER_ID_OPTION_KEY, row.provider_id),
                    option_entry(CC_SWITCH_APP_TYPE_OPTION_KEY, row.app_type),
                    option_entry("error", error.to_string()),
                ],
            ));
            return None;
        }
    };
    provider_options
        .entries
        .extend(cc_switch_metadata_entries(db_path, &row, &mapping));

    let mut redacted_fields = Vec::new();
    collect_json_secret_fields(
        &settings,
        ProviderNativeImportSource::CcSwitch,
        ProviderNativeConfigFileKind::CcSwitchDatabase,
        &mut redacted_fields,
    );
    let secret_references = cc_switch_secret_references(
        &settings,
        ProviderSecretKind::AuthToken,
        "OPENCODE_AUTH_TOKEN",
        "OpenCode auth token from CC Switch provider",
    );
    let status = if secret_references.is_empty() {
        ProviderNativeImportItemStatus::Partial
    } else {
        ProviderNativeImportItemStatus::NeedsSecretSetup
    };
    let default_model = config.models.first().cloned();
    let import_item_id = deterministic_request_id(&cc_switch_import_suffix(&mapping, &row));

    Some(ProviderNativeImportItem {
        import_item_id,
        source: ProviderNativeImportSource::CcSwitch,
        provider_kind: ProviderKind::Acp,
        agent_id: Some(mapping.agent_id),
        display_name: cc_switch_display_name(&row),
        account_alias: Some(row.provider_id.clone()),
        base_url: cc_switch_json_string(
            &settings,
            &["base_url", "baseUrl", "api_base_url", "apiBaseUrl"],
        ),
        default_model,
        small_model: None,
        large_model: None,
        reasoning_effort: None,
        provider_options,
        secret_references,
        status,
        redacted_fields,
        diagnostics: Vec::new(),
    })
}

fn cc_switch_agent_mapping(app_type: &str) -> Option<CcSwitchAgentMapping> {
    let normalized = normalize_cc_switch_app_type(app_type);
    let agent_id = match normalized.as_str() {
        "claude-code" => "claude",
        "open-code" => "opencode",
        _ => normalized.as_str(),
    };
    let definition = builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.id.as_str() == agent_id)?;
    let provider_kind = match definition.id.as_str() {
        "claude" => ProviderKind::Claude,
        "codex" => ProviderKind::Codex,
        _ => ProviderKind::Acp,
    };
    Some(CcSwitchAgentMapping {
        app_type: normalized,
        agent_id: definition.id,
        provider_kind,
    })
}

fn normalize_cc_switch_app_type(app_type: &str) -> String {
    app_type.trim().to_ascii_lowercase().replace('_', "-")
}

fn cc_switch_metadata_entries(
    db_path: &Path,
    row: &CcSwitchProviderRow,
    mapping: &CcSwitchAgentMapping,
) -> Vec<ProviderBindingMetadata> {
    let mut entries = vec![
        option_entry("nativeSource", CC_SWITCH_NATIVE_SOURCE),
        option_entry(CC_SWITCH_DB_PATH_OPTION_KEY, db_path.display().to_string()),
        option_entry(CC_SWITCH_PROVIDER_ID_OPTION_KEY, row.provider_id.clone()),
        option_entry(CC_SWITCH_APP_TYPE_OPTION_KEY, mapping.app_type.clone()),
        option_entry("nativeModelProvider", row.provider_id.clone()),
        option_entry("nativeModelProviderName", cc_switch_display_name(row)),
    ];
    if let Some(website_url) = row.website_url.as_ref() {
        entries.push(option_entry(CC_SWITCH_WEBSITE_URL_OPTION_KEY, website_url));
    }
    entries
}

fn cc_switch_codex_extra_metadata_entries(
    row: &CcSwitchProviderRow,
    mapping: &CcSwitchAgentMapping,
) -> Vec<ProviderBindingMetadata> {
    let mut entries = vec![
        option_entry(CC_SWITCH_PROVIDER_ID_OPTION_KEY, row.provider_id.clone()),
        option_entry(CC_SWITCH_APP_TYPE_OPTION_KEY, mapping.app_type.clone()),
    ];
    if let Some(website_url) = row.website_url.as_ref() {
        entries.push(option_entry(CC_SWITCH_WEBSITE_URL_OPTION_KEY, website_url));
    }
    entries
}

fn cc_switch_display_name(row: &CcSwitchProviderRow) -> String {
    let trimmed = row.name.trim();
    if trimmed.is_empty() {
        format!("CC Switch {}", row.provider_id)
    } else {
        trimmed.to_string()
    }
}

fn cc_switch_secret_references(
    settings: &JsonValue,
    secret_kind: ProviderSecretKind,
    default_secret_key: &str,
    display_label: &str,
) -> Vec<ProviderSecretReferenceCreateRequest> {
    let Some(env_key) = cc_switch_secret_env_key(settings, default_secret_key) else {
        return Vec::new();
    };
    vec![placeholder_secret(secret_kind, env_key, display_label)]
}

fn cc_switch_acp_env_references(
    settings: &JsonValue,
    default_secret_key: &str,
) -> Vec<AcpProviderEnvReference> {
    let Some(env_key) = cc_switch_secret_env_key(settings, default_secret_key) else {
        return Vec::new();
    };
    vec![AcpProviderEnvReference {
        key: env_key.clone(),
        source: AcpProviderEnvSource::SecretReference,
        value: None,
        secret_lookup_key: Some(env_key),
        redacted_hint: "present in cc-switch".to_string(),
    }]
}

fn cc_switch_secret_env_key(settings: &JsonValue, default_secret_key: &str) -> Option<String> {
    if cc_switch_secret_value(settings, default_secret_key).is_some() {
        return Some(default_secret_key.to_string());
    }
    cc_switch_first_secret(settings).map(|(key, _value)| key)
}

fn cc_switch_secret_value(settings: &JsonValue, env_key: &str) -> Option<String> {
    settings
        .get("auth")
        .and_then(|auth| json_secret_string_by_key(auth, env_key))
        .or_else(|| json_secret_string_by_key(settings, env_key))
}

fn cc_switch_first_secret(settings: &JsonValue) -> Option<(String, String)> {
    settings
        .get("auth")
        .and_then(json_first_secret_string)
        .or_else(|| json_first_secret_string(settings))
}

fn json_secret_string_by_key(value: &JsonValue, env_key: &str) -> Option<String> {
    let JsonValue::Object(map) = value else {
        return None;
    };
    for (key, value) in map {
        if key == env_key
            && let Some(secret) = value
                .as_str()
                .map(str::trim)
                .filter(|secret| !secret.is_empty())
        {
            return Some(secret.to_string());
        }
        if let Some(secret) = json_secret_string_by_key(value, env_key) {
            return Some(secret);
        }
    }
    None
}

fn json_first_secret_string(value: &JsonValue) -> Option<(String, String)> {
    let JsonValue::Object(map) = value else {
        return None;
    };
    for (key, value) in map {
        if is_secret_key(key)
            && let Some(secret) = value
                .as_str()
                .map(str::trim)
                .filter(|secret| !secret.is_empty())
        {
            return Some((key.clone(), secret.to_string()));
        }
        if let Some(secret) = json_first_secret_string(value) {
            return Some(secret);
        }
    }
    None
}

fn cc_switch_json_string(value: &JsonValue, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = json_string(value, key) {
            return Some(value);
        }
    }
    for container_key in ["config", "settings", "provider"] {
        if let Some(container) = value
            .get(container_key)
            .filter(|container| container.is_object())
        {
            for key in keys {
                if let Some(value) = json_string(container, key) {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn cc_switch_json_env_string(value: &JsonValue, keys: &[&str]) -> Option<String> {
    value
        .get("env")
        .filter(|env| env.is_object())
        .and_then(|env| keys.iter().find_map(|key| json_string(env, key)))
}

fn cc_switch_json_string_list(value: &JsonValue, keys: &[&str]) -> Option<Vec<String>> {
    for key in keys {
        if let Some(values) = json_string_list(value.get(key)) {
            return Some(values);
        }
    }
    for container_key in ["config", "settings", "provider"] {
        if let Some(container) = value
            .get(container_key)
            .filter(|container| container.is_object())
        {
            for key in keys {
                if let Some(values) = json_string_list(container.get(key)) {
                    return Some(values);
                }
            }
        }
    }
    None
}

fn json_string_list(value: Option<&JsonValue>) -> Option<Vec<String>> {
    match value? {
        JsonValue::Array(values) => {
            let values = values
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            Some(dedupe_strings(values))
        }
        JsonValue::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(vec![trimmed.to_string()])
            }
        }
        _ => None,
    }
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        if !deduped.iter().any(|candidate| candidate == &value) {
            deduped.push(value);
        }
    }
    deduped
}
fn cc_switch_db_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for key in ["CC_SWITCH_CONFIG_DIR", "CC_SWITCH_HOME"] {
        if let Some(value) = std::env::var_os(key).filter(|value| !value.is_empty()) {
            let path = PathBuf::from(value);
            candidates.push(
                if path.extension().is_some_and(|extension| extension == "db") {
                    path
                } else {
                    path.join("cc-switch.db")
                },
            );
        }
    }
    candidates.extend(cc_switch_store_override_db_candidates());
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".cc-switch").join("cc-switch.db"));
    }
    #[cfg(windows)]
    if let Ok(home_env) = std::env::var("HOME") {
        let trimmed = home_env.trim();
        if !trimmed.is_empty() {
            candidates.push(
                PathBuf::from(trimmed)
                    .join(".cc-switch")
                    .join("cc-switch.db"),
            );
        }
    }
    dedupe_paths(candidates)
}

fn cc_switch_store_override_db_candidates() -> Vec<PathBuf> {
    cc_switch_app_paths_store_candidates()
        .into_iter()
        .filter_map(|path| {
            let raw = fs::read_to_string(path).ok()?;
            let value = serde_json::from_str::<JsonValue>(&raw).ok()?;
            let override_path = value
                .get("app_config_dir_override")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            Some(resolve_tilde_path(override_path).join("cc-switch.db"))
        })
        .collect()
}

fn cc_switch_app_paths_store_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(config_dir) = dirs::config_dir() {
        for app_id in ["com.ccswitch.desktop", "cc-switch"] {
            candidates.push(config_dir.join(app_id).join("app_paths.json"));
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        candidates.push(
            home.join("Library")
                .join("Application Support")
                .join("com.ccswitch.desktop")
                .join("app_paths.json"),
        );
    }
    #[cfg(windows)]
    if let Some(data_dir) = dirs::data_dir() {
        candidates.push(data_dir.join("com.ccswitch.desktop").join("app_paths.json"));
    }
    dedupe_paths(candidates)
}

fn resolve_tilde_path(raw: &str) -> PathBuf {
    if raw == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    if let Some(stripped) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    PathBuf::from(raw)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduped = Vec::new();
    for path in paths {
        if !deduped.iter().any(|candidate| candidate == &path) {
            deduped.push(path);
        }
    }
    deduped
}

fn collect_claude_preview(
    root_override: Option<PathBuf>,
    mcp_path_override: Option<PathBuf>,
    files: &mut Vec<ProviderNativeConfigFile>,
    items: &mut Vec<ProviderNativeImportItem>,
    diagnostics: &mut Vec<ProviderNativeImportDiagnostic>,
) -> VibexResult<()> {
    let root = root_override.unwrap_or_else(claude_config_root);
    let settings_path = root.join("settings.json");
    let legacy_path = root.join("claude.json");
    let settings = read_json_file(
        ProviderNativeImportSource::Claude,
        ProviderNativeConfigFileKind::ClaudeSettingsJson,
        settings_path,
    );
    let legacy = read_json_file(
        ProviderNativeImportSource::Claude,
        ProviderNativeConfigFileKind::ClaudeLegacyJson,
        legacy_path,
    );
    let mcp = read_json_file(
        ProviderNativeImportSource::Claude,
        ProviderNativeConfigFileKind::ClaudeMcpJson,
        mcp_path_override.unwrap_or_else(claude_mcp_path),
    );
    diagnostics.extend(settings.file.diagnostics.clone());
    diagnostics.extend(legacy.file.diagnostics.clone());
    diagnostics.extend(mcp.file.diagnostics.clone());
    files.push(settings.file.clone());
    files.push(legacy.file.clone());
    files.push(mcp.file);

    let (config_value, config_kind) = if settings.value.is_some() {
        (
            settings.value.as_ref(),
            ProviderNativeConfigFileKind::ClaudeSettingsJson,
        )
    } else {
        (
            legacy.value.as_ref(),
            ProviderNativeConfigFileKind::ClaudeLegacyJson,
        )
    };

    let Some(value) = config_value else {
        if settings.file.status == ProviderNativeConfigFileStatus::Missing
            && legacy.file.status == ProviderNativeConfigFileStatus::Missing
        {
            diagnostics.push(diagnostic(
                ProviderNativeImportSource::Claude,
                None,
                "provider_native_import_source_missing",
                "no Claude native config files were found",
                vec![option_entry("root", root.display().to_string())],
            ));
        }
        return Ok(());
    };

    let mut redacted_fields = Vec::new();
    let mut item_diagnostics = Vec::new();
    collect_json_secret_fields(
        value,
        ProviderNativeImportSource::Claude,
        config_kind,
        &mut redacted_fields,
    );
    add_unknown_json_fields(
        value,
        ProviderNativeImportSource::Claude,
        config_kind,
        &[
            "model",
            "default_model",
            "defaultModel",
            "base_url",
            "baseUrl",
            "apiBaseUrl",
            "account",
            "accountAlias",
        ],
        &mut item_diagnostics,
    );

    let secret_references = if json_has_secret(value) {
        vec![placeholder_secret(
            ProviderSecretKind::AuthToken,
            "ANTHROPIC_API_KEY",
            "Claude auth token from native config",
        )]
    } else {
        Vec::new()
    };
    let status = if secret_references.is_empty() {
        ProviderNativeImportItemStatus::Partial
    } else {
        ProviderNativeImportItemStatus::NeedsSecretSetup
    };
    let model = json_string(value, "model")
        .or_else(|| json_string(value, "default_model"))
        .or_else(|| json_string(value, "defaultModel"));
    let base_url = json_string(value, "base_url")
        .or_else(|| json_string(value, "baseUrl"))
        .or_else(|| json_string(value, "apiBaseUrl"));
    let account_alias =
        json_string(value, "accountAlias").or_else(|| json_string(value, "account"));

    let mut entries = vec![
        option_entry("nativeSource", "claude"),
        option_entry("nativeRoot", root.display().to_string()),
        option_entry("nativeConfigFile", format!("{config_kind:?}")),
    ];
    push_option_entry(
        &mut entries,
        "unknownFieldCount",
        unknown_json_count(value).map(|count| count.to_string()),
    );

    items.push(ProviderNativeImportItem {
        import_item_id: deterministic_request_id("claude_default"),
        source: ProviderNativeImportSource::Claude,
        provider_kind: ProviderKind::Claude,
        agent_id: Some(builtin_agent_id("claude")),
        display_name: account_alias
            .as_deref()
            .map(|alias| format!("Claude native {alias}"))
            .unwrap_or_else(|| "Claude native config".to_string()),
        account_alias,
        base_url,
        default_model: model,
        small_model: None,
        large_model: None,
        reasoning_effort: None,
        provider_options: ProviderOptions {
            schema_version: 1,
            entries,
        },
        secret_references,
        status,
        redacted_fields,
        diagnostics: item_diagnostics,
    });

    Ok(())
}

fn profile_request_from_item(item: ProviderNativeImportItem) -> ProviderProfileCreateRequest {
    let configured_models = item
        .default_model
        .iter()
        .chain(item.small_model.iter())
        .chain(item.large_model.iter())
        .map(|model| vibex_core::ProviderConfiguredModel {
            id: model.clone(),
            display_name: None,
            enabled: true,
            wire_api: None,
        })
        .collect();

    ProviderProfileCreateRequest {
        agent_id: item.agent_id,
        kind: item.provider_kind,
        display_name: item.display_name,
        account_alias: item.account_alias,
        base_url: item.base_url,
        default_model: item.default_model,
        small_model: item.small_model,
        large_model: item.large_model,
        configured_models,
        reasoning_effort: item.reasoning_effort,
        sandbox_defaults: None,
        network_defaults: None,
        permission_defaults: None,
        provider_options: Some(item.provider_options),
        secret_references: item.secret_references,
    }
}

fn is_cc_switch_import_item(item: &ProviderNativeImportItem) -> bool {
    provider_option_value(&item.provider_options, "nativeSource").as_deref() == Some("cc-switch")
}

fn cc_switch_import_identity(options: &ProviderOptions) -> Option<CcSwitchImportIdentity> {
    Some(CcSwitchImportIdentity {
        db_path: provider_option_value(options, CC_SWITCH_DB_PATH_OPTION_KEY)?,
        provider_id: provider_option_value(options, CC_SWITCH_PROVIDER_ID_OPTION_KEY)?,
        app_type: provider_option_value(options, CC_SWITCH_APP_TYPE_OPTION_KEY)?,
    })
}

fn migrate_cc_switch_secret_reference(
    item: &ProviderNativeImportItem,
    request: &mut ProviderProfileCreateRequest,
) -> VibexResult<CcSwitchSecretMigration> {
    let Some((env_key, secret_kind, secret_value)) = cc_switch_secret_for_import_item(item)? else {
        return Ok(CcSwitchSecretMigration::default());
    };
    let lookup_key = format!("vibex-provider-secret-{}", RequestId::new().as_str());
    if let Err(error) = secrets::store_provider_secret(&lookup_key, &secret_value) {
        ensure_missing_secret_placeholder(request, secret_kind, &env_key);
        return Ok(CcSwitchSecretMigration {
            migrated_lookup_key: None,
            diagnostics: vec![diagnostic(
                ProviderNativeImportSource::CcSwitch,
                Some(ProviderNativeConfigFileKind::CcSwitchDatabase),
                "provider_native_import_cc_switch_secret_keychain_unavailable",
                "CC Switch secret could not be stored in the OS keychain; imported profile keeps a missing secret placeholder",
                vec![
                    option_entry("backend", "os_keychain"),
                    option_entry("errorCode", error.code),
                ],
            )],
        });
    }

    request.secret_references.retain(|secret| {
        !(secret.secret_kind == secret_kind && secret.backend == ProviderSecretBackend::Placeholder)
    });
    request
        .secret_references
        .push(ProviderSecretReferenceCreateRequest {
            secret_kind,
            backend: ProviderSecretBackend::OsKeychain,
            setup_state: ProviderSecretSetupState::Available,
            lookup_key: lookup_key.clone(),
            display_label: env_key,
            redacted_hint: "stored in Vibex OS keychain".to_string(),
        });
    Ok(CcSwitchSecretMigration {
        migrated_lookup_key: Some(lookup_key),
        diagnostics: Vec::new(),
    })
}

fn ensure_missing_secret_placeholder(
    request: &mut ProviderProfileCreateRequest,
    secret_kind: ProviderSecretKind,
    env_key: &str,
) {
    if request
        .secret_references
        .iter()
        .any(|secret| secret.secret_kind == secret_kind)
    {
        return;
    }
    request
        .secret_references
        .push(placeholder_secret(secret_kind, env_key, env_key));
}

fn cc_switch_secret_for_import_item(
    item: &ProviderNativeImportItem,
) -> VibexResult<Option<(String, ProviderSecretKind, String)>> {
    let db_path = provider_option_value(&item.provider_options, CC_SWITCH_DB_PATH_OPTION_KEY)
        .ok_or_else(|| {
            VibexError::validation(
                "provider_native_import_cc_switch_metadata_missing",
                "CC Switch import item is missing its database path",
            )
        })?;
    let provider_id =
        provider_option_value(&item.provider_options, CC_SWITCH_PROVIDER_ID_OPTION_KEY)
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_native_import_cc_switch_metadata_missing",
                    "CC Switch import item is missing its provider id",
                )
            })?;
    let app_type = provider_option_value(&item.provider_options, CC_SWITCH_APP_TYPE_OPTION_KEY)
        .ok_or_else(|| {
            VibexError::validation(
                "provider_native_import_cc_switch_metadata_missing",
                "CC Switch import item is missing its app type",
            )
        })?;
    let settings = read_cc_switch_provider_settings(Path::new(&db_path), &app_type, &provider_id)?;
    let placeholder = item
        .secret_references
        .iter()
        .find(|secret| secret.backend == ProviderSecretBackend::Placeholder);
    let default_env_key =
        provider_option_value(&item.provider_options, CODEX_API_KEY_ENV_OPTION_KEY)
            .or_else(|| placeholder.map(|secret| secret.lookup_key.clone()))
            .or_else(|| default_cc_switch_secret_key(item.provider_kind).map(ToOwned::to_owned));
    let secret_kind = placeholder
        .map(|secret| secret.secret_kind)
        .unwrap_or_else(|| default_cc_switch_secret_kind(item.provider_kind));
    let Some(default_env_key) = default_env_key else {
        return Ok(None);
    };
    let Some(secret_value) = cc_switch_secret_value(&settings, &default_env_key)
        .or_else(|| cc_switch_first_secret(&settings).map(|(_key, value)| value))
    else {
        return Ok(None);
    };
    Ok(Some((default_env_key, secret_kind, secret_value)))
}

fn read_cc_switch_provider_settings(
    db_path: &Path,
    app_type: &str,
    provider_id: &str,
) -> VibexResult<JsonValue> {
    let connection = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        VibexError::storage(
            "provider_native_import_cc_switch_unreadable",
            "failed to open CC Switch provider database",
        )
        .with_diagnostic("path", db_path.display().to_string())
        .with_diagnostic("error", error.to_string())
    })?;
    let settings_config = connection
        .query_row(
            "SELECT settings_config FROM providers \
             WHERE lower(app_type) = ?1 AND id = ?2 LIMIT 1",
            (app_type, provider_id),
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| {
            VibexError::storage(
                "provider_native_import_cc_switch_read_failed",
                "failed to read CC Switch provider row",
            )
            .with_diagnostic("path", db_path.display().to_string())
            .with_diagnostic(CC_SWITCH_PROVIDER_ID_OPTION_KEY, provider_id)
            .with_diagnostic(CC_SWITCH_APP_TYPE_OPTION_KEY, app_type)
            .with_diagnostic("error", error.to_string())
        })?
        .ok_or_else(|| {
            VibexError::validation(
                "provider_native_import_item_not_found",
                "CC Switch import item was not found in the current provider database",
            )
            .with_diagnostic(CC_SWITCH_PROVIDER_ID_OPTION_KEY, provider_id)
            .with_diagnostic(CC_SWITCH_APP_TYPE_OPTION_KEY, app_type)
        })?;
    serde_json::from_str::<JsonValue>(&settings_config).map_err(|error| {
        VibexError::validation(
            "provider_native_import_cc_switch_provider_parse_failed",
            "failed to parse CC Switch provider settings",
        )
        .with_diagnostic(CC_SWITCH_PROVIDER_ID_OPTION_KEY, provider_id)
        .with_diagnostic(CC_SWITCH_APP_TYPE_OPTION_KEY, app_type)
        .with_diagnostic("error", error.to_string())
    })
}

fn normalize_sources(sources: Vec<ProviderNativeImportSource>) -> Vec<ProviderNativeImportSource> {
    if sources.is_empty() {
        return vec![
            ProviderNativeImportSource::Codex,
            ProviderNativeImportSource::Claude,
            ProviderNativeImportSource::CcSwitch,
        ];
    }

    let mut normalized = Vec::new();
    for source in sources {
        if !normalized.contains(&source) {
            normalized.push(source);
        }
    }
    normalized
}

fn dedupe_native_import_items(items: &mut Vec<ProviderNativeImportItem>) {
    let mut seen_item_ids = std::collections::HashSet::new();
    items.retain(|item| seen_item_ids.insert(item.import_item_id.clone()));
}

fn builtin_agent_id(value: &str) -> AgentId {
    AgentId::parse(value).expect("builtin Agent ids used by native import must be valid")
}

fn default_cc_switch_secret_key(provider_kind: ProviderKind) -> Option<&'static str> {
    match provider_kind {
        ProviderKind::Codex => Some("OPENAI_API_KEY"),
        ProviderKind::Claude => Some("ANTHROPIC_API_KEY"),
        ProviderKind::Acp => Some("OPENCODE_AUTH_TOKEN"),
    }
}

fn default_cc_switch_secret_kind(provider_kind: ProviderKind) -> ProviderSecretKind {
    match provider_kind {
        ProviderKind::Codex => ProviderSecretKind::ApiKey,
        ProviderKind::Claude | ProviderKind::Acp => ProviderSecretKind::AuthToken,
    }
}

fn read_json_file(
    source: ProviderNativeImportSource,
    kind: ProviderNativeConfigFileKind,
    path: PathBuf,
) -> ReadJsonResult {
    let path_string = path.display().to_string();
    if !path.exists() {
        return ReadJsonResult {
            value: None,
            file: config_file(
                source,
                kind,
                path_string,
                ProviderNativeConfigFileStatus::Missing,
                Vec::new(),
            ),
        };
    }

    match fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<JsonValue>(&raw) {
            Ok(value) => ReadJsonResult {
                value: Some(value),
                file: config_file(
                    source,
                    kind,
                    path_string,
                    ProviderNativeConfigFileStatus::Parsed,
                    Vec::new(),
                ),
            },
            Err(error) => {
                let diagnostics = vec![diagnostic(
                    source,
                    Some(kind),
                    "provider_native_import_parse_failed",
                    "failed to parse native JSON config",
                    vec![
                        option_entry("path", path_string.clone()),
                        option_entry("error", error.to_string()),
                    ],
                )];
                ReadJsonResult {
                    value: None,
                    file: config_file(
                        source,
                        kind,
                        path_string,
                        ProviderNativeConfigFileStatus::ParseError,
                        diagnostics,
                    ),
                }
            }
        },
        Err(error) => {
            let diagnostics = vec![diagnostic(
                source,
                Some(kind),
                "provider_native_import_file_unreadable",
                "failed to read native JSON config",
                vec![
                    option_entry("path", path_string.clone()),
                    option_entry("error", error.kind().to_string()),
                ],
            )];
            ReadJsonResult {
                value: None,
                file: config_file(
                    source,
                    kind,
                    path_string,
                    ProviderNativeConfigFileStatus::Unreadable,
                    diagnostics,
                ),
            }
        }
    }
}

fn read_toml_file(
    source: ProviderNativeImportSource,
    kind: ProviderNativeConfigFileKind,
    path: PathBuf,
) -> ReadTomlResult {
    let path_string = path.display().to_string();
    if !path.exists() {
        return ReadTomlResult {
            value: None,
            file: config_file(
                source,
                kind,
                path_string,
                ProviderNativeConfigFileStatus::Missing,
                Vec::new(),
            ),
        };
    }

    match fs::read_to_string(&path) {
        Ok(raw) => match raw.parse::<TomlValue>() {
            Ok(value) => ReadTomlResult {
                value: Some(value),
                file: config_file(
                    source,
                    kind,
                    path_string,
                    ProviderNativeConfigFileStatus::Parsed,
                    Vec::new(),
                ),
            },
            Err(error) => {
                let diagnostics = vec![diagnostic(
                    source,
                    Some(kind),
                    "provider_native_import_parse_failed",
                    "failed to parse native TOML config",
                    vec![
                        option_entry("path", path_string.clone()),
                        option_entry("error", error.to_string()),
                    ],
                )];
                ReadTomlResult {
                    value: None,
                    file: config_file(
                        source,
                        kind,
                        path_string,
                        ProviderNativeConfigFileStatus::ParseError,
                        diagnostics,
                    ),
                }
            }
        },
        Err(error) => {
            let diagnostics = vec![diagnostic(
                source,
                Some(kind),
                "provider_native_import_file_unreadable",
                "failed to read native TOML config",
                vec![
                    option_entry("path", path_string.clone()),
                    option_entry("error", error.kind().to_string()),
                ],
            )];
            ReadTomlResult {
                value: None,
                file: config_file(
                    source,
                    kind,
                    path_string,
                    ProviderNativeConfigFileStatus::Unreadable,
                    diagnostics,
                ),
            }
        }
    }
}

fn config_file(
    source: ProviderNativeImportSource,
    kind: ProviderNativeConfigFileKind,
    path: String,
    status: ProviderNativeConfigFileStatus,
    diagnostics: Vec<ProviderNativeImportDiagnostic>,
) -> ProviderNativeConfigFile {
    ProviderNativeConfigFile {
        source,
        kind,
        path,
        status,
        diagnostics,
    }
}

fn diagnostic(
    source: ProviderNativeImportSource,
    file_kind: Option<ProviderNativeConfigFileKind>,
    code: impl Into<String>,
    message: impl Into<String>,
    redacted_details: Vec<ProviderBindingMetadata>,
) -> ProviderNativeImportDiagnostic {
    ProviderNativeImportDiagnostic {
        code: code.into(),
        message: message.into(),
        source,
        file_kind,
        redacted_details,
    }
}

fn deterministic_request_id(suffix: &str) -> RequestId {
    RequestId::parse(format!("request_native_import_{suffix}"))
        .expect("deterministic native import ids must use request prefix")
}

fn codex_config_root() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

fn claude_config_root() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

fn claude_mcp_path() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".claude.json"))
        .unwrap_or_else(|| PathBuf::from(".claude.json"))
}

fn resolve_neighbor_path(root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn codex_provider_table<'a>(
    value: Option<&'a TomlValue>,
    provider_id: &str,
) -> Option<&'a TomlValue> {
    value?.get("model_providers")?.as_table()?.get(provider_id)
}

fn codex_import_provider_ids(
    value: Option<&TomlValue>,
    active_provider: Option<&str>,
) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(active_provider) = active_provider.filter(|value| !value.trim().is_empty()) {
        ids.push(active_provider.to_string());
    }
    if let Some(table) = value
        .and_then(|value| value.get("model_providers"))
        .and_then(TomlValue::as_table)
    {
        let mut keys = table.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            if !ids.iter().any(|id| id == &key) {
                ids.push(key);
            }
        }
    }
    ids
}

fn codex_provider_base_url(table: &TomlValue) -> Option<String> {
    toml_string(table.get("base_url"))
        .or_else(|| toml_string(table.get("baseURL")))
        .or_else(|| toml_string(table.get("api_base")))
}

fn codex_model_catalog_path(value: &TomlValue) -> Option<String> {
    toml_string(value.get("model_catalog_json"))
}

fn toml_string(value: Option<&TomlValue>) -> Option<String> {
    match value? {
        TomlValue::String(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        TomlValue::Integer(value) => Some(value.to_string()),
        TomlValue::Float(value) => Some(value.to_string()),
        TomlValue::Boolean(value) => Some(value.to_string()),
        _ => None,
    }
}

fn json_string(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn json_has_secret(value: &JsonValue) -> bool {
    match value {
        JsonValue::Object(map) => map
            .iter()
            .any(|(key, value)| is_secret_key(key) || json_has_secret(value)),
        JsonValue::Array(values) => values.iter().any(json_has_secret),
        _ => false,
    }
}

fn collect_json_secret_fields(
    value: &JsonValue,
    source: ProviderNativeImportSource,
    file_kind: ProviderNativeConfigFileKind,
    fields: &mut Vec<ProviderNativeImportRedactedField>,
) {
    if let JsonValue::Object(map) = value {
        for (key, value) in map {
            if is_secret_key(key) {
                fields.push(ProviderNativeImportRedactedField {
                    key: key.clone(),
                    source,
                    file_kind,
                    hint: secret_value_hint(value),
                });
            } else {
                collect_json_secret_fields(value, source, file_kind, fields);
            }
        }
    }
}

fn collect_toml_secret_fields(
    value: &TomlValue,
    source: ProviderNativeImportSource,
    file_kind: ProviderNativeConfigFileKind,
    fields: &mut Vec<ProviderNativeImportRedactedField>,
    secret_references: &mut Vec<vibex_core::ProviderSecretReferenceCreateRequest>,
) {
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            if is_secret_key(key) {
                fields.push(ProviderNativeImportRedactedField {
                    key: key.clone(),
                    source,
                    file_kind,
                    hint: "present".to_string(),
                });
                secret_references.push(placeholder_secret(
                    ProviderSecretKind::AuthToken,
                    key.clone(),
                    format!("Codex native {key}"),
                ));
            } else if let Some(nested) = value.as_table() {
                for (nested_key, nested_value) in nested {
                    if is_secret_key(nested_key) {
                        fields.push(ProviderNativeImportRedactedField {
                            key: format!("{key}.{nested_key}"),
                            source,
                            file_kind,
                            hint: secret_value_hint_toml(nested_value),
                        });
                    }
                }
            }
        }
    }
}

fn add_unknown_json_fields(
    value: &JsonValue,
    source: ProviderNativeImportSource,
    file_kind: ProviderNativeConfigFileKind,
    known: &[&str],
    diagnostics: &mut Vec<ProviderNativeImportDiagnostic>,
) {
    let Some(map) = value.as_object() else {
        return;
    };
    for key in map.keys().filter(|key| !known.contains(&key.as_str())) {
        diagnostics.push(diagnostic(
            source,
            Some(file_kind),
            "provider_native_import_unknown_field",
            "native config field is not mapped yet",
            vec![
                option_entry("field", key),
                option_entry("value", "<redacted>"),
            ],
        ));
    }
}

fn add_unknown_toml_fields(
    value: &TomlValue,
    source: ProviderNativeImportSource,
    file_kind: ProviderNativeConfigFileKind,
    known: &[&str],
    diagnostics: &mut Vec<ProviderNativeImportDiagnostic>,
) {
    let Some(map) = value.as_table() else {
        return;
    };
    for key in map.keys().filter(|key| !known.contains(&key.as_str())) {
        diagnostics.push(diagnostic(
            source,
            Some(file_kind),
            "provider_native_import_unknown_field",
            "native config field is not mapped yet",
            vec![
                option_entry("field", key),
                option_entry("value", "<redacted>"),
            ],
        ));
    }
}

fn unknown_json_count(value: &JsonValue) -> Option<usize> {
    value.as_object().map(|map| map.len())
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .collect::<String>()
        .to_ascii_lowercase();
    normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("bearer")
        || normalized.contains("oauth")
        || normalized.contains("authorization")
        || normalized.contains("privatekey")
        || normalized.contains("password")
}

fn secret_value_hint(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) if value.is_empty() => "empty".to_string(),
        JsonValue::String(_) => "present".to_string(),
        JsonValue::Null => "missing".to_string(),
        _ => "present".to_string(),
    }
}

fn secret_value_hint_toml(value: &TomlValue) -> String {
    match value {
        TomlValue::String(value) if value.is_empty() => "empty".to_string(),
        TomlValue::String(_) => "present".to_string(),
        _ => "present".to_string(),
    }
}

fn push_option_entry(
    entries: &mut Vec<ProviderBindingMetadata>,
    key: impl Into<String>,
    value: Option<String>,
) {
    if let Some(value) = value {
        entries.push(option_entry(key, value));
    }
}

fn cc_switch_import_suffix(mapping: &CcSwitchAgentMapping, row: &CcSwitchProviderRow) -> String {
    format!(
        "cc_switch_{}_{}_{}",
        sanitize_request_id_suffix(&mapping.app_type),
        sanitize_request_id_suffix(&row.provider_id),
        stable_request_id_hash(&[mapping.app_type.as_str(), row.provider_id.as_str()])
    )
}

fn stable_request_id_hash(parts: &[&str]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in parts {
        for byte in (part.len() as u64).to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

fn sanitize_request_id_suffix(value: &str) -> String {
    let mut sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while sanitized.contains("__") {
        sanitized = sanitized.replace("__", "_");
    }
    let sanitized = sanitized.trim_matches('_').to_string();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, fs};

    use tempfile::tempdir;
    use vibex_core::{
        ProviderNativeImportCreateRequest, ProviderNativeImportPreviewRequest,
        ProviderNativeImportSource,
    };

    use super::*;

    #[test]
    fn native_import_codex_preview_redacts_auth_and_maps_config() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"secret-value","auth_mode":"api_key"}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"model_provider = "vendor"
model = "gpt-5.4"

[model_providers.vendor]
base_url = "https://api.example.invalid/v1"
wire_api = "responses"
experimental_bearer_token = "do-not-copy"
"#,
        )
        .unwrap();

        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::Codex],
            },
            NativeImportRoots {
                codex_root: Some(dir.path().to_path_buf()),
                cc_switch_db_paths: Some(Vec::new()),
                ..NativeImportRoots::default()
            },
        )
        .unwrap();

        assert_eq!(preview.items.len(), 1);
        let item = &preview.items[0];
        assert_eq!(item.provider_kind, ProviderKind::Codex);
        assert_eq!(item.default_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(
            item.base_url.as_deref(),
            Some("https://api.example.invalid/v1")
        );
        assert!(!item.secret_references.is_empty());
        assert!(!format!("{preview:?}").contains("secret-value"));
        assert!(!format!("{preview:?}").contains("do-not-copy"));
    }

    #[test]
    fn native_import_codex_preview_maps_all_model_providers() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"secret-value","auth_mode":"api_key"}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"model_provider = "vendor_b"
model = "gpt-5.5"

[model_providers.vendor_a]
name = "Vendor Alpha"
base_url = "https://alpha.example.invalid/v1"
wire_api = "chat"

[model_providers.vendor_b]
name = "Vendor Beta"
base_url = "https://beta.example.invalid/v1"
wire_api = "responses"
"#,
        )
        .unwrap();

        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::Codex],
            },
            NativeImportRoots {
                codex_root: Some(dir.path().to_path_buf()),
                cc_switch_db_paths: Some(Vec::new()),
                ..NativeImportRoots::default()
            },
        )
        .unwrap();

        assert_eq!(preview.items.len(), 2);
        assert!(preview.items.iter().any(|item| {
            item.display_name == "Codex native Vendor Alpha"
                && item.base_url.as_deref() == Some("https://alpha.example.invalid/v1")
                && item.default_model.as_deref() == Some("gpt-5.5")
        }));
        assert!(preview.items.iter().any(|item| {
            item.display_name == "Codex native Vendor Beta"
                && item.base_url.as_deref() == Some("https://beta.example.invalid/v1")
                && item.default_model.as_deref() == Some("gpt-5.5")
        }));
        assert!(!format!("{preview:?}").contains("secret-value"));
    }

    #[test]
    fn native_import_cc_switch_preview_reads_supported_agent_providers() {
        let native_dir = tempdir().unwrap();
        let cc_switch_dir = tempdir().unwrap();
        let db_path = cc_switch_dir.path().join("cc-switch.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE providers (
                    id TEXT PRIMARY KEY,
                    app_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    settings_config TEXT NOT NULL,
                    website_url TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "codex-alpha",
                    "codex",
                    "Alpha",
                    r#"{"auth":{"OPENAI_API_KEY":"alpha-secret"},"config":"model_provider = \"alpha\"\nmodel = \"gpt-5.5\"\n\n[model_providers.alpha]\nname = \"Alpha\"\nbase_url = \"https://alpha.example.invalid/v1\"\nwire_api = \"responses\"\n"}"#,
                    "https://alpha.example.invalid",
                ),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "claude-alpha",
                    "claude",
                    "Claude Alpha",
                    r#"{"env":{"ANTHROPIC_BASE_URL":"https://claude.example.invalid","ANTHROPIC_AUTH_TOKEN":"claude-secret"},"model":"claude-sonnet"}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "codex-beta",
                    "codex",
                    "Beta",
                    r#"{"auth":{"OPENAI_API_KEY":"beta-secret"},"config":"model_provider = \"beta\"\nmodel = \"gpt-5.4\"\n\n[model_providers.beta]\nbase_url = \"https://beta.example.invalid/v1\"\n"}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "opencode-alpha",
                    "opencode",
                    "OpenCode Alpha",
                    r#"{"auth":{"OPENCODE_AUTH_TOKEN":"opencode-secret"},"model":"opencode-fast"}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "unknown-alpha",
                    "future-agent",
                    "Future Alpha",
                    r#"{"auth":{"FUTURE_API_KEY":"future-secret"}}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();

        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::CcSwitch],
            },
            NativeImportRoots {
                codex_root: Some(native_dir.path().to_path_buf()),
                cc_switch_db_paths: Some(vec![db_path]),
                ..NativeImportRoots::default()
            },
        )
        .unwrap();

        assert_eq!(preview.items.len(), 4);
        assert!(preview.items.iter().any(|item| {
            item.source == ProviderNativeImportSource::CcSwitch
                && item.agent_id.as_ref().map(|id| id.as_str()) == Some("codex")
                && item.provider_kind == ProviderKind::Codex
                && item.display_name == "Alpha"
                && item.base_url.as_deref() == Some("https://alpha.example.invalid/v1")
                && item.default_model.as_deref() == Some("gpt-5.5")
                && item
                    .provider_options
                    .entries
                    .iter()
                    .any(|entry| entry.key == "nativeSource" && entry.value == "cc-switch")
        }));
        let codex_alpha_item = preview
            .items
            .iter()
            .find(|item| {
                item.source == ProviderNativeImportSource::CcSwitch
                    && item.agent_id.as_ref().map(|id| id.as_str()) == Some("codex")
                    && item.display_name == "Alpha"
            })
            .unwrap();
        let mut option_keys = HashSet::new();
        for entry in &codex_alpha_item.provider_options.entries {
            assert!(
                option_keys.insert(entry.key.as_str()),
                "duplicate provider option key: {}",
                entry.key
            );
        }
        assert!(preview.items.iter().any(|item| {
            item.display_name == "Beta"
                && item.agent_id.as_ref().map(|id| id.as_str()) == Some("codex")
                && item.base_url.as_deref() == Some("https://beta.example.invalid/v1")
                && item.default_model.as_deref() == Some("gpt-5.4")
        }));
        assert!(preview.items.iter().any(|item| {
            item.display_name == "Claude Alpha"
                && item.agent_id.as_ref().map(|id| id.as_str()) == Some("claude")
                && item.provider_kind == ProviderKind::Claude
                && item.base_url.as_deref() == Some("https://claude.example.invalid")
                && item.default_model.as_deref() == Some("claude-sonnet")
        }));
        let opencode_item = preview
            .items
            .iter()
            .find(|item| item.display_name == "OpenCode Alpha")
            .unwrap();
        assert_eq!(
            opencode_item.agent_id.as_ref().map(|id| id.as_str()),
            Some("opencode")
        );
        assert_eq!(opencode_item.provider_kind, ProviderKind::Acp);
        assert!(
            opencode_item
                .provider_options
                .entries
                .iter()
                .any(|entry| entry.key == "acp.config.v1")
        );
        assert!(preview.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "provider_native_import_cc_switch_app_type_unmapped"
        }));
        let preview_debug = format!("{preview:?}");
        assert!(!preview_debug.contains("alpha-secret"));
        assert!(!preview_debug.contains("beta-secret"));
        assert!(!preview_debug.contains("claude-secret"));
        assert!(!preview_debug.contains("opencode-secret"));
        assert!(!preview_debug.contains("future-secret"));
    }

    #[test]
    fn native_import_cc_switch_preview_keeps_non_ascii_provider_ids_distinct() {
        let cc_switch_dir = tempdir().unwrap();
        let db_path = cc_switch_dir.path().join("cc-switch.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE providers (
                    id TEXT PRIMARY KEY,
                    app_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    settings_config TEXT NOT NULL,
                    website_url TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "君",
                    "codex",
                    "Jun",
                    r#"{"auth":{"OPENAI_API_KEY":"jun-secret"},"config":"model_provider = \"jun\"\nmodel = \"gpt-5.4\"\n\n[model_providers.jun]\nbase_url = \"https://jun.example.invalid/v1\"\n"}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "乙",
                    "codex",
                    "Yi",
                    r#"{"auth":{"OPENAI_API_KEY":"yi-secret"},"config":"model_provider = \"yi\"\nmodel = \"gpt-5.5\"\n\n[model_providers.yi]\nbase_url = \"https://yi.example.invalid/v1\"\n"}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();

        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::CcSwitch],
            },
            NativeImportRoots {
                cc_switch_db_paths: Some(vec![db_path]),
                ..NativeImportRoots::default()
            },
        )
        .unwrap();

        assert_eq!(preview.items.len(), 2);
        assert!(preview.items.iter().all(|item| {
            item.agent_id.as_ref().map(|id| id.as_str()) == Some("codex")
                && item.source == ProviderNativeImportSource::CcSwitch
        }));
        let unique_import_ids = preview
            .items
            .iter()
            .map(|item| item.import_item_id.clone())
            .collect::<HashSet<_>>();
        assert_eq!(unique_import_ids.len(), preview.items.len());
        assert!(preview.items.iter().any(|item| {
            item.account_alias.as_deref() == Some("君")
                && item.display_name == "Jun"
                && item.default_model.as_deref() == Some("gpt-5.4")
        }));
        assert!(preview.items.iter().any(|item| {
            item.account_alias.as_deref() == Some("乙")
                && item.display_name == "Yi"
                && item.default_model.as_deref() == Some("gpt-5.5")
        }));
    }

    #[test]
    fn native_import_create_migrates_cc_switch_secret_to_vibex_profile() {
        let native_dir = tempdir().unwrap();
        let cc_switch_dir = tempdir().unwrap();
        let db_path = cc_switch_dir.path().join("cc-switch.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE providers (
                    id TEXT PRIMARY KEY,
                    app_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    settings_config TEXT NOT NULL,
                    website_url TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "codex-alpha",
                    "codex",
                    "Alpha",
                    r#"{"auth":{"OPENAI_API_KEY":"alpha-secret"},"config":"model_provider = \"alpha\"\nmodel = \"gpt-5.5\"\n\n[model_providers.alpha]\nname = \"Alpha\"\nbase_url = \"https://alpha.example.invalid/v1\"\nwire_api = \"responses\"\n"}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();

        let roots = NativeImportRoots {
            codex_root: Some(native_dir.path().to_path_buf()),
            cc_switch_db_paths: Some(vec![db_path]),
            ..NativeImportRoots::default()
        };
        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::CcSwitch],
            },
            roots.clone(),
        )
        .unwrap();
        let item = preview.items[0].clone();
        assert_eq!(item.account_alias.as_deref(), Some("codex-alpha"));
        assert!(item.provider_options.entries.iter().any(|entry| {
            entry.key == CODEX_MODEL_PROVIDER_ID_OPTION_KEY && entry.value == "alpha"
        }));

        let vibex_dir = tempdir().unwrap();
        let vibex_db = vibex_dir.path().join("vibex.db");
        let service = ProviderConfigService::new(vibex_db);
        let result = service
            .create_profile_from_import_with_roots(
                ProviderNativeImportCreateRequest {
                    preview_request: ProviderNativeImportPreviewRequest {
                        sources: vec![ProviderNativeImportSource::CcSwitch],
                    },
                    import_item_id: item.import_item_id,
                },
                roots,
            )
            .unwrap();

        let profile = result.profile;
        assert_eq!(profile.agent_id.as_str(), "codex");
        assert_eq!(profile.account_alias.as_deref(), Some("codex-alpha"));
        assert_eq!(profile.default_model.as_deref(), Some("gpt-5.5"));
        assert_eq!(profile.secrets.len(), 1);
        assert_eq!(
            profile.secrets[0].backend,
            ProviderSecretBackend::OsKeychain
        );
        assert_eq!(
            profile.secrets[0].setup_state,
            ProviderSecretSetupState::Available
        );
        assert!(!profile.secrets[0].lookup_key.contains("alpha-secret"));
        assert!(!format!("{profile:?}").contains("alpha-secret"));
        assert_eq!(
            secrets::resolve_provider_secret(&profile.secrets[0])
                .unwrap()
                .as_deref(),
            Some("alpha-secret")
        );
    }

    #[test]
    fn native_import_create_keeps_placeholder_when_cc_switch_secret_keychain_store_fails() {
        let native_dir = tempdir().unwrap();
        let cc_switch_dir = tempdir().unwrap();
        let db_path = cc_switch_dir.path().join("cc-switch.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE providers (
                    id TEXT PRIMARY KEY,
                    app_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    settings_config TEXT NOT NULL,
                    website_url TEXT
                )",
                [],
            )
            .unwrap();
        let failing_secret = secrets::test_provider_secret_store_failure_value();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "codex-alpha",
                    "codex",
                    "Alpha",
                    format!(
                        r#"{{"auth":{{"OPENAI_API_KEY":"{failing_secret}"}},"config":"model_provider = \"alpha\"\nmodel = \"gpt-5.5\"\n\n[model_providers.alpha]\nname = \"Alpha\"\nbase_url = \"https://alpha.example.invalid/v1\"\nwire_api = \"responses\"\n"}}"#
                    ),
                    Option::<String>::None,
                ),
            )
            .unwrap();

        let roots = NativeImportRoots {
            codex_root: Some(native_dir.path().to_path_buf()),
            cc_switch_db_paths: Some(vec![db_path]),
            ..NativeImportRoots::default()
        };
        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::CcSwitch],
            },
            roots.clone(),
        )
        .unwrap();
        let item = preview.items[0].clone();

        let vibex_dir = tempdir().unwrap();
        let service = ProviderConfigService::new(vibex_dir.path().join("vibex.db"));
        let result = service
            .create_profile_from_import_with_roots(
                ProviderNativeImportCreateRequest {
                    preview_request: ProviderNativeImportPreviewRequest {
                        sources: vec![ProviderNativeImportSource::CcSwitch],
                    },
                    import_item_id: item.import_item_id,
                },
                roots,
            )
            .unwrap();

        let profile = result.profile;
        assert_eq!(profile.secrets.len(), 1);
        assert_eq!(
            profile.secrets[0].backend,
            ProviderSecretBackend::Placeholder
        );
        assert_eq!(
            profile.secrets[0].setup_state,
            ProviderSecretSetupState::Missing
        );
        assert_eq!(profile.secrets[0].lookup_key, "OPENAI_API_KEY");
        assert_eq!(
            secrets::resolve_provider_secret(&profile.secrets[0])
                .unwrap()
                .as_deref(),
            None
        );
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "provider_native_import_cc_switch_secret_keychain_unavailable"
                && diagnostic.redacted_details.iter().any(|entry| {
                    entry.key == "errorCode"
                        && entry.value == "provider_secret_keychain_store_failed"
                })
        }));
        assert!(!format!("{profile:?}").contains(failing_secret));
        assert!(!format!("{:?}", result.diagnostics).contains(failing_secret));
    }

    #[test]
    fn native_import_create_cc_switch_opencode_profile_uses_agent_id_and_acp_config() {
        let cc_switch_dir = tempdir().unwrap();
        let db_path = cc_switch_dir.path().join("cc-switch.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE providers (
                    id TEXT PRIMARY KEY,
                    app_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    settings_config TEXT NOT NULL,
                    website_url TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "opencode-alpha",
                    "opencode",
                    "OpenCode Alpha",
                    r#"{"model":"opencode-fast"}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();

        let roots = NativeImportRoots {
            cc_switch_db_paths: Some(vec![db_path]),
            ..NativeImportRoots::default()
        };
        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::CcSwitch],
            },
            roots.clone(),
        )
        .unwrap();
        let item = preview.items[0].clone();
        assert_eq!(item.provider_kind, ProviderKind::Acp);
        assert_eq!(
            item.agent_id.as_ref().map(|id| id.as_str()),
            Some("opencode")
        );

        let vibex_dir = tempdir().unwrap();
        let service = ProviderConfigService::new(vibex_dir.path().join("vibex.db"));
        let result = service
            .create_profile_from_import_with_roots(
                ProviderNativeImportCreateRequest {
                    preview_request: ProviderNativeImportPreviewRequest {
                        sources: vec![ProviderNativeImportSource::CcSwitch],
                    },
                    import_item_id: item.import_item_id,
                },
                roots,
            )
            .unwrap();

        let profile = result.profile;
        assert_eq!(profile.kind, ProviderKind::Acp);
        assert_eq!(profile.agent_id.as_str(), "opencode");
        assert_eq!(profile.default_model.as_deref(), Some("opencode-fast"));
        assert!(
            profile
                .provider_options
                .entries
                .iter()
                .any(|entry| entry.key == "acp.config.v1")
        );
    }

    #[test]
    fn native_import_create_cc_switch_profile_is_idempotent() {
        let cc_switch_dir = tempdir().unwrap();
        let db_path = cc_switch_dir.path().join("cc-switch.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE providers (
                    id TEXT PRIMARY KEY,
                    app_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    settings_config TEXT NOT NULL,
                    website_url TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO providers (id, app_type, name, settings_config, website_url)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    "codex-alpha",
                    "codex",
                    "Alpha",
                    r#"{"config":"model_provider = \"alpha\"\nmodel = \"gpt-5.5\"\n\n[model_providers.alpha]\nname = \"Alpha\"\nbase_url = \"https://alpha.example.invalid/v1\"\nwire_api = \"responses\"\n"}"#,
                    Option::<String>::None,
                ),
            )
            .unwrap();

        let roots = NativeImportRoots {
            cc_switch_db_paths: Some(vec![db_path.clone()]),
            ..NativeImportRoots::default()
        };
        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::CcSwitch],
            },
            roots.clone(),
        )
        .unwrap();
        let item = preview.items[0].clone();

        let service = ProviderConfigService::new(cc_switch_dir.path().join("vibex.db"));
        let first = service
            .create_profile_from_import_with_roots(
                ProviderNativeImportCreateRequest {
                    preview_request: ProviderNativeImportPreviewRequest {
                        sources: vec![ProviderNativeImportSource::CcSwitch],
                    },
                    import_item_id: item.import_item_id.clone(),
                },
                roots.clone(),
            )
            .unwrap();
        let second = service
            .create_profile_from_import_with_roots(
                ProviderNativeImportCreateRequest {
                    preview_request: ProviderNativeImportPreviewRequest {
                        sources: vec![ProviderNativeImportSource::CcSwitch],
                    },
                    import_item_id: item.import_item_id,
                },
                roots,
            )
            .unwrap();

        assert_eq!(second.profile.id, first.profile.id);
        let profiles = service.list_profiles().unwrap();
        let matching_profiles = profiles
            .iter()
            .filter(|profile| {
                profile.agent_id.as_str() == "codex"
                    && profile.kind == ProviderKind::Acp
                    && provider_option_value(
                        &profile.provider_options,
                        CC_SWITCH_DB_PATH_OPTION_KEY,
                    ) == Some(db_path.display().to_string())
                    && provider_option_value(
                        &profile.provider_options,
                        CC_SWITCH_PROVIDER_ID_OPTION_KEY,
                    )
                    .as_deref()
                        == Some("codex-alpha")
                    && provider_option_value(
                        &profile.provider_options,
                        CC_SWITCH_APP_TYPE_OPTION_KEY,
                    )
                    .as_deref()
                        == Some("codex")
            })
            .count();
        assert_eq!(matching_profiles, 1);
        assert_eq!(first.profile.kind, ProviderKind::Acp);
        assert_eq!(second.profile.kind, ProviderKind::Acp);
    }

    #[test]
    fn native_import_handles_malformed_files_as_diagnostics() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("config.toml"), "model_provider = ").unwrap();

        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::Codex],
            },
            NativeImportRoots {
                codex_root: Some(dir.path().to_path_buf()),
                cc_switch_db_paths: Some(Vec::new()),
                ..NativeImportRoots::default()
            },
        )
        .unwrap();

        assert!(preview.items.is_empty());
        assert!(
            preview
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "provider_native_import_parse_failed" })
        );
        assert!(preview.files.iter().any(|file| {
            file.kind == ProviderNativeConfigFileKind::CodexConfigToml
                && file.status == ProviderNativeConfigFileStatus::ParseError
        }));
    }

    #[test]
    fn native_import_claude_preview_uses_settings_json() {
        let dir = tempdir().unwrap();
        let mcp = dir.path().join(".claude.json");
        fs::write(
            dir.path().join("settings.json"),
            r#"{"model":"claude-sonnet","accountAlias":"work","ANTHROPIC_API_KEY":"hidden"}"#,
        )
        .unwrap();

        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::Claude],
            },
            NativeImportRoots {
                claude_root: Some(dir.path().to_path_buf()),
                claude_mcp_path: Some(mcp),
                cc_switch_db_paths: Some(Vec::new()),
                ..NativeImportRoots::default()
            },
        )
        .unwrap();

        let item = preview.items.first().unwrap();
        assert_eq!(item.provider_kind, ProviderKind::Claude);
        assert_eq!(item.default_model.as_deref(), Some("claude-sonnet"));
        assert_eq!(item.account_alias.as_deref(), Some("work"));
        assert!(!format!("{preview:?}").contains("hidden"));
    }

    #[test]
    fn native_import_create_profile_does_not_modify_native_files() {
        let native_dir = tempdir().unwrap();
        let db_dir = tempdir().unwrap();
        let config_path = native_dir.path().join("config.toml");
        let auth_path = native_dir.path().join("auth.json");
        fs::write(&config_path, "model = \"gpt-5.4\"\n").unwrap();
        fs::write(&auth_path, r#"{"OPENAI_API_KEY":"secret-value"}"#).unwrap();
        let original_config = fs::read_to_string(&config_path).unwrap();
        let original_auth = fs::read_to_string(&auth_path).unwrap();

        let service = ProviderConfigService::new(db_dir.path().join("vibex.db"));
        let preview = preview_native_import_with_roots(
            ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::Codex],
            },
            NativeImportRoots {
                codex_root: Some(native_dir.path().to_path_buf()),
                cc_switch_db_paths: Some(Vec::new()),
                ..NativeImportRoots::default()
            },
        )
        .unwrap();
        let item = preview.items.first().unwrap().clone();
        let profile = service
            .create_profile(profile_request_from_item(item.clone()))
            .unwrap();

        assert_eq!(profile.kind, ProviderKind::Codex);
        assert_eq!(fs::read_to_string(&config_path).unwrap(), original_config);
        assert_eq!(fs::read_to_string(&auth_path).unwrap(), original_auth);

        let create_request = ProviderNativeImportCreateRequest {
            preview_request: ProviderNativeImportPreviewRequest {
                sources: vec![ProviderNativeImportSource::Codex],
            },
            import_item_id: item.import_item_id,
        };
        assert_eq!(
            create_request.preview_request.sources,
            vec![ProviderNativeImportSource::Codex]
        );
    }
}
