//! Live file watching for the files the editor has open.
//!
//! The preview panel reads a file once and paints what it read. Without a
//! watch, an Agent or another editor that rewrites the file leaves the open
//! tab showing stale content that still claims to be saved, and the reader
//! only learns about it when the next save fails its revision check.
//!
//! This module owns the filesystem side of the fix: one `notify` watcher per
//! workbench, a [`WatchTargets`] snapshot of the paths that should be watched,
//! and a debounced event stream. It deliberately knows nothing about GPUI,
//! buffers, or the editor's conflict rules — the workbench turns a
//! [`WatchEvent`] into a buffer update.
//!
//! The watcher registers each target's **parent directory**, not the file.
//! Every editor that saves by writing a temporary file and renaming it over the
//! original ends the life of the watched inode, and a watch on the file itself
//! goes silent with it; a directory watch survives that and still reports the
//! new file. Events are filtered back down to the declared targets, so watching
//! a directory costs nothing beyond the filtering.
//!
//! Paths are normalized when the target set is built, because `notify` reports
//! the path it was handed and therefore a workspace-relative editor path could
//! never match. The watcher keeps both the normalized path and the editor key,
//! so an event routes back without guessing how the two spellings differ.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _,
    event::{AccessKind, AccessMode},
};
use tokio::sync::mpsc;

/// How long a burst of events for one path is allowed to settle before it is
/// reported.
///
/// A single save can arrive as several raw events — truncate, write, close,
/// attribute — and some editors replace the inode instead of writing through
/// it. Debouncing keeps the workbench from reading the same file three times
/// in a row and from acting on a half-written file.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(120);

/// One path whose content changed on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    /// The path the watcher saw, normalized the same way as [`WatchTargets`].
    pub path: PathBuf,
    pub kind: WatchEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEventKind {
    /// The file has new content (or was just created).
    Changed,
    /// The file is gone. A save may have replaced it with a new inode, so the
    /// workbench re-reads before treating this as a deletion.
    Removed,
}

/// The set of files the workbench wants watched, keyed by editor path.
///
/// The workbench hands one of these over whenever its open-file set changes;
/// the watcher reconciles its directory registrations against it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchTargets {
    entries: BTreeMap<String, PathBuf>,
}

impl WatchTargets {
    pub fn new(entries: impl IntoIterator<Item = (String, PathBuf)>) -> Self {
        let entries = entries
            .into_iter()
            .map(|(key, path)| (key, normalize(&path)))
            .collect();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The parent directories that have to be registered to see these files.
    fn directories(&self) -> BTreeSet<PathBuf> {
        self.entries
            .values()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect()
    }

    /// The editor key for one reported path, if the path is still declared.
    fn key_for(&self, path: &Path) -> Option<&String> {
        self.entries
            .iter()
            .find(|(_, candidate)| candidate.as_path() == path)
            .map(|(key, _)| key)
    }
}

/// Canonicalizes the longest existing prefix and keeps the rest verbatim, so a
/// path that has just been deleted still compares equal to the one that was
/// declared while the file existed.
fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(parent) = parent.canonicalize()
    {
        return parent.join(name);
    }
    path.to_path_buf()
}

/// Resolves one settled batch against `targets`.
///
/// The result is keyed by editor path and keeps the strongest kind per key, so
/// a caller never matches on spelling and never reads a file twice for one
/// burst.
pub fn route(targets: &WatchTargets, batch: Vec<WatchEvent>) -> BTreeMap<String, WatchEventKind> {
    let mut routed = BTreeMap::new();
    for event in batch {
        let path = normalize(&event.path);
        if let Some(key) = targets.key_for(&path) {
            routed.insert(key.clone(), event.kind);
        }
    }
    routed
}

/// A running watcher and the receiver of its debounced events.
///
/// The stream is handed to the workbench once; dropping the handle closes it,
/// which is how the consuming task learns to stop.
pub struct FileWatch {
    /// Dropping the watcher unregisters every directory and ends its thread.
    watcher: RecommendedWatcher,
    stream: Option<mpsc::UnboundedReceiver<Vec<WatchEvent>>>,
    targets: WatchTargets,
    directories: BTreeSet<PathBuf>,
}

impl FileWatch {
    /// Starts a watcher over `targets`.
    ///
    /// Returns `None` when the platform has no usable watcher (an exotic
    /// filesystem, a container without inotify). The caller then keeps the
    /// read-on-open behaviour instead of pretending it will notice changes.
    pub fn start(targets: WatchTargets) -> Option<Self> {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
        let watcher = RecommendedWatcher::new(
            move |event| {
                // A closed receiver means the workbench is gone; the send error
                // is not interesting and must not panic the platform thread.
                let _ = raw_tx.send(event);
            },
            Config::default(),
        )
        .ok()?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let mut watch = Self {
            watcher,
            stream: Some(event_rx),
            targets: WatchTargets::default(),
            directories: BTreeSet::new(),
        };
        watch.reconcile(targets);
        spawn_debouncer(raw_rx, event_tx);
        Some(watch)
    }

