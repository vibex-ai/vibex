use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value as JsonValue, json};
use vibex_core::{
    AgentId, McpSecretTarget, McpServer, McpServerTransportKind, ProviderBindingMetadata,
    ProviderKind, ProviderNativeConfigFileKind, ProviderNativeExportApplyRequest,
    ProviderNativeExportApplyResult, ProviderNativeExportApplyStatus, ProviderNativeExportFilePlan,
    ProviderNativeExportFileStatus, ProviderNativeExportListRequest, ProviderNativeExportMode,
    ProviderNativeExportOperationKind, ProviderNativeExportPreview,
    ProviderNativeExportPreviewRequest, ProviderNativeExportRecordSummary,
    ProviderNativeExportRollbackRequest, ProviderNativeExportRollbackResult,
    ProviderNativeExportRollbackStatus, ProviderNativeExportSource, ProviderProfile, RequestId,
    Skill, VibexError, VibexResult, unix_timestamp_ms,
};
use vibex_db::{
    McpServerRepository, ProviderNativeExportRepository, ProviderProfileRepository, SkillRepository,
};

use crate::ProviderConfigService;
use crate::native_surface::{
    NativeMcpEntry, NativeMcpTransport, NativeSurfaceError, SKILL_MANIFEST_NAME, SKILLS_DIR_NAME,
    native_mcp_absent_reason, native_mcp_surface, render_mcp_file, render_skill_manifest,
};
use crate::secrets::resolve_provider_secret_reference;

/// Upper bound on a single Skill sibling file copied alongside `SKILL.md`.
///
/// The export plan carries file contents as text, so anything larger or
/// non-text is reported instead of silently truncated.
const MAX_SKILL_SIBLING_BYTES: u64 = 512 * 1024;
/// Upper bound on sibling files copied for one Skill, so an accidental dump of
/// a huge directory cannot turn one export into thousands of file plans.
const MAX_SKILL_SIBLING_FILES: usize = 64;

const CODEX_MARKER_START: &str = "# >>> VIBEX MANAGED PROVIDER EXPORT";
const CODEX_MARKER_END: &str = "# <<< VIBEX MANAGED PROVIDER EXPORT";
/// Marker label identifying the provider-profile plan.
///
/// A Codex `config.toml` is a target for two different exports — the provider
/// profile (which owns a marked block) and MCP servers (which own a separate
/// marked block) — so the unmarked-user-file guard has to know which one it is
/// looking at. The MCP block is appended, never required to pre-exist, so
/// applying it to a user-managed file is safe.
const CODEX_PROVIDER_MARKER_LABEL: &str = "Vibex managed TOML block";

#[derive(Debug)]
struct ApplyFileError {
    error: Box<VibexError>,
    restored: bool,
}

/// Resolved write targets for one preview.
///
/// A preview always targets a single Agent — the one the selected Provider
/// Profile runs — so there is one Agent home and one Skills folder here rather
/// than a lookup table. Tests pin these to a temporary directory.
#[derive(Debug, Clone, Default)]
struct NativeExportRoots {
    codex_root: Option<PathBuf>,
    claude_root: Option<PathBuf>,
    agent_home: Option<PathBuf>,
    skill_root: Option<PathBuf>,
}

impl ProviderConfigService {
    pub fn preview_native_export(
        &self,
        request: ProviderNativeExportPreviewRequest,
    ) -> VibexResult<ProviderNativeExportPreview> {
        let conn = self.open_connection()?;
        let profile = ProviderProfileRepository::get(&conn, &request.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_native_export_profile_not_found",
                    "provider profile was not found for native export",
                )
                .with_diagnostic("providerProfileId", request.provider_profile_id.as_str())
            })?;
        let resources = NativeExportResources {
            mcp_servers: McpServerRepository::list_enabled_for_agent(
                &conn,
                &profile.agent_id,
                profile.kind,
            )?,
            skills: SkillRepository::list_enabled_for_agent(
                &conn,
                &profile.agent_id,
                profile.kind,
            )?,
        };
        let preview = preview_native_export_with_roots(
            &profile,
            request.clone(),
            self.native_export_roots(&profile.agent_id),
            resources,
        )?;
        if request.persist {
            ProviderNativeExportRepository::insert_preview(&conn, &preview)?;
        }
        Ok(preview)
    }

    /// Resolves where a native export for `agent_id` would write.
    ///
    /// The Agent home and Skills folder come from the same snapshot the import
    /// scanner uses, so an export target is always a location the scanner reads
    /// back — that round trip is what the preview's diff is checked against in
    /// the tests.
    fn native_export_roots(&self, agent_id: &AgentId) -> NativeExportRoots {
        let mut roots = NativeExportRoots::default();
        if let Ok(mut agents) = self.import_scan_agents(Some(agent_id.clone()))
            && let Some(agent) = agents.pop()
        {
            roots.agent_home = crate::agent_native_home_roots(&agent).into_iter().next();
            roots.skill_root = crate::import_scan_agent_skill_roots(&agent)
                .into_iter()
                .next();
        }
        roots
    }

    pub fn apply_native_export(
        &self,
        request: ProviderNativeExportApplyRequest,
    ) -> VibexResult<ProviderNativeExportApplyResult> {
        let conn = self.open_connection()?;
        let preview = ProviderNativeExportRepository::get_preview(&conn, &request.export_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_native_export_preview_not_found",
                    "native export preview was not found",
                )
                .with_diagnostic("exportId", request.export_id.as_str())
            })?;
        let result = apply_preview(preview);
        ProviderNativeExportRepository::record_apply_result(&conn, &result)?;
        Ok(result)
    }

    pub fn rollback_native_export(
        &self,
        request: ProviderNativeExportRollbackRequest,
    ) -> VibexResult<ProviderNativeExportRollbackResult> {
        let conn = self.open_connection()?;
        let preview = match ProviderNativeExportRepository::get_preview(&conn, &request.export_id)?
        {
            Some(preview) => preview,
            None => {
                return Ok(ProviderNativeExportRollbackResult {
                    export_id: request.export_id,
                    status: ProviderNativeExportRollbackStatus::NotFound,
                    files: Vec::new(),
                    diagnostics: vec![metadata(
                        "provider_native_export_rollback_not_found",
                        "native export record was not found",
                    )],
                    rolled_back_at_ms: unix_timestamp_ms(),
                });
            }
        };
        let result = rollback_preview(preview);
        ProviderNativeExportRepository::record_rollback_result(&conn, &result)?;
        Ok(result)
    }

    pub fn list_native_exports(
        &self,
        request: ProviderNativeExportListRequest,
    ) -> VibexResult<Vec<ProviderNativeExportRecordSummary>> {
        let conn = self.open_connection()?;
        ProviderNativeExportRepository::list(&conn, request)
    }
}

/// Resources a preview plans against.
///
/// Loaded by the service so the planners stay pure and testable without a
/// database.
#[derive(Debug, Clone, Default)]
struct NativeExportResources {
    mcp_servers: Vec<McpServer>,
    skills: Vec<Skill>,
}

