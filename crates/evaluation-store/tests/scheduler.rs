use cybercore_evaluation_core::{
    ArtifactManifest, AttemptAction, AttemptIdentity, EvaluationSchedule, ExecutionBudget,
    ExecutionDisposition, NetworkPolicy, RecordReference, ResourceUsage, RetryPolicy, RunnerResult,
    SideEffectPolicy, TrialDefinition, TrialExecutionState,
};
use cybercore_evaluation_store::{
    DurableScheduler, PutOutcome, SqliteMetadataStore, StoreError, INTERRUPTED_ATTEMPT_MESSAGE,
};
use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const LONG: Duration = Duration::from_secs(300);

fn reference(record_type: &str, record_id: &str) -> RecordReference {
    RecordReference {
        record_id: record_id.into(),
        record_type: record_type.into(),
        schema_version: Some("0.1.0".into()),
    }
}

fn trial(id: &str) -> TrialDefinition {
    TrialDefinition {
        trial_ref: reference("trial", id),
        task_revision_ref: reference("task_revision", "TASK-1"),
        system_fingerprint_ref: reference("system_fingerprint", "FP-1"),
        task_input: serde_json::json!({"prompt": "public fixture"}),
        budget: ExecutionBudget {
            max_wall_time_ms: 10_000,
            max_output_bytes: 16_384,
            max_artifact_bytes: 1_048_576,
            max_tool_calls: 5,
            max_input_tokens: None,
            max_output_tokens: None,
        },
        network_policy: NetworkPolicy::DenyAll,
        side_effect_policy: SideEffectPolicy::DenyAll,
        capability_grant_refs: vec![],
    }
}

fn schedule(evaluation_id: &str, policy: RetryPolicy) -> EvaluationSchedule {
    EvaluationSchedule {
        evaluation_ref: reference("evaluation", evaluation_id),
        plan_ref: reference("evaluation_plan", "PLAN-1"),
        max_active_runs: 2,
        retry_policy: policy,
        trials: vec![trial("TRIAL-1"), trial("TRIAL-2")],
    }
}

fn retrying() -> RetryPolicy {
    RetryPolicy {
        max_attempts_per_trial: 2,
        retry_retryable_runner_errors: true,
        ..RetryPolicy::default()
    }
}

fn identity(tag: &str) -> AttemptIdentity {
    AttemptIdentity {
        run_ref: reference("run", &format!("RUN-{tag}")),
        request_id: format!("REQ-{tag}"),
        idempotency_key: format!("IDEMP-{tag}"),
    }
}

fn finished(request_id: &str) -> RunnerResult {
    RunnerResult {
        request_id: request_id.into(),
        disposition: ExecutionDisposition::Finished,
        events: vec![],
        artifacts: Vec::<ArtifactManifest>::new(),
        resource_usage: ResourceUsage::default(),
        output: None,
        failure: None,
    }
}

struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cybercore-scheduler-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> PathBuf {
        self.0.join("meta.sqlite3")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Expires an evaluation's lease by moving `expires_at` into the past. The
/// tests do this instead of sleeping past a short TTL, so they do not depend on
/// how fast the machine opens and fsyncs the database.
fn expire_lease(path: &PathBuf, evaluation_id: &str) {
    let changed = Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE scheduler_leases SET expires_at = '2000-01-01T00:00:00.000Z'
             WHERE evaluation_id = ?1",
            [evaluation_id],
        )
        .unwrap();
    assert_eq!(changed, 1, "no lease to expire for {evaluation_id}");
}

fn journal_len(path: &PathBuf) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM scheduler_journal", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn schedules_are_validated_immutable_and_idempotent() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let eval = schedule("EVAL-1", RetryPolicy::default());
    assert_eq!(store.create_schedule(&eval).unwrap(), PutOutcome::Inserted);
    assert_eq!(
        store.create_schedule(&eval).unwrap(),
        PutOutcome::AlreadyPresent
    );

    let mut changed = eval.clone();
    changed.max_active_runs = 1;
    assert!(matches!(
        store.create_schedule(&changed),
        Err(StoreError::Conflict(_))
    ));

    let mut invalid = schedule("EVAL-2", RetryPolicy::default());
    invalid.trials.clear();
    assert!(matches!(
        store.create_schedule(&invalid),
        Err(StoreError::Orchestration(_))
    ));
}

