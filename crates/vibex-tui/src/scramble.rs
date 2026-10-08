//! The character transition a line wears when its text changes.
//!
//! A terminal has no fade, no slide and no cross-dissolve: the only thing it
//! can do between two states of a line is draw the cells in between. What it
//! can draw is characters, and characters are enough — a line whose text is
//! replaced cell by cell, the new letters arriving out of a scatter of noise,
//! reads as the same line *becoming* something else rather than as one line
//! being yanked out and another dropped in.
//!
//! The whole point is the direction: the text resolves from the left, so the
//! reader's eye is carried along the line the way it reads it, and the cells
//! that are not changing stay untouched. A workspace path whose last folder
//! changed therefore scrambles its tail and leaves the path that led there
//! alone, which is the difference between an effect that says *this part
//! changed* and one that says *something happened*.
//!
//! # What is deliberately not here
//!
//! * **no colour.** The transition is characters, so it survives a terminal
//!   that can draw none, and it costs the page no palette decision. A reader
//!   who wants it gone turns it off in the settings; nothing about it depends
//!   on the theme.
//! * **no randomness the client cannot repeat.** The noise on a cell is a
//!   function of the cell and the frame, never of the moment the frame was
//!   drawn. Two renders of one frame are the same picture — which is what makes
//!   a resize, a scroll or a second window draw the line the reader is already
//!   looking at rather than a new shuffle of it, and what lets a test say what
//!   a frame looks like.
//! * **no bookkeeping in the gestures.** What a line *was* is recorded by the
//!   frame that draws it ([`Transitions::observe`]), because the text is the
//!   only honest answer to "did this line change": every path that writes a new
//!   Agent or a new workspace — the picker, the settings, a catalogue that
//!   arrives late — gets the same effect without any of them knowing the effect
//!   exists.

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthChar;

/// Which line a transition belongs to.
///
/// Two lines rather than one, because they change independently: a reader who
/// changed the directory has not changed the Agent, and a single run would
/// scramble both to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    /// The Agent and model the composing page will run.
    Runtime,
    /// The directory the session will work in.
    Workspace,
}

/// Frames one transition takes.
///
/// Read at the tick the client runs while something is moving — about eight
/// frames a second — a transition is a second and a half: long enough to be
/// seen as a settle, short enough that a reader walking a directory tree with
/// `Ctrl+W` is never waiting for the last one to finish before starting the
/// next.
pub const FRAMES: u32 = 12;

/// The characters a cell wears while it waits its turn.
///
/// Punctuation, and only punctuation: it is the one set that is one column wide
/// in every font, that no theme gives a meaning to, and that cannot be mistaken
/// for the text it is hiding. A letter from the alphabet would read as a typo
/// rather than as noise.
const NOISE: &[char] = &[
    '!', '<', '>', '-', '_', '\\', '/', '[', ']', '{', '}', '(', ')', '=', '+', '*', '^', '?', '#',
    '@', '$', '%', '&', ':', ';', ',', '.', '|', '~',
];

/// The wide cells a full-width character is hidden behind.
///
/// A wide character replaced by a narrow one would change the width of the line
/// and pull the whole row sideways for as long as the transition ran. These are
/// the punctuation of a full-width font: the same statement `NOISE` makes, in
/// the two columns the cell actually occupies. Every one of them is
/// unambiguously wide — the half-width and ambiguous forms would measure as one
/// column in a Western font and reintroduce exactly the shift this avoids.
const WIDE_NOISE: &[char] = &['＃', '＊', '＋', '＝', '？', '！', '％', '＠'];

/// One transition in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scramble {
    /// What each part of the line showed when the transition began.
    ///
    /// Parts rather than one string because a line can be drawn in more than
    /// one style: the Agent and the model are two spans on the composer's line,
    /// and they must arrive as themselves rather than as a single flattened
    /// run.
    from: Vec<String>,
    /// The animation frame the transition began on.
    started: u32,
}

