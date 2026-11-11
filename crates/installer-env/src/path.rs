//! Pure `Path` editing rules.
//!
//! A Windows `Path` is a semicolon-separated list of directories. Editing it
//! looks trivial until the details decide whether a user loses an entry:
//!
//! * matching is whole-entry and case-insensitive, so `C:\VibexTools` never
//!   matches `C:\Vibex` and `c:\vibex` matches `C:\Vibex`;
//! * a trailing separator and surrounding quotes do not make a different
//!   directory;
//! * empty entries are preserved, because they have a meaning (`Path` splits
//!   are also used with an empty entry standing for the working directory);
//! * a removal that empties the value reports `DeleteValue` instead of writing
//!   `""`, because Windows would then search only the working directory.
//!
//! The functions are deliberately independent of the registry and of Windows
//! so the rules are covered by ordinary unit tests on every CI host.

/// What removing a directory would do to the stored value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// The directory is not in the value.
    Unchanged,
    /// The directory was the only real entry; delete the value so Windows falls
    /// back to the system `Path` instead of searching only the working
    /// directory.
    DeleteValue,
    /// The remaining value, byte for byte except for the removed entries.
    Remaining(String),
}

/// Append `directory` to `current` unless it is already an entry.
///
/// `current` is `None` when the value does not exist at all, which is different
/// from an empty value only in that the caller must create it. `None` is also
/// returned when the value is already correct, so the caller can skip the write
/// (and the environment broadcast).
pub fn append(current: Option<&str>, directory: &str) -> Option<String> {
    let directory = directory.trim();
    if directory.is_empty() || directory.contains(';') {
        return None;
    }

    let Some(value) = current else {
        return Some(directory.to_owned());
    };
    if value
        .split(';')
        .any(|entry| same_directory(entry, directory))
    {
        return None;
    }
    if value.is_empty() {
        return Some(directory.to_owned());
    }
    // A trailing separator already separates the new entry; adding another
    // would create an empty entry.
    if value.ends_with(';') {
        Some(format!("{value}{directory}"))
    } else {
        Some(format!("{value};{directory}"))
    }
}

/// Remove every entry that names `directory`.
///
/// Only whole entries are compared. A `Path` another installer or the user
/// wrote is otherwise preserved exactly, including its order, spelling and
/// empty entries.
pub fn remove(current: &str, directory: &str) -> Removal {
    let directory = directory.trim();
    if current.is_empty() || directory.is_empty() || directory.contains(';') {
        return Removal::Unchanged;
    }

    let mut matched = false;
    let mut remaining = Vec::new();
    for entry in current.split(';') {
        // An empty entry is data, not a candidate for `directory`, even when
        // the caller passes an empty directory.
        if !entry.trim().is_empty() && same_directory(entry, directory) {
            matched = true;
        } else {
            remaining.push(entry);
        }
    }

    if !matched {
        return Removal::Unchanged;
    }
    if remaining.iter().all(|entry| entry.trim().is_empty()) {
        return Removal::DeleteValue;
    }
    Removal::Remaining(remaining.join(";"))
}

/// Compare two `Path` entries as directories rather than as strings.
fn same_directory(entry: &str, directory: &str) -> bool {
    let entry = normalize(entry);
    let directory = normalize(directory);
    if entry.is_empty() || directory.is_empty() {
        return false;
    }

    let mut left = entry.chars();
    let mut right = directory.chars();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(left), Some(right)) if same_character(left, right) => {}
            _ => return false,
        }
    }
}

/// Strip the decoration `Path` tolerates but does not consider part of the
/// directory name.
fn normalize(value: &str) -> &str {
    let mut value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value = &value[1..value.len() - 1];
    }
    // Keep `C:\` intact: trimming it to `C:` would name a drive-relative path.
    while value.len() > 3 && (value.ends_with('\\') || value.ends_with('/')) {
        value = &value[..value.len() - 1];
    }
    value
}

fn same_character(left: char, right: char) -> bool {
    let left = if left == '/' { '\\' } else { left };
    let right = if right == '/' { '\\' } else { right };
    left.eq_ignore_ascii_case(&right)
}

#[cfg(test)]
mod tests {
    use super::{Removal, append, remove};

    #[test]
    fn append_creates_an_absent_value() {
        assert_eq!(append(None, r"C:\Vibex"), Some(r"C:\Vibex".to_owned()));
        assert_eq!(append(Some(""), r"C:\Vibex"), Some(r"C:\Vibex".to_owned()));
    }

    #[test]
    fn append_keeps_every_existing_entry() {
        let current = r"%USERPROFILE%\bin;C:\Tools;C:\Program Files\Git\cmd";
        assert_eq!(
            append(Some(current), r"C:\Vibex"),
            Some(format!(r"{current};C:\Vibex"))
        );
    }