#[test]
fn state_survives_release_and_reacquire() {
    let db = TempDb::new("reacquire");
    let store = SqliteMetadataStore::open(db.path()).unwrap();
    store
        .create_schedule(&schedule("EVAL-1", RetryPolicy::default()))
        .unwrap();

    let (mut scheduler, report) =
        DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    assert_eq!(report.replayed_commands, 0);
    let reservation = scheduler.reserve_attempt("TRIAL-1", identity("1")).unwrap();
    assert_eq!(reservation.request.request_id, "REQ-1");
    let decision = scheduler
        .complete_attempt("TRIAL-1", "REQ-1", Ok(finished("REQ-1")))
        .unwrap();
    assert_eq!(decision.action, AttemptAction::AwaitingAssessment);
    scheduler.cancel_trial("TRIAL-2").unwrap();
    scheduler.release().unwrap();
    drop(store);

    let store = SqliteMetadataStore::open(db.path()).unwrap();
    let (scheduler, report) =
        DurableScheduler::acquire(&store, "EVAL-1", "worker-b", LONG).unwrap();
    assert_eq!(report.replayed_commands, 3);
    assert!(report.interrupted_attempts.is_empty());
    let coordinator = scheduler.coordinator();
    assert_eq!(
        coordinator.snapshot("TRIAL-1").unwrap().state,
        TrialExecutionState::ExecutionComplete
    );
    assert_eq!(
        coordinator.snapshot("TRIAL-2").unwrap().state,
        TrialExecutionState::Cancelled
    );
    assert!(coordinator.ready_trial_ids().is_empty());
}

#[test]
fn in_flight_attempts_are_closed_and_retried_by_policy_after_a_crash() {
    let db = TempDb::new("crash-retry");
    let store = SqliteMetadataStore::open(db.path()).unwrap();
    store
        .create_schedule(&schedule("EVAL-1", retrying()))
        .unwrap();
    {
        let (mut scheduler, _) =
            DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
        scheduler.reserve_attempt("TRIAL-1", identity("1")).unwrap();
        // Crash: the scheduler disappears without completing or releasing.
    }
    expire_lease(&db.path(), "EVAL-1");

    let (mut scheduler, report) =
        DurableScheduler::acquire(&store, "EVAL-1", "worker-b", LONG).unwrap();
    assert_eq!(report.interrupted_attempts, vec!["REQ-1".to_string()]);
    let snapshot = scheduler.coordinator().snapshot("TRIAL-1").unwrap();
    assert_eq!(snapshot.state, TrialExecutionState::RetryReady);
    assert_eq!(snapshot.attempts.len(), 1);

    // The retry gets a fresh identity and completes normally.
    scheduler.reserve_attempt("TRIAL-1", identity("2")).unwrap();
    scheduler
        .complete_attempt("TRIAL-1", "REQ-2", Ok(finished("REQ-2")))
        .unwrap();
    assert_eq!(
        scheduler.coordinator().snapshot("TRIAL-1").unwrap().state,
        TrialExecutionState::ExecutionComplete
    );

    // The interruption is in the journal with the documented message.
    let payload: String = Connection::open(db.path())
        .unwrap()
        .query_row(
            "SELECT payload FROM scheduler_journal WHERE command = 'complete' ORDER BY seq LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(payload.contains(INTERRUPTED_ATTEMPT_MESSAGE));
}

#[test]
fn interrupted_attempts_are_not_retried_unless_the_policy_allows_it() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    store
        .create_schedule(&schedule("EVAL-1", RetryPolicy::default()))
        .unwrap();
    {
        let (mut scheduler, _) =
            DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
        scheduler.reserve_attempt("TRIAL-1", identity("1")).unwrap();
    }
    // Same owner restarting: it may take its own unexpired lease back.
    let (scheduler, report) =
        DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    assert_eq!(report.interrupted_attempts.len(), 1);
    assert_eq!(
        scheduler.coordinator().snapshot("TRIAL-1").unwrap().state,
        TrialExecutionState::AttemptsExhausted
    );
}

#[test]
fn a_live_lease_blocks_other_schedulers() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    store
        .create_schedule(&schedule("EVAL-1", RetryPolicy::default()))
        .unwrap();
    let (scheduler, _) = DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    assert!(matches!(
        DurableScheduler::acquire(&store, "EVAL-1", "worker-b", LONG),
        Err(StoreError::LeaseHeld(_))
    ));
    scheduler.release().unwrap();
    assert!(DurableScheduler::acquire(&store, "EVAL-1", "worker-b", LONG).is_ok());
}

#[test]
fn a_fenced_out_scheduler_cannot_write_and_its_memory_is_rolled_back() {
    let db = TempDb::new("fencing");
    let store = SqliteMetadataStore::open(db.path()).unwrap();
    store
        .create_schedule(&schedule("EVAL-1", RetryPolicy::default()))
        .unwrap();
    let (mut stale, _) = DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    expire_lease(&db.path(), "EVAL-1");
    let (fresh, _) = DurableScheduler::acquire(&store, "EVAL-1", "worker-b", LONG).unwrap();
    assert!(fresh.fencing_token() > stale.fencing_token());

    assert!(matches!(
        stale.reserve_attempt("TRIAL-1", identity("1")),
        Err(StoreError::LeaseLost(_))
    ));
    assert!(matches!(stale.renew_lease(), Err(StoreError::LeaseLost(_))));
    assert_eq!(journal_len(&db.path()), 0);
    // The stale scheduler's coordinator was rebuilt from the journal, so it
    // does not believe TRIAL-1 is in flight.
    assert_eq!(
        stale.coordinator().snapshot("TRIAL-1").unwrap().state,
        TrialExecutionState::Ready
    );
}

