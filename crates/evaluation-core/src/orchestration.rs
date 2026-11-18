//! In-memory evaluation/trial orchestration primitives.
//!
//! An evaluation is a campaign, a trial is one planned statistical observation,
//! and a run is one execution attempt. Retries always create new run identities
//! within the same trial. This module does not persist records or grade outcomes.

use crate::runner::{
    validate_result, ExecutionBudget, ExecutionDisposition, ExecutionRequest, NetworkPolicy,
    RecordReference, RunnerError, RunnerErrorKind, RunnerResult, SideEffectPolicy,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    /// Total execution attempts per trial, including the initial attempt.
    pub max_attempts_per_trial: u32,
    /// Retry only retryable `Unavailable` or `Internal` runner errors.
    pub retry_retryable_runner_errors: bool,
    /// Retry only explicitly listed execution failure codes when the runner marks
    /// the failure retryable. An empty set means no execution failure is retryable.
    pub retryable_execution_failure_codes: BTreeSet<String>,
    /// Retry timeouts. This is independent of the execution-failure code allowlist.
    pub retry_timeouts: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts_per_trial: 1,
            retry_retryable_runner_errors: false,
            retryable_execution_failure_codes: BTreeSet::new(),
            retry_timeouts: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialDefinition {
    pub trial_ref: RecordReference,
    pub task_revision_ref: RecordReference,
    pub system_fingerprint_ref: RecordReference,
    /// Agent-visible task input only. Hidden grader data must never be included.
    pub task_input: Value,
    pub budget: ExecutionBudget,
    pub network_policy: NetworkPolicy,
    pub side_effect_policy: SideEffectPolicy,
    pub capability_grant_refs: Vec<RecordReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSchedule {
    pub evaluation_ref: RecordReference,
    pub plan_ref: RecordReference,
    /// Maximum number of attempts this in-memory coordinator may reserve at once.
    /// A durable multi-process limit must additionally be enforced by the store/queue.
    pub max_active_runs: usize,
    pub retry_policy: RetryPolicy,
    pub trials: Vec<TrialDefinition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialExecutionState {
    Ready,
    InFlight,
    RetryReady,
    ExecutionComplete,
    AttemptsExhausted,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    Reserved,
    Finished,
    Errored,
    TimedOut,
    Cancelled,
    RunnerError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryReason {
    RunnerUnavailable,
    RunnerInternal,
    ExecutionFailure,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptAction {
    /// Execution completed; independent grading and verification remain separate.
    AwaitingAssessment,
    RetryScheduled,
    AttemptsExhausted,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptIdentity {
    pub run_ref: RecordReference,
    pub request_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialAttempt {
    pub attempt_number: u32,
    pub run_ref: RecordReference,
    pub request_id: String,
    pub idempotency_key: String,
    pub status: AttemptStatus,
    pub disposition: Option<ExecutionDisposition>,
    pub runner_error_kind: Option<RunnerErrorKind>,
    /// Machine-readable failure code only; raw runner messages are not retained.
    pub failure_code: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialSnapshot {
    pub trial_ref: RecordReference,
    pub state: TrialExecutionState,
    pub attempts: Vec<TrialAttempt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptDecision {
    pub trial_ref: RecordReference,
    pub attempt_number: u32,
    pub state: TrialExecutionState,
    pub action: AttemptAction,
    pub retry_reason: Option<RetryReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttemptReservation {
    pub trial_ref: RecordReference,
    pub attempt_number: u32,
    pub request: ExecutionRequest,
}

#[derive(Debug)]
pub enum OrchestrationError {
    InvalidSchedule(String),
    UnknownTrial(String),
    TrialNotReady(String),
    ConcurrencyLimitReached { limit: usize },
    InvalidAttemptIdentity(String),
    DuplicateAttemptIdentity(String),
    NoActiveAttempt(String),
    RequestIdMismatch { expected: String, actual: String },
    ActiveAttemptCannotBeCancelled(String),
}

impl fmt::Display for OrchestrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSchedule(message) => write!(f, "invalid evaluation schedule: {message}"),
            Self::UnknownTrial(id) => write!(f, "unknown trial: {id}"),
            Self::TrialNotReady(id) => write!(f, "trial is not ready for an attempt: {id}"),
            Self::ConcurrencyLimitReached { limit } => {
                write!(f, "active run limit reached ({limit})")
            }
            Self::InvalidAttemptIdentity(message) => {
                write!(f, "invalid attempt identity: {message}")
            }
            Self::DuplicateAttemptIdentity(value) => {
                write!(f, "attempt identity was already used: {value}")
            }
            Self::NoActiveAttempt(id) => write!(f, "trial has no active attempt: {id}"),
            Self::RequestIdMismatch { expected, actual } => {
                write!(
                    f,
                    "attempt request ID mismatch: expected {expected}, received {actual}"
                )
            }
            Self::ActiveAttemptCannotBeCancelled(id) => {
                write!(f, "trial cannot be cancelled while an attempt is active or after it is terminal: {id}")
            }
        }
    }
}

impl Error for OrchestrationError {}

#[derive(Debug, Clone)]
struct ActiveAttempt {
    request: ExecutionRequest,
    attempt_number: u32,
}

#[derive(Debug, Clone)]
struct TrialRuntime {
    definition: TrialDefinition,
    state: TrialExecutionState,
    attempts: Vec<TrialAttempt>,
    active: Option<ActiveAttempt>,
}

/// Coordinates trial reservations and retry decisions within one process.
///
/// This object is intentionally in-memory and single-owner. It is not a durable
/// scheduler, distributed semaphore, or idempotency store. A persistent adapter
/// must atomically reserve attempts and enforce uniqueness/concurrency across
/// processes before using these decisions in a hosted deployment.
pub struct TrialCoordinator {
    evaluation_ref: RecordReference,
    plan_ref: RecordReference,
    max_active_runs: usize,
    retry_policy: RetryPolicy,
    active_runs: usize,
    trials: BTreeMap<String, TrialRuntime>,
    seen_run_ids: BTreeSet<String>,
    seen_request_ids: BTreeSet<String>,
    seen_idempotency_keys: BTreeSet<String>,
}

impl TrialCoordinator {
    pub fn new(schedule: EvaluationSchedule) -> Result<Self, OrchestrationError> {
        validate_reference(&schedule.evaluation_ref, "evaluation", "evaluation_ref")?;
        validate_reference(&schedule.plan_ref, "evaluation_plan", "plan_ref")?;
        if schedule.max_active_runs == 0 {
            return Err(OrchestrationError::InvalidSchedule(
                "max_active_runs must be greater than zero".into(),
            ));
        }
        if schedule.retry_policy.max_attempts_per_trial == 0 {
            return Err(OrchestrationError::InvalidSchedule(
                "max_attempts_per_trial must be at least one".into(),
            ));
        }
        if schedule
            .retry_policy
            .retryable_execution_failure_codes
            .iter()
            .any(|code| code.trim().is_empty())
        {
            return Err(OrchestrationError::InvalidSchedule(
                "retryable execution failure codes must not be empty".into(),
            ));
        }
        if schedule.trials.is_empty() {
            return Err(OrchestrationError::InvalidSchedule(
                "an evaluation schedule must contain at least one trial".into(),
            ));
        }

        let mut trials = BTreeMap::new();
        for definition in schedule.trials {
            validate_reference(&definition.trial_ref, "trial", "trial_ref")?;
            validate_reference(
                &definition.task_revision_ref,
                "task_revision",
                "task_revision_ref",
            )?;
            validate_reference(
                &definition.system_fingerprint_ref,
                "system_fingerprint",
                "system_fingerprint_ref",
            )?;
            if definition.budget.max_wall_time_ms == 0 {
                return Err(OrchestrationError::InvalidSchedule(
                    "each trial must have a positive max_wall_time_ms".into(),
                ));
            }
            let id = definition.trial_ref.record_id.clone();
            if trials.contains_key(&id) {
                return Err(OrchestrationError::InvalidSchedule(format!(
                    "duplicate trial ID: {id}"
                )));
            }
            trials.insert(
                id,
                TrialRuntime {
                    definition,
                    state: TrialExecutionState::Ready,
                    attempts: Vec::new(),
                    active: None,
                },
            );
        }

        Ok(Self {
            evaluation_ref: schedule.evaluation_ref,
            plan_ref: schedule.plan_ref,
            max_active_runs: schedule.max_active_runs,
            retry_policy: schedule.retry_policy,
            active_runs: 0,
            trials,
            seen_run_ids: BTreeSet::new(),
            seen_request_ids: BTreeSet::new(),
            seen_idempotency_keys: BTreeSet::new(),
        })
    }

    pub fn evaluation_ref(&self) -> &RecordReference {
        &self.evaluation_ref
    }

    pub fn plan_ref(&self) -> &RecordReference {
        &self.plan_ref
    }

    pub fn active_run_count(&self) -> usize {
        self.active_runs
    }

    pub fn available_run_slots(&self) -> usize {
        self.max_active_runs.saturating_sub(self.active_runs)
    }

    pub fn trial_count(&self) -> usize {
        self.trials.len()
    }

    pub fn ready_trial_ids(&self) -> Vec<String> {
        self.trials
            .iter()
            .filter(|(_, runtime)| {
                matches!(
                    runtime.state,
                    TrialExecutionState::Ready | TrialExecutionState::RetryReady
                )
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn snapshot(&self, trial_id: &str) -> Result<TrialSnapshot, OrchestrationError> {
        let runtime = self
            .trials
            .get(trial_id)
            .ok_or_else(|| OrchestrationError::UnknownTrial(trial_id.to_string()))?;
        Ok(TrialSnapshot {
            trial_ref: runtime.definition.trial_ref.clone(),
            state: runtime.state,
            attempts: runtime.attempts.clone(),
        })
    }

    /// Reserve exactly one run attempt for a planned trial. The caller supplies
    /// fresh run/request/idempotency identities; this core never invents IDs.
    pub fn reserve_attempt(
        &mut self,
        trial_id: &str,
        identity: AttemptIdentity,
    ) -> Result<AttemptReservation, OrchestrationError> {
        let runtime = self
            .trials
            .get(trial_id)
            .ok_or_else(|| OrchestrationError::UnknownTrial(trial_id.to_string()))?;
        if !matches!(
            runtime.state,
            TrialExecutionState::Ready | TrialExecutionState::RetryReady
        ) {
            return Err(OrchestrationError::TrialNotReady(trial_id.to_string()));
        }
        if runtime.attempts.len() >= self.retry_policy.max_attempts_per_trial as usize {
            return Err(OrchestrationError::TrialNotReady(trial_id.to_string()));
        }
        if self.active_runs >= self.max_active_runs {
            return Err(OrchestrationError::ConcurrencyLimitReached {
                limit: self.max_active_runs,
            });
        }
        validate_reference(&identity.run_ref, "run", "run_ref")?;
        if identity.request_id.trim().is_empty() || identity.idempotency_key.trim().is_empty() {
            return Err(OrchestrationError::InvalidAttemptIdentity(
                "request_id and idempotency_key are required".into(),
            ));
        }
        if self.seen_run_ids.contains(&identity.run_ref.record_id) {
            return Err(OrchestrationError::DuplicateAttemptIdentity(
                identity.run_ref.record_id,
            ));
        }
        if self.seen_request_ids.contains(&identity.request_id) {
            return Err(OrchestrationError::DuplicateAttemptIdentity(
                identity.request_id,
            ));
        }
        if self
            .seen_idempotency_keys
            .contains(&identity.idempotency_key)
        {
            return Err(OrchestrationError::DuplicateAttemptIdentity(
                identity.idempotency_key,
            ));
        }

        let definition = runtime.definition.clone();
        let attempt_number = u32::try_from(runtime.attempts.len() + 1)
            .map_err(|_| OrchestrationError::InvalidSchedule("attempt number overflow".into()))?;
        let request = ExecutionRequest {
            request_id: identity.request_id.clone(),
            idempotency_key: identity.idempotency_key.clone(),
            run_ref: identity.run_ref.clone(),
            evaluation_ref: self.evaluation_ref.clone(),
            trial_ref: definition.trial_ref.clone(),
            task_revision_ref: definition.task_revision_ref,
            system_fingerprint_ref: definition.system_fingerprint_ref,
            task_input: definition.task_input,
            budget: definition.budget,
            network_policy: definition.network_policy,
            side_effect_policy: definition.side_effect_policy,
            capability_grant_refs: definition.capability_grant_refs,
        };

        self.seen_run_ids.insert(identity.run_ref.record_id.clone());
        self.seen_request_ids.insert(identity.request_id.clone());
        self.seen_idempotency_keys
            .insert(identity.idempotency_key.clone());

        let runtime = self
            .trials
            .get_mut(trial_id)
            .ok_or_else(|| OrchestrationError::UnknownTrial(trial_id.to_string()))?;
        runtime.attempts.push(TrialAttempt {
            attempt_number,
            run_ref: identity.run_ref,
            request_id: identity.request_id,
            idempotency_key: identity.idempotency_key,
            status: AttemptStatus::Reserved,
            disposition: None,
            runner_error_kind: None,
            failure_code: None,
            retryable: false,
        });
        runtime.active = Some(ActiveAttempt {
            request: request.clone(),
            attempt_number,
        });
        runtime.state = TrialExecutionState::InFlight;
        self.active_runs += 1;

        Ok(AttemptReservation {
            trial_ref: runtime.definition.trial_ref.clone(),
            attempt_number,
            request,
        })
    }

    /// Complete the active attempt with the normalized runner result/error.
    /// Successful execution only advances the trial to `ExecutionComplete`; it
    /// does not assign pass/fail or verification state.
    pub fn complete_attempt(
        &mut self,
        trial_id: &str,
        request_id: &str,
        result: Result<RunnerResult, RunnerError>,
    ) -> Result<AttemptDecision, OrchestrationError> {
        let retry_policy = self.retry_policy.clone();
        let runtime = self
            .trials
            .get_mut(trial_id)
            .ok_or_else(|| OrchestrationError::UnknownTrial(trial_id.to_string()))?;
        let active = runtime
            .active
            .as_ref()
            .ok_or_else(|| OrchestrationError::NoActiveAttempt(trial_id.to_string()))?;
        if active.request.request_id != request_id {
            return Err(OrchestrationError::RequestIdMismatch {
                expected: active.request.request_id.clone(),
                actual: request_id.to_string(),
            });
        }
        let active = runtime.active.take().expect("active attempt was checked");
        let attempt_number = active.attempt_number;
        let attempts_used = runtime.attempts.len();
        let validated_result = match result {
            Ok(value) => match validate_result(&active.request, &value) {
                Ok(()) => Ok(value),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        let attempt = runtime
            .attempts
            .last_mut()
            .expect("active attempt always has a corresponding attempt record");

        let (action, retry_reason, next_state) = match validated_result {
            Ok(value) => {
                attempt.disposition = Some(value.disposition);
                match value.disposition {
                    ExecutionDisposition::Finished => {
                        attempt.status = AttemptStatus::Finished;
                        (
                            AttemptAction::AwaitingAssessment,
                            None,
                            TrialExecutionState::ExecutionComplete,
                        )
                    }
                    ExecutionDisposition::Cancelled => {
                        attempt.status = AttemptStatus::Cancelled;
                        (
                            AttemptAction::Cancelled,
                            None,
                            TrialExecutionState::Cancelled,
                        )
                    }
                    ExecutionDisposition::TimedOut => {
                        attempt.status = AttemptStatus::TimedOut;
                        choose_retry(
                            attempts_used,
                            retry_policy.max_attempts_per_trial,
                            retry_policy.retry_timeouts,
                            RetryReason::Timeout,
                        )
                    }
                    ExecutionDisposition::Errored => {
                        attempt.status = AttemptStatus::Errored;
                        let failure = value
                            .failure
                            .as_ref()
                            .expect("validated errored result has failure");
                        attempt.failure_code = Some(failure.code.clone());
                        attempt.retryable = failure.retryable;
                        let allowed = failure.retryable
                            && retry_policy
                                .retryable_execution_failure_codes
                                .contains(&failure.code);
                        choose_retry(
                            attempts_used,
                            retry_policy.max_attempts_per_trial,
                            allowed,
                            RetryReason::ExecutionFailure,
                        )
                    }
                }
            }
            Err(error) => {
                attempt.status = AttemptStatus::RunnerError;
                attempt.runner_error_kind = Some(error.kind);
                attempt.retryable = error.retryable;
                let retry_reason = if retry_policy.retry_retryable_runner_errors && error.retryable
                {
                    match error.kind {
                        RunnerErrorKind::Unavailable => Some(RetryReason::RunnerUnavailable),
                        RunnerErrorKind::Internal => Some(RetryReason::RunnerInternal),
                        _ => None,
                    }
                } else {
                    None
                };
                match retry_reason {
                    Some(reason) => choose_retry(
                        attempts_used,
                        retry_policy.max_attempts_per_trial,
                        true,
                        reason,
                    ),
                    None => (
                        AttemptAction::AttemptsExhausted,
                        None,
                        TrialExecutionState::AttemptsExhausted,
                    ),
                }
            }
        };

        runtime.state = next_state;
        let trial_ref = runtime.definition.trial_ref.clone();
        self.active_runs = self.active_runs.saturating_sub(1);

        Ok(AttemptDecision {
            trial_ref,
            attempt_number,
            state: next_state,
            action,
            retry_reason,
        })
    }

    /// Cancel a trial that has not started an attempt or is waiting for a retry.
    /// An in-flight runner needs an explicit cancellation protocol of its own.
    pub fn cancel_trial(&mut self, trial_id: &str) -> Result<TrialSnapshot, OrchestrationError> {
        let runtime = self
            .trials
            .get_mut(trial_id)
            .ok_or_else(|| OrchestrationError::UnknownTrial(trial_id.to_string()))?;
        if !matches!(
            runtime.state,
            TrialExecutionState::Ready | TrialExecutionState::RetryReady
        ) {
            return Err(OrchestrationError::ActiveAttemptCannotBeCancelled(
                trial_id.to_string(),
            ));
        }
        runtime.state = TrialExecutionState::Cancelled;
        Ok(TrialSnapshot {
            trial_ref: runtime.definition.trial_ref.clone(),
            state: runtime.state,
            attempts: runtime.attempts.clone(),
        })
    }
}

fn validate_reference(
    reference: &RecordReference,
    expected_type: &str,
    field: &str,
) -> Result<(), OrchestrationError> {
    if reference.record_type != expected_type || reference.record_id.trim().is_empty() {
        return Err(OrchestrationError::InvalidSchedule(format!(
            "{field} must reference a non-empty {expected_type} record"
        )));
    }
    Ok(())
}

fn choose_retry(
    attempts_used: usize,
    max_attempts: u32,
    retry_allowed: bool,
    reason: RetryReason,
) -> (AttemptAction, Option<RetryReason>, TrialExecutionState) {
    if retry_allowed && attempts_used < max_attempts as usize {
        (
            AttemptAction::RetryScheduled,
            Some(reason),
            TrialExecutionState::RetryReady,
        )
    } else {
        (
            AttemptAction::AttemptsExhausted,
            if retry_allowed { Some(reason) } else { None },
            TrialExecutionState::AttemptsExhausted,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{ArtifactManifest, ResourceUsage, RunnerFailure};

    fn reference(record_type: &str, record_id: &str) -> RecordReference {
        RecordReference {
            record_id: record_id.into(),
            record_type: record_type.into(),
            schema_version: Some("1.0.0".into()),
        }
    }

    fn trial(id: &str) -> TrialDefinition {
        TrialDefinition {
            trial_ref: reference("trial", id),
            task_revision_ref: reference("task_revision", "TASK-1"),
            system_fingerprint_ref: reference("system_fingerprint", "SYS-1"),
            task_input: serde_json::json!({"prompt": "evaluate the public fixture"}),
            budget: ExecutionBudget {
                max_wall_time_ms: 10_000,
                max_output_bytes: 16_384,
                max_artifact_bytes: 1_048_576,
                max_tool_calls: 5,
                max_input_tokens: Some(5_000),
                max_output_tokens: Some(1_000),
            },
            network_policy: NetworkPolicy::DenyAll,
            side_effect_policy: SideEffectPolicy::DenyAll,
            capability_grant_refs: vec![],
        }
    }

    fn schedule(
        trials: Vec<TrialDefinition>,
        retry_policy: RetryPolicy,
        max_active_runs: usize,
    ) -> EvaluationSchedule {
        EvaluationSchedule {
            evaluation_ref: reference("evaluation", "EVAL-1"),
            plan_ref: reference("evaluation_plan", "PLAN-1"),
            max_active_runs,
            retry_policy,
            trials,
        }
    }

    fn identity(run_id: &str, suffix: &str) -> AttemptIdentity {
        AttemptIdentity {
            run_ref: reference("run", run_id),
            request_id: format!("REQ-{suffix}"),
            idempotency_key: format!("IDEMP-{suffix}"),
        }
    }

    fn coordinator(policy: RetryPolicy, active: usize) -> TrialCoordinator {
        TrialCoordinator::new(schedule(
            vec![trial("TRIAL-1"), trial("TRIAL-2")],
            policy,
            active,
        ))
        .unwrap()
    }

    fn result(
        request_id: &str,
        disposition: ExecutionDisposition,
        failure: Option<RunnerFailure>,
    ) -> RunnerResult {
        RunnerResult {
            request_id: request_id.into(),
            disposition,
            events: vec![],
            artifacts: Vec::<ArtifactManifest>::new(),
            resource_usage: ResourceUsage::default(),
            output: None,
            failure,
        }
    }

    #[test]
    fn schedule_rejects_zero_concurrency_and_zero_attempts() {
        let policy = RetryPolicy::default();
        assert!(TrialCoordinator::new(schedule(vec![trial("T")], policy.clone(), 0)).is_err());
        let mut invalid = policy;
        invalid.max_attempts_per_trial = 0;
        assert!(TrialCoordinator::new(schedule(vec![trial("T")], invalid, 1)).is_err());
    }

    #[test]
    fn schedule_rejects_duplicate_trial_ids() {
        assert!(TrialCoordinator::new(schedule(
            vec![trial("DUP"), trial("DUP")],
            RetryPolicy::default(),
            2
        ))
        .is_err());
    }

    #[test]
    fn reservation_builds_a_single_run_attempt_for_the_existing_trial() {
        let mut coordinator = coordinator(RetryPolicy::default(), 2);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        assert_eq!(reservation.attempt_number, 1);
        assert_eq!(reservation.request.trial_ref.record_id, "TRIAL-1");
        assert_eq!(reservation.request.run_ref.record_id, "RUN-1");
        assert_eq!(coordinator.trial_count(), 2);
        assert_eq!(coordinator.active_run_count(), 1);
    }

    #[test]
    fn concurrency_limit_is_enforced_before_reservation() {
        let mut coordinator = coordinator(RetryPolicy::default(), 1);
        coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        assert!(matches!(
            coordinator.reserve_attempt("TRIAL-2", identity("RUN-2", "2")),
            Err(OrchestrationError::ConcurrencyLimitReached { limit: 1 })
        ));
        assert_eq!(coordinator.active_run_count(), 1);
    }

    #[test]
    fn finished_execution_waits_for_assessment_and_does_not_assign_pass() {
        let mut coordinator = coordinator(RetryPolicy::default(), 1);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        let decision = coordinator
            .complete_attempt(
                "TRIAL-1",
                &reservation.request.request_id,
                Ok(result(
                    &reservation.request.request_id,
                    ExecutionDisposition::Finished,
                    None,
                )),
            )
            .unwrap();
        assert_eq!(decision.action, AttemptAction::AwaitingAssessment);
        assert_eq!(decision.state, TrialExecutionState::ExecutionComplete);
        assert_eq!(coordinator.snapshot("TRIAL-1").unwrap().attempts.len(), 1);
        assert_eq!(coordinator.active_run_count(), 0);
    }

    #[test]
    fn retry_requires_predeclared_failure_code_and_retryable_marker() {
        let mut policy = RetryPolicy {
            max_attempts_per_trial: 2,
            ..RetryPolicy::default()
        };
        policy
            .retryable_execution_failure_codes
            .insert("provider_transient".into());
        policy.retry_retryable_runner_errors = true;
        let mut coordinator = coordinator(policy, 1);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        let failure = RunnerFailure {
            code: "provider_transient".into(),
            message: "temporary provider failure".into(),
            retryable: true,
        };
        let decision = coordinator
            .complete_attempt(
                "TRIAL-1",
                &reservation.request.request_id,
                Ok(result(
                    &reservation.request.request_id,
                    ExecutionDisposition::Errored,
                    Some(failure),
                )),
            )
            .unwrap();
        assert_eq!(decision.action, AttemptAction::RetryScheduled);
        assert_eq!(decision.retry_reason, Some(RetryReason::ExecutionFailure));
        assert_eq!(
            coordinator.ready_trial_ids(),
            vec!["TRIAL-1".to_string(), "TRIAL-2".to_string()]
        );

        let second = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-2", "2"))
            .unwrap();
        assert_eq!(second.attempt_number, 2);
        assert_eq!(second.request.trial_ref.record_id, "TRIAL-1");
        assert_eq!(coordinator.trial_count(), 2);
    }

    #[test]
    fn unlisted_or_nonretryable_execution_failure_is_not_retried() {
        let mut policy = RetryPolicy {
            max_attempts_per_trial: 3,
            ..RetryPolicy::default()
        };
        policy
            .retryable_execution_failure_codes
            .insert("allowed_code".into());
        let mut coordinator = coordinator(policy, 1);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        let failure = RunnerFailure {
            code: "different_code".into(),
            message: "failed".into(),
            retryable: true,
        };
        let decision = coordinator
            .complete_attempt(
                "TRIAL-1",
                &reservation.request.request_id,
                Ok(result(
                    &reservation.request.request_id,
                    ExecutionDisposition::Errored,
                    Some(failure),
                )),
            )
            .unwrap();
        assert_eq!(decision.action, AttemptAction::AttemptsExhausted);
    }

    #[test]
    fn retryable_runner_unavailable_error_obeys_policy_and_attempt_limit() {
        let policy = RetryPolicy {
            max_attempts_per_trial: 2,
            retry_retryable_runner_errors: true,
            ..RetryPolicy::default()
        };
        let mut coordinator = coordinator(policy, 1);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        let error = RunnerError {
            kind: RunnerErrorKind::Unavailable,
            retryable: true,
            message: "runner temporarily unavailable".into(),
        };
        let decision = coordinator
            .complete_attempt("TRIAL-1", &reservation.request.request_id, Err(error))
            .unwrap();
        assert_eq!(decision.action, AttemptAction::RetryScheduled);
        let second = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-2", "2"))
            .unwrap();
        let error = RunnerError {
            kind: RunnerErrorKind::Unavailable,
            retryable: true,
            message: "still unavailable".into(),
        };
        let final_decision = coordinator
            .complete_attempt("TRIAL-1", &second.request.request_id, Err(error))
            .unwrap();
        assert_eq!(final_decision.action, AttemptAction::AttemptsExhausted);
        assert_eq!(
            final_decision.retry_reason,
            Some(RetryReason::RunnerUnavailable)
        );
    }

    #[test]
    fn timeout_retry_is_independently_configured() {
        let policy = RetryPolicy {
            max_attempts_per_trial: 2,
            retry_timeouts: true,
            ..RetryPolicy::default()
        };
        let mut coordinator = coordinator(policy, 1);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        let decision = coordinator
            .complete_attempt(
                "TRIAL-1",
                &reservation.request.request_id,
                Ok(result(
                    &reservation.request.request_id,
                    ExecutionDisposition::TimedOut,
                    None,
                )),
            )
            .unwrap();
        assert_eq!(decision.action, AttemptAction::RetryScheduled);
        assert_eq!(decision.retry_reason, Some(RetryReason::Timeout));
    }

    #[test]
    fn cancellation_is_terminal_and_never_retried() {
        let policy = RetryPolicy {
            max_attempts_per_trial: 3,
            retry_timeouts: true,
            ..RetryPolicy::default()
        };
        let mut coordinator = coordinator(policy, 1);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        let decision = coordinator
            .complete_attempt(
                "TRIAL-1",
                &reservation.request.request_id,
                Ok(result(
                    &reservation.request.request_id,
                    ExecutionDisposition::Cancelled,
                    None,
                )),
            )
            .unwrap();
        assert_eq!(decision.action, AttemptAction::Cancelled);
        assert!(coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-2", "2"))
            .is_err());
    }

    #[test]
    fn request_mismatch_does_not_release_the_active_reservation() {
        let mut coordinator = coordinator(RetryPolicy::default(), 1);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        assert!(matches!(
            coordinator.complete_attempt(
                "TRIAL-1",
                "WRONG",
                Ok(result("WRONG", ExecutionDisposition::Finished, None))
            ),
            Err(OrchestrationError::RequestIdMismatch { .. })
        ));
        assert_eq!(coordinator.active_run_count(), 1);
        assert_eq!(
            coordinator.snapshot("TRIAL-1").unwrap().state,
            TrialExecutionState::InFlight
        );
        coordinator
            .complete_attempt(
                "TRIAL-1",
                &reservation.request.request_id,
                Ok(result(
                    &reservation.request.request_id,
                    ExecutionDisposition::Finished,
                    None,
                )),
            )
            .unwrap();
        assert_eq!(coordinator.active_run_count(), 0);
    }

    #[test]
    fn duplicate_attempt_identities_are_rejected_across_trials() {
        let mut coordinator = coordinator(RetryPolicy::default(), 2);
        coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        let duplicate = AttemptIdentity {
            run_ref: reference("run", "RUN-1"),
            request_id: "REQ-2".into(),
            idempotency_key: "IDEMP-2".into(),
        };
        assert!(matches!(
            coordinator.reserve_attempt("TRIAL-2", duplicate),
            Err(OrchestrationError::DuplicateAttemptIdentity(_))
        ));
    }

    #[test]
    fn invalid_runner_result_becomes_nonretryable_protocol_error() {
        let policy = RetryPolicy {
            max_attempts_per_trial: 3,
            retry_retryable_runner_errors: true,
            ..RetryPolicy::default()
        };
        let mut coordinator = coordinator(policy, 1);
        let reservation = coordinator
            .reserve_attempt("TRIAL-1", identity("RUN-1", "1"))
            .unwrap();
        let invalid = result("WRONG-REQUEST", ExecutionDisposition::Finished, None);
        let decision = coordinator
            .complete_attempt("TRIAL-1", &reservation.request.request_id, Ok(invalid))
            .unwrap();
        assert_eq!(decision.action, AttemptAction::AttemptsExhausted);
        let snapshot = coordinator.snapshot("TRIAL-1").unwrap();
        assert_eq!(snapshot.attempts[0].status, AttemptStatus::RunnerError);
        assert_eq!(
            snapshot.attempts[0].runner_error_kind,
            Some(RunnerErrorKind::ProtocolViolation)
        );
        assert_eq!(coordinator.active_run_count(), 0);
    }

    #[test]
    fn pending_trial_can_be_cancelled_but_inflight_trial_cannot() {
        let mut coordinator = coordinator(RetryPolicy::default(), 1);
        assert_eq!(
            coordinator.cancel_trial("TRIAL-1").unwrap().state,
            TrialExecutionState::Cancelled
        );
        coordinator
            .reserve_attempt("TRIAL-2", identity("RUN-2", "2"))
            .unwrap();
        assert!(coordinator.cancel_trial("TRIAL-2").is_err());
    }
}
