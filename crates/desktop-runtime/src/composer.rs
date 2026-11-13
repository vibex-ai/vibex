//! Composer command discovery shared by the local authority and the gateway.
//!
//! A composer trigger resolves against the Agent's own command catalogue, the
//! workspace file tree, and the Skills found in the workspace. The `@` trigger
//! additionally offers the collaborators the user may hand work to and the
//! sessions they may point at — the two references the product can act on
//! without the user retyping an identifier. All of them live with the
//! authority, so the same assembly serves the native backend and a paired
//! client instead of each side re-deriving it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use vibex_config_switch::skills::LocalSkillScanRequest;
use vibex_core::{
    AgentCommandDiscoverRequest, AgentCommandDiscoverResponse, AgentCommandDiscovery,
    AgentCommandEntry, AgentCommandExecutionBehavior, AgentCommandSelectionBehavior,
    AgentCommandSourceKind, AgentCommandTrigger, AgentId, AgentModelSourceRef, FileTreeEntry,
    FileTreeRequest, ProviderBindingMetadata, ProviderDefaultScopeKind, ProviderKind,
    ProviderProfileDefaultScope, ProviderProfileId, RuntimeOptionAvailability,
    SessionRuntimeOption, SessionRuntimeSelection, VibexResult, VibexUseRef, WorkspaceId,
};
use vibex_db::{AgentModelSourceDefaultRepository, ProviderProfileRepository, open_database};
use vibex_desktop_model::runtime_selection_use_count;

use crate::catalog::RuntimeComposerPreferences;
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
    // The workspace files are the `@` popup's own list; the collaborators and
    // the conversations are its second and third tabs. Each is bounded on its
    // own, so neither one's size can decide what the others offer.
    let (delegation_entries, session_entries) = match vibex_use {
        Some(vibex_use) => (
            collaborator_reference_commands(vibex_use, files, &request).await?,
            session_reference_commands(vibex_use, &request)?,
        ),
        None => (Vec::new(), Vec::new()),
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
        session_entries,
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
    files: &FileHandle,
    request: &AgentCommandDiscoverRequest,
) -> VibexResult<Vec<AgentCommandEntry>> {
    if !mention_references_are_offered(request) {
        return Ok(Vec::new());
    }
    let catalog_service = vibex_use.runtime_option_catalog();
    let catalog = catalog_service.list().await?;
    let preferences = catalog_service.composer_preferences();
    let selection = DelegationSelection::load(files, &catalog, request, preferences);
    Ok(runtime_reference_commands(
        &catalog.options,
        request,
        &selection,
    ))
}

/// The entries of the `@` popup's conversation tab.
///
/// A conversation reference is context, not a target: it points a task at a
/// session it may read, and picking one inserts the reference instead of
/// starting a delegation. It is resolved against the same bounded session
/// lookup the delegation tab used to share, now with a budget of its own so a
/// long collaborator list cannot hide the conversations.
fn session_reference_commands(
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
    Ok(vibex_use
        .referenceable_sessions(&query, limit)?
        .iter()
        .map(|session| session_reference_entry(&session.id, &session.title, &session.agent_id))
        .collect())
}

fn session_reference_entry(
    session_id: &vibex_core::VibexSessionId,
    title: &str,
    agent_id: &AgentId,
) -> AgentCommandEntry {
    AgentCommandEntry {
        id: format!("reference:session:{}", session_id.as_str()),
        trigger: AgentCommandTrigger::Mention,
        source_kind: AgentCommandSourceKind::Reference,
        label: format!("@{title}"),
        description: Some(agent_id.to_string()),
        insertion_text: format!("@{title} "),
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
            value: VibexUseRef::session(session_id).as_uri(),
        }],
    }
}

/// The evidence behind the one configuration shown for each Agent.
///
/// The delegation tab answers "who can take this work", not "which of this
/// Agent's configurations can take it": one Agent once. Which configuration
/// that row carries is decided here, from the Agent's default source, the
/// models the reader has run in it, and the explicit choice the row's edit
/// button writes down.
#[derive(Debug, Default)]
struct DelegationSelection {
    default_sources: BTreeMap<AgentId, AgentModelSourceRef>,
    default_models: BTreeMap<ProviderProfileId, String>,
    preferences: RuntimeComposerPreferences,
}

