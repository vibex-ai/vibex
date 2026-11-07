//! Agent daemons that leave the runtime's process tree.
//!
//! Every managed ACP Agent is spawned as the leader of its own process group,
//! and stopping its runtime kills that group. Some Agent CLIs nevertheless
//! start a background service of their own — `opencode serve --service`,
//! `cline --cline-hub-daemon` — and detach it with `setsid`, which takes it out
//! of the group and, once its parent exits, out of the runtime's process tree.
//! The idle sweep never sees such a process, it survives runtime restarts, and
//! it holds tens of megabytes for as long as the machine stays up.
//!
//! This module finds them by what they run: a process adopted by init (or by
//! the user's service manager) whose command line names a file under the
//! managed install root, `<home>/acp-agents/agents/<agent>/…`. A daemon is
//! ended only when no attached process of the same Agent remains anywhere — a
//! live Agent may be its client — and only after that has held for a grace
//! period, so an Agent that starts again shortly can still find its service.
//! When the runtime exits, the grace period is skipped: nothing it owned will
//! use the daemon again.
//!
//! The scan reads `/proc`, so it runs on Linux only. Elsewhere a sweep finds
//! nothing and the previous behaviour stands.

// The pure decision helpers are compiled on every platform and exercised by
// the tests there; only the Linux scan drives them in a production build, so on
// another platform they would read as dead code.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often the runtime looks for detached Agent daemons.
pub(crate) const AGENT_DAEMON_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// How long a detached daemon goes without an attached process of its Agent
/// before it is ended. Matches the time an idle Agent process is kept.
const AGENT_DAEMON_GRACE: Duration = Duration::from_secs(2 * 60);
/// How long an ended daemon is given to exit on `SIGTERM` before `SIGKILL`.
#[cfg(target_os = "linux")]
const AGENT_DAEMON_TERM_GRACE: Duration = Duration::from_secs(1);

/// The directory the managed ACP Agents are installed under.
pub(crate) fn managed_agents_root(home: &Path) -> PathBuf {
    home.join("acp-agents").join("agents")
}

/// One process, as the sweep needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessEntry {
    pid: u32,
    parent: u32,
    /// Kernel start time. With the pid it names one process even after the
    /// pid has been reused.
    start_ticks: u64,
    argv: Vec<String>,
}

/// A pid together with the start time it was observed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProcessKey {
    pid: u32,
    start_ticks: u64,
}

/// A detached daemon and everything it started.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedDaemon {
    agent: String,
    /// The daemon first, then the processes below it.
    members: Vec<ProcessKey>,
}

impl DetachedDaemon {
    fn key(&self) -> ProcessKey {
        self.members[0]
    }
}

/// The Agent a process belongs to, read from the managed file its command
/// line names.
fn agent_of(argv: &[String], roots: &[String]) -> Option<String> {
    argv.iter().find_map(|argument| {
        roots.iter().find_map(|root| {
            let rest = argument.strip_prefix(root.as_str())?.strip_prefix('/')?;
            let agent = rest.split('/').next()?;
            (!agent.is_empty()).then(|| agent.to_string())
        })
    })
}

/// Whether a parent is where detached processes end up: init, or the user's
/// service manager acting as the session's subreaper.
fn adopts_orphans(parent: u32, entry: Option<&ProcessEntry>) -> bool {
    parent == 1
        || entry.is_some_and(|entry| {
            entry
                .argv
                .first()
                .and_then(|program| Path::new(program).file_name())
                .is_some_and(|name| name == "systemd")
                && entry.argv.iter().any(|argument| argument == "--user")
        })
}

