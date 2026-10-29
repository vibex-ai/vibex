//! The configuration centre: its navigation, its writes, and its frame.
//!
//! These tests drive the real reducer and the real renderer at real terminal
//! sizes. They are the layer that catches the mistakes the compiler cannot:
//! a module that cannot be reached, an action menu built from the wrong
//! record, a table whose columns do not line up with their headings.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use vibex_backend::DisconnectedBackend;
use vibex_tui::app::{App, AppOptions, ManagementData, Overlay, Page};
use vibex_tui::config_center::{
    ConfigAction, ConfigCenter, ConfigModule, ConfigSection, ConfigZone,
};
use vibex_tui::theme::{ColorCapability, ColorMode, GlyphMode};
use vibex_tui::{Locale, SeatKind};

fn app(columns: u16, rows: u16) -> App {
    let mut app = App::new(
        DisconnectedBackend::facade(),
        AppOptions {
            seat: SeatKind::Remote,
            capability: ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
            theme_id: Some("vibex-dark".to_string()),
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Locale::En,
            // No arrangement file: a test must never write into the runner's
            // home directory, and each test wants a clean surface.
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.keymap = vibex_tui::keymap::Keymap::built_in();
    app.resize(columns, rows);
    app
}

/// The spinner frame drawn on the line that says `text`, if it wears one.
///
/// Found by the words rather than by the row's number: the table's rows move
/// with the module and the filter, and what the test is about is the mark that
/// stands beside the sentence a write put there.
fn spinner_before(screen: &str, text: &str) -> Option<&'static str> {
    let line = screen.lines().find(|line| line.contains(text))?;
    vibex_tui::glyphs::spinner_frames(vibex_tui::glyphs::GlyphTier::Full)
        .iter()
        .copied()
        .find(|frame| line.contains(frame))
}

/// Draw one frame and return the text a reader would see.
fn text(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, app))
        .expect("frame draws");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
        }
        out.push('\n');
    }
    out
}

/// One Agent with a managed install that has an update waiting.
fn agent() -> vibex_core::AgentSnapshotEntry {
    serde_json::from_value(serde_json::json!({
        "id": "codex",
        "label": "Codex",
        "description": "An open-source coding agent",
        "runtimeKind": "acp",
        "sourceKind": "builtin",
        "added": true,
        "enabled": true,
        "orderIndex": 0,
        "installed": true,
        "configured": true,
        "installStatus": "installed",
        "managedInstall": {
            "managed": true,
            "status": "update_available",
            "distributionKind": "npm",
            "installedVersion": "1.2.3",
            "availableVersion": "1.3.0"
        },
        "configStatus": "configured",
        "runtimeStatus": "ready",
        "models": ["gpt-5", "gpt-5-mini"],
        "modes": [],
        "capabilityHints": [],
        "env": {},
        "params": {},
        "diagnostics": [],
        "nativeConfigPaths": ["/home/dev/.codex/config.toml"]
    }))
    .expect("agent fixture")
}

fn mcp_server() -> vibex_core::McpServer {
    serde_json::from_value(serde_json::json!({
        "id": "mcp_filesystem",
        "displayName": "Filesystem",
        "transportKind": "stdio",
        "status": "enabled",
        "scopeKind": "user",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem"],
        "env": [],
        "headers": [],
        "secretReferences": [],
        "providerMatrix": [],
        "description": "Read and write files in a workspace",
        "tags": ["files"],
        "agentMatrix": [
            {"agentId": "codex", "enabled": true, "sourceKind": "manual", "updatedAtMs": 1}
        ],
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("server fixture")
}

fn prompt() -> vibex_core::Prompt {
    serde_json::from_value(serde_json::json!({
        "id": "prompt_review",
        "displayName": "Review",
        "kind": "reusable_prompt",
        "status": "enabled",
        "scopeKind": "user",
        "body": "Review this workspace.",
        "tags": ["review"],
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("prompt fixture")
}

/// The centre, opened on the Agent roster with one of everything loaded.
fn loaded(columns: u16, rows: u16) -> App {
    let mut app = app(columns, rows);
    // A live connection: a mutating verb is refused while the client is still
    // attaching, and every verb worth testing here is one.
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(vibex_tui::action::Intent::GotoManagement);
    app.management_data = ManagementData {
        agents: vec![agent()],
        mcp: vec![mcp_server()],
        prompts: vec![prompt()],
        ..Default::default()
    };
    app
}

#[test]
fn the_centre_opens_on_the_agent_module_and_reaches_every_other_one() {
    let mut app = loaded(140, 40);
    assert_eq!(app.page, Page::Management);
    assert_eq!(app.config_center.module, ConfigModule::Agent);
    assert_eq!(app.config_center.section, ConfigSection::Agents);

    let mut seen = vec![ConfigModule::Agent];
    for _ in 0..ConfigModule::ALL.len() - 1 {
        app.perform(vibex_tui::action::Intent::ConfigModuleNext);
        seen.push(app.config_center.module);
    }
    assert_eq!(seen, ConfigModule::ALL.to_vec());
    // And it wraps rather than stopping at the last module.
    app.perform(vibex_tui::action::Intent::ConfigModuleNext);
    assert_eq!(app.config_center.module, ConfigModule::Agent);
}

#[test]
fn the_rail_switches_the_area_and_the_filter_belongs_to_the_area() {
    let mut app = loaded(140, 40);
    assert_eq!(app.config_center.section, ConfigSection::Agents);
    app.perform(vibex_tui::action::Intent::ConfigSectionNext);
    assert_eq!(app.config_center.section, ConfigSection::Providers);
    // A filter written in one area must not follow the reader into the next,
    // or a table would look empty for a reason nothing on screen explains.
    app.push_config_filter('c');
    app.perform(vibex_tui::action::Intent::ConfigSectionNext);
    assert_eq!(app.config_center.filter, "");
    // The module's own areas are the only ones the rail can reach.
    for _ in 0..6 {
        app.perform(vibex_tui::action::Intent::ConfigSectionNext);
    }
    assert!(
        ConfigModule::Agent
            .sections()
            .contains(&app.config_center.section)
    );
}

#[test]
fn the_tabs_and_the_rail_are_drawn_with_their_counts() {
    let mut app = loaded(140, 40);
    let screen = text(&mut app, 140, 40);
    for needle in [
        "Agent",
        "MCP",
        "Skills",
        "Prompts",
        "Agents",
        "Model providers",
    ] {
        assert!(
            screen.contains(needle),
            "the centre lost {needle}:\n{screen}"
        );
    }
    // The table names its columns and the record it holds.
    for needle in ["Install", "Runtime", "Codex", "Enabled"] {
        assert!(
            screen.contains(needle),
            "the table lost {needle}:\n{screen}"
        );
    }
    // And the detail pane describes the highlighted record.
    for needle in ["1.2.3 → 1.3.0", "gpt-5, gpt-5-mini"] {
        assert!(
            screen.contains(needle),
            "the detail lost {needle}:\n{screen}"
        );
    }
}

#[test]
fn a_click_switches_the_module_and_selects_the_row_under_it() {
    let mut app = loaded(140, 40);
    let _ = text(&mut app, 140, 40);
    // The tab strip publishes where each module landed.
    let (rect, _) = app
        .regions
        .config_tabs
        .iter()
        .find(|(_, module)| *module == ConfigModule::Mcp)
        .copied()
        .expect("the MCP tab is published");
    let outcome = app.click_config_control(rect.x + 1, rect.y);
    assert!(outcome.is_some(), "the tab click was not handled");
    assert_eq!(app.config_center.module, ConfigModule::Mcp);

    // A record row selects on the first click and opens its actions on the
    // second, the same contract every other list in this client keeps.
    let _ = text(&mut app, 140, 40);
    let region = app.regions.list.clone().expect("the table is published");
    assert!(region.rows >= 1);
    let outcome = app.click_config_row(0, false);
    assert!(
        outcome.effects.is_empty(),
        "the first click opened something"
    );
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
    let _ = text(&mut app, 140, 40);
    app.click_config_row(0, true);
    assert!(
        matches!(app.overlay, Some(Overlay::EntryActions { .. })),
        "a second click did not open the action menu"
    );
}

#[test]
fn the_action_menu_is_built_from_the_record_it_was_opened_on() {
    let mut app = loaded(140, 40);
    app.perform(vibex_tui::action::Intent::ConfigEntryActions);
    let Some(Overlay::EntryActions { entry }) = app.overlay.clone() else {
        panic!("the action menu did not open");
    };
    assert_eq!(entry.row_key, "codex");
    // A managed Agent can be updated, rolled back and uninstalled; an external
    // one cannot, which is why the menu is per record. This Agent is at a
    // published version, so the verb is the upgrade rather than a first
    // install — the two run the same call and the label is what differs.
    for action in [
        ConfigAction::Upgrade,
        ConfigAction::CheckUpdate,
        ConfigAction::Rollback,
        ConfigAction::Toggle,
    ] {
        assert!(
            entry.actions.contains(&action),
            "the Agent menu lost {action:?}"
        );
    }
    assert!(
        !entry.actions.contains(&ConfigAction::Install),
        "an Agent already at a published version was offered a first install"
    );
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains("Roll back"), "{screen}");
}

#[test]
fn the_form_opens_on_the_record_and_saves_what_was_typed() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor did not open");
    };
    assert!(form.value("name").as_deref() == Some("Filesystem"));
    assert_eq!(form.value("transport").as_deref(), Some("stdio"));
    assert_eq!(form.value("command").as_deref(), Some("npx"));

    // Typing reaches the field the cursor is on, and only that one.
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    app.type_form_char('!');
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor closed");
    };
    assert!(form.value("name").as_deref() == Some("Filesystem!"));
    assert_eq!(form.value("command").as_deref(), Some("npx"));
    assert!(form.dirty());

    // `Esc` gives the field up before it gives the form up.
    app.perform(vibex_tui::action::Intent::Back);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the first Esc closed the whole form");
    };
    assert_eq!(form.value("name").as_deref(), Some("Filesystem"));
    app.perform(vibex_tui::action::Intent::Back);
    assert!(app.overlay.is_none(), "the second Esc left the form open");
}

#[test]
fn a_required_field_left_empty_is_refused_with_the_reason_on_screen() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    // Clear the name, which the authority will not accept.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.fields[0].value.clear();
    }
    app.perform(vibex_tui::action::Intent::ConfigFormSave);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on an invalid save");
    };
    assert!(form.error.is_some(), "no reason was given");
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains("name is required"), "{screen}");
}

#[test]
fn the_switch_verbs_ask_the_authority_rather_than_patching_the_row() {
    let mut app = loaded(140, 40);
    // The Agent's switch is a configuration write…
    let outcome = app.perform(vibex_tui::action::Intent::ToggleSelectedEntry);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            assert_eq!(mutation.label(), "agent");
        }
        other => panic!("the Agent switch produced {other:?}"),
    }
    // …while an MCP server's is the update the runtime already had, because
    // the record is the runtime's and only the runtime may change it.
    app.show_config_module(ConfigModule::Mcp, None);
    let outcome = app.perform(vibex_tui::action::Intent::ToggleSelectedEntry);
    assert!(matches!(
        outcome.effects.as_slice(),
        [vibex_tui::app::Effect::ToggleMcp { enabled: false, .. }]
    ));
}

#[test]
fn a_delete_is_a_question_before_it_is_a_write() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigEntryDelete);
    assert!(outcome.effects.is_empty(), "the delete ran unasked");
    assert!(matches!(app.overlay, Some(Overlay::Confirm { .. })));
    let screen = text(&mut app, 140, 40);
    // The question names the record it is about, so a reader who pressed `d`
    // on the wrong row can see that before they answer.
    assert!(screen.contains("mcp_filesystem"), "{screen}");
    assert!(screen.contains("Delete this record?"), "{screen}");
    // Answering it is the write.
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            assert_eq!(mutation.label(), "mcp_delete");
        }
        other => panic!("the answer produced {other:?}"),
    }
}

#[test]
fn the_filter_narrows_the_table_and_says_so_when_nothing_matches() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    app.perform(vibex_tui::action::Intent::BeginFilter);
    assert!(app.config_center.filtering);
    for character in "zzz".chars() {
        app.push_config_filter(character);
    }
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        0
    );
    // An empty table is what an answer left behind, so the area is read before
    // the empty state is the thing on screen.
    app.management_data
        .mark_read(ConfigModule::Mcp, ConfigSection::Servers);
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains("No matches"), "{screen}");
    // Clearing it brings the record back.
    app.perform(vibex_tui::action::Intent::ClearFilter);
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        1
    );
}

#[test]
fn the_wheel_moves_the_cursor_in_the_table_and_the_pane_in_the_detail() {
    let mut app = loaded(140, 40);
    app.management_data.mcp.push(mcp_server());
    app.management_data.mcp.push(mcp_server());
    app.show_config_module(ConfigModule::Mcp, None);
    let _ = text(&mut app, 140, 40);
    let region = app.regions.list.clone().expect("the table is published");
    app.scroll_config_area(1, region.rect.x + 2, region.rect.y);
    assert_eq!(app.config_center.cursor(3), 1);
    // Below the table the wheel scrolls the pane rather than the cursor.
    let before = app.config_center.cursor(3);
    app.scroll_config_area(1, region.rect.right() + 40, region.rect.bottom() + 3);
    assert_eq!(app.config_center.cursor(3), before);
}

#[test]
fn a_narrow_terminal_still_draws_the_centre() {
    // The rail is fixed width, so the frame has to make the table fit beside
    // it or say so rather than drawing half a table.
    for columns in [40u16, 60, 80, 120] {
        let mut app = loaded(columns, 24);
        let screen = text(&mut app, columns, 24);
        assert!(
            screen.contains("Codex") || screen.contains("Configuration"),
            "{columns} columns lost the centre:\n{screen}"
        );
    }
}

#[test]
fn the_default_centre_is_empty_rather_than_wrong() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::GotoManagement);
    // The centre opens on an area nobody has read yet; the empty state is what
    // the answer leaves when it has nothing to put there.
    app.management_data
        .mark_read(ConfigModule::Agent, ConfigSection::Agents);
    let screen = text(&mut app, 120, 40);
    assert!(screen.contains("Nothing here yet"), "{screen}");
    assert!(
        !screen.contains("Devices"),
        "the centre grew a device module:\n{screen}"
    );
    assert_eq!(ConfigCenter::default().zone, ConfigZone::Rows);
}

#[test]
fn the_three_panes_wrap_when_the_left_and_right_keys_walk_them() {
    let mut app = loaded(140, 40);
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
    // The three panes are the ones the surface draws, in reading order.
    assert_eq!(
        ConfigZone::ALL,
        [ConfigZone::Modules, ConfigZone::Rail, ConfigZone::Rows]
    );
    let mut walked = vec![app.config_center.zone];
    for _ in 0..2 {
        app.perform(vibex_tui::action::Intent::ConfigPaneNext);
        walked.push(app.config_center.zone);
    }
    assert_eq!(
        walked,
        [ConfigZone::Rows, ConfigZone::Modules, ConfigZone::Rail]
    );
    // The fourth press is the first again: `→` never dead-ends.
    app.perform(vibex_tui::action::Intent::ConfigPaneNext);
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
    // And `←` walks the other way, also wrapping.
    app.perform(vibex_tui::action::Intent::ConfigPanePrevious);
    assert_eq!(app.config_center.zone, ConfigZone::Rail);
    app.perform(vibex_tui::action::Intent::ConfigPanePrevious);
    assert_eq!(app.config_center.zone, ConfigZone::Modules);
    app.perform(vibex_tui::action::Intent::ConfigPanePrevious);
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
}

#[test]
fn the_focused_pane_is_named_on_screen_and_the_others_are_not() {
    let mut app = loaded(140, 40);
    for zone in ConfigZone::ALL {
        while app.config_center.zone != zone {
            app.perform(vibex_tui::action::Intent::ConfigPaneNext);
        }
        let screen = text(&mut app, 140, 40);
        let expected = format!("Focus · {}", zone.label(app.strings));
        assert!(
            screen.contains(&expected),
            "the footer does not name the {zone:?} pane:\n{screen}"
        );
        // Exactly one pane is named: a second focus line would be a second
        // answer to "where are my keys going".
        let named = ["Focus · Modules", "Focus · Areas", "Focus · Records"]
            .iter()
            .filter(|needle| screen.contains(*needle))
            .count();
        assert_eq!(named, 1, "two panes claimed the focus:\n{screen}");
    }
}

#[test]
fn the_heading_row_sits_over_the_cells_it_names() {
    use ratatui::backend::TestBackend;

    let mut app = loaded(160, 40);
    // A wide glyph in the record's own name: it is two columns wide, and a
    // table padded by character count walks every cell after it out of line by
    // one column per glyph.
    if let Some(agent) = app.management_data.agents.first_mut() {
        agent.label = "代码助手".to_string();
    }
    if let Some(server) = app.management_data.mcp.first_mut() {
        server.display_name = "文件系统".to_string();
    }
    for (module, section) in [
        (ConfigModule::Agent, ConfigSection::Agents),
        (ConfigModule::Mcp, ConfigSection::Servers),
    ] {
        app.show_config_module(module, None);
        let columns = vibex_tui::config_center::columns(module, section, app.strings);
        let rows = app.config_center.visible(&app.management_data, app.strings);
        let first = rows
            .first()
            .unwrap_or_else(|| panic!("{module:?} has no row"));

        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| vibex_tui::view::render(frame, &mut app))
            .expect("frame draws");
        let buffer = terminal.backend().buffer().clone();

        // The columns are positions in the *drawn* frame, so the check is
        // positional: a heading and the cell it names must start in the same
        // terminal column, whatever either of them holds.
        let heading_row = (0..buffer.area.height)
            .find(|row| {
                buffer_find(&buffer, *row, columns[0]).is_some()
                    && buffer_find(&buffer, *row, "●").is_none()
                    && buffer_find(&buffer, *row, "○").is_none()
            })
            .unwrap_or_else(|| panic!("no heading row for {module:?}"));
        let record_row = (0..buffer.area.height)
            .find(|row| {
                buffer_find(&buffer, *row, "●").is_some()
                    || buffer_find(&buffer, *row, "○").is_some()
            })
            .unwrap_or_else(|| panic!("no record row for {module:?}"));

        let mut previous = 0u16;
        for (index, column) in columns.iter().enumerate() {
            let Some(cell) = first.cells.get(index) else {
                panic!("{module:?} has a heading with no cell under it: {column}");
            };
            let heading_at = buffer_find(&buffer, heading_row, column)
                .unwrap_or_else(|| panic!("{column} is not in the heading row"));
            // The cell is looked for *from* the heading's own column: a short
            // cell like `2` also appears inside a longer one to its left, and a
            // search from the start of the row would compare a position with
            // itself.
            let record_at = buffer_find_from(&buffer, record_row, cell, heading_at)
                .unwrap_or_else(|| panic!("{cell} is not under {column} in the record row"));
            assert_eq!(
                heading_at, record_at,
                "the {column} heading of {module:?} is not over its cell"
            );
            assert!(
                heading_at >= previous,
                "the headings of {module:?} are out of order"
            );
            previous = heading_at;
        }
    }
}