impl Scramble {
    /// Begin a transition from what a line showed.
    pub fn new(from: Vec<String>, started: u32) -> Self {
        Self { from, started }
    }

    /// Whether the transition has drawn its last frame.
    pub fn finished(&self, phase: u32) -> bool {
        phase.wrapping_sub(self.started) >= FRAMES
    }

    /// How far into the transition `phase` is, in frames.
    fn elapsed(&self, phase: u32) -> u32 {
        phase.wrapping_sub(self.started).min(FRAMES)
    }

    /// The text one part of the line is drawn as at `phase`.
    ///
    /// `part` is which piece of the line this is — the Agent, the model — so
    /// each keeps its own style and its own arrival. A part the transition did
    /// not know about is treated as empty, which scrambles it whole: a line that
    /// grew a piece has something to say about it.
    pub fn drawn(&self, part: usize, target: &str, phase: u32) -> String {
        if self.finished(phase) {
            return target.to_string();
        }
        let from = self.from.get(part).map(String::as_str).unwrap_or_default();
        let target: Vec<char> = target.chars().collect();
        let from: Vec<char> = from.chars().collect();
        // The cells the two texts share hold still from the first frame. The
        // transition is about what changed, so a line that changed in one
        // folder does not scramble the path that led to it.
        let changed = target
            .iter()
            .enumerate()
            .filter(|(index, character)| from.get(*index) != Some(*character))
            .count();
        if changed == 0 {
            return target.iter().collect();
        }

        let elapsed = self.elapsed(phase);
        let mut drawn = String::new();
        let mut order = 0u32;
        for (index, character) in target.iter().enumerate() {
            if from.get(index) == Some(character) {
                drawn.push(*character);
                continue;
            }
            // The cells that changed settle one after another from the left:
            // the k-th of them is due after its own share of the transition, so
            // the line arrives the way it is read and the last cell settles on
            // the frame the transition ends.
            let due = u64::from(order + 1) * u64::from(FRAMES) / changed as u64;
            if u64::from(elapsed) >= due {
                drawn.push(*character);
            } else {
                drawn.push(noise(index as u64, u64::from(phase), *character));
            }
            order += 1;
        }
        drawn
    }
}

/// What a cell wears while it waits: a character of the same width that is not
/// the one it is hiding.
fn noise(index: u64, phase: u64, hidden: char) -> char {
    let wide = UnicodeWidthChar::width(hidden).unwrap_or(1) > 1;
    let set: &[char] = if wide { WIDE_NOISE } else { NOISE };
    let roll = roll(index << 20 | phase);
    let mut character = set[(roll % set.len() as u64) as usize];
    if character == hidden {
        character = set[((roll >> 8) as usize + 1) % set.len()];
    }
    character
}

/// The three lines of the composing page, as the last frame drew them.
///
/// Held apart rather than as one string because that is how they are drawn: the
/// Agent and the model are two spans, and the workspace is its own row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Seen {
    agent: String,
    model: String,
    workspace: String,
}

/// The transitions in flight, and what the lines looked like before them.
///
/// One of these lives on the app. It is driven by the frame that draws the
/// page: the page says what it is about to draw, and every line that differs
/// from the last look gets a transition. A page that is not on screen — or one
/// an overlay is covering — is not observed at all, so a value that arrives
/// while the reader is elsewhere waits to be seen rather than playing to an
/// empty room.
#[derive(Debug, Clone, Default)]
pub struct Transitions {
    /// What the lines were last drawn as, or `None` before the page has been
    /// drawn at all: the first look records rather than transitions, because a
    /// page that has just been opened has nothing to carry.
    seen: Option<Seen>,
    runs: BTreeMap<Slot, Scramble>,
}