/// The detached daemons whose Agent has no attached process left.
///
/// A process is attached when it is not part of a detached daemon: an Agent
/// the runtime is running, or one someone runs from a shell. Either may be a
/// daemon's client, so its Agent's daemons stay.
///
/// A child of the runtime itself is never detached. That also covers a runtime
/// running as pid 1, as a container's init: it adopts every orphan itself, so
/// parentage cannot tell a detached daemon from its own Agent there, and the
/// sweep finds nothing rather than ending the runtime's own Agents.
fn unused_detached_daemons(
    processes: &[ProcessEntry],
    roots: &[String],
    runtime_pid: u32,
) -> Vec<DetachedDaemon> {
    let by_pid = processes
        .iter()
        .map(|entry| (entry.pid, entry))
        .collect::<HashMap<_, _>>();
    let mut children = HashMap::<u32, Vec<&ProcessEntry>>::new();
    for entry in processes {
        children.entry(entry.parent).or_default().push(entry);
    }
    let agents = processes
        .iter()
        .filter_map(|entry| agent_of(&entry.argv, roots).map(|agent| (entry.pid, agent)))
        .collect::<HashMap<_, _>>();

    let mut daemons = Vec::new();
    let mut detached = HashSet::new();
    for entry in processes {
        let Some(agent) = agents.get(&entry.pid) else {
            continue;
        };
        if entry.parent == runtime_pid
            || !adopts_orphans(entry.parent, by_pid.get(&entry.parent).copied())
        {
            continue;
        }
        let mut members = Vec::new();
        let mut pending = vec![entry];
        while let Some(process) = pending.pop() {
            if !detached.insert(process.pid) {
                continue;
            }
            members.push(ProcessKey {
                pid: process.pid,
                start_ticks: process.start_ticks,
            });
            if let Some(below) = children.get(&process.pid) {
                pending.extend(below.iter().copied());
            }
        }
        daemons.push(DetachedDaemon {
            agent: agent.clone(),
            members,
        });
    }
    let attached = agents
        .iter()
        .filter(|(pid, _)| !detached.contains(*pid))
        .map(|(_, agent)| agent.as_str())
        .collect::<HashSet<_>>();
    daemons.retain(|daemon| !attached.contains(daemon.agent.as_str()));
    daemons
}

/// Finds and ends the managed Agent daemons nothing uses any more.
#[derive(Debug)]
pub(crate) struct AgentDaemonReclaimer {
    /// The managed install root as configured and, when it differs, as the
    /// filesystem resolves it: a daemon that re-executes itself may name the
    /// resolved path.
    roots: Vec<String>,
    /// The runtime's own process, whose children are its attached Agents.
    runtime_pid: u32,
    grace: Duration,
    /// When each unused daemon was first seen without an attached process.
    unused_since: HashMap<ProcessKey, Instant>,
}

impl AgentDaemonReclaimer {
    pub(crate) fn new(home: &Path) -> Self {
        let root = managed_agents_root(home);
        let mut roots = vec![root.to_string_lossy().trim_end_matches('/').to_string()];
        if let Ok(resolved) = std::fs::canonicalize(&root) {
            let resolved = resolved.to_string_lossy().trim_end_matches('/').to_string();
            if !roots.contains(&resolved) {
                roots.push(resolved);
            }
        }
        roots.retain(|root| !root.is_empty());
        Self {
            roots,
            runtime_pid: std::process::id(),
            grace: AGENT_DAEMON_GRACE,
            unused_since: HashMap::new(),
        }
    }

    /// The daemons due in this pass. `immediate` skips the grace period.
    fn due(
        &mut self,
        processes: &[ProcessEntry],
        now: Instant,
        immediate: bool,
    ) -> Vec<DetachedDaemon> {
        let unused = unused_detached_daemons(processes, &self.roots, self.runtime_pid);
        let current = unused
            .iter()
            .map(DetachedDaemon::key)
            .collect::<HashSet<_>>();
        self.unused_since.retain(|key, _| current.contains(key));
        let mut due = Vec::new();
        for daemon in unused {
            let key = daemon.key();
            let since = *self.unused_since.entry(key).or_insert(now);
            if immediate || now.saturating_duration_since(since) >= self.grace {
                self.unused_since.remove(&key);
                due.push(daemon);
            }
        }
        due
    }