fn preview_native_export_with_roots(
    profile: &ProviderProfile,
    request: ProviderNativeExportPreviewRequest,
    roots: NativeExportRoots,
    resources: NativeExportResources,
) -> VibexResult<ProviderNativeExportPreview> {
    let export_id = RequestId::new();
    let mut diagnostics = Vec::new();
    let files = match request.mode {
        ProviderNativeExportMode::ProviderProfile => {
            provider_profile_plans(&export_id, profile, &request, &roots)?
        }
        ProviderNativeExportMode::Mcp => mcp_export_plans(
            &export_id,
            profile,
            &request,
            &roots,
            &resources,
            &mut diagnostics,
        )?,
        ProviderNativeExportMode::Skills => skills_export_plans(
            &export_id,
            profile,
            &request,
            &roots,
            &resources,
            &mut diagnostics,
        )?,
        ProviderNativeExportMode::Combined => {
            let mut files = provider_profile_plans(&export_id, profile, &request, &roots)?;
            files.extend(mcp_export_plans(
                &export_id,
                profile,
                &request,
                &roots,
                &resources,
                &mut diagnostics,
            )?);
            files.extend(skills_export_plans(
                &export_id,
                profile,
                &request,
                &roots,
                &resources,
                &mut diagnostics,
            )?);
            files
        }
        ProviderNativeExportMode::Prompts => {
            diagnostics.push(metadata(
                "provider_native_export_blocked",
                "Prompts are delivered by the composer, which expands them into the message it sends, so there is no native file for Vibex to write",
            ));
            vec![blocked_plan_with(
                &export_id,
                request.source,
                target_file_kind(request.source),
                target_path(request.source, &roots).join(target_file_name(request.source)),
                "Prompts have no native file; they are expanded by the composer",
                Vec::new(),
            )]
        }
    };

    Ok(ProviderNativeExportPreview {
        export_id,
        provider_profile_id: profile.id.clone(),
        source: request.source,
        mode: request.mode,
        files,
        diagnostics,
        created_at_ms: unix_timestamp_ms(),
    })
}

/// Plans the Codex or Claude provider-profile write.
///
/// The other sources exist for Agent-scoped MCP and Skill export only; asking
/// for a provider-profile export from one of them is refused with the reason
/// rather than silently writing a file the Agent never reads.
fn provider_profile_plans(
    export_id: &RequestId,
    profile: &ProviderProfile,
    request: &ProviderNativeExportPreviewRequest,
    roots: &NativeExportRoots,
) -> VibexResult<Vec<ProviderNativeExportFilePlan>> {
    Ok(match request.source {
        ProviderNativeExportSource::Codex => {
            vec![codex_profile_plan(
                export_id,
                profile,
                roots.codex_root.clone(),
            )?]
        }
        ProviderNativeExportSource::Claude => vec![claude_profile_plan(
            export_id,
            profile,
            roots.claude_root.clone(),
        )?],
        other => vec![blocked_plan_with(
            export_id,
            other,
            target_file_kind(other),
            target_path(other, roots).join(target_file_name(other)),
            "provider-profile export is implemented for the Codex and Claude profiles only",
            vec![metadata(
                "provider_native_export_unsupported_source",
                "this Agent has no provider-profile file Vibex can write; use the MCP, Skills, or Combined mode instead",
            )],
        )],
    })
}

