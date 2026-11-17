use crate::api::ReplayErrorClassification;
use crate::api::{ReplayError, ReplayErrorKind, ReplayLocations};
use std::fmt;
use std::path::PathBuf;

impl ReplayError {
    pub(crate) fn configuration(message: impl Into<String>) -> Self {
        Self::new(ReplayErrorKind::Configuration, message)
    }

    pub(crate) fn new(kind: ReplayErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            locations: None,
        }
    }

    pub(crate) fn trajectory(message: impl Into<String>) -> Self {
        Self::new(ReplayErrorKind::Trajectory, message)
    }

    pub(crate) fn continuation(message: impl Into<String>) -> Self {
        Self::new(ReplayErrorKind::Continuation, message)
    }

    pub(crate) fn classify_continuation(message: impl Into<String>, output: &str) -> Self {
        let message = message.into();
        let lowered = output.to_ascii_lowercase();
        let kind = if ["rate limit", "too many requests", "status 429"]
            .iter()
            .any(|needle| lowered.contains(needle))
        {
            ReplayErrorKind::ModelRateLimit
        } else if [
            "internal server error",
            "status 500",
            "status 502",
            "status 503",
        ]
        .iter()
        .any(|needle| lowered.contains(needle))
        {
            ReplayErrorKind::ModelInternal
        } else if [
            "connection refused",
            "connection reset",
            "connection timed out",
            "could not resolve host",
            "connecterror",
        ]
        .iter()
        .any(|needle| lowered.contains(needle))
        {
            ReplayErrorKind::ModelConnection
        } else {
            ReplayErrorKind::Continuation
        };
        Self::new(kind, message)
    }

    pub(crate) fn with_default_locations(
        mut self,
        run_id: impl Into<String>,
        state_dir: PathBuf,
        output_dir: PathBuf,
    ) -> Self {
        if self.locations.is_none() {
            self.locations = Some(ReplayLocations {
                run_id: run_id.into(),
                state_dir,
                output_dir,
            });
        }
        self
    }
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReplayError {}

pub(crate) trait ResultExt<T> {
    fn replay_context(
        self,
        kind: ReplayErrorKind,
        message: impl Into<String>,
    ) -> Result<T, ReplayError>;
}

impl<T, E: fmt::Display> ResultExt<T> for Result<T, E> {
    fn replay_context(
        self,
        kind: ReplayErrorKind,
        message: impl Into<String>,
    ) -> Result<T, ReplayError> {
        let message = message.into();
        self.map_err(|error| ReplayError::new(kind, format!("{message}: {error}")))
    }
}

impl ReplayErrorClassification for ReplayErrorKind {
    fn category(self) -> &'static str {
        match self {
            Self::Configuration => "configuration_error",
            Self::Trajectory => "trajectory_error",
            Self::UnsupportedAgent => "unsupported_agent",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Workspace => "workspace_error",
            Self::Executor => "executor_error",
            Self::AmbiguousExecution => "ambiguous_execution",
            Self::Continuation => "continuation_error",
            Self::ModelRateLimit => "model_rate_limit",
            Self::ModelInternal => "model_internal_error",
            Self::ModelConnection => "model_connection_error",
            Self::Internal => "internal_error",
        }
    }
    fn exit_code(self) -> i32 {
        match self {
            Self::Configuration => 2,
            Self::Trajectory => 10,
            Self::UnsupportedAgent | Self::UnsupportedVersion => 11,
            Self::Workspace => 20,
            Self::Executor => 30,
            Self::AmbiguousExecution => 31,
            Self::Continuation
            | Self::ModelRateLimit
            | Self::ModelInternal
            | Self::ModelConnection => 40,
            Self::Internal => 50,
        }
    }
    fn retryable(self) -> bool {
        matches!(
            self,
            Self::ModelRateLimit | Self::ModelInternal | Self::ModelConnection
        )
    }
}
