//! The scheduler loop: reserve → execute → complete → record, durably, with
//! up to `max_active_runs` attempts executing at once.

use crate::error::{StoreError, StoreResult};
use crate::local::LocalStore;
use crate::metadata::now;
use crate::recorder::{
    staged_files, CompletedAttempt, CriterionVerdict, RecordReport, RunRecorderConfig,
    VerificationInput, Verifier,
};
use crate::scheduler::DurableScheduler;
use cybercore_evaluation_core::{
    execute_with_runner, AttemptIdentity, ExecutionDisposition, Runner, RunnerError, RunnerResult,
};
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::task::Poll;

/// What one `run_ready_trials` call did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DriveReport {
    /// Records written for attempts completed before this call (catch-up).
    pub caught_up: RecordReport,
    /// Run IDs executed and recorded by this call, in completion order.
    pub executed: Vec<String>,
    /// Most attempts that were executing at the same time.
    pub peak_concurrency: usize,
}

/// An attempt whose runner call has returned.
struct Executed {
    trial_id: String,
    identity: AttemptIdentity,
    reserved_at: String,
    result: Result<RunnerResult, RunnerError>,
    /// Verdicts for a finished execution, produced inside this attempt's own
    /// future so a slow verifier never pauses the other attempts.
    verdicts: Option<Vec<CriterionVerdict>>,
}

type Execution<'a> = Pin<Box<dyn Future<Output = Executed> + 'a>>;

/// Runs every ready trial of the scheduler's evaluation until none is ready
/// (all complete, exhausted or cancelled).
///
/// Up to the schedule's `max_active_runs` attempts execute concurrently on the
/// caller's task (no spawning, so the runner may borrow from the caller).
/// Every reservation is journaled before its attempt starts. As each attempt
/// returns, it is completed (journaled), its run, run_event and
/// verification_result records are written in one transaction, and the lease
/// is renewed; freed slots are refilled at once, including retries the
/// schedule's policy allows. Records missing from an earlier crash are
/// written first. `identity_for(trial_id, attempt_number)` must return
/// globally fresh identities.
///
/// The store calls between polls are synchronous (SQLite, fsynced); they are
/// short, but they do pause the in-flight attempts' polling while they run.
/// If an error stops the loop, attempts still executing are dropped (the
/// local runner kills them) and remain reserved in the journal, so the next
/// `acquire` records them as interrupted.
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
        ..DriveReport::default()
    };
    let schedule = scheduler.store().load_schedule(scheduler.evaluation_id())?;
    let mut in_flight: Vec<Execution<'_>> = Vec::new();

    loop {
        // Fill every free slot with a ready trial (initial attempts and
        // retries alike); a reserved trial is in flight, so it is not ready.
        while scheduler.coordinator().available_run_slots() > 0 {
            let Some(trial_id) = scheduler.coordinator().ready_trial_ids().first().cloned() else {
                break;
            };
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
            let request = reservation.request;
            let trial = schedule
                .trials
                .iter()
                .find(|t| t.trial_ref.record_id == trial_id)
                .ok_or_else(|| {
                    StoreError::Integrity(format!("schedule has no trial {trial_id}"))
                })?;
            in_flight.push(Box::pin(async move {
                let result = execute_with_runner(runner, &request).await;
                let verdicts = match &result {
                    Ok(finished) if finished.disposition == ExecutionDisposition::Finished => {
                        let files = staged_files(config, &request.request_id, &result);
                        let input = VerificationInput {
                            trial,
                            run_ref: &identity.run_ref,
                            result: finished,
                            files: &files,
                        };
                        Some(verifier.verify_async(&input).await)
                    }
                    _ => None,
                };
                Executed {
                    trial_id,
                    identity,
                    reserved_at,
                    result,
                    verdicts,
                }
            }));
        }
        report.peak_concurrency = report.peak_concurrency.max(in_flight.len());
        if in_flight.is_empty() {
            break;
        }

        // Wait for whichever attempt returns first.
        let executed = poll_fn(|cx| {
            for index in 0..in_flight.len() {
                if let Poll::Ready(done) = in_flight[index].as_mut().poll(cx) {
                    drop(in_flight.swap_remove(index));
                    return Poll::Ready(done);
                }
            }
            Poll::Pending
        })
        .await;

        let trial = schedule
            .trials
            .iter()
            .find(|t| t.trial_ref.record_id == executed.trial_id)
            .ok_or_else(|| {
                StoreError::Integrity(format!("schedule has no trial {}", executed.trial_id))
            })?;
        scheduler.complete_attempt(
            &executed.trial_id,
            &executed.identity.request_id,
            executed.result.clone(),
        )?;
        store.record_attempt(
            scheduler,
            &CompletedAttempt {
                trial,
                identity: &executed.identity,
                result: &executed.result,
                reserved_at: &executed.reserved_at,
                verdicts: executed.verdicts.as_deref(),
            },
            config,
            verifier,
        )?;
        scheduler.renew_lease()?;
        report.executed.push(executed.identity.run_ref.record_id);
    }
    Ok(report)
}