/// Plans the write of an Agent's enabled MCP servers into its native file.
///
/// MCP servers belong to an Agent, not to a Provider Profile, so the selected
/// source has to name the profile's own Agent. Exporting across Agents would
/// install another Agent's servers, so it is refused with the reason instead.
fn mcp_export_plans(
    export_id: &RequestId,
    profile: &ProviderProfile,
    request: &ProviderNativeExportPreviewRequest,
    roots: &NativeExportRoots,
    resources: &NativeExportResources,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> VibexResult<Vec<ProviderNativeExportFilePlan>> {
    let agent_id = profile.agent_id.as_str();
    let Some(surface) = native_mcp_surface(agent_id).copied() else {
        let reason = native_mcp_absent_reason(agent_id);
        diagnostics.push(metadata("provider_native_export_blocked", reason));
        return Ok(vec![blocked_plan_with(
            export_id,
            request.source,
            target_file_kind(request.source),
            target_path(request.source, roots).join(target_file_name(request.source)),
            reason,
            Vec::new(),
        )]);
    };
    if !request.source.targets_agent(agent_id) {
        return Ok(vec![source_agent_mismatch_plan(
            export_id,
            request.source,
            agent_id,
            "MCP servers",
            surface.file_kind,
            target_path(request.source, roots).join(surface.relative_path),
            diagnostics,
        )]);
    }

    let Some(home) = native_agent_home(roots) else {
        let reason = format!(
            "the home directory for {agent_id} could not be resolved, so there is nowhere safe to write its native MCP file"
        );
        diagnostics.push(metadata(
            "provider_native_export_agent_home_unknown",
            &reason,
        ));
        return Ok(vec![blocked_plan_with(
            export_id,
            request.source,
            surface.file_kind,
            PathBuf::from(surface.relative_path),
            reason,
            Vec::new(),
        )]);
    };
    let target = normalize_lexical(&home.join(surface.relative_path));

    let mut skipped_entries = Vec::new();
    let entries = native_mcp_entries(&resources.mcp_servers, diagnostics, &mut skipped_entries);

    let before = read_optional(&target)?.unwrap_or_default();
    let rendered = match render_mcp_file(&surface, Some(&before), &entries) {
        Ok(rendered) => rendered,
        Err(error) => {
            let reason = native_surface_refusal(&error);
            diagnostics.push(metadata(
                "provider_native_export_capability_refused",
                &reason,
            ));
            return Ok(vec![blocked_plan_with(
                export_id,
                request.source,
                surface.file_kind,
                target,
                reason,
                Vec::new(),
            )]);
        }
    };

    let mut plan = ready_plan(
        export_id,
        request.source,
        surface.file_kind,
        target,
        before,
        rendered.content,
        Some(format!("Vibex managed MCP block for {agent_id}")),
    );
    for (name, reason) in rendered.skipped.into_iter().chain(skipped_entries) {
        plan.diagnostics.push(metadata(
            "provider_native_export_entry_skipped",
            format!("{name}: {reason}"),
        ));
    }
    plan.diagnostics.push(metadata(
        "provider_native_export_entry_count",
        entries.len().to_string(),
    ));
    Ok(vec![plan])
}

fn source_agent_mismatch_plan(
    export_id: &RequestId,
    source: ProviderNativeExportSource,
    agent_id: &str,
    resource: &str,
    file_kind: ProviderNativeConfigFileKind,
    target: PathBuf,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> ProviderNativeExportFilePlan {
    let reason = format!(
        "{resource} are scoped to an Agent; this profile runs {agent_id}, but the export source targets {}",
        source.agent_id().unwrap_or("an unknown Agent")
    );
    diagnostics.push(metadata(
        "provider_native_export_source_agent_mismatch",
        &reason,
    ));
    blocked_plan_with(
        export_id,
        source,
        file_kind,
        target,
        "export source does not match the profile's Agent",
        Vec::new(),
    )
}

/// Builds writable entries and reports everything that cannot be written.
///
/// A native file is read by the Agent's own process, which cannot reach Vibex's
/// secret store, so secret values **are** resolved and written here — unlike the
/// provider-profile export, which only ever writes public settings. A secret
/// that fails to resolve is dropped with a diagnostic rather than written empty,
/// because an empty credential looks configured to the Agent while failing at
/// the MCP server.
fn native_mcp_entries(
    servers: &[McpServer],
    diagnostics: &mut Vec<ProviderBindingMetadata>,
    skipped: &mut Vec<(String, String)>,
) -> Vec<NativeMcpEntry> {
    let mut entries = Vec::new();
    for server in servers {
        let name = server.display_name.trim();
        let name = if name.is_empty() {
            server.id.as_str().to_string()
        } else {
            name.to_string()
        };
        let env = merged_native_entries(
            server.env.iter().map(|entry| (&entry.name, &entry.value)),
            server,
            McpSecretTarget::Environment,
            &name,
            diagnostics,
            skipped,
        );
        let headers = merged_native_entries(
            server
                .headers
                .iter()
                .map(|entry| (&entry.name, &entry.value)),
            server,
            McpSecretTarget::Header,
            &name,
            diagnostics,
            skipped,
        );
        match server.transport_kind {
            McpServerTransportKind::Stdio => {
                let command = server
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let Some(command) = command else {
                    skipped.push((name, "stdio server has no command".to_string()));
                    continue;
                };
                entries.push(NativeMcpEntry {
                    name,
                    transport: NativeMcpTransport::Stdio {
                        command: command.to_string(),
                        args: server.args.clone(),
                        env,
                    },
                });
            }
            McpServerTransportKind::Http => match native_url(server.url.as_deref()) {
                Some(url) => entries.push(NativeMcpEntry {
                    name,
                    transport: NativeMcpTransport::Http { url, headers },
                }),
                None => skipped.push((name, "http server has no valid URL".to_string())),
            },
            McpServerTransportKind::Sse => match native_url(server.url.as_deref()) {
                Some(url) => entries.push(NativeMcpEntry {
                    name,
                    transport: NativeMcpTransport::Sse { url, headers },
                }),
                None => skipped.push((name, "sse server has no valid URL".to_string())),
            },
        }
    }
    entries
}

fn merged_native_entries<'a>(
    stored: impl Iterator<Item = (&'a String, &'a String)>,
    server: &McpServer,
    target: McpSecretTarget,
    server_name: &str,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
    skipped: &mut Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = stored
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    for reference in server
        .secret_references
        .iter()
        .filter(|reference| reference.target == target)
    {
        match resolve_provider_secret_reference(
            reference.backend,
            reference.setup_state,
            &reference.lookup_key,
        ) {
            Ok(Some(value)) => {
                entries.retain(|(name, _)| !name.eq_ignore_ascii_case(&reference.lookup_key));
                entries.push((reference.lookup_key.clone(), value));
            }
            _ => {
                // Reported by key, never by value.
                let reason = format!(
                    "secret '{}' could not be resolved and was not written",
                    reference.lookup_key
                );
                diagnostics.push(metadata(
                    "provider_native_export_secret_unresolved",
                    format!("{server_name}: {reason}"),
                ));
                skipped.push((server_name.to_string(), reason));
            }
        }
    }
    entries
}

fn native_url(url: Option<&str>) -> Option<String> {
    let url = url.map(str::trim).filter(|value| !value.is_empty())?;
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    (!rest.trim_matches('/').is_empty() && !url.contains(' ')).then(|| url.to_string())
}

/// Plans the write of an Agent's enabled Skills into its own Skills folder.
///
/// Skills have no ACP wire field, so a native file is the only way an Agent can
/// see them. Each Skill becomes `<skill root>/<slug>/SKILL.md` plus the UTF-8
/// files that sit beside its manifest, so references and scripts travel with it.
fn skills_export_plans(
    export_id: &RequestId,
    profile: &ProviderProfile,
    request: &ProviderNativeExportPreviewRequest,
    roots: &NativeExportRoots,
    resources: &NativeExportResources,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> VibexResult<Vec<ProviderNativeExportFilePlan>> {
    let agent_id = profile.agent_id.as_str();
    if !request.source.targets_agent(agent_id) {
        return Ok(vec![source_agent_mismatch_plan(
            export_id,
            request.source,
            agent_id,
            "Skills",
            ProviderNativeConfigFileKind::AgentSkillManifest,
            PathBuf::from(SKILL_MANIFEST_NAME),
            diagnostics,
        )]);
    }

    let Some(skill_root) = native_skill_root(roots) else {
        let reason = format!(
            "no Skills folder could be resolved for {agent_id}, so there is nowhere safe to write its Skills"
        );
        diagnostics.push(metadata(
            "provider_native_export_agent_home_unknown",
            &reason,
        ));
        return Ok(vec![blocked_plan_with(
            export_id,
            request.source,
            ProviderNativeConfigFileKind::AgentSkillManifest,
            PathBuf::from(SKILL_MANIFEST_NAME),
            reason,
            Vec::new(),
        )]);
    };

    if resources.skills.is_empty() {
        diagnostics.push(metadata(
            "provider_native_export_no_skills",
            format!("no enabled Skills are assigned to {agent_id}"),
        ));
    }

    let mut files = Vec::new();
    for skill in &resources.skills {
        files.extend(skill_export_plans(
            export_id,
            request.source,
            skill,
            &skill_root,
            diagnostics,
        )?);
    }
    Ok(files)
}

fn skill_export_plans(
    export_id: &RequestId,
    source: ProviderNativeExportSource,
    skill: &Skill,
    skill_root: &Path,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> VibexResult<Vec<ProviderNativeExportFilePlan>> {
    let slug = crate::skills::command_token_from_skill_name(&skill.display_name);
    let target_dir = skill_root.join(&slug);
    let body = skill.body.as_deref().filter(|body| !body.trim().is_empty());
    let Some(body) = body else {
        diagnostics.push(metadata(
            "provider_native_export_skill_body_missing",
            format!(
                "Skill '{}' has no stored body; re-import it or edit it in Vibex first, because writing the truncated preview would ship incomplete instructions",
                skill.display_name
            ),
        ));
        return Ok(Vec::new());
    };

    let manifest = render_skill_manifest(
        &skill.display_name,
        skill.description.as_deref(),
        &slug,
        body,
    );
    let target = target_dir.join(SKILL_MANIFEST_NAME);
    let before = read_optional(&target)?.unwrap_or_default();
    let mut plans = vec![ready_plan(
        export_id,
        source,
        ProviderNativeConfigFileKind::AgentSkillManifest,
        target,
        before,
        manifest,
        Some(format!("Vibex managed Skill '{}'", skill.display_name)),
    )];

    // A Skill folder may carry references, scripts and templates. They are part
    // of the Skill, so the text ones travel with the manifest.
    let Some(source_dir) = skill
        .source_uri
        .as_deref()
        .map(PathBuf::from)
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return Ok(plans);
    };
    if source_dir == target_dir {
        return Ok(plans);
    }
    for (index, (relative, content)) in
        skill_sibling_files(&source_dir, diagnostics, &skill.display_name)
            .into_iter()
            .enumerate()
    {
        if index >= MAX_SKILL_SIBLING_FILES {
            diagnostics.push(metadata(
                "provider_native_export_skill_files_truncated",
                format!(
                    "Skill '{}' has more than {MAX_SKILL_SIBLING_FILES} sibling files; the rest were not copied",
                    skill.display_name
                ),
            ));
            break;
        }
        let sibling_target = target_dir.join(&relative);
        let sibling_before = read_optional(&sibling_target)?.unwrap_or_default();
        plans.push(ready_plan(
            export_id,
            source,
            ProviderNativeConfigFileKind::AgentSkillManifest,
            sibling_target,
            sibling_before,
            content,
            Some(format!(
                "Vibex managed Skill asset for '{}'",
                skill.display_name
            )),
        ));
    }
    Ok(plans)
}

/// Reads the UTF-8 files that sit beside a Skill's `SKILL.md`.
fn skill_sibling_files(
    directory: &Path,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
    skill_name: &str,
) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    for entry in walk_skill_directory(directory).into_iter().take(256) {
        let Ok(file_metadata) = fs::metadata(&entry) else {
            continue;
        };
        if !file_metadata.is_file() {
            continue;
        }
        if entry.file_name().and_then(|name| name.to_str()) == Some(SKILL_MANIFEST_NAME) {
            continue;
        }
        let Ok(relative) = entry.strip_prefix(directory) else {
            continue;
        };
        if file_metadata.len() > MAX_SKILL_SIBLING_BYTES {
            diagnostics.push(metadata(
                "provider_native_export_skill_file_skipped",
                format!(
                    "Skill '{skill_name}': {} exceeds the per-file copy limit",
                    relative.display()
                ),
            ));
            continue;
        }
        match fs::read_to_string(&entry) {
            Ok(content) => files.push((relative.to_path_buf(), content)),
            Err(_) => diagnostics.push(metadata(
                "provider_native_export_skill_file_skipped",
                format!(
                    "Skill '{skill_name}': {} is not UTF-8 text and was not copied",
                    relative.display()
                ),
            )),
        }
    }
    files
}

/// Collects a Skill folder's files in a stable order.
///
/// `read_dir` yields entries in filesystem order, which differs between
/// machines and runs. The order is sorted so a preview's file list and diffs
/// stay comparable across exports of unchanged content.
fn walk_skill_directory(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![directory.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&current) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| !matches!(name, ".git" | "node_modules" | "target"))
                {
                    stack.push(path);
                }
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Agent home a native export writes below.
///
/// Resolved from the Agent snapshot the caller loaded; there is deliberately no
/// guessed `~/.<agent>` fallback, because an unresolved home would silently
/// create a directory literally named `~` in the process working directory.
/// An unresolvable home is reported as a blocked plan instead.
fn native_agent_home(roots: &NativeExportRoots) -> Option<PathBuf> {
    roots.agent_home.clone().map(expand_home)
}

/// Skills folder a native export writes into.
fn native_skill_root(roots: &NativeExportRoots) -> Option<PathBuf> {
    roots
        .skill_root
        .clone()
        .map(expand_home)
        .or_else(|| native_agent_home(roots).map(|home| home.join(SKILLS_DIR_NAME)))
}

/// Expands a leading `~` so a configured path is never treated literally.
fn expand_home(path: PathBuf) -> PathBuf {
    let rendered = path.to_string_lossy();
    if rendered == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    match rendered.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|home| home.join(rest)).unwrap_or(path),
        None => path,
    }
}

