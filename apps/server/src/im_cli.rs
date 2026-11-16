//! `vibex-server im` — administer IM channels from a headless deployment.
//!
//! The runtime owns the live connection, but a container operator has no
//! settings page. These commands edit the same SQLite rows the runtime reads
//! and use the same service the desktop calls, so a pairing code minted here is
//! indistinguishable from one minted in the app.
//!
//! The service is built with [`AdministrativeAgent`]: listing, pairing, and
//! revoking never dispatch a turn, and starting a runtime just to satisfy the
//! signature would take the home lock away from the daemon that is actually
//! serving.

use std::sync::Arc;

use vibex_core::{ImChannelId, ImPairingRequest, ImPrincipalRole};
use vibex_desktop_runtime::DesktopRuntimeConfig;
use vibex_im_channel::{AdministrativeAgent, ImChannelService, ImChannelServiceConfig};

/// One parsed `im` subcommand.
#[derive(Debug)]
pub enum ImCommand {
    List {
        json: bool,
    },
    Pair {
        channel: String,
        role: ImPrincipalRole,
        ttl_ms: Option<u32>,
        scope: vibex_core::ImPairingScope,
        group_chat: Option<String>,
    },
    Revoke {
        channel: String,
        principal: String,
    },
    Bindings {
        channel: String,
    },
    Unbind {
        binding: String,
    },
    Test {
        channel: String,
    },
    Enable {
        channel: String,
        enabled: bool,
    },
}

impl ImCommand {
    /// Parses the arguments after `im`.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let Some(action) = args.first().map(String::as_str) else {
            return Err(
                "usage: vibex-server im <list|pair|revoke|bindings|unbind|test|enable|disable>"
                    .to_string(),
            );
        };
        let rest = &args[1..];
        let flag = |name: &str| -> Option<String> {
            rest.iter()
                .position(|arg| arg == name)
                .and_then(|index| rest.get(index + 1))
                .cloned()
        };
        match action {
            "list" => Ok(Self::List {
                json: rest.iter().any(|arg| arg == "--json"),
            }),
            "pair" => {
                let channel = flag("--channel").ok_or("pair needs --channel <id|name>")?;
                let role = match flag("--role").as_deref() {
                    None | Some("operator") => ImPrincipalRole::Operator,
                    Some("observer") => ImPrincipalRole::Observer,
                    Some("approver") => ImPrincipalRole::Approver,
                    Some("admin") => ImPrincipalRole::Admin,
                    Some(other) => {
                        return Err(format!(
                            "unknown role {other}; use observer, approver, operator, or admin"
                        ));
                    }
                };
                let ttl_ms = flag("--ttl-minutes")
                    .map(|value| {
                        value
                            .parse::<u32>()
                            .map(|minutes| minutes.saturating_mul(60_000))
                            .map_err(|_| "--ttl-minutes must be a number".to_string())
                    })
                    .transpose()?;
                let group_chat = flag("--chat");
                let scope = if group_chat.is_some() {
                    vibex_core::ImPairingScope::Group
                } else {
                    vibex_core::ImPairingScope::Direct
                };
                Ok(Self::Pair {
                    channel,
                    role,
                    ttl_ms,
                    scope,
                    group_chat,
                })
            }
            "revoke" => Ok(Self::Revoke {
                channel: flag("--channel").ok_or("revoke needs --channel <id|name>")?,
                principal: flag("--principal").ok_or("revoke needs --principal <id>")?,
            }),
            "bindings" => Ok(Self::Bindings {
                channel: flag("--channel").ok_or("bindings needs --channel <id|name>")?,
            }),
            "unbind" => Ok(Self::Unbind {
                binding: flag("--binding").ok_or("unbind needs --binding <id>")?,
            }),
            "test" => Ok(Self::Test {
                channel: flag("--channel").ok_or("test needs --channel <id|name>")?,
            }),
            "enable" => Ok(Self::Enable {
                channel: flag("--channel").ok_or("enable needs --channel <id|name>")?,
                enabled: true,
            }),
            "disable" => Ok(Self::Enable {
                channel: flag("--channel").ok_or("disable needs --channel <id|name>")?,
                enabled: false,
            }),
            other => Err(format!("unknown im command {other}")),
        }
    }
}

