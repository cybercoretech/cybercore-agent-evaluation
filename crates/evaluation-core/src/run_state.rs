use serde::Serialize;
use std::fmt;

/// Execution lifecycle from `evaluation/run.schema.json`.
///
/// This dimension describes what happened during execution. It does not describe
/// whether the task passed or whether an independent verifier accepted the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Created,
    Queued,
    Starting,
    Running,
    Finalizing,
    Finished,
    Errored,
    TimedOut,
    Cancelled,
}

impl ExecutionStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Errored | Self::TimedOut | Self::Cancelled
        )
    }
}

/// Evaluator-assigned outcome. It is deliberately independent of execution status
/// and independent verification state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Pending,
    Pass,
    Fail,
    Inconclusive,
    NotScored,
    Invalid,
}

/// Independent verification lifecycle from `evaluation/run.schema.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    NotStarted,
    InProgress,
    VerifiedPass,
    VerifiedFail,
    Mixed,
    Inconclusive,
    Unavailable,
}

impl VerificationState {
    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::VerifiedPass
                | Self::VerifiedFail
                | Self::Mixed
                | Self::Inconclusive
                | Self::Unavailable
        )
    }
}

/// A small, in-memory state projection. Persisted run records remain governed by
/// the JSON Schema contracts; this type enforces legal transitions while executing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunState {
    execution_status: ExecutionStatus,
    run_outcome: RunOutcome,
    verification_state: VerificationState,
    started_at: Option<String>,
    ended_at: Option<String>,
}

impl Default for RunState {
    fn default() -> Self {
        Self::new()
    }
}

impl RunState {
    pub fn new() -> Self {
        Self {
            execution_status: ExecutionStatus::Created,
            run_outcome: RunOutcome::Pending,
            verification_state: VerificationState::NotStarted,
            started_at: None,
            ended_at: None,
        }
    }

    pub fn execution_status(&self) -> ExecutionStatus {
        self.execution_status
    }

    pub fn run_outcome(&self) -> RunOutcome {
        self.run_outcome
    }

    pub fn verification_state(&self) -> VerificationState {
        self.verification_state
    }

    pub fn started_at(&self) -> Option<&str> {
        self.started_at.as_deref()
    }

    pub fn ended_at(&self) -> Option<&str> {
        self.ended_at.as_deref()
    }

    /// Advance execution only. `at` must be an RFC 3339 timestamp validated by
    /// the contract boundary before this state transition is persisted.
    pub fn transition_execution(
        &mut self,
        next: ExecutionStatus,
        at: &str,
    ) -> Result<(), RunStateError> {
        if at.trim().is_empty() {
            return Err(RunStateError::EmptyTimestamp);
        }
        if !is_allowed_execution_transition(self.execution_status, next) {
            return Err(RunStateError::InvalidExecutionTransition {
                from: self.execution_status,
                to: next,
            });
        }

        if next == ExecutionStatus::Starting && self.started_at.is_none() {
            self.started_at = Some(at.to_owned());
        }
        if matches!(
            next,
            ExecutionStatus::Running | ExecutionStatus::Finalizing | ExecutionStatus::Finished
        ) && self.started_at.is_none()
        {
            return Err(RunStateError::StartTimestampRequired);
        }
        if next.is_terminal() {
            self.ended_at = Some(at.to_owned());
        }
        self.execution_status = next;
        Ok(())
    }

    /// Record an evaluator outcome once execution has reached a terminal state.
    /// This does not modify verification state.
    pub fn record_outcome(&mut self, outcome: RunOutcome) -> Result<(), RunStateError> {
        if !self.execution_status.is_terminal() {
            return Err(RunStateError::ExecutionNotTerminal);
        }
        if outcome == RunOutcome::Pending {
            return Err(RunStateError::PendingOutcomeCannotBeRecorded);
        }
        if self.run_outcome != RunOutcome::Pending {
            return Err(RunStateError::OutcomeAlreadyRecorded);
        }
        self.run_outcome = outcome;
        Ok(())
    }

    /// Advance independent verification. Verification can begin only after the
    /// execution attempt is terminal and never silently changes `run_outcome`.
    pub fn transition_verification(
        &mut self,
        next: VerificationState,
    ) -> Result<(), RunStateError> {
        if !self.execution_status.is_terminal() {
            return Err(RunStateError::ExecutionNotTerminal);
        }
        let allowed = match self.verification_state {
            VerificationState::NotStarted => {
                matches!(
                    next,
                    VerificationState::InProgress | VerificationState::Unavailable
                )
            }
            VerificationState::InProgress => next.is_terminal(),
            _ => false,
        };
        if !allowed {
            return Err(RunStateError::InvalidVerificationTransition {
                from: self.verification_state,
                to: next,
            });
        }
        self.verification_state = next;
        Ok(())
    }
}

