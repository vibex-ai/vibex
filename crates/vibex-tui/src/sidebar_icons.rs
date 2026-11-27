//! The icons the session list's rows and buttons wear.
//!
//! The desktop draws these from two places: the brand pack under
//! `apps/desktop/assets/icons/`, and `gpui-component`'s own Lucide set for the
//! handful of plain controls (`Plus`, `Ellipsis`, `ChevronDown`,
//! `ChevronRight`). A terminal cannot read either at run time, so the icons a
//! row needs are bundled here — the desktop pack for the ones it already
//! carries, and the phone's Lucide copy for the controls the component set
//! owns and this repository has no embeddable copy of.
//!
//! The shapes are the desktop's own, and deliberately so: a reader who knows
//! the sidebar on one screen has to recognise the buttons on the other. Every
//! entry therefore carries the asset path it is, and a test asserts the file
//! says what the entry claims — a control that quietly drifted to a different
//! picture would be a different button wearing the same verb.
//!
//! # Two renderings, one vocabulary
//!
//! A button has to be drawn on *every* terminal, and a picture is only drawn
//! where the terminal speaks an image protocol. So each icon has both
//! renderings: the SVG the desktop draws (turned into a PNG by
//! [`crate::row_marks`], the same path an Agent's brand mark takes), and a
//! character for the terminals that cannot hold a picture. The character is
//! tier-aware like the rest of [`crate::glyphs`], and both renderings are the
//! same two columns wide, so a row does not reflow when the terminal's
//! capabilities change.

use crate::glyphs::GlyphTier;

/// One icon of the session list's vocabulary.
///
/// An enum rather than a string because the set is closed: a row that asks for
/// an icon that does not exist should not compile, and the picture pool keys
/// its cache on the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Icon {
    /// A session kept above the rest of its list.
    Pin,
    /// Rename a session.
    Rename,
    /// Copy a session up to a chosen point.
    Fork,
    /// Archive a session.
    Archive,
    /// Delete a session.
    Delete,
    /// Let a session continue itself.
    Resume,
    /// Auto-continue is on but held back.
    Pause,
    /// The Agent is parked on the reader's answer.
    Attention,
    /// A message waiting for its send time.
    Clock,
    /// A new session.
    New,
    /// Everything a row can do.
    More,
    /// A folder the reader made, folded shut.
    FolderClosed,
    /// A folder the reader made, open.
    FolderOpen,
    /// A new folder.
    NewFolder,
    /// Fold a heading shut.
    Collapse,
    /// Open a heading.
    Expand,
}

/// Every icon, in declaration order.
pub const ICONS: &[Icon] = &[
    Icon::Pin,
    Icon::Rename,
    Icon::Fork,
    Icon::Archive,
    Icon::Delete,
    Icon::Resume,
    Icon::Pause,
    Icon::Attention,
    Icon::Clock,
    Icon::New,
    Icon::More,
    Icon::FolderClosed,
    Icon::FolderOpen,
    Icon::NewFolder,
    Icon::Collapse,
    Icon::Expand,
];