/// The heading row names every column the table draws.
///
/// The record's own name and its status chip are columns like any other: a
/// heading row that left them blank would begin halfway across the table, which
/// reads as a table whose first columns were lost rather than one whose first
/// columns need no label.
#[test]
fn the_heading_row_names_every_column_the_table_draws() {
    use ratatui::backend::TestBackend;

    let mut app = loaded(160, 40);
    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, &mut app))
        .expect("frame draws");
    let buffer = terminal.backend().buffer().clone();

    let heading_row = (0..buffer.area.height)
        .find(|row| buffer_find(&buffer, *row, "Name").is_some())
        .expect("the heading row names the record column");
    let row = (0..buffer.area.width)
        .map(|column| {
            buffer
                .cell((column, heading_row))
                .map(|cell| cell.symbol().to_string())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let line = row.join("");
    // The two columns the data list does not name are named here, and they do
    // not sit after the last data heading: the heading row starts where the
    // cells start.
    assert!(
        line.contains("Name") && line.contains("Status"),
        "the heading row left a column unnamed:\n{line}"
    );
    assert!(
        line.find("Name").unwrap_or(usize::MAX) < line.find("Install").unwrap_or(usize::MAX),
        "the record column is not the first heading:\n{line}"
    );
    // Every heading the section declares is on the row, in order.
    let headings = vibex_tui::config_center::columns(
        app.config_center.module,
        app.config_center.section,
        app.strings,
    );
    let mut previous = 0usize;
    for heading in headings {
        let at = line
            .find(heading)
            .unwrap_or_else(|| panic!("{heading} is not on the heading row:\n{line}"));
        assert!(at >= previous, "the headings are out of order:\n{line}");
        previous = at;
    }
}

/// An Agent the desktop does not expose is not one the TUI lists either.
#[test]
fn the_agent_list_hides_the_integrations_the_desktop_hides() {
    let mut app = loaded(140, 40);
    // A builtin the product policy keeps out of the user-facing catalog. The
    // policy list is the same one the desktop reads, so this is exactly the
    // row the two clients have to agree on.
    let hidden = vibex_core::builtin_agent_definitions()
        .into_iter()
        .find(|definition| !vibex_core::is_user_visible_agent(&definition.id))
        .map(|definition| definition.id)
        .expect("the catalog keeps at least one integration out of the product");
    let mut hidden_entry = agent();
    hidden_entry.id = hidden.clone();
    hidden_entry.label = format!("Hidden {}", hidden.as_str());
    app.management_data.agents.push(hidden_entry);

    // A Custom Agent with an id the policy does not know is still the reader's.
    let mut custom = agent();
    custom.id = vibex_core::AgentId::parse("my-own-agent").expect("custom id");
    custom.label = "My own Agent".to_string();
    custom.source_kind = vibex_core::AgentSourceKind::Custom;
    app.management_data.agents.push(custom);

    let rows = app
        .config_center
        .visible(&app.management_data, app.strings)
        .into_iter()
        .map(|row| row.key)
        .collect::<Vec<_>>();
    assert!(
        !rows.iter().any(|key| key == hidden.as_str()),
        "the list shows an Agent the desktop hides: {rows:?}"
    );
    assert!(
        rows.iter().any(|key| key == "my-own-agent"),
        "the list hid the reader's own Agent: {rows:?}"
    );
}

/// The terminal column a word starts at in one drawn row, if it is there.
///
/// A word is matched against the cells themselves rather than against a
/// string built from them: a wide glyph occupies two cells and the second one
/// is empty, so a string index would drift one column per wide glyph and the
/// alignment this test is about would be invisible.
fn buffer_find(buffer: &ratatui::buffer::Buffer, row: u16, needle: &str) -> Option<u16> {
    buffer_find_from(buffer, row, needle, 0)
}

/// As [`buffer_find`], starting the search at `from`.
fn buffer_find_from(
    buffer: &ratatui::buffer::Buffer,
    row: u16,
    needle: &str,
    from: u16,
) -> Option<u16> {
    let symbols = (0..buffer.area.width)
        .map(|column| {
            buffer
                .cell((column, row))
                .map(|cell| cell.symbol().to_string())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let wanted = needle.chars().map(|ch| ch.to_string()).collect::<Vec<_>>();
    (usize::from(from)..symbols.len())
        .find(|start| {
            wanted
                .iter()
                .enumerate()
                .all(|(offset, expected)| symbols.get(start + offset) == Some(expected))
        })
        .map(|start| start as u16)
}

#[test]
fn the_agent_module_has_no_health_area_and_prompts_have_no_hooks() {
    assert_eq!(
        ConfigModule::Agent.sections(),
        [ConfigSection::Agents, ConfigSection::Providers]
    );
    assert_eq!(ConfigModule::Prompts.sections(), [ConfigSection::Prompts]);
    // Every section still in the model is reachable from some module, so a
    // variant cannot outlive the area that drew it.
    for section in [
        ConfigSection::Agents,
        ConfigSection::Providers,
        ConfigSection::Servers,
        ConfigSection::Installed,
        ConfigSection::Catalog,
        ConfigSection::Sources,
        ConfigSection::Prompts,
    ] {
        assert!(
            ConfigModule::ALL
                .iter()
                .any(|module| module.sections().contains(&section)),
            "{section:?} is unreachable"
        );
    }
    let mut app = loaded(140, 40);
    let screen = text(&mut app, 140, 40);
    assert!(!screen.contains("Health"), "{screen}");
    assert!(!screen.contains("Hook"), "{screen}");
    app.show_config_module(ConfigModule::Prompts, None);
    let screen = text(&mut app, 140, 40);
    assert!(!screen.contains("Hook"), "{screen}");
}

#[test]
fn the_page_keys_scroll_the_detail_that_the_cursor_describes() {
    let mut app = loaded(140, 40);
    // Nothing to scroll until the pane holds more lines than it has rows, so
    // give the highlighted record enough to overflow it.
    if let Some(agent) = app.management_data.agents.first_mut() {
        for index in 0..40 {
            agent
                .native_config_paths
                .push(format!("/tmp/config-{index}.toml"));
        }
    }
    let before = app.config_center.detail_scroll;
    app.perform(vibex_tui::action::Intent::ScrollPageDown);
    assert!(
        app.config_center.detail_scroll > before,
        "the page key did not move the detail pane"
    );
    // The cursor is untouched: the detail belongs to the records pane, and the
    // page keys move the pane rather than the row it describes.
    assert_eq!(app.config_center.cursor(1), 0);
    app.perform(vibex_tui::action::Intent::ScrollPageUp);
    assert_eq!(app.config_center.detail_scroll, before);
}

#[test]
fn every_agent_verb_says_what_it_is_doing_on_the_row_it_was_pressed_on() {
    // The complaint this covers: a verb ran and left the reader unable to tell
    // whether the key had done anything at all, let alone what. Each verb marks
    // the record it was about, and the action line says the verb rather than
    // the effect key the write travels under.
    use vibex_tui::config_center::ConfigVerb;
    for (action, verb) in [
        (ConfigAction::Install, ConfigVerb::Install),
        (ConfigAction::CheckUpdate, ConfigVerb::CheckUpdate),
        (ConfigAction::Rollback, ConfigVerb::Rollback),
        (ConfigAction::RefreshSnapshot, ConfigVerb::RefreshSnapshot),
        (ConfigAction::Auth, ConfigVerb::SignIn),
        (ConfigAction::Logout, ConfigVerb::SignOut),
        (ConfigAction::Uninstall, ConfigVerb::Uninstall),
        // The fixture's Agent is enabled, so the switch asks for "disable".
        (ConfigAction::Toggle, ConfigVerb::Disable),
    ] {
        let mut app = loaded(140, 40);
        // These verbs are the authority's, and a client that never saw it
        // advertise them refuses them: the row is marked only over work that
        // was actually asked for. The Agent's accounts are the agent domain's;
        // its install is the management domain's.
        app.capabilities.agent = vibex_backend::DomainCapabilities::available([
            vibex_backend::BackendOperation::AgentAuthRead,
            vibex_backend::BackendOperation::AgentAuthManage,
        ]);
        app.capabilities.management = vibex_backend::DomainCapabilities::available([
            vibex_backend::BackendOperation::ManagementAgents,
        ]);
        // The idle row wears its status mark; a working one wears a spinner
        // instead, so its absence is what says the marker moved.
        assert!(text(&mut app, 140, 40).contains('●'));
        app.run_config_action_by_key("codex", action);
        // Uninstall asks before it runs, and answering the question is what
        // starts the write: the row is not marked over a question.
        if matches!(app.overlay, Some(Overlay::Confirm { .. })) {
            app.perform(vibex_tui::action::Intent::ConfirmOverlay);
        }

        let pending = app
            .config_center
            .pending
            .get("codex")
            .unwrap_or_else(|| panic!("{action:?} left the row silent"));
        assert_eq!(pending.verb, verb, "{action:?} named the wrong verb");
        assert_eq!(
            pending.subject, "Codex",
            "{action:?} named the wrong record"
        );

        // A verb that opens a surface covers the row it was pressed on, and the
        // marker underneath is what the reader finds when they close it.
        app.overlay = None;
        let screen = text(&mut app, 140, 40);
        assert!(
            screen.contains(verb.running(app.strings)),
            "{action:?} says nothing while it works:\n{screen}"
        );
        assert!(
            !screen.contains('●'),
            "{action:?} left the row looking idle while it worked:\n{screen}"
        );
    }
}

#[test]
fn a_write_in_flight_keeps_the_marker_turning_until_it_answers() {
    // The working row replaces its status mark with a spinner, and that spinner
    // is drawn from the animation phase. A clock that stopped under the write
    // left the marker standing on one frame for the whole round trip, which is
    // the one thing a write marker must not do: it is there to say the press
    // landed.
    let mut app = loaded(140, 40);
    assert!(
        !app.chrome_animating(),
        "the idle centre is asking for frames"
    );

    app.run_config_action_by_key("codex", ConfigAction::CheckUpdate);
    let verb = app
        .config_center
        .pending
        .get("codex")
        .expect("the row is waiting")
        .verb;
    let working = verb.running(app.strings);

    let mut drawn = Vec::new();
    for _ in 0..4 {
        let screen = text(&mut app, 140, 40);
        drawn.push(
            spinner_before(&screen, working)
                .unwrap_or_else(|| panic!("the row says it works with nothing turning:\n{screen}"))
                .to_string(),
        );
        assert!(
            app.chrome_animating(),
            "the centre stopped asking for frames under a write"
        );
        // Each frame is held for a few ticks, so a frame that moved is a clock
        // that ran rather than one the renderer happened to sample late.
        for _ in 0..vibex_tui::glyphs::SPINNER_TICKS_PER_FRAME {
            assert!(
                app.advance_transcript_animation(),
                "the clock stopped under a write"
            );
        }
    }
    assert!(
        drawn.iter().any(|frame| *frame != drawn[0]),
        "the marker never moved: {drawn:?}"
    );

    // The answer releases the clock with the marker: an answered record is a
    // quiet page again.
    app.config_center.finish_pending("codex");
    let screen = text(&mut app, 140, 40);
    assert!(
        spinner_before(&screen, working).is_none(),
        "the row kept turning after its answer:\n{screen}"
    );
    assert!(
        !app.chrome_animating(),
        "the answered centre keeps asking for frames"
    );
    assert!(!app.advance_transcript_animation());
}

#[test]
fn a_second_press_on_a_working_record_is_refused_rather_than_queued() {
    // Two of the same call in flight is how a row ends up describing a write
    // that finished before the one it is waiting on. The second press is
    // answered rather than sent.
    let mut app = loaded(140, 40);
    let first = app.run_config_action_by_key("codex", ConfigAction::CheckUpdate);
    assert_eq!(
        first.effects.len(),
        1,
        "the first press did not ask for work"
    );

    let second = app.run_config_action_by_key("codex", ConfigAction::CheckUpdate);
    assert!(
        second.effects.is_empty(),
        "the same call was queued a second time"
    );
    let toast = app.toast.as_ref().expect("the refusal was silent");
    assert_eq!(toast.tone, vibex_tui::app::ToastTone::Warning);

    // Once it has answered, the record is free again.
    app.config_center.finish_pending("codex");
    let third = app.run_config_action_by_key("codex", ConfigAction::RefreshSnapshot);
    assert_eq!(
        third.effects.len(),
        1,
        "the record stayed blocked afterwards"
    );
}

#[test]
fn what_a_write_answered_is_drawn_under_the_table_after_the_toast_is_gone() {
    // The toast is gone in seconds; a reader who looked away would have to
    // repeat the action to find out what it did. The line under the table is
    // the same sentence, kept.
    use vibex_tui::config_center::ConfigTone;
    let mut app = loaded(140, 40);
    let answer = "Codex: Installed · version 9.9.9";
    assert!(!text(&mut app, 140, 40).contains(answer));

    app.config_center.set_notice(answer, ConfigTone::Ok);
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains(answer), "{screen}");
    // The verbs are still on the line: a notice reports the last write without
    // taking away the next one.
    assert!(screen.contains("Install"), "{screen}");
}

/// One drawn row of a frame, one string per terminal column.
fn row_symbols(screen: &str, row: usize) -> Vec<String> {
    screen
        .lines()
        .nth(row)
        .expect("the row was drawn")
        .chars()
        .map(|character| character.to_string())
        .collect()
}

#[test]
fn a_notice_shifts_the_verb_buttons_without_losing_the_click() {
    // The notice is drawn in front of the verbs, so the buttons move right when
    // one is showing. Each frame republishes where they landed and a press is
    // resolved against that, which is what keeps a mouse press on the verb the
    // reader can see rather than on the one that used to be there.
    use vibex_tui::config_center::{ConfigTone, ConfigVerb};
    let mut app = loaded(140, 40);
    app.capabilities.management = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::ManagementAgents,
    ]);
    app.config_center
        .set_notice("Codex: Installed · version 1.3.0", ConfigTone::Ok);
    let screen = text(&mut app, 140, 40);

    let (rect, action) = app
        .regions
        .config_buttons
        .iter()
        .find(|(_, action)| *action == ConfigAction::CheckUpdate)
        .copied()
        .expect("the verbs were not published as buttons");
    let drawn: String = row_symbols(&screen, usize::from(rect.y))
        [usize::from(rect.x)..usize::from(rect.x + rect.width)]
        .concat();
    assert_eq!(drawn, format!(" {} ", action.label(app.strings)));

    app.click_config_control(rect.x + 2, rect.y)
        .expect("the press found no control");
    let pending = app
        .config_center
        .pending
        .get("codex")
        .expect("the press did not start the verb it was over");
    assert_eq!(pending.verb, ConfigVerb::CheckUpdate);
}

/// One Agent other than the built-in `codex`, so a matrix has two rows.
fn second_agent() -> vibex_core::AgentSnapshotEntry {
    serde_json::from_value(serde_json::json!({
        "id": "claude",
        "label": "Claude",
        "runtimeKind": "acp",
        "sourceKind": "builtin",
        "added": true,
        "enabled": true,
        "orderIndex": 1,
        "installed": true,
        "configured": true,
        "installStatus": "installed",
        "managedInstall": {"managed": false, "status": "external"},
        "configStatus": "configured",
        "runtimeStatus": "ready",
        "models": [],
        "modes": [],
        "capabilityHints": [],
        "env": {},
        "params": {},
        "diagnostics": [],
        "nativeConfigPaths": []
    }))
    .expect("agent fixture")
}

/// One model provider profile, as the Agent's own list reports it.
fn provider_profile(id: &str, name: &str) -> vibex_core::ProviderProfileSummary {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "agentId": "codex",
        "kind": "codex",
        "displayName": name,
        "status": "enabled",
        "accountAlias": "work",
        "defaultModel": "gpt-5",
        "configuredModels": [{
            "id": "gpt-5",
            "enabled": true,
            "capabilities": {}
        }],
        "secretSetupState": "missing",
        "updatedAtMs": 1
    }))
    .expect("provider fixture")
}

/// One registry entry, as the market page lists it.
fn market_entry(id: &str) -> vibex_core::McpMarketEntry {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": "Trace tools",
        "description": "Inspect a trace",
        "transport": "stdio",
        "command": "npx",
        "args": ["-y", "@example/trace"],
        "env": [{
            "name": "TRACE_TOKEN",
            "required": true,
            "secret": true,
            "defaultValue": "from-registry"
        }],
        "version": "1.0.0",
        "author": "example",
        "packageKind": "npm",
        "status": "active"
    }))
    .expect("market fixture")
}

/// One installable Skill, as the Skill index lists it.
fn skill_market_entry(id: &str) -> vibex_core::SkillMarketEntry {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "slug": id,
        "ownerHandle": "example",
        "name": "Trace reader",
        "summary": "Read a trace",
        "version": "1.0.0",
        "installs": 3,
        "stars": 1,
        "downloads": 9
    }))
    .expect("skill market fixture")
}

/// The action the menu offers, chosen the way the reader chooses it.
fn choose_action(app: &mut App, action: ConfigAction) {
    let Some(Overlay::EntryActions { mut entry }) = app.overlay.clone() else {
        panic!("the action menu was not open");
    };
    entry.selected = entry
        .actions
        .iter()
        .position(|offered| *offered == action)
        .unwrap_or_else(|| panic!("the record does not offer {action:?}"));
    app.overlay = Some(Overlay::EntryActions { entry });
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);
}

/// `Edit` in the action menu is the editor, not a verb with no write behind it.
///
/// The menu is the surface's front door — `Enter` opens it on every record — so
/// a verb it offers and the reducer does not answer is a key that does nothing
/// at all. It is offered by four modules, and each of them has an editor to
/// open.
#[test]
fn the_action_menu_opens_the_editor_for_the_record_it_was_opened_on() {
    for (module, section, expected) in [
        (ConfigModule::Mcp, ConfigSection::Servers, "Filesystem"),
        (ConfigModule::Prompts, ConfigSection::Prompts, "Review"),
    ] {
        let mut app = loaded(140, 40);
        app.show_config_module(module, Some(section));
        app.perform(vibex_tui::action::Intent::ConfigEntryActions);
        choose_action(&mut app, ConfigAction::Edit);
        let Some(Overlay::Form { form }) = app.overlay.clone() else {
            panic!("{module:?}: Edit opened no editor");
        };
        assert_eq!(
            form.value("name").as_deref(),
            Some(expected),
            "{module:?}: the editor opened on another record"
        );
    }
}

/// A provider's secret is written to the profile the reader pressed on.
///
/// The prompt is one line of text with no room for the record it is about, so
/// the target travels with it. Reading it back from the list cursor instead
/// would send the key to whichever profile happened to be under the cursor when
/// the reader pressed `Enter` — and a read landing while they type is enough to
/// move it.
#[test]
fn a_provider_secret_is_written_to_the_profile_it_was_asked_for() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![
        provider_profile("provider_first", "First"),
        provider_profile("provider_second", "Second"),
    ];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    let outcome = app.run_config_action_by_key("provider_second", ConfigAction::Secret);
    assert!(outcome.effects.is_empty(), "the prompt wrote something");
    let Some(Overlay::Prompt { target, .. }) = app.overlay.clone() else {
        panic!("the secret prompt did not open");
    };
    assert_eq!(target.as_deref(), Some("provider_second"));

    // The list moving under the prompt must not move the write.
    app.set_selection(vibex_tui::keymap::Scope::Providers, 0);
    if let Some(Overlay::Prompt { value, .. }) = app.overlay.as_mut() {
        *value = "sk-sentinel".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::WriteProviderSecret { profile_id, secret }] => {
            assert_eq!(profile_id.as_str(), "provider_second");
            assert_eq!(secret, "sk-sentinel");
        }
        other => panic!("the secret produced {other:?}"),
    }
}

/// `Validate` on a registry entry checks the launcher that would be installed.
///
/// The entry has no record yet, so the check has to carry the candidate the
/// registry published. A menu that offers the verb and answers nothing is worse
/// than one that does not offer it: the reader is told the entry is fine by a
/// client that never looked.
#[test]
fn a_registry_entry_can_be_checked_before_it_is_installed() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    let outcome = app.run_config_action_by_key("registry_trace", ConfigAction::Validate);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            assert_eq!(mutation.label(), "mcp_market_validate");
            let vibex_tui::app::ConfigMutation::ValidateMcpMarketEntry { candidate, .. } = mutation
            else {
                panic!("the check did not carry a candidate: {mutation:?}");
            };
            assert_eq!(candidate.command.as_deref(), Some("npx"));
            assert_eq!(
                candidate
                    .env
                    .iter()
                    .map(|entry| (entry.name.as_str(), entry.value.as_str()))
                    .collect::<Vec<_>>(),
                vec![("TRACE_TOKEN", "from-registry")]
            );
        }
        other => panic!("the check produced {other:?}"),
    }
}

/// An Agent the record never held can be switched on in the matrix form.
///
/// The form draws every Agent the client knows about, so it is the set the
/// matrix is rebuilt from: rebuilding it out of the record's own entries would
/// silently drop the one press the reader made, which is the only reason they
/// opened the form.
#[test]
fn the_matrix_form_enables_an_agent_the_record_did_not_hold() {
    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent(), second_agent()];
    app.show_config_module(ConfigModule::Mcp, None);
    // The fixture server is assigned to `codex` alone.
    app.perform(vibex_tui::action::Intent::ConfigEntryActions);
    choose_action(&mut app, ConfigAction::Matrix);
    let Some(Overlay::Form { form }) = app.overlay.as_mut() else {
        panic!("the matrix form did not open");
    };
    let field = form
        .fields
        .iter_mut()
        .find(|field| field.key == "agent:claude")
        .expect("the form did not draw the second Agent");
    field.value = "on".to_string();

    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SetMcpAgentMatrix { matrix, .. } = mutation else {
                panic!("the save was not a matrix write: {mutation:?}");
            };
            let claude = matrix
                .iter()
                .find(|entry| entry.agent_id.as_str() == "claude")
                .expect("the Agent that was switched on is missing from the write");
            assert!(claude.enabled);
            // And the assignment the record already had is left as it was.
            let codex = matrix
                .iter()
                .find(|entry| entry.agent_id.as_str() == "codex")
                .expect("the existing assignment was dropped");
            assert!(codex.enabled);
            assert_eq!(codex.updated_at_ms, 1);
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// A registry page says how much of the registry it is, and lets the reader ask
/// for another one.
///
/// Forty rows of a catalogue the runtime is still walking must not read as the
/// whole of it, and the two narrowings the registries offer have to be reachable
/// — a control drawn with no key behind it is a promise the page does not keep.
/// The line belongs to the registry areas only: the installed areas have no
/// catalogue behind them.
#[test]
fn a_registry_page_says_how_much_of_the_registry_it_covers() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: false,
        cursor: None,
        error: None,
    });
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));
    let screen = text(&mut app, 140, 40);
    assert!(
        screen.contains("showing 1 of 312 registry entries"),
        "the page did not say how much it covers:\n{screen}"
    );
    assert!(
        screen.contains("still indexing"),
        "the page claimed to be the whole registry:\n{screen}"
    );
    // The bar names the question the page was asked with.
    assert!(screen.contains("Transport"), "{screen}");
    assert!(screen.contains("All"), "{screen}");
    // 312 matches at 20 a page is sixteen pages, and the reader is on the first.
    assert!(screen.contains("1/16"), "{screen}");

    // The installed area has no catalogue behind it, so it has no such line.
    // The marker is the page indicator rather than the word "Transport": that
    // word is also the name of one of this table's own columns.
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Servers));
    let screen = text(&mut app, 140, 40);
    assert!(!screen.contains("registry entries"), "{screen}");
    assert!(!screen.contains("1/16"), "{screen}");
}

