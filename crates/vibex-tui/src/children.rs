//! The ownership tree behind the session list and a session's own page.
//!
//! A session created by a delegated task is deliberately absent from the
//! runtime's root session list, so a client that draws only that list cannot
//! show the work a session delegated. The runtime publishes the ownership
//! edges as a tree instead, and this is the TUI's copy of it: one page of
//! direct children per parent, the metadata that names them, and the path from
//! the ownership root down to the session the reader has open.
//!
//! The state is a cache, never an authority. Every answer replaces the page it
//! was asked for, a session that disappears from the root list takes its branch
//! with it, and a runtime that cannot answer at all leaves the list exactly as
//! it was before this module existed.

use std::collections::{BTreeMap, BTreeSet};

use vibex_core::{AgentSession, SessionTreeNode, SessionTreePage, VibexSessionId};

/// How many children one page asks for.
///
/// One screenful of a branch is worth reading at once, and the runtime's own
/// team page uses the same bound; a parent with more children than this says so
/// and loads the rest on request.
pub const CHILD_PAGE_LIMIT: usize = 100;

/// How long a branch's answer stays fresh.
///
/// The list is re-read whenever a session changes, and a branch's own states
/// come from the same events; asking the runtime again for every one of them
/// would turn a busy turn into a stream of ownership reads. Two seconds is
/// short enough that a worker's state cannot lag noticeably behind its parent's
/// row and long enough that a turn's worth of updates costs one read.
pub const CHILD_REFRESH_MS: i64 = 2_000;

/// One parent's direct children, as the runtime last answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildPage {
    pub nodes: Vec<SessionTreeNode>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    /// When the answer landed, so a refresh can tell a fresh page from a stale
    /// one without a timer of its own.
    pub loaded_at_ms: i64,
}

impl ChildPage {
    fn of(page: SessionTreePage, now_ms: i64) -> Self {
        Self {
            nodes: page.nodes,
            next_cursor: page.next_cursor,
            has_more: page.has_more,
            loaded_at_ms: now_ms,
        }
    }

    /// One more page of the same parent, appended behind what is drawn.
    ///
    /// The runtime pages by session id, so an answer can only extend the list;
    /// an id already held is dropped rather than drawn twice.
    fn extend(&mut self, page: SessionTreePage, now_ms: i64) {
        for node in page.nodes {
            if !self
                .nodes
                .iter()
                .any(|held| held.session_ref.id == node.session_ref.id)
            {
                self.nodes.push(node);
            }
        }
        self.next_cursor = page.next_cursor;
        self.has_more = page.has_more;
        self.loaded_at_ms = now_ms;
    }
}

/// The ownership tree as this client holds it.
#[derive(Debug, Clone, Default)]
pub struct ChildSessions {
    /// Ownership roots, keyed by nothing: there is one page of them.
    roots: Option<ChildPage>,
    /// Direct children by parent session id.
    pages: BTreeMap<String, ChildPage>,
    /// Metadata for every node and ancestor this client has been told about,
    /// including the children the root list omits.
    registry: BTreeMap<String, AgentSession>,
    /// The ownership path from the root down to the parent of `open`, oldest
    /// first: what "back to the main session" walks.
    ancestors: Vec<SessionTreeNode>,
    /// The session the ancestors belong to.
    open: Option<String>,
    /// Branches the reader folded shut, by the session that owns them.
    collapsed: BTreeSet<String>,
    /// Parents whose page is on its way, keyed by session id; the empty string
    /// is the ownership roots.
    loading: BTreeSet<String>,
    /// Parents whose last answer failed, so a refresh retries them and the
    /// renderer does not pretend a branch is empty.
    failed: BTreeSet<String>,
    /// Whether the runtime cannot answer at all. A client that learns this
    /// stops asking; the list keeps drawing without children.
    unsupported: bool,
}

/// The parent key of a page: the ownership roots use the empty string.
const ROOTS: &str = "";

fn key(parent: Option<&VibexSessionId>) -> String {
    parent
        .map(|id| id.as_str().to_string())
        .unwrap_or_else(|| ROOTS.to_string())
}

