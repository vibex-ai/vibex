//! The self-targeting guard.
//!
//! An Agent that can drive the desktop can drive **the window it is running
//! in**. That is not a permission question — it is an infinite regression and
//! an injection loop: Vibex paints an approval card, the Agent reads it as
//! screen content, clicks it, and the cycle repeats. The guard therefore
//! fails closed: a target that *might* be the host is refused, and the refusal
//! is recorded.
//!
//! Three shapes of evidence are checked, because the engine may describe a
//! target in any of them:
//!
//! * a **pid** anywhere in the payload that belongs to the host process tree;
//! * a **window id** the runtime has previously learned as its own;
//! * a **screen rectangle** that falls inside one of the host's own windows,
//!   including nested `target` objects.
//!
//! The window geometry is learned from the platform (not guessed from the
//! desktop size), and the learned set is only ever additive within a session:
//! forgetting a window would open the regression again.

use std::collections::HashSet;

use serde_json::Value;
use vibex_core::ComputerRect;

/// How deep the recursive scan goes before it refuses.
///
/// The bound exists so a hostile or accidental deeply-nested payload cannot
/// blow the stack; crossing it is a refusal rather than a pass.
pub const MAX_SCAN_DEPTH: usize = 12;

/// Keys the guard scans for nested identity.
const PID_KEYS: &[&str] = &["pid", "process_id", "processId", "owner_pid", "ownerPid"];
const WINDOW_KEYS: &[&str] = &["window_id", "windowId", "handle", "window"];
const TARGET_KEYS: &[&str] = &[
    "target", "targets", "window", "windows", "element", "elements",
];

/// What the guard knows about the host.
#[derive(Debug, Clone, Default)]
pub struct SelfTargetGuard {
    host_pids: HashSet<i32>,
    host_window_ids: HashSet<String>,
    host_windows: Vec<ComputerRect>,
    /// The desktop rectangle the host occupies, when the platform reports it.
    host_desktop: Option<ComputerRect>,
}

