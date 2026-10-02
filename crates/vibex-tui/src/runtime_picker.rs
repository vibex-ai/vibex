//! The runtime switcher's own state: the catalogue grouped, folded and
//! filtered, and the run options staged on it before they are applied.
//!
//! Three ideas live here rather than in the reducer, because the renderer needs
//! them as much as the key handlers do:
//!
//! * the *rows* the catalogue view draws — Agent headings with their entries
//!   under them, plus the pinned recent and starred sections — so a heading can
//!   never be selected as though it were an entry;
//! * the *draft*: the run options a reader has changed but not yet applied, so
//!   several of them cost one runtime switch rather than one each;
//! * the *status* one entry reports: whether it can be chosen at all, and
//!   whether the session is already moving onto it.

use std::collections::BTreeSet;

use vibex_core::{
    AgentId, RuntimeAuthSourceAvailability, RuntimeOptionAvailability, SessionRuntimeOption,
    SessionRuntimeSelection, SessionRuntimeSelectionStatus,
};

use crate::app::{App, RunOption, RunOptionKey, RuntimePickerView};
use crate::locale::Strings;
use crate::runtime_prefs::identity_matches;

/// How many remembered models the pinned "recent" section offers.
///
/// Small on purpose: the section is a shortcut past a long catalogue, and it is
/// also what the number keys choose from, so it has to fit in one glance.
pub const RECENT_LIMIT: usize = 5;

/// Rows one page key moves the picker's cursor.
pub const PICKER_PAGE_ROWS: usize = 10;

/// The switcher's state that is not "which view, which row".
///
/// It lives beside the overlay rather than inside it for two reasons: the
/// overlay is a value the renderer compares, and the filter survives the steps
/// between the two views, so a reader who searched once does not search again
/// on the way back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimePickerState {
    /// What the reader typed to narrow the catalogue.
    pub query: String,
    /// Whether typed characters go to the filter.
    ///
    /// A mode rather than type-to-filter: the catalogue's own keys (`r`, `*`,
    /// digits) have to stay reachable, and a reader mid-search is the only one
    /// who means a letter as text.
    pub filtering: bool,
    /// The Agents whose groups are folded away.
    pub folded: BTreeSet<AgentId>,
    /// The run options staged on the picker and not yet applied.
    pub draft: Option<RuntimeDraft>,
    /// The run-option row the cursor was last on.
    ///
    /// Kept outside the draft so leaving the run options does not lose the
    /// reader's place in them.
    pub option_row: usize,
    /// The first catalogue row the last frame drew.
    ///
    /// Recorded by the renderer, the way the transcript band records the rows it
    /// drew: the page keys need the window's height, and only the frame that put
    /// the list on screen knows what it was.
    pub scroll: usize,
    /// How many catalogue rows that frame had room for.
    pub page_rows: usize,
}

/// The run options one reader has changed but not applied.
///
/// Both halves are kept: `base` is what the Agent is on now, which is what
/// "discard" returns to and what the rows compare against to say what changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDraft {
    pub base: SessionRuntimeSelection,
    pub working: SessionRuntimeSelection,
}

impl RuntimeDraft {
    pub fn new(selection: SessionRuntimeSelection) -> Self {
        Self {
            base: selection.clone(),
            working: selection,
        }
    }

    /// Whether anything was staged at all.
    pub fn is_dirty(&self) -> bool {
        self.working != self.base
    }

    /// Whether one row's value is staged rather than in effect.
    pub fn changed(&self, key: &RunOptionKey) -> bool {
        match key {
            RunOptionKey::ReasoningEffort => {
                self.base.reasoning_effort != self.working.reasoning_effort
            }
            RunOptionKey::Mode => self.base.mode_id != self.working.mode_id,
            RunOptionKey::Feature(id) => {
                self.base.config_values.get(id) != self.working.config_values.get(id)
            }
        }
    }

    /// How many rows the draft moves.
    pub fn change_count(&self) -> usize {
        changed_run_options(&self.base, &self.working).len()
    }
}

/// The run options two selections disagree about.
pub fn changed_run_options(
    base: &SessionRuntimeSelection,
    working: &SessionRuntimeSelection,
) -> Vec<RunOptionKey> {
    let mut keys = Vec::new();
    if base.reasoning_effort != working.reasoning_effort {
        keys.push(RunOptionKey::ReasoningEffort);
    }
    if base.mode_id != working.mode_id {
        keys.push(RunOptionKey::Mode);
    }
    let mut ids = BTreeSet::new();
    ids.extend(base.config_values.keys().cloned());
    ids.extend(working.config_values.keys().cloned());
    for id in ids {
        if base.config_values.get(&id) != working.config_values.get(&id) {
            keys.push(RunOptionKey::Feature(id));
        }
    }
    keys
}