impl DelegationSelection {
    fn load(
        files: &FileHandle,
        catalog: &vibex_core::SessionRuntimeOptionCatalog,
        request: &AgentCommandDiscoverRequest,
        preferences: RuntimeComposerPreferences,
    ) -> Self {
        let scope = delegation_default_scope(request.workspace_id.as_ref());
        let mut default_sources = BTreeMap::new();
        let mut default_models = BTreeMap::new();
        // A default source only decides between multiple configurations. An
        // Agent with a single option has nothing to resolve, so the per-Agent
        // fallback queries are skipped for it. Two models of one Provider still
        // need the profile's own default model when no usage has accumulated.
        let mut option_count_by_agent = BTreeMap::<AgentId, usize>::new();
        for option in &catalog.options {
            *option_count_by_agent
                .entry(option.selection.agent_id.clone())
                .or_default() += 1;
        }
        let agents = option_count_by_agent
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(agent_id, _)| agent_id)
            .collect::<Vec<_>>();
        let Ok(connection) = open_database(files.database_path()) else {
            return Self {
                default_sources,
                default_models,
                preferences,
            };
        };
        for agent_id in agents {
            if let Ok(default) = AgentModelSourceDefaultRepository::resolve(
                &connection,
                scope.clone(),
                agent_id.clone(),
            ) && let Some(source) = default.source
            {
                default_sources.insert(agent_id, source);
            }
        }
        if let Ok(profiles) = ProviderProfileRepository::list_all(&connection) {
            for profile in profiles {
                if let Some(model) = profile
                    .default_model
                    .filter(|model| !model.trim().is_empty())
                {
                    default_models.insert(profile.id, model);
                }
            }
        }
        Self {
            default_sources,
            default_models,
            preferences,
        }
    }
}

fn delegation_default_scope(workspace_id: Option<&WorkspaceId>) -> ProviderProfileDefaultScope {
    match workspace_id {
        Some(workspace_id) => ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Workspace,
            project_id: None,
            workspace_id: Some(workspace_id.clone()),
        },
        None => ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Global,
            project_id: None,
            workspace_id: None,
        },
    }
}

/// The Agent references of one catalogue, one per Agent.
///
/// Each row carries the configuration that Agent should run as: its explicit
/// delegation choice if the reader made one, otherwise the commonly used model
/// of its default source, otherwise the provider/model pair the reader used
/// last. The old list dealt every configuration, which showed the same Agent
/// several times and made the reader choose a configuration when all they
/// wanted was a collaborator.
fn runtime_reference_commands(
    options: &[SessionRuntimeOption],
    request: &AgentCommandDiscoverRequest,
    selection: &DelegationSelection,
) -> Vec<AgentCommandEntry> {
    let limit = request.limit.unwrap_or(50) as usize;
    let query = request
        .query
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_lowercase();
    let mut agent_order = Vec::new();
    let mut available_by_agent = HashMap::<AgentId, Vec<&SessionRuntimeOption>>::new();
    for option in options {
        if option.availability != RuntimeOptionAvailability::Available {
            // An Agent that cannot start right now is not offered as if it
            // could; `vibex_discover` reports why.
            continue;
        }
        let agent_id = option.selection.agent_id.clone();
        let slot = available_by_agent
            .entry(agent_id.clone())
            .or_insert_with(|| {
                agent_order.push(agent_id);
                Vec::new()
            });
        slot.push(option);
    }

    let mut entries = Vec::with_capacity(limit.min(agent_order.len()));
    for agent_id in agent_order {
        if entries.len() >= limit {
            break;
        }
        let Some(available) = available_by_agent.get(&agent_id) else {
            continue;
        };
        // The query narrows against the Agent's whole vocabulary, not only the
        // configuration the row happens to carry: a reader who typed a model
        // name still finds the collaborator that can run it.
        let matches = query.is_empty()
            || available.iter().any(|option| {
                option.agent_label.to_lowercase().contains(&query)
                    || runtime_reference_description(option)
                        .to_lowercase()
                        .contains(&query)
            });
        if !matches {
            continue;
        }
        let Some(option) = preferred_delegation_option(available, &agent_id, selection) else {
            continue;
        };
        let selection_ref = runtime_option_ref(option);
        entries.push(AgentCommandEntry {
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
                    value: selection_ref,
                },
            ],
        });
    }
    entries
}