impl Transitions {
    /// Note the lines the composing page is about to draw, and begin a
    /// transition for each one that changed since the last look.
    ///
    /// `enabled` is the reader's setting. It is checked here rather than at the
    /// draw site so that a page with the effect switched off still *records*
    /// what it drew: switching the effect back on must not scramble the page
    /// with a change that happened while it was off.
    pub fn observe(
        &mut self,
        agent: &str,
        model: &str,
        workspace: &str,
        phase: u32,
        enabled: bool,
    ) {
        let seen = Seen {
            agent: agent.to_string(),
            model: model.to_string(),
            workspace: workspace.to_string(),
        };
        let Some(previous) = self.seen.replace(seen) else {
            return;
        };
        if !enabled {
            self.runs.clear();
            return;
        }
        self.begin(
            Slot::Runtime,
            vec![previous.agent, previous.model],
            &[agent, model],
            phase,
        );
        self.begin(
            Slot::Workspace,
            vec![previous.workspace],
            &[workspace],
            phase,
        );
    }

    /// Start one line's transition, if the line changed since the last look.
    ///
    /// A line that did not change is left as it is: what it is already carrying
    /// is still being carried, and the frame that notes it drew the same text
    /// is the second frame of a transition rather than the end of one.
    fn begin(&mut self, slot: Slot, from: Vec<String>, target: &[&str], phase: u32) {
        if from.iter().map(String::as_str).eq(target.iter().copied()) {
            return;
        }
        self.runs.insert(slot, Scramble::new(from, phase));
    }

    /// The text one part of a line is drawn as, with any transition applied.
    pub fn drawn(&self, slot: Slot, part: usize, target: &str, phase: u32) -> String {
        match self.runs.get(&slot) {
            Some(run) => run.drawn(part, target, phase),
            None => target.to_string(),
        }
    }

    /// Whether any transition still has a frame to draw.
    pub fn running(&self, phase: u32) -> bool {
        self.runs.values().any(|run| !run.finished(phase))
    }

    /// Drop the transitions that have finished.
    pub fn prune(&mut self, phase: u32) {
        self.runs.retain(|_, run| !run.finished(phase));
    }

    /// Stop everything in flight, keeping what the lines looked like.
    ///
    /// The page was left in the middle of a transition. The rest of it is frames
    /// nobody is looking at, and the change it was carrying has already been
    /// recorded: the next visit draws the lines as what they are rather than
    /// scrambling a change the reader has since read elsewhere.
    pub fn cancel(&mut self) {
        self.runs.clear();
    }
}

