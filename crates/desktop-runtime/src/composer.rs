//! Composer command discovery shared by the local authority and the gateway.
//!
//! A composer trigger resolves against the Agent's own command catalogue, the
//! workspace file tree, and the Skills found in the workspace. The `@` trigger
//! additionally offers the collaborators the user may hand work to and the
//! sessions they may point at — the two references the product can act on
//! without the user retyping an identifier. All of them live with the
//! authority, so the same assembly serves the native backend and a paired
//! client instead of each side re-deriving it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use vibex_config_switch::skills::LocalSkillScanRequest;
use vibex_core::{
    AgentCommandDiscoverRequest, AgentCommandDiscoverResponse, AgentCommandDiscovery,
    AgentCommandEntry, AgentCommandExecutionBehavior, AgentCommandSelectionBehavior,
    AgentCommandSourceKind, AgentCommandTrigger, AgentId, FileTreeEntry, FileTreeRequest,
    ProviderBindingMetadata, ProviderKind, VibexResult, VibexUseRef,
};

use crate::runtime_option_ref;
use crate::vibex_use::VibexUseService;
use crate::{AgentHandle, FileHandle, ProviderHandle};

/// Metadata key carrying the stable collaborator reference of an `@Agent` entry.
pub const COMPOSER_AGENT_REFERENCE_KEY: &str = "vibexUseAgentReference";
/// Metadata key carrying the stable selection reference of an `@Agent` entry.
pub const COMPOSER_SELECTION_REFERENCE_KEY: &str = "vibexUseSelectionReference";
/// Metadata key carrying the stable session reference of an `@session` entry.
pub const COMPOSER_SESSION_REFERENCE_KEY: &str = "vibexUseSessionReference";
/// Metadata key marking a legacy direct-action entry.
///
/// Current authorities emit one `@Agent` reference per configuration and let
/// the client offer the reference and the hand-off as two halves of that row.
/// The key stays on the wire contract because an older paired authority may
/// still send the separate entry; clients filter it from the row list.
pub const COMPOSER_DELEGATE_NOW_KEY: &str = "vibexUseDelegateNow";

/// Resolves one composer discovery request against the authority's state.
pub async fn discover_composer_commands(
    agent: &AgentHandle,
    files: &FileHandle,
    providers: &ProviderHandle,
    vibex_use: Option<&Arc<VibexUseService>>,
    request: AgentCommandDiscoverRequest,
) -> VibexResult<AgentCommandDiscovery> {
    let manager = agent.manager();
    let mut response = manager.discover_commands(request.clone()).await?;
    let capabilities = manager.command_discovery_capabilities(&request)?;
    // The workspace files are the `@` popup's own list; the collaborators are
    // its second tab. They are bounded separately, so neither one's size can
    // decide what the other offers.
    let delegation_entries = match vibex_use {
        Some(vibex_use) => collaborator_reference_commands(vibex_use, &request).await?,
        None => Vec::new(),
    };
    append_file_reference_commands(files, &request, &mut response)?;
    append_workspace_skill_commands(providers, &request, &mut response, capabilities.skills)?;
    let quick_phrases = manager.discover_quick_phrases(&request)?;
    Ok(AgentCommandDiscovery {
        response,
        slash_commands: capabilities.slash_commands,
        skills: capabilities.skills,
        quick_phrases,
        delegation_entries,
    })
}

/// Whether this request resolves the references the `@` trigger offers.
///
/// A request without a trigger asks for the unfiltered list, which is the same
/// list `@` opens on, so it counts as one.
fn mention_references_are_offered(request: &AgentCommandDiscoverRequest) -> bool {
    request
        .trigger
        .is_none_or(|trigger| trigger == AgentCommandTrigger::Mention)
}