/// One row of the catalogue view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimePickerRow {
    /// A pinned section's title, which nothing can be done with.
    Section(RuntimePickerSection),
    /// One Agent's heading: how many entries it has, and whether it is folded.
    Agent {
        agent_id: AgentId,
        label: String,
        count: usize,
        folded: bool,
        /// Whether the entry the page is on lives in this group.
        current: bool,
        /// How many of its entries cannot be chosen right now.
        unavailable: usize,
    },
    /// One catalogue entry, by index into `catalog.options`.
    Entry {
        index: usize,
        /// The digit that chooses this row outright, when it has one.
        quick: Option<char>,
    },
}

impl RuntimePickerRow {
    /// Whether the row is a heading rather than something to choose.
    pub const fn is_heading(&self) -> bool {
        !matches!(self, Self::Entry { .. })
    }

    /// The catalogue entry this row names, when it names one.
    pub const fn entry(&self) -> Option<usize> {
        match self {
            Self::Entry { index, .. } => Some(*index),
            _ => None,
        }
    }
}

/// The pinned sections the catalogue view draws above the Agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimePickerSection {
    /// What the reader used last, whatever Agent it belonged to.
    Recent,
    /// What the reader starred.
    Favorites,
}

impl RuntimePickerSection {
    pub const ALL: [Self; 2] = [Self::Recent, Self::Favorites];

    pub const fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::Recent => strings.runtime_recent(),
            Self::Favorites => strings.runtime_favorites(),
        }
    }
}

/// What one catalogue row says about itself beyond its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEntryStatus {
    /// Nothing to report: the row can be chosen and answers.
    Ready,
    /// The session is already moving onto this entry.
    Switching,
    /// The last switch onto this entry did not take.
    Failed,
    /// The account behind it needs signing in.
    SignIn,
    /// The runtime is checking the account.
    Verifying,
    /// The runtime is reading the account's model list.
    Discovering,
    /// Something has to be set up first.
    Configure,
    /// Published, but not usable at all right now.
    Unavailable,
}

impl RuntimeEntryStatus {
    /// Whether the row has nothing to say for itself.
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// The words for it, or nothing when the row speaks for itself.
    pub const fn label(self, strings: &Strings) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Switching => Some(strings.runtime_switching()),
            Self::Failed => Some(strings.runtime_switch_failed()),
            Self::SignIn => Some(strings.runtime_status_sign_in()),
            Self::Verifying => Some(strings.runtime_status_verifying()),
            Self::Discovering => Some(strings.runtime_status_discovering()),
            Self::Configure => Some(strings.runtime_status_configure()),
            Self::Unavailable => Some(strings.runtime_unavailable()),
        }
    }
}

impl App {
    /// The selection the picker's run options read and write.
    ///
    /// A staged draft answers while it belongs to the selection the page is on:
    /// the rows have to show what the reader is about to apply, not what is
    /// still in effect behind the panel. The catalogue marker keeps asking the
    /// *page* ([`App::page_runtime_selection`]) instead, because staging a
    /// thinking depth does not move the session to another entry.
    pub fn picker_selection(&self) -> Option<SessionRuntimeSelection> {
        let selection = self.page_runtime_selection()?;
        match self.runtime_picker.draft.as_ref() {
            Some(draft) if identity_matches(&draft.base, &selection) => Some(draft.working.clone()),
            _ => Some(selection),
        }
    }

    /// The run options the picker lists: the page's Agent, staged values in
    /// place.
    pub fn picker_run_options(&self) -> Vec<RunOption> {
        match self.picker_selection() {
            Some(selection) => self.run_options_for(&selection),
            None => Vec::new(),
        }
    }

    /// How many rows one view of the switcher lists.
    pub fn runtime_picker_row_count(&self, view: RuntimePickerView) -> usize {
        match view {
            RuntimePickerView::Choices => self.runtime_picker_rows().len(),
            RuntimePickerView::Options => self.picker_run_options().len(),
        }
    }