/// `t` narrows the MCP page and `s` ranks the Skill one, each from the first
/// page and by asking the registry again.
#[test]
fn the_registry_narrowings_ask_the_registry_again() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    // MCP is narrowed by transport, and the cycle runs All → Local → Remote.
    app.perform(vibex_tui::action::Intent::ConfigRegistryFilter);
    assert_eq!(
        app.config_center.registry.transport,
        Some(vibex_tui::config_center::McpServerTransportFilter::Local)
    );
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryFilter);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::SearchRegistry { registry, .. }] => assert_eq!(
            registry.transport_filter(),
            Some(vibex_core::McpMarketTransportFilter::Remote)
        ),
        other => panic!("the narrowing did not ask again: {other:?}"),
    }
    app.perform(vibex_tui::action::Intent::ConfigRegistryFilter);
    assert_eq!(app.config_center.registry.transport, None);

    // The transport key means nothing to the Skill index, which is narrowed by
    // ranking instead.
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));
    app.perform(vibex_tui::action::Intent::ConfigRegistryFilter);
    assert_eq!(app.config_center.registry.transport, None);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistrySort);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::SearchRegistry { registry, .. }] => assert_eq!(
            registry.sort.wire(),
            vibex_core::SkillMarketSort::Downloads,
            "the ranking did not move"
        ),
        other => panic!("the ranking did not ask again: {other:?}"),
    }
}

/// Moving a page moves the question, and the cursor comes with it.
#[test]
fn the_registry_pages_step_within_what_the_registry_answered() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: false,
        cursor: None,
        error: None,
    });
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));
    app.config_center.set_cursor(3);

    // MCP is paged by the authority, so the next page is a read.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert_eq!(app.config_center.registry.page, 1);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::SearchRegistry { registry, .. }] => {
            assert_eq!(registry.page, 1);
        }
        other => panic!("the page turn did not read: {other:?}"),
    }
    // The cursor belongs to the page it was chosen on.
    assert_eq!(app.config_center.cursor(10), 0);

    // And the first page has nothing before it.
    app.perform(vibex_tui::action::Intent::ConfigRegistryPagePrevious);
    assert_eq!(app.config_center.registry.page, 0);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryPagePrevious);
    assert!(outcome.effects.is_empty(), "the page ran off the front");
    assert_eq!(app.config_center.registry.page, 0);
}

/// The Skill index hands one window over, so its pages are cut here.
#[test]
fn the_skill_registry_pages_the_window_it_was_handed() {
    let mut app = loaded(140, 40);
    app.management_data.skill_market = (0..45)
        .map(|index| skill_market_entry(&format!("skill_{index}")))
        .collect();
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(45),
        exhausted: true,
        cursor: None,
        error: None,
    });
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));

    // Three pages of twenty, twenty and five.
    assert_eq!(app.config_center.registry_pages(&app.management_data), 3);
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        20
    );
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert!(
        outcome.effects.is_empty(),
        "a page the client cuts is a redraw, not a read"
    );
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        20
    );
    app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert_eq!(
        app.config_center
            .visible(&app.management_data, app.strings)
            .len(),
        5
    );
    // The last page is the last one.
    app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert_eq!(app.config_center.registry.page, 2);
}

/// Installing a registry entry is a form, not a press.
///
/// The registry publishes a launcher and the variables it needs; it never
/// publishes the reader's own token for them, and it has no idea which Agents
/// should receive the server. Both are asked for, the command that will be saved
/// is read back, and what is written is the entry the form showed rather than a
/// second reading of a registry that may have moved on.
#[test]
fn a_registry_entry_is_installed_through_a_form() {
    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent(), second_agent()];
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    let outcome = app.run_config_action_by_key("registry_trace", ConfigAction::Install);
    assert!(outcome.effects.is_empty(), "the install ran unasked");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the install form did not open");
    };
    assert_eq!(form.preview.as_deref(), Some("npx -y @example/trace"));
    let token = form
        .field("env:TRACE_TOKEN")
        .expect("the variable the registry asked for was not offered");
    assert!(
        token.secret,
        "a secret requirement was offered in the clear"
    );
    assert_eq!(token.value, "from-registry");
    assert_eq!(
        form.enabled_agents(),
        vec!["codex".to_string(), "claude".to_string()],
        "every added Agent should start switched on"
    );

    // Nothing secret is echoed: the value is masked where it is drawn.
    let screen = text(&mut app, 140, 40);
    assert!(
        !screen.contains("from-registry"),
        "a secret requirement was echoed to the screen:\n{screen}"
    );

    // The reader fills the token in and turns the second Agent off.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("env:TRACE_TOKEN")
            .expect("token field")
            .value = "s3cret".to_string();
        // The second Agent, which is the one the assertion below is about.
        form.field_mut("agent:claude").expect("agent field").value = "off".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::InstallMcpMarketEntry {
                env_values,
                agent_ids,
                candidate,
                ..
            } = mutation
            else {
                panic!("the save was not an install: {mutation:?}");
            };
            assert_eq!(
                env_values
                    .iter()
                    .map(|entry| (entry.name.as_str(), entry.value.as_str()))
                    .collect::<Vec<_>>(),
                vec![("TRACE_TOKEN", "s3cret")]
            );
            assert_eq!(
                agent_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                vec!["codex"],
                "the Agent that was switched off is still in the write"
            );
            assert_eq!(
                candidate.env, *env_values,
                "the launcher and the request disagree about the environment"
            );
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// The command the form is about to save can be taken away.
#[test]
fn a_form_hands_over_the_command_it_is_about_to_save() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));
    app.run_config_action_by_key("registry_trace", ConfigAction::Install);

    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormCopy);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::Clipboard { text }] => {
            assert_eq!(text, "npx -y @example/trace");
        }
        other => panic!("the copy produced {other:?}"),
    }

    // A form with nothing to read back copies nothing.
    app.overlay = None;
    app.show_config_module(ConfigModule::Mcp, None);
    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormCopy);
    assert!(
        outcome.effects.is_empty(),
        "a plain record editor copied something"
    );
}

/// A registry whose index is still widening is asked again on its own clock.
///
/// The runtime walks the registry on its own time and nothing arrives to say it
/// grew, so a page that says "still indexing" and then never asks again is a
/// sentence the client stopped checking. A finished index, and an area that is
/// not a registry, stop the clock.
#[test]
fn a_widening_registry_is_asked_again_without_a_key() {
    let mut app = loaded(140, 40);
    app.management_data.mcp_market = vec![market_entry("registry_trace")];
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: false,
        cursor: None,
        error: None,
    });
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    let asked = |app: &mut App| {
        app.tick().effects.iter().any(|effect| {
            matches!(
                effect,
                vibex_tui::app::Effect::SearchRegistry { module, .. }
                    if *module == ConfigModule::Mcp
            )
        })
    };

    // The clock starts on the page that was just read rather than re-reading it.
    assert!(
        !asked(&mut app),
        "the page was read again the moment it opened"
    );
    let mut fired = 0;
    for _ in 0..vibex_tui::config_center::REGISTRY_POLL_TICKS * 2 {
        if asked(&mut app) {
            fired += 1;
        }
    }
    assert_eq!(fired, 2, "the registry was not asked on its own clock");

    // A finished index stops the clock.
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: true,
        cursor: None,
        error: None,
    });
    app.config_center.registry_poll_ticks = 0;
    for _ in 0..vibex_tui::config_center::REGISTRY_POLL_TICKS * 2 {
        assert!(!asked(&mut app), "a finished index was polled anyway");
    }

    // So does leaving the registry.
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(312),
        exhausted: false,
        cursor: None,
        error: None,
    });
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Servers));
    for _ in 0..vibex_tui::config_center::REGISTRY_POLL_TICKS * 2 {
        assert!(!asked(&mut app), "the installed list was polled");
    }
}

/// A profile can be made from nothing, which no other record in the centre can.
///
/// The reader picks the Agent the profile belongs to — the authority mints the
/// id and derives the kind from that Agent — names it, and lists the Models it
/// offers. The Agents offered are the ones the runtime says can hold a provider
/// configuration, so the form cannot ask for something the save would refuse.
#[test]
fn a_provider_profile_can_be_made_from_nothing() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    // `n` opens the preset list first: a preset is the shortcut past knowing an
    // address, a protocol and a model id. The form after it is the same form.
    app.perform(vibex_tui::action::Intent::ConfigEntryCreate);
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::ProviderPresetPicker { selected: 0, .. })
        ),
        "`n` did not open the preset list: {:?}",
        app.overlay
    );
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("choosing a preset opened no form");
    };
    assert!(matches!(
        form.purpose,
        vibex_tui::config_center::FormPurpose::NewProviderProfile { source: None }
    ));
    assert!(
        !form.value("agent").unwrap_or_default().is_empty(),
        "the form did not offer an Agent to belong to"
    );
    // The preset filled the address and the model list, and the protocol it
    // chose is written into the options so a reopen cannot relabel it.
    assert!(
        !form.value("name").unwrap_or_default().is_empty(),
        "the preset left the form nameless"
    );
    assert!(
        !form.value("base_url").unwrap_or_default().is_empty(),
        "the preset left the address empty"
    );
    assert!(
        !form.value("models").unwrap_or_default().is_empty(),
        "the preset left the model list empty"
    );

    // The reader renames it and lists two Models of their own.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("name").unwrap().value = "Work account".to_string();
        form.field_mut("base_url").unwrap().value = "https://api.example/v1".to_string();
        form.field_mut("models").unwrap().value = "gpt-5\ngpt-5-mini".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::CreateProviderProfile(request) = mutation else {
                panic!("the save was not a creation: {mutation:?}");
            };
            assert_eq!(request.display_name, "Work account");
            assert_eq!(request.base_url.as_deref(), Some("https://api.example/v1"));
            assert_eq!(
                request
                    .configured_models
                    .iter()
                    .map(|model| (model.id.as_str(), model.enabled))
                    .collect::<Vec<_>>(),
                vec![("gpt-5", true), ("gpt-5-mini", true)],
                "the listed Models are not what would be written"
            );
        }
        other => panic!("the save produced {other:?}"),
    }

    // A profile with no name is refused before anything is sent.
    app.overlay = None;
    app.perform(vibex_tui::action::Intent::ConfigEntryCreate);
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("name").unwrap().value.clear();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a nameless profile was written");
}

/// One profile as the authority holds it, which is what an editor needs.
fn provider_profile_detail() -> vibex_core::ProviderProfile {
    serde_json::from_value(serde_json::json!({
        "id": "provider_first",
        "agentId": "codex",
        "kind": "codex",
        "displayName": "First",
        "status": "enabled",
        "accountAlias": "work",
        "baseUrl": "https://api.example/v1",
        "defaultModel": "gpt-5",
        "configuredModels": [{"id": "gpt-5", "enabled": true, "capabilities": {}}],
        "sandboxDefaults": {
            "permissionMode": "default",
            "askOnRisk": true,
            "bypassAllPermissions": false
        },
        "networkDefaults": {"allowNetwork": true},
        "permissionDefaults": {
            "fileRead": "allow",
            "fileWrite": "ask",
            "command": "ask",
            "network": "ask"
        },
        "providerOptions": {
            "schemaVersion": 1,
            "entries": [
                {"key": "ccSwitchWebsiteUrl", "value": "https://example.test"},
                {"key": "nativeSource", "value": "cc-switch"},
                {"key": "headerOverrideJson", "value": "{\"X-Trace\":\"on\",\"x-opencode-session\":{\"variable\":\"session_id\"}}"},
                {"key": "protocolBaseUrl.anthropic_messages", "value": "https://anthropic.example/v1"}
            ]
        },
        "secrets": [],
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("profile detail fixture")
}

/// A copy is written from the profile itself, not from the summary of it.
///
/// The list answers with a summary, which carries the name, the account and the
/// Models but neither the endpoint nor the request options. A copy written from
/// that would be a profile that launches nowhere, so the surface waits for the
/// full read: the press asks for it, and the answer is what opens the form.
#[test]
fn a_copy_is_written_from_the_whole_profile() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    // The press is a read first: the record it is about has not been read.
    let outcome = app.run_config_action_by_key("provider_first", ConfigAction::Duplicate);
    match outcome.effects.as_slice() {
        [
            vibex_tui::app::Effect::LoadProviderProfile {
                agent_id,
                profile_id,
            },
        ] => {
            assert_eq!(agent_id.as_str(), "codex");
            assert_eq!(profile_id.as_str(), "provider_first");
        }
        other => panic!("the copy did not read the profile first: {other:?}"),
    }
    assert_eq!(
        app.pending_provider_open.as_ref().map(|(_, open)| *open),
        Some(vibex_tui::app::ProviderOpen::Duplicate)
    );

    // The answer is what opens the form, and it carries what the summary could
    // not: the endpoint, and the request options besides.
    app.management_data.provider_detail = Some(provider_profile_detail());
    app.show_provider_open(
        &vibex_core::ProviderProfileId::parse("provider_first").unwrap(),
        vibex_tui::app::ProviderOpen::Duplicate,
    );
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the answer opened no form");
    };
    assert_eq!(form.value("name").as_deref(), Some("First copy"));
    assert_eq!(form.value("account_alias").as_deref(), Some("work"));
    assert_eq!(form.value("models").as_deref(), Some("gpt-5"));
    assert_eq!(
        form.value("base_url").as_deref(),
        Some("https://api.example/v1"),
        "the copy could not read the endpoint it is copying"
    );

    // And the save writes them, rather than dropping what the form has no field
    // for.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::CreateProviderProfile(request) = mutation else {
                panic!("the copy was not a creation: {mutation:?}");
            };
            assert_eq!(request.base_url.as_deref(), Some("https://api.example/v1"));
            assert!(
                request
                    .provider_options
                    .as_ref()
                    .is_some_and(|options| options
                        .entries
                        .iter()
                        .any(|entry| entry.key == "nativeSource")),
                "the request options were dropped by the copy"
            );
            assert!(
                request.secret_references.is_empty(),
                "a copy must not share the credential it was copied from"
            );
        }
        other => panic!("the copy produced {other:?}"),
    }
}

/// Editing a profile needs the same read, for the same reason.
#[test]
fn editing_a_profile_reads_it_before_it_opens() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor did not open");
    };
    // A summary has no endpoint, so the field is empty and the form says so:
    // an empty field leaves what is stored alone.
    assert_eq!(form.value("base_url").as_deref(), Some(""));
    assert!(form.field("base_url").unwrap().hint.is_some());
}

/// The Models a profile offers are edited as a list inside the record.
///
/// A profile declares the Models it offers and an endpoint answers with the ones
/// it serves, and those are two different things: an entry the endpoint lists and
/// the profile does not offer yet is a row the reader can turn on rather than a
/// line in a report. Every press writes through the profile it belongs to, so the
/// list is never a draft that a re-read can contradict.
#[test]
fn a_profiles_models_are_edited_through_the_profile() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.management_data.provider_models = vec![
        vibex_core::ProviderConfiguredModel {
            id: "gpt-5".to_string(),
            display_name: None,
            enabled: true,
            wire_api: None,
            capabilities: vibex_core::ProviderModelCapabilities::default(),
        },
        vibex_core::ProviderConfiguredModel {
            id: "gpt-5-mini".to_string(),
            display_name: Some("Mini".to_string()),
            enabled: true,
            wire_api: None,
            capabilities: vibex_core::ProviderModelCapabilities::default(),
        },
    ];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::ConfigModels);
    let Some(Overlay::Models { view }) = app.overlay.clone() else {
        panic!("`m` opened no Model list");
    };
    assert_eq!(view.profile_id, "provider_first");

    // The declared Model comes first, and the catalogue entry the profile does
    // not offer is offered as an available row after it.
    let outcome = app.perform(vibex_tui::action::Intent::SelectNext);
    assert!(
        outcome.effects.is_empty(),
        "moving the cursor wrote something"
    );
    let outcome = app.perform(vibex_tui::action::Intent::ConfigModelToggle);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the toggle was not a Model write: {mutation:?}");
            };
            assert_eq!(
                models
                    .iter()
                    .map(|model| (model.id.as_str(), model.enabled))
                    .collect::<Vec<_>>(),
                vec![("gpt-5", true), ("gpt-5-mini", true)],
                "turning on a catalogue entry did not declare it"
            );
        }
        other => panic!("the toggle produced {other:?}"),
    }
    // And the editor stays open over the write.
    assert!(matches!(app.overlay, Some(Overlay::Models { .. })));

    // A record carries one write at a time, so the next press waits for this
    // one to land. There is no worker here to answer it, so the test retires it
    // the way an answer would.
    app.config_center.pending.clear();

    // `d` takes a declared Model out.
    app.overlay = Some(Overlay::Models {
        view: vibex_tui::config_center::ModelListView {
            agent_id: "codex".to_string(),
            profile_id: "provider_first".to_string(),
            title: "First".to_string(),
            selected: 0,
            notice: None,
        },
    });
    let outcome = app.perform(vibex_tui::action::Intent::ConfigModelRemove);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the removal was not a Model write: {mutation:?}");
            };
            assert!(models.is_empty(), "the Model was not taken out: {models:?}");
        }
        other => panic!("the removal produced {other:?}"),
    }

    // `f` asks the endpoint, and it is the profile that is asked about.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigModelFetch);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::FetchProviderModels { profile_id }] => {
            assert_eq!(profile_id.as_str(), "provider_first");
        }
        other => panic!("the fetch produced {other:?}"),
    }
}

/// Adding a Model by hand names the profile it belongs to.
#[test]
fn a_model_typed_by_hand_is_added_to_the_profile_it_was_asked_for() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    app.perform(vibex_tui::action::Intent::ConfigModels);
    app.perform(vibex_tui::action::Intent::ConfigModelAdd);

    let Some(Overlay::Prompt { field, target, .. }) = app.overlay.clone() else {
        panic!("`a` opened no prompt");
    };
    assert_eq!(field, vibex_tui::app::PromptField::NewModelId);
    assert_eq!(target.as_deref(), Some("provider_first"));

    if let Some(Overlay::Prompt { value, .. }) = app.overlay.as_mut() {
        *value = "  gpt-6  ".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the add was not a Model write: {mutation:?}");
            };
            assert_eq!(models.len(), 2, "the typed Model was not added: {models:?}");
            assert_eq!(models[1].id, "gpt-6", "the id was not trimmed");
        }
        other => panic!("the add produced {other:?}"),
    }
    // The reader is put back where they were: the list they were editing.
    assert!(matches!(app.overlay, Some(Overlay::Models { .. })));
}