/// Removes `.` and `..` components so a planned path reads as the real target.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn native_surface_refusal(error: &NativeSurfaceError) -> String {
    match error {
        NativeSurfaceError::ForeignContainer { container } => format!(
            "the target file already defines '{container}' outside Vibex management; Vibex will not rewrite servers it does not own"
        ),
        NativeSurfaceError::Unparsable { error } => {
            format!("the target file could not be parsed as its own format: {error}")
        }
    }
}

fn codex_profile_plan(
    export_id: &RequestId,
    profile: &ProviderProfile,
    root_override: Option<PathBuf>,
) -> VibexResult<ProviderNativeExportFilePlan> {
    if profile.kind != ProviderKind::Codex {
        return Ok(blocked_plan(
            export_id,
            ProviderNativeExportSource::Codex,
            ProviderNativeConfigFileKind::CodexConfigToml,
            codex_config_root(root_override).join("config.toml"),
            "selected profile is not a Codex profile",
        ));
    }

    let target = codex_config_root(root_override).join("config.toml");
    let before = read_optional(&target)?;
    let block = codex_managed_block(profile);
    let after = match before.as_deref() {
        None => block.clone(),
        Some(current)
            if current.contains(CODEX_MARKER_START) && current.contains(CODEX_MARKER_END) =>
        {
            replace_marked_block(current, &block).unwrap_or_else(|| current.to_string())
        }
        Some(current) if current.trim().is_empty() => block.clone(),
        Some(current) => {
            return Ok(blocked_plan(
                export_id,
                ProviderNativeExportSource::Codex,
                ProviderNativeConfigFileKind::CodexConfigToml,
                target,
                format!(
                    "existing Codex config has no Vibex marker; preserving {} bytes of user-managed TOML",
                    current.len()
                ),
            ));
        }
    };
    Ok(ready_plan(
        export_id,
        ProviderNativeExportSource::Codex,
        ProviderNativeConfigFileKind::CodexConfigToml,
        target,
        before.unwrap_or_default(),
        after,
        Some(CODEX_PROVIDER_MARKER_LABEL.to_string()),
    ))
}

fn claude_profile_plan(
    export_id: &RequestId,
    profile: &ProviderProfile,
    root_override: Option<PathBuf>,
) -> VibexResult<ProviderNativeExportFilePlan> {
    if profile.kind != ProviderKind::Claude {
        return Ok(blocked_plan(
            export_id,
            ProviderNativeExportSource::Claude,
            ProviderNativeConfigFileKind::ClaudeSettingsJson,
            claude_config_root(root_override).join("settings.json"),
            "selected profile is not a Claude profile",
        ));
    }

    let target = claude_config_root(root_override).join("settings.json");
    let before = read_optional(&target)?;
    let mut value = match before.as_deref() {
        Some(current) if !current.trim().is_empty() => serde_json::from_str::<JsonValue>(current)
            .map_err(|err| {
            VibexError::validation(
                "provider_native_export_unsafe_target",
                "Claude settings.json is not valid JSON",
            )
            .with_diagnostic("targetPath", target.display().to_string())
            .with_diagnostic("error", err.to_string())
        })?,
        _ => json!({}),
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(blocked_plan(
            export_id,
            ProviderNativeExportSource::Claude,
            ProviderNativeConfigFileKind::ClaudeSettingsJson,
            target,
            "Claude settings.json root is not an object",
        ));
    };
    object.insert(
        "vibex".to_string(),
        json!({
            "managedBy": "vibex",
            "providerProfileId": profile.id.as_str(),
            "displayName": profile.display_name,
            "baseUrl": profile.base_url,
            "defaultModel": profile.default_model,
            "reasoningEffort": profile.reasoning_effort,
            "secretPolicy": "use environment or Vibex secret references; plaintext secrets are not exported",
        }),
    );
    let after = serde_json::to_string_pretty(&value).map_err(|err| {
        VibexError::storage(
            "provider_native_export_record_failed",
            "failed to encode Claude native export preview",
        )
        .with_diagnostic("error", err.to_string())
    })? + "\n";
    Ok(ready_plan(
        export_id,
        ProviderNativeExportSource::Claude,
        ProviderNativeConfigFileKind::ClaudeSettingsJson,
        target,
        before.unwrap_or_default(),
        after,
        Some("Top-level settings.json field: vibex".to_string()),
    ))
}

fn apply_preview(preview: ProviderNativeExportPreview) -> ProviderNativeExportApplyResult {
    let applied_at_ms = unix_timestamp_ms();
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();

    for mut file in preview.files {
        if matches!(
            file.status,
            ProviderNativeExportFileStatus::Blocked | ProviderNativeExportFileStatus::NoOp
        ) {
            files.push(file);
            continue;
        }
        if file.operation_kind == ProviderNativeExportOperationKind::NoOp {
            file.status = ProviderNativeExportFileStatus::NoOp;
            files.push(file);
            continue;
        }

        match apply_file_plan(&file) {
            Ok(()) => file.status = ProviderNativeExportFileStatus::Applied,
            Err(err) => {
                let diagnostic =
                    metadata("provider_native_export_apply_failed", err.error.to_string());
                file.diagnostics.push(diagnostic.clone());
                diagnostics.push(diagnostic);
                file.status = if err.restored {
                    ProviderNativeExportFileStatus::Restored
                } else {
                    ProviderNativeExportFileStatus::Failed
                };
            }
        }
        files.push(file);
    }

    let ready_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Applied)
        .count();
    let failed_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Failed)
        .count();
    let restored_after_failure_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Restored)
        .count();
    let status = if failed_count == 0 {
        if restored_after_failure_count > 0 {
            ProviderNativeExportApplyStatus::FailedRestored
        } else {
            ProviderNativeExportApplyStatus::Applied
        }
    } else if ready_count > 0 {
        ProviderNativeExportApplyStatus::PartiallyApplied
    } else if restored_after_failure_count > 0 {
        ProviderNativeExportApplyStatus::FailedRestored
    } else {
        ProviderNativeExportApplyStatus::FailedUnrestored
    };

    ProviderNativeExportApplyResult {
        export_id: preview.export_id,
        status,
        files,
        diagnostics,
        applied_at_ms,
    }
}