#[test]
fn an_expired_lease_stops_writes_until_reacquired() {
    let db = TempDb::new("expiry");
    let store = SqliteMetadataStore::open(db.path()).unwrap();
    store
        .create_schedule(&schedule("EVAL-1", RetryPolicy::default()))
        .unwrap();
    let (mut scheduler, _) = DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    scheduler.renew_lease().unwrap();
    expire_lease(&db.path(), "EVAL-1");
    assert!(matches!(
        scheduler.reserve_attempt("TRIAL-1", identity("1")),
        Err(StoreError::LeaseLost(_))
    ));
    assert!(matches!(
        scheduler.renew_lease(),
        Err(StoreError::LeaseLost(_))
    ));
    assert_eq!(journal_len(&db.path()), 0);

    // Reacquiring (same owner) restores the ability to write.
    let (mut scheduler, _) = DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    scheduler.reserve_attempt("TRIAL-1", identity("1")).unwrap();
    assert_eq!(journal_len(&db.path()), 1);
}

#[test]
fn attempt_identities_are_unique_across_evaluations() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    store
        .create_schedule(&schedule("EVAL-1", RetryPolicy::default()))
        .unwrap();
    store
        .create_schedule(&schedule("EVAL-2", RetryPolicy::default()))
        .unwrap();
    let (mut first, _) = DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    let (mut second, _) = DurableScheduler::acquire(&store, "EVAL-2", "worker-a", LONG).unwrap();

    first
        .reserve_attempt("TRIAL-1", identity("SHARED"))
        .unwrap();
    assert!(matches!(
        second.reserve_attempt("TRIAL-1", identity("SHARED")),
        Err(StoreError::Conflict(_))
    ));
    // Nothing was journaled for EVAL-2 and its coordinator was rolled back.
    assert_eq!(
        second.coordinator().snapshot("TRIAL-1").unwrap().state,
        TrialExecutionState::Ready
    );
    second
        .reserve_attempt("TRIAL-1", identity("OTHER"))
        .unwrap();
}

#[test]
fn rejected_commands_write_nothing() {
    let db = TempDb::new("rejected");
    let store = SqliteMetadataStore::open(db.path()).unwrap();
    store
        .create_schedule(&schedule("EVAL-1", RetryPolicy::default()))
        .unwrap();
    let (mut scheduler, _) = DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    assert!(matches!(
        scheduler.complete_attempt("TRIAL-1", "REQ-X", Ok(finished("REQ-X"))),
        Err(StoreError::Orchestration(_))
    ));
    assert!(matches!(
        scheduler.reserve_attempt("NO-SUCH-TRIAL", identity("1")),
        Err(StoreError::Orchestration(_))
    ));
    assert_eq!(journal_len(&db.path()), 0);
}

#[test]
fn the_journal_is_append_only_and_gaps_are_detected() {
    let db = TempDb::new("journal");
    let store = SqliteMetadataStore::open(db.path()).unwrap();
    store
        .create_schedule(&schedule("EVAL-1", RetryPolicy::default()))
        .unwrap();
    let (mut scheduler, _) = DurableScheduler::acquire(&store, "EVAL-1", "worker-a", LONG).unwrap();
    scheduler.reserve_attempt("TRIAL-1", identity("1")).unwrap();
    scheduler.release().unwrap();

    let raw = Connection::open(db.path()).unwrap();
    assert!(raw.execute("DELETE FROM scheduler_journal", []).is_err());
    assert!(raw
        .execute("UPDATE scheduler_journal SET payload = '{}'", [])
        .is_err());
    assert!(raw
        .execute("UPDATE evaluation_schedules SET schedule = '{}'", [])
        .is_err());

    raw.execute(
        "INSERT INTO scheduler_journal (evaluation_id, seq, command, payload, fencing_token, recorded_at)
         VALUES ('EVAL-1', 5, 'cancel', '{\"trial_id\":\"TRIAL-2\"}', 1, '2026-01-01T00:00:00.000Z')",
        [],
    )
    .unwrap();
    assert!(matches!(
        DurableScheduler::acquire(&store, "EVAL-1", "worker-b", LONG),
        Err(StoreError::Integrity(_))
    ));
}
