//! OpenSSH config discovery and effective-value resolution.
//!
//! Discovery is intentionally separate from connection setup: it only enumerates
//! concrete `Host` aliases and never opens a connection. `ssh -G` is used when a
//! caller needs the effective hostname/user/port for one alias.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vibex_core::{VibexError, VibexResult};

const MAX_INCLUDE_DEPTH: usize = 8;
const MAX_WARNINGS: usize = 32;
const MAX_ALIAS_LEN: usize = 255;

/// A concrete alias found in one config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHostEntry {
    pub alias: String,
    pub source: PathBuf,
    pub line: usize,
}

/// Discovery never fails as a whole: a bad line or a missing include becomes a
/// warning so the user can still add a host manually.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshDiscovery {
    pub hosts: Vec<SshHostEntry>,
    pub warnings: Vec<String>,
}

/// Values `ssh -G <alias>` reports, reduced to the fields the desktop needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshEffectiveConfig {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<PathBuf>,
    pub proxy_jump: Option<String>,
}

/// `~/.ssh/config` on Unix and `%USERPROFILE%\.ssh\config` on Windows.
pub fn default_ssh_config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = env::var_os("HOME");
    home.filter(|value| !value.to_string_lossy().is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".ssh").join("config"))
}

pub fn discover_default_hosts() -> SshDiscovery {
    default_ssh_config_path()
        .map(|path| discover_hosts(&path))
        .unwrap_or_default()
}

/// Enumerates concrete aliases from `config_path` and its includes.
pub fn discover_hosts(config_path: &Path) -> SshDiscovery {
    if !config_path.is_file() {
        return SshDiscovery::default();
    }
    let base = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut state = DiscoveryState::new(base);
    state.visit(config_path, 0, true);
    state.finish()
}

