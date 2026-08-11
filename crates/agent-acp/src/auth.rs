use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use agent_client_protocol_schema::v1::AuthMethod as SchemaAuthMethod;
use serde::Deserialize;
use serde_json::Value;
use vibex_core::{
    AgentAuthCatalog, AgentAuthEnvironmentVariable, AgentAuthMethod, AgentAuthMethodKind,
    AgentAuthStatus, AgentId, unix_timestamp_ms,
};

const AUTH_METHOD_LIMIT: usize = 32;
const AUTH_ENV_LIMIT: usize = 32;
const AUTH_TEXT_LIMIT: usize = 512;
const AUTH_METHOD_ID_LIMIT: usize = 512;
const AUTH_ENV_KEY_LIMIT: usize = 256;
const AUTH_CREDENTIAL_LINK_LIMIT: usize = 4096;
const AUTH_TERMINAL_ARG_LIMIT: usize = 128;
const AUTH_TERMINAL_ENV_LIMIT: usize = 64;
const AUTH_TERMINAL_ARG_TEXT_LIMIT: usize = 4096;
const AUTH_TERMINAL_ENV_VALUE_LIMIT: usize = 16 * 1024;
const AUTH_TERMINAL_COMMAND_LIMIT: usize = 4096;
const KNOWN_TERMINAL_LOGIN_METHOD_ID: &str = "vibex-cli-login";