/// The same key from a session id a caller already holds as text.
fn str_key(parent: Option<&str>) -> String {
    parent.unwrap_or(ROOTS).to_string()
}

impl ChildSessions {
    /// A tree that starts with the branches the reader folded in an earlier
    /// run, before anything has been read.
    pub fn with_collapsed(collapsed: BTreeSet<String>) -> Self {
        Self {
            collapsed,
            ..Self::default()
        }
    }

    /// The branches the reader folded shut.
    pub fn collapsed_ids(&self) -> BTreeSet<String> {
        self.collapsed.clone()
    }

    /// Whether the runtime cannot answer ownership questions.
    pub fn is_unsupported(&self) -> bool {
        self.unsupported
    }

    /// Record that the runtime cannot answer, so the client stops asking.
    pub fn mark_unsupported(&mut self) {
        self.unsupported = true;
        self.loading.clear();
    }

    /// Whether any page (roots or a branch) has been read.
    pub fn is_loaded(&self) -> bool {
        self.roots.is_some() || !self.pages.is_empty()
    }

    /// Whether the reader has folded this session's branch shut.
    pub fn is_collapsed(&self, session_id: &str) -> bool {
        self.collapsed.contains(session_id)
    }

    /// Fold the branch open or shut, answering the state it now has.
    pub fn toggle_collapsed(&mut self, session_id: &str) -> bool {
        if self.collapsed.remove(session_id) {
            false
        } else {
            self.collapsed.insert(session_id.to_string());
            true
        }
    }

    /// Open a branch, answering whether it was folded shut.
    ///
    /// The reader's cursor is what opens a branch now that a row carries no
    /// disclosure of its own, and walking onto an already open row has to leave
    /// it alone: a selection that folded what it landed on would take the row
    /// the reader is heading for off the list.
    pub fn open(&mut self, session_id: &str) -> bool {
        self.collapsed.remove(session_id)
    }

    /// Keep only the branches whose owner is still a session this client knows
    /// about, so a deleted session cannot leave a heading behind.
    pub fn retain(&mut self, known: &BTreeSet<String>) {
        self.collapsed.retain(|id| known.contains(id));
        self.pages.retain(|parent, _| known.contains(parent));
        for page in self.pages.values_mut() {
            page.nodes
                .retain(|node| known.contains(&node.session_ref.id));
        }
        self.registry.retain(|id, _| known.contains(id));
        self.ancestors
            .retain(|node| known.contains(&node.session_ref.id));
    }

    /// The ownership roots the runtime published.
    pub fn roots(&self) -> &[SessionTreeNode] {
        self.roots
            .as_ref()
            .map(|page| page.nodes.as_slice())
            .unwrap_or_default()
    }

    /// A parent's direct children, in the order the runtime lists them.
    pub fn children(&self, parent: &str) -> &[SessionTreeNode] {
        self.pages
            .get(parent)
            .map(|page| page.nodes.as_slice())
            .unwrap_or_default()
    }

    /// A parent's direct children as ids, which is what a row needs.
    pub fn child_ids(&self, parent: &str) -> Vec<VibexSessionId> {
        self.children(parent)
            .iter()
            .filter_map(|node| node.session_ref.session_id())
            .collect()
    }

    /// Whether a branch has anything under it, as far as this client knows.
    pub fn has_children(&self, parent: &str) -> bool {
        self.child_count(parent) > 0
    }

    /// How many children a loaded page holds.
    pub fn child_count(&self, parent: &str) -> usize {
        self.pages
            .get(parent)
            .map(|page| page.nodes.len())
            .unwrap_or(0)
    }

    /// How many direct children a session owns.
    ///
    /// An unread branch answers what the runtime said when it last named the
    /// session, so a disclosure can be drawn before the branch is read; a read
    /// branch answers what it actually holds.
    pub fn declared_child_count(&self, session_id: &str) -> usize {
        let loaded = self.child_count(session_id);
        if loaded > 0 {
            return loaded;
        }
        self.node(session_id)
            .map(|node| node.child_count.max(usize::from(node.has_more_children)))
            .unwrap_or(0)
    }