/// Runs one `im` command against the deployment's database.
pub async fn run(config: &DesktopRuntimeConfig, command: ImCommand) -> Result<(), String> {
    // A short-lived process still has to read the same secret store the daemon
    // writes. Headless deployments keep secrets in the host file; a desktop
    // home keeps them in the OS keychain.
    configure_secret_store(config);
    let service = ImChannelService::new(ImChannelServiceConfig {
        db_path: config.database_path.clone(),
        home_dir: config.home_dir.clone(),
        agent: Arc::new(AdministrativeAgent),
        telegram_api_base: None,
        lark_api_base: None,
        locale: None,
    })
    .map_err(|error| error.message)?;
    let mut conn = vibex_db::open_database(&config.database_path).map_err(|error| error.message)?;
    vibex_db::apply_migrations(&mut conn).map_err(|error| error.message)?;
    drop(conn);

    match command {
        ImCommand::List { json } => {
            let channels = service
                .list_channels()
                .await
                .map_err(|error| error.message)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&channels).map_err(|error| error.to_string())?
                );
                return Ok(());
            }
            if channels.is_empty() {
                println!("no IM channels configured");
                return Ok(());
            }
            for channel in channels {
                println!(
                    "{}  {}  kind={}  enabled={}  configured={}  state={:?}",
                    channel.channel_id.as_str(),
                    channel.display_name,
                    channel.kind.as_str(),
                    channel.enabled,
                    channel.configured,
                    channel.status.state
                );
            }
            Ok(())
        }
        ImCommand::Pair {
            channel,
            role,
            ttl_ms,
            scope,
            group_chat,
        } => {
            let channel_id = resolve_channel(&service, &channel).await?;
            let pairing = service
                .create_pairing_code(ImPairingRequest {
                    channel_id,
                    role,
                    ttl_ms,
                    scope,
                    target_chat_id: group_chat,
                })
                .await
                .map_err(|error| error.message)?;
            println!("pairing_code={}", pairing.code);
            println!("role={}", pairing.role.as_str());
            println!("expires_at_ms={}", pairing.expires_at_ms);
            println!("warning=one-time code; it is not stored in plaintext");
            Ok(())
        }
        ImCommand::Revoke { channel, principal } => {
            let channel_id = resolve_channel(&service, &channel).await?;
            let principals = service
                .list_principals(&channel_id)
                .await
                .map_err(|error| error.message)?;
            let target = principals
                .iter()
                .find(|candidate| candidate.principal_id.as_str() == principal)
                .map(|candidate| candidate.principal_id.clone())
                .or_else(|| {
                    // An external user id is what an operator usually has: it
                    // is the number or open id the chat showed them.
                    principals
                        .iter()
                        .find(|candidate| candidate.external_user_id == principal)
                        .map(|candidate| candidate.principal_id.clone())
                })
                .ok_or_else(|| format!("no principal {principal} on this channel"))?;
            service
                .revoke_principal(&target)
                .await
                .map_err(|error| error.message)?;
            println!("revoked={}", target.as_str());
            Ok(())
        }
        ImCommand::Bindings { channel } => {
            let channel_id = resolve_channel(&service, &channel).await?;
            let bindings = service
                .list_bindings(&channel_id)
                .await
                .map_err(|error| error.message)?;
            if bindings.is_empty() {
                println!("no session bindings");
                return Ok(());
            }
            for binding in bindings {
                println!(
                    "{}  session={}  chat={}  thread={:?}  status={:?}",
                    binding.binding_id.as_str(),
                    binding.session_id.as_str(),
                    binding.external_chat_id,
                    (!binding.thread_key.is_empty()).then_some(binding.thread_key.as_str()),
                    binding.status
                );
            }
            Ok(())
        }
        ImCommand::Unbind { binding } => {
            let binding_id = vibex_core::ImBindingId::parse(binding.clone())
                .map_err(|_| format!("{binding} is not an IM binding id"))?;
            service
                .revoke_binding(&binding_id)
                .await
                .map_err(|error| error.message)?;
            println!("unbound={}", binding_id.as_str());
            Ok(())
        }
        ImCommand::Test { channel } => {
            let channel_id = resolve_channel(&service, &channel).await?;
            let report = service
                .test_channel(&channel_id)
                .await
                .map_err(|error| error.message)?;
            println!("ok={}", report.ok);
            if let Some(name) = report.bot_name {
                println!("bot={name}");
            }
            if let Some(title) = report.chat_title {
                println!("chat={title}");
            }
            for warning in report.warnings {
                println!("warning={warning}");
            }
            Ok(())
        }
        ImCommand::Enable { channel, enabled } => {
            let channel_id = resolve_channel(&service, &channel).await?;
            // Enabling from the CLI opens the transport in this process, which
            // exits immediately. The row is what the daemon reads, so the flag
            // is written and the connection is left to the process that owns
            // the home.
            let summary = service
                .set_channel_enabled(&channel_id, enabled)
                .await
                .map_err(|error| error.message)?;
            println!(
                "{} enabled={} configured={}",
                summary.display_name, summary.enabled, summary.configured
            );
            Ok(())
        }
    }
}

