use super::*;
use crate::action::Intent;
use crate::app::{AppOptions, Page};
use vibex_backend::{
    BackendCapabilitySnapshot, BackendOperation, DisconnectedBackend, DomainCapabilities,
};
use vibex_core::{DelegationTaskView, TeamSnapshot, VibexUseRef};

fn app() -> App {
    let mut app = App::new(
        DisconnectedBackend::facade(),
        AppOptions {
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.team = vibex_ui::TeamWorkflowController::new(
        std::sync::Arc::new(DisconnectedBackend),
        BackendCapabilitySnapshot::desktop_native_v1().agent,
    );
    app.agent.state.selected_session_id = Some(VibexSessionId::new());
    app.page = Page::Agent;
    app
}

fn snapshot(session: &VibexSessionId, revision: u64) -> TeamSnapshot {
    let task: DelegationTaskView = serde_json::from_value(serde_json::json!({
        "taskRef": VibexUseRef::task(&AgentDelegationId::parse("delegation_test").unwrap()),
        "sessionRef": VibexUseRef::session(&VibexSessionId::new()),
        "parentSessionRef": VibexUseRef::session(session), "rootSessionRef": VibexUseRef::session(session),
        "title": "Review storage", "phase": "active", "legacyStatus": "running",
        "ownershipKind": "owned_child", "completionPolicy": "owner_review",
        "revision": revision, "createdAtMs": 1, "updatedAtMs": 1
    })).unwrap();
    serde_json::from_value(serde_json::json!({
        "rootSessionId": session, "tasks": [task], "executions": [],
        "inbox": {"events": [], "nextCursor": 0, "hasMore": false},
        "tree": {"nodes": [], "registry": [], "nextCursor": null, "hasMore": false}, "groups": [],
        "capability": { "delivery": "mcp", "authority": "test",
            "callerSessionRef": VibexUseRef::session(session), "rootSessionRef": VibexUseRef::session(session),
            "currentWorkspaceRef": VibexUseRef::workspace("workspace_test"), "catalogRevision": 1,
            "activationRevision": 1, "remainingExecutions": 4, "maxDepth": 2, "remainingDepth": 2,
            "canDelegate": true, "canReadAnySession": false,
            "presentation": vibex_core::VibexUsePresentationCapability::default(), "availableTools": [] },
        "nextTaskCursor": null, "hasMoreTasks": false, "nextExecutionCursor": null, "hasMoreExecutions": false
    })).unwrap()
}

fn open_and_load(app: &mut App) {
    let result = app.perform(Intent::ToggleDock);
    let ticket = result
        .effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::LoadTeam { ticket } => Some(ticket),
            _ => None,
        })
        .unwrap();
    let loaded = snapshot(ticket.session_id(), 7);
    assert!(app.team.apply(&ticket, Ok(loaded)));
}

fn select(app: &mut App, predicate: impl Fn(&TeamDockAction) -> bool) {
    app.dock_selection = app
        .dock_rows()
        .iter()
        .position(|row| matches!(row, DockRow::Team { action, .. } if predicate(action)));
    assert!(app.dock_selection.is_some());
}

#[test]
fn team_stop_intent_requires_confirmation_for_the_current_task_revision() {
    let mut app = app();
    open_and_load(&mut app);
    select(&mut app, |action| {
        matches!(action, TeamDockAction::Inspect(_))
    });
    assert!(app.perform(Intent::DockActivate).effects.is_empty());
    select(&mut app, |action| {
        matches!(action, TeamDockAction::ConfirmStop(_))
    });
    assert!(app.perform(Intent::DockActivate).effects.is_empty());
    assert_eq!(
        app.team_confirm_cancel.as_ref().unwrap().expected_revision,
        Some(7)
    );

    // A new task revision invalidates the confirmation the reader saw.
    let ticket = app.team.begin_load(false).unwrap();
    app.team
        .apply(&ticket, Ok(snapshot(ticket.session_id(), 8)));
    assert!(!app.dock_rows().iter().any(|row| matches!(
        row,
        DockRow::Team {
            action: TeamDockAction::Control(TeamTaskControlRequest {
                action: TeamTaskAction::Cancel { .. },
                ..
            }),
            ..
        }
    )));
    select(&mut app, |action| {
        matches!(action, TeamDockAction::ConfirmStop(_))
    });
    app.perform(Intent::DockActivate);
    select(&mut app, |action| {
        matches!(action, TeamDockAction::KeepRunning)
    });
    assert!(app.perform(Intent::DockActivate).effects.is_empty());
    assert!(app.team_confirm_cancel.is_none());

    select(&mut app, |action| {
        matches!(action, TeamDockAction::ConfirmStop(_))
    });
    app.perform(Intent::DockActivate);
    select(&mut app, |action| {
        matches!(action, TeamDockAction::Control(_))
    });
    let result = app.perform(Intent::DockActivate);
    assert!(
        matches!(&result.effects[..], [Effect::ControlTeam { request }] if request.expected_revision == Some(8)
        && request.session_id == *app.team.session_id().unwrap()
        && request.task_id.as_str() == "delegation_test"
        && request.action == TeamTaskAction::Cancel { cascade: true })
    );
    assert!(app.team_busy);
    assert!(app.team_confirm_cancel.is_none());
}

#[test]
fn read_only_team_dock_exposes_inspection_and_no_mutations() {
    let mut app = app();
    app.team = vibex_ui::TeamWorkflowController::new(
        std::sync::Arc::new(DisconnectedBackend),
        DomainCapabilities::available([BackendOperation::AgentTeamRead]),
    );
    open_and_load(&mut app);
    select(&mut app, |action| {
        matches!(action, TeamDockAction::Inspect(_))
    });
    app.perform(Intent::DockActivate);
    assert!(app.dock_rows().iter().any(|row| matches!(
        row,
        DockRow::Team {
            action: TeamDockAction::Open(_),
            ..
        }
    )));
    assert!(!app.dock_rows().iter().any(|row| matches!(
        row,
        DockRow::Team {
            action: TeamDockAction::Control(_)
                | TeamDockAction::ConfirmStop(_)
                | TeamDockAction::Acknowledge(_),
            ..
        }
    )));
}

#[test]
fn team_dock_renders_with_clipped_tiny_and_normal_terminal_sizes() {
    let mut app = app();
    open_and_load(&mut app);
    for (width, height) in [(1, 1), (20, 6), (48, 16), (110, 30)] {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::view::render(frame, &mut app))
            .unwrap();
        assert_eq!(terminal.backend().buffer().area.width, width);
        assert_eq!(terminal.backend().buffer().area.height, height);
    }
}