#[derive(Clone)]
pub(crate) struct AcpTerminalAuthSpec {
    pub title: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

impl fmt::Debug for AcpTerminalAuthSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpTerminalAuthSpec")
            .field("title", &crate::redact_summary(&self.title))
            .field("has_command", &!self.command.trim().is_empty())
            .field("argument_count", &self.args.len())
            .field("has_cwd", &self.cwd.is_some())
            .field(
                "env_keys",
                &self.env.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct ParsedAcpAuthCatalog {
    pub catalog: AgentAuthCatalog,
    pub terminal_methods: BTreeMap<String, AcpTerminalAuthSpec>,
}

pub(crate) fn parse_initialize_auth_catalog(
    agent_id: AgentId,
    initialize: &Value,
    command: &str,
    base_args: &[String],
    base_env: &[(String, String)],
    cwd: Option<String>,
) -> ParsedAcpAuthCatalog {
    let supports_logout = initialize
        .get("agentCapabilities")
        .and_then(|capabilities| capabilities.get("auth"))
        .and_then(|auth| auth.get("logout"))
        .is_some_and(Value::is_object);
    let mut seen_ids = BTreeSet::new();
    let mut methods = Vec::new();
    let mut terminal_methods = BTreeMap::new();

    for value in initialize
        .get("authMethods")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(AUTH_METHOD_LIMIT)
    {
        let Ok(method) = serde_json::from_value::<SchemaAuthMethod>(value.clone()) else {
            continue;
        };
        let id = method.id().0.as_ref().to_string();
        let name = bounded_text(method.name());
        if !valid_method_id(&id) || name.is_empty() || !seen_ids.insert(id.clone()) {
            continue;
        }
        let description = method
            .description()
            .map(bounded_text)
            .filter(|value| !value.is_empty());
        let legacy_terminal = legacy_terminal_spec(&method, cwd.clone());

        let (kind, environment, credential_link, terminal) = match method {
            SchemaAuthMethod::EnvVar(method) => {
                let environment = method
                    .vars
                    .into_iter()
                    .filter_map(|variable| {
                        let name = variable.name.trim().to_string();
                        valid_env_key(&name).then(|| AgentAuthEnvironmentVariable {
                            name,
                            label: variable
                                .label
                                .map(|label| bounded_text(&label))
                                .filter(|label| !label.is_empty()),
                            secret: variable.secret,
                            optional: variable.optional,
                            configured: false,
                        })
                    })
                    .take(AUTH_ENV_LIMIT)
                    .collect::<Vec<_>>();
                if environment.is_empty() {
                    continue;
                }
                (
                    AgentAuthMethodKind::Environment,
                    environment,
                    method.link.filter(|link| valid_credential_link(link)),
                    legacy_terminal,
                )
            }
            SchemaAuthMethod::Terminal(method) => {
                let terminal = standard_terminal_spec(
                    &name,
                    command,
                    base_args,
                    base_env,
                    method.args,
                    method.env.into_iter().collect(),
                    cwd.clone(),
                );
                let Some(terminal) = terminal else {
                    continue;
                };
                (
                    AgentAuthMethodKind::Terminal,
                    Vec::new(),
                    None,
                    Some(terminal),
                )
            }
            SchemaAuthMethod::Agent(_) => (
                if legacy_terminal.is_some() {
                    AgentAuthMethodKind::Terminal
                } else {
                    AgentAuthMethodKind::Agent
                },
                Vec::new(),
                None,
                legacy_terminal,
            ),
            _ => continue,
        };
        if let Some(terminal) = terminal {
            terminal_methods.insert(id.clone(), terminal);
        }
        methods.push(AgentAuthMethod {
            id,
            name,
            description,
            kind,
            environment,
            credential_link,
        });
    }

    ParsedAcpAuthCatalog {
        catalog: AgentAuthCatalog {
            agent_id,
            methods,
            supports_logout,
            status: AgentAuthStatus::Unknown,
            refreshed_at_ms: unix_timestamp_ms(),
        },
        terminal_methods,
    }
}

pub(crate) fn append_known_terminal_auth_fallback(
    parsed: &mut ParsedAcpAuthCatalog,
    agent_id: &AgentId,
    command: &str,
    base_args: &[String],
    base_env: &[(String, String)],
    cwd: Option<String>,
) -> bool {
    if parsed
        .catalog
        .methods
        .iter()
        .any(|method| method.id == KNOWN_TERMINAL_LOGIN_METHOD_ID)
    {
        return false;
    }
    let (acp_mode, title, description) = match agent_id.as_str() {
        "auggie" => (
            "--acp",
            "Sign in to Augment CLI",
            "Open the Augment CLI login flow in a terminal.",
        ),
        "kiro" => (
            "acp",
            "Sign in to Kiro CLI",
            "Open the Kiro CLI login flow in a terminal.",
        ),
        _ => return false,
    };
    let mut login_args = base_args.to_vec();
    let Some(mode) = login_args.last_mut() else {
        return false;
    };
    if mode != acp_mode {
        return false;
    }
    *mode = "login".to_string();
    let Some(terminal) = standard_terminal_spec(
        title,
        command,
        &login_args,
        base_env,
        Vec::new(),
        BTreeMap::new(),
        cwd,
    ) else {
        return false;
    };
    parsed.catalog.methods.push(AgentAuthMethod {
        id: KNOWN_TERMINAL_LOGIN_METHOD_ID.to_string(),
        name: title.to_string(),
        description: Some(description.to_string()),
        kind: AgentAuthMethodKind::Terminal,
        environment: Vec::new(),
        credential_link: None,
    });
    parsed
        .terminal_methods
        .insert(KNOWN_TERMINAL_LOGIN_METHOD_ID.to_string(), terminal);
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyTerminalAuth {
    label: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

fn legacy_terminal_spec(
    method: &SchemaAuthMethod,
    cwd: Option<String>,
) -> Option<AcpTerminalAuthSpec> {
    let meta = serde_json::to_value(method.meta()?).ok()?;
    let auth =
        serde_json::from_value::<LegacyTerminalAuth>(meta.get("terminal-auth")?.clone()).ok()?;
    let command = auth.command.trim();
    if command.is_empty() || command.contains(['\0', '\n', '\r']) {
        return None;
    }
    let title = bounded_text(&auth.label);
    standard_terminal_spec(&title, command, &[], &[], auth.args, auth.env, cwd)
}

fn bounded_text(value: &str) -> String {
    value.trim().chars().take(AUTH_TEXT_LIMIT).collect()
}

fn valid_env_key(value: &str) -> bool {
    if value.chars().count() > AUTH_ENV_KEY_LIMIT {
        return false;
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn valid_credential_link(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= AUTH_CREDENTIAL_LINK_LIMIT
        && !value.chars().any(char::is_control)
        && matches!(value.split(':').next(), Some("https" | "http"))
}

fn valid_method_id(value: &str) -> bool {
    !value.is_empty()
        && !value.trim().is_empty()
        && value.chars().count() <= AUTH_METHOD_ID_LIMIT
        && !value.chars().any(char::is_control)
}

fn standard_terminal_spec(
    title: &str,
    command: &str,
    base_args: &[String],
    base_env: &[(String, String)],
    additional_args: Vec<String>,
    additional_env: BTreeMap<String, String>,
    cwd: Option<String>,
) -> Option<AcpTerminalAuthSpec> {
    if title.is_empty()
        || command.is_empty()
        || command.chars().count() > AUTH_TERMINAL_COMMAND_LIMIT
        || command.chars().any(char::is_control)
        || additional_args.len() > AUTH_TERMINAL_ARG_LIMIT
        || additional_env.len() > AUTH_TERMINAL_ENV_LIMIT
        || additional_args.iter().any(|arg| {
            arg.is_empty()
                || arg.chars().count() > AUTH_TERMINAL_ARG_TEXT_LIMIT
                || arg.contains('\0')
        })
        || additional_env.iter().any(|(key, value)| {
            !valid_env_key(key)
                || value.chars().count() > AUTH_TERMINAL_ENV_VALUE_LIMIT
                || value.contains('\0')
        })
    {
        return None;
    }

    let mut args = base_args.to_vec();
    args.extend(additional_args);
    let mut env = base_env.to_vec();
    for (key, value) in additional_env {
        if let Some(existing) = env.iter_mut().find(|(current, _)| current == &key) {
            existing.1 = value;
        } else {
            env.push((key, value));
        }
    }
    Some(AcpTerminalAuthSpec {
        title: title.to_string(),
        command: command.to_string(),
        args,
        cwd,
        env,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn known_cli_login_fallbacks_replace_the_acp_mode_argument() {
        for (agent_id, base_args, expected_args) in [
            (
                "auggie",
                vec!["/managed/augment.mjs".to_string(), "--acp".to_string()],
                vec!["/managed/augment.mjs".to_string(), "login".to_string()],
            ),
            ("kiro", vec!["acp".to_string()], vec!["login".to_string()]),
        ] {
            let agent_id = AgentId::parse(agent_id).unwrap();
            let mut parsed = parse_initialize_auth_catalog(
                agent_id.clone(),
                &json!({ "protocolVersion": 1, "authMethods": [] }),
                "/managed/agent",
                &base_args,
                &[],
                Some("/workspace".to_string()),
            );

            assert!(append_known_terminal_auth_fallback(
                &mut parsed,
                &agent_id,
                "/managed/agent",
                &base_args,
                &[],
                Some("/workspace".to_string()),
            ));
            assert_eq!(parsed.catalog.methods.len(), 1);
            assert_eq!(
                parsed.catalog.methods[0].kind,
                AgentAuthMethodKind::Terminal
            );
            assert_eq!(
                parsed
                    .terminal_methods
                    .get(KNOWN_TERMINAL_LOGIN_METHOD_ID)
                    .unwrap()
                    .args,
                expected_args
            );
        }
    }

    #[test]
    fn known_cli_login_fallbacks_fail_closed_for_unexpected_commands() {
        let agent_id = AgentId::parse("kiro").unwrap();
        let mut parsed = parse_initialize_auth_catalog(
            agent_id.clone(),
            &json!({ "protocolVersion": 1, "authMethods": [] }),
            "/managed/kiro-cli",
            &["serve".to_string()],
            &[],
            None,
        );

        assert!(!append_known_terminal_auth_fallback(
            &mut parsed,
            &agent_id,
            "/managed/kiro-cli",
            &["serve".to_string()],
            &[],
            None,
        ));
        assert!(parsed.catalog.methods.is_empty());
    }

    #[test]
    fn parses_all_auth_method_shapes_without_exposing_terminal_env_values() {
        let parsed = parse_initialize_auth_catalog(
            AgentId::parse("gemini").unwrap(),
            &json!({
                "protocolVersion": 1,
                "agentCapabilities": { "auth": { "logout": {} } },
                "authMethods": [
                    { "id": "oauth", "name": "Browser login", "description": "Sign in" },
                    {
                        "type": "env_var",
                        "id": "api-key",
                        "name": "API key",
                        "vars": [
                            { "name": "GEMINI_API_KEY", "label": "Gemini API key" },
                            { "name": "GOOGLE_CLOUD_PROJECT", "secret": false, "optional": true }
                        ],
                        "link": "https://example.invalid/keys"
                    },
                    {
                        "type": "terminal",
                        "id": "terminal",
                        "name": "Terminal login",
                        "args": ["auth", "login"],
                        "env": { "AUTH_TOKEN": "never-render-me" }
                    }
                ]
            }),
            "gemini",
            &["--stdio".to_string()],
            &[("PROFILE_ENV".to_string(), "redacted-base".to_string())],
            Some("/workspace".to_string()),
        );

        assert!(parsed.catalog.supports_logout);
        assert_eq!(parsed.catalog.methods.len(), 3);
        assert_eq!(
            parsed.catalog.methods[1].kind,
            AgentAuthMethodKind::Environment
        );
        assert_eq!(
            parsed.catalog.methods[1].environment[0].name,
            "GEMINI_API_KEY"
        );
        assert_eq!(
            parsed.catalog.methods[2].kind,
            AgentAuthMethodKind::Terminal
        );
        let debug = format!("{:?}", parsed.terminal_methods["terminal"]);
        assert!(debug.contains("AUTH_TOKEN"));
        assert!(!debug.contains("never-render-me"));
        assert!(!debug.contains("/workspace"));
        assert!(!debug.contains("--stdio"));
        assert_eq!(
            parsed.terminal_methods["terminal"].args,
            vec!["--stdio", "auth", "login"]
        );
        assert_eq!(
            parsed.terminal_methods["terminal"].env,
            vec![
                ("PROFILE_ENV".to_string(), "redacted-base".to_string()),
                ("AUTH_TOKEN".to_string(), "never-render-me".to_string())
            ]
        );
    }

    #[test]
    fn skips_duplicate_and_invalid_methods() {
        let parsed = parse_initialize_auth_catalog(
            AgentId::parse("custom").unwrap(),
            &json!({
                "authMethods": [
                    { "id": "login", "name": "Login" },
                    { "id": "login", "name": "Duplicate" },
                    { "type": "env_var", "id": "broken", "name": "Broken", "vars": [
                        { "name": "NOT-AN-ENV" }
                    ]},
                    { "type": "future", "id": "future", "name": "Future" }
                ]
            }),
            "custom-agent",
            &[],
            &[],
            None,
        );

        assert_eq!(parsed.catalog.methods.len(), 2);
        assert_eq!(parsed.catalog.methods[1].id, "future");
        assert!(!parsed.catalog.supports_logout);
    }

    #[test]
    fn preserves_method_ids_exactly_and_rejects_invalid_ids() {
        let exact = "oauth/browser:v1";
        let oversized = "x".repeat(AUTH_METHOD_ID_LIMIT + 1);
        let parsed = parse_initialize_auth_catalog(
            AgentId::parse("custom").unwrap(),
            &json!({
                "authMethods": [
                    { "id": exact, "name": "Exact" },
                    { "id": oversized, "name": "Too long" },
                    { "id": "bad\nid", "name": "Control" }
                ]
            }),
            "custom-agent",
            &[],
            &[],
            None,
        );

        assert_eq!(parsed.catalog.methods.len(), 1);
        assert_eq!(parsed.catalog.methods[0].id, exact);
    }

    #[test]
    fn legacy_terminal_auth_keeps_its_explicit_command_environment() {
        let parsed = parse_initialize_auth_catalog(
            AgentId::parse("custom").unwrap(),
            &json!({
                "authMethods": [{
                    "id": "legacy",
                    "name": "Legacy",
                    "_meta": {
                        "terminal-auth": {
                            "label": "Legacy login",
                            "command": "legacy-login",
                            "args": ["--interactive"],
                            "env": { "LEGACY_ENV": "legacy-value" }
                        }
                    }
                }]
            }),
            "custom-agent",
            &["--stdio".to_string()],
            &[("PROFILE_ENV".to_string(), "profile-value".to_string())],
            None,
        );

        let terminal = &parsed.terminal_methods["legacy"];
        assert_eq!(terminal.command, "legacy-login");
        assert_eq!(terminal.args, vec!["--interactive"]);
        assert_eq!(
            terminal.env,
            vec![("LEGACY_ENV".to_string(), "legacy-value".to_string())]
        );
    }

    #[test]
    fn rejects_oversized_terminal_auth_payloads() {
        let oversized_args = vec!["arg"; AUTH_TERMINAL_ARG_LIMIT + 1];
        let parsed = parse_initialize_auth_catalog(
            AgentId::parse("custom").unwrap(),
            &json!({
                "authMethods": [{
                    "type": "terminal",
                    "id": "terminal",
                    "name": "Terminal",
                    "args": oversized_args
                }]
            }),
            "custom-agent",
            &[],
            &[],
            None,
        );

        assert!(parsed.catalog.methods.is_empty());
        assert!(parsed.terminal_methods.is_empty());
    }

    #[test]
    fn rejects_oversized_environment_keys_and_credential_links() {
        let oversized_key = format!("A{}", "B".repeat(AUTH_ENV_KEY_LIMIT));
        let oversized_link = format!(
            "https://example.invalid/{}",
            "x".repeat(AUTH_CREDENTIAL_LINK_LIMIT)
        );
        let parsed = parse_initialize_auth_catalog(
            AgentId::parse("custom").unwrap(),
            &json!({
                "authMethods": [{
                    "type": "env_var",
                    "id": "environment",
                    "name": "Environment",
                    "vars": [
                        { "name": oversized_key },
                        { "name": "VALID_API_KEY" }
                    ],
                    "link": oversized_link
                }]
            }),
            "custom-agent",
            &[],
            &[],
            None,
        );

        assert_eq!(parsed.catalog.methods.len(), 1);
        assert_eq!(parsed.catalog.methods[0].environment.len(), 1);
        assert_eq!(
            parsed.catalog.methods[0].environment[0].name,
            "VALID_API_KEY"
        );
        assert!(parsed.catalog.methods[0].credential_link.is_none());
    }

    #[test]
    fn logout_requires_the_schema_capability_object() {
        for value in [Value::Null, Value::Bool(false), Value::Bool(true)] {
            let parsed = parse_initialize_auth_catalog(
                AgentId::parse("custom").unwrap(),
                &json!({
                    "agentCapabilities": { "auth": { "logout": value } },
                    "authMethods": []
                }),
                "custom-agent",
                &[],
                &[],
                None,
            );
            assert!(!parsed.catalog.supports_logout);
        }
    }
}