/// Resolves one alias through the system OpenSSH client.
pub fn resolve_effective(
    ssh_binary: &Path,
    config_path: Option<&Path>,
    alias: &str,
) -> VibexResult<SshEffectiveConfig> {
    validate_alias(alias)?;
    let mut command = Command::new(ssh_binary);
    if let Some(path) = config_path {
        command.arg("-F").arg(expand_tilde_path(path));
    }
    command.arg("-G").arg(alias);
    command.env("LC_ALL", "C");
    let output = command.output().map_err(|error| {
        VibexError::process("ssh_g_spawn_failed", "the SSH client could not be started")
            .with_diagnostic("error", error.to_string())
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(VibexError::process(
            "ssh_g_failed",
            "the SSH client could not resolve the host",
        )
        .with_diagnostic("stderr", truncate(&stderr, 512)));
    }
    Ok(SshEffectiveConfig::parse(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

pub fn resolve_effective_default(
    config_path: Option<&Path>,
    alias: &str,
) -> VibexResult<SshEffectiveConfig> {
    resolve_effective(Path::new("ssh"), config_path, alias)
}

impl SshEffectiveConfig {
    /// Parses the `key value` lines printed by `ssh -G`.
    pub fn parse(output: &str) -> Self {
        let mut config = Self::default();
        for line in output.lines() {
            let mut parts = line.splitn(2, |character: char| character.is_whitespace());
            let Some(key) = parts.next() else {
                continue;
            };
            let value = parts.next().unwrap_or("").trim();
            if key.is_empty() || value.is_empty() {
                continue;
            }
            match key {
                "hostname" if config.hostname.is_none() => {
                    config.hostname = Some(unquote(value));
                }
                "user" if config.user.is_none() => {
                    config.user = Some(unquote(value));
                }
                "port" if config.port.is_none() => {
                    if let Ok(port) = value.parse::<u16>()
                        && port > 0
                    {
                        config.port = Some(port);
                    }
                }
                "identityfile" if config.identity_file.is_none() => {
                    config.identity_file = Some(PathBuf::from(unquote(value)));
                }
                "proxyjump" if config.proxy_jump.is_none() => {
                    let value = unquote(value);
                    if !value.eq_ignore_ascii_case("none") && !value.is_empty() {
                        config.proxy_jump = Some(value);
                    }
                }
                _ => {}
            }
        }
        config
    }
}

struct DiscoveryState {
    hosts: Vec<SshHostEntry>,
    warnings: Vec<String>,
    visited: HashSet<PathBuf>,
    seen_aliases: HashSet<String>,
    include_base: PathBuf,
}

impl DiscoveryState {
    fn new(include_base: PathBuf) -> Self {
        Self {
            hosts: Vec::new(),
            warnings: Vec::new(),
            visited: HashSet::new(),
            seen_aliases: HashSet::new(),
            include_base,
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(message.into());
        }
    }

    fn visit(&mut self, path: &Path, depth: usize, required: bool) {
        if depth > MAX_INCLUDE_DEPTH {
            self.warn(format!("include depth exceeded at {}", path.display()));
            return;
        }
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !self.visited.insert(canonical.clone()) {
            return;
        }
        match fs::read_to_string(&canonical) {
            Ok(content) => self.parse(&canonical, &content, depth),
            Err(error) => {
                if required {
                    self.warn(format!("cannot read {}: {error}", path.display()));
                }
            }
        }
    }

    fn parse(&mut self, source: &Path, content: &str, depth: usize) {
        let mut in_match = false;
        for (index, raw) in content.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((keyword, value)) = split_directive(line) else {
                self.warn(format!(
                    "{}:{}: invalid directive",
                    source.display(),
                    index + 1
                ));
                continue;
            };
            match keyword.to_ascii_lowercase().as_str() {
                "host" => {
                    in_match = false;
                    let aliases = value.split_whitespace().collect::<Vec<_>>();
                    if aliases.is_empty() {
                        self.warn(format!(
                            "{}:{}: Host has no aliases",
                            source.display(),
                            index + 1
                        ));
                        continue;
                    }
                    for alias in aliases {
                        if !is_selectable_alias(alias) {
                            continue;
                        }
                        if alias.len() > MAX_ALIAS_LEN {
                            self.warn(format!(
                                "{}:{}: alias is too long",
                                source.display(),
                                index + 1
                            ));
                            continue;
                        }
                        if self.seen_aliases.insert(alias.to_string()) {
                            self.hosts.push(SshHostEntry {
                                alias: alias.to_string(),
                                source: source.to_path_buf(),
                                line: index + 1,
                            });
                        }
                    }
                }
                "match" => in_match = true,
                "include" if !in_match => {
                    for pattern in split_include_patterns(value) {
                        self.include(&pattern, depth);
                    }
                }
                _ => {}
            }
        }
    }

    fn include(&mut self, pattern: &str, depth: usize) {
        if pattern.is_empty() {
            return;
        }
        let expanded = expand_tilde(pattern);
        let path = if expanded.is_absolute() {
            expanded
        } else {
            self.include_base.join(expanded)
        };
        let pattern_text = path.to_string_lossy().to_string();
        if pattern_text.contains('*') || pattern_text.contains('?') || pattern_text.contains('[') {
            let entries = match glob::glob(&pattern_text) {
                Ok(entries) => entries,
                Err(error) => {
                    self.warn(format!("invalid include pattern {pattern}: {error}"));
                    return;
                }
            };
            let mut matched = Vec::new();
            for entry in entries {
                match entry {
                    Ok(path) if path.is_file() => matched.push(path),
                    Ok(_) => {}
                    Err(error) => self.warn(format!("include {pattern}: {error}")),
                }
            }
            matched.sort();
            if matched.is_empty() {
                self.warn(format!("include matched nothing: {pattern}"));
            }
            for path in matched {
                self.visit(&path, depth + 1, true);
            }
        } else {
            self.visit(&path, depth + 1, true);
        }
    }

    fn finish(mut self) -> SshDiscovery {
        self.hosts.sort_by(|left, right| {
            left.alias
                .cmp(&right.alias)
                .then_with(|| left.source.cmp(&right.source))
        });
        SshDiscovery {
            hosts: self.hosts,
            warnings: self.warnings,
        }
    }
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '#' if !quoted => {
                let preceded_by_space = line[..index]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace);
                if preceded_by_space {
                    return &line[..index];
                }
            }
            _ => {}
        }
    }
    line
}

fn split_directive(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(2, |character: char| {
        character.is_whitespace() || character == '='
    });
    let keyword = parts.next()?.trim();
    if keyword.is_empty() {
        return None;
    }
    let mut value = parts.next().unwrap_or("").trim();
    if let Some(rest) = value.strip_prefix('=') {
        value = rest.trim();
    }
    Some((keyword, value))
}