    /// The catalogue rows the choices view draws, in order.
    ///
    /// A search flattens the catalogue: the reader typed a name, and a fold
    /// left over from browsing must not hide the row they asked for. Without a
    /// search the pinned sections come first — the shortcut past a long
    /// catalogue — and then the Agents in the order the runtime published them,
    /// each heading carrying what the reader needs to decide whether to open it.
    pub fn runtime_picker_rows(&self) -> Vec<RuntimePickerRow> {
        let Some(catalog) = self.runtime_options.as_ref() else {
            return Vec::new();
        };
        let query = normalized_query(&self.runtime_picker.query);
        let mut rows = Vec::new();
        let groups = catalog_groups(catalog);
        if !query.is_empty() {
            for (agent_id, label, indices) in groups {
                let matches = indices
                    .iter()
                    .copied()
                    .filter(|index| entry_matches(&catalog.options[*index], &query))
                    .collect::<Vec<_>>();
                if matches.is_empty() {
                    continue;
                }
                rows.push(self.agent_heading(&catalog.options, agent_id, label, &matches, false));
                rows.extend(
                    matches
                        .into_iter()
                        .map(|index| RuntimePickerRow::Entry { index, quick: None }),
                );
            }
            return rows;
        }
        let recent = self.runtime_prefs.recent(catalog, RECENT_LIMIT);
        if !recent.is_empty() {
            rows.push(RuntimePickerRow::Section(RuntimePickerSection::Recent));
            for (position, option) in recent.iter().enumerate() {
                rows.push(RuntimePickerRow::Entry {
                    index: catalog_index(catalog, &option.selection),
                    quick: char::from_digit(position as u32 + 1, 10),
                });
            }
        }
        let starred = self.runtime_prefs.starred(catalog);
        if !starred.is_empty() {
            rows.push(RuntimePickerRow::Section(RuntimePickerSection::Favorites));
            for option in starred {
                rows.push(RuntimePickerRow::Entry {
                    index: catalog_index(catalog, &option.selection),
                    quick: None,
                });
            }
        }
        for (agent_id, label, indices) in groups {
            let folded = self.runtime_picker.folded.contains(&agent_id);
            rows.push(self.agent_heading(&catalog.options, agent_id, label, &indices, folded));
            if !folded {
                rows.extend(
                    indices
                        .into_iter()
                        .map(|index| RuntimePickerRow::Entry { index, quick: None }),
                );
            }
        }
        rows
    }

    /// One Agent's heading, counted from the entries it will draw.
    fn agent_heading(
        &self,
        options: &[SessionRuntimeOption],
        agent_id: AgentId,
        label: String,
        indices: &[usize],
        folded: bool,
    ) -> RuntimePickerRow {
        let current = indices
            .iter()
            .any(|index| self.runtime_option_is_current(&options[*index]));
        let unavailable = indices
            .iter()
            .filter(|index| options[**index].availability != RuntimeOptionAvailability::Available)
            .count();
        RuntimePickerRow::Agent {
            agent_id,
            label,
            count: indices.len(),
            folded,
            current,
            unavailable,
        }
    }

    /// What one catalogue row says for itself.
    pub fn runtime_entry_status(&self, option: &SessionRuntimeOption) -> RuntimeEntryStatus {
        // The entry the session is on answers with the switch's own state: a
        // reader who asked for it deserves "moving" or "did not take" rather
        // than a second "chosen".
        if self.page_shows_session() && self.runtime_option_is_current(option) {
            match self.runtime_selection_status() {
                Some(
                    SessionRuntimeSelectionStatus::WaitingForCurrentWork
                    | SessionRuntimeSelectionStatus::Preparing,
                ) => return RuntimeEntryStatus::Switching,
                Some(SessionRuntimeSelectionStatus::FailedUsingPrevious) => {
                    return RuntimeEntryStatus::Failed;
                }
                _ => {}
            }
        }
        match option.availability {
            RuntimeOptionAvailability::Available => {}
            RuntimeOptionAvailability::RequiresConfiguration => {
                return RuntimeEntryStatus::Configure;
            }
            RuntimeOptionAvailability::TemporarilyUnavailable => {
                return RuntimeEntryStatus::Unavailable;
            }
        }
        let source = self.runtime_options.as_ref().and_then(|catalog| {
            catalog.auth_sources.iter().find(|source| {
                source.agent_id == option.selection.agent_id
                    && source.source == option.selection.auth_source
            })
        });
        match source.map(|source| source.availability) {
            Some(RuntimeAuthSourceAvailability::RequiresAuthentication) => {
                RuntimeEntryStatus::SignIn
            }
            Some(RuntimeAuthSourceAvailability::Verifying) => RuntimeEntryStatus::Verifying,
            Some(RuntimeAuthSourceAvailability::DiscoveringModels) => {
                RuntimeEntryStatus::Discovering
            }
            Some(RuntimeAuthSourceAvailability::RequiresConfiguration) => {
                RuntimeEntryStatus::Configure
            }
            Some(
                RuntimeAuthSourceAvailability::TemporarilyUnavailable
                | RuntimeAuthSourceAvailability::Unsupported,
            ) => RuntimeEntryStatus::Unavailable,
            Some(RuntimeAuthSourceAvailability::Available) | None => RuntimeEntryStatus::Ready,
        }
    }