    #[test]
    fn append_respects_an_existing_trailing_separator() {
        assert_eq!(
            append(Some(r"C:\Tools;"), r"C:\Vibex"),
            Some(r"C:\Tools;C:\Vibex".to_owned())
        );
    }

    #[test]
    fn append_is_idempotent_and_case_insensitive() {
        assert_eq!(append(Some(r"C:\Tools;c:\vibex"), r"C:\Vibex"), None);
        assert_eq!(append(Some(r"C:\Tools;C:\Vibex\"), r"C:\Vibex"), None);
        assert_eq!(append(Some(r#""C:\Vibex";C:\Tools"#), r"C:\Vibex"), None);
        assert_eq!(append(Some(r"C:/Vibex;C:\Tools"), r"C:\Vibex"), None);
    }

    #[test]
    fn append_does_not_match_a_prefix() {
        assert_eq!(
            append(Some(r"C:\VibexTools"), r"C:\Vibex"),
            Some(r"C:\VibexTools;C:\Vibex".to_owned())
        );
    }

    #[test]
    fn append_preserves_a_path_longer_than_the_nsis_limit() {
        let current = (0..100)
            .map(|index| format!(r"C:\tools\entry-{index:03}-with-a-long-name"))
            .collect::<Vec<_>>()
            .join(";");
        assert!(
            current.len() > 1500,
            "test path must exceed 1023 characters"
        );

        let updated = append(Some(&current), r"C:\Vibex").expect("the entry is new");
        assert!(updated.starts_with(&current));
        assert_eq!(updated, format!(r"{current};C:\Vibex"));
        assert_eq!(remove(&updated, r"C:\Vibex"), Removal::Remaining(current));
    }

    #[test]
    fn remove_takes_out_only_the_named_entry() {
        assert_eq!(
            remove(r"C:\Tools;C:\Vibex;C:\Program Files\Git\cmd", r"C:\Vibex"),
            Removal::Remaining(r"C:\Tools;C:\Program Files\Git\cmd".to_owned())
        );
        assert_eq!(
            remove(r"C:\Tools;C:\VibexTools", r"C:\Vibex"),
            Removal::Unchanged
        );
        assert_eq!(remove(r"C:\Tools", r"C:\Vibex"), Removal::Unchanged);
    }

    #[test]
    fn remove_matches_case_and_decoration() {
        assert_eq!(remove(r"c:\vibex", r"C:\Vibex"), Removal::DeleteValue);
        assert_eq!(remove(r"C:\Vibex\", r"C:\Vibex"), Removal::DeleteValue);
        assert_eq!(remove(r#""C:\Vibex""#, r"C:\Vibex"), Removal::DeleteValue);
        assert_eq!(remove(r"C:/Vibex", r"C:\Vibex"), Removal::DeleteValue);
    }

    #[test]
    fn remove_keeps_empty_entries_until_the_value_is_empty() {
        assert_eq!(
            remove(r";C:\Vibex;C:\Tools", r"C:\Vibex"),
            Removal::Remaining(r";C:\Tools".to_owned())
        );
        assert_eq!(
            remove(r"C:\Vibex;", r"C:\Vibex"),
            Removal::DeleteValue,
            "a trailing separator is not a remaining directory"
        );
        assert_eq!(
            remove(r"C:\Vibex", r"C:\Vibex"),
            Removal::DeleteValue,
            "the value must be deleted, not written empty"
        );
    }

    #[test]
    fn remove_every_duplicate() {
        assert_eq!(
            remove(r"C:\Vibex;C:\Tools;C:\Vibex", r"C:\Vibex"),
            Removal::Remaining(r"C:\Tools".to_owned())
        );
    }

    #[test]
    fn remove_ignores_an_empty_directory() {
        assert_eq!(remove(r"C:\Tools", ""), Removal::Unchanged);
        assert_eq!(remove("", r"C:\Vibex"), Removal::Unchanged);
    }

    #[test]
    fn append_and_remove_preserve_expandable_references() {
        let current = r"%USERPROFILE%\bin;%APPDATA%\npm";
        let updated = append(Some(current), r"C:\Vibex").expect("the entry is new");
        assert_eq!(updated, r"%USERPROFILE%\bin;%APPDATA%\npm;C:\Vibex");
        assert_eq!(
            remove(&updated, r"C:\Vibex"),
            Removal::Remaining(current.to_owned())
        );
    }

    #[test]
    fn a_drive_root_is_not_a_drive_relative_path() {
        assert_eq!(append(Some(r"C:\"), r"C:\"), None);
        assert_eq!(
            append(Some(r"C:"), r"C:\"),
            Some(r"C:;C:\".to_owned()),
            "C: and C:\\ are different directories"
        );
    }
}