fn split_include_patterns(value: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in value.chars() {
        match character {
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    patterns.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if !current.is_empty() {
        patterns.push(current);
    }
    patterns
}

fn is_selectable_alias(alias: &str) -> bool {
    !alias.is_empty()
        && !alias.starts_with('!')
        && !alias.starts_with('-')
        && !alias.contains('*')
        && !alias.contains('?')
        && !alias.chars().any(char::is_control)
}

fn validate_alias(alias: &str) -> VibexResult<()> {
    if alias.is_empty()
        || alias.len() > MAX_ALIAS_LEN
        || alias.starts_with('-')
        || alias
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || alias.contains('*')
        || alias.contains('?')
    {
        return Err(VibexError::validation(
            "ssh_alias_invalid",
            "the SSH alias is invalid",
        ));
    }
    Ok(())
}

fn expand_tilde(value: &str) -> PathBuf {
    if value == "~"
        && let Some(home) = home_dir()
    {
        return home;
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(value)
}

fn expand_tilde_path(path: &Path) -> PathBuf {
    expand_tilde(&path.to_string_lossy())
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = env::var_os("HOME");
    home.map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        value.to_string()
    }
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    fn aliases(discovery: &SshDiscovery) -> Vec<&str> {
        discovery
            .hosts
            .iter()
            .map(|host| host.alias.as_str())
            .collect()
    }

    #[test]
    fn discovers_concrete_aliases_and_skips_wildcards() {
        let dir = tempfile::tempdir().unwrap();
        let config = write(
            dir.path(),
            "config",
            "# comment\n\
             Host alpha beta\n\
               HostName alpha.example.com\n\
             Host *\n\
               User root\n\
             Host gamma\n\
               HostName gamma.example.com\n",
        );

        let discovery = discover_hosts(&config);
        assert_eq!(aliases(&discovery), vec!["alpha", "beta", "gamma"]);
        assert!(discovery.warnings.is_empty());
    }

    #[test]
    fn expands_relative_includes_with_globs() {
        let dir = tempfile::tempdir().unwrap();
        let config = write(dir.path(), "config", "Include config.d/*\nHost local\n");
        write(dir.path(), "config.d/one", "Host delta\n");
        write(dir.path(), "config.d/two", "Host epsilon\n");

        let discovery = discover_hosts(&config);
        assert_eq!(aliases(&discovery), vec!["delta", "epsilon", "local"]);
        assert!(discovery.warnings.is_empty());
    }

    #[test]
    fn include_cycles_terminate() {
        let dir = tempfile::tempdir().unwrap();
        let config = write(dir.path(), "config", "Include config\nHost alpha\n");

        let discovery = discover_hosts(&config);
        assert_eq!(aliases(&discovery), vec!["alpha"]);
    }

    #[test]
    fn match_blocks_do_not_contribute_hosts_or_conditional_includes() {
        let dir = tempfile::tempdir().unwrap();
        let config = write(
            dir.path(),
            "config",
            "Match host foo\n  Include conditional\nHost visible\n",
        );
        write(dir.path(), "conditional", "Host hidden\n");

        let discovery = discover_hosts(&config);
        assert_eq!(aliases(&discovery), vec!["visible"]);
    }

    #[test]
    fn missing_file_is_empty_and_missing_include_warns() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_hosts(&dir.path().join("absent")).hosts.is_empty());

        let config = write(dir.path(), "config", "Include absent.conf\nHost alpha\n");
        let discovery = discover_hosts(&config);
        assert_eq!(aliases(&discovery), vec!["alpha"]);
        assert!(!discovery.warnings.is_empty());
    }

    #[test]
    fn deduplicates_an_alias_seen_in_an_include() {
        let dir = tempfile::tempdir().unwrap();
        let config = write(dir.path(), "config", "Host alpha\nInclude extra\n");
        write(dir.path(), "extra", "Host alpha\n");

        let discovery = discover_hosts(&config);
        assert_eq!(aliases(&discovery), vec!["alpha"]);
    }

    #[test]
    fn warns_when_host_has_no_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let config = write(dir.path(), "config", "Host\nHost alpha\n");

        let discovery = discover_hosts(&config);
        assert_eq!(aliases(&discovery), vec!["alpha"]);
        assert!(!discovery.warnings.is_empty());
    }

    #[test]
    fn strips_inline_comments_without_treating_them_as_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let config = write(dir.path(), "config", "Host alpha # note\nHost beta\n");

        let discovery = discover_hosts(&config);
        assert_eq!(aliases(&discovery), vec!["alpha", "beta"]);
    }

    #[test]
    fn parses_effective_values_from_ssh_g() {
        let config = SshEffectiveConfig::parse(
            "hostname 10.0.0.5\n\
             user dev\n\
             port 2222\n\
             identityfile \"~/.ssh/id_ed25519\"\n\
             proxyjump none\n",
        );
        assert_eq!(config.hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(config.user.as_deref(), Some("dev"));
        assert_eq!(config.port, Some(2222));
        assert_eq!(
            config.identity_file,
            Some(PathBuf::from("~/.ssh/id_ed25519"))
        );
        assert_eq!(config.proxy_jump, None);
    }

    #[cfg(unix)]
    #[test]
    fn resolves_through_the_supplied_client() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-ssh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'hostname 10.0.0.5\\nuser dev\\nport 2222\\nproxyjump none\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let config = resolve_effective(&script, None, "build-box").unwrap();
        assert_eq!(config.hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(config.user.as_deref(), Some("dev"));
        assert_eq!(config.port, Some(2222));
    }

    #[test]
    fn rejects_option_like_aliases_before_spawning() {
        let error = resolve_effective(Path::new("ssh"), None, "-oProxyCommand=id").unwrap_err();
        assert_eq!(error.code, "ssh_alias_invalid");
    }
}