    /// The one receiver of this watcher's settled batches.
    pub fn take_stream(&mut self) -> Option<mpsc::UnboundedReceiver<Vec<WatchEvent>>> {
        self.stream.take()
    }

    /// The targets the watcher is currently registered against.
    pub fn targets(&self) -> &WatchTargets {
        &self.targets
    }

    /// Replaces the watched set with `targets`.
    ///
    /// Opening a file adds a directory at most; closing the last file in a
    /// directory removes one. The reconciler only touches directories whose
    /// membership actually changed.
    pub fn reconcile(&mut self, targets: WatchTargets) {
        if self.targets == targets {
            return;
        }
        let next = targets.directories();
        for directory in self.directories.difference(&next) {
            // A directory that vanished before we got here is already unwatched
            // as far as the platform is concerned; the error is not news.
            let _ = self.watcher.unwatch(directory);
        }
        for directory in next.difference(&self.directories) {
            if self
                .watcher
                .watch(directory, RecursiveMode::NonRecursive)
                .is_err()
            {
                // Inotify runs out of watches on a busy machine, and a network
                // mount may not report changes at all. Losing one directory
                // degrades those tabs to read-on-open; it must not take the
                // rest of the set down.
                tracing::debug!(
                    target: "vibex_desktop",
                    path = %directory.display(),
                    "File watch registration failed"
                );
            }
        }
        self.directories = next;
        self.targets = targets;
    }
}

/// Collapses a burst of raw events into one settled batch.
///
/// Events for the same path in one burst merge into the strongest kind: a
/// removed-then-recreated file is `Changed`, because the interesting fact is
/// that its content is now different, and re-reading settles the rest.
///
/// The thread is deliberately not a Tokio task: the watcher runs outside the
/// GPUI executor and the platform event thread has no reactor, so a plain
/// blocking thread with a receive timeout is the version that cannot panic for
/// want of one.
fn spawn_debouncer(
    raw: Receiver<notify::Result<Event>>,
    events: mpsc::UnboundedSender<Vec<WatchEvent>>,
) {
    std::thread::Builder::new()
        .name("vibex-file-watch".to_string())
        .spawn(move || {
            loop {
                let first = match raw.recv() {
                    Ok(event) => event,
                    Err(_) => return,
                };
                let mut batch = BTreeMap::<PathBuf, WatchEventKind>::new();
                absorb(first, &mut batch);
                // Every further event that arrives inside the window belongs
                // to this settlement. The timeout is what ends it.
                loop {
                    match raw.recv_timeout(WATCH_DEBOUNCE) {
                        Ok(event) => absorb(event, &mut batch),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => {
                            let _ = send_settled(&events, batch);
                            return;
                        }
                    }
                }
                if send_settled(&events, batch).is_err() {
                    return;
                }
            }
        })
        .ok();
}

fn send_settled(
    events: &mpsc::UnboundedSender<Vec<WatchEvent>>,
    batch: BTreeMap<PathBuf, WatchEventKind>,
) -> Result<(), ()> {
    let settled = batch
        .into_iter()
        .map(|(path, kind)| WatchEvent { path, kind })
        .collect::<Vec<_>>();
    events.send(settled).map_err(|_| ())
}

