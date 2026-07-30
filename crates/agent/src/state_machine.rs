use vibex_core::{AgentSessionState, VibexError, VibexResult};

pub const fn is_transition_allowed(from: AgentSessionState, to: AgentSessionState) -> bool {
    use AgentSessionState::*;
    matches!(
        (from, to),
        (Initializing, Idle)
            | (Initializing, Error)
            | (Idle, Running)
            | (Running, Idle)
            | (Running, NeedsInput)
            | (NeedsInput, Running)
            | (NeedsInput, Error)
            | (Running, Error)
            | (Error, Running)
            | (Idle, Closed)
            | (Error, Idle)
            | (Error, Closed)
            | (Closed, Archived)
            | (Idle, Archived)
            | (Error, Archived)
    )
}

pub fn validate_transition(from: AgentSessionState, to: AgentSessionState) -> VibexResult<()> {
    if from == to || is_transition_allowed(from, to) {
        return Ok(());
    }

    Err(VibexError::conflict(
        "invalid_session_state_transition",
        format!("cannot transition Agent session from {from:?} to {to:?}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_documented_transitions() {
        assert!(is_transition_allowed(
            AgentSessionState::Initializing,
            AgentSessionState::Idle
        ));
        assert!(is_transition_allowed(
            AgentSessionState::Running,
            AgentSessionState::NeedsInput
        ));
        assert!(is_transition_allowed(
            AgentSessionState::Closed,
            AgentSessionState::Archived
        ));
        assert!(is_transition_allowed(
            AgentSessionState::Error,
            AgentSessionState::Running
        ));
    }

    #[test]
    fn rejects_undocumented_transitions() {
        assert!(
            validate_transition(AgentSessionState::Archived, AgentSessionState::Running).is_err()
        );
        assert!(
            validate_transition(AgentSessionState::Idle, AgentSessionState::NeedsInput).is_err()
        );
    }
}