/// One Model's settings are edited on the Model, and written through the profile.
///
/// A Model is not a record: its name, its protocol, whether it reasons and the
/// limits it declares belong to the entry in the profile's list. The thinking
/// levels are read by the authority's own parser, so the form accepts exactly
/// what a thinking-depth control can consume and refuses the rest with a
/// sentence rather than writing a table dispatch would choke on.
#[test]
fn one_models_settings_are_edited_and_written_through_the_profile() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    app.perform(vibex_tui::action::Intent::ConfigModels);
    app.perform(vibex_tui::action::Intent::ConfigModelEdit);

    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("`e` opened no settings form");
    };
    assert_eq!(form.value("wire_api").as_deref(), Some("inherit"));
    assert_eq!(form.value("reasoning").as_deref(), Some("inherit"));

    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("display_name").unwrap().value = "GPT-5".to_string();
        form.field_mut("wire_api").unwrap().value = "anthropic_messages".to_string();
        form.field_mut("reasoning").unwrap().value = "reasoning".to_string();
        form.field_mut("context_tokens").unwrap().value = "200000".to_string();
        form.field_mut("reasoning_efforts").unwrap().value = "low, high, max".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the save was not a Model write: {mutation:?}");
            };
            assert_eq!(models.len(), 1, "the settings changed the number of Models");
            let model = &models[0];
            assert_eq!(model.display_name.as_deref(), Some("GPT-5"));
            assert_eq!(
                model.wire_api,
                Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
            );
            assert_eq!(model.capabilities.reasoning, Some(true));
            assert_eq!(model.capabilities.context_tokens, Some(200_000));
            assert_eq!(model.capabilities.output_tokens, None);
            let levels = model
                .capabilities
                .reasoning_efforts
                .as_ref()
                .expect("the levels were dropped")
                .declared_levels()
                .map(|(level, _)| level.as_str())
                .collect::<Vec<_>>();
            assert_eq!(levels, vec!["low", "high", "max"]);
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// A limit that is not a number, and a level that is not a level, are refused
/// with the reason on screen rather than written as something else.
#[test]
fn a_model_setting_the_authority_could_not_read_is_refused() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    app.perform(vibex_tui::action::Intent::ConfigModels);
    app.perform(vibex_tui::action::Intent::ConfigModelEdit);

    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("context_tokens").unwrap().value = "a lot".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a bad limit was written");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on an invalid save");
    };
    assert!(form.error.is_some(), "no reason was given");
    let screen = text(&mut app, 140, 40);
    assert!(screen.contains("whole numbers"), "{screen}");

    // A level the parser cannot read is refused too, and the form stays open.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("context_tokens").unwrap().value.clear();
        form.field_mut("reasoning_efforts").unwrap().value = "quite a lot".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a bad level was written");
    assert!(matches!(app.overlay, Some(Overlay::Form { .. })));
}

/// A profile is moved a place at a time, and the whole order is what is written.
///
/// The authority lists profiles in its own order and the reader's arrangement is
/// a separate record, so the rows follow that record when this client has read
/// it — and a move writes the list it produced rather than one index, because an
/// order that named only the moved profile would say nothing about the ones it
/// moved past.
#[test]
fn a_profile_is_moved_within_its_agents_order() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![
        provider_profile("provider_first", "First"),
        provider_profile("provider_second", "Second"),
    ];
    // The reader's arrangement puts the second one first.
    app.management_data.provider_order = vec![
        serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_second",
            "orderIndex": 0,
            "updatedAtMs": 1
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_first",
            "orderIndex": 1,
            "updatedAtMs": 1
        }))
        .unwrap(),
    ];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    // The rows follow the arrangement rather than the order the authority
    // happened to list them in.
    let rows = app.management_data.providers.clone();
    let visible = app.config_center.visible(&app.management_data, app.strings);
    assert_eq!(
        visible
            .iter()
            .map(|row| row.key.as_str())
            .collect::<Vec<_>>(),
        vec!["provider_second", "provider_first"],
        "the rows did not follow the reader's arrangement (listed {rows:?})"
    );

    // Moving the first one down swaps the two, and the write is the whole order.
    let outcome = app.run_config_action_by_key("provider_second", ConfigAction::MoveDown);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SetProviderOrder { agent_id, profiles } = mutation
            else {
                panic!("the move was not an order write: {mutation:?}");
            };
            assert_eq!(agent_id.as_str(), "codex");
            assert_eq!(
                profiles.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                vec!["provider_first", "provider_second"]
            );
        }
        other => panic!("the move produced {other:?}"),
    }

    // At the end of the list there is nowhere to go.
    app.config_center.pending.clear();
    app.management_data.provider_order = vec![
        serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_second",
            "orderIndex": 0,
            "updatedAtMs": 1
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_first",
            "orderIndex": 1,
            "updatedAtMs": 1
        }))
        .unwrap(),
    ];
    let outcome = app.run_config_action_by_key("provider_first", ConfigAction::MoveDown);
    assert!(outcome.effects.is_empty(), "the last profile moved down");
}

/// The arrangement is read per Agent, beside the list it arranges.
#[test]
fn reading_the_providers_area_reads_the_arrangement_too() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![
        provider_profile("provider_first", "First"),
        provider_profile("provider_second", "Second"),
    ];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    let outcome = app.perform(vibex_tui::action::Intent::ReloadManagement);
    let reads = outcome
        .effects
        .iter()
        .filter(|effect| matches!(effect, vibex_tui::app::Effect::LoadProviderOrder { .. }))
        .count();
    assert_eq!(
        reads, 1,
        "the arrangement was read {reads} times for one Agent: {:#?}",
        outcome.effects
    );
}

/// The provider's options are edited over the bag the profile shares.
///
/// The options are a key/value bag other writers also use — where the profile
/// was imported from, which switch app type it came from — so the form owns
/// three keys and leaves every other entry exactly as it found it. A field that
/// wrote the whole bag would delete a record it never showed the reader.
#[test]
fn a_providers_options_are_edited_over_the_bag_it_shares() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.management_data.provider_detail = Some(provider_profile_detail());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor did not open");
    };
    assert_eq!(
        form.value("website").as_deref(),
        Some("https://example.test")
    );
    assert_eq!(
        form.value("headers").as_deref(),
        Some("X-Trace: on\nx-opencode-session: {{session_id}}"),
        "the headers were not read back in the shape they are written"
    );
    assert_eq!(
        form.value("protocol_urls").as_deref(),
        Some("anthropic_messages=https://anthropic.example/v1")
    );

    // The reader adds a header and a second protocol address.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("headers").unwrap().value =
            "X-Trace: on\nx-opencode-session: {{session_id}}\nX-Team: vibex".to_string();
        form.field_mut("protocol_urls").unwrap().value =
            "anthropic_messages=https://anthropic.example/v1\nopenai_responses=https://openai.example/v1"
                .to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                provider_options: Some(options),
                ..
            } = mutation
            else {
                panic!("the save wrote no options: {mutation:?}");
            };
            let entry = |key: &str| {
                options
                    .entries
                    .iter()
                    .find(|entry| entry.key == key)
                    .map(|entry| entry.value.as_str())
            };
            assert_eq!(entry("ccSwitchWebsiteUrl"), Some("https://example.test"));
            assert_eq!(
                entry("nativeSource"),
                Some("cc-switch"),
                "a key the form does not own was deleted"
            );
            assert_eq!(
                entry("protocolBaseUrl.openai_responses"),
                Some("https://openai.example/v1")
            );
            // The variable stays a variable and the literal stays a literal.
            let headers: serde_json::Value =
                serde_json::from_str(entry("headerOverrideJson").expect("headers")).unwrap();
            assert_eq!(headers["X-Trace"], serde_json::json!("on"));
            assert_eq!(headers["X-Team"], serde_json::json!("vibex"));
            assert_eq!(
                headers["x-opencode-session"],
                serde_json::json!({"variable": "session_id"})
            );
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// A header whose value is a fillable variable has to name one the runtime
/// fills, and a gateway address turns the session header on by itself.
#[test]
fn a_provider_option_the_runtime_could_not_read_is_refused_or_completed() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.management_data.provider_detail = Some(provider_profile_detail());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    // A variable the runtime does not fill is refused rather than sent as the
    // literal text of a placeholder.
    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("headers").unwrap().value = "X-Trace: {{session}}".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a bad variable was written");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on an invalid save");
    };
    assert!(form.error.is_some(), "no reason was given");

    // An address that routes by the session gets the header that gateway reads,
    // without the reader having to know it exists.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("headers").unwrap().value.clear();
        form.field_mut("base_url").unwrap().value = "https://opencode.ai/zen/go/v1".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                provider_options: Some(options),
                ..
            } = mutation
            else {
                panic!("the save wrote no options: {mutation:?}");
            };
            let headers = options
                .entries
                .iter()
                .find(|entry| entry.key == "headerOverrideJson")
                .map(|entry| entry.value.clone())
                .expect("the session header was not added");
            let headers: serde_json::Value = serde_json::from_str(&headers).unwrap();
            assert_eq!(
                headers["x-opencode-session"],
                serde_json::json!({"variable": "session_id"}),
                "the gateway's session header was not enabled"
            );
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// A key can be taken away, not only replaced.
///
/// Writing a credential was the only verb; a reader who wanted the profile to
/// hold none had no way to say so, and the field that holds one cannot be
/// cleared by writing an empty value because an empty value is not a key.
#[test]
fn a_profile_key_can_be_cleared() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    let outcome = app.run_config_action_by_key("provider_first", ConfigAction::ClearSecret);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ClearProviderSecret { profile_id }] => {
            assert_eq!(profile_id.as_str(), "provider_first");
        }
        other => panic!("clearing produced {other:?}"),
    }
}

/// The endpoint an Agent pins is shown and written, but not editable.
///
/// Some Agents reach a fixed service whatever a profile says. Letting the reader
/// type another address would ask for something the authority refuses, so the
/// field carries the address it will actually call and refuses to open an
/// editor, saying why on its own line.
#[test]
fn an_endpoint_an_agent_pins_is_not_the_readers_to_change() {
    let mut app = loaded(140, 40);
    // The pin comes from the runtime's own projection descriptors, so the test
    // asks which Agent has one rather than writing down a name that a manifest
    // change would move.
    let pinned_agent = vibex_core::model_provider_configurable_agent_ids()
        .unwrap_or_default()
        .into_iter()
        .find(|agent_id| vibex_tui::config_center::fixed_endpoint_for(agent_id).is_some());
    let Some(pinned_agent) = pinned_agent else {
        // No Agent pins its endpoint in this build, so there is nothing to pin.
        return;
    };
    let url = vibex_tui::config_center::fixed_endpoint_for(&pinned_agent).unwrap();

    let mut profile = provider_profile("provider_first", "First");
    profile.agent_id = pinned_agent.clone();
    app.management_data.providers = vec![profile];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor did not open");
    };
    let field = form.field("base_url").expect("the endpoint field");
    assert!(field.locked, "the pinned endpoint was offered as editable");
    assert_eq!(field.value, url, "the pinned endpoint was not shown");
    assert!(
        field
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains(&url)),
        "the form did not say why the address cannot be changed"
    );

    // `Enter` on it does not open an editor.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.selected = form
            .fields
            .iter()
            .position(|field| field.key == "base_url")
            .expect("the endpoint field is in the form");
    }
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed");
    };
    assert!(form.editing.is_none(), "a pinned field opened an editor");
}

/// One native-settings scan, as the authority answers it.
fn native_import_preview() -> vibex_core::ProviderNativeImportPreview {
    serde_json::from_value(serde_json::json!({
        "previewId": "request_import",
        "sources": ["codex", "cc_switch"],
        "files": [],
        "items": [
            {
                "importItemId": "request_one",
                "source": "codex",
                "providerKind": "codex",
                "agentId": "codex",
                "displayName": "Work account",
                "baseUrl": "https://api.example/v1",
                "defaultModel": "gpt-5",
                "configuredModels": [],
                "providerOptions": {"schemaVersion": 1, "entries": []},
                "secretReferences": [],
                "status": "importable",
                "redactedFields": [],
                "diagnostics": []
            },
            {
                "importItemId": "request_two",
                "source": "cc_switch",
                "providerKind": "claude",
                "displayName": "Broken account",
                "configuredModels": [],
                "providerOptions": {"schemaVersion": 1, "entries": []},
                "secretReferences": [],
                "status": "blocked_by_parse_error",
                "redactedFields": [],
                "diagnostics": [{"code": "parse_error", "message": "the file could not be read", "source": "cc_switch", "fileKind": null, "redactedDetails": []}]
            }
        ],
        "diagnostics": [],
        "createdAtMs": 1
    }))
    .expect("native import fixture")
}

/// The import picker is the Providers area's verb, and its rows are the scan.
///
/// A reader looks for settings to import before there is a profile to hang them
/// on, so the verb belongs to the area rather than to a record — and a candidate
/// the authority could not read is offered as a row that explains itself instead
/// of a press that fails.
#[test]
fn native_settings_are_offered_for_import() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    let outcome = app.perform(vibex_tui::action::Intent::ConfigImport);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::PreviewNativeImport { sources }] => assert_eq!(
            sources.len(),
            3,
            "the picker did not ask about the CLI tools a reader is likely to have"
        ),
        other => panic!("the import asked nothing: {other:?}"),
    }
    assert!(
        matches!(app.overlay, Some(Overlay::NativeImport { .. })),
        "the picker did not open"
    );

    // The scan answers with two candidates: one importable, one the authority
    // could not read.
    app.management_data.native_import = Some(native_import_preview());
    let rows = vibex_tui::config_center::native_import_rows(
        app.management_data.native_import.as_ref(),
        app.strings,
    );
    assert_eq!(rows.len(), 2);
    assert!(rows[0].importable);
    assert!(!rows[1].importable, "an unreadable candidate was offered");

    // Importing the first one names it and the sources it came from.
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::ImportNativeProfile { item_id, sources } = mutation
            else {
                panic!("the import was not an import: {mutation:?}");
            };
            assert_eq!(item_id, "request_one");
            assert_eq!(sources.len(), 3);
        }
        other => panic!("the import produced {other:?}"),
    }

    // And pressing it on the unreadable one writes nothing.
    app.config_center.pending.clear();
    app.overlay = Some(Overlay::NativeImport {
        view: vibex_tui::config_center::NativeImportView {
            selected: 1,
            loading: false,
            notice: None,
        },
    });
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    assert!(
        outcome.effects.is_empty(),
        "an unreadable candidate was sent"
    );
    assert!(app.toast.is_some(), "the picker refused without saying why");

    // The verb belongs to the Providers area: elsewhere it does nothing.
    app.overlay = None;
    app.show_config_module(ConfigModule::Mcp, None);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigImport);
    assert!(outcome.effects.is_empty(), "another area imported settings");
}

/// One export plan, as the authority answers a preview with.
fn native_export_preview() -> vibex_core::ProviderNativeExportPreview {
    serde_json::from_value(serde_json::json!({
        "exportId": "request_export",
        "providerProfileId": "provider_first",
        "source": "agent_default",
        "mode": "provider_profile",
        "files": [{
            "operationId": "request_file",
            "source": "agent_default",
            "fileKind": "codex_config_toml",
            "operationKind": "update_file",
            "targetPath": "/home/dev/.codex/config.toml",
            "redactedBefore": "model = \"gpt-4\"",
            "redactedAfter": "model = \"gpt-5\"",
            "redactedDiff": "-model = \"gpt-4\"\n+model = \"gpt-5\"",
            "rollbackPlan": "restore the backup",
            "diagnostics": [],
            "status": "ready"
        }],
        "diagnostics": [],
        "createdAtMs": 1
    }))
    .expect("native export fixture")
}

/// A profile can be written into another tool's own settings, and taken back.
///
/// The plan is the thing the reader agrees to, so Apply names the plan on screen
/// rather than deriving a second one: an export re-derived at apply time could
/// write something the reader never read. Changing the question drops the plan
/// for the same reason.
#[test]
fn a_profile_can_be_exported_to_another_tools_settings() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    // `x` opens the picker and asks what has been exported before.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigExport);
    assert!(
        outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, vibex_tui::app::Effect::NativeExportHistory { .. })),
        "the picker did not read the history: {:#?}",
        outcome.effects
    );
    let Some(Overlay::NativeExport { view }) = app.overlay.clone() else {
        panic!("the picker did not open");
    };
    assert_eq!(view.profile_id, "provider_first");
    assert_eq!(
        view.source,
        vibex_core::ProviderNativeExportSource::AgentDefault,
        "the picker did not open on the profile's own Agent"
    );

    // Applying with nothing previewed asks for the plan instead of writing one.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigExportApply);
    assert!(outcome.effects.is_empty(), "an unseen plan was applied");

    // A preview carries the question, and the answer is what Apply names.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigExportPreview);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::PreviewNativeExport { source, mode, .. }] => {
            assert_eq!(
                *source,
                vibex_core::ProviderNativeExportSource::AgentDefault
            );
            assert_eq!(*mode, vibex_core::ProviderNativeExportMode::ProviderProfile);
        }
        other => panic!("the preview asked nothing: {other:?}"),
    }
    if let Some(Overlay::NativeExport { view }) = app.overlay.as_mut() {
        view.preview = Some(Box::new(native_export_preview()));
    }

    // The files the plan would write are listed, with the difference.
    let files = vibex_tui::config_center::native_export_file_rows(
        match &app.overlay {
            Some(Overlay::NativeExport { view }) => view.preview.as_deref(),
            _ => None,
        },
        app.strings,
    );
    assert_eq!(files.len(), 1);
    assert!(files[0].diff.contains("+model = \"gpt-5\""));

    let outcome = app.perform(vibex_tui::action::Intent::ConfigExportApply);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::ApplyNativeExport { export_id } = mutation else {
                panic!("the apply named something else: {mutation:?}");
            };
            assert_eq!(export_id, "request_export");
        }
        other => panic!("the apply produced {other:?}"),
    }

    // Changing the question drops the plan: it describes the old one.
    app.config_center.pending.clear();
    app.perform(vibex_tui::action::Intent::ConfigExportSource);
    let Some(Overlay::NativeExport { view }) = app.overlay.clone() else {
        panic!("the picker closed");
    };
    assert!(view.preview.is_none(), "a plan for another target was kept");
    assert_ne!(
        view.source,
        vibex_core::ProviderNativeExportSource::AgentDefault
    );
}

/// The export belongs to a record, so it does nothing away from the profiles.
#[test]
fn the_export_verb_belongs_to_a_profile() {
    let mut app = loaded(140, 40);
    app.show_config_module(ConfigModule::Mcp, None);
    let outcome = app.perform(vibex_tui::action::Intent::ConfigExport);
    assert!(
        outcome.effects.is_empty() && app.overlay.is_none(),
        "another area exported settings"
    );
}

/// One Agent's account catalogue, as the authority answers it.
fn agent_auth_catalog() -> vibex_core::AgentAuthCatalog {
    serde_json::from_value(serde_json::json!({
        "agentId": "codex",
        "methods": [
            {
                "id": "chatgpt",
                "name": "Sign in with ChatGPT",
                "description": "Opens a browser",
                "kind": "agent",
                "effect": "writes_agent_state_home",
                "environment": [{
                    "name": "OPENAI_API_KEY",
                    "label": "API key",
                    "secret": true,
                    "optional": false,
                    "configured": true
                }],
                "credentialLink": "https://example.test/keys"
            },
            {
                "id": "shared-profile",
                "name": "Use the model provider configuration",
                "kind": "environment",
                "effect": "requires_provider_profile",
                "environment": []
            }
        ],
        "supportsLogout": true,
        "status": "authenticated",
        "refreshedAtMs": 1
    }))
    .expect("auth catalog fixture")
}

/// The account panel is one list: the ways in, and what can be done with the
/// account the Agent holds.
///
/// A method the authority reports as needing a provider profile is one this
/// client cannot finish — the profile is chosen in the Providers area — so it is
/// a line that explains itself rather than a form whose answer would be refused.
#[test]
fn an_agents_account_is_read_as_one_list() {
    let mut app = loaded(140, 40);
    // The account is the agent domain's, and a client that never saw the
    // authority advertise it refuses to open the panel at all.
    app.capabilities.agent = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::AgentAuthRead,
    ]);
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    let outcome = app.run_config_action_by_key("codex", ConfigAction::Auth);
    assert!(
        matches!(app.overlay, Some(Overlay::AgentAuth { .. })),
        "the account panel did not open"
    );
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::LoadAgentAuthCatalog { agent_id, refresh }] => {
            assert_eq!(agent_id.as_str(), "codex");
            assert!(
                !refresh,
                "opening the panel re-probed the Agent instead of reading the snapshot"
            );
        }
        other => panic!("the panel read nothing: {other:?}"),
    }

    app.management_data.agent_auth_catalog = Some(agent_auth_catalog());
    let (status, _) = vibex_tui::config_center::agent_auth_status(
        app.management_data.agent_auth_catalog.as_ref(),
        app.strings,
    );
    assert_eq!(status, "Signed in");

    let rows = vibex_tui::config_center::agent_auth_rows(
        app.management_data.agent_auth_catalog.as_ref(),
        app.strings,
    );
    // A page for the credentials the method names, two methods, then the verbs
    // a signed-in account with a logout offers: refresh, verify, refresh models
    // and sign out.
    assert_eq!(rows.len(), 7, "the panel lost a line: {rows:#?}");
    assert_eq!(rows[0].label, "Get credentials", "the method names a page");
    let sign_in = rows
        .iter()
        .find(|row| row.label == "Sign in with ChatGPT")
        .expect("the usable method");
    assert!(sign_in.action.is_some(), "a usable method was not offered");
    assert!(
        sign_in
            .detail
            .iter()
            .any(|(label, state)| label == "API key" && state == "set"),
        "the method's variables were not listed with what is already set: {:#?}",
        sign_in.detail
    );
    let shared = rows
        .iter()
        .find(|row| row.label == "Use the model provider configuration")
        .expect("the method that needs a profile");
    assert!(
        shared.action.is_none(),
        "a method needing a provider profile was offered as usable"
    );

    // `Enter` on the verify line asks the Agent's own runtime.
    app.overlay = Some(Overlay::AgentAuth {
        view: vibex_tui::config_center::AgentAuthView {
            agent_id: "codex".to_string(),
            title: "Codex".to_string(),
            selected: 4,
            loading: false,
            confirming_sign_out: false,
            notice: None,
            want_line: vibex_tui::config_center::AgentAuthLine::First,
            pending_sign_in: None,
        },
    });
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::VerifyAgentAuth { agent_id }] => {
            assert_eq!(agent_id.as_str(), "codex");
        }
        other => panic!("the verify line produced {other:?}"),
    }
}