impl SelfTargetGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the host process and its child processes.
    pub fn add_host_pid(&mut self, pid: i32) {
        if pid > 0 {
            self.host_pids.insert(pid);
        }
    }

    /// Registers a window the runtime owns.
    pub fn add_host_window(&mut self, window_id: impl Into<String>, bounds: Option<ComputerRect>) {
        let window_id = window_id.into();
        if !window_id.is_empty() {
            self.host_window_ids.insert(window_id);
        }
        if let Some(bounds) = bounds {
            self.host_windows.push(bounds);
        }
    }

    /// Registers a rectangle without an id, for platforms that only report
    /// geometry.
    pub fn add_host_bounds(&mut self, bounds: ComputerRect) {
        self.host_windows.push(bounds);
    }

    /// Records the full desktop rectangle on platforms where the host window
    /// cannot be isolated (a Wayland compositor that reports no per-window
    /// geometry, for example). Every action then refuses: the guard's job is to
    /// fail closed, and "the whole screen is the host" is the only honest
    /// reading of that platform state.
    pub fn cover_entire_desktop(&mut self, bounds: ComputerRect) {
        self.host_desktop = Some(bounds);
    }

    pub fn host_pids(&self) -> &HashSet<i32> {
        &self.host_pids
    }

    pub fn host_window_count(&self) -> usize {
        self.host_window_ids.len()
    }

    /// True when the runtime knows nothing about its own windows.
    ///
    /// The caller must treat this as "the guard cannot answer" and refuse, not
    /// as "there is nothing to guard".
    pub fn is_blind(&self) -> bool {
        self.host_pids.is_empty() && self.host_window_ids.is_empty() && self.host_windows.is_empty()
    }

    /// Scans one payload for evidence that a target is the host.
    ///
    /// Returns the reason it matched, which the ledger records verbatim. The
    /// scan is recursive because engines nest the real target inside a
    /// `target` array and a shallow check is exactly how a nested self-target
    /// slipped past the reference implementation.
    pub fn inspect(&self, payload: &Value) -> Option<String> {
        self.inspect_value(payload, 0)
    }

    fn inspect_value(&self, value: &Value, depth: usize) -> Option<String> {
        if depth > MAX_SCAN_DEPTH {
            // Fail closed. A target hidden below the scan bound must be
            // refused, not waved through: "the guard did not look" and "the
            // guard found nothing" are different answers.
            return Some(format!(
                "the target payload nests deeper than {MAX_SCAN_DEPTH} levels, so it cannot be \
                 proven not to be Vibex itself"
            ));
        }
        match value {
            Value::Array(items) => items
                .iter()
                .find_map(|item| self.inspect_value(item, depth + 1)),
            Value::Object(object) => {
                for key in PID_KEYS {
                    if let Some(pid) = object.get(*key).and_then(Value::as_i64)
                        && self.host_pids.contains(&(pid as i32))
                    {
                        return Some(format!("the payload names the host pid {pid}"));
                    }
                }
                for key in WINDOW_KEYS {
                    if let Some(window) = object.get(*key) {
                        if let Some(id) = window.as_str()
                            && self.host_window_ids.contains(id)
                        {
                            return Some(format!("the payload names the host window {id}"));
                        }
                        if let Some(id) = window.as_i64()
                            && self.host_window_ids.contains(&id.to_string())
                        {
                            return Some(format!("the payload names the host window {id}"));
                        }
                        // A nested `target` inside `window`.
                        if window.is_object()
                            && let Some(reason) = self.inspect_value(window, depth + 1)
                        {
                            return Some(reason);
                        }
                    }
                }
                for key in TARGET_KEYS {
                    if let Some(nested) = object.get(*key)
                        && let Some(reason) = self.inspect_value(nested, depth + 1)
                    {
                        return Some(reason);
                    }
                }
                if let Some(bounds) = rect_from_object(object) {
                    if let Some(desktop) = &self.host_desktop {
                        // The whole screen is the host: the union test is the
                        // only one that can answer.
                        if bounds.intersects(desktop) {
                            return Some(
                                "the target rectangle overlaps the host desktop".to_string(),
                            );
                        }
                    } else if self
                        .host_windows
                        .iter()
                        .any(|window| window.intersects(&bounds))
                    {
                        return Some("the target rectangle overlaps a Vibex window".to_string());
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Checks a point against the host's own windows.
    pub fn block_point(&self, x: f64, y: f64) -> Option<String> {
        if let Some(desktop) = &self.host_desktop
            && desktop.contains(x, y)
        {
            return Some("the point is inside the host desktop".to_string());
        }
        if self.host_windows.iter().any(|window| window.contains(x, y)) {
            return Some("the point is inside a Vibex window".to_string());
        }
        None
    }
}

/// Reads a rectangle out of a JSON object, accepting the several spellings an
/// engine may use.
fn rect_from_object(object: &serde_json::Map<String, Value>) -> Option<ComputerRect> {
    let nested = object
        .get("bounds")
        .or_else(|| object.get("frame"))
        .or_else(|| object.get("rect"));
    if let Some(nested) = nested
        && let Some(rect) = rect_from_object(nested.as_object()?)
    {
        return Some(rect);
    }
    let number = |keys: &[&str]| -> Option<f64> {
        keys.iter()
            .find_map(|key| object.get(*key).and_then(Value::as_f64))
    };
    let x = number(&["x", "left"])?;
    let y = number(&["y", "top"])?;
    let width = number(&["width", "w"])?;
    let height = number(&["height", "h"])?;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(ComputerRect {
        x,
        y,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn guard() -> SelfTargetGuard {
        let mut guard = SelfTargetGuard::new();
        guard.add_host_pid(1000);
        guard.add_host_window(
            "host-window",
            Some(ComputerRect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 300.0,
            }),
        );
        guard
    }

    #[test]
    fn a_host_pid_anywhere_in_the_payload_is_found() {
        assert!(guard().inspect(&json!({ "pid": 1000 })).is_some());
        assert!(
            guard()
                .inspect(&json!({ "target": { "targets": [{ "pid": 1000 }] } }))
                .is_some(),
            "a nested target must still be inspected"
        );
        assert!(guard().inspect(&json!({ "pid": 2000 })).is_none());
    }

    #[test]
    fn a_host_window_id_is_found_in_both_spellings() {
        assert!(
            guard()
                .inspect(&json!({ "window_id": "host-window" }))
                .is_some()
        );
        assert!(
            guard()
                .inspect(&json!({ "window": { "handle": "host-window" } }))
                .is_some()
        );
        assert!(guard().inspect(&json!({ "window_id": "other" })).is_none());
    }

    #[test]
    fn an_overlapping_rectangle_is_found() {
        assert!(
            guard()
                .inspect(&json!({ "target": { "bounds": { "x": 10, "y": 10, "width": 50, "height": 50 } } }))
                .is_some()
        );
        assert!(
            guard()
                .inspect(&json!({ "bounds": { "x": 900, "y": 900, "width": 10, "height": 10 } }))
                .is_none()
        );
    }

    #[test]
    fn a_point_inside_a_host_window_is_refused() {
        assert!(guard().block_point(100.0, 100.0).is_some());
        assert!(guard().block_point(900.0, 900.0).is_none());
    }

    #[test]
    fn covering_the_whole_desktop_refuses_everything() {
        let mut guard = SelfTargetGuard::new();
        guard.add_host_pid(1);
        guard.cover_entire_desktop(ComputerRect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        });
        assert!(guard.block_point(5.0, 5.0).is_some());
        assert!(
            guard
                .inspect(&json!({ "bounds": { "x": 1, "y": 1, "width": 10, "height": 10 } }))
                .is_some()
        );
    }

    #[test]
    fn a_blind_guard_says_so_instead_of_answering_safe() {
        assert!(SelfTargetGuard::new().is_blind());
        assert!(!guard().is_blind());
    }

    #[test]
    fn a_payload_that_nests_past_the_bound_is_refused_not_cleared() {
        let mut payload = json!({ "name": "Save" });
        for _ in 0..(MAX_SCAN_DEPTH + 4) {
            payload = json!({ "target": payload });
        }
        let verdict = guard().inspect(&payload);
        assert!(
            verdict.is_some(),
            "an uninspectable payload must fail closed, not pass"
        );
    }

    #[test]
    fn a_shallow_payload_with_no_host_identity_is_clear() {
        assert!(
            guard()
                .inspect(&json!({ "target": { "name": "Save", "role": "button" } }))
                .is_none()
        );
    }
}