    /// The session's own switch state, when the page is showing a session.
    ///
    /// A page showing none has no session to be switching, and the client keeps
    /// one selected behind every such page — reporting that one's state would
    /// describe a session the reader is not looking at.
    pub fn runtime_selection_status(&self) -> Option<SessionRuntimeSelectionStatus> {
        if !self.page_shows_session() {
            return None;
        }
        self.agent
            .state
            .runtime_selection
            .value
            .as_ref()
            .map(|state| state.status)
    }

    /// Whether the switcher is the modal on screen.
    pub fn runtime_picker_is_open(&self) -> bool {
        matches!(
            self.overlay,
            Some(crate::app::Overlay::RuntimePicker { .. })
        )
    }

    /// Whether the picker's catalogue is taking typed characters.
    pub fn runtime_picker_filtering(&self) -> bool {
        matches!(
            self.overlay,
            Some(crate::app::Overlay::RuntimePicker {
                view: RuntimePickerView::Choices,
                ..
            })
        ) && self.runtime_picker.filtering
    }

    /// The query the catalogue is filtered by, for the renderer.
    pub fn runtime_picker_query(&self) -> &str {
        &self.runtime_picker.query
    }

    /// Type one character into the catalogue's filter.
    ///
    /// The cursor goes back to the top: the rows under it changed, and the row
    /// that was at index four is not the row the reader was looking at.
    pub fn push_runtime_picker_query(&mut self, character: char) {
        self.runtime_picker.query.push(character);
        self.runtime_picker.scroll = 0;
        self.set_runtime_row_zero();
    }

    /// Take the last character back out of the filter.
    pub fn pop_runtime_picker_query(&mut self) {
        self.runtime_picker.query.pop();
        self.runtime_picker.scroll = 0;
        self.set_runtime_row_zero();
    }

    /// Empty the filter.
    pub fn clear_runtime_picker_query(&mut self) {
        self.runtime_picker.query.clear();
        self.runtime_picker.scroll = 0;
        self.set_runtime_row_zero();
    }

    /// Give up the filter: the query first, then the mode.
    ///
    /// Answers whether there was anything to give up, which is what lets `Esc`
    /// close the switcher once the filter is already empty.
    pub fn cancel_runtime_picker_filter(&mut self) -> bool {
        if !self.runtime_picker.query.is_empty() {
            self.clear_runtime_picker_query();
            return true;
        }
        if self.runtime_picker.filtering {
            self.runtime_picker.filtering = false;
            return true;
        }
        false
    }

    /// Put the catalogue cursor back on its first row without letting the
    /// renderer's remembered offset drag the view down with it.
    fn set_runtime_row_zero(&mut self) {
        if let Some(crate::app::Overlay::RuntimePicker { view, .. }) = self.overlay {
            self.overlay = Some(crate::app::Overlay::RuntimePicker { view, selected: 0 });
        }
    }

    /// Remember one applied selection and write the file.
    ///
    /// Free-text values are dropped first: what is remembered is what the Agent
    /// can be asked for again, not a word typed for one session.
    pub fn remember_runtime_selection(&mut self, selection: &SessionRuntimeSelection) {
        let persisted = crate::runtime_prefs::persisted(self.runtime_options.as_ref(), selection);
        self.runtime_prefs.remember(&persisted);
        self.runtime_prefs.save(self.runtime_path.as_deref());
    }

    /// Reconcile the composing page's choice with a catalogue that moved.
    ///
    /// The choice stands while the catalogue still publishes it: a reader who
    /// picked an entry did not ask for a catalogue read to overwrite it. When
    /// the Agent or the model went away, the Agent's own remembered answer is
    /// the closest thing to what they asked for.
    pub fn reconcile_remembered_runtime(&mut self) -> bool {
        let Some(catalog) = self.runtime_options.as_ref() else {
            return false;
        };
        let Some(selection) = self.new_session_runtime.as_ref() else {
            // Remembered defaults are derived, not unsent edits. A late
            // catalogue read must not claim ownership of the next draft.
            return false;
        };
        let agent_id = Some(selection.agent_id.clone());
        if let Some(selection) = self.new_session_runtime.as_ref()
            && crate::runtime_prefs::catalog_entry(catalog, selection).is_some()
        {
            return false;
        }
        let preferred = self.runtime_prefs.preferred(catalog, agent_id.as_ref());
        if preferred == self.new_session_runtime {
            return false;
        }
        self.new_session_runtime = preferred;
        true
    }