/// The entries of the `@` popup's delegation tab.
///
/// A reference here is a *convenience*, never a grant: the entry carries the
/// same `vibex://` reference the Agent tools use, and the tool call that
/// follows still authorizes it. Picking `@Codex` from this list is what tells
/// the main Agent which collaborator the user has in mind; it does not start
/// anything by itself.
async fn collaborator_reference_commands(
    vibex_use: &Arc<VibexUseService>,
    request: &AgentCommandDiscoverRequest,
) -> VibexResult<Vec<AgentCommandEntry>> {
    if !mention_references_are_offered(request) {
        return Ok(Vec::new());
    }
    let limit = request.limit.unwrap_or(50) as usize;
    let query = request
        .query
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_lowercase();
    let catalog = vibex_use.runtime_option_catalog().list().await?;
    let mut entries = runtime_reference_commands(&catalog.options, request);
    // An Agent's own configurations usually fill the tab, and the session
    // lookup is a database read of its own: only reach for it when the list
    // still has room.
    if entries.len() >= limit {
        return Ok(entries);
    }
    let mut existing_ids = entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<HashSet<_>>();

    for session in vibex_use.referenceable_sessions(&query, limit)? {
        if entries.len() >= limit {
            break;
        }
        let entry = AgentCommandEntry {
            id: format!("reference:session:{}", session.id.as_str()),
            trigger: AgentCommandTrigger::Mention,
            source_kind: AgentCommandSourceKind::Reference,
            label: format!("@{}", session.title),
            description: Some(session.agent_id.to_string()),
            insertion_text: format!("@{} ", session.title),
            command_name: None,
            provider_kind: Some(ProviderKind::Acp),
            prompt_id: None,
            skill_id: None,
            reference_path: None,
            selection_behavior: AgentCommandSelectionBehavior::Insert,
            execution_behavior: AgentCommandExecutionBehavior::None,
            destructive: false,
            metadata: vec![ProviderBindingMetadata {
                key: COMPOSER_SESSION_REFERENCE_KEY.to_string(),
                value: VibexUseRef::session(&session.id).as_uri(),
            }],
        };
        if !existing_ids.insert(entry.id.clone()) {
            continue;
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// The Agent references and direct delegation actions of one catalogue.
///
/// Each available account/model combination is independently selectable, so the
/// entry pair it produces is bounded by the request's own limit here rather
/// than by whatever the caller has already listed.
fn runtime_reference_commands(
    options: &[vibex_core::SessionRuntimeOption],
    request: &AgentCommandDiscoverRequest,
) -> Vec<AgentCommandEntry> {
    let limit = request.limit.unwrap_or(50) as usize;
    let mut remaining = limit;
    let query = request
        .query
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_lowercase();
    let mut entries: Vec<AgentCommandEntry> = Vec::with_capacity(limit);
    // Availability is checked before deduplication, so an unavailable default
    // cannot hide another usable configuration of the same Agent.
    let mut seen_selections = HashSet::new();
    // One Agent owns as many configurations as it has accounts, models and
    // modes, so dealt in catalogue order the first Agent fills the list with
    // itself. The entries are dealt one configuration per Agent per round
    // instead: every collaborator reaches the list before any Agent offers a
    // second configuration, and a query still narrows its way to whichever
    // configuration the reader named.
    let mut configurations_by_agent = Vec::<Vec<&vibex_core::SessionRuntimeOption>>::new();
    let mut agent_slots = HashMap::<AgentId, usize>::new();
    for option in options {
        let slot = *agent_slots
            .entry(option.selection.agent_id.clone())
            .or_insert_with(|| {
                configurations_by_agent.push(Vec::new());
                configurations_by_agent.len() - 1
            });
        configurations_by_agent[slot].push(option);
    }
    let rounds = configurations_by_agent
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or_default();
    'rounds: for round in 0..rounds {
        for agent_options in &configurations_by_agent {
            let Some(option) = agent_options.get(round) else {
                continue;
            };
            if remaining == 0 {
                break 'rounds;
            }
            let agent_id = option.selection.agent_id.clone();
            if option.availability != vibex_core::RuntimeOptionAvailability::Available {
                // An Agent that cannot start right now is not offered as if it
                // could; `vibex_discover` reports why.
                continue;
            }
            let selection_ref = runtime_option_ref(option);
            if !seen_selections.insert(selection_ref.clone()) {
                continue;
            }
            let entry = AgentCommandEntry {
                id: format!("reference:agent:{}:{selection_ref}", agent_id.as_str()),
                trigger: AgentCommandTrigger::Mention,
                source_kind: AgentCommandSourceKind::Reference,
                label: format!("@{}", option.agent_label),
                description: Some(runtime_reference_description(option)),
                insertion_text: format!("@{} ", option.agent_label),
                command_name: None,
                provider_kind: Some(ProviderKind::Acp),
                prompt_id: None,
                skill_id: None,
                reference_path: None,
                selection_behavior: AgentCommandSelectionBehavior::Insert,
                execution_behavior: AgentCommandExecutionBehavior::None,
                destructive: false,
                metadata: vec![
                    ProviderBindingMetadata {
                        key: COMPOSER_AGENT_REFERENCE_KEY.to_string(),
                        value: agent_id.as_str().to_string(),
                    },
                    ProviderBindingMetadata {
                        key: COMPOSER_SELECTION_REFERENCE_KEY.to_string(),
                        value: selection_ref.clone(),
                    },
                ],
            };
            if query.is_empty() || command_entry_matches_query(&entry, &query) {
                entries.push(entry);
                remaining -= 1;
            }
        }
    }
    entries
}

fn runtime_reference_description(option: &vibex_core::SessionRuntimeOption) -> String {
    let mut parts = vec![
        option.auth_source_label.as_str(),
        option.model_label.as_str(),
    ];
    for (selected, choices) in [
        (option.selection.mode_id.as_deref(), &option.modes),
        (
            option.selection.reasoning_effort.as_deref(),
            &option.reasoning_efforts,
        ),
    ] {
        if let Some(selected) = selected {
            parts.push(
                choices
                    .iter()
                    .find(|choice| choice.value == selected)
                    .and_then(|choice| choice.label.as_deref())
                    .unwrap_or(selected),
            );
        }
    }
    parts.join(" · ")
}

fn append_file_reference_commands(
    files: &FileHandle,
    request: &AgentCommandDiscoverRequest,
    response: &mut AgentCommandDiscoverResponse,
) -> VibexResult<()> {
    if !mention_references_are_offered(request) {
        return Ok(());
    }
    let Some(workspace_id) = request.workspace_id.clone() else {
        return Ok(());
    };

    let entries = files.list_tree(&FileTreeRequest {
        workspace_id: workspace_id.clone(),
        path: None,
        max_depth: Some(8),
        include_hidden: true,
    })?;
    let query = request.query.as_deref().map(str::trim).unwrap_or("");
    let query_lower = query.to_lowercase();
    let limit = request.limit.unwrap_or(50) as usize;
    let remaining = limit.saturating_sub(response.entries.len());
    if remaining == 0 {
        return Ok(());
    }

    response.entries.extend(
        entries
            .into_iter()
            .filter_map(|entry: FileTreeEntry| {
                let matches = query_lower.is_empty()
                    || entry.path.to_lowercase().contains(&query_lower)
                    || entry.name.to_lowercase().contains(&query_lower);
                matches.then(|| AgentCommandEntry {
                    id: format!("reference:file:{}", entry.path),
                    trigger: AgentCommandTrigger::Mention,
                    source_kind: AgentCommandSourceKind::Reference,
                    label: format!("@{}", entry.name),
                    description: None,
                    insertion_text: format!("@{} ", entry.path),
                    command_name: None,
                    provider_kind: Some(ProviderKind::Acp),
                    prompt_id: None,
                    skill_id: None,
                    reference_path: Some(entry.path),
                    selection_behavior: AgentCommandSelectionBehavior::Insert,
                    execution_behavior: AgentCommandExecutionBehavior::None,
                    destructive: false,
                    metadata: Vec::new(),
                })
            })
            .take(remaining),
    );

    Ok(())
}

fn append_workspace_skill_commands(
    providers: &ProviderHandle,
    request: &AgentCommandDiscoverRequest,
    response: &mut AgentCommandDiscoverResponse,
    provider_supports_skills: bool,
) -> VibexResult<()> {
    if request
        .trigger
        .is_some_and(|trigger| trigger != AgentCommandTrigger::Dollar)
        || !provider_supports_skills
    {
        return Ok(());
    }

    let limit = request.limit.unwrap_or(50) as usize;
    let mut remaining = limit.saturating_sub(response.entries.len());
    if remaining == 0 {
        return Ok(());
    }

    let query = request
        .query
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_lowercase();
    let mut existing_labels = response
        .entries
        .iter()
        .map(|entry| entry.label.to_lowercase())
        .collect::<HashSet<_>>();
    let local_entries = providers
        .service()
        .scan_local_skills(LocalSkillScanRequest {
            source_agent_id: request.agent_id.clone(),
            workspace_id: request.workspace_id.clone(),
        })?;

    for entry in local_entries {
        if remaining == 0 {
            break;
        }
        let command_entry = AgentCommandEntry {
            id: format!("skill:local:{}:{}", entry.root_source, entry.source_hash),
            trigger: AgentCommandTrigger::Dollar,
            source_kind: AgentCommandSourceKind::Skill,
            label: format!("${}", entry.command_name),
            description: entry.description,
            insertion_text: format!("${} ", entry.command_name),
            command_name: Some(entry.command_name),
            provider_kind: Some(ProviderKind::Acp),
            prompt_id: None,
            skill_id: None,
            reference_path: Some(entry.manifest_path.display().to_string()),
            selection_behavior: AgentCommandSelectionBehavior::Insert,
            execution_behavior: AgentCommandExecutionBehavior::None,
            destructive: false,
            metadata: vec![
                ProviderBindingMetadata {
                    key: "skillSource".to_string(),
                    value: entry.root_source,
                },
                ProviderBindingMetadata {
                    key: "manifestPathHash".to_string(),
                    value: entry.source_hash,
                },
                ProviderBindingMetadata {
                    key: "sourceAgentId".to_string(),
                    value: entry.source_agent_id.into_string(),
                },
            ],
        };
        if !query.is_empty() && !command_entry_matches_query(&command_entry, &query) {
            continue;
        }
        if !existing_labels.insert(command_entry.label.to_lowercase()) {
            continue;
        }
        response.entries.push(command_entry);
        remaining -= 1;
    }

    Ok(())
}

fn command_entry_matches_query(entry: &AgentCommandEntry, query: &str) -> bool {
    entry.label.to_lowercase().contains(query)
        || entry.insertion_text.to_lowercase().contains(query)
        || entry
            .description
            .as_deref()
            .is_some_and(|description| description.to_lowercase().contains(query))
}

/// Serves composer command discovery to the gateway.
///
/// The runtime installs this source for the same reason as the other source
/// traits: `vibex-remote` cannot reach the agent, file and provider handles.
pub struct ComposerCommandSource {
    agent: AgentHandle,
    files: FileHandle,
    providers: ProviderHandle,
    vibex_use: Option<Arc<VibexUseService>>,
}

impl ComposerCommandSource {
    pub fn new(
        agent: AgentHandle,
        files: FileHandle,
        providers: ProviderHandle,
        vibex_use: Option<Arc<VibexUseService>>,
    ) -> Self {
        Self {
            agent,
            files,
            providers,
            vibex_use,
        }
    }
}

#[async_trait::async_trait]
impl vibex_remote::RemoteAgentCommandDiscoverySource for ComposerCommandSource {
    async fn discover_commands(
        &self,
        request: AgentCommandDiscoverRequest,
    ) -> VibexResult<AgentCommandDiscovery> {
        discover_composer_commands(
            &self.agent,
            &self.files,
            &self.providers,
            self.vibex_use.as_ref(),
            request,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentAuthContextId, AgentId, ProviderProfileId, RuntimeOptionAvailability,
        SessionConfigValue, SessionRuntimeOption, SessionRuntimeSelection,
    };

    fn option(profile: &str, model: &str) -> SessionRuntimeOption {
        agent_option("codex", "Codex", profile, model)
    }

    fn agent_option(agent: &str, label: &str, profile: &str, model: &str) -> SessionRuntimeOption {
        SessionRuntimeOption {
            selection: SessionRuntimeSelection::provider(
                AgentId::parse(agent).unwrap(),
                ProviderProfileId::parse(profile).unwrap(),
                model,
            ),
            agent_label: label.into(),
            auth_source_label: profile.into(),
            model_label: model.into(),
            reasoning_efforts: Vec::new(),
            modes: Vec::new(),
            features: Vec::new(),
            availability: RuntimeOptionAvailability::Available,
            member_count: 1,
            auto_routes: false,
        }
    }

    fn entries(
        options: &[SessionRuntimeOption],
        query: &str,
        limit: u32,
    ) -> Vec<AgentCommandEntry> {
        let request = AgentCommandDiscoverRequest {
            agent_id: None,
            provider_profile_id: None,
            session_id: None,
            workspace_id: None,
            trigger: Some(AgentCommandTrigger::Mention),
            query: Some(query.into()),
            limit: Some(limit),
        };
        runtime_reference_commands(options, &request)
    }

    #[test]
    fn unavailable_option_does_not_hide_an_available_selection() {
        let available = option("provider_main", "model-a");
        let mut unavailable = available.clone();
        unavailable.availability = RuntimeOptionAvailability::TemporarilyUnavailable;
        let commands = entries(&[unavailable, available.clone(), available], "", 50);
        assert_eq!(commands.len(), 1);
        assert!(commands[0].id.starts_with("reference:agent:"));
    }

    #[test]
    fn account_model_and_configuration_choices_remain_distinct() {
        let provider = option("provider_main", "model-a");
        let mut account = provider.clone();
        account.selection = SessionRuntimeSelection::agent_default(
            AgentId::parse("codex").unwrap(),
            AgentAuthContextId::parse("agent_auth_context_main").unwrap(),
        );
        account.auth_source_label = "Main account".into();
        account.model_label = "Agent default".into();
        let mut configured = provider.clone();
        configured
            .selection
            .config_values
            .insert("review_mode".into(), "strict".into());
        let commands = entries(
            &[
                provider,
                account,
                configured,
                option("provider_main", "model-b"),
            ],
            "",
            50,
        );
        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands
                .iter()
                .map(|entry| &entry.id)
                .collect::<HashSet<_>>()
                .len(),
            4
        );
        assert!(
            commands
                .iter()
                .any(|entry| entry.description.as_deref() == Some("Main account · Agent default"))
        );
    }

    #[test]
    fn selected_mode_and_effort_are_searchable_without_expanding_the_catalog() {
        let mut configured = option("provider_main", "model-a");
        configured.selection.mode_id = Some("plan".into());
        configured.selection.reasoning_effort = Some("high".into());
        configured.modes = vec![SessionConfigValue {
            value: "plan".into(),
            label: Some("Plan".into()),
        }];
        configured.reasoning_efforts = vec![SessionConfigValue {
            value: "high".into(),
            label: Some("High reasoning".into()),
        }];
        let commands = entries(std::slice::from_ref(&configured), "high reasoning", 50);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].description.as_deref(),
            Some("provider_main · model-a · Plan · High reasoning")
        );
        assert_eq!(entries(&[configured], "", 1).len(), 1);
    }

    #[test]
    fn each_collaborator_is_offered_before_any_agent_offers_a_second_configuration() {
        let commands = entries(
            &[
                agent_option("codex", "Codex", "provider_main", "model-a"),
                agent_option("codex", "Codex", "provider_main", "model-b"),
                agent_option("claude", "Claude", "provider_main", "model-a"),
                agent_option("claude", "Claude", "provider_main", "model-b"),
                agent_option("opencode", "OpenCode", "provider_main", "model-a"),
                agent_option("opencode", "OpenCode", "provider_main", "model-b"),
            ],
            "",
            3,
        );
        // The first round covers every Agent; the second is what the bound cuts.
        assert_eq!(commands.len(), 3);
        let labels: Vec<_> = commands.iter().map(|entry| entry.label.clone()).collect();
        assert_eq!(labels, vec!["@Codex", "@Claude", "@OpenCode"]);
    }
}