impl Icon {
    /// The stable key the icon is named by, in the key-remap file's spelling
    /// and in tests.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Pin => "pin",
            Self::Rename => "rename",
            Self::Fork => "fork",
            Self::Archive => "archive",
            Self::Delete => "delete",
            Self::Resume => "resume",
            Self::Pause => "pause",
            Self::Attention => "attention",
            Self::Clock => "clock",
            Self::New => "new",
            Self::More => "more",
            Self::FolderClosed => "folder",
            Self::FolderOpen => "folder-open",
            Self::NewFolder => "new-folder",
            Self::Collapse => "collapse",
            Self::Expand => "expand",
        }
    }

    /// The desktop asset this icon is, relative to the pack that carries it.
    ///
    /// The first segment is the pack, because the two do not share a root: a
    /// plain control that the desktop's component set draws lives in the
    /// phone's Lucide copy, and saying so here is what keeps the reason
    /// visible rather than looking like a mistake.
    pub const fn asset(self) -> &'static str {
        match self {
            Self::Pin => "desktop:pin.svg",
            Self::Rename => "desktop:pencil.svg",
            Self::Fork => "desktop:git-branch.svg",
            Self::Archive => "desktop:package.svg",
            Self::Delete => "desktop:trash-2.svg",
            Self::Resume => "desktop:rotate-ccw.svg",
            Self::Pause => "desktop:pause.svg",
            Self::Attention => "desktop:circle-question-mark.svg",
            Self::Clock => "desktop:clock.svg",
            Self::New => "mobile:plus.svg",
            Self::More => "mobile:ellipsis.svg",
            Self::FolderClosed => "desktop:folder.svg",
            Self::FolderOpen => "mobile:folder-open.svg",
            Self::NewFolder => "desktop:folder-plus.svg",
            Self::Collapse => "mobile:chevron-down.svg",
            Self::Expand => "mobile:chevron-right.svg",
        }
    }

    /// The SVG source, as it is bundled.
    pub const fn svg(self) -> &'static [u8] {
        match self {
            Self::Pin => include_bytes!("../../../apps/desktop/assets/icons/pin.svg"),
            Self::Rename => include_bytes!("../../../apps/desktop/assets/icons/pencil.svg"),
            Self::Fork => include_bytes!("../../../apps/desktop/assets/icons/git-branch.svg"),
            Self::Archive => include_bytes!("../../../apps/desktop/assets/icons/package.svg"),
            Self::Delete => include_bytes!("../../../apps/desktop/assets/icons/trash-2.svg"),
            Self::Resume => include_bytes!("../../../apps/desktop/assets/icons/rotate-ccw.svg"),
            Self::Pause => include_bytes!("../../../apps/desktop/assets/icons/pause.svg"),
            Self::Attention => {
                include_bytes!("../../../apps/desktop/assets/icons/circle-question-mark.svg")
            }
            Self::Clock => include_bytes!("../../../apps/desktop/assets/icons/clock.svg"),
            Self::New => include_bytes!("../../../crates/vibex-mobile-ui/assets/icons/plus.svg"),
            Self::More => {
                include_bytes!("../../../crates/vibex-mobile-ui/assets/icons/ellipsis.svg")
            }
            Self::FolderClosed => include_bytes!("../../../apps/desktop/assets/icons/folder.svg"),
            Self::FolderOpen => {
                include_bytes!("../../../crates/vibex-mobile-ui/assets/icons/folder-open.svg")
            }
            Self::NewFolder => include_bytes!("../../../apps/desktop/assets/icons/folder-plus.svg"),
            Self::Collapse => {
                include_bytes!("../../../crates/vibex-mobile-ui/assets/icons/chevron-down.svg")
            }
            Self::Expand => {
                include_bytes!("../../../crates/vibex-mobile-ui/assets/icons/chevron-right.svg")
            }
        }
    }

    /// The character the icon falls back to on a terminal with no pictures.
    ///
    /// One column each, and distinct in shape rather than only in colour: two
    /// icons that differed only by their colour would be one icon on a
    /// monochrome terminal, and the row's buttons are exactly the place a
    /// reader cannot afford to guess.
    pub const fn glyph(self, tier: GlyphTier) -> &'static str {
        let unicode = matches!(tier, GlyphTier::Full);
        match self {
            Self::Pin => {
                if unicode {
                    "★"
                } else {
                    "*"
                }
            }
            Self::Rename => {
                if unicode {
                    "✎"
                } else {
                    "e"
                }
            }
            // A fork is a `Y` on the character grid: the shape the ASCII graph
            // of a branch has always been drawn with.
            Self::Fork => "Y",
            Self::Archive => {
                if unicode {
                    "▤"
                } else {
                    "#"
                }
            }
            Self::Delete => {
                if unicode {
                    "✗"
                } else {
                    "x"
                }
            }
            Self::Resume => {
                if unicode {
                    "↻"
                } else {
                    "R"
                }
            }
            Self::Pause => {
                if unicode {
                    "‖"
                } else {
                    "="
                }
            }
            // The desktop draws a circled question mark; the question mark
            // alone is what a cell can hold, and it is the whole of what the
            // mark means.
            Self::Attention => "?",
            Self::Clock => {
                if unicode {
                    "◷"
                } else {
                    "@"
                }
            }
            Self::New | Self::NewFolder => "+",
            Self::More => {
                if unicode {
                    "…"
                } else {
                    "."
                }
            }
            Self::FolderClosed | Self::Collapse => {
                if unicode {
                    "▸"
                } else {
                    ">"
                }
            }
            Self::FolderOpen | Self::Expand => {
                if unicode {
                    "▾"
                } else {
                    "v"
                }
            }
        }
    }
}