    /// The node the tree holds for one session, wherever it sits.
    pub fn node(&self, session_id: &str) -> Option<&SessionTreeNode> {
        self.roots
            .iter()
            .chain(self.pages.values())
            .flat_map(|page| page.nodes.iter())
            .find(|node| node.session_ref.id == session_id)
    }

    /// Whether a session's children are still being read.
    pub fn is_loading(&self, parent: Option<&str>) -> bool {
        self.loading.contains(&str_key(parent))
    }

    /// Whether a parent has more children than the page this client holds.
    pub fn has_more_children(&self, parent: &str) -> bool {
        self.pages.get(parent).is_some_and(|page| page.has_more)
    }

    /// The cursor a parent's next page continues from.
    pub fn next_cursor(&self, parent: &str) -> Option<String> {
        self.pages
            .get(parent)
            .and_then(|page| page.next_cursor.clone())
    }

    /// Whether a page is missing, failed, or old enough to ask again.
    pub fn needs_refresh(&self, parent: Option<&VibexSessionId>, now_ms: i64) -> bool {
        if self.loading.contains(&key(parent)) {
            return false;
        }
        let page = match parent {
            Some(parent) => self.pages.get(parent.as_str()),
            None => self.roots.as_ref(),
        };
        match page {
            Some(page) => {
                self.failed.contains(&key(parent))
                    || now_ms.saturating_sub(page.loaded_at_ms) >= CHILD_REFRESH_MS
            }
            // Nothing is held for this parent yet, which is exactly the state
            // a read answers: the roots have never been read, or a session the
            // runtime says has children has never had its branch asked for.
            None => true,
        }
    }

    /// The parents whose children the list is drawing.
    ///
    /// A branch is drawn unless the reader folded it, and a session the runtime
    /// says has children is a branch even before its page has been read — which
    /// is what makes the first read happen without a gesture. The walk is
    /// bounded so a malformed ownership graph cannot make a refresh unbounded.
    pub fn expanded_parents(&self) -> Vec<VibexSessionId> {
        const WALK_LIMIT: usize = 512;
        let mut parents: Vec<VibexSessionId> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut stack = self.roots().to_vec();
        let mut walked = 0usize;
        while let Some(node) = stack.pop() {
            walked += 1;
            if walked > WALK_LIMIT {
                break;
            }
            if !seen.insert(node.session_ref.id.clone()) {
                continue;
            }
            let Some(id) = node.session_ref.session_id() else {
                continue;
            };
            if self.is_collapsed(id.as_str()) {
                // A folded branch hides its own children and everything under
                // them: nothing behind it needs to be current.
                continue;
            }
            let owns =
                node.child_count > 0 || node.has_more_children || self.has_children(id.as_str());
            if owns && !parents.iter().any(|held| held == &id) {
                parents.push(id.clone());
            }
            if let Some(page) = self.pages.get(id.as_str()) {
                stack.extend(page.nodes.iter().cloned());
            }
        }
        parents
    }

    /// Whether the last read of a parent failed.
    pub fn has_failed(&self, parent: Option<&str>) -> bool {
        self.failed.contains(&str_key(parent))
    }

    /// The session metadata behind an id, whether it is a root or a child.
    pub fn session(&self, session_id: &str) -> Option<&AgentSession> {
        self.registry.get(session_id)
    }

    /// The title a session is drawn under, when the tree is what named it.
    pub fn session_title(&self, session_id: &str) -> Option<&str> {
        self.session(session_id)
            .map(|session| session.title.as_str())
    }

    /// The path from the ownership root down to the parent of the session the
    /// reader has open, oldest first. Empty for a root and for a session whose
    /// path was never read.
    pub fn ancestors(&self) -> &[SessionTreeNode] {
        &self.ancestors
    }

    /// The session the ancestors describe, when the reader has one open.
    pub fn open_session(&self) -> Option<&str> {
        self.open.as_deref()
    }