/// Which of one Agent's available configurations the delegation row carries.
fn preferred_delegation_option<'a>(
    available: &[&'a SessionRuntimeOption],
    agent_id: &AgentId,
    selection: &DelegationSelection,
) -> Option<&'a SessionRuntimeOption> {
    let preferences = &selection.preferences;
    if available.is_empty() {
        return None;
    }
    if let Some(source) = selection.default_sources.get(agent_id) {
        let scoped = available
            .iter()
            .copied()
            .filter(|option| runtime_auth_source_matches(&option.selection.auth_source, source))
            .collect::<Vec<_>>();
        if !scoped.is_empty() {
            // "Commonly used" is the reader's own count, not a provider's
            // ordering: the model they pick most often inside the default
            // source is the one the row should carry.
            if let Some(option) = most_used_delegation_option(&scoped, preferences) {
                return Some(option);
            }
            // No usage yet: the last pair used inside that source, then the
            // source's own default model, then catalogue order.
            if let Some(preferred) = preferences.by_agent.get(agent_id)
                && let Some(option) = scoped
                    .iter()
                    .copied()
                    .find(|option| delegation_selection_matches(&option.selection, preferred))
            {
                return Some(option);
            }
            if let Some(profile_id) = source.provider_profile_id()
                && let Some(model) = selection.default_models.get(profile_id)
                && let Some(option) = scoped
                    .iter()
                    .copied()
                    .find(|option| option.selection.model_id() == Some(model.as_str()))
            {
                return Some(option);
            }
            return scoped.first().copied();
        }
    }

    // No default source: the Agent's own last used provider/model pair, then
    // the tail of the model recency list, then the last applied selection.
    if let Some(preferred) = preferences.by_agent.get(agent_id)
        && let Some(option) = available
            .iter()
            .copied()
            .find(|option| delegation_selection_matches(&option.selection, preferred))
    {
        return Some(option);
    }
    for preferred in preferences.by_model.iter().rev() {
        if &preferred.agent_id != agent_id {
            continue;
        }
        if let Some(option) = available
            .iter()
            .copied()
            .find(|option| delegation_selection_matches(&option.selection, preferred))
        {
            return Some(option);
        }
    }
    if let Some(preferred) = preferences.default_selection.as_ref()
        && &preferred.agent_id == agent_id
        && let Some(option) = available
            .iter()
            .copied()
            .find(|option| delegation_selection_matches(&option.selection, preferred))
    {
        return Some(option);
    }
    available.first().copied()
}

/// The option with the highest use count, newest use breaking a tie.
///
/// `None` means none of the scoped models has been chosen yet; the caller has
/// a provider default to fall back to.
fn most_used_delegation_option<'a>(
    options: &[&'a SessionRuntimeOption],
    preferences: &RuntimeComposerPreferences,
) -> Option<&'a SessionRuntimeOption> {
    let recency = |selection: &SessionRuntimeSelection| {
        preferences
            .by_model
            .iter()
            .rposition(|remembered| delegation_selection_matches(remembered, selection))
            .map(|index| index + 1)
            .unwrap_or(0)
    };
    options
        .iter()
        .copied()
        .map(|option| {
            (
                runtime_selection_use_count(&preferences.usage, &option.selection),
                recency(&option.selection),
                option,
            )
        })
        .filter(|(count, _, _)| *count > 0)
        .max_by_key(|(count, recency, _)| (*count, *recency))
        .map(|(_, _, option)| option)
}

/// Whether two selections name the same catalogue entry.
///
/// Identity is Agent, authentication source and model: the mode and thinking
/// depth are how that entry runs, and a reader who changed one did not move to
/// a different provider/model pair.
fn delegation_selection_matches(
    left: &SessionRuntimeSelection,
    right: &SessionRuntimeSelection,
) -> bool {
    left.agent_id == right.agent_id
        && left.auth_source == right.auth_source
        && left.model == right.model
}