/// A method's variables are collected before the sign-in is asked for.
///
/// The runtime never hands a stored value back, so the fields open empty and say
/// what is already set instead: a blank field keeps what is there, which is why
/// taking one away is a switch of its own rather than an empty answer. The two
/// calls travel as one press because the sign-in reads the environment the
/// values are stored in.
#[test]
fn signing_in_collects_what_the_method_needs() {
    let mut app = loaded(140, 40);
    app.capabilities.agent = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::AgentAuthRead,
    ]);
    app.management_data.agent_auth_catalog = Some(agent_auth_catalog());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));
    app.overlay = Some(Overlay::AgentAuth {
        view: vibex_tui::config_center::AgentAuthView {
            agent_id: "codex".to_string(),
            title: "Codex".to_string(),
            selected: 1,
            loading: false,
            confirming_sign_out: false,
            notice: None,
            want_line: vibex_tui::config_center::AgentAuthLine::First,
            pending_sign_in: None,
        },
    });

    // `Enter` on the method opens the form its variables describe.
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    assert!(outcome.effects.is_empty(), "the form ran something");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the method opened no form");
    };
    let field = form.field("value:OPENAI_API_KEY").expect("the key field");
    assert!(field.secret, "a secret variable was offered in the clear");
    assert_eq!(field.value, "", "a stored value was echoed into the form");
    assert_eq!(field.hint.as_deref(), Some("set"));
    // The variable is already stored, so taking it away is offered too.
    assert!(
        form.field("clear:OPENAI_API_KEY").is_some(),
        "clearing a stored value was not offered"
    );

    // Typing a key and signing in writes both calls as one press.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("value:OPENAI_API_KEY").unwrap().value = "sk-new".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::AuthenticateAgentMethod {
                method_id, values, ..
            } = mutation
            else {
                panic!("the sign-in was not a sign-in: {mutation:?}");
            };
            assert_eq!(method_id, "chatgpt");
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].name, "OPENAI_API_KEY");
            assert_eq!(values[0].value.as_deref(), Some("sk-new"));
            assert!(values[0].secret);
            assert!(!values[0].clear);
        }
        other => panic!("the sign-in produced {other:?}"),
    }

    // The clear switch is what takes a stored value away, and it sends no value
    // with it.
    app.config_center.pending.clear();
    let environment = app
        .management_data
        .agent_auth_catalog
        .as_ref()
        .expect("the catalogue")
        .methods[0]
        .environment
        .clone();
    let strings = app.strings;
    app.overlay = Some(Overlay::Form {
        form: vibex_tui::config_center::agent_auth_method_form(
            "codex",
            "chatgpt",
            "Sign in with ChatGPT",
            &environment,
            strings,
        ),
    });
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("clear:OPENAI_API_KEY").unwrap().value = "on".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::AuthenticateAgentMethod { values, .. } = mutation
            else {
                panic!("the clear was not a sign-in: {mutation:?}");
            };
            assert!(values[0].clear, "the stored value was not cleared");
            assert!(values[0].value.is_none(), "a cleared value carried one");
        }
        other => panic!("the clear produced {other:?}"),
    }
}

/// The credential page a method names is one press from the panel.
#[test]
fn a_methods_credential_page_is_opened_by_its_own_line() {
    let mut app = loaded(140, 40);
    app.capabilities.agent = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::AgentAuthRead,
    ]);
    app.management_data.agent_auth_catalog = Some(agent_auth_catalog());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));
    app.overlay = Some(Overlay::AgentAuth {
        view: vibex_tui::config_center::AgentAuthView {
            agent_id: "codex".to_string(),
            title: "Codex".to_string(),
            selected: 0,
            loading: false,
            confirming_sign_out: false,
            notice: None,
            want_line: vibex_tui::config_center::AgentAuthLine::First,
            pending_sign_in: None,
        },
    });
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::OpenUrl { url }] => {
            assert_eq!(url, "https://example.test/keys");
        }
        other => panic!("the credential line produced {other:?}"),
    }
}

/// Signing out is asked about first, with what it would end.
///
/// A sign-out ends sessions that use the account, so the first press reads the
/// impact out and the second is the answer. The authority refuses a count that
/// no longer matches, which is what makes the preview a decision rather than a
/// formality — and `Esc` gives up the question before it gives up the panel.
#[test]
fn signing_out_is_asked_about_with_what_it_ends() {
    let mut app = loaded(140, 40);
    app.capabilities.agent = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::AgentAuthRead,
        vibex_backend::BackendOperation::AgentAuthManage,
    ]);
    app.management_data.agent_auth_catalog = Some(agent_auth_catalog());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));
    app.overlay = Some(Overlay::AgentAuth {
        view: vibex_tui::config_center::AgentAuthView {
            agent_id: "codex".to_string(),
            title: "Codex".to_string(),
            // The sign-out line, which a signed-in account with a logout has.
            selected: 6,
            loading: false,
            confirming_sign_out: false,
            notice: None,
            want_line: vibex_tui::config_center::AgentAuthLine::First,
            pending_sign_in: None,
        },
    });

    // The first press asks what it would end rather than signing out.
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::PreviewAgentAuthLogout { agent_id }] => {
            assert_eq!(agent_id.as_str(), "codex");
        }
        other => panic!("the sign-out did not ask first: {other:?}"),
    }
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::AgentAuth { view }) if view.confirming_sign_out
        ),
        "the question was not marked on the panel"
    );

    // The answer carries the impact the reader was shown.
    app.management_data.agent_auth_logout = Some(
        serde_json::from_value(serde_json::json!({
            "context": {
                "id": "agent_auth_context_codex",
                "agentId": "codex",
                "status": "authenticated",
                "revision": 7,
                "createdAtMs": 1,
                "updatedAtMs": 1
            },
            "affectedSessionIds": ["session_one", "session_two"]
        }))
        .expect("logout preview fixture"),
    );
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::LogoutAgentAuthContext {
                auth_context_id,
                expected_context_revision,
                confirmed_affected_session_count,
            } = mutation
            else {
                panic!("the sign-out was not a sign-out: {mutation:?}");
            };
            assert_eq!(auth_context_id, "agent_auth_context_codex");
            assert_eq!(*expected_context_revision, 7);
            assert_eq!(
                *confirmed_affected_session_count, 2,
                "the count the reader saw is not the count they confirmed"
            );
        }
        other => panic!("the sign-out produced {other:?}"),
    }

    // `Esc` gives up the question, not the panel.
    app.config_center.pending.clear();
    app.overlay = Some(Overlay::AgentAuth {
        view: vibex_tui::config_center::AgentAuthView {
            agent_id: "codex".to_string(),
            title: "Codex".to_string(),
            selected: 6,
            loading: false,
            confirming_sign_out: true,
            notice: None,
            want_line: vibex_tui::config_center::AgentAuthLine::First,
            pending_sign_in: None,
        },
    });
    app.perform(vibex_tui::action::Intent::Back);
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::AgentAuth { view }) if !view.confirming_sign_out
        ),
        "the first Esc closed the panel instead of the question"
    );
}

/// A sign-in that finishes somewhere else says where.
///
/// The runtime keeps the operation open and names the place the reader has to
/// finish it — a terminal or browser on the host, or a device code. This client
/// has no terminal of its own, so the honest answer is where to go, not a
/// sign-in that did not happen.
#[test]
fn a_sign_in_that_finishes_elsewhere_says_where() {
    let app: vibex_tui::app::App = loaded(140, 40);
    let strings = app.strings;
    let pending = vibex_tui::config_center::PendingWrite {
        subject: "Codex".to_string(),
        verb: vibex_tui::config_center::ConfigVerb::SignIn,
    };
    let message = |outcome: &vibex_tui::app::MutationOutcome| pending.message(strings, outcome);

    // The runtime reports a terminal method as `RemoteAttachableTerminal`, not
    // `HostTerminal`: it is a PTY the authority opened, which a client attaches
    // to by id. Both are "a terminal to finish in", and the line has to say so —
    // a branch that matched only `HostTerminal` sent every real sign-in to the
    // catch-all, which named nothing at all.
    let terminal = message(&vibex_tui::app::MutationOutcome::AuthWaiting {
        location: vibex_core::AgentAuthExecutionLocation::RemoteAttachableTerminal,
        detail: Some("Sign in to Codex".to_string()),
        operation: None,
        terminal: None,
    });
    assert!(
        terminal.contains("terminal") && terminal.contains("Sign in to Codex"),
        "the terminal answer named neither the place nor the sign-in: {terminal}"
    );
    let host_terminal = message(&vibex_tui::app::MutationOutcome::AuthWaiting {
        location: vibex_core::AgentAuthExecutionLocation::HostTerminal,
        detail: None,
        operation: None,
        terminal: None,
    });
    assert!(
        host_terminal.contains("terminal"),
        "a terminal on the host did not name the terminal: {host_terminal}"
    );

    let browser = message(&vibex_tui::app::MutationOutcome::AuthWaiting {
        location: vibex_core::AgentAuthExecutionLocation::HostBrowser,
        detail: None,
        operation: None,
        terminal: None,
    });
    assert!(
        browser.contains("browser"),
        "the browser answer did not name the browser: {browser}"
    );

    let device = message(&vibex_tui::app::MutationOutcome::AuthWaiting {
        location: vibex_core::AgentAuthExecutionLocation::ClientBrowserWithDeviceCode,
        detail: None,
        operation: None,
        terminal: None,
    });
    assert!(
        device.contains("code"),
        "the device-code answer did not mention the code: {device}"
    );

    // A completed sign-in still says it completed.
    let done = message(&vibex_tui::app::MutationOutcome::Auth {
        methods: 0,
        account: Some("work".to_string()),
        authenticated: true,
    });
    assert!(
        !done.contains("still signing in"),
        "a completed sign-in was reported as waiting: {done}"
    );
}

/// A market card the reader already runs says so, and offers to do it again.
///
/// The registry has no idea what this machine holds, so the match is against
/// the stored servers by name: a card that said "Install" over a server the
/// reader already runs would be offering to do what has been done. The card's
/// own page is one press away when it names one.
#[test]
fn a_market_card_the_reader_already_runs_says_so() {
    let mut app = loaded(140, 40);
    let mut entry = market_entry("entry_trace");
    entry.homepage = Some("https://example.test/trace".to_string());
    app.management_data.mcp_market = vec![entry];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    // Nothing stored: the card offers the install.
    let rows = vibex_tui::config_center::market_rows(&app.management_data, app.strings);
    let actions = rows[0].actions.clone();
    assert!(
        actions.contains(&ConfigAction::Install),
        "a card for something not installed did not offer the install: {actions:?}"
    );
    assert!(actions.contains(&ConfigAction::OpenPage));

    // The same server, stored: the card offers it again by its other name.
    let mut server = mcp_server();
    server.display_name = "Trace tools".to_string();
    app.management_data.mcp = vec![server];
    let rows = vibex_tui::config_center::market_rows(&app.management_data, app.strings);
    let actions = rows[0].actions.clone();
    assert!(
        actions.contains(&ConfigAction::Reinstall) && !actions.contains(&ConfigAction::Install),
        "an installed card offered a first install: {actions:?}"
    );
    assert_eq!(rows[0].chip, "Installed");

    // And the press opens the page it named, preferring its own homepage.
    let outcome = app.run_config_action_by_key("entry_trace", ConfigAction::OpenPage);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::OpenUrl { url }] => {
            assert_eq!(url, "https://example.test/trace")
        }
        other => panic!("the card's page produced {other:?}"),
    }
}

/// An Agent that is missing offers the page it can be installed from.
///
/// It is the one thing a client can still do about an Agent whose Install
/// cannot run, and the authority publishes the address in the snapshot itself.
#[test]
fn a_missing_agent_offers_its_install_page() {
    let mut app = loaded(140, 40);
    let mut missing = agent();
    missing.installed = false;
    missing.install_status = vibex_core::AgentInstallStatus::Missing;
    missing.params = serde_json::json!({"installUrl": "https://example.test/install"});
    app.management_data.agents = vec![missing];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    assert!(
        rows[0].actions.contains(&ConfigAction::InstallDocs),
        "a missing Agent offered no page: {:?}",
        rows[0].actions
    );

    let outcome = app.run_config_action_by_key("codex", ConfigAction::InstallDocs);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::OpenUrl { url }] => {
            assert_eq!(url, "https://example.test/install")
        }
        other => panic!("the install page produced {other:?}"),
    }

    // An Agent that is installed has nothing to install, so it has no page.
    let mut installed = agent();
    installed.params = serde_json::json!({"installUrl": "https://example.test/install"});
    app.management_data.agents = vec![installed];
    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    assert!(
        !rows[0].actions.contains(&ConfigAction::InstallDocs),
        "an installed Agent offered an install page"
    );
}

/// Installing a Skill asks which Agents receive it.
///
/// A Skill is stored once and enabled per Agent, so an install that quietly
/// enabled it for nobody would be an install the reader cannot find afterwards.
#[test]
fn installing_a_skill_asks_which_agents_receive_it() {
    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent(), second_agent()];
    app.management_data.skill_market = vec![skill_market_entry("entry_reader")];
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));

    // The install reads the Skill before it asks anything: a reader who has not
    // seen the bundle is not in a position to agree to it.
    let outcome = app.run_config_action_by_key("entry_reader", ConfigAction::Install);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::PreviewSkillMarket { entry_id, .. }] => {
            assert_eq!(entry_id, "entry_reader")
        }
        other => panic!("the install did not read the Skill first: {other:?}"),
    }
    assert!(
        matches!(app.overlay, Some(Overlay::SkillPreview { .. })),
        "the preview did not open"
    );

    // What the registry answered is what the install would write.
    app.management_data.skill_preview = Some(Box::new(skill_document("entry_reader")));
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    assert!(
        outcome.effects.is_empty(),
        "the install ran without asking who receives it: {:#?}",
        outcome.effects
    );
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the skill install opened no form");
    };
    let toggles = form
        .fields
        .iter()
        .filter(|field| field.key.starts_with("agent:"))
        .collect::<Vec<_>>();
    assert_eq!(toggles.len(), 2, "the Agents were not offered");
    assert!(
        toggles.iter().all(|field| field.value == "on"),
        "an Agent did not start switched on"
    );

    // Turning them all off is refused with the reason on the form.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        for field in form
            .fields
            .iter_mut()
            .filter(|f| f.key.starts_with("agent:"))
        {
            field.value = "off".to_string();
        }
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "an install for nobody was sent");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on an invalid save");
    };
    assert!(form.error.is_some(), "no reason was given");

    // Turning one back on sends the install with the Agents that receive it.
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("agent:codex").unwrap().value = "on".to_string();
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::InstallSkillMarketEntry {
                entry_id,
                agent_ids,
                document,
                ..
            } = mutation
            else {
                panic!("the install was not an install: {mutation:?}");
            };
            assert_eq!(entry_id, "entry_reader");
            assert_eq!(
                agent_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                vec!["codex"]
            );
            // The bytes the reader read travel with the write: a second read
            // could return another revision, and what landed would then not be
            // what was previewed.
            assert_eq!(
                document.as_deref().map(|document| document.body.as_str()),
                Some("# Trace reader\nRead a trace."),
                "the previewed document was not the one written"
            );
        }
        other => panic!("the install produced {other:?}"),
    }
}

/// The provider form's headers are carried as the JSON that will be written.
///
/// It is the form's preview, which is what `c` copies: the reader compares what
/// they typed against what will be stored, in the shape the runtime reads.
#[test]
fn a_provider_form_carries_its_headers_as_json() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.management_data.provider_detail = Some(provider_profile_detail());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the editor did not open");
    };
    let preview = form.preview.clone().expect("the headers preview");
    let headers: serde_json::Value = serde_json::from_str(&preview).expect("the preview is JSON");
    assert_eq!(headers["X-Trace"], serde_json::json!("on"));
    assert_eq!(
        headers["x-opencode-session"],
        serde_json::json!({"variable": "session_id"}),
        "a variable came back as a literal: {preview}"
    );
}

/// One registry Skill's document, as the authority answers it.
fn skill_document(entry_id: &str) -> vibex_core::SkillMarketDocument {
    serde_json::from_value(serde_json::json!({
        "entryId": entry_id,
        "name": "Trace reader",
        "description": "Read a trace",
        "body": "# Trace reader\nRead a trace.",
        "bytes": 30,
        "tooLarge": false,
        "files": [
            {"path": "reference/trace.md", "content": "notes", "bytes": 5},
            {"path": "scripts/trace.sh", "content": "echo", "bytes": 4}
        ],
        "bundleBytes": 39,
        "skippedFiles": ["binary/trace.png: not a text file"]
    }))
    .expect("skill document fixture")
}

/// A Skill is read before it is installed, and what was read is what is written.
///
/// A published Skill is a folder, not one file: the preview shows the bundle,
/// the files the registry refused to send, and the instructions themselves —
/// and the document the reader read is the document the install writes, so a
/// revision published in between cannot land unnoticed.
#[test]
fn a_skill_is_previewed_before_it_is_installed() {
    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent()];
    app.management_data.skill_market = vec![skill_market_entry("entry_reader")];
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));

    let outcome = app.run_config_action_by_key("entry_reader", ConfigAction::Install);
    match outcome.effects.as_slice() {
        [
            vibex_tui::app::Effect::PreviewSkillMarket {
                entry_id,
                slug,
                version,
                ..
            },
        ] => {
            assert_eq!(entry_id, "entry_reader");
            assert_eq!(slug, "entry_reader");
            assert_eq!(version.as_deref(), Some("1.0.0"));
        }
        other => panic!("the preview asked for nothing: {other:?}"),
    }

    app.management_data.skill_preview = Some(Box::new(skill_document("entry_reader")));
    let lines = vibex_tui::config_center::skill_preview_lines(
        app.management_data.skill_preview.as_deref(),
        app.strings,
    );
    assert_eq!(
        lines.files,
        vec![
            "reference/trace.md · 5 bytes".to_string(),
            "scripts/trace.sh · 4 bytes".to_string()
        ],
        "the bundle was not listed with its sizes"
    );
    assert_eq!(
        lines.skipped,
        vec!["binary/trace.png: not a text file".to_string()],
        "a file that will not be written was not disclosed"
    );
    assert!(
        lines
            .bundle
            .as_deref()
            .is_some_and(|bundle| bundle.contains('2')),
        "the bundle summary did not count the files: {:?}",
        lines.bundle
    );
    assert!(lines.banner.is_none(), "a writable document warned");
    assert!(lines.body.iter().any(|line| line.contains("Read a trace")));
}

/// A document too large to install is refused where it is read.
///
/// The runtime would reject it after the reader had answered everything else,
/// so the preview says so and the install does not open.
#[test]
fn a_skill_too_large_to_install_is_refused_at_the_preview() {
    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent()];
    app.management_data.skill_market = vec![skill_market_entry("entry_reader")];
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));
    app.run_config_action_by_key("entry_reader", ConfigAction::Install);

    let mut document = skill_document("entry_reader");
    document.too_large = true;
    document.bytes = 4_194_304;
    app.management_data.skill_preview = Some(Box::new(document));
    let lines = vibex_tui::config_center::skill_preview_lines(
        app.management_data.skill_preview.as_deref(),
        app.strings,
    );
    assert!(
        lines
            .banner
            .as_deref()
            .is_some_and(|banner| banner.contains("4194304")),
        "the preview did not say how large it was: {:?}",
        lines.banner
    );

    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    assert!(
        outcome.effects.is_empty(),
        "an oversized Skill was installed"
    );
    assert!(
        matches!(app.overlay, Some(Overlay::SkillPreview { .. })),
        "the preview closed on a refusal"
    );
    assert!(app.toast.is_some(), "the refusal gave no reason");
}