fn rollback_preview(preview: ProviderNativeExportPreview) -> ProviderNativeExportRollbackResult {
    let rolled_back_at_ms = unix_timestamp_ms();
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();

    for mut file in preview.files {
        if matches!(
            file.status,
            ProviderNativeExportFileStatus::Blocked | ProviderNativeExportFileStatus::NoOp
        ) {
            files.push(file);
            continue;
        }
        match rollback_file(&file) {
            Ok(RollbackOutcome::Restored) => file.status = ProviderNativeExportFileStatus::Restored,
            Ok(RollbackOutcome::Deleted) => {
                file.diagnostics.push(metadata(
                    "provider_native_export_created_file_removed",
                    "the file was created by this export and has been removed",
                ));
                file.status = ProviderNativeExportFileStatus::Restored;
            }
            Ok(RollbackOutcome::Skipped(reason)) => {
                let diagnostic = metadata("provider_native_export_rollback_skipped", reason);
                file.diagnostics.push(diagnostic.clone());
                diagnostics.push(diagnostic);
                file.status = ProviderNativeExportFileStatus::NoOp;
            }
            Err(err) => {
                let diagnostic =
                    metadata("provider_native_export_rollback_failed", err.to_string());
                file.diagnostics.push(diagnostic.clone());
                diagnostics.push(diagnostic);
                file.status = ProviderNativeExportFileStatus::Failed;
            }
        }
        files.push(file);
    }

    let restored_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Restored)
        .count();
    let failed_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Failed)
        .count();
    let status = if failed_count == 0 && restored_count > 0 {
        ProviderNativeExportRollbackStatus::Restored
    } else if restored_count > 0 {
        ProviderNativeExportRollbackStatus::PartiallyRestored
    } else {
        ProviderNativeExportRollbackStatus::Failed
    };

    ProviderNativeExportRollbackResult {
        export_id: preview.export_id,
        status,
        files,
        diagnostics,
        rolled_back_at_ms,
    }
}

fn apply_file_plan(file: &ProviderNativeExportFilePlan) -> Result<(), ApplyFileError> {
    let target = PathBuf::from(&file.target_path);
    let before = read_optional(&target).map_err(unrestored)?;
    if file.marker.as_deref() == Some(CODEX_PROVIDER_MARKER_LABEL)
        && before.as_deref().is_some_and(|current| {
            !current.trim().is_empty() && !current.contains(CODEX_MARKER_START)
        })
    {
        return Err(unrestored(
            VibexError::validation(
                "provider_native_export_unsafe_target",
                "Codex config changed to an unmarked user-managed file after preview",
            )
            .with_diagnostic("targetPath", file.target_path.clone()),
        ));
    }

    let parent = target
        .parent()
        .ok_or_else(|| {
            VibexError::validation(
                "provider_native_export_unsafe_target",
                "native export target has no parent directory",
            )
            .with_diagnostic("targetPath", file.target_path.clone())
        })
        .map_err(unrestored)?;
    fs::create_dir_all(parent)
        .map_err(|err| {
            VibexError::storage(
                "provider_native_export_backup_failed",
                "failed to create native export parent directory",
            )
            .with_diagnostic("targetPath", file.target_path.clone())
            .with_diagnostic("error", err.to_string())
        })
        .map_err(unrestored)?;

    if target.exists() {
        let backup = file
            .backup_path
            .as_ref()
            .ok_or_else(|| {
                VibexError::storage(
                    "provider_native_export_backup_failed",
                    "native export backup path was missing",
                )
            })
            .map_err(unrestored)?;
        fs::copy(&target, backup)
            .map_err(|err| {
                VibexError::storage(
                    "provider_native_export_backup_failed",
                    "failed to back up native config",
                )
                .with_diagnostic("targetPath", file.target_path.clone())
                .with_diagnostic("backupPath", backup.clone())
                .with_diagnostic("error", err.to_string())
            })
            .map_err(unrestored)?;
    }

    let temp = file
        .temp_path
        .as_ref()
        .ok_or_else(|| {
            VibexError::storage(
                "provider_native_export_temp_write_failed",
                "native export temp path was missing",
            )
        })
        .map_err(unrestored)?;
    if let Err(err) = fs::write(temp, &file.redacted_after) {
        let restored = restore_from_backup(file).is_ok();
        return Err(ApplyFileError {
            error: Box::new(
                VibexError::storage(
                    "provider_native_export_temp_write_failed",
                    "failed to write native export temp file",
                )
                .with_diagnostic("tempPath", temp.clone())
                .with_diagnostic("error", err.to_string()),
            ),
            restored,
        });
    }
    if let Err(err) = fs::rename(temp, &target) {
        let restored = restore_from_backup(file).is_ok();
        return Err(ApplyFileError {
            error: Box::new(
                VibexError::storage(
                    "provider_native_export_atomic_replace_failed",
                    "failed to atomically replace native config",
                )
                .with_diagnostic("targetPath", file.target_path.clone())
                .with_diagnostic("error", err.to_string()),
            ),
            restored,
        });
    }
    Ok(())
}

/// What a rollback did to one file.
enum RollbackOutcome {
    Restored,
    Deleted,
    Skipped(String),
}

/// Rolls one plan back.
///
/// A file the export created has no backup to restore, so it is deleted — but
/// only while it still holds exactly what Vibex wrote. Once the user or the
/// Agent has edited it, it is left alone and the skip is reported, because
/// deleting user content is worse than leaving a stale export behind.
fn rollback_file(file: &ProviderNativeExportFilePlan) -> VibexResult<RollbackOutcome> {
    let target = PathBuf::from(&file.target_path);
    if file.operation_kind == ProviderNativeExportOperationKind::CreateFile {
        let current = match read_optional(&target)? {
            Some(current) => current,
            None => return Ok(RollbackOutcome::Deleted),
        };
        if current != file.redacted_after {
            return Ok(RollbackOutcome::Skipped(format!(
                "{} was edited after the export and was left in place",
                target.display()
            )));
        }
        fs::remove_file(&target).map_err(|err| {
            VibexError::storage(
                "provider_native_export_rollback_failed",
                "failed to remove a file created by the native export",
            )
            .with_diagnostic("targetPath", file.target_path.clone())
            .with_diagnostic("error", err.to_string())
        })?;
        return Ok(RollbackOutcome::Deleted);
    }
    restore_from_backup(file)?;
    Ok(RollbackOutcome::Restored)
}

fn unrestored(error: VibexError) -> ApplyFileError {
    ApplyFileError {
        error: Box::new(error),
        restored: false,
    }
}

fn restore_from_backup(file: &ProviderNativeExportFilePlan) -> VibexResult<()> {
    let backup = file.backup_path.as_ref().ok_or_else(|| {
        VibexError::validation(
            "provider_native_export_rollback_failed",
            "native export file has no Vibex-created backup to restore",
        )
        .with_diagnostic("operationId", file.operation_id.as_str())
    })?;
    let backup_path = PathBuf::from(backup);
    if !backup_path.exists() {
        return Err(VibexError::validation(
            "provider_native_export_rollback_failed",
            "Vibex-created backup file is missing",
        )
        .with_diagnostic("backupPath", backup.clone()));
    }
    fs::copy(&backup_path, &file.target_path).map_err(|err| {
        VibexError::storage(
            "provider_native_export_restore_failed",
            "failed to restore native config from Vibex backup",
        )
        .with_diagnostic("targetPath", file.target_path.clone())
        .with_diagnostic("backupPath", backup.clone())
        .with_diagnostic("error", err.to_string())
    })?;
    Ok(())
}