    /// The catalogue entry at one row of the choices view.
    pub fn runtime_picker_entry(&self, row: usize) -> Option<usize> {
        self.runtime_picker_rows()
            .get(row)
            .and_then(|row| row.entry())
    }

    /// The row of the choices view the cursor is on, when it names an entry.
    pub fn runtime_picker_highlighted_entry(&self, row: usize) -> Option<SessionRuntimeOption> {
        let index = self.runtime_picker_entry(row)?;
        self.runtime_options
            .as_ref()
            .and_then(|catalog| catalog.options.get(index))
            .cloned()
    }
}

/// The three words a filter is matched by, lowercased and stripped of repeated
/// whitespace, so `  DeepSeek   flash ` means what it says.
pub fn normalized_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether one catalogue entry answers a filter: its Agent, its account and its
/// model, whichever the reader half-remembers.
pub fn entry_matches(option: &SessionRuntimeOption, query: &str) -> bool {
    query.is_empty()
        || option.agent_label.to_lowercase().contains(query)
        || option.auth_source_label.to_lowercase().contains(query)
        || option.model_label.to_lowercase().contains(query)
}

/// The catalogue's entries, grouped by Agent, in the order the runtime
/// published them.
fn catalog_groups(
    catalog: &vibex_core::SessionRuntimeOptionCatalog,
) -> Vec<(AgentId, String, Vec<usize>)> {
    let mut groups: Vec<(AgentId, String, Vec<usize>)> = Vec::new();
    for (index, option) in catalog.options.iter().enumerate() {
        match groups
            .iter_mut()
            .find(|(agent_id, ..)| *agent_id == option.selection.agent_id)
        {
            Some((_, _, indices)) => indices.push(index),
            None => groups.push((
                option.selection.agent_id.clone(),
                option.agent_label.clone(),
                vec![index],
            )),
        }
    }
    groups
}

/// Where one selection sits in the catalogue.
fn catalog_index(
    catalog: &vibex_core::SessionRuntimeOptionCatalog,
    selection: &SessionRuntimeSelection,
) -> usize {
    catalog
        .options
        .iter()
        .position(|option| identity_matches(&option.selection, selection))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{ProviderProfileId, SessionConfigValue};

    fn selection(agent: &str, model: &str) -> SessionRuntimeSelection {
        SessionRuntimeSelection::provider(
            AgentId::parse(agent).expect("agent id"),
            ProviderProfileId::new(),
            model,
        )
    }

    #[test]
    fn changed_run_options_names_every_field_the_draft_moves() {
        let base = selection("codex", "gpt-5");
        let mut working = base.clone();
        working.reasoning_effort = Some("high".to_string());
        working
            .config_values
            .insert("web_search".to_string(), "true".to_string());
        assert_eq!(
            changed_run_options(&base, &working),
            vec![
                RunOptionKey::ReasoningEffort,
                RunOptionKey::Feature("web_search".to_string())
            ]
        );
        let draft = RuntimeDraft {
            base: base.clone(),
            working: working.clone(),
        };
        assert!(draft.is_dirty());
        assert_eq!(draft.change_count(), 2);
        assert!(draft.changed(&RunOptionKey::ReasoningEffort));
        assert!(!draft.changed(&RunOptionKey::Mode));
        // A draft returned to its base is not dirty, which is what makes
        // `r`-ing every row back to the default the same as never having
        // touched them.
        assert!(!RuntimeDraft::new(base).is_dirty());
    }

    #[test]
    fn a_filter_matches_each_of_the_three_names() {
        let option = SessionRuntimeOption {
            selection: selection("deepseek-harness", "deepseek-v4.1-flash"),
            agent_label: "DeepSeek Harness".to_string(),
            auth_source_label: "Default CLI account".to_string(),
            model_label: "bai/deepseek-v4.1-flash".to_string(),
            reasoning_efforts: vec![SessionConfigValue {
                value: "high".to_string(),
                label: None,
            }],
            modes: Vec::new(),
            features: Vec::new(),
            availability: RuntimeOptionAvailability::Available,
        };
        for query in ["deepseek", "harness", "cli", "v4.1", "flash"] {
            assert!(
                entry_matches(&option, &normalized_query(query)),
                "{query} did not match"
            );
        }
        assert!(!entry_matches(&option, &normalized_query("openrouter")));
        assert_eq!(
            normalized_query("  DeepSeek   Harness "),
            "deepseek harness"
        );
    }
}
