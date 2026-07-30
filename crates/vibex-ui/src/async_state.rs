use serde::{Deserialize, Serialize};
use vibex_backend::{BackendError, BackendErrorKind};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncPhase {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsyncState<T> {
    pub phase: AsyncPhase,
    pub value: Option<T>,
    pub error: Option<BackendError>,
}

impl<T> Default for AsyncState<T> {
    fn default() -> Self {
        Self {
            phase: AsyncPhase::Idle,
            value: None,
            error: None,
        }
    }
}

impl<T> AsyncState<T> {
    pub fn clear(&mut self) {
        self.phase = AsyncPhase::Idle;
        self.value = None;
        self.error = None;
    }

    pub fn begin(&mut self) {
        self.phase = AsyncPhase::Loading;
        self.error = None;
    }

    pub fn resolve(&mut self, value: T) {
        self.phase = AsyncPhase::Ready;
        self.value = Some(value);
        self.error = None;
    }

    pub fn reject(&mut self, error: BackendError) {
        self.phase = AsyncPhase::Failed;
        self.error = Some(error);
    }

    pub fn is_loading(&self) -> bool {
        self.phase == AsyncPhase::Loading
    }

    pub fn is_offline(&self) -> bool {
        self.error
            .as_ref()
            .is_some_and(|error| error.kind == BackendErrorKind::Offline)
    }

    pub fn is_conflicted(&self) -> bool {
        self.error
            .as_ref()
            .is_some_and(|error| error.kind == BackendErrorKind::Conflict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_keeps_previous_value_while_loading_and_on_failure() {
        let mut state = AsyncState::default();
        state.resolve(vec![1, 2]);
        state.begin();
        assert_eq!(state.value.as_deref(), Some([1, 2].as_slice()));
        state.reject(BackendError::offline("host_offline", "host is offline"));
        assert!(state.is_offline());
        assert_eq!(state.value.as_deref(), Some([1, 2].as_slice()));
    }

    #[test]
    fn clear_invalidates_values_when_the_view_generation_changes() {
        let mut state = AsyncState::default();
        state.resolve(vec![1, 2]);
        state.clear();
        assert_eq!(state.phase, AsyncPhase::Idle);
        assert!(state.value.is_none());
        assert!(state.error.is_none());
    }
}