fn runtime_auth_source_matches(
    auth_source: &vibex_core::RuntimeAuthSource,
    source: &AgentModelSourceRef,
) -> bool {
    match source {
        AgentModelSourceRef::Provider(provider_profile_id) => {
            auth_source.provider_profile_id() == Some(provider_profile_id)
        }
        AgentModelSourceRef::RouteGroup(model_route_id) => {
            auth_source.model_route_id() == Some(model_route_id)
        }
    }
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

    let query = request.query.as_deref().map(str::trim).unwrap_or("");
    let directory_query = composer_directory_query(query);
    // A query that has reached a directory browses that directory itself
    // rather than searching for it in the workspace-wide listing: that
    // listing is bounded, and a large sibling subtree can exhaust the bound
    // before the directory the reader named is ever reached.
    let scoped_entries = directory_query.and_then(|(directory, _)| {
        files
            .list_tree(&FileTreeRequest {
                workspace_id: workspace_id.clone(),
                path: Some(directory.to_string()),
                max_depth: Some(1),
                include_hidden: true,
            })
            .ok()
    });
    let scoped = scoped_entries.is_some();
    let entries = match scoped_entries {
        Some(entries) => entries,
        // The named directory does not exist yet — the reader may still be
        // typing its name. Fall back to the workspace listing so a partial
        // path keeps narrowing the tree as it did before.
        None => files.list_tree(&FileTreeRequest {
            workspace_id,
            path: None,
            max_depth: Some(8),
            include_hidden: true,
        })?,
    };
    // Inside a directory only the final segment is matched: the directory is
    // already the scope, and matching the whole path would let the parent's
    // own name answer for every child.
    let needle = directory_query
        .map(|(_, fragment)| fragment)
        .unwrap_or(query)
        .to_lowercase();
    let limit = request.limit.unwrap_or(50) as usize;
    let remaining = limit.saturating_sub(response.entries.len());
    if remaining == 0 {
        return Ok(());
    }

    response.entries.extend(
        entries
            .into_iter()
            .filter_map(|entry: FileTreeEntry| {
                composer_file_matches_query(&entry, &needle, scoped).then(|| AgentCommandEntry {
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

/// The directory a workspace-file query has descended into, and the fragment
/// still being typed inside it.
///
/// An `@` reference is a workspace-relative path, so a `/` in the query is what
/// tells the popup the reader is naming a directory rather than a top-level
/// entry. The split is at the last separator: everything before it scopes the
/// listing, everything after it filters the children.
fn composer_directory_query(query: &str) -> Option<(&str, &str)> {
    let separator = query.rfind('/')?;
    Some((&query[..separator], &query[separator + 1..]))
}

/// Whether one tree entry answers a workspace-file query.
///
/// `needle` is already lowercased. A scoped listing only has to match the
/// fragment after the last separator against the entry's own name: the
/// directory is already the scope, and the parent's name must not answer for
/// every child. The workspace listing matches the whole path, so naming a
/// directory anywhere in it still finds its files.
fn composer_file_matches_query(
    entry: &FileTreeEntry,
    needle: &str,
    inside_directory: bool,
) -> bool {
    if needle.is_empty() {
        return true;
    }
    if inside_directory {
        return entry.name.to_lowercase().contains(needle);
    }
    entry.path.to_lowercase().contains(needle) || entry.name.to_lowercase().contains(needle)
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
        AgentAuthContextId, AgentId, FileEntryKind, ProviderProfileId, RuntimeOptionAvailability,
        SessionConfigValue, SessionRuntimeOption, SessionRuntimeSelection, VibexSessionId,
        WorkspaceId,
    };
    use vibex_db::{WorkspaceRepository, apply_migrations, open_database};

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

    fn request(query: &str, limit: u32) -> AgentCommandDiscoverRequest {
        AgentCommandDiscoverRequest {
            agent_id: None,
            provider_profile_id: None,
            session_id: None,
            workspace_id: None,
            trigger: Some(AgentCommandTrigger::Mention),
            query: Some(query.into()),
            limit: Some(limit),
        }
    }

    fn entries(
        options: &[SessionRuntimeOption],
        query: &str,
        limit: u32,
    ) -> Vec<AgentCommandEntry> {
        runtime_reference_commands(
            options,
            &request(query, limit),
            &DelegationSelection::default(),
        )
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
    fn one_agent_is_offered_once_across_its_configurations() {
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
        // Four configurations of one Agent are one collaborator, not four
        // rows: the list answers "who can take this work".
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].label, "@Codex");
    }

    #[test]
    fn a_conversation_is_an_insert_only_reference_without_an_agent_target() {
        let agent_id = AgentId::parse("codex").unwrap();
        let session_id = VibexSessionId::new();
        let entry = session_reference_entry(&session_id, "Fix the flaky test", &agent_id);

        // The conversation tab points at a session to read. It must carry no
        // Agent or selection reference: those two are what turn a row into a
        // delegation target in the popup.
        assert_eq!(
            entry.id,
            format!("reference:session:{}", session_id.as_str())
        );
        assert_eq!(entry.label, "@Fix the flaky test");
        assert_eq!(entry.insertion_text, "@Fix the flaky test ");
        assert_eq!(
            entry.selection_behavior,
            AgentCommandSelectionBehavior::Insert
        );
        assert_eq!(
            entry.execution_behavior,
            AgentCommandExecutionBehavior::None
        );
        assert!(entry.metadata.iter().all(|metadata| {
            metadata.key != COMPOSER_AGENT_REFERENCE_KEY
                && metadata.key != COMPOSER_SELECTION_REFERENCE_KEY
        }));
        let reference = entry
            .metadata
            .iter()
            .find(|metadata| metadata.key == COMPOSER_SESSION_REFERENCE_KEY)
            .map(|metadata| metadata.value.as_str());
        assert_eq!(
            reference,
            Some(format!("vibex://session/{}", session_id.as_str()).as_str())
        );
    }

    #[test]
    fn a_default_source_contributes_its_most_used_model() {
        let agent_id = AgentId::parse("codex").unwrap();
        let other = option("provider_other", "model-other");
        let unused = option("provider_main", "model-unused");
        let mut used = option("provider_main", "model-used");
        used.selection.reasoning_effort = Some("high".into());
        used.reasoning_efforts = vec![SessionConfigValue {
            value: "high".into(),
            label: Some("High reasoning".into()),
        }];
        let mut selection = DelegationSelection::default();
        selection.default_sources.insert(
            agent_id.clone(),
            AgentModelSourceRef::provider(ProviderProfileId::parse("provider_main").unwrap()),
        );
        selection.preferences.usage.insert(
            vibex_desktop_model::runtime_selection_usage_key(&used.selection),
            4,
        );
        selection.preferences.usage.insert(
            vibex_desktop_model::runtime_selection_usage_key(&unused.selection),
            1,
        );

        let commands = runtime_reference_commands(
            &[other, unused, used.clone()],
            &request("", 50),
            &selection,
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].description.as_deref(),
            Some("provider_main · model-used · High reasoning")
        );
    }

    #[test]
    fn without_usage_the_default_source_uses_its_profile_default_model() {
        let agent_id = AgentId::parse("codex").unwrap();
        let profile_id = ProviderProfileId::parse("provider_main").unwrap();
        let first = option("provider_main", "model-a");
        let default = option("provider_main", "model-b");
        let mut selection = DelegationSelection::default();
        selection
            .default_sources
            .insert(agent_id, AgentModelSourceRef::provider(profile_id.clone()));
        selection
            .default_models
            .insert(profile_id, "model-b".to_string());

        let commands = runtime_reference_commands(&[first, default], &request("", 50), &selection);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].description.as_deref(),
            Some("provider_main · model-b")
        );
    }

    #[test]
    fn without_a_default_source_the_last_used_pair_is_offered() {
        let agent_id = AgentId::parse("codex").unwrap();
        let first = option("provider_a", "model-a");
        let last = option("provider_b", "model-b");
        let mut selection = DelegationSelection::default();
        selection
            .preferences
            .by_agent
            .insert(agent_id, last.selection.clone());

        let commands = runtime_reference_commands(&[first, last], &request("", 50), &selection);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].description.as_deref(),
            Some("provider_b · model-b")
        );
    }

    #[test]
    fn a_query_matches_any_configuration_of_an_agent_but_offers_one_row() {
        let selected = option("provider_main", "model-a");
        let queried = option("provider_main", "model-z");
        let commands = runtime_reference_commands(
            &[selected, queried],
            &request("model-z", 50),
            &DelegationSelection::default(),
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].description.as_deref(),
            Some("provider_main · model-a")
        );
        assert!(entries(&[option("provider_main", "model-a")], "absent", 50).is_empty());
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
    fn each_collaborator_occupies_one_row_regardless_of_its_configurations() {
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
        // Two configurations each, still one row per collaborator, in the
        // catalogue's own Agent order.
        assert_eq!(commands.len(), 3);
        let labels: Vec<_> = commands.iter().map(|entry| entry.label.clone()).collect();
        assert_eq!(labels, vec!["@Codex", "@Claude", "@OpenCode"]);
    }

    #[test]
    fn a_trailing_path_segment_scopes_the_file_list_to_that_directory() {
        assert_eq!(
            composer_directory_query("flex-work-ai/docs/"),
            Some(("flex-work-ai/docs", ""))
        );
        assert_eq!(
            composer_directory_query("flex-work-ai/docs/rea"),
            Some(("flex-work-ai/docs", "rea"))
        );
        assert_eq!(
            composer_directory_query("flex-work-ai/"),
            Some(("flex-work-ai", ""))
        );
        // A query still naming a top-level entry searches the whole tree.
        assert_eq!(composer_directory_query("flex-work-ai"), None);
        assert_eq!(composer_directory_query(""), None);
        // A leading separator names the workspace root rather than escaping it.
        assert_eq!(composer_directory_query("/"), Some(("", "")));
    }

    #[test]
    fn a_scoped_file_list_matches_the_child_name_not_the_parent_path() {
        let entry = FileTreeEntry {
            workspace_id: WorkspaceId::new(),
            path: "flex-work-ai/docs/file.md".into(),
            name: "file.md".into(),
            parent_path: Some("flex-work-ai/docs".into()),
            kind: FileEntryKind::File,
            size_bytes: None,
            modified_at_ms: None,
            hidden: false,
            ignored: false,
        };
        assert!(composer_file_matches_query(&entry, "", true));
        assert!(composer_file_matches_query(&entry, "file", true));
        // The directory's own name is the scope, not a needle: `docs/` must
        // list every child instead of letting "docs" match all of them.
        assert!(!composer_file_matches_query(&entry, "docs", true));
        // The workspace listing still matches the path a reader typed.
        assert!(composer_file_matches_query(
            &entry,
            "flex-work-ai/docs/",
            false
        ));
    }

    #[test]
    fn a_typed_directory_lists_its_children_even_when_the_workspace_scan_is_full() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("vibex.db");
        let workspace_root = directory.path().join("workspace");
        // A large first directory that consumes the bounded root scan before
        // it can reach the directory the reader names.
        let large = workspace_root.join("aaa-large");
        std::fs::create_dir_all(&large).unwrap();
        let docs = workspace_root.join("flex-work-ai").join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("readme.md"), "# docs\n").unwrap();
        for index in 0..2100 {
            std::fs::write(large.join(format!("entry-{index:04}.txt")), "nested\n").unwrap();
        }
        let mut connection = open_database(&database_path).unwrap();
        apply_migrations(&mut connection).unwrap();
        let (_, workspace) = WorkspaceRepository::ensure(
            &connection,
            &workspace_root,
            vibex_core::WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let files = FileHandle {
            db_path: database_path.clone(),
        };
        let request = AgentCommandDiscoverRequest {
            agent_id: None,
            provider_profile_id: None,
            session_id: None,
            workspace_id: Some(workspace.id),
            trigger: Some(AgentCommandTrigger::Mention),
            query: Some("flex-work-ai/docs/".into()),
            limit: Some(50),
        };
        let mut response = AgentCommandDiscoverResponse {
            entries: Vec::new(),
            diagnostics: Vec::new(),
        };

        append_file_reference_commands(&files, &request, &mut response).unwrap();

        let paths = response
            .entries
            .iter()
            .filter_map(|entry| entry.reference_path.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["flex-work-ai/docs/readme.md"]);
    }
}
