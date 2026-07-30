use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsyncPhase {
    Idle,
    Loading,
    Ready,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AsyncGeneration(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryState<T> {
    pub phase: AsyncPhase,
    pub generation: AsyncGeneration,
    pub data: Option<T>,
    pub error: Option<QueryError>,
    pub invalidated: bool,
    pub updated_at_ms: Option<i64>,
}

impl<T> Default for QueryState<T> {
    fn default() -> Self {
        Self {
            phase: AsyncPhase::Idle,
            generation: AsyncGeneration(0),
            data: None,
            error: None,
            invalidated: false,
            updated_at_ms: None,
        }
    }
}

impl<T> QueryState<T> {
    pub fn begin(&mut self) -> AsyncGeneration {
        self.generation.0 = self.generation.0.saturating_add(1);
        self.phase = AsyncPhase::Loading;
        self.error = None;
        self.generation
    }

    pub fn resolve(&mut self, generation: AsyncGeneration, data: T, updated_at_ms: i64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.phase = AsyncPhase::Ready;
        self.data = Some(data);
        self.error = None;
        self.invalidated = false;
        self.updated_at_ms = Some(updated_at_ms);
        true
    }

    pub fn reject(&mut self, generation: AsyncGeneration, error: QueryError) -> bool {
        if generation != self.generation {
            return false;
        }
        self.phase = AsyncPhase::Error;
        self.error = Some(error);
        true
    }

    pub fn cancel(&mut self, generation: AsyncGeneration) -> bool {
        if generation != self.generation || self.phase != AsyncPhase::Loading {
            return false;
        }
        self.phase = if self.data.is_some() {
            AsyncPhase::Ready
        } else {
            AsyncPhase::Idle
        };
        true
    }

    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    pub fn is_current(&self, generation: AsyncGeneration) -> bool {
        self.generation == generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationState<T> {
    pub phase: AsyncPhase,
    pub generation: AsyncGeneration,
    pub result: Option<T>,
    pub error: Option<QueryError>,
}

impl<T> Default for MutationState<T> {
    fn default() -> Self {
        Self {
            phase: AsyncPhase::Idle,
            generation: AsyncGeneration(0),
            result: None,
            error: None,
        }
    }
}

impl<T> MutationState<T> {
    pub fn begin(&mut self) -> AsyncGeneration {
        self.generation.0 = self.generation.0.saturating_add(1);
        self.phase = AsyncPhase::Loading;
        self.result = None;
        self.error = None;
        self.generation
    }

    pub fn resolve(&mut self, generation: AsyncGeneration, result: T) -> bool {
        if generation != self.generation {
            return false;
        }
        self.phase = AsyncPhase::Ready;
        self.result = Some(result);
        self.error = None;
        true
    }

    pub fn reject(&mut self, generation: AsyncGeneration, error: QueryError) -> bool {
        if generation != self.generation {
            return false;
        }
        self.phase = AsyncPhase::Error;
        self.error = Some(error);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_query_result_cannot_replace_current_data() {
        let mut query = QueryState::default();
        let old = query.begin();
        let current = query.begin();

        assert!(!query.resolve(old, "old", 10));
        assert!(query.resolve(current, "current", 20));
        assert_eq!(query.data, Some("current"));
        assert_eq!(query.updated_at_ms, Some(20));
    }

    #[test]
    fn previous_data_survives_loading_failure_and_cancel() {
        let mut query = QueryState::default();
        let initial = query.begin();
        assert!(query.resolve(initial, 7, 10));

        let failed = query.begin();
        assert_eq!(query.data, Some(7));
        assert!(query.reject(
            failed,
            QueryError {
                code: "offline".into(),
                message: "Disconnected".into(),
                retryable: true,
            }
        ));
        assert_eq!(query.data, Some(7));

        let cancelled = query.begin();
        assert!(query.cancel(cancelled));
        assert_eq!(query.phase, AsyncPhase::Ready);
    }
}