/// A small, reproducible scatter for one cell of one frame.
fn roll(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(from: &[&str], part: usize, target: &str, started: u32, phase: u32) -> String {
        Scramble::new(from.iter().map(|part| part.to_string()).collect(), started)
            .drawn(part, target, phase)
    }

    #[test]
    fn a_transition_arrives_from_the_left_and_ends_on_the_text() {
        let from = ["Codex"];
        let target = "Claude";
        // The first frame is all noise: there is nothing of the new text on
        // screen yet, which is what makes the line read as arriving.
        let first = drawn(&from, 0, target, 0, 0);
        assert_eq!(first.chars().count(), target.chars().count());
        assert_ne!(first, target);
        // Nothing anywhere in the transition is longer or shorter than the text
        // it will become: a line that grew would move the rows under it.
        for phase in 0..FRAMES {
            assert_eq!(drawn(&from, 0, target, 0, phase).chars().count(), 6);
        }
        // It settles from the left: whatever has arrived is a prefix of the
        // text, so the tail is the part still in flight.
        let mut arrived = 0;
        for phase in 1..=FRAMES {
            let frame = drawn(&from, 0, target, 0, phase);
            let prefix = frame
                .chars()
                .zip(target.chars())
                .take_while(|(drawn, target)| drawn == target)
                .count();
            assert!(
                prefix >= arrived,
                "phase {phase} lost a character it had already settled"
            );
            arrived = prefix;
        }
        // And the last frame is the text exactly.
        assert_eq!(drawn(&from, 0, target, 0, FRAMES), target);
        assert_eq!(drawn(&from, 0, target, 0, FRAMES + 40), target);
    }

    #[test]
    fn only_the_cells_that_changed_are_disturbed() {
        // A directory picked one level down: the path that led there is not
        // part of what changed, so it is not part of the effect.
        let from = ["/home/peatboy/vibex-dev"];
        let target = "/home/peatboy/vibex-dev/apps";
        let first = drawn(&from, 0, target, 0, 0);
        assert!(
            first.starts_with("/home/peatboy/vibex-dev"),
            "the transition disturbed the path that did not change: {first}"
        );
        assert_ne!(first, target);
        // What did change is in flight, and the whole line is the width it
        // will settle at.
        assert_ne!(&first[23..], "/apps");
        assert_eq!(first.chars().count(), target.chars().count());

        // Two texts with nothing in common are scrambled from the first cell,
        // while a shared opening is left where it is.
        assert!(!drawn(&["Codex"], 0, "gemini", 0, 0).starts_with('g'));
        assert!(drawn(&["Codex"], 0, "Claude", 0, 0).starts_with('C'));
    }

    #[test]
    fn the_same_frame_is_the_same_picture_every_time() {
        // The noise is a function of the cell and the frame, so a frame drawn
        // twice is one picture — which is what keeps a resize or a second draw
        // of the same tick from reshuffling a transition already on screen.
        let scramble = Scramble::new(vec!["Codex".to_string()], 7);
        for phase in 7..(7 + FRAMES) {
            assert_eq!(
                scramble.drawn(0, "Claude", phase),
                scramble.drawn(0, "Claude", phase)
            );
        }
        assert_ne!(
            scramble.drawn(0, "Claude", 7),
            scramble.drawn(0, "Claude", 8),
            "the noise did not move between frames"
        );
        // The noise never spells the character it is hiding.
        for phase in 7..(7 + FRAMES) {
            let frame = scramble.drawn(0, "Claude", phase);
            for (index, character) in frame.chars().enumerate() {
                if let Some(target) = "Claude".chars().nth(index)
                    && character == target
                {
                    continue;
                }
                assert!(
                    NOISE.contains(&character) || WIDE_NOISE.contains(&character),
                    "phase {phase} drew {character:?}, which is neither the text nor noise"
                );
            }
        }
    }

    #[test]
    fn a_line_drawn_in_parts_keeps_its_parts() {
        // The Agent and the model are two spans on the composer's line, and the
        // transition has to leave them two: a part that arrived as one flattened
        // run would lose the model's own emphasis.
        let scramble = Scramble::new(vec!["Codex".to_string(), "gpt-5".to_string()], 0);
        assert_eq!(scramble.drawn(0, "Claude", FRAMES), "Claude");
        assert_eq!(scramble.drawn(1, "opus", FRAMES), "opus");
        // A part the transition never saw is scrambled whole rather than shown
        // as if it had always been there.
        let grown = drawn(&["Codex"], 1, "opus", 0, 0);
        assert_ne!(grown, "opus");
        assert_eq!(grown.chars().count(), 4);

        // A wide character is hidden behind a wide one: a narrow substitute
        // would pull the line sideways for as long as the transition ran.
        let wide = drawn(&[""], 0, "工作区", 0, 0);
        assert_ne!(wide, "工作区");
        assert_eq!(crate::text::display_width(&wide), 6);
        for character in wide.chars() {
            assert_eq!(
                UnicodeWidthChar::width(character),
                Some(2),
                "{character:?} is not the width of the cell it hides"
            );
        }
    }

    #[test]
    fn the_page_carries_the_lines_that_changed_and_no_others() {
        let mut transitions = Transitions::default();
        // The first look records: a page that has just been opened has nothing
        // to carry.
        transitions.observe("Codex", "gpt-5", "/home/peatboy", 0, true);
        assert!(!transitions.running(0));

        // A new workspace is a transition on the workspace line and not on the
        // Agent's.
        transitions.observe("Codex", "gpt-5", "/home/peatboy/vibex", 3, true);
        assert!(transitions.running(3));
        assert_ne!(
            transitions.drawn(Slot::Workspace, 0, "/home/peatboy/vibex", 3),
            "/home/peatboy/vibex"
        );
        assert_eq!(transitions.drawn(Slot::Runtime, 0, "Codex", 3), "Codex");
        assert_eq!(transitions.drawn(Slot::Runtime, 1, "gpt-5", 3), "gpt-5");

        // A whole transition later, the lines are what they say they are.
        let settled = 3 + FRAMES;
        assert!(!transitions.running(settled));
        assert_eq!(
            transitions.drawn(Slot::Workspace, 0, "/home/peatboy/vibex", settled),
            "/home/peatboy/vibex"
        );

        // The Agent changed: both of its parts arrive, and the workspace that
        // did not change is left alone.
        transitions.observe("Claude", "opus", "/home/peatboy/vibex", settled, true);
        assert!(transitions.running(settled));
        assert_ne!(
            transitions.drawn(Slot::Runtime, 0, "Claude", settled),
            "Claude"
        );
        assert_eq!(
            transitions.drawn(Slot::Workspace, 0, "/home/peatboy/vibex", settled),
            "/home/peatboy/vibex"
        );

        // The next look finds the same text, and that is not the end of the
        // transition: the frame that notes a line is unchanged is the second
        // frame of what it is carrying, not the last one.
        transitions.observe("Claude", "opus", "/home/peatboy/vibex", settled + 1, true);
        assert!(transitions.running(settled + 1));
        assert_ne!(
            transitions.drawn(Slot::Runtime, 0, "Claude", settled + 1),
            "Claude"
        );

        // It finishes on its own clock, and a page whose lines have stopped
        // changing settles for good.
        let done = settled + FRAMES;
        assert!(!transitions.running(done));
        transitions.prune(done);
        transitions.observe("Claude", "opus", "/home/peatboy/vibex", done + 1, true);
        assert!(!transitions.running(done + 1));
        assert_eq!(
            transitions.drawn(Slot::Runtime, 0, "Claude", done + 1),
            "Claude"
        );
    }

    #[test]
    fn the_effect_switched_off_still_records_what_the_page_drew() {
        // A reader who turns the effect off must not have it fire once, the
        // moment they turn it back on, for a change they never saw.
        let mut transitions = Transitions::default();
        transitions.observe("Codex", "gpt-5", "/home/peatboy", 0, false);
        transitions.observe("Claude", "opus", "/home/peatboy/vibex", 4, false);
        assert!(!transitions.running(4));

        transitions.observe("Claude", "opus", "/home/peatboy/vibex", 8, false);
        assert!(!transitions.running(8));
        // Turning it on is not itself a change: the lines settle as they are.
        transitions.observe("Claude", "opus", "/home/peatboy/vibex", 9, true);
        assert!(!transitions.running(9));

        // And the next change is carried.
        transitions.observe("Claude", "sonnet", "/home/peatboy/vibex", 12, true);
        assert!(transitions.running(12));
    }

    #[test]
    fn leaving_the_page_stops_the_transition() {
        let mut transitions = Transitions::default();
        transitions.observe("Codex", "gpt-5", "/home/peatboy", 0, true);
        transitions.observe("Claude", "opus", "/home/peatboy", 2, true);
        assert!(transitions.running(2));

        // The reader leaves mid-transition: the frames nobody is looking at are
        // dropped rather than drawn, and the line they were carrying is drawn
        // as the text it already is when the page comes back.
        transitions.cancel();
        assert!(!transitions.running(2));
        transitions.observe("Claude", "opus", "/home/peatboy", 30, true);
        assert!(!transitions.running(30));
        assert_eq!(transitions.drawn(Slot::Runtime, 0, "Claude", 30), "Claude");

        // A finished transition is dropped rather than kept for the life of the
        // page, so the client can stop repainting.
        transitions.observe("Claude", "sonnet", "/home/peatboy", 32, true);
        assert!(transitions.running(32));
        transitions.prune(32 + FRAMES);
        assert!(!transitions.running(32 + FRAMES));
    }
}