fn absorb(event: notify::Result<Event>, batch: &mut BTreeMap<PathBuf, WatchEventKind>) {
    let Ok(event) = event else {
        return;
    };
    let kind = match event.kind {
        // Opening and closing a file for reading is not a content change, and
        // on Linux every read the editor does would otherwise re-trigger it.
        EventKind::Access(AccessKind::Open(_) | AccessKind::Close(AccessMode::Read)) => return,
        EventKind::Remove(_) => WatchEventKind::Removed,
        EventKind::Access(_)
        | EventKind::Create(_)
        | EventKind::Modify(_)
        | EventKind::Any
        | EventKind::Other => WatchEventKind::Changed,
    };
    for path in event.paths {
        if path.as_os_str().is_empty() {
            continue;
        }
        let entry = batch.entry(path).or_insert(kind);
        // A removal that follows a change in the same burst describes the same
        // save (the editor deleted the old inode, then renamed the new one into
        // place), so `Changed` wins over `Removed`.
        if *entry == WatchEventKind::Removed && kind == WatchEventKind::Changed {
            *entry = WatchEventKind::Changed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};

    fn targets(entries: &[(&str, &str)]) -> WatchTargets {
        WatchTargets::new(
            entries
                .iter()
                .map(|(key, path)| (key.to_string(), PathBuf::from(path))),
        )
    }

    fn raw(kind: EventKind, paths: &[&str]) -> notify::Result<Event> {
        Ok(paths.iter().fold(Event::new(kind), |event, path| {
            event.add_path(PathBuf::from(path))
        }))
    }

    #[test]
    fn one_directory_serves_every_open_file_in_it() {
        let targets = targets(&[
            ("src/lib.rs", "/tmp/project/src/lib.rs"),
            ("src/app.rs", "/tmp/project/src/app.rs"),
            ("README.md", "/tmp/project/README.md"),
        ]);
        assert_eq!(targets.directories().len(), 2);
    }

    #[test]
    fn one_reported_change_reaches_one_batch_entry() {
        let mut batch = BTreeMap::new();
        absorb(
            raw(
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                &["/a/lib.rs"],
            ),
            &mut batch,
        );
        absorb(
            raw(
                EventKind::Access(AccessKind::Close(AccessMode::Write)),
                &["/a/lib.rs"],
            ),
            &mut batch,
        );
        assert_eq!(
            batch.get(Path::new("/a/lib.rs")),
            Some(&WatchEventKind::Changed),
            "a save that arrives as several raw events is read once"
        );
    }

    #[test]
    fn a_removal_then_recreation_is_a_change() {
        let mut batch = BTreeMap::new();
        absorb(
            raw(EventKind::Remove(RemoveKind::File), &["/a/lib.rs"]),
            &mut batch,
        );
        absorb(
            raw(EventKind::Create(CreateKind::File), &["/a/lib.rs"]),
            &mut batch,
        );
        assert_eq!(
            batch.get(Path::new("/a/lib.rs")),
            Some(&WatchEventKind::Changed)
        );
    }

    #[test]
    fn read_access_events_never_become_a_change() {
        let mut batch = BTreeMap::new();
        absorb(
            raw(
                EventKind::Access(AccessKind::Open(AccessMode::Read)),
                &["/a/lib.rs"],
            ),
            &mut batch,
        );
        absorb(
            raw(
                EventKind::Access(AccessKind::Close(AccessMode::Read)),
                &["/a/lib.rs"],
            ),
            &mut batch,
        );
        assert!(batch.is_empty());
    }

    #[test]
    fn a_write_close_is_a_change() {
        let mut batch = BTreeMap::new();
        absorb(
            raw(
                EventKind::Access(AccessKind::Close(AccessMode::Write)),
                &["/a/lib.rs"],
            ),
            &mut batch,
        );
        assert_eq!(
            batch.get(Path::new("/a/lib.rs")),
            Some(&WatchEventKind::Changed)
        );
    }

    #[test]
    fn a_rename_reports_both_spellings() {
        let mut batch = BTreeMap::new();
        absorb(
            raw(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &["/a/lib.rs", "/a/lib.rs.bak"],
            ),
            &mut batch,
        );
        assert_eq!(batch.len(), 2);
        assert!(batch.values().all(|kind| *kind == WatchEventKind::Changed));
    }

    #[test]
    fn routing_resolves_a_reported_path_back_to_its_editor_key() {
        let targets = targets(&[("src/lib.rs", "/tmp/project/src/lib.rs")]);
        let routed = route(
            &targets,
            vec![WatchEvent {
                path: PathBuf::from("/tmp/project/src/lib.rs"),
                kind: WatchEventKind::Changed,
            }],
        );
        assert_eq!(routed.get("src/lib.rs"), Some(&WatchEventKind::Changed));
    }

    #[test]
    fn routing_ignores_a_path_that_is_no_longer_open() {
        let targets = targets(&[("src/lib.rs", "/tmp/project/src/lib.rs")]);
        let routed = route(
            &targets,
            vec![WatchEvent {
                path: PathBuf::from("/tmp/project/src/other.rs"),
                kind: WatchEventKind::Changed,
            }],
        );
        assert!(routed.is_empty());
    }

    #[test]
    fn a_removed_file_still_routes_through_its_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("lib.rs");
        std::fs::write(&path, "one").expect("write file");
        let targets = WatchTargets::new(vec![("lib.rs".to_string(), path.clone())]);
        std::fs::remove_file(&path).expect("remove file");
        let routed = route(
            &targets,
            vec![WatchEvent {
                path: path.clone(),
                kind: WatchEventKind::Removed,
            }],
        );
        assert_eq!(
            routed.get("lib.rs"),
            Some(&WatchEventKind::Removed),
            "a deleted file still compares equal to the path that was declared"
        );
    }
}
