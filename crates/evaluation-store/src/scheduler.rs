//! Durable scheduling (ADR-0010): a lease-fenced, write-ahead journal around
//! the core's deterministic `TrialCoordinator`.
//!
//! Every coordinator command is appended to `scheduler_journal` before its
//! result is handed back, so an execution request is never returned to a
//! caller unless its reservation is durable. After a crash, `acquire` rebuilds
//! the coordinator by replaying the journal and closes attempts that were in
//! flight as retryable runner errors, letting the schedule's own retry policy
//! decide whether they run again.

use crate::canonical::{canonical_json, sha256_hex};
use crate::error::{StoreError, StoreResult};
use crate::metadata::{now, PutOutcome, SqliteMetadataStore};
use chrono::{SecondsFormat, Utc};
use cybercore_evaluation_core::{
    AttemptDecision, AttemptIdentity, AttemptReservation, EvaluationSchedule, RunnerError,
    RunnerErrorKind, RunnerResult, TrialCoordinator, TrialExecutionState, TrialSnapshot,
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// Message recorded for attempts that were in flight when their scheduler
/// stopped. The outcome is unknown, so the attempt is closed as a retryable
/// internal runner error; whether it is retried is the schedule's decision.
pub const INTERRUPTED_ATTEMPT_MESSAGE: &str =
    "scheduler stopped while the attempt was in flight; its outcome is unknown";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReservePayload {
    pub(crate) trial_id: String,
    pub(crate) identity: AttemptIdentity,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompletePayload {
    pub(crate) trial_id: String,
    pub(crate) request_id: String,
    pub(crate) result: Result<RunnerResult, RunnerError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelPayload {
    pub(crate) trial_id: String,
}

pub(crate) enum Command {
    Reserve(ReservePayload),
    Complete(CompletePayload),
    Cancel(CancelPayload),
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Reserve(_) => "reserve",
            Self::Complete(_) => "complete",
            Self::Cancel(_) => "cancel",
        }
    }

    fn payload(&self) -> StoreResult<Value> {
        let value = match self {
            Self::Reserve(payload) => serde_json::to_value(payload),
            Self::Complete(payload) => serde_json::to_value(payload),
            Self::Cancel(payload) => serde_json::to_value(payload),
        };
        value.map_err(|error| StoreError::InvalidRecord(format!("journal payload: {error}")))
    }

    fn parse(name: &str, payload: &str) -> StoreResult<Self> {
        let bad = |error: serde_json::Error| {
            StoreError::Integrity(format!("journal {name} payload is unreadable: {error}"))
        };
        Ok(match name {
            "reserve" => Self::Reserve(serde_json::from_str(payload).map_err(bad)?),
            "complete" => Self::Complete(serde_json::from_str(payload).map_err(bad)?),
            "cancel" => Self::Cancel(serde_json::from_str(payload).map_err(bad)?),
            other => {
                return Err(StoreError::Integrity(format!(
                    "unknown journal command {other:?}"
                )))
            }
        })
    }
}

pub(crate) struct JournalEntry {
    pub(crate) seq: i64,
    pub(crate) command: Command,
    pub(crate) recorded_at: String,
}

/// What `acquire` found and repaired.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Journal entries replayed.
    pub replayed_commands: u64,
    /// Request IDs of attempts that were in flight and were closed as
    /// interrupted (retryable internal runner errors).
    pub interrupted_attempts: Vec<String>,
}