    /// The immediate parent of the open session, which is where the way back
    /// leads. `None` for an ownership root and for a session whose page has not
    /// been read yet.
    pub fn parent_of_open(&self) -> Option<&SessionTreeNode> {
        self.ancestors.last()
    }

    /// Where a session sits in the tree, when this client has seen the edge.
    ///
    /// The node's own parent is the answer wherever it is known, so a child
    /// opened from the list rather than from the open session's page still
    /// knows where it came from.
    pub fn parent_of(&self, session_id: &str) -> Option<VibexSessionId> {
        self.node(session_id)?
            .parent_session_ref
            .as_ref()
            .and_then(|reference| reference.session_id())
    }

    /// Start a read of one parent's children, unless one is already in flight.
    ///
    /// The answer is what clears the mark, so a read that is never answered
    /// cannot spin forever: the next refresh asks again.
    pub fn begin_load(&mut self, parent: Option<&VibexSessionId>) -> bool {
        if self.unsupported || !self.loading.insert(key(parent)) {
            return false;
        }
        self.failed.remove(&key(parent));
        true
    }

    /// Apply one page the runtime answered with.
    ///
    /// `append` continues the page already held; anything else replaces it, so
    /// a session the runtime no longer lists stops being drawn.
    pub fn apply(
        &mut self,
        parent: Option<&VibexSessionId>,
        append: bool,
        now_ms: i64,
        page: SessionTreePage,
    ) -> bool {
        let parent_key = key(parent);
        self.loading.remove(&parent_key);
        self.failed.remove(&parent_key);
        for session in page.registry.iter() {
            self.registry
                .insert(session.id.as_str().to_string(), session.clone());
        }
        if parent.is_none() {
            match (append, self.roots.as_mut()) {
                (true, Some(held)) => held.extend(page, now_ms),
                _ => self.roots = Some(ChildPage::of(page, now_ms)),
            }
        } else {
            match (append, self.pages.get_mut(&parent_key)) {
                (true, Some(held)) => held.extend(page, now_ms),
                _ => {
                    self.pages.insert(parent_key, ChildPage::of(page, now_ms));
                }
            }
        }
        if !append {
            self.prune_registry();
        }
        true
    }

    /// Record that a parent's read failed, keeping whatever was drawn before.
    pub fn fail(&mut self, parent: Option<&VibexSessionId>) {
        let key = key(parent);
        self.loading.remove(&key);
        self.failed.insert(key);
    }

    /// Record the path an open session's own page answered with.
    ///
    /// The path belongs to one session, so opening another replaces it rather
    /// than letting one session's parent stand in for another's.
    pub fn set_open_path(&mut self, session_id: &VibexSessionId, ancestors: Vec<SessionTreeNode>) {
        self.open = Some(session_id.as_str().to_string());
        self.ancestors = ancestors;
    }

    /// Whether the whole known tree still holds one session, so an id that
    /// never appears is not asked for twice.
    pub fn knows(&self, session_id: &str) -> bool {
        self.registry.contains_key(session_id)
    }

    /// The sessions the tree adds to the list: every child this client has
    /// metadata for.
    pub fn child_sessions(&self) -> impl Iterator<Item = &AgentSession> {
        self.registry
            .values()
            .filter(|session| self.parent_of(session.id.as_str()).is_some())
    }

    /// The owner of a child session, for a row that has to name its parent.
    pub fn parent_session(&self, session_id: &str) -> Option<&AgentSession> {
        let parent = self.parent_of(session_id)?;
        self.session(parent.as_str())
    }

