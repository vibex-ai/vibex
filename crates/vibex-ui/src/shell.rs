use serde::{Deserialize, Serialize};

use crate::MIN_TOUCH_TARGET_PX;

pub const ABSOLUTE_MIN_WIDTH: u32 = 360;
pub const ABSOLUTE_MIN_HEIGHT: u32 = 620;
pub const WIDE_MIN_WIDTH: f32 = 1_100.0;
pub const MEDIUM_MIN_WIDTH: f32 = 760.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    Wide,
    Medium,
    Compact,
}

impl ShellKind {
    pub fn from_width(width: f32) -> Self {
        if width >= WIDE_MIN_WIDTH {
            Self::Wide
        } else if width >= MEDIUM_MIN_WIDTH {
            Self::Medium
        } else {
            Self::Compact
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Wide => "Wide",
            Self::Medium => "Medium",
            Self::Compact => "Compact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelPresentation {
    Docked,
    Overlay,
    Drawer,
    Sheet,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellContentMinimums {
    pub wide: f32,
    pub medium: f32,
    pub primary: f32,
}

impl Default for ShellContentMinimums {
    fn default() -> Self {
        Self {
            wide: WIDE_MIN_WIDTH,
            medium: MEDIUM_MIN_WIDTH,
            primary: ABSOLUTE_MIN_WIDTH as f32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellLayout {
    pub kind: ShellKind,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub sidebar: PanelPresentation,
    pub preview: PanelPresentation,
    pub right_rail: PanelPresentation,
    pub sidebar_width: f32,
    pub preview_min_width: f32,
    pub right_rail_width: f32,
    pub primary_min_width: f32,
    pub compact_title_bar: bool,
}

impl ShellLayout {
    pub fn resolve(width: u32, height: u32) -> Self {
        Self::resolve_for_content(width, height, ShellContentMinimums::default())
    }

    pub fn resolve_for_content(width: u32, height: u32, minimums: ShellContentMinimums) -> Self {
        let viewport_width = width.max(ABSOLUTE_MIN_WIDTH);
        let viewport_height = height.max(ABSOLUTE_MIN_HEIGHT);
        let width = viewport_width as f32;
        let wide_min = minimums.wide.max(WIDE_MIN_WIDTH);
        let medium_min = minimums.medium.max(MEDIUM_MIN_WIDTH).min(wide_min);
        let kind = if width >= wide_min {
            ShellKind::Wide
        } else if width >= medium_min {
            ShellKind::Medium
        } else {
            ShellKind::Compact
        };
        let (sidebar, preview, right_rail, sidebar_width, preview_min_width, right_rail_width) =
            match kind {
                ShellKind::Wide if viewport_width >= 1_600 => (
                    PanelPresentation::Docked,
                    PanelPresentation::Docked,
                    PanelPresentation::Docked,
                    280.0,
                    520.0,
                    360.0,
                ),
                ShellKind::Wide if viewport_width >= 1_440 => (
                    PanelPresentation::Docked,
                    PanelPresentation::Docked,
                    PanelPresentation::Docked,
                    260.0,
                    460.0,
                    320.0,
                ),
                ShellKind::Wide => (
                    PanelPresentation::Docked,
                    PanelPresentation::Docked,
                    PanelPresentation::Overlay,
                    256.0,
                    460.0,
                    340.0,
                ),
                ShellKind::Medium if viewport_width >= 900 => (
                    PanelPresentation::Docked,
                    PanelPresentation::Docked,
                    PanelPresentation::Drawer,
                    220.0,
                    300.0,
                    320.0,
                ),
                ShellKind::Medium => (
                    PanelPresentation::Drawer,
                    PanelPresentation::Docked,
                    PanelPresentation::Drawer,
                    280.0,
                    360.0,
                    320.0,
                ),
                ShellKind::Compact => (
                    PanelPresentation::Drawer,
                    PanelPresentation::Sheet,
                    PanelPresentation::Sheet,
                    (width - 32.0).clamp(280.0, 340.0),
                    (width - 32.0).max(280.0),
                    (width - 32.0).clamp(280.0, 340.0),
                ),
            };
        Self {
            kind,
            viewport_width,
            viewport_height,
            sidebar,
            preview,
            right_rail,
            sidebar_width,
            preview_min_width,
            right_rail_width,
            primary_min_width: minimums.primary.max(280.0),
            compact_title_bar: kind != ShellKind::Wide,
        }
    }

    pub fn docked_minimum_width(&self) -> f32 {
        self.primary_min_width
            + if self.sidebar == PanelPresentation::Docked {
                self.sidebar_width
            } else {
                0.0
            }
            + if self.preview == PanelPresentation::Docked {
                self.preview_min_width
            } else {
                0.0
            }
            + if self.right_rail == PanelPresentation::Docked {
                self.right_rail_width
            } else {
                0.0
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WideShell {
    pub layout: ShellLayout,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediumShell {
    pub layout: ShellLayout,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactShell {
    pub layout: ShellLayout,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AdaptiveShell {
    Wide(WideShell),
    Medium(MediumShell),
    Compact(CompactShell),
}

impl AdaptiveShell {
    pub fn resolve(width: u32, height: u32) -> Self {
        let layout = ShellLayout::resolve(width, height);
        match layout.kind {
            ShellKind::Wide => Self::Wide(WideShell { layout }),
            ShellKind::Medium => Self::Medium(MediumShell { layout }),
            ShellKind::Compact => Self::Compact(CompactShell { layout }),
        }
    }

    pub fn layout(self) -> ShellLayout {
        match self {
            Self::Wide(shell) => shell.layout,
            Self::Medium(shell) => shell.layout,
            Self::Compact(shell) => shell.layout,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalDestination {
    Sessions,
    Management,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDestination {
    Agent,
    Files,
    Changes,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavigationLevel {
    Global,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlaySemantic {
    Popover,
    Dialog,
    Inspector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayState {
    pub semantic: OverlaySemantic,
    pub presentation: PanelPresentation,
    pub restore_focus: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationAction {
    pub touch_target_px: u16,
    pub hover_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactNavigation {
    pub level: NavigationLevel,
    pub global: GlobalDestination,
    pub session: SessionDestination,
    pub session_id: Option<String>,
    pub overlay: Option<OverlayState>,
    pub focus_target: Option<String>,
}

impl Default for CompactNavigation {
    fn default() -> Self {
        Self {
            level: NavigationLevel::Global,
            global: GlobalDestination::Sessions,
            session: SessionDestination::Agent,
            session_id: None,
            overlay: None,
            focus_target: None,
        }
    }
}

impl CompactNavigation {
    pub fn enter_session(&mut self, session_id: impl Into<String>) {
        self.level = NavigationLevel::Session;
        self.global = GlobalDestination::Sessions;
        self.session_id = Some(session_id.into());
        self.session = SessionDestination::Agent;
    }

    pub fn select_global(&mut self, destination: GlobalDestination) {
        self.level = NavigationLevel::Global;
        self.global = destination;
        self.session_id = None;
        self.overlay = None;
    }

    pub fn select_session(&mut self, destination: SessionDestination) {
        self.session = destination;
        if self.session_id.is_some() {
            self.level = NavigationLevel::Session;
        } else {
            self.level = NavigationLevel::Global;
            self.global = GlobalDestination::Sessions;
            self.overlay = None;
        }
    }

    pub fn open_overlay(
        &mut self,
        semantic: OverlaySemantic,
        shell: ShellKind,
        restore_focus: impl Into<String>,
    ) {
        let presentation = match shell {
            ShellKind::Compact => PanelPresentation::Sheet,
            ShellKind::Medium => PanelPresentation::Drawer,
            ShellKind::Wide => PanelPresentation::Overlay,
        };
        self.overlay = Some(OverlayState {
            semantic,
            presentation,
            restore_focus: Some(restore_focus.into()),
        });
    }

    pub fn close_overlay(&mut self) -> Option<String> {
        let overlay = self.overlay.take()?;
        self.focus_target = overlay.restore_focus.clone();
        overlay.restore_focus
    }

    pub fn back(&mut self) -> bool {
        if self.overlay.is_some() {
            self.close_overlay();
            return true;
        }
        if self.level == NavigationLevel::Session {
            self.level = NavigationLevel::Global;
            self.global = GlobalDestination::Sessions;
            self.session_id = None;
            return true;
        }
        false
    }

    pub fn actions(&self) -> Vec<NavigationAction> {
        let count = if self.level == NavigationLevel::Session {
            4
        } else {
            3
        };
        (0..count)
            .map(|_| NavigationAction {
                touch_target_px: MIN_TOUCH_TARGET_PX,
                hover_required: false,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_viewports_select_the_expected_shell_without_user_agent_input() {
        let cases = [
            (360, 800, ShellKind::Compact),
            (390, 844, ShellKind::Compact),
            (768, 1_024, ShellKind::Medium),
            (1_200, 800, ShellKind::Wide),
            (1_440, 900, ShellKind::Wide),
            (360, 620, ShellKind::Compact),
        ];
        for (width, height, expected) in cases {
            let layout = AdaptiveShell::resolve(width, height).layout();
            assert_eq!(layout.kind, expected);
            assert!(layout.docked_minimum_width() <= layout.viewport_width as f32);
            assert!(layout.viewport_height >= ABSOLUTE_MIN_HEIGHT);
        }
    }

    #[test]
    fn content_minimum_can_select_a_smaller_shell_at_the_same_viewport() {
        let layout = ShellLayout::resolve_for_content(
            1_200,
            800,
            ShellContentMinimums {
                wide: 1_280.0,
                ..ShellContentMinimums::default()
            },
        );
        assert_eq!(layout.kind, ShellKind::Medium);
    }

    #[test]
    fn shell_recomposition_keeps_one_primary_task_surface_at_each_narrowing_step() {
        let medium = ShellLayout::resolve(900, 720);
        assert_eq!(medium.kind, ShellKind::Medium);
        assert_eq!(medium.sidebar, PanelPresentation::Docked);
        assert_eq!(medium.preview, PanelPresentation::Docked);
        assert_eq!(medium.right_rail, PanelPresentation::Drawer);

        let narrow_medium = ShellLayout::resolve(768, 1_024);
        assert_eq!(narrow_medium.kind, ShellKind::Medium);
        assert_eq!(narrow_medium.sidebar, PanelPresentation::Drawer);
        assert_eq!(narrow_medium.preview, PanelPresentation::Docked);
        assert_eq!(narrow_medium.right_rail, PanelPresentation::Drawer);

        let compact = ShellLayout::resolve(390, 844);
        assert_eq!(compact.kind, ShellKind::Compact);
        assert_eq!(compact.preview, PanelPresentation::Sheet);
        assert_eq!(compact.right_rail, PanelPresentation::Sheet);
    }

    #[test]
    fn compact_two_level_navigation_and_sheet_restore_focus() {
        let mut navigation = CompactNavigation::default();
        navigation.enter_session("session_test");
        navigation.select_session(SessionDestination::Changes);
        navigation.open_overlay(
            OverlaySemantic::Dialog,
            ShellKind::Compact,
            "approve-button",
        );
        assert_eq!(navigation.level, NavigationLevel::Session);
        assert_eq!(navigation.session, SessionDestination::Changes);
        assert_eq!(
            navigation
                .overlay
                .as_ref()
                .map(|overlay| overlay.presentation),
            Some(PanelPresentation::Sheet)
        );
        assert_eq!(
            navigation.close_overlay().as_deref(),
            Some("approve-button")
        );
        assert_eq!(navigation.focus_target.as_deref(), Some("approve-button"));
        assert!(navigation.back());
        assert_eq!(navigation.level, NavigationLevel::Global);
        assert!(!navigation.back());
    }

    #[test]
    fn session_workflow_empty_states_remain_browsable_without_a_session() {
        let mut navigation = CompactNavigation::default();
        navigation.select_global(GlobalDestination::Management);
        navigation.select_session(SessionDestination::Files);

        assert_eq!(navigation.level, NavigationLevel::Global);
        assert_eq!(navigation.global, GlobalDestination::Sessions);
        assert_eq!(navigation.session, SessionDestination::Files);
        assert!(navigation.session_id.is_none());
    }

    #[test]
    fn compact_navigation_has_no_hover_only_core_action() {
        let mut navigation = CompactNavigation::default();
        for action in navigation.actions() {
            assert!(!action.hover_required);
            assert!(action.touch_target_px >= MIN_TOUCH_TARGET_PX);
        }
        navigation.enter_session("session_test");
        assert_eq!(navigation.actions().len(), 4);
        assert!(
            navigation
                .actions()
                .iter()
                .all(|action| !action.hover_required)
        );
    }
}