/// A Skill the reader already installed says so, and its card counts what it has.
///
/// The registry has no idea what this machine holds, so the match is against
/// the stored Skill's origin: a card that offered a first install over an
/// installed Skill would be offering to do what has been done. The count is the
/// first one the registry published rather than a zero it filled in.
#[test]
fn a_skill_the_reader_already_has_says_so() {
    let mut app = loaded(140, 40);
    let mut entry = skill_market_entry("entry_reader");
    entry.installs = 0;
    entry.downloads = 9;
    entry.stars = 1;
    app.management_data.skill_market = vec![entry];
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));

    // Nothing stored: the card offers the install and counts the downloads it
    // has, because a zero over nine downloads reads as a Skill nobody wanted.
    let rows = vibex_tui::config_center::skill_market_rows(&app.management_data, app.strings);
    assert!(rows[0].actions.contains(&ConfigAction::Install));
    assert_eq!(rows[0].cells[3], "9", "the count did not fall back");
    assert_eq!(rows[0].chip, "Registry");

    // The same Skill, stored: the card offers it again by its other name.
    app.management_data.skills = vec![
        serde_json::from_value(serde_json::json!({
            "id": "skill_reader",
            "displayName": "Trace reader",
            "sourceKind": "marketplace",
            "status": "enabled",
            "scopeKind": "user",
            "sourceUri": "market:entry_reader",
            "description": "Read a trace",
            "instructions": "Read a trace.",
            "tags": [],
            "providerMatrix": [],
            "agentMatrix": [],
            "createdAtMs": 1,
            "updatedAtMs": 1
        }))
        .expect("skill fixture"),
    ];
    let rows = vibex_tui::config_center::skill_market_rows(&app.management_data, app.strings);
    assert!(
        rows[0].actions.contains(&ConfigAction::Reinstall)
            && !rows[0].actions.contains(&ConfigAction::Install),
        "an installed Skill offered a first install: {:?}",
        rows[0].actions
    );
    assert_eq!(rows[0].chip, "Installed");
}

/// Reading further is the registry's next page, not a window the client widened.
///
/// The cursor is the registry's own value, echoed back untouched, and paging
/// past the last page the client holds is what asks for it.
#[test]
fn paging_past_the_last_page_asks_the_registry_for_more() {
    let mut app = loaded(140, 40);
    // More entries than one page holds, so there is a page to be past.
    app.management_data.skill_market = (0..45)
        .map(|index| skill_market_entry(&format!("entry_{index}")))
        .collect();
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(45),
        exhausted: false,
        cursor: Some("cursor_two".to_string()),
        error: None,
    });
    app.show_config_module(ConfigModule::Skills, Some(ConfigSection::Catalog));
    // The last page the client holds.
    app.config_center.registry.page = 2;

    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::LoadMoreSkillMarket { cursor, .. }] => {
            assert_eq!(cursor, "cursor_two", "the client invented a page")
        }
        other => panic!("paging past the end produced {other:?}"),
    }

    // A registry with nothing after it keeps the reader where they are.
    app.management_data.market_note = Some(vibex_tui::app::MarketNote {
        matched: Some(45),
        exhausted: true,
        cursor: None,
        error: None,
    });
    let outcome = app.perform(vibex_tui::action::Intent::ConfigRegistryPageNext);
    assert!(
        outcome.effects.is_empty(),
        "a page was asked for at the end"
    );
}

/// An area nobody has read yet draws the rows it is waiting for.
///
/// An empty list and a list that has not been asked look the same on screen and
/// are not the same thing, so the unanswered one draws the shape of its rows
/// with the word on it. The answer — even a refusal — takes the skeleton away,
/// because an area with an error in it is not an area that is still loading.
#[test]
fn an_area_that_has_not_answered_yet_draws_its_skeleton() {
    let mut app = loaded(140, 40);
    // An area that has not answered has no rows yet, which is what the skeleton
    // stands in for.
    app.management_data.mcp.clear();
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Servers));

    let screen = text(&mut app, 140, 40);
    assert!(
        screen.contains("Reading…"),
        "the area drew nothing to say it was reading:\n{screen}"
    );
    assert!(
        !screen.contains("Nothing here yet"),
        "an unasked area claimed to be empty:\n{screen}"
    );

    // The answer takes it away, whatever the answer said.
    app.management_data
        .mark_read(ConfigModule::Mcp, ConfigSection::Servers);
    let screen = text(&mut app, 140, 40);
    assert!(
        !screen.contains("Reading…"),
        "the skeleton outlived the answer:\n{screen}"
    );

    // And the answer is recorded per area: the Skill catalog is not read just
    // because the MCP one was.
    app.management_data
        .mark_read(ConfigModule::Mcp, ConfigSection::Catalog);
    assert!(
        app.management_data
            .has_read(ConfigModule::Mcp, ConfigSection::Catalog)
    );
    assert!(
        !app.management_data
            .has_read(ConfigModule::Skills, ConfigSection::Catalog),
        "the Skill catalog was marked read by the MCP one"
    );
    assert!(
        !app.management_data
            .has_read(ConfigModule::Skills, ConfigSection::Installed)
    );
}

/// A market card says how many of its variables have to be filled in.
#[test]
fn a_market_card_counts_the_variables_it_needs() {
    let mut app = loaded(140, 40);
    let mut entry = market_entry("entry_trace");
    entry.env.push(
        serde_json::from_value(serde_json::json!({
            "name": "TRACE_HOST",
            "required": false
        }))
        .expect("requirement"),
    );
    app.management_data.mcp_market = vec![entry];
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Catalog));

    let rows = vibex_tui::config_center::market_rows(&app.management_data, app.strings);
    let counted = rows[0]
        .detail
        .iter()
        .find(|(label, _)| label == "Variables required")
        .map(|(_, value)| value.clone());
    assert_eq!(
        counted.as_deref(),
        Some("1 / 2"),
        "the card did not count what it requires: {:?}",
        rows[0].detail
    );
}

/// A form the reader has typed into is not thrown away on one press.
///
/// Everything else in this centre can be done again; what was typed cannot, so
/// `Esc` asks before it discards, and answering "no" gives the form back with
/// the reader's own values in it.
#[test]
fn a_half_written_form_is_not_discarded_without_asking() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.management_data.provider_detail = Some(provider_profile_detail());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
        form.field_mut("default_model").unwrap().value = "gpt-6".to_string();
    }

    // `Esc` asks rather than discarding.
    app.perform(vibex_tui::action::Intent::Back);
    let Some(Overlay::Confirm { title, confirm, .. }) = app.overlay.clone() else {
        panic!("a typed-into form closed without asking");
    };
    assert!(
        title.contains("Discard"),
        "the question was not the question: {title}"
    );
    assert_eq!(confirm, vibex_tui::action::Intent::ConfigFormDiscard);

    // Answering "no" gives the form back with the typed value still in it.
    app.perform(vibex_tui::action::Intent::Back);
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("backing out of the question dropped the form");
    };
    assert_eq!(form.value("default_model").as_deref(), Some("gpt-6"));

    // A form nobody has typed into is not worth a question.
    let untouched = vibex_tui::config_center::agent_auth_method_form(
        "codex",
        "chatgpt",
        "Sign in",
        &[],
        app.strings,
    );
    app.overlay = Some(Overlay::Form { form: untouched });
    app.perform(vibex_tui::action::Intent::Back);
    assert!(
        app.overlay.is_none(),
        "an untouched form asked about discarding"
    );
}

/// Finding the local Agent CLIs is one pass, and it says what it found.
///
/// The pass is about the Agents area as a whole rather than about a record, and
/// it is refused where it would do nothing: a machine with nothing left to
/// probe gets the reason rather than a request that would answer nothing.
#[test]
fn finding_local_agents_is_one_pass_over_what_is_not_added() {
    let mut app = loaded(140, 40);
    app.capabilities.management = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::ManagementAgents,
    ]);
    let mut not_added = second_agent();
    not_added.added = false;
    app.management_data.agents = vec![agent(), not_added];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    let outcome = app.perform(vibex_tui::action::Intent::ConfigDiscover);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::DiscoverLocalAgents] => {}
        other => panic!("the probe asked nothing: {other:?}"),
    }
    assert!(
        app.config_center.pending.contains_key("agents"),
        "the area did not say it was working"
    );

    // Nothing left to probe: the reader is told rather than sent a request.
    let mut added = agent();
    added.added = true;
    app.management_data.agents = vec![added];
    app.config_center.pending.clear();
    let outcome = app.perform(vibex_tui::action::Intent::ConfigDiscover);
    assert!(outcome.effects.is_empty(), "an empty pass was sent");
    assert!(app.toast.is_some(), "the refusal gave no reason");

    // And it belongs to the Agents area, not to the centre as a whole.
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Servers));
    let outcome = app.perform(vibex_tui::action::Intent::ConfigDiscover);
    assert!(
        outcome.effects.is_empty(),
        "another area probed the machine"
    );
}

/// An install is watched on the record, and the card says what it is doing.
///
/// A managed install runs on the authority and answers nothing until it is
/// done, so the client asks about the record until the authority stops working
/// — and the phases the reader sees are the authority's own rather than the
/// client's guess at what a download looks like.
#[test]
fn an_install_is_watched_until_the_authority_stops_working() {
    use vibex_core::AgentManagedInstallStatus;

    let mut app = loaded(140, 40);
    app.capabilities.management = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::ManagementAgents,
    ]);
    let mut managed = agent();
    managed.managed_install.status = AgentManagedInstallStatus::Installing;
    managed.managed_install.available_version = Some("0.9.2".to_string());
    app.management_data.agents = vec![managed];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    // The press starts the watch.
    app.run_config_action_by_key("codex", ConfigAction::Install);
    let watch = app.install_watch.as_ref().expect("the watch did not start");
    assert_eq!(watch.agent_id.as_str(), "codex");
    assert_eq!(watch.label, "Codex");
    assert!(
        !watch.running(),
        "a watch with no phase claims to be running"
    );

    // The record's own phase is what the card shows, and the version it named
    // travels with it: an upgrade that says what it is moving to is a different
    // wait from one that does not.
    {
        let watch = app.install_watch.as_mut().unwrap();
        watch.phase = Some(AgentManagedInstallStatus::Installing);
        watch.version = Some("0.9.2".to_string());
    }
    assert!(app.install_watch.as_ref().unwrap().running());
    assert_eq!(
        app.install_watch.as_ref().unwrap().phase_label(app.strings),
        "Downloading"
    );
    let screen = text(&mut app, 140, 40);
    assert!(
        screen.contains("Working on Codex")
            && screen.contains("Downloading")
            && screen.contains("v0.9.2"),
        "the card did not draw the work:\n{screen}"
    );

    // The authority stops working: the watch ends and says what landed.
    app.install_watch.as_mut().unwrap().phase = Some(AgentManagedInstallStatus::Installed);
    app.tick();
    assert!(
        app.install_watch.is_none(),
        "the watch outlived the install"
    );
    assert!(
        app.toast.is_some(),
        "the end of the install was not reported"
    );

    // A failed install says what the authority said about it.
    app.toast = None;
    app.config_center.pending.clear();
    app.run_config_action_by_key("codex", ConfigAction::Install);
    {
        let watch = app.install_watch.as_mut().unwrap();
        watch.phase = Some(AgentManagedInstallStatus::Failed);
        watch.error = Some("the download was refused".to_string());
    }
    app.tick();
    assert!(app.install_watch.is_none());
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|toast| format!("{toast:?}").contains("the download was refused")),
        "the failure did not carry the authority's reason"
    );
}

/// A custom ACP Agent is registered from nothing, and the form is the authority
/// on what can be registered.
///
/// It is the one record in this centre with nothing behind it: the Agent does
/// not exist until the authority writes it, so every field is the reader's and
/// the checks are the desktop's own — the id is what everything else keys the
/// Agent by, and an environment that is not `KEY=value` is refused with the
/// reason on the form rather than after a round trip.
#[test]
fn a_custom_agent_is_registered_from_a_form() {
    let mut app = loaded(140, 40);
    app.capabilities.management = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::ManagementAgents,
    ]);
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    // `n` opens the form, selected row or not.
    let outcome = app.perform(vibex_tui::action::Intent::ConfigEntryCreate);
    assert!(outcome.effects.is_empty(), "the form ran something");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("`n` opened no form on the Agents area");
    };
    for field in ["agent_id", "label", "command", "args", "env"] {
        assert!(form.field(field).is_some(), "the form has no {field} field");
    }

    // An id that is not a kebab-case Agent id is refused before the write.
    let fill = |app: &mut App, id: &str, env: &str| {
        if let Some(Overlay::Form { form }) = app.overlay.as_mut() {
            form.field_mut("agent_id").unwrap().value = id.to_string();
            form.field_mut("label").unwrap().value = "My Agent".to_string();
            form.field_mut("command").unwrap().value = "/usr/bin/my-agent".to_string();
            form.field_mut("args").unwrap().value = "--acp --verbose".to_string();
            form.field_mut("env").unwrap().value = env.to_string();
        }
    };
    fill(&mut app, "My Agent", "");
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a bad id was written");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on a bad id");
    };
    assert_eq!(
        form.error.as_deref(),
        Some("Use a lowercase kebab-case Agent id")
    );

    // A value that is not KEY=value is refused too.
    fill(&mut app, "my-agent", "TOKEN");
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a bad environment was written");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on a bad environment");
    };
    assert_eq!(
        form.error.as_deref(),
        Some("Environment variables must use KEY=value, one per line")
    );

    // A name that is not a variable name is refused with its own reason.
    fill(&mut app, "my-agent", "1TOKEN=secret");
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    assert!(outcome.effects.is_empty(), "a bad name was written");
    let Some(Overlay::Form { form }) = app.overlay.clone() else {
        panic!("the form closed on a bad variable name");
    };
    assert_eq!(
        form.error.as_deref(),
        Some("Environment variable names may contain letters, digits, and underscores")
    );

    // And the request is what the reader typed, split the way it is written.
    fill(&mut app, "my-agent", "TOKEN=secret\nPATH_SUFFIX=/opt/bin");
    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::CreateCustomAgent(request) = mutation else {
                panic!("the save was not a registration: {mutation:?}");
            };
            assert_eq!(request.agent_id.as_str(), "my-agent");
            assert_eq!(request.label, "My Agent");
            assert_eq!(request.command.command, "/usr/bin/my-agent");
            assert_eq!(request.command.args, vec!["--acp", "--verbose"]);
            assert_eq!(request.env.get("TOKEN").map(String::as_str), Some("secret"));
            assert_eq!(
                request.env.get("PATH_SUFFIX").map(String::as_str),
                Some("/opt/bin")
            );
        }
        other => panic!("the save produced {other:?}"),
    }
}

/// The roster is three groups, and joining or leaving it is not an install.
///
/// Which group an Agent is in is the first thing the reader needs: an Agent the
/// roster does not hold is one they have not agreed to see, and one added but
/// switched off is one they have. Adding and removing say what the reader wants
/// to work with and touch nothing on disk, which is why neither is an install.
#[test]
fn the_roster_is_three_groups_and_two_verbs() {
    let mut app = loaded(140, 40);
    let mut added_off = second_agent();
    added_off.enabled = false;
    let mut not_added = second_agent();
    not_added.id = vibex_core::AgentId::parse("gemini".to_string()).expect("agent id");
    not_added.label = "Gemini".to_string();
    not_added.added = false;
    app.management_data.agents = vec![not_added, added_off, agent()];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    assert_eq!(
        rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>(),
        vec!["codex", "claude", "gemini"],
        "the three groups are not contiguous"
    );
    assert_eq!(
        rows.iter().map(|row| row.chip.as_str()).collect::<Vec<_>>(),
        vec!["Enabled", "Disabled", "Not added"],
        "the group was not named on the row"
    );
    assert_eq!(
        rows.iter()
            .map(|row| {
                row.detail
                    .iter()
                    .find(|(label, _)| label == "Roster")
                    .map(|(_, value)| value.as_str())
                    .unwrap_or("")
            })
            .collect::<Vec<_>>(),
        vec!["Enabled", "Added", "Not added"],
        "the group was not named in full"
    );

    // Each row offers the verb its group calls for.
    let actions = |key: &str| {
        rows.iter()
            .find(|row| row.key == key)
            .map(|row| row.actions.clone())
            .unwrap_or_default()
    };
    assert!(actions("codex").contains(&ConfigAction::RemoveFromRoster));
    assert!(!actions("codex").contains(&ConfigAction::AddToRoster));
    assert!(actions("gemini").contains(&ConfigAction::AddToRoster));
    assert!(!actions("gemini").contains(&ConfigAction::RemoveFromRoster));

    // Joining writes the roster and nothing else.
    let outcome = app.run_config_action_by_key("gemini", ConfigAction::AddToRoster);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SetAgentAdded { agent_id, added } = mutation else {
                panic!("joining the roster was something else: {mutation:?}");
            };
            assert_eq!(agent_id.as_str(), "gemini");
            assert!(added, "the wrong direction was written");
        }
        other => panic!("joining the roster produced {other:?}"),
    }

    app.config_center.pending.clear();
    let outcome = app.run_config_action_by_key("codex", ConfigAction::RemoveFromRoster);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SetAgentAdded { added, .. } = mutation else {
                panic!("leaving the roster was something else: {mutation:?}");
            };
            assert!(!added, "the wrong direction was written");
        }
        other => panic!("leaving the roster produced {other:?}"),
    }
}

/// A failed managed install says so on the row, in the authority's own words.
///
/// A failure the reader cannot see is one they will meet again: the authority
/// keeps what it said, and the row is where it is read.
#[test]
fn a_failed_install_says_so_on_the_row() {
    let mut app = loaded(140, 40);
    let mut failed = agent();
    failed.managed_install.status = vibex_core::AgentManagedInstallStatus::Failed;
    failed.managed_install.last_error_code = Some("npm_unreachable".to_string());
    failed.managed_install.last_error_message = Some("the registry timed out".to_string());
    app.management_data.agents = vec![failed];

    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    assert_eq!(rows[0].chip, "install failed");
    assert_eq!(rows[0].tone, vibex_tui::config_center::ConfigTone::Bad);
    assert!(
        rows[0]
            .detail
            .iter()
            .any(|(label, value)| label == "Last error" && value == "the registry timed out"),
        "the authority's own words were not kept: {:#?}",
        rows[0].detail
    );

    // Without a message the code is still worth more than nothing.
    let mut coded = agent();
    coded.managed_install.status = vibex_core::AgentManagedInstallStatus::Failed;
    coded.managed_install.last_error_code = Some("npm_unreachable".to_string());
    app.management_data.agents = vec![coded];
    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    assert!(
        rows[0]
            .detail
            .iter()
            .any(|(_, value)| value == "npm_unreachable"),
        "the error code was dropped"
    );
}

/// The roster's order is the authority's, so a move is a swap of two places.
///
/// Every client reads the same order, which is why it is not a list this client
/// keeps to itself. An Agent at either end of its own group has nowhere to go,
/// and one at the edge of its group does not cross into the next: the groups
/// are the roster's, not the reader's.
#[test]
fn moving_an_agent_swaps_two_places_in_its_own_group() {
    let mut app = loaded(140, 40);
    let mut second = second_agent();
    second.order_index = 1;
    let mut third = agent();
    third.id = vibex_core::AgentId::parse("gemini".to_string()).expect("agent id");
    third.label = "Gemini".to_string();
    third.order_index = 2;
    app.management_data.agents = vec![third, agent(), second];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    // The rows follow the authority's order rather than the order it listed
    // them in.
    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    assert_eq!(
        rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>(),
        vec!["codex", "claude", "gemini"]
    );

    // Moving the first one down swaps it with the one after it.
    let outcome = app.run_config_action_by_key("codex", ConfigAction::MoveDown);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SwapAgentOrder {
                first,
                first_index,
                second,
                second_index,
            } = mutation
            else {
                panic!("the move was not a swap: {mutation:?}");
            };
            assert_eq!(first.as_str(), "codex");
            assert_eq!(second.as_str(), "claude");
            assert_eq!((*first_index, *second_index), (0, 1));
        }
        other => panic!("the move produced {other:?}"),
    }

    // At the end of the list there is nowhere to go.
    app.config_center.pending.clear();
    let outcome = app.run_config_action_by_key("gemini", ConfigAction::MoveDown);
    assert!(outcome.effects.is_empty(), "the last Agent moved down");
    let outcome = app.run_config_action_by_key("codex", ConfigAction::MoveUp);
    assert!(outcome.effects.is_empty(), "the first Agent moved up");

    // A group's edge is not a way into the next group: the not-added Agent
    // stays where it is rather than swapping with an enabled one.
    let mut outside = second_agent();
    outside.id = vibex_core::AgentId::parse("qwen".to_string()).expect("agent id");
    outside.label = "Qwen".to_string();
    outside.added = false;
    outside.order_index = 9;
    app.management_data.agents.push(outside);
    app.config_center.pending.clear();
    let outcome = app.run_config_action_by_key("qwen", ConfigAction::MoveUp);
    assert!(
        outcome.effects.is_empty(),
        "an Agent crossed out of its own group"
    );
}