    /// Scans, decides and ends what is due, returning how many daemons were
    /// ended. Blocking — it reads `/proc` and waits for the processes it
    /// signals — so callers run it off the async executor.
    pub(crate) fn sweep(&mut self, immediate: bool) -> usize {
        #[cfg(target_os = "linux")]
        {
            let processes = linux::snapshot();
            let due = self.due(&processes, Instant::now(), immediate);
            for daemon in &due {
                // The command line is never logged: an Agent's arguments can
                // carry credentials.
                tracing::info!(
                    target: "vibex_desktop",
                    agent = %daemon.agent,
                    pid = daemon.key().pid,
                    processes = daemon.members.len(),
                    "ending a detached Agent daemon that no Agent uses"
                );
            }
            // Together, so one grace period covers every daemon: the last pass
            // runs inside the runtime's shutdown budget.
            let members = due
                .iter()
                .flat_map(|daemon| daemon.members.iter().copied())
                .collect::<Vec<_>>();
            linux::terminate(&members, AGENT_DAEMON_TERM_GRACE);
            due.len()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = immediate;
            0
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, Instant};

    use super::{ProcessEntry, ProcessKey};

    /// The current user's processes. Others' cannot be signalled, and a
    /// kernel thread has no command line to match.
    pub(super) fn snapshot() -> Vec<ProcessEntry> {
        // SAFETY: `getuid` has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
                if entry.metadata().ok()?.uid() != uid {
                    return None;
                }
                let (parent, start_ticks, zombie) = stat(pid)?;
                if zombie {
                    return None;
                }
                let argv = std::fs::read(format!("/proc/{pid}/cmdline"))
                    .ok()?
                    .split(|byte| *byte == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect::<Vec<_>>();
                (!argv.is_empty()).then_some(ProcessEntry {
                    pid,
                    parent,
                    start_ticks,
                    argv,
                })
            })
            .collect()
    }