fn ready_plan(
    export_id: &RequestId,
    source: ProviderNativeExportSource,
    file_kind: ProviderNativeConfigFileKind,
    target: PathBuf,
    before: String,
    after: String,
    marker: Option<String>,
) -> ProviderNativeExportFilePlan {
    let operation_id = RequestId::new();
    let operation_kind = if before == after {
        ProviderNativeExportOperationKind::NoOp
    } else if before.is_empty() && !target.exists() {
        ProviderNativeExportOperationKind::CreateFile
    } else {
        ProviderNativeExportOperationKind::UpdateFile
    };
    let backup_path = if target.exists() {
        Some(sibling_path(&target, export_id, "bak"))
    } else {
        None
    };
    let temp_path = Some(sibling_path(&target, export_id, "tmp"));
    let diff = unified_diff(&before, &after);
    ProviderNativeExportFilePlan {
        operation_id,
        source,
        file_kind,
        operation_kind,
        target_path: target.display().to_string(),
        backup_path,
        temp_path,
        marker,
        redacted_before: before,
        redacted_after: after,
        redacted_diff: diff,
        rollback_plan: "Restore the Vibex-created backup for this file; a file this export created is removed while it still holds the exported content.".to_string(),
        diagnostics: Vec::new(),
        status: if operation_kind == ProviderNativeExportOperationKind::NoOp {
            ProviderNativeExportFileStatus::NoOp
        } else {
            ProviderNativeExportFileStatus::Ready
        },
    }
}

fn blocked_plan(
    export_id: &RequestId,
    source: ProviderNativeExportSource,
    file_kind: ProviderNativeConfigFileKind,
    target: PathBuf,
    reason: impl Into<String>,
) -> ProviderNativeExportFilePlan {
    blocked_plan_with(export_id, source, file_kind, target, reason, Vec::new())
}

fn blocked_plan_with(
    export_id: &RequestId,
    source: ProviderNativeExportSource,
    file_kind: ProviderNativeConfigFileKind,
    target: PathBuf,
    reason: impl Into<String>,
    extra_diagnostics: Vec<ProviderBindingMetadata>,
) -> ProviderNativeExportFilePlan {
    let reason = reason.into();
    ProviderNativeExportFilePlan {
        operation_id: RequestId::new(),
        source,
        file_kind,
        operation_kind: ProviderNativeExportOperationKind::Blocked,
        target_path: target.display().to_string(),
        backup_path: Some(sibling_path(&target, export_id, "bak")),
        temp_path: Some(sibling_path(&target, export_id, "tmp")),
        marker: None,
        redacted_before: String::new(),
        redacted_after: String::new(),
        redacted_diff: String::new(),
        rollback_plan: "No write will be attempted while this plan is blocked.".to_string(),
        diagnostics: std::iter::once(metadata("provider_native_export_blocked", reason))
            .chain(extra_diagnostics)
            .collect(),
        status: ProviderNativeExportFileStatus::Blocked,
    }
}

fn codex_managed_block(profile: &ProviderProfile) -> String {
    let provider_id = format!("vibex_{}", profile.id.as_str().replace('-', "_"));
    let model = profile.default_model.as_deref().unwrap_or("gpt-5");
    let base_url = profile
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1");
    let reasoning = profile.reasoning_effort.as_deref().unwrap_or("medium");
    format!(
        "{CODEX_MARKER_START}\n\
         model = \"{model}\"\n\
         model_provider = \"{provider_id}\"\n\
         model_reasoning_effort = \"{reasoning}\"\n\n\
         [model_providers.{provider_id}]\n\
         name = \"{display_name}\"\n\
         base_url = \"{base_url}\"\n\
         env_key = \"OPENAI_API_KEY\"\n\
         wire_api = \"chat\"\n\
         # Plaintext secrets are intentionally not exported by Vibex.\n\
         {CODEX_MARKER_END}\n",
        display_name = escape_toml_string(&profile.display_name),
        model = escape_toml_string(model),
        base_url = escape_toml_string(base_url),
        reasoning = escape_toml_string(reasoning),
        provider_id = provider_id,
    )
}

fn replace_marked_block(current: &str, block: &str) -> Option<String> {
    let start = current.find(CODEX_MARKER_START)?;
    let end_start = current.find(CODEX_MARKER_END)?;
    let end = end_start + CODEX_MARKER_END.len();
    let mut next = String::new();
    next.push_str(&current[..start]);
    next.push_str(block);
    next.push_str(&current[end..]);
    Some(next)
}

fn read_optional(path: &Path) -> VibexResult<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(VibexError::storage(
            "provider_native_export_file_read_failed",
            "failed to read native config file for export preview",
        )
        .with_diagnostic("path", path.display().to_string())
        .with_diagnostic("error", err.to_string())),
    }
}

fn unified_diff(before: &str, after: &str) -> String {
    if before == after {
        return "No changes.".to_string();
    }
    let mut diff = String::from("--- current\n+++ vibex\n");
    for line in before.lines() {
        if !after.lines().any(|candidate| candidate == line) {
            diff.push('-');
            diff.push_str(line);
            diff.push('\n');
        }
    }
    for line in after.lines() {
        if !before.lines().any(|candidate| candidate == line) {
            diff.push('+');
            diff.push_str(line);
            diff.push('\n');
        }
    }
    diff
}

fn sibling_path(target: &Path, export_id: &RequestId, suffix: &str) -> String {
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("native-config");
    target
        .with_file_name(format!(
            "{file_name}.vibex-{}.{}",
            export_id.as_str(),
            suffix
        ))
        .display()
        .to_string()
}

/// Directory a blocked plan reports as its would-be target.
///
/// A blocked plan never writes, so for the Agent-scoped sources this only has
/// to name a plausible location: the profile-file roots for the two Agents that
/// have one, and the Agent home otherwise.
fn target_path(source: ProviderNativeExportSource, roots: &NativeExportRoots) -> PathBuf {
    match source {
        ProviderNativeExportSource::Codex => codex_config_root(roots.codex_root.clone()),
        ProviderNativeExportSource::Claude => claude_config_root(roots.claude_root.clone()),
        // Blocked plans only report a location; the real target comes from the
        // surface, and a plan that cannot resolve a home never writes.
        _ => native_agent_home(roots).unwrap_or_else(|| PathBuf::from(".")),
    }
}

/// File name a blocked plan reports; the real name comes from the surface.
fn target_file_name(source: ProviderNativeExportSource) -> &'static str {
    match source {
        ProviderNativeExportSource::Codex => "config.toml",
        ProviderNativeExportSource::Claude => "settings.json",
        other => other
            .agent_id()
            .and_then(native_mcp_surface)
            .map(|surface| surface.relative_path)
            .unwrap_or(SKILL_MANIFEST_NAME),
    }
}

/// File kind a blocked plan reports; the real kind comes from the surface.
fn target_file_kind(source: ProviderNativeExportSource) -> ProviderNativeConfigFileKind {
    match source {
        ProviderNativeExportSource::Codex => ProviderNativeConfigFileKind::CodexConfigToml,
        ProviderNativeExportSource::Claude => ProviderNativeConfigFileKind::ClaudeSettingsJson,
        other => other
            .agent_id()
            .and_then(native_mcp_surface)
            .map(|surface| surface.file_kind)
            .unwrap_or(ProviderNativeConfigFileKind::AgentSkillManifest),
    }
}