fn is_allowed_execution_transition(from: ExecutionStatus, to: ExecutionStatus) -> bool {
    use ExecutionStatus::*;
    matches!(
        (from, to),
        (Created, Queued)
            | (Created, Starting)
            | (Created, Cancelled)
            | (Queued, Starting)
            | (Queued, Cancelled)
            | (Starting, Running)
            | (Starting, Errored)
            | (Starting, TimedOut)
            | (Starting, Cancelled)
            | (Running, Finalizing)
            | (Running, Errored)
            | (Running, TimedOut)
            | (Running, Cancelled)
            | (Finalizing, Finished)
            | (Finalizing, Errored)
            | (Finalizing, TimedOut)
            | (Finalizing, Cancelled)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStateError {
    EmptyTimestamp,
    StartTimestampRequired,
    ExecutionNotTerminal,
    PendingOutcomeCannotBeRecorded,
    OutcomeAlreadyRecorded,
    InvalidExecutionTransition {
        from: ExecutionStatus,
        to: ExecutionStatus,
    },
    InvalidVerificationTransition {
        from: VerificationState,
        to: VerificationState,
    },
}

impl fmt::Display for RunStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTimestamp => write!(f, "transition timestamp must not be empty"),
            Self::StartTimestampRequired => {
                write!(f, "run must have started before this transition")
            }
            Self::ExecutionNotTerminal => write!(
                f,
                "execution must be terminal before assessment or verification"
            ),
            Self::PendingOutcomeCannotBeRecorded => {
                write!(f, "pending is not a recordable run outcome")
            }
            Self::OutcomeAlreadyRecorded => write!(f, "run outcome has already been recorded"),
            Self::InvalidExecutionTransition { from, to } => {
                write!(f, "invalid execution transition: {from:?} -> {to:?}")
            }
            Self::InvalidVerificationTransition { from, to } => {
                write!(f, "invalid verification transition: {from:?} -> {to:?}")
            }
        }
    }
}

impl std::error::Error for RunStateError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish_run() -> RunState {
        let mut state = RunState::new();
        state
            .transition_execution(ExecutionStatus::Queued, "2026-11-17T12:00:00Z")
            .unwrap();
        state
            .transition_execution(ExecutionStatus::Starting, "2026-11-17T12:00:01Z")
            .unwrap();
        state
            .transition_execution(ExecutionStatus::Running, "2026-11-17T12:00:02Z")
            .unwrap();
        state
            .transition_execution(ExecutionStatus::Finalizing, "2026-11-17T12:00:03Z")
            .unwrap();
        state
            .transition_execution(ExecutionStatus::Finished, "2026-11-17T12:00:04Z")
            .unwrap();
        state
    }

    #[test]
    fn execution_follows_the_declared_lifecycle() {
        let state = finish_run();
        assert_eq!(state.execution_status(), ExecutionStatus::Finished);
        assert_eq!(state.started_at(), Some("2026-11-17T12:00:01Z"));
        assert_eq!(state.ended_at(), Some("2026-11-17T12:00:04Z"));
    }

    #[test]
    fn terminal_execution_cannot_be_reopened() {
        let mut state = finish_run();
        assert!(matches!(
            state.transition_execution(ExecutionStatus::Running, "2026-11-17T12:00:05Z"),
            Err(RunStateError::InvalidExecutionTransition { .. })
        ));
    }

    #[test]
    fn outcome_and_verification_remain_independent() {
        let mut state = finish_run();
        state
            .transition_verification(VerificationState::InProgress)
            .unwrap();
        state
            .transition_verification(VerificationState::VerifiedPass)
            .unwrap();
        assert_eq!(state.verification_state(), VerificationState::VerifiedPass);
        assert_eq!(state.run_outcome(), RunOutcome::Pending);
        state.record_outcome(RunOutcome::Pass).unwrap();
        assert_eq!(state.run_outcome(), RunOutcome::Pass);
    }

    #[test]
    fn outcomes_cannot_be_recorded_before_execution_finishes() {
        let mut state = RunState::new();
        assert_eq!(
            state.record_outcome(RunOutcome::Pass),
            Err(RunStateError::ExecutionNotTerminal)
        );
    }

    #[test]
    fn cancellation_before_start_has_end_time_but_no_start_time() {
        let mut state = RunState::new();
        state
            .transition_execution(ExecutionStatus::Cancelled, "2026-11-17T12:00:00Z")
            .unwrap();
        assert_eq!(state.started_at(), None);
        assert_eq!(state.ended_at(), Some("2026-11-17T12:00:00Z"));
    }
}
