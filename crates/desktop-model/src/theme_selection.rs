//! The user's independent light and dark theme choices.
//!
//! A selection stores one theme id per appearance. Keeping the two slots
//! independent is what lets a user pair, say, a warm light palette with a cool
//! dark one: switching the OS appearance changes which slot is read, never
//! which theme the other slot points at.
//!
//! This crate stays framework-neutral, so the type stores opaque ids and does
//! not know which ids exist. `vibex_ui::theme_catalog` owns resolution against
//! the generated catalog and decides what an unknown or absent id falls back
//! to.

use serde::{Deserialize, Serialize};

/// Independent light and dark theme selections.
///
/// `None` means "no explicit choice", which resolvers treat as the catalog's
/// default for that appearance. Persisted state predating this field therefore
/// loads as the product defaults rather than as an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<String>,
}

impl ThemeSelection {
    /// The explicitly selected light theme, if any.
    pub fn light(&self) -> Option<&str> {
        self.light.as_deref()
    }

    /// The explicitly selected dark theme, if any.
    pub fn dark(&self) -> Option<&str> {
        self.dark.as_deref()
    }

    /// Record a light theme choice.
    pub fn select_light(&mut self, id: impl Into<String>) {
        self.light = Some(id.into());
    }

    /// Record a dark theme choice.
    pub fn select_dark(&mut self, id: impl Into<String>) {
        self.dark = Some(id.into());
    }

    /// Whether either slot names `id`.
    ///
    /// Used to describe a theme row's state without caring which appearance
    /// slot happens to hold it.
    pub fn contains(&self, id: &str) -> bool {
        self.light() == Some(id) || self.dark() == Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_independent() {
        let mut selection = ThemeSelection::default();
        selection.select_light("warm-light");
        selection.select_dark("cool-dark");
        assert_eq!(selection.light(), Some("warm-light"));
        assert_eq!(selection.dark(), Some("cool-dark"));

        selection.select_light("other-light");
        assert_eq!(
            selection.dark(),
            Some("cool-dark"),
            "the dark slot is untouched"
        );
    }

    #[test]
    fn absent_state_loads_as_no_choice() {
        let loaded: ThemeSelection = serde_json::from_str("{}").unwrap();
        assert_eq!(loaded, ThemeSelection::default());
        assert_eq!(loaded.light(), None);
    }

    #[test]
    fn unset_slots_are_not_serialized() {
        let mut selection = ThemeSelection::default();
        selection.select_dark("nord");
        assert_eq!(
            serde_json::to_string(&selection).unwrap(),
            r#"{"dark":"nord"}"#
        );
    }
}
