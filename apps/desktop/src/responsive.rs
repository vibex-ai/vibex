use vibex_ui::{PanelPresentation, ShellLayout};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkbenchVisibility {
    pub layout: ShellLayout,
    pub sidebar_docked: bool,
    pub preview_docked: bool,
    pub right_rail_docked: bool,
    pub sidebar_toggle_reachable: bool,
    pub preview_toggle_reachable: bool,
    pub right_rail_toggle_reachable: bool,
}

impl WorkbenchVisibility {
    pub fn resolve(width: u32, height: u32) -> Self {
        let layout = ShellLayout::resolve(width, height);
        Self {
            sidebar_docked: layout.sidebar == PanelPresentation::Docked,
            preview_docked: layout.preview == PanelPresentation::Docked,
            right_rail_docked: layout.right_rail == PanelPresentation::Docked,
            sidebar_toggle_reachable: true,
            preview_toggle_reachable: true,
            right_rail_toggle_reachable: true,
            layout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_viewports_keep_every_panel_reachable() {
        for (width, height) in [
            (1_600, 1_000),
            (1_200, 780),
            (900, 720),
            (760, 1_000),
            (360, 800),
            (360, 620),
        ] {
            let visibility = WorkbenchVisibility::resolve(width, height);
            assert!(visibility.sidebar_toggle_reachable);
            assert!(visibility.preview_toggle_reachable);
            assert!(visibility.right_rail_toggle_reachable);
            assert!(visibility.layout.docked_minimum_width() <= width as f32);
        }
    }
}