/// The pane wears the record's badge, not only the row.
#[test]
fn the_pane_wears_the_badge_the_row_wears() {
    let mut app = loaded(140, 40);
    let mut failed = agent();
    failed.managed_install.status = vibex_core::AgentManagedInstallStatus::Failed;
    app.management_data.agents = vec![failed];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    let screen = text(&mut app, 140, 40);
    assert!(
        screen.contains("install failed"),
        "the pane did not carry the state the row carries:\n{screen}"
    );
    // The row's marker stays the row's: the pane carries the badge, which says
    // the same thing in words, and two markers for one state would be two
    // answers to the question the row's marker is already answering.
    assert!(
        screen.matches("install failed").count() >= 1,
        "the badge was not drawn"
    );
}

/// The built-in tool panels are per Agent and per panel, and the reader's
/// answer is the authority's record rather than this client's copy.
///
/// The panels reach an Agent the user never configured, so what is rendered is
/// what the authority holds: a client that kept its own list would show a
/// default the runtime does not share.
#[test]
fn the_builtin_tool_panels_are_switched_per_agent() {
    use vibex_core::AgentBuiltinTool;

    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent()];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    // Reading the area asks the authority for the panels it holds.
    let outcome = app.perform(vibex_tui::action::Intent::ReloadManagement);
    assert!(
        outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, vibex_tui::app::Effect::LoadAgentTools)),
        "the area did not read the panels: {:#?}",
        outcome.effects
    );

    // Both panels start on, which is the documented default.
    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    let tools = rows[0]
        .detail
        .iter()
        .find(|(label, _)| label == "Built-in tools")
        .map(|(_, value)| value.clone())
        .expect("the panels were not on the record");
    assert!(
        tools.contains("Browser tools on") && tools.contains("Computer use on"),
        "the panels did not start from the default: {tools}"
    );

    // Switching one off writes one panel and only that one.
    let outcome = app.run_config_action_by_key("codex", ConfigAction::ComputerTools);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SetAgentTool {
                agent_id,
                tool,
                enabled,
            } = mutation
            else {
                panic!("the switch was something else: {mutation:?}");
            };
            assert_eq!(agent_id.as_str(), "codex");
            assert_eq!(*tool, AgentBuiltinTool::Computer);
            assert!(!enabled, "a panel that was on was switched on again");
        }
        other => panic!("the switch produced {other:?}"),
    }

    // The answer the authority gives is what the row then renders.
    app.management_data
        .agent_tools
        .set_enabled("codex", AgentBuiltinTool::Computer, false);
    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    let tools = rows[0]
        .detail
        .iter()
        .find(|(label, _)| label == "Built-in tools")
        .map(|(_, value)| value.clone())
        .expect("the panels were not on the record");
    assert!(
        tools.contains("Browser tools on") && tools.contains("Computer use off"),
        "the row did not follow the authority's answer: {tools}"
    );

    // The browser panel is its own switch, in the other direction.
    app.config_center.pending.clear();
    let outcome = app.run_config_action_by_key("codex", ConfigAction::BrowserTools);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::SetAgentTool { tool, enabled, .. } = mutation
            else {
                panic!("the switch was something else: {mutation:?}");
            };
            assert_eq!(*tool, AgentBuiltinTool::Browser);
            assert!(!enabled, "the browser panel was not the one switched");
        }
        other => panic!("the switch produced {other:?}"),
    }
}

/// A record verb answers for the pane the reader is in.
///
/// The centre has three panes and one cursor each, and a verb that acts on "the
/// row" needs the reader to be on one. A reader whose focus is on the module
/// tabs or the area rail is not, and pressing `d` there is not a request to
/// delete whatever row the cursor happens to be on in a pane they are not
/// looking at. The moves already answer for their pane; this is the same rule
/// for the keys that change something.
#[test]
fn record_verbs_answer_for_the_pane_the_reader_is_in() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    assert_eq!(app.config_center.zone, ConfigZone::Rows);

    // On the rows, the verbs act.
    let _ = app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    assert!(
        matches!(app.overlay, Some(Overlay::Form { .. })),
        "the rows did not open the editor"
    );
    app.overlay = None;

    // On the tabs, none of them do — and nothing opens behind the reader.
    app.perform(vibex_tui::action::Intent::ConfigPanePrevious);
    app.perform(vibex_tui::action::Intent::ConfigPanePrevious);
    assert_eq!(app.config_center.zone, ConfigZone::Modules);
    for intent in [
        vibex_tui::action::Intent::EditSelectedEntry,
        vibex_tui::action::Intent::ConfigEntryDelete,
        vibex_tui::action::Intent::ToggleSelectedEntry,
        vibex_tui::action::Intent::ConfigEntryActions,
        vibex_tui::action::Intent::ConfigModels,
        vibex_tui::action::Intent::ConfigEntryCreate,
    ] {
        let outcome = app.perform(intent);
        assert!(
            outcome.effects.is_empty() && app.overlay.is_none(),
            "{intent:?} acted from the module tabs"
        );
        assert!(
            app.config_center.pending.is_empty(),
            "{intent:?} filed work from the module tabs"
        );
    }

    // The area rail is a pane of its own, and it is not the rows either.
    app.perform(vibex_tui::action::Intent::ConfigPaneNext);
    assert_eq!(app.config_center.zone, ConfigZone::Rail);
    let outcome = app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    assert!(
        outcome.effects.is_empty() && app.overlay.is_none(),
        "a record verb acted from the area rail"
    );

    // Moving back to the rows makes them act again.
    app.perform(vibex_tui::action::Intent::ConfigPaneNext);
    assert_eq!(app.config_center.zone, ConfigZone::Rows);
    let _ = app.perform(vibex_tui::action::Intent::EditSelectedEntry);
    assert!(
        matches!(app.overlay, Some(Overlay::Form { .. })),
        "the rows stopped acting after the walk back"
    );
}

/// The whole Model list can be offered or withdrawn at once.
///
/// The desktop offers the same three presses (a select-all beside these two);
/// selecting and enabling are one press here because a Model the profile does
/// not hold has nothing to enable. Disabling is about what the profile holds: a
/// Model it does not hold has nothing to switch off, and adding one to switch
/// it off would be the opposite of what was asked.
#[test]
fn the_whole_model_list_is_offered_or_withdrawn_at_once() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_first", "First")];
    app.management_data.provider_detail = Some(provider_profile_detail());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    app.perform(vibex_tui::action::Intent::ConfigModels);

    // The catalogue lists two Models; the profile holds one of them.
    let model = |id: &str| {
        serde_json::from_value::<vibex_core::ProviderConfiguredModel>(serde_json::json!({
            "id": id,
            "enabled": false,
            "capabilities": {}
        }))
        .expect("model fixture")
    };
    let offered = model("gpt-5");
    let extra = model("gpt-5-mini");
    app.management_data.provider_models = vec![offered, extra];

    let outcome = app.perform(vibex_tui::action::Intent::ConfigModelEnableAll);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the press did not write the list: {mutation:?}");
            };
            assert_eq!(models.len(), 2, "the catalogue was not all offered");
            assert!(
                models.iter().all(|model| model.enabled),
                "a Model the catalogue lists was not enabled"
            );
        }
        other => panic!("offering everything produced {other:?}"),
    }

    // Disabling leaves the list as it is and switches every Model off.
    app.config_center.pending.clear();
    app.overlay = Some(Overlay::Models {
        view: vibex_tui::config_center::ModelListView {
            agent_id: "codex".to_string(),
            profile_id: "provider_first".to_string(),
            title: "First".to_string(),
            selected: 0,
            notice: None,
        },
    });
    let outcome = app.perform(vibex_tui::action::Intent::ConfigModelDisableAll);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::ConfigMutation { mutation, .. }] => {
            let vibex_tui::app::ConfigMutation::UpdateProviderProfile {
                configured_models: Some(models),
                ..
            } = mutation
            else {
                panic!("the press did not write the list: {mutation:?}");
            };
            assert!(
                models.iter().all(|model| !model.enabled),
                "a Model was left enabled"
            );
        }
        other => panic!("withdrawing everything produced {other:?}"),
    }
}

/// An empty table says how to get its rows back.
#[test]
fn an_empty_table_offers_the_way_out_of_the_filter() {
    let mut app = loaded(140, 40);
    app.management_data.mcp.clear();
    app.management_data
        .mark_read(ConfigModule::Mcp, ConfigSection::Servers);
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Servers));
    app.config_center.filter = "nothing matches this".to_string();

    let screen = text(&mut app, 140, 40);
    assert!(
        screen.contains("Delete clears the filter"),
        "the empty table did not say how to clear the filter:\n{screen}"
    );

    // And the key it names does clear it.
    app.perform(vibex_tui::action::Intent::ClearFilter);
    assert!(app.config_center.filter.is_empty());
}

/// A read that refused is the state the area is in, and the way to ask again is
/// drawn with it.
///
/// A toast is gone before a reader who looked away can read it. An area that
/// refused and then answers clears the failure, because the state it described
/// has changed.
#[test]
fn a_failed_read_is_a_state_the_area_is_in() {
    let mut app = loaded(140, 40);
    app.management_data.mcp.clear();
    app.show_config_module(ConfigModule::Mcp, Some(ConfigSection::Servers));

    // The area has not answered yet: the skeleton, not a failure.
    let screen = text(&mut app, 140, 40);
    assert!(!screen.contains("The read failed"), "{screen}");

    app.management_data.mark_read_failed(
        ConfigModule::Mcp,
        ConfigSection::Servers,
        "mcp_unavailable".to_string(),
        "the runtime served no MCP list".to_string(),
    );
    let screen = text(&mut app, 140, 40);
    assert!(
        screen.contains("The read failed")
            && screen.contains("mcp_unavailable")
            && screen.contains("the runtime served no MCP list"),
        "the area did not say what it said:\n{screen}"
    );
    assert!(
        screen.contains("r asks again"),
        "the area did not name the way to ask again:\n{screen}"
    );

    // Asking again is the key the state names, and an answer clears it.
    let outcome = app.perform(vibex_tui::action::Intent::ReloadManagement);
    assert!(!outcome.effects.is_empty(), "the retry asked for nothing");
    app.management_data
        .mark_read(ConfigModule::Mcp, ConfigSection::Servers);
    let screen = text(&mut app, 140, 40);
    assert!(
        !screen.contains("The read failed"),
        "the failure outlived the answer that replaced it:\n{screen}"
    );
}

/// A switch answers the press before the authority does.
///
/// It is the one record write whose intent cannot be misread: the reader is
/// looking at a row that says "off" and asking for "on". Nothing here survives
/// a refusal — the write re-reads the area it touched — so a switch the
/// authority declined is drawn back the way it was.
#[test]
fn a_switch_answers_the_press_before_the_authority_does() {
    let mut app = loaded(140, 40);
    app.management_data.agents = vec![agent()];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    // The row says on, and the press turns it off on the spot.
    let before = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    assert_eq!(before[0].chip, "Enabled");
    app.run_config_action_by_key("codex", ConfigAction::Toggle);
    let after = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    assert_eq!(
        after[0].chip, "Disabled",
        "the row waited for the authority"
    );
    assert!(
        app.config_center.pending.contains_key("codex"),
        "the write was not filed"
    );

    // A record the reader did not switch is not touched: the flip is about the
    // row the press was on.
    let mut other = second_agent();
    other.id = vibex_core::AgentId::parse("gemini".to_string()).expect("agent id");
    other.label = "Gemini".to_string();
    app.management_data.agents.push(other);
    app.config_center.pending.clear();
    app.run_config_action_by_key("codex", ConfigAction::Toggle);
    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    let gemini = rows
        .iter()
        .find(|row| row.key == "gemini")
        .expect("the other Agent");
    assert_eq!(gemini.chip, "Enabled", "another row moved with the press");

    // The panels are switches too, and they answer the same way.
    app.config_center.pending.clear();
    app.run_config_action_by_key("codex", ConfigAction::BrowserTools);
    assert!(
        !app.management_data
            .agent_tools
            .enabled("codex", vibex_core::AgentBuiltinTool::Browser),
        "the panel waited for the authority"
    );
}

/// The Agents table reads the account column, and the row offers the verbs that
/// account actually has.
///
/// The complaint this covers: the column read "Not checked" for every Agent
/// forever, because nothing ever asked for the contexts, and the sign-out verb
/// was drawn over Agents nobody had signed into.
#[test]
fn the_agent_table_reads_the_account_it_draws() {
    let mut app = loaded(140, 40);
    app.capabilities.agent = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::AgentAuthRead,
        vibex_backend::BackendOperation::AgentAuthManage,
    ]);
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    // Opening the area is what asks for the contexts: the column has no other
    // source, and a verb that reads nothing is a verb that reports nothing.
    let effects = app.config_section_effects();
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, vibex_tui::app::Effect::ListAgentAuth { .. })),
        "the Agents area never asked for the accounts it draws: {effects:?}"
    );

    // Nobody signed in: there is no account to sign out of, so the verb is not
    // offered. The sign-in verb stays — the panel behind it can still say there
    // is no way in.
    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    let row = rows
        .iter()
        .find(|row| row.key == "codex")
        .expect("the fixture Agent");
    assert!(
        row.actions.contains(&ConfigAction::Auth),
        "the way in was withdrawn: {:?}",
        row.actions
    );
    assert!(
        !row.actions.contains(&ConfigAction::Logout),
        "sign-out was offered with no account to end: {:?}",
        row.actions
    );

    // Once the authority holds a context, the account has something to end.
    app.management_data.agent_auth = vec![
        serde_json::from_value(serde_json::json!({
            "id": "agent_auth_context_codex",
            "agentId": "codex",
            "status": "authenticated",
            "accountHint": "work@example.com",
            "revision": 3,
            "createdAtMs": 1,
            "updatedAtMs": 1
        }))
        .expect("auth context fixture"),
    ];
    let rows = vibex_tui::config_center::agent_rows(&app.management_data, app.strings);
    let row = rows
        .iter()
        .find(|row| row.key == "codex")
        .expect("the fixture Agent");
    assert!(
        row.actions.contains(&ConfigAction::Logout),
        "an account existed and sign-out was not offered: {:?}",
        row.actions
    );
    assert!(
        row.cells.iter().any(|cell| cell == "work@example.com"),
        "the account column did not name the account: {:?}",
        row.cells
    );
}

/// The account panel's status is the authority's answer about the account, not
/// the catalogue's guess about the Agent.
///
/// The runtime only ever reports `Unknown` or `AuthenticationRequired` for a
/// freshly read catalogue: whether one of the advertised methods was used is the
/// context's answer. A panel that ignored it said "Not checked" over an account
/// the reader was signed into, and hid the verbs gated on being signed in.
#[test]
fn the_account_status_follows_the_context_the_authority_holds() {
    let mut catalog = agent_auth_catalog();
    // What an ACP handshake actually produces.
    catalog.status = vibex_core::AgentAuthStatus::Unknown;

    let signed_in: vibex_core::AgentAuthContext = serde_json::from_value(serde_json::json!({
        "id": "agent_auth_context_codex",
        "agentId": "codex",
        "status": "authenticated",
        "revision": 3,
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("auth context fixture");
    vibex_tui::config_center::merge_agent_auth_context(&mut catalog, &[signed_in]);
    assert_eq!(catalog.status, vibex_core::AgentAuthStatus::Authenticated);

    // The same account, ended.
    let required: vibex_core::AgentAuthContext = serde_json::from_value(serde_json::json!({
        "id": "agent_auth_context_codex",
        "agentId": "codex",
        "status": "authentication_required",
        "revision": 4,
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("auth context fixture");
    vibex_tui::config_center::merge_agent_auth_context(&mut catalog, &[required]);
    assert_eq!(
        catalog.status,
        vibex_core::AgentAuthStatus::AuthenticationRequired
    );

    // Another Agent's account is not this Agent's answer.
    let mut catalog = agent_auth_catalog();
    catalog.status = vibex_core::AgentAuthStatus::Unknown;
    let other: vibex_core::AgentAuthContext = serde_json::from_value(serde_json::json!({
        "id": "agent_auth_context_claude",
        "agentId": "claude",
        "status": "authenticated",
        "revision": 1,
        "createdAtMs": 1,
        "updatedAtMs": 1
    }))
    .expect("auth context fixture");
    vibex_tui::config_center::merge_agent_auth_context(&mut catalog, &[other]);
    assert_eq!(
        catalog.status,
        vibex_core::AgentAuthStatus::Unknown,
        "another Agent's account was worn by this one"
    );
}

/// An account write re-reads the account, so the panel is not the before-picture.
#[test]
fn a_sign_in_or_out_re_reads_the_account_it_changed() {
    let mut app = loaded(140, 40);
    app.management_data.agent_auth_catalog = Some(agent_auth_catalog());

    for key in ["agent_auth_sign_in", "agent_auth_sign_out"] {
        let outcome = app.refresh_after_mutation(key);
        assert!(
            outcome
                .effects
                .iter()
                .any(|effect| matches!(effect, vibex_tui::app::Effect::ListAgentAuth { .. })),
            "{key} did not re-read the contexts: {:?}",
            outcome.effects
        );
        assert!(
            outcome.effects.iter().any(|effect| matches!(
                effect,
                vibex_tui::app::Effect::LoadAgentAuthCatalog { .. }
            )),
            "{key} did not re-read the catalogue: {:?}",
            outcome.effects
        );
    }
}

/// The row's sign-out verb lands the panel on the line it was pressed for.
///
/// It is the same destructive write the panel itself asks about, so the row must
/// not be a second, quieter way to end an account.
#[test]
fn the_rows_sign_out_opens_the_panel_on_the_line_it_meant() {
    let mut app = loaded(140, 40);
    app.capabilities.agent = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::AgentAuthRead,
        vibex_backend::BackendOperation::AgentAuthManage,
    ]);
    app.management_data.agent_auth_catalog = Some(agent_auth_catalog());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Agents));

    let outcome = app.run_config_action_by_key("codex", ConfigAction::Logout);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::LoadAgentAuthCatalog { agent_id, refresh }] => {
            assert_eq!(agent_id.as_str(), "codex");
            assert!(!refresh, "the row's sign-out re-probed the Agent");
        }
        other => panic!("the row's sign-out did not open the account: {other:?}"),
    }
    let Some(Overlay::AgentAuth { view }) = &app.overlay else {
        panic!("the row's sign-out did not open the account panel");
    };
    assert_eq!(
        view.want_line,
        vibex_tui::config_center::AgentAuthLine::SignOut,
        "the panel opened somewhere other than the line the row meant"
    );
}

/// A sign-in the authority is still running can be called off.
///
/// A browser or a terminal the client cannot follow is otherwise a process
/// nobody can stop from here, and the panel had no line for it at all.
#[test]
fn a_running_sign_in_is_offered_and_its_cancel_is_asked_for() {
    let mut app = loaded(140, 40);
    app.capabilities.agent = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::AgentAuthRead,
        vibex_backend::BackendOperation::AgentAuthManage,
    ]);
    app.management_data.agent_auth_catalog = Some(agent_auth_catalog());
    let operation: vibex_core::AgentAuthenticationOperation =
        serde_json::from_value(serde_json::json!({
            "operationId": "agent_auth_operation_codex",
            "authContextId": "agent_auth_context_codex",
            "expectedContextRevision": 7,
            "methodId": "chatgpt",
            "state": "awaiting_user",
            "createdAtMs": 1,
            "updatedAtMs": 2
        }))
        .expect("operation fixture");
    app.overlay = Some(Overlay::AgentAuth {
        view: vibex_tui::config_center::AgentAuthView {
            agent_id: "codex".to_string(),
            title: "Codex".to_string(),
            selected: 0,
            loading: false,
            confirming_sign_out: false,
            notice: None,
            want_line: vibex_tui::config_center::AgentAuthLine::First,
            pending_sign_in: Some(operation),
        },
    });

    let rows = vibex_tui::config_center::agent_auth_rows_with_sign_in(
        app.management_data.agent_auth_catalog.as_ref(),
        true,
        app.strings,
    );
    let cancel = rows
        .iter()
        .position(|row| row.action == Some(vibex_tui::config_center::AgentAuthAction::Cancel))
        .expect("a running sign-in offered no way to stop it");

    if let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut() {
        view.selected = cancel;
    }
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [
            vibex_tui::app::Effect::CancelAgentAuth {
                agent_id,
                operation_id,
            },
        ] => {
            assert_eq!(agent_id.as_str(), "codex");
            assert_eq!(operation_id.as_str(), "agent_auth_operation_codex");
        }
        other => panic!("the cancel produced {other:?}"),
    }
    let Some(Overlay::AgentAuth { view }) = &app.overlay else {
        panic!("the panel closed instead of dropping the finished sign-in");
    };
    assert!(
        view.pending_sign_in.is_none(),
        "the panel went on offering to cancel a sign-in it had already called off"
    );
}