/// Accepts either a channel id or its display name.
async fn resolve_channel(service: &ImChannelService, value: &str) -> Result<ImChannelId, String> {
    if let Ok(channel_id) = ImChannelId::parse(value.to_string()) {
        return Ok(channel_id);
    }
    let channels = service
        .list_channels()
        .await
        .map_err(|error| error.message)?;
    let matches: Vec<_> = channels
        .iter()
        .filter(|channel| channel.display_name == value)
        .collect();
    match matches.as_slice() {
        [channel] => Ok(channel.channel_id.clone()),
        [] => Err(format!("no IM channel named {value}")),
        _ => Err(format!(
            "{value} names more than one channel; use the channel id"
        )),
    }
}

/// Mirrors the runtime's own selection so a CLI write lands where the daemon
/// reads.
fn configure_secret_store(config: &DesktopRuntimeConfig) {
    use vibex_config_switch::secrets::ProviderSecretStoreConfig;

    let configured = std::env::var("VIBEX_PROVIDER_SECRET_STORE").ok();
    let store = match configured.as_deref() {
        Some("keychain") | Some("os_keychain") => ProviderSecretStoreConfig::os_keychain(),
        Some("file") | Some("host_file") => {
            ProviderSecretStoreConfig::host_file(config.home_dir.clone())
        }
        _ if config.mode == vibex_desktop_runtime::DesktopRuntimeMode::Headless => {
            ProviderSecretStoreConfig::host_file(config.home_dir.clone())
        }
        _ => ProviderSecretStoreConfig::os_keychain(),
    };
    vibex_config_switch::secrets::configure_provider_secret_store(store);
}

/// The `im` help block, printed by the top-level help.
pub fn print_help() {
    println!("  im list [--json]                     list configured channels");
    println!("  im pair --channel <id|name> [--role observer|approver|operator|admin]");
    println!("          [--ttl-minutes N] [--chat GROUP_ID]");
    println!("                                       mint a one-time pairing code; --chat");
    println!("                                       binds a code to one group");
    println!("  im revoke --channel <id|name> --principal <id|external-id>");
    println!("  im bindings --channel <id|name>      list session bindings");
    println!("  im unbind --binding <id>             drop one binding");
    println!("  im test --channel <id|name>          read-only provider probe");
    println!("  im enable|disable --channel <id|name>");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn list_defaults_to_human_output() {
        let command = ImCommand::parse(&args(&["list"])).expect("parse");
        assert!(matches!(command, ImCommand::List { json: false }));
        let command = ImCommand::parse(&args(&["list", "--json"])).expect("parse");
        assert!(matches!(command, ImCommand::List { json: true }));
    }

    #[test]
    fn pair_defaults_to_operator_and_reads_a_role() {
        let command =
            ImCommand::parse(&args(&["pair", "--channel", "imchannel_ab"])).expect("parse");
        match command {
            ImCommand::Pair {
                channel,
                role,
                ttl_ms,
                scope,
                group_chat,
            } => {
                assert_eq!(channel, "imchannel_ab");
                assert_eq!(role, ImPrincipalRole::Operator);
                assert!(ttl_ms.is_none());
                assert_eq!(scope, vibex_core::ImPairingScope::Direct);
                assert!(group_chat.is_none());
            }
            _ => panic!("expected pair"),
        }
        let command = ImCommand::parse(&args(&[
            "pair",
            "--channel",
            "bot",
            "--role",
            "admin",
            "--ttl-minutes",
            "2",
        ]))
        .expect("parse");
        match command {
            ImCommand::Pair { role, ttl_ms, .. } => {
                assert_eq!(role, ImPrincipalRole::Admin);
                assert_eq!(ttl_ms, Some(120_000));
            }
            _ => panic!("expected pair"),
        }
    }

    #[test]
    fn a_group_code_needs_a_chat_and_carries_it() {
        let command = ImCommand::parse(&args(&["pair", "--channel", "bot", "--chat", "-1001234"]))
            .expect("parse");
        match command {
            ImCommand::Pair {
                scope, group_chat, ..
            } => {
                assert_eq!(scope, vibex_core::ImPairingScope::Group);
                assert_eq!(group_chat.as_deref(), Some("-1001234"));
            }
            _ => panic!("expected pair"),
        }
    }

    #[test]
    fn a_missing_channel_is_reported_with_its_flag() {
        let error = ImCommand::parse(&args(&["test"])).expect_err("missing channel");
        assert!(error.contains("--channel"), "{error}");
        let error = ImCommand::parse(&args(&["pair", "--channel", "x", "--role", "root"]))
            .expect_err("bad role");
        assert!(error.contains("observer"), "{error}");
    }

    #[test]
    fn enable_and_disable_share_a_parse_path() {
        assert!(matches!(
            ImCommand::parse(&args(&["enable", "--channel", "x"])).expect("parse"),
            ImCommand::Enable { enabled: true, .. }
        ));
        assert!(matches!(
            ImCommand::parse(&args(&["disable", "--channel", "x"])).expect("parse"),
            ImCommand::Enable { enabled: false, .. }
        ));
        assert!(ImCommand::parse(&args(&["nonsense"])).is_err());
    }
}
