use vibex_core::AgentEventRawExtension;

pub(super) use vibex_ui::tool_detail::{command_cwd, invocation};

pub(super) fn output(
    extension: Option<&AgentEventRawExtension>,
    summary: Option<&str>,
    known_exit_code: Option<i32>,
) -> Option<String> {
    vibex_ui::tool_detail::output(
        extension,
        summary,
        known_exit_code,
        super::timeline_activity::current_locale(),
    )
}
