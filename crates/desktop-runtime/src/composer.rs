//! Composer command discovery shared by the local authority and the gateway.
//!
//! A composer trigger resolves against three sources: the Agent's own command
//! catalogue, the workspace file tree for `@` references, and the Skills found
//! in the workspace for `$` commands. All three live with the authority, so
//! the same assembly serves the native backend and a paired client instead of
//! each side re-deriving it.

use std::collections::HashSet;

use vibex_config_switch::skills::LocalSkillScanRequest;
use vibex_core::{
    AgentCommandDiscoverRequest, AgentCommandDiscoverResponse, AgentCommandDiscovery,
    AgentCommandEntry, AgentCommandExecutionBehavior, AgentCommandSelectionBehavior,
    AgentCommandSourceKind, AgentCommandTrigger, FileTreeEntry, FileTreeRequest,
    ProviderBindingMetadata, ProviderKind, VibexResult,
};

use crate::{AgentHandle, FileHandle, ProviderHandle};

/// Resolves one composer discovery request against the authority's state.
pub async fn discover_composer_commands(
    agent: &AgentHandle,
    files: &FileHandle,
    providers: &ProviderHandle,
    request: AgentCommandDiscoverRequest,
) -> VibexResult<AgentCommandDiscovery> {
    let manager = agent.manager();
    let mut response = manager.discover_commands(request.clone()).await?;
    let capabilities = manager.command_discovery_capabilities(&request)?;
    append_file_reference_commands(files, &request, &mut response)?;
    append_workspace_skill_commands(providers, &request, &mut response, capabilities.skills)?;
    Ok(AgentCommandDiscovery {
        response,
        slash_commands: capabilities.slash_commands,
        skills: capabilities.skills,
    })
}

fn append_file_reference_commands(
    files: &FileHandle,
    request: &AgentCommandDiscoverRequest,
    response: &mut AgentCommandDiscoverResponse,
) -> VibexResult<()> {
    if request
        .trigger
        .is_some_and(|trigger| trigger != AgentCommandTrigger::Mention)
    {
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
}

impl ComposerCommandSource {
    pub fn new(agent: AgentHandle, files: FileHandle, providers: ProviderHandle) -> Self {
        Self {
            agent,
            files,
            providers,
        }
    }
}

#[async_trait::async_trait]
impl vibex_remote::RemoteAgentCommandDiscoverySource for ComposerCommandSource {
    async fn discover_commands(
        &self,
        request: AgentCommandDiscoverRequest,
    ) -> VibexResult<AgentCommandDiscovery> {
        discover_composer_commands(&self.agent, &self.files, &self.providers, request).await
    }
}