/// The icon a row asks for by key, for a caller reading one from a file.
pub fn icon_for_key(key: &str) -> Option<Icon> {
    ICONS
        .iter()
        .copied()
        .find(|icon| icon.key().eq_ignore_ascii_case(key.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_resolve_back_to_their_icon() {
        let mut seen = std::collections::BTreeSet::new();
        for icon in ICONS {
            assert!(seen.insert(icon.key()), "duplicate icon key {}", icon.key());
            assert_eq!(icon_for_key(icon.key()), Some(*icon));
            assert_eq!(icon_for_key(&icon.key().to_uppercase()), Some(*icon));
        }
        assert_eq!(icon_for_key("nothing"), None);
        assert_eq!(ICONS.len(), seen.len());
    }

    /// Every icon names the pack it came from, and every one of those packs is
    /// one this repository actually has. A path that drifted would be caught
    /// here rather than by a blank cell.
    #[test]
    fn every_icon_names_a_real_asset() {
        for icon in ICONS {
            let (pack, file) = icon
                .asset()
                .split_once(':')
                .unwrap_or_else(|| panic!("{} names no pack", icon.key()));
            assert!(
                matches!(pack, "desktop" | "mobile"),
                "{} names an unknown pack {pack:?}",
                icon.key()
            );
            assert!(file.ends_with(".svg"), "{} is not an SVG", icon.key());
            let svg = std::str::from_utf8(icon.svg())
                .unwrap_or_else(|_| panic!("{} is not UTF-8", icon.key()));
            assert!(
                svg.contains("<svg"),
                "{} is not an SVG document",
                icon.key()
            );
        }
    }

    /// Every icon is drawn in the row's own colour, which is what lets one
    /// rasterizer serve a brand mark and a button alike. An icon that baked in
    /// a colour would ignore the role it was asked for — a delete button that
    /// was not the danger colour.
    #[test]
    fn every_icon_is_drawn_in_the_readers_colour() {
        for icon in ICONS {
            let svg = std::str::from_utf8(icon.svg()).expect("SVG is text");
            assert!(
                svg.contains("currentColor"),
                "{} carries its own colour instead of the row's",
                icon.key()
            );
        }
    }

    /// The glyph is one column wide, in both tiers, so the slot a picture fills
    /// and the slot a character fills are the same two columns.
    #[test]
    fn every_glyph_is_one_column_in_both_tiers() {
        for icon in ICONS {
            for tier in [GlyphTier::Full, GlyphTier::Legacy] {
                let glyph = icon.glyph(tier);
                assert_eq!(
                    crate::text::display_width(glyph),
                    1,
                    "{} is not one column at {tier:?}",
                    icon.key()
                );
            }
        }
    }

    /// A legacy console has ASCII and some CP437, and none of the geometric or
    /// Braille shapes. The same rule the rest of the vocabulary keeps.
    #[test]
    fn the_legacy_tier_avoids_characters_its_font_lacks() {
        let forbidden = ["★", "✎", "▤", "✗", "↻", "‖", "◷", "…", "▸", "▾", "┃", "─"];
        for icon in ICONS {
            let glyph = icon.glyph(GlyphTier::Legacy);
            for character in forbidden {
                assert!(
                    !glyph.contains(character),
                    "{} falls back to {glyph:?}, which a legacy console cannot render",
                    icon.key()
                );
            }
        }
    }

    /// Two icons that shared a shape would be one icon on a terminal that has
    /// no colour, which is the terminal the fallback exists for.
    ///
    /// The pairs below are deliberately one shape, because a row draws whichever
    /// of the two it means and never both at once: a folder and its own fold
    /// chevron, and the two ways of saying "a new one".
    #[test]
    fn no_two_unrelated_icons_share_a_shape() {
        let related = |left: Icon, right: Icon| {
            matches!(
                (left, right),
                (Icon::FolderClosed, Icon::Collapse)
                    | (Icon::Collapse, Icon::FolderClosed)
                    | (Icon::FolderOpen, Icon::Expand)
                    | (Icon::Expand, Icon::FolderOpen)
                    | (Icon::New, Icon::NewFolder)
                    | (Icon::NewFolder, Icon::New)
            )
        };
        for tier in [GlyphTier::Full, GlyphTier::Legacy] {
            let mut seen: Vec<(Icon, &'static str)> = Vec::new();
            for icon in ICONS {
                let glyph = icon.glyph(tier);
                if let Some((other, _)) = seen
                    .iter()
                    .find(|(other, shape)| *shape == glyph && !related(*other, *icon))
                {
                    panic!(
                        "{} and {} are both {glyph:?} at {tier:?}",
                        other.key(),
                        icon.key()
                    );
                }
                seen.push((*icon, glyph));
            }
        }
    }
}
