//! Storage for the right rail's pinned web panels.
//!
//! The rows live in `right_rail_plugins`, the table the rail was always meant
//! to keep its non-plugin surfaces in. A panel is user data: a title and an
//! address, with no package, version, permission or consent behind it.

use rusqlite::{Connection, params};
use vibex_core::{RightRailWebPanel, VibexResult};

/// The `kind` every web panel row carries.
pub const RIGHT_RAIL_WEB_KIND: &str = "web";
/// The `status` an enabled panel carries.
const RIGHT_RAIL_WEB_STATUS: &str = "enabled";

/// Reads and writes the right rail's web panels.
pub struct RightRailPluginRepository;

impl RightRailPluginRepository {
    /// The live web panels, in rail order.
    pub fn list_web_panels(conn: &Connection) -> VibexResult<Vec<RightRailWebPanel>> {
        let mut statement = conn
            .prepare(
                "SELECT plugin_id, display_name, url, order_index \
                 FROM right_rail_plugins \
                 WHERE kind = ?1 AND deleted_at_ms IS NULL \
                 ORDER BY order_index, plugin_id",
            )
            .map_err(db_err("right_rail_web_panels_list_failed"))?;
        let rows = statement
            .query_map(params![RIGHT_RAIL_WEB_KIND], |row| {
                Ok(RightRailWebPanel {
                    panel_id: row.get(0)?,
                    title: row.get(1)?,
                    url: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    order_index: row.get(3)?,
                })
            })
            .map_err(db_err("right_rail_web_panels_list_failed"))?;
        let mut panels = Vec::new();
        for row in rows {
            panels.push(row.map_err(db_err("right_rail_web_panels_list_failed"))?);
        }
        Ok(panels)
    }

    /// How many panels are pinned.
    pub fn web_panel_count(conn: &Connection) -> VibexResult<usize> {
        let count = conn
            .query_row(
                "SELECT COUNT(*) FROM right_rail_plugins \
                 WHERE kind = ?1 AND deleted_at_ms IS NULL",
                params![RIGHT_RAIL_WEB_KIND],
                |row| row.get::<_, i64>(0),
            )
            .map_err(db_err("right_rail_web_panels_count_failed"))?;
        Ok(count.max(0) as usize)
    }

    /// The order index a newly added panel should take.
    pub fn next_order_index(conn: &Connection) -> VibexResult<i64> {
        let next = conn
            .query_row(
                "SELECT COALESCE(MAX(order_index), -1) + 1 FROM right_rail_plugins \
                 WHERE kind = ?1 AND deleted_at_ms IS NULL",
                params![RIGHT_RAIL_WEB_KIND],
                |row| row.get::<_, i64>(0),
            )
            .map_err(db_err("right_rail_web_panels_order_failed"))?;
        Ok(next)
    }

    /// Inserts a panel, or replaces the row with the same id.
    pub fn insert_web_panel(
        conn: &Connection,
        panel: &RightRailWebPanel,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "INSERT OR REPLACE INTO right_rail_plugins (
                plugin_id, kind, display_name, url, status, order_index,
                created_at_ms, updated_at_ms, deleted_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, NULL)",
            params![
                panel.panel_id,
                RIGHT_RAIL_WEB_KIND,
                panel.title,
                panel.url,
                RIGHT_RAIL_WEB_STATUS,
                panel.order_index,
                now_ms,
            ],
        )
        .map_err(db_err("right_rail_web_panel_insert_failed"))?;
        Ok(())
    }

    /// Soft-deletes a panel; `false` when no live row had that id.
    pub fn soft_delete_web_panel(
        conn: &Connection,
        panel_id: &str,
        now_ms: i64,
    ) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "UPDATE right_rail_plugins SET deleted_at_ms = ?3, updated_at_ms = ?3 \
                 WHERE plugin_id = ?1 AND kind = ?2 AND deleted_at_ms IS NULL",
                params![panel_id, RIGHT_RAIL_WEB_KIND, now_ms],
            )
            .map_err(db_err("right_rail_web_panel_delete_failed"))?;
        Ok(changed > 0)
    }
}

/// Maps a rusqlite failure onto a storage error.
fn db_err(code: &'static str) -> impl FnOnce(rusqlite::Error) -> vibex_core::VibexError {
    move |error| super::storage_err(code, "the right rail storage operation failed")(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        super::super::apply_migrations(&mut connection).unwrap();
        connection
    }

    fn panel(id: &str, order: i64) -> RightRailWebPanel {
        RightRailWebPanel {
            panel_id: id.to_string(),
            title: "Status".to_string(),
            url: "https://status.example.com/".to_string(),
            order_index: order,
        }
    }

    #[test]
    fn panels_round_trip_in_rail_order() {
        let connection = connection();
        assert!(
            RightRailPluginRepository::list_web_panels(&connection)
                .unwrap()
                .is_empty()
        );
        let mut second = panel("web.2", 1);
        second.title = "Charts".to_string();
        RightRailPluginRepository::insert_web_panel(&connection, &panel("web.1", 0), 100).unwrap();
        RightRailPluginRepository::insert_web_panel(&connection, &second, 100).unwrap();

        let panels = RightRailPluginRepository::list_web_panels(&connection).unwrap();
        assert_eq!(
            panels
                .iter()
                .map(|panel| panel.panel_id.as_str())
                .collect::<Vec<_>>(),
            vec!["web.1", "web.2"]
        );
        assert_eq!(panels[1].title, "Charts");
        assert_eq!(
            RightRailPluginRepository::web_panel_count(&connection).unwrap(),
            2
        );
        assert_eq!(
            RightRailPluginRepository::next_order_index(&connection).unwrap(),
            2
        );
    }

    #[test]
    fn a_removed_panel_disappears_but_is_not_erased() {
        let connection = connection();
        RightRailPluginRepository::insert_web_panel(&connection, &panel("web.1", 0), 100).unwrap();
        assert!(
            RightRailPluginRepository::soft_delete_web_panel(&connection, "web.1", 200).unwrap()
        );
        assert!(
            RightRailPluginRepository::list_web_panels(&connection)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            RightRailPluginRepository::web_panel_count(&connection).unwrap(),
            0
        );
        // Removing it twice reports that nothing changed.
        assert!(
            !RightRailPluginRepository::soft_delete_web_panel(&connection, "web.1", 300).unwrap()
        );
        let soft_deleted: i64 = connection
            .query_row(
                "SELECT deleted_at_ms FROM right_rail_plugins WHERE plugin_id = 'web.1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(soft_deleted, 200);
    }
}