    /// Drop metadata no drawn node points at any more.
    fn prune_registry(&mut self) {
        let mut keep: BTreeSet<String> = self
            .roots
            .iter()
            .flat_map(|page| page.nodes.iter())
            .chain(self.pages.values().flat_map(|page| page.nodes.iter()))
            .chain(self.ancestors.iter())
            .map(|node| node.session_ref.id.clone())
            .collect();
        keep.extend(self.pages.keys().cloned());
        self.registry.retain(|id, _| keep.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentId, ProjectId, VibexUseRef, WorkspaceId, WorkspaceMode};

    fn session(id: &str, title: &str, project: &str) -> AgentSession {
        AgentSession {
            id: VibexSessionId::parse(id).expect("valid session id"),
            title: title.to_string(),
            project_id: ProjectId::parse(project).expect("valid project id"),
            workspace_id: WorkspaceId::new(),
            workspace_root: "/repo/vibex".to_string(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("claude").expect("valid agent id"),
            state: vibex_core::AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 2,
            last_message_at_ms: 2,
            archived_at_ms: None,
            deleted_at_ms: None,
            tools: Default::default(),
        }
    }

    fn node(id: &str, parent: Option<&str>) -> SessionTreeNode {
        SessionTreeNode {
            session_ref: VibexUseRef::session(&VibexSessionId::parse(id).unwrap()),
            parent_session_ref: parent
                .map(|parent| VibexUseRef::session(&VibexSessionId::parse(parent).unwrap())),
            title: id.to_string(),
            agent_id: None,
            agent_label: None,
            task_ref: None,
            task_title: None,
            task_phase: None,
            completion_policy: None,
            child_count: 0,
            has_more_children: false,
            blocked_on: None,
            active_descendants: 0,
            blocked_descendants: 0,
            current_task_ref: None,
            updated_at_ms: 2,
        }
    }

    fn page(nodes: Vec<SessionTreeNode>, sessions: Vec<AgentSession>) -> SessionTreePage {
        SessionTreePage {
            nodes,
            ancestors: Vec::new(),
            registry: sessions,
            next_cursor: None,
            has_more: false,
        }
    }

    #[test]
    fn a_page_replaces_what_it_was_asked_for_and_names_its_children() {
        let mut tree = ChildSessions::default();
        assert!(tree.begin_load(None));
        assert!(
            !tree.begin_load(None),
            "a second read started while one was live"
        );
        tree.apply(
            None,
            false,
            1_000,
            page(
                vec![
                    node("session_root0001", None),
                    node("session_root0002", None),
                ],
                vec![
                    session("session_root0001", "root one", "project_one"),
                    session("session_root0002", "root two", "project_one"),
                ],
            ),
        );
        assert_eq!(tree.roots().len(), 2);
        assert!(tree.knows("session_root0001"));
        assert_eq!(tree.session_title("session_root0002"), Some("root two"));

        // A child page hangs off its parent and carries the metadata the root
        // list omits.
        let parent = VibexSessionId::parse("session_root0001").unwrap();
        assert!(tree.begin_load(Some(&parent)));
        tree.apply(
            Some(&parent),
            false,
            1_000,
            page(
                vec![node("session_child001", Some("session_root0001"))],
                vec![session("session_child001", "the worker", "project_one")],
            ),
        );
        assert_eq!(tree.child_count("session_root0001"), 1);
        assert_eq!(
            tree.parent_of("session_child001"),
            Some(VibexSessionId::parse("session_root0001").unwrap())
        );
        assert_eq!(tree.child_ids("session_root0001").len(), 1);

        // A fresh answer replaces the page: a worker the runtime dropped is no
        // longer drawn.
        tree.apply(Some(&parent), false, 2_000, page(Vec::new(), Vec::new()));
        assert!(!tree.has_children("session_root0001"));
    }

    #[test]
    fn an_appended_page_continues_the_branch_instead_of_restarting_it() {
        let parent = VibexSessionId::parse("session_root0001").unwrap();
        let mut tree = ChildSessions::default();
        let mut first = page(
            vec![node("session_child001", Some("session_root0001"))],
            vec![session("session_child001", "one", "project_one")],
        );
        first.has_more = true;
        first.next_cursor = Some("session_child001".to_string());
        tree.apply(Some(&parent), false, 1_000, first);
        assert!(tree.has_more_children("session_root0001"));

        tree.apply(
            Some(&parent),
            true,
            2_000,
            page(
                vec![
                    node("session_child001", Some("session_root0001")),
                    node("session_child002", Some("session_root0001")),
                ],
                vec![
                    session("session_child001", "one", "project_one"),
                    session("session_child002", "two", "project_one"),
                ],
            ),
        );
        assert_eq!(tree.child_count("session_root0001"), 2);
        assert!(!tree.has_more_children("session_root0001"));
    }

    #[test]
    fn the_open_sessions_path_is_its_own_and_folds_what_the_reader_closed() {
        let mut tree = ChildSessions::default();
        let open = VibexSessionId::parse("session_child001").unwrap();
        tree.set_open_path(
            &open,
            vec![
                node("session_root0001", None),
                node("session_mid00001", Some("session_root0001")),
            ],
        );
        assert_eq!(tree.open_session(), Some("session_child001"));
        assert_eq!(
            tree.parent_of_open()
                .map(|node| node.session_ref.id.as_str()),
            Some("session_mid00001")
        );

        // The answer is the state the branch now has.
        assert!(tree.toggle_collapsed("session_root0001"));
        assert!(tree.is_collapsed("session_root0001"));
        assert!(!tree.toggle_collapsed("session_root0001"));

        // A failed read is a state the renderer can tell from an empty branch.
        assert!(tree.begin_load(Some(&open)));
        tree.fail(Some(&open));
        assert!(tree.has_failed(Some(open.as_str())));
        assert!(!tree.is_loading(Some(open.as_str())));
    }

    #[test]
    fn only_unfolded_branches_are_kept_current() {
        let root = VibexSessionId::parse("session_root0001").unwrap();
        let mut tree = ChildSessions::default();
        let mut owner = node("session_root0001", None);
        owner.child_count = 1;
        tree.apply(
            None,
            false,
            1_000,
            page(
                vec![owner],
                vec![session("session_root0001", "root", "project_one")],
            ),
        );
        // A node the runtime says has children is a branch before its page has
        // been read, which is what makes the list fill itself in.
        assert_eq!(tree.expanded_parents(), vec![root.clone()]);
        // A fresh answer is not asked for again; an old one is. The roots are
        // a page of their own, and so is the branch under each session.
        assert!(!tree.needs_refresh(None, 1_500));
        assert!(tree.needs_refresh(None, 1_000 + CHILD_REFRESH_MS));
        assert!(tree.needs_refresh(Some(&root), 1_500));
        assert!(tree.begin_load(Some(&root)));
        tree.apply(
            Some(&root),
            false,
            1_500,
            page(
                vec![node("session_child001", Some("session_root0001"))],
                vec![session("session_child001", "worker", "project_one")],
            ),
        );
        assert!(!tree.needs_refresh(Some(&root), 2_000));
        assert!(tree.needs_refresh(Some(&root), 1_500 + CHILD_REFRESH_MS));
        // A branch whose read failed is asked for again at once.
        assert!(tree.begin_load(Some(&root)));
        tree.fail(Some(&root));
        assert!(tree.needs_refresh(Some(&root), 1_600));
        // Folding a branch hides everything under it, so nothing behind it is
        // kept current.
        assert!(tree.toggle_collapsed("session_root0001"));
        assert!(tree.expanded_parents().is_empty());
    }

    #[test]
    fn the_tree_is_pruned_back_to_the_sessions_that_still_exist() {
        let root = VibexSessionId::parse("session_root0001").unwrap();
        let mut tree = ChildSessions::default();
        tree.apply(
            None,
            false,
            1_000,
            page(
                vec![node("session_root0001", None)],
                vec![session("session_root0001", "root", "project_one")],
            ),
        );
        tree.apply(
            Some(&root),
            false,
            1_000,
            page(
                vec![node("session_child001", Some("session_root0001"))],
                vec![session("session_child001", "worker", "project_one")],
            ),
        );
        tree.toggle_collapsed("session_child001");
        let known = BTreeSet::from(["session_root0001".to_string()]);
        tree.retain(&known);
        assert!(!tree.knows("session_child001"));
        assert!(!tree.is_collapsed("session_child001"));
        assert!(tree.knows("session_root0001"));
    }
}