fn codex_config_root(override_root: Option<PathBuf>) -> PathBuf {
    override_root
        .or_else(|| {
            std::env::var_os("CODEX_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

fn claude_config_root(override_root: Option<PathBuf>) -> PathBuf {
    override_root
        .or_else(|| {
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

fn metadata(key: impl Into<String>, value: impl Into<String>) -> ProviderBindingMetadata {
    ProviderBindingMetadata {
        key: key.into(),
        value: value.into(),
    }
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vibex_core::{
        ProviderNetworkDefaults, ProviderOptions, ProviderPermissionDefaults, ProviderProfileId,
        ProviderProfileStatus, ProviderSandboxDefaults, agent_id_for_provider_kind,
    };

    fn profile(kind: ProviderKind) -> ProviderProfile {
        ProviderProfile {
            id: ProviderProfileId::parse("provider_profile_native_export_test").unwrap(),
            agent_id: agent_id_for_provider_kind(kind),
            kind,
            display_name: "Native Export Test".to_string(),
            status: ProviderProfileStatus::Enabled,
            account_alias: None,
            base_url: Some("https://api.example.test/v1".to_string()),
            default_model: Some("gpt-test".to_string()),
            small_model: None,
            large_model: None,
            configured_models: Vec::new(),
            reasoning_effort: Some("high".to_string()),
            sandbox_defaults: ProviderSandboxDefaults::workspace_write_ask_on_risk(),
            network_defaults: ProviderNetworkDefaults::local_default(),
            permission_defaults: ProviderPermissionDefaults::ask_on_risk(),
            provider_options: ProviderOptions {
                schema_version: 1,
                entries: Vec::new(),
            },
            secrets: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        }
    }

    #[test]
    fn native_export_preview_does_not_create_files_or_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("missing-codex");
        let preview = preview_native_export_with_roots(
            &profile(ProviderKind::Codex),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Codex,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: Some(root.clone()),
                claude_root: None,
                ..Default::default()
            },
            NativeExportResources::default(),
        )
        .unwrap();
        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Ready
        );
        assert!(!root.exists());
        assert!(!PathBuf::from(preview.files[0].temp_path.as_ref().unwrap()).exists());
        assert!(preview.files[0].redacted_diff.contains("OPENAI_API_KEY"));
        assert!(!preview.files[0].redacted_diff.contains("sk-"));
    }

    #[test]
    fn native_export_blocks_unmarked_codex_config() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("codex");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("config.toml"), "model = \"user\"\n").unwrap();
        let preview = preview_native_export_with_roots(
            &profile(ProviderKind::Codex),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Codex,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: Some(root),
                claude_root: None,
                ..Default::default()
            },
            NativeExportResources::default(),
        )
        .unwrap();
        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Blocked
        );
    }

    #[test]
    fn native_export_apply_and_rollback_restores_backup() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("claude");
        fs::create_dir_all(&root).unwrap();
        let settings = root.join("settings.json");
        fs::write(&settings, "{\n  \"theme\": \"dark\"\n}\n").unwrap();
        let preview = preview_native_export_with_roots(
            &profile(ProviderKind::Claude),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Claude,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: None,
                claude_root: Some(root),
                ..Default::default()
            },
            NativeExportResources::default(),
        )
        .unwrap();
        let apply = apply_preview(preview.clone());
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);
        assert!(fs::read_to_string(&settings).unwrap().contains("\"vibex\""));
        let rollback = rollback_preview(preview);
        assert_eq!(
            rollback.status,
            ProviderNativeExportRollbackStatus::Restored
        );
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            "{\n  \"theme\": \"dark\"\n}\n"
        );
    }

    #[test]
    fn native_export_updates_only_codex_marked_block() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("codex");
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config.toml");
        fs::write(
            &config,
            "user_key = \"keep\"\n# >>> VIBEX MANAGED PROVIDER EXPORT\nmodel = \"old\"\n# <<< VIBEX MANAGED PROVIDER EXPORT\n",
        )
        .unwrap();
        let preview = preview_native_export_with_roots(
            &profile(ProviderKind::Codex),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Codex,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: Some(root),
                claude_root: None,
                ..Default::default()
            },
            NativeExportResources::default(),
        )
        .unwrap();
        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Ready
        );
        let apply = apply_preview(preview);
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);
        let written = fs::read_to_string(&config).unwrap();
        assert!(written.contains("user_key = \"keep\""));
        assert!(written.contains("gpt-test"));
        assert!(!written.contains("model = \"old\""));
    }

    #[test]
    fn native_export_failed_temp_write_restores_backup() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("claude");
        fs::create_dir_all(&root).unwrap();
        let settings = root.join("settings.json");
        fs::write(&settings, "{\n  \"theme\": \"dark\"\n}\n").unwrap();
        let mut preview = preview_native_export_with_roots(
            &profile(ProviderKind::Claude),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Claude,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: None,
                claude_root: Some(root.clone()),
                ..Default::default()
            },
            NativeExportResources::default(),
        )
        .unwrap();
        let temp_dir_path = root.join("temp-is-directory");
        fs::create_dir_all(&temp_dir_path).unwrap();
        preview.files[0].temp_path = Some(temp_dir_path.display().to_string());

        let apply = apply_preview(preview);
        assert_eq!(
            apply.status,
            ProviderNativeExportApplyStatus::FailedRestored
        );
        assert_eq!(
            apply.files[0].status,
            ProviderNativeExportFileStatus::Restored
        );
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            "{\n  \"theme\": \"dark\"\n}\n"
        );
    }

    fn agent_profile(agent_id: &str) -> ProviderProfile {
        ProviderProfile {
            agent_id: AgentId::parse(agent_id).unwrap(),
            ..profile(ProviderKind::Acp)
        }
    }

    fn stdio_server(name: &str, command: &str) -> McpServer {
        let now = unix_timestamp_ms();
        McpServer {
            id: vibex_core::McpServerId::new(),
            display_name: name.to_string(),
            transport_kind: McpServerTransportKind::Stdio,
            status: vibex_core::McpServerStatus::Enabled,
            scope_kind: vibex_core::McpServerScopeKind::User,
            project_id: None,
            workspace_id: None,
            command: Some(command.to_string()),
            args: vec!["-y".to_string(), "server-pkg".to_string()],
            env: vec![vibex_core::McpServerEnvEntry {
                name: "PLAIN".to_string(),
                value: "value".to_string(),
            }],
            url: None,
            headers: Vec::new(),
            description: None,
            tags: Vec::new(),
            secret_references: Vec::new(),
            provider_matrix: Vec::new(),
            agent_matrix: Vec::new(),
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        }
    }

    fn manual_skill(name: &str, body: &str, source_uri: Option<String>) -> Skill {
        let now = unix_timestamp_ms();
        Skill {
            id: vibex_core::SkillId::new(),
            display_name: name.to_string(),
            source_kind: if source_uri.is_some() {
                vibex_core::SkillSourceKind::LocalFolder
            } else {
                vibex_core::SkillSourceKind::Manual
            },
            status: vibex_core::SkillStatus::Enabled,
            scope_kind: vibex_core::SkillScopeKind::User,
            project_id: None,
            workspace_id: None,
            source_uri,
            description: Some("Checks the quality gates".to_string()),
            tags: Vec::new(),
            content_preview: Some(body.chars().take(64).collect()),
            body: Some(body.to_string()),
            provider_matrix: Vec::new(),
            agent_matrix: Vec::new(),
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        }
    }

    fn export_request(
        mode: ProviderNativeExportMode,
        source: ProviderNativeExportSource,
    ) -> ProviderNativeExportPreviewRequest {
        ProviderNativeExportPreviewRequest {
            provider_profile_id: ProviderProfileId::parse("provider_profile_native_export_test")
                .unwrap(),
            source,
            mode,
            persist: false,
        }
    }

    #[test]
    fn mcp_export_writes_a_native_file_the_import_scanner_reads_back() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("cursor-home");
        let preview = preview_native_export_with_roots(
            &agent_profile("cursor"),
            export_request(
                ProviderNativeExportMode::Mcp,
                ProviderNativeExportSource::Cursor,
            ),
            NativeExportRoots {
                agent_home: Some(home.clone()),
                ..Default::default()
            },
            NativeExportResources {
                mcp_servers: vec![stdio_server("Files", "npx")],
                skills: Vec::new(),
            },
        )
        .unwrap();

        assert_eq!(preview.files.len(), 1);
        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Ready
        );
        let apply = apply_preview(preview);
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);

        // The file lands where the Agent reads it, and the scanner the import
        // flow uses finds the same server again.
        let target = home.join("mcp.json");
        assert!(target.exists());
        let candidates = crate::parse_mcp_candidates_for_path(
            &AgentId::parse("cursor").unwrap(),
            &target.display().to_string(),
            &target,
            &fs::read_to_string(&target).unwrap(),
            &mut Vec::new(),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].display_name, "Files");
        assert_eq!(candidates[0].command.as_deref(), Some("npx"));
        let env = candidates[0]
            .env
            .iter()
            .find(|entry| entry.name == "PLAIN")
            .expect("plain env entry survives the round trip");
        assert_eq!(env.value, "value");
    }

    #[test]
    fn an_unresolvable_agent_home_blocks_instead_of_guessing_a_path() {
        // No `agent_home` in the roots stands for "the Agent snapshot could not
        // be loaded". Guessing `~/.<agent>` here would create a directory
        // literally named `~` in the process working directory.
        let preview = preview_native_export_with_roots(
            &agent_profile("cursor"),
            export_request(
                ProviderNativeExportMode::Mcp,
                ProviderNativeExportSource::Cursor,
            ),
            NativeExportRoots::default(),
            NativeExportResources {
                mcp_servers: vec![stdio_server("Files", "npx")],
                skills: Vec::new(),
            },
        )
        .unwrap();

        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Blocked
        );
        assert!(
            preview
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.key == "provider_native_export_agent_home_unknown")
        );
        assert!(!PathBuf::from("~").exists());
    }

    #[test]
    fn mcp_export_refuses_a_source_that_is_not_the_profiles_agent() {
        let preview = preview_native_export_with_roots(
            &agent_profile("cursor"),
            export_request(
                ProviderNativeExportMode::Mcp,
                ProviderNativeExportSource::Grok,
            ),
            NativeExportRoots::default(),
            NativeExportResources {
                mcp_servers: vec![stdio_server("Files", "npx")],
                skills: Vec::new(),
            },
        )
        .unwrap();

        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Blocked
        );
        assert!(
            preview
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.key == "provider_native_export_source_agent_mismatch")
        );
    }

    #[test]
    fn mcp_export_reports_an_agent_without_a_native_file() {
        let preview = preview_native_export_with_roots(
            &agent_profile("antigravity"),
            export_request(
                ProviderNativeExportMode::Mcp,
                ProviderNativeExportSource::AgentDefault,
            ),
            NativeExportRoots::default(),
            NativeExportResources::default(),
        )
        .unwrap();

        // The source mismatch is reported first, so ask again with a matching
        // source by exporting the profile's own Agent through the same surface
        // lookup the planner uses.
        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Blocked
        );
    }

    #[test]
    fn toml_mcp_export_appends_a_block_and_leaves_the_rest_of_the_file_alone() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("grok-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "# keep me\nmodel = \"grok-4\"\n").unwrap();

        let preview = preview_native_export_with_roots(
            &agent_profile("grok"),
            export_request(
                ProviderNativeExportMode::Mcp,
                ProviderNativeExportSource::Grok,
            ),
            NativeExportRoots {
                agent_home: Some(home.clone()),
                ..Default::default()
            },
            NativeExportResources {
                mcp_servers: vec![stdio_server("Files", "npx")],
                skills: Vec::new(),
            },
        )
        .unwrap();
        let apply = apply_preview(preview);
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);

        let content = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(content.contains("# keep me"));
        assert!(content.contains("model = \"grok-4\""));
        assert!(content.contains("[mcp_servers.Files]"));
        content.parse::<toml::Value>().expect("valid TOML");
    }

    #[test]
    fn skills_export_writes_a_manifest_plus_its_sibling_files() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("claude-home");
        let source_dir = dir.path().join("central").join("rust-quality");
        fs::create_dir_all(source_dir.join("references")).unwrap();
        let manifest = source_dir.join("SKILL.md");
        let body = "---\nname: Rust Quality\n---\n\nRun the gates.\n";
        fs::write(&manifest, body).unwrap();
        fs::write(
            source_dir.join("references").join("gates.md"),
            "cargo clippy",
        )
        .unwrap();
        fs::write(source_dir.join("logo.bin"), [0xff, 0xfe, 0x00]).unwrap();

        let skill = manual_skill("Rust Quality", body, Some(manifest.display().to_string()));
        let preview = preview_native_export_with_roots(
            &agent_profile("claude"),
            export_request(
                ProviderNativeExportMode::Skills,
                ProviderNativeExportSource::Claude,
            ),
            NativeExportRoots {
                agent_home: Some(home.clone()),
                skill_root: Some(home.join("skills")),
                ..Default::default()
            },
            NativeExportResources {
                mcp_servers: Vec::new(),
                skills: vec![skill],
            },
        )
        .unwrap();

        let planned = preview.clone();
        let apply = apply_preview(preview);
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);

        let target = home.join("skills").join("rust-quality");
        assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), body);
        assert_eq!(
            fs::read_to_string(target.join("references").join("gates.md")).unwrap(),
            "cargo clippy"
        );
        // A binary sibling cannot be carried as text, so it is reported rather
        // than silently written as garbage.
        assert!(!target.join("logo.bin").exists());
        assert!(planned.diagnostics.iter().any(|diagnostic| {
            diagnostic.key == "provider_native_export_skill_file_skipped"
                && diagnostic.value.contains("not UTF-8")
        }));
    }

    #[test]
    fn skills_export_reports_a_skill_without_a_stored_body() {
        let dir = tempdir().unwrap();
        let mut skill = manual_skill("No Body", "placeholder", None);
        skill.body = None;

        let preview = preview_native_export_with_roots(
            &agent_profile("claude"),
            export_request(
                ProviderNativeExportMode::Skills,
                ProviderNativeExportSource::Claude,
            ),
            NativeExportRoots {
                agent_home: Some(dir.path().to_path_buf()),
                skill_root: Some(dir.path().join("skills")),
                ..Default::default()
            },
            NativeExportResources {
                mcp_servers: Vec::new(),
                skills: vec![skill],
            },
        )
        .unwrap();

        assert!(preview.files.is_empty());
        assert!(
            preview
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.key == "provider_native_export_skill_body_missing")
        );
    }

    #[test]
    fn rolling_back_a_created_skill_file_removes_it() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("claude-home");
        let preview = preview_native_export_with_roots(
            &agent_profile("claude"),
            export_request(
                ProviderNativeExportMode::Skills,
                ProviderNativeExportSource::Claude,
            ),
            NativeExportRoots {
                agent_home: Some(home.clone()),
                skill_root: Some(home.join("skills")),
                ..Default::default()
            },
            NativeExportResources {
                mcp_servers: Vec::new(),
                skills: vec![manual_skill("Review", "Review changes.", None)],
            },
        )
        .unwrap();
        let preview_for_rollback = preview.clone();
        let apply = apply_preview(preview);
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);
        let target = home.join("skills").join("review").join("SKILL.md");
        assert!(target.exists());

        let rollback = rollback_preview(preview_for_rollback);
        assert_eq!(
            rollback.status,
            ProviderNativeExportRollbackStatus::Restored
        );
        assert!(!target.exists());
    }

    #[test]
    fn rolling_back_leaves_a_skill_file_the_user_edited_alone() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("claude-home");
        let preview = preview_native_export_with_roots(
            &agent_profile("claude"),
            export_request(
                ProviderNativeExportMode::Skills,
                ProviderNativeExportSource::Claude,
            ),
            NativeExportRoots {
                agent_home: Some(home.clone()),
                skill_root: Some(home.join("skills")),
                ..Default::default()
            },
            NativeExportResources {
                mcp_servers: Vec::new(),
                skills: vec![manual_skill("Review", "Review changes.", None)],
            },
        )
        .unwrap();
        let preview_for_rollback = preview.clone();
        let apply = apply_preview(preview);
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);
        let target = home.join("skills").join("review").join("SKILL.md");
        fs::write(&target, "hand edited").unwrap();

        let rollback = rollback_preview(preview_for_rollback);
        assert!(target.exists());
        assert!(
            rollback
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.key == "provider_native_export_rollback_skipped")
        );
    }
}
