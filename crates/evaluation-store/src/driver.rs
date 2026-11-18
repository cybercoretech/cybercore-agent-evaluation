//! The scheduler loop: reserve → execute → complete → record, durably.

use crate::error::{StoreError, StoreResult};
use crate::local::LocalStore;
use crate::metadata::now;
use crate::recorder::{CompletedAttempt, RecordReport, RunRecorderConfig, Verifier};
use crate::scheduler::DurableScheduler;
use cybercore_evaluation_core::{execute_with_runner, AttemptIdentity, Runner};

/// What one `run_ready_trials` call did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DriveReport {
    /// Records written for attempts completed before this call (catch-up).
    pub caught_up: RecordReport,
    /// Run IDs executed and recorded by this call, in order.
    pub executed: Vec<String>,
}

/// Runs every ready trial of the scheduler's evaluation, one attempt at a
/// time, until none is ready (all complete, exhausted or cancelled).
///
/// For each attempt: reserve (journaled), execute with `runner`, complete
/// (journaled), then write the run, run_event and verification_result records
/// in one transaction and renew the lease. Records missing from an earlier
/// crash are written first. `identity_for(trial_id, attempt_number)` must
/// return globally fresh identities.
pub async fn run_ready_trials<F>(
    store: &LocalStore,
    scheduler: &mut DurableScheduler<'_>,
    runner: &dyn Runner,
    config: &RunRecorderConfig,
    verifier: &dyn Verifier,
    mut identity_for: F,
) -> StoreResult<DriveReport>
where
    F: FnMut(&str, u32) -> AttemptIdentity,
{
    let mut report = DriveReport {
        caught_up: store.record_completed_runs(scheduler, config, verifier)?,
        executed: Vec::new(),
    };
    let schedule = scheduler.store().load_schedule(scheduler.evaluation_id())?;

    while let Some(trial_id) = scheduler.coordinator().ready_trial_ids().first().cloned() {
        let trial = schedule
            .trials
            .iter()
            .find(|t| t.trial_ref.record_id == trial_id)
            .ok_or_else(|| StoreError::Integrity(format!("schedule has no trial {trial_id}")))?;
        let attempt_number = u32::try_from(
            scheduler
                .coordinator()
                .snapshot(&trial_id)
                .map_err(StoreError::Orchestration)?
                .attempts
                .len()
                + 1,
        )
        .map_err(|_| StoreError::Integrity("attempt number overflow".into()))?;
        let identity = identity_for(&trial_id, attempt_number);

        let reserved_at = now();
        let reservation = scheduler.reserve_attempt(&trial_id, identity.clone())?;
        let result = execute_with_runner(runner, &reservation.request).await;
        scheduler.complete_attempt(&trial_id, &reservation.request.request_id, result.clone())?;
        store.record_attempt(
            scheduler,
            &CompletedAttempt {
                trial,
                identity: &identity,
                result: &result,
                reserved_at: &reserved_at,
            },
            config,
            verifier,
        )?;
        scheduler.renew_lease()?;
        report.executed.push(identity.run_ref.record_id);
    }
    Ok(report)
}
