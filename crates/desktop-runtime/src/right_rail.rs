//! The right rail's pinned web panels.
//!
//! The panels are user data, so they live beside the plugin rows rather than
//! inside them: a panel is an address the reader pinned, not a package that was
//! installed. Rendering belongs to the desktop's embedded browser; this handle
//! only keeps the list.

use std::path::PathBuf;

use rusqlite::Connection;
use vibex_core::{
    MAX_RIGHT_RAIL_WEB_PANELS, RightRailWebPanel, RightRailWebPanelAddRequest,
    RightRailWebPanelRemoveRequest, VibexError, VibexResult, unix_timestamp_ms,
    validate_web_panel_title, validate_web_panel_url,
};
use vibex_db::{RightRailPluginRepository, open_database};

/// The right rail's storage, reached the way the plugin handle is.
pub struct RightRailHandle {
    database_path: PathBuf,
}

impl RightRailHandle {
    pub fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }

    fn connection(&self) -> VibexResult<Connection> {
        open_database(&self.database_path)
    }

    /// The pinned panels, in rail order.
    pub fn web_panels(&self) -> VibexResult<Vec<RightRailWebPanel>> {
        RightRailPluginRepository::list_web_panels(&self.connection()?)
    }

    /// Pins a panel after checking its title and address.
    pub fn add_web_panel(
        &self,
        request: RightRailWebPanelAddRequest,
    ) -> VibexResult<RightRailWebPanel> {
        let title = validate_web_panel_title(&request.title).map_err(|message| {
            VibexError::validation("right_rail_web_panel_title_invalid", message)
        })?;
        let url = validate_web_panel_url(&request.url).map_err(|message| {
            VibexError::validation("right_rail_web_panel_url_invalid", message)
        })?;
        let connection = self.connection()?;
        if RightRailPluginRepository::web_panel_count(&connection)? >= MAX_RIGHT_RAIL_WEB_PANELS {
            return Err(VibexError::validation(
                "right_rail_web_panel_limit",
                format!("the right rail holds at most {MAX_RIGHT_RAIL_WEB_PANELS} web panels"),
            ));
        }
        let panel = RightRailWebPanel {
            panel_id: format!("web.{}", vibex_core::RequestId::new().as_str()),
            title,
            url,
            order_index: RightRailPluginRepository::next_order_index(&connection)?,
        };
        RightRailPluginRepository::insert_web_panel(&connection, &panel, unix_timestamp_ms())?;
        Ok(panel)
    }

    /// Unpins a panel; `false` when the id is not a live panel.
    pub fn remove_web_panel(&self, request: RightRailWebPanelRemoveRequest) -> VibexResult<bool> {
        RightRailPluginRepository::soft_delete_web_panel(
            &self.connection()?,
            &request.panel_id,
            unix_timestamp_ms(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_db::apply_migrations;

    fn handle(directory: &std::path::Path) -> (RightRailHandle, PathBuf) {
        let path = directory.join("vibex.sqlite");
        let mut connection = Connection::open(&path).unwrap();
        apply_migrations(&mut connection).unwrap();
        drop(connection);
        (RightRailHandle::new(path.clone()), path)
    }

    #[test]
    fn a_panel_is_validated_and_listed_in_order() {
        let directory = tempfile::tempdir().unwrap();
        let (handle, _) = handle(directory.path());
        let first = handle
            .add_web_panel(RightRailWebPanelAddRequest {
                title: "  Status  ".to_string(),
                url: "https://status.example.com/".to_string(),
            })
            .unwrap();
        assert_eq!(first.title, "Status");
        assert_eq!(first.order_index, 0);
        let second = handle
            .add_web_panel(RightRailWebPanelAddRequest {
                title: "Charts".to_string(),
                url: "https://charts.example.com/".to_string(),
            })
            .unwrap();
        assert_eq!(second.order_index, 1);
        let panels = handle.web_panels().unwrap();
        assert_eq!(
            panels
                .iter()
                .map(|panel| panel.panel_id.as_str())
                .collect::<Vec<_>>(),
            vec![first.panel_id.as_str(), second.panel_id.as_str()]
        );

        assert!(
            handle
                .remove_web_panel(RightRailWebPanelRemoveRequest {
                    panel_id: first.panel_id.clone(),
                })
                .unwrap()
        );
        assert_eq!(handle.web_panels().unwrap().len(), 1);
    }

    #[test]
    fn a_bad_address_or_title_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let (handle, _) = handle(directory.path());
        for (title, url) in [
            ("", "https://example.com/"),
            ("Panel", "http://example.com/"),
            ("Panel", "javascript:alert(1)"),
            ("Panel", "https://user:pass@example.com/"),
        ] {
            assert!(
                handle
                    .add_web_panel(RightRailWebPanelAddRequest {
                        title: title.to_string(),
                        url: url.to_string(),
                    })
                    .is_err(),
                "{title} / {url} should be refused"
            );
        }
        // A loopback dashboard is allowed.
        assert!(
            handle
                .add_web_panel(RightRailWebPanelAddRequest {
                    title: "Local".to_string(),
                    url: "http://localhost:3000/".to_string(),
                })
                .is_ok()
        );
    }
}