impl SqliteMetadataStore {
    /// Stores the frozen schedule for an evaluation. The schedule is validated
    /// by constructing a coordinator from it. Storing an identical schedule
    /// again is a no-op; a different schedule for the same evaluation is a
    /// `Conflict` (schedules are immutable).
    pub fn create_schedule(&self, schedule: &EvaluationSchedule) -> StoreResult<PutOutcome> {
        TrialCoordinator::new(schedule.clone()).map_err(StoreError::Orchestration)?;
        let value = serde_json::to_value(schedule)
            .map_err(|error| StoreError::InvalidRecord(format!("schedule: {error}")))?;
        let body = canonical_json(&value);
        let digest = sha256_hex(body.as_bytes());
        let evaluation_id = &schedule.evaluation_ref.record_id;

        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT schedule_sha256 FROM evaluation_schedules WHERE evaluation_id = ?1",
                params![evaluation_id],
                |row| row.get(0),
            )
            .optional()?;
        let outcome = match existing {
            Some(stored) if stored == digest => PutOutcome::AlreadyPresent,
            Some(_) => {
                return Err(StoreError::Conflict(format!(
                    "evaluation {evaluation_id} already has a different schedule"
                )))
            }
            None => {
                transaction.execute(
                    "INSERT INTO evaluation_schedules (evaluation_id, schedule, schedule_sha256, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![evaluation_id, body, digest, now()],
                )?;
                PutOutcome::Inserted
            }
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub(crate) fn load_schedule(&self, evaluation_id: &str) -> StoreResult<EvaluationSchedule> {
        let connection = self.lock()?;
        let (body, digest): (String, String) = connection
            .query_row(
                "SELECT schedule, schedule_sha256 FROM evaluation_schedules WHERE evaluation_id = ?1",
                params![evaluation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("schedule for {evaluation_id}")))?;
        if sha256_hex(body.as_bytes()) != digest {
            return Err(StoreError::Integrity(format!(
                "schedule for {evaluation_id} does not match its digest"
            )));
        }
        serde_json::from_str(&body).map_err(|error| {
            StoreError::Integrity(format!("schedule for {evaluation_id}: {error}"))
        })
    }

    fn load_journal(&self, evaluation_id: &str) -> StoreResult<Vec<(i64, Command)>> {
        Ok(self
            .load_journal_entries(evaluation_id)?
            .into_iter()
            .map(|entry| (entry.seq, entry.command))
            .collect())
    }

    /// The journal in `seq` order with each entry's commit time. Fails with
    /// `Integrity` on a gap or an unreadable payload.
    pub(crate) fn load_journal_entries(
        &self,
        evaluation_id: &str,
    ) -> StoreResult<Vec<JournalEntry>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare_cached(
            "SELECT seq, command, payload, recorded_at FROM scheduler_journal
             WHERE evaluation_id = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map(params![evaluation_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut entries = Vec::new();
        for (expected, row) in (1_i64..).zip(rows) {
            let (seq, name, payload, recorded_at) = row?;
            if seq != expected {
                return Err(StoreError::Integrity(format!(
                    "journal for {evaluation_id} has a gap at seq {expected}"
                )));
            }
            entries.push(JournalEntry {
                seq,
                command: Command::parse(&name, &payload)?,
                recorded_at,
            });
        }
        Ok(entries)
    }
}

/// The single, lease-holding scheduler for one evaluation.
///
/// Typical loop: `acquire` → for each slot, `reserve_attempt` and hand the
/// returned request to a runner → `complete_attempt` with the runner's result
/// → `renew_lease` well before `lease_ttl` elapses → `release` when done.
/// Any `LeaseLost` error means another scheduler took over: stop at once.
pub struct DurableScheduler<'s> {
    store: &'s SqliteMetadataStore,
    evaluation_id: String,
    owner: String,
    fencing_token: i64,
    lease_ttl: Duration,
    next_seq: i64,
    coordinator: TrialCoordinator,
}

impl<'s> DurableScheduler<'s> {
    /// Takes the lease on an evaluation (failing with `LeaseHeld` while
    /// another owner's lease is unexpired), rebuilds the coordinator from the
    /// journal, and closes attempts left in flight by a previous scheduler.
    pub fn acquire(
        store: &'s SqliteMetadataStore,
        evaluation_id: &str,
        owner: &str,
        lease_ttl: Duration,
    ) -> StoreResult<(Self, RecoveryReport)> {
        if owner.trim().is_empty() {
            return Err(StoreError::InvalidRecord(
                "scheduler owner must not be empty".into(),
            ));
        }
        let schedule = store.load_schedule(evaluation_id)?;
        let fencing_token = take_lease(store, evaluation_id, owner, lease_ttl)?;

        let mut scheduler = Self {
            store,
            evaluation_id: evaluation_id.to_string(),
            owner: owner.to_string(),
            fencing_token,
            lease_ttl,
            next_seq: 1,
            coordinator: TrialCoordinator::new(schedule).map_err(StoreError::Orchestration)?,
        };
        let replayed = scheduler.rebuild()?;

        let mut report = RecoveryReport {
            replayed_commands: replayed,
            interrupted_attempts: Vec::new(),
        };
        for (trial_id, request_id) in scheduler.in_flight_attempts()? {
            scheduler.complete_attempt(
                &trial_id,
                &request_id,
                Err(RunnerError {
                    kind: RunnerErrorKind::Internal,
                    retryable: true,
                    message: INTERRUPTED_ATTEMPT_MESSAGE.into(),
                }),
            )?;
            report.interrupted_attempts.push(request_id);
        }
        Ok((scheduler, report))
    }

    /// Read-only view of the rebuilt coordinator (ready trials, snapshots,
    /// free slots).
    pub fn coordinator(&self) -> &TrialCoordinator {
        &self.coordinator
    }

    pub fn fencing_token(&self) -> i64 {
        self.fencing_token
    }

    /// Reserves an attempt. The returned request is only handed back after the
    /// reservation and its identities are durably journaled.
    pub fn reserve_attempt(
        &mut self,
        trial_id: &str,
        identity: AttemptIdentity,
    ) -> StoreResult<AttemptReservation> {
        let reservation = self
            .coordinator
            .reserve_attempt(trial_id, identity.clone())
            .map_err(StoreError::Orchestration)?;
        let command = Command::Reserve(ReservePayload {
            trial_id: trial_id.to_string(),
            identity,
        });
        self.persist_or_rebuild(&command)?;
        Ok(reservation)
    }

    /// Records the runner's result (or error) for the active attempt and
    /// returns the coordinator's retry decision.
    pub fn complete_attempt(
        &mut self,
        trial_id: &str,
        request_id: &str,
        result: Result<RunnerResult, RunnerError>,
    ) -> StoreResult<AttemptDecision> {
        let decision = self
            .coordinator
            .complete_attempt(trial_id, request_id, result.clone())
            .map_err(StoreError::Orchestration)?;
        let command = Command::Complete(CompletePayload {
            trial_id: trial_id.to_string(),
            request_id: request_id.to_string(),
            result,
        });
        self.persist_or_rebuild(&command)?;
        Ok(decision)
    }

    /// Cancels a trial that is not in flight.
    pub fn cancel_trial(&mut self, trial_id: &str) -> StoreResult<TrialSnapshot> {
        let snapshot = self
            .coordinator
            .cancel_trial(trial_id)
            .map_err(StoreError::Orchestration)?;
        self.persist_or_rebuild(&Command::Cancel(CancelPayload {
            trial_id: trial_id.to_string(),
        }))?;
        Ok(snapshot)
    }

    /// Extends the lease by `lease_ttl` from now. Fails with `LeaseLost` if
    /// the lease expired or another scheduler took it over.
    pub fn renew_lease(&mut self) -> StoreResult<()> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction()?;
        check_lease(&transaction, &self.evaluation_id, self.fencing_token)?;
        transaction.execute(
            "UPDATE scheduler_leases SET expires_at = ?3
             WHERE evaluation_id = ?1 AND fencing_token = ?2",
            params![
                self.evaluation_id,
                self.fencing_token,
                expiry(self.lease_ttl)?
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Gives up the lease so another scheduler can acquire it immediately.
    pub fn release(self) -> StoreResult<()> {
        let connection = self.store.lock()?;
        connection.execute(
            "DELETE FROM scheduler_leases WHERE evaluation_id = ?1 AND fencing_token = ?2",
            params![self.evaluation_id, self.fencing_token],
        )?;
        Ok(())
    }

    /// Appends `command` under the current lease. If anything fails after the
    /// in-memory coordinator already applied it, the coordinator is rebuilt
    /// from the journal so memory never runs ahead of the durable state.
    fn persist_or_rebuild(&mut self, command: &Command) -> StoreResult<()> {
        match self.append(command) {
            Ok(()) => {
                self.next_seq += 1;
                Ok(())
            }
            Err(error) => {
                // Best effort: if the rebuild itself fails, the original error
                // is still the one the caller must see.
                let _ = self.rebuild();
                Err(error)
            }
        }
    }

    fn append(&self, command: &Command) -> StoreResult<()> {
        let payload = canonical_json(&command.payload()?);
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction()?;
        check_lease(&transaction, &self.evaluation_id, self.fencing_token)?;
        if let Command::Reserve(reserve) = command {
            claim_identities(&transaction, &self.evaluation_id, reserve)?;
        }
        transaction.execute(
            "INSERT INTO scheduler_journal (evaluation_id, seq, command, payload, fencing_token, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                self.evaluation_id,
                self.next_seq,
                command.name(),
                payload,
                self.fencing_token,
                now()
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Replays the journal into a fresh coordinator. Returns the number of
    /// commands replayed.
    fn rebuild(&mut self) -> StoreResult<u64> {
        let schedule = self.store.load_schedule(&self.evaluation_id)?;
        let mut coordinator = TrialCoordinator::new(schedule).map_err(StoreError::Orchestration)?;
        let journal = self.store.load_journal(&self.evaluation_id)?;
        let count = journal.len() as u64;
        for (seq, command) in journal {
            let applied = match command {
                Command::Reserve(p) => coordinator
                    .reserve_attempt(&p.trial_id, p.identity)
                    .map(|_| ()),
                Command::Complete(p) => coordinator
                    .complete_attempt(&p.trial_id, &p.request_id, p.result)
                    .map(|_| ()),
                Command::Cancel(p) => coordinator.cancel_trial(&p.trial_id).map(|_| ()),
            };
            applied.map_err(|error| {
                StoreError::Integrity(format!(
                    "journal for {} does not replay at seq {seq}: {error}",
                    self.evaluation_id
                ))
            })?;
        }
        self.coordinator = coordinator;
        self.next_seq = i64::try_from(count).unwrap_or(i64::MAX - 1) + 1;
        Ok(count)
    }

    /// (trial_id, request_id) of every attempt still reserved.
    fn in_flight_attempts(&self) -> StoreResult<Vec<(String, String)>> {
        let mut in_flight = Vec::new();
        for (_, command) in self.store.load_journal(&self.evaluation_id)? {
            if let Command::Reserve(reserve) = command {
                let snapshot = self
                    .coordinator
                    .snapshot(&reserve.trial_id)
                    .map_err(StoreError::Orchestration)?;
                let still_reserved = snapshot.state == TrialExecutionState::InFlight
                    && snapshot
                        .attempts
                        .last()
                        .is_some_and(|attempt| attempt.request_id == reserve.identity.request_id);
                if still_reserved {
                    in_flight.push((reserve.trial_id, reserve.identity.request_id));
                }
            }
        }
        Ok(in_flight)
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn evaluation_id(&self) -> &str {
        &self.evaluation_id
    }

    pub(crate) fn store(&self) -> &'s SqliteMetadataStore {
        self.store
    }
}

fn expiry(ttl: Duration) -> StoreResult<String> {
    let ttl = chrono::Duration::from_std(ttl)
        .map_err(|_| StoreError::InvalidRecord("lease ttl is too large".into()))?;
    Ok((Utc::now() + ttl).to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn take_lease(
    store: &SqliteMetadataStore,
    evaluation_id: &str,
    owner: &str,
    ttl: Duration,
) -> StoreResult<i64> {
    let expires_at = expiry(ttl)?;
    let mut connection = store.lock()?;
    let transaction = connection.transaction()?;
    let current: Option<(String, String)> = transaction
        .query_row(
            "SELECT owner, expires_at FROM scheduler_leases WHERE evaluation_id = ?1",
            params![evaluation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((holder, holder_expires)) = current {
        if holder != owner && holder_expires > now() {
            return Err(StoreError::LeaseHeld(format!(
                "{evaluation_id} is held by {holder} until {holder_expires}"
            )));
        }
    }
    let last: Option<i64> = transaction
        .query_row(
            "SELECT last_token FROM scheduler_fencing WHERE evaluation_id = ?1",
            params![evaluation_id],
            |row| row.get(0),
        )
        .optional()?;
    let token = last.unwrap_or(0) + 1;
    transaction.execute(
        "INSERT INTO scheduler_fencing (evaluation_id, last_token) VALUES (?1, ?2)
         ON CONFLICT (evaluation_id) DO UPDATE SET last_token = excluded.last_token",
        params![evaluation_id, token],
    )?;
    transaction.execute(
        "INSERT INTO scheduler_leases (evaluation_id, owner, fencing_token, expires_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (evaluation_id) DO UPDATE SET
             owner = excluded.owner,
             fencing_token = excluded.fencing_token,
             expires_at = excluded.expires_at",
        params![evaluation_id, owner, token, expires_at],
    )?;
    transaction.commit()?;
    Ok(token)
}

fn check_lease(transaction: &Transaction<'_>, evaluation_id: &str, token: i64) -> StoreResult<()> {
    let lease: Option<(i64, String)> = transaction
        .query_row(
            "SELECT fencing_token, expires_at FROM scheduler_leases WHERE evaluation_id = ?1",
            params![evaluation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match lease {
        Some((current, _)) if current != token => Err(StoreError::LeaseLost(format!(
            "{evaluation_id} lease was taken over (token {current}, ours {token})"
        ))),
        Some((_, expires_at)) if expires_at <= now() => Err(StoreError::LeaseLost(format!(
            "{evaluation_id} lease expired at {expires_at}"
        ))),
        Some(_) => Ok(()),
        None => Err(StoreError::LeaseLost(format!(
            "{evaluation_id} has no active lease"
        ))),
    }
}

fn claim_identities(
    transaction: &Transaction<'_>,
    evaluation_id: &str,
    reserve: &ReservePayload,
) -> StoreResult<()> {
    let claims = [
        ("run_id", reserve.identity.run_ref.record_id.as_str()),
        ("request_id", reserve.identity.request_id.as_str()),
        ("idempotency_key", reserve.identity.idempotency_key.as_str()),
    ];
    for (kind, value) in claims {
        let taken: Option<String> = transaction
            .query_row(
                "SELECT evaluation_id FROM attempt_identities WHERE identity_kind = ?1 AND identity = ?2",
                params![kind, value],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = taken {
            return Err(StoreError::Conflict(format!(
                "{kind} {value:?} was already used by evaluation {owner}"
            )));
        }
        transaction.execute(
            "INSERT INTO attempt_identities (identity_kind, identity, evaluation_id, trial_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![kind, value, evaluation_id, reserve.trial_id],
        )?;
    }
    Ok(())
}