    /// Parent pid, start time and zombie state from `/proc/<pid>/stat`.
    fn stat(pid: u32) -> Option<(u32, u64, bool)> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name is parenthesised and may itself hold spaces or
        // parentheses; the fields after the last `)` are positional.
        let (_, rest) = text.rsplit_once(')')?;
        let fields = rest.split_whitespace().collect::<Vec<_>>();
        let zombie = *fields.first()? == "Z";
        let parent = fields.get(1)?.parse().ok()?;
        let start_ticks = fields.get(19)?.parse().ok()?;
        Some((parent, start_ticks, zombie))
    }

    /// Whether the pid still names the process it was observed as.
    fn is_running(key: ProcessKey) -> bool {
        stat(key.pid)
            .is_some_and(|(_, start_ticks, zombie)| start_ticks == key.start_ticks && !zombie)
    }

    fn signal(key: ProcessKey, signal: libc::c_int) {
        if !is_running(key) {
            return;
        }
        let Ok(pid) = libc::pid_t::try_from(key.pid) else {
            return;
        };
        // SAFETY: `kill` only sends a signal, and the pid was just checked
        // against the start time it was observed with, so it still names the
        // process the sweep decided to end.
        unsafe {
            libc::kill(pid, signal);
        }
    }

    /// Asks every member to exit, then kills whatever is still running once
    /// the grace period has passed.
    pub(super) fn terminate(members: &[ProcessKey], grace: Duration) {
        for member in members {
            signal(*member, libc::SIGTERM);
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && members.iter().any(|member| is_running(*member)) {
            std::thread::sleep(Duration::from_millis(50));
        }
        for member in members {
            signal(*member, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/home/u/.vibex/acp-agents/agents";
    /// The runtime's pid in the fixtures below.
    const RUNTIME: u32 = 100;

    fn process(pid: u32, parent: u32, argv: &[&str]) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent,
            start_ticks: u64::from(pid) * 10,
            argv: argv.iter().map(|argument| argument.to_string()).collect(),
        }
    }

    fn roots() -> Vec<String> {
        vec![ROOT.to_string()]
    }

    fn agents(daemons: &[DetachedDaemon]) -> Vec<(&str, u32)> {
        daemons
            .iter()
            .map(|daemon| (daemon.agent.as_str(), daemon.key().pid))
            .collect()
    }

    #[test]
    fn the_agent_is_the_directory_under_the_managed_root() {
        let argv = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            agent_of(
                &argv(&[
                    &format!("{ROOT}/opencode/2.2.5-bc7d8e35b059/opencode"),
                    "serve"
                ]),
                &roots()
            )
            .as_deref(),
            Some("opencode")
        );
        // A Node Agent names its script, not its interpreter.
        assert_eq!(
            agent_of(
                &argv(&[
                    "/home/u/.vibex/acp-agents/runtimes/node/bin/node",
                    &format!("{ROOT}/claude/0.101.0-b6/node_modules/x/index.js"),
                ]),
                &roots()
            )
            .as_deref(),
            Some("claude")
        );
        // A sibling directory that only shares the prefix is not managed.
        assert_eq!(
            agent_of(&argv(&[&format!("{ROOT}-old/opencode/opencode")]), &roots()),
            None
        );
        assert_eq!(agent_of(&argv(&["opencode", "serve"]), &roots()), None);
    }

    #[test]
    fn a_detached_daemon_with_no_attached_agent_is_unused() {
        let processes = vec![
            process(100, 1, &["/usr/bin/vibex-desktop"]),
            // The daemon and a worker it started.
            process(
                200,
                1,
                &[&format!("{ROOT}/opencode/2.2.5/opencode"), "serve"],
            ),
            process(201, 200, &["/usr/bin/node", "worker.js"]),
        ];
        let unused = unused_detached_daemons(&processes, &roots(), RUNTIME);
        assert_eq!(agents(&unused), vec![("opencode", 200)]);
        assert_eq!(
            unused[0]
                .members
                .iter()
                .map(|member| member.pid)
                .collect::<Vec<_>>(),
            vec![200, 201],
            "the daemon's own workers go with it"
        );
    }

    #[test]
    fn a_daemon_stays_while_an_attached_process_of_its_agent_runs() {
        let processes = vec![
            process(100, 1, &["/usr/bin/vibex-desktop"]),
            // A terminal-mode cline session the runtime is running…
            process(
                110,
                100,
                &[&format!("{ROOT}/cline/3.0.74/bin/cline"), "--tui"],
            ),
            // …may be the hub daemon's client.
            process(
                300,
                1,
                &[
                    &format!("{ROOT}/cline/3.0.74/bin/cline"),
                    "--cline-hub-daemon",
                ],
            ),
            process(
                400,
                1,
                &[&format!("{ROOT}/opencode/2.2.5/opencode"), "serve"],
            ),
        ];
        assert_eq!(
            agents(&unused_detached_daemons(&processes, &roots(), RUNTIME)),
            vec![("opencode", 400)]
        );
    }

    #[test]
    fn only_processes_adopted_by_init_or_the_user_manager_are_detached() {
        let processes = vec![
            process(50, 1, &["/usr/lib/systemd/systemd", "--user"]),
            process(60, 1, &["/usr/bin/bash"]),
            // Adopted by the user's service manager: detached.
            process(
                200,
                50,
                &[&format!("{ROOT}/opencode/2.2.5/opencode"), "serve"],
            ),
            // Run from someone's shell: attached, and it protects its Agent.
            process(210, 60, &[&format!("{ROOT}/kimi/2.4.1/kimi"), "acp"]),
            process(220, 1, &[&format!("{ROOT}/kimi/2.4.1/kimi"), "daemon"]),
        ];
        assert_eq!(
            agents(&unused_detached_daemons(&processes, &roots(), RUNTIME)),
            vec![("opencode", 200)]
        );
    }

    #[test]
    fn a_runtime_running_as_init_never_mistakes_its_own_agents_for_daemons() {
        // In a container the runtime may be pid 1, and then every Agent it
        // spawns has pid 1 for a parent, exactly like an adopted daemon.
        let processes = vec![
            process(1, 0, &["/usr/bin/vibex-server"]),
            process(20, 1, &[&format!("{ROOT}/opencode/2.2.5/opencode"), "acp"]),
            process(
                30,
                1,
                &[&format!("{ROOT}/opencode/2.2.5/opencode"), "serve"],
            ),
        ];
        assert!(unused_detached_daemons(&processes, &roots(), 1).is_empty());
    }

    #[test]
    fn unmanaged_detached_processes_are_never_touched() {
        let processes = vec![
            process(200, 1, &["/usr/bin/npm", "run", "dev"]),
            process(210, 1, &["/home/u/.local/bin/opencode", "serve"]),
        ];
        assert!(unused_detached_daemons(&processes, &roots(), RUNTIME).is_empty());
    }

    #[test]
    fn a_daemon_is_due_only_after_the_grace_period_unless_the_runtime_is_exiting() {
        let mut reclaimer = AgentDaemonReclaimer {
            roots: roots(),
            runtime_pid: RUNTIME,
            grace: Duration::from_secs(120),
            unused_since: HashMap::new(),
        };
        let daemon = process(
            200,
            1,
            &[&format!("{ROOT}/opencode/2.2.5/opencode"), "serve"],
        );
        let start = Instant::now();
        assert!(
            reclaimer
                .due(std::slice::from_ref(&daemon), start, false)
                .is_empty()
        );
        assert!(
            reclaimer
                .due(
                    std::slice::from_ref(&daemon),
                    start + Duration::from_secs(60),
                    false
                )
                .is_empty()
        );

        // An Agent that comes back in time restarts the clock.
        let client = process(
            300,
            100,
            &[&format!("{ROOT}/opencode/2.2.5/opencode"), "acp"],
        );
        assert!(
            reclaimer
                .due(
                    &[daemon.clone(), client],
                    start + Duration::from_secs(90),
                    false
                )
                .is_empty()
        );
        assert!(
            reclaimer
                .due(
                    std::slice::from_ref(&daemon),
                    start + Duration::from_secs(150),
                    false
                )
                .is_empty(),
            "the grace period counts from when the daemon was last unused"
        );
        assert_eq!(
            agents(&reclaimer.due(
                std::slice::from_ref(&daemon),
                start + Duration::from_secs(271),
                false
            )),
            vec![("opencode", 200)]
        );

        // Exiting skips the wait.
        let mut exiting = AgentDaemonReclaimer {
            roots: roots(),
            runtime_pid: RUNTIME,
            grace: Duration::from_secs(120),
            unused_since: HashMap::new(),
        };
        assert_eq!(
            agents(&exiting.due(&[daemon], start, true)),
            vec![("opencode", 200)]
        );
    }

    #[test]
    fn a_reused_pid_starts_a_new_grace_period() {
        let mut reclaimer = AgentDaemonReclaimer {
            roots: roots(),
            runtime_pid: RUNTIME,
            grace: Duration::from_secs(120),
            unused_since: HashMap::new(),
        };
        let start = Instant::now();
        let first = process(
            200,
            1,
            &[&format!("{ROOT}/opencode/2.2.5/opencode"), "serve"],
        );
        assert!(
            reclaimer
                .due(std::slice::from_ref(&first), start, false)
                .is_empty()
        );
        let reused = ProcessEntry {
            start_ticks: first.start_ticks + 1,
            ..first
        };
        assert!(
            reclaimer
                .due(&[reused], start + Duration::from_secs(130), false)
                .is_empty(),
            "a different process under the same pid is not the one first seen"
        );
    }

    /// The real thing: a detached process under a managed root is ended once
    /// due, and a process outside it is left alone.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_due_daemon_is_ended_and_an_unmanaged_one_is_left() {
        use std::os::unix::process::CommandExt;

        let home = tempfile::tempdir().unwrap();
        let agent_dir = managed_agents_root(home.path())
            .join("fake-agent")
            .join("1.0.0");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let script = agent_dir.join("daemon.sh");
        // No `exec`: the daemon has to keep naming its managed file, the way a
        // real one keeps naming its binary, and its `sleep` is a worker of its
        // own that has to go with it.
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        // Detach the way the real daemons do: a new session whose parent exits
        // at once, so init adopts it. `sh -c` stands in for that parent, and
        // leads its own group so `setsid` does not need to fork.
        let spawn_detached = |program: &str, argument: &str| -> u32 {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "setsid {program} {argument} >/dev/null 2>&1 & echo $!"
                ))
                .process_group(0)
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse()
                .unwrap()
        };
        let managed = spawn_detached("sh", &script.to_string_lossy());
        let unmanaged = spawn_detached("sleep", "30");
        let running = |pid: u32| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(')')
                        .map(|(_, rest)| !rest.trim_start().starts_with('Z'))
                })
                .unwrap_or(false)
        };
        // The parent exits a moment after printing the pid; retry until the
        // daemon has been adopted.
        let mut reclaimer = AgentDaemonReclaimer::new(home.path());
        let mut ended = 0;
        for _ in 0..50 {
            ended = reclaimer.sweep(true);
            if ended > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(ended, 1, "the managed daemon was found and ended");
        let mut managed_gone = false;
        for _ in 0..50 {
            if !running(managed) {
                managed_gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(managed_gone);
        assert!(running(unmanaged), "an unmanaged process is never touched");
        // SAFETY: the test owns this process and only signals it.
        unsafe {
            libc::kill(unmanaged as libc::pid_t, libc::SIGKILL);
        }
    }
}