/// "Refresh methods" re-probes the Agent; opening the panel does not.
///
/// They are different questions. Opening is a read, and a read that launched the
/// Agent every time would be a process the reader never asked for; the refresh
/// line is the reader saying what they have is stale, and answering it from the
/// same snapshot is a key that reports reading without reading anything.
#[test]
fn the_refresh_line_re_probes_while_opening_the_panel_only_reads() {
    let mut app = loaded(140, 40);
    app.capabilities.agent = vibex_backend::DomainCapabilities::available([
        vibex_backend::BackendOperation::AgentAuthRead,
        vibex_backend::BackendOperation::AgentAuthManage,
    ]);
    app.management_data.agent_auth_catalog = Some(agent_auth_catalog());
    app.overlay = Some(Overlay::AgentAuth {
        view: vibex_tui::config_center::AgentAuthView {
            agent_id: "codex".to_string(),
            title: "Codex".to_string(),
            // The refresh line: after two methods and the credentials page.
            selected: 3,
            loading: false,
            confirming_sign_out: false,
            notice: None,
            want_line: vibex_tui::config_center::AgentAuthLine::First,
            pending_sign_in: None,
        },
    });
    let rows = vibex_tui::config_center::agent_auth_rows(
        app.management_data.agent_auth_catalog.as_ref(),
        app.strings,
    );
    let refresh = rows
        .iter()
        .position(|row| {
            row.action == Some(vibex_tui::config_center::AgentAuthAction::RefreshMethods)
        })
        .expect("the refresh line");
    if let Some(Overlay::AgentAuth { view }) = app.overlay.as_mut() {
        view.selected = refresh;
    }

    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    match outcome.effects.as_slice() {
        [vibex_tui::app::Effect::LoadAgentAuthCatalog { agent_id, refresh }] => {
            assert_eq!(agent_id.as_str(), "codex");
            assert!(refresh, "the refresh line answered from the snapshot");
        }
        other => panic!("the refresh line produced {other:?}"),
    }
}

/// A method the client cannot drive is a line that explains itself.
///
/// The rule is `AgentAuthMethod::is_client_drivable`, shared with the desktop so
/// both clients offer the same verbs for the same catalogue: a terminal method is
/// always drivable, and an Agent-managed one needs either an advertised logout or
/// a verified browser flow for that exact `(agent, method)` pair.
#[test]
fn a_method_the_client_cannot_drive_is_not_offered_as_usable() {
    let mut catalog = agent_auth_catalog();
    catalog.supports_logout = false;
    // The fixture's first method is Agent-managed; with no logout the client
    // cannot replace that account, so the line must not pretend otherwise.
    let rows = vibex_tui::config_center::agent_auth_rows(
        Some(&catalog),
        vibex_tui::Strings::for_locale(vibex_tui::Locale::En),
    );
    let sign_in = rows
        .iter()
        .find(|row| row.label == "Sign in with ChatGPT")
        .expect("the Agent-managed method");
    assert!(
        sign_in.action.is_none(),
        "an Agent-managed method with no logout was offered as usable"
    );
    assert_eq!(sign_in.chip, "the Agent's own sign-in");

    // An Agent that supports logout can be re-authenticated, so the same method
    // becomes a verb.
    let mut catalog = agent_auth_catalog();
    catalog.supports_logout = true;
    let rows = vibex_tui::config_center::agent_auth_rows(
        Some(&catalog),
        vibex_tui::Strings::for_locale(vibex_tui::Locale::En),
    );
    let sign_in = rows
        .iter()
        .find(|row| row.label == "Sign in with ChatGPT")
        .expect("the Agent-managed method");
    assert!(
        sign_in.action.is_some(),
        "a re-authenticatable method was not offered"
    );

    // A verified browser flow is a sign-in even with no logout: `cursor` and
    // `grok.com` open a browser, advertise no credential store, and would
    // otherwise be drawn as an external credential the reader cannot change.
    for (agent, method) in [
        ("cursor", "cursor_login"),
        ("grok", "grok.com"),
        ("gemini", "oauth-personal"),
    ] {
        let browser: vibex_core::AgentAuthCatalog = serde_json::from_value(serde_json::json!({
            "agentId": agent,
            "methods": [{
                "id": method,
                "name": "Sign in",
                "kind": "agent",
                "effect": "writes_agent_state_home",
                "environment": []
            }],
            "supportsLogout": false,
            "status": "authentication_required",
            "refreshedAtMs": 1
        }))
        .expect("browser catalogue");
        let rows = vibex_tui::config_center::agent_auth_rows(
            Some(&browser),
            vibex_tui::Strings::for_locale(vibex_tui::Locale::En),
        );
        let row = rows
            .iter()
            .find(|row| row.label == "Sign in")
            .expect("the row");
        assert!(
            row.action.is_some(),
            "{agent}/{method} opens a browser but was drawn as external"
        );
        assert_ne!(row.chip, "the Agent's own sign-in", "{agent}/{method} chip");
    }

    // A terminal method is drivable whatever the logout capability says: the
    // client attaches to the PTY the authority opened.
    let terminal: vibex_core::AgentAuthCatalog = serde_json::from_value(serde_json::json!({
        "agentId": "kiro",
        "methods": [{
            "id": "vibex-cli-login",
            "name": "Sign in to Kiro CLI",
            "kind": "terminal",
            "effect": "writes_agent_state_home",
            "environment": []
        }],
        "supportsLogout": false,
        "status": "authentication_required",
        "refreshedAtMs": 1
    }))
    .expect("terminal catalogue");
    let rows = vibex_tui::config_center::agent_auth_rows(
        Some(&terminal),
        vibex_tui::Strings::for_locale(vibex_tui::Locale::En),
    );
    assert!(
        rows.iter().any(|row| matches!(
            row.action,
            Some(vibex_tui::config_center::AgentAuthAction::Method {
                kind: vibex_core::AgentAuthMethodKind::Terminal,
                ..
            })
        )),
        "a terminal method was withheld without a logout capability"
    );
}

/// A terminal that asked for the mouse gets the pointer, in the form a terminal
/// reports it.
///
/// A login flow that draws its own list asks for mouse reporting and expects
/// clicks to arrive as SGR reports. A client that kept the click for itself
/// would leave a list that looks clickable and is not.
#[test]
fn a_terminal_that_asked_for_the_mouse_receives_the_click() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut app = loaded(140, 40);
    app.overlay = Some(Overlay::AgentAuth {
        view: vibex_tui::config_center::AgentAuthView {
            agent_id: "codex".to_string(),
            title: "Codex".to_string(),
            selected: 0,
            loading: false,
            confirming_sign_out: false,
            notice: None,
            want_line: vibex_tui::config_center::AgentAuthLine::First,
            pending_sign_in: None,
        },
    });
    let terminal_id = vibex_core::TerminalId::new();
    app.auth_terminal = Some(vibex_tui::auth_terminal::AuthTerminal::new(
        vibex_core::AgentId::parse("codex").unwrap(),
        terminal_id,
        "Sign in".to_string(),
        10,
        40,
    ));
    // The grid the panel drew, which the click is measured against.
    app.regions.agent_auth_terminal = Some(ratatui::layout::Rect {
        x: 10,
        y: 5,
        width: 40,
        height: 10,
    });

    // A program that has not asked for the mouse keeps the pointer.
    assert!(
        app.auth_terminal_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 7,
            modifiers: KeyModifiers::NONE,
        })
        .is_none(),
        "a click was taken by a program that never asked for the mouse"
    );

    // The same program asking for it gets the click, grid-relative and
    // 1-based, in the SGR form.
    if let Some(terminal) = app.auth_terminal.as_mut() {
        terminal.advance(b"\x1b[?1000h\x1b[?1006h");
        terminal.refresh();
    }
    let (target, bytes) = app
        .auth_terminal_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 7,
            modifiers: KeyModifiers::NONE,
        })
        .expect("the click did not reach the program that asked for it");
    assert_eq!(target, app.auth_terminal.as_ref().unwrap().terminal_id);
    assert_eq!(bytes, "\x1b[<0;3;3M", "the report is not grid-relative SGR");

    // A click outside the grid is the client's, not the program's.
    assert!(
        app.auth_terminal_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 7,
            modifiers: KeyModifiers::NONE,
        })
        .is_none(),
        "a click beside the grid was sent into it"
    );
}

/// The gateway leads the Providers table and reports its own state.
///
/// A reader has to be able to tell a shared, gateway-served model from a direct
/// Provider, and to see a cooling Key without opening anything: the row is the
/// only place that information lives.
#[test]
fn the_gateway_leads_the_providers_table_and_reports_its_state() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_direct", "Direct")];
    let upstream = vibex_core::ModelProviderProfileId::new();
    app.management_data.gateway = Some(vibex_core::GatewaySnapshot {
        settings: vibex_core::ModelGatewaySettings {
            display_name: "Vibex Gateway".to_string(),
            ..vibex_core::ModelGatewaySettings::default()
        },
        runtime: vibex_core::ModelGatewayRuntimeState::running(3425, 1),
        upstreams: vec![vibex_core::GatewayUpstreamSummary {
            model_provider_profile_id: upstream,
            display_name: "DeepSeek".to_string(),
            model_count: 2,
            keys: vec![
                vibex_core::GatewayKeySummary {
                    credential_id: vibex_core::RequestId::new(),
                    display_name: "Primary".to_string(),
                    masked_hint: "sk-********abcd".to_string(),
                    state: vibex_core::GatewayKeyState::Ready,
                    used_tokens: 0,
                    limits: None,
                    models: Vec::new(),
                    proxy_url: None,
                },
                vibex_core::GatewayKeySummary {
                    credential_id: vibex_core::RequestId::new(),
                    display_name: "Backup".to_string(),
                    masked_hint: "sk-********efgh".to_string(),
                    state: vibex_core::GatewayKeyState::Cooling {
                        kind: vibex_core::GatewayFailureKind::Rate,
                        until_ms: Some(1),
                    },
                    used_tokens: 0,
                    limits: None,
                    models: Vec::new(),
                    proxy_url: None,
                },
            ],
        }],
        models: vec![vibex_core::GatewayModelSummary {
            model_provider_profile_id: vibex_core::ModelProviderProfileId::new(),
            provider_name: "Team".to_string(),
            model_id: "deepseek-chat".to_string(),
            display_id: "group/model_route_test/deepseek-chat".to_string(),
            wire_protocol_id: String::new(),
            route_id: Some("model_route_test".to_string()),
            source_count: 1,
            model_count: 1,
        }],
    });
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    let strings = app.strings;
    let rows = app.config_center.rows(&app.management_data, strings);
    assert_eq!(
        rows.len(),
        5,
        "the gateway, its upstream, two Key rows, and one direct Provider"
    );
    assert_eq!(rows[0].key, "gateway");
    assert!(rows[1].key.starts_with("gateway-upstream:"));
    assert!(rows[2].key.starts_with("gateway-key:"));
    assert!(rows[3].key.starts_with("gateway-key:"));
    assert_eq!(rows[4].key, "provider_direct");
    assert!(
        rows[2..4]
            .iter()
            .any(|row| row.cells.iter().any(|cell| cell.contains("cooling"))),
        "a Key row must say it is cooling: {:?}",
        rows[2].cells
    );
    assert!(
        rows[0].cells.iter().any(|cell| cell.contains("cooling")),
        "the gateway row must say a Key is cooling: {:?}",
        rows[0].cells
    );
    // The gateway row counts the route groups it serves, not the Agents that
    // use it: an Agent reaches the gateway by binding a group, so an Agent
    // column here would be a second authority.
    assert_eq!(
        rows[0].cells[0], "1",
        "the gateway row must count the groups it serves: {:?}",
        rows[0].cells
    );
    assert_eq!(
        rows[0].cells[3], "1",
        "the gateway row must count the models its groups publish: {:?}",
        rows[0].cells
    );
    assert!(
        !rows[0].cells.iter().any(|cell| cell.contains("codex")),
        "the gateway row must not name an Agent: {:?}",
        rows[0].cells
    );
}

/// A running gateway with one upstream, for the switch tests.
fn running_gateway() -> vibex_core::GatewaySnapshot {
    let upstream = vibex_core::ModelProviderProfileId::new();
    vibex_core::GatewaySnapshot {
        settings: vibex_core::ModelGatewaySettings {
            display_name: "Vibex Gateway".to_string(),
            ..vibex_core::ModelGatewaySettings::default()
        },
        runtime: vibex_core::ModelGatewayRuntimeState::running(3425, 1),
        upstreams: vec![vibex_core::GatewayUpstreamSummary {
            model_provider_profile_id: upstream,
            display_name: "DeepSeek".to_string(),
            model_count: 2,
            keys: Vec::new(),
        }],
        models: Vec::new(),
    }
}

/// The gateway's switch is a write, not decoration on the row.
///
/// The row has always advertised `Toggle` while the table had no arm for it, so
/// the verb was drawn, pressed, and dropped. What it means is what the row
/// shows: a gateway that is serving is turned off, and anything else asks to be
/// turned on.
#[test]
fn the_gateway_switch_writes_and_answers_the_press() {
    let mut app = loaded(140, 40);
    app.management_data.gateway = Some(running_gateway());
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    let before = app.config_center.rows(&app.management_data, app.strings);
    assert_eq!(before[0].key, "gateway");
    assert_eq!(before[0].chip, "Ready");
    assert!(
        before[0]
            .detail
            .iter()
            .any(|(label, value)| label == "address" && value.contains("3425")),
        "the running row must name the port it serves on: {:?}",
        before[0].detail
    );

    app.run_config_action_by_key("gateway", ConfigAction::Toggle);
    let after = app.config_center.rows(&app.management_data, app.strings);
    assert_eq!(after[0].chip, "off", "the row waited for the authority");
    assert!(
        app.config_center.pending.contains_key("gateway"),
        "the switch was not filed as a write"
    );
    assert!(
        after[0]
            .detail
            .iter()
            .all(|(label, value)| label != "address" || !value.contains("3425")),
        "a port the authority has not confirmed must not stay on the row: {:?}",
        after[0].detail
    );
}

/// A Key row is a write target, not decoration: switching it and preferring
/// it both reach the gateway settings command with the row's own identity.
#[test]
fn a_gateway_key_row_switches_and_prefers() {
    let mut app = loaded(140, 40);
    let upstream = vibex_core::ModelProviderProfileId::new();
    let credential = vibex_core::RequestId::new();
    app.management_data.gateway = Some(vibex_core::GatewaySnapshot {
        settings: vibex_core::ModelGatewaySettings {
            display_name: "Vibex Gateway".to_string(),
            ..vibex_core::ModelGatewaySettings::default()
        },
        runtime: vibex_core::ModelGatewayRuntimeState::running(3425, 1),
        upstreams: vec![vibex_core::GatewayUpstreamSummary {
            model_provider_profile_id: upstream,
            display_name: "DeepSeek".to_string(),
            model_count: 1,
            keys: vec![vibex_core::GatewayKeySummary {
                credential_id: credential,
                display_name: "Primary".to_string(),
                masked_hint: "sk-********abcd".to_string(),
                state: vibex_core::GatewayKeyState::Ready,
                used_tokens: 0,
                limits: None,
                models: Vec::new(),
                proxy_url: None,
            }],
        }],
        models: Vec::new(),
    });
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    let key = app
        .config_center
        .rows(&app.management_data, app.strings)
        .into_iter()
        .find(|row| row.key.starts_with("gateway-key:"))
        .expect("a Key row");

    app.run_config_action_by_key(&key.key, ConfigAction::Toggle);
    assert!(app.config_center.pending.contains_key(&key.key));
    app.run_config_action_by_key(&key.key, ConfigAction::Prefer);
    assert!(app.config_center.pending.contains_key(&key.key));
}

/// A gateway nobody has read yet has no row to switch.
#[test]
fn a_gateway_that_is_not_there_cannot_be_switched() {
    let mut app = loaded(140, 40);
    app.management_data.gateway = None;
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    app.run_config_action_by_key("gateway", ConfigAction::Toggle);
    assert!(
        !app.config_center.pending.contains_key("gateway"),
        "a press with no row behind it must not file a write"
    );
}

/// The switch reaches the authority, and the answer it returns is the row's
/// next state.
///
/// Only the authority knows which port comes back when the gateway is turned
/// on, and turning it off moves every bound Agent back to its own Provider, so
/// the client draws the snapshot it was handed rather than a guess.
#[test]
fn the_gateway_switch_reaches_the_authority() {
    let source = include_str!("../src/worker.rs");
    let arm = source
        .split_once("Effect::SetGatewayEnabled { enabled } => {")
        .and_then(|(_, tail)| tail.split_once("\n            Effect::"))
        .map(|(body, _)| body)
        .expect("the gateway switch must remain inspectable");
    assert!(arm.contains("set_model_gateway_enabled(enabled)"));
    assert!(arm.contains("AppMessage::Gateway(Ok(snapshot))"));
    assert!(arm.contains("failure_for(\"gateway_toggle\""));
}

/// A gateway that cannot be read leaves the direct Providers readable.
#[test]
fn a_missing_gateway_does_not_remove_the_direct_providers() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![provider_profile("provider_direct", "Direct")];
    app.management_data.gateway = None;
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));
    let rows = app.config_center.rows(&app.management_data, app.strings);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, "provider_direct");
}

/// A plan preset shows the price its vendor publishes in the picker.
///
/// The price is static vendor copy, not a computed amount; the TUI has no
/// other place to put it before the plan is chosen.
#[test]
fn a_plan_preset_shows_its_price_in_the_picker() {
    let source = include_str!("../src/view.rs");
    let picker = source
        .split_once("Overlay::ProviderPresetPicker { selected, .. } => {")
        .and_then(|(_, tail)| tail.split_once("Overlay::WorkspacePicker { selected }"))
        .map(|(body, _)| body)
        .expect("the preset picker should remain inspectable");
    assert!(picker.contains("preset.plan_note.as_deref()"));
    assert!(picker.contains("detail = if let Some(note)"));
}

/// A preset-created Provider keeps the protocol the preset chose.
///
/// This is the regression the two-server report called out: without an explicit
/// `wireApi`, the projection falls back to the first protocol that has a URL,
/// so an Anthropic preset comes back as Chat Completions the next time the
/// editor opens.
#[test]
fn a_preset_created_provider_keeps_its_protocol() {
    let mut app = loaded(140, 40);
    app.management_data.providers = vec![];
    app.show_config_module(ConfigModule::Agent, Some(ConfigSection::Providers));

    app.perform(vibex_tui::action::Intent::ConfigEntryCreate);
    // DeepSeek is not the first preset, so walk to it rather than assuming it.
    let presets = vibex_core::provider_presets();
    let deepseek = presets
        .iter()
        .position(|preset| preset.id == vibex_core::PROVIDER_PRESET_DEEPSEEK)
        .expect("the first batch ships DeepSeek");
    for _ in 0..deepseek {
        app.perform(vibex_tui::action::Intent::SelectNext);
    }
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);

    let outcome = app.perform(vibex_tui::action::Intent::ConfigFormSave);
    let vibex_tui::app::Effect::ConfigMutation { mutation, .. } = &outcome.effects[0] else {
        panic!("the save produced {:?}", outcome.effects);
    };
    let vibex_tui::app::ConfigMutation::CreateProviderProfile(request) = mutation else {
        panic!("the save was not a creation");
    };
    let options = request
        .provider_options
        .as_ref()
        .expect("a preset-created Provider must carry its options");
    let wire_api = options
        .entries
        .iter()
        .find(|entry| entry.key == "wireApi")
        .map(|entry| entry.value.as_str());
    assert_eq!(
        wire_api,
        Some(vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS),
        "the preset's protocol was not written explicitly: {:?}",
        options.entries
    );
    // Both of DeepSeek's addresses travel, so switching protocol later does not
    // need the reader to look the other one up.
    assert!(
        options
            .entries
            .iter()
            .any(|entry| entry.key.starts_with("protocolBaseUrl.")),
        "the per-protocol addresses were dropped: {:?}",
        options.entries
    );
    assert!(
        request
            .configured_models
            .iter()
            .all(|model| model.wire_api.is_some()),
        "the preset's models lost their protocol"
    );
}
