use cybercore_evaluation_core::{
    ArtifactManifest, AttemptIdentity, EvaluationSchedule, ExecutionBudget, ExecutionDisposition,
    ExecutionRequest, NetworkPolicy, RecordReference, ResourceUsage, RetryPolicy, Runner,
    RunnerCapabilities, RunnerDescriptor, RunnerError, RunnerErrorKind, RunnerEvent, RunnerFuture,
    RunnerResult, RunnerType, SideEffectPolicy, TrialDefinition,
};
use cybercore_evaluation_store::{
    run_ready_trials, DurableScheduler, LocalStore, RunRecorderConfig, StdoutEqualsVerifier,
    INTERRUPTED_ATTEMPT_MESSAGE,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const LONG: Duration = Duration::from_secs(300);

#[derive(Clone)]
enum Script {
    Stdout(&'static str),
    Unavailable,
}

/// Returns scripted results per trial, in order.
struct ScriptedRunner {
    descriptor: RunnerDescriptor,
    capabilities: RunnerCapabilities,
    script: Mutex<BTreeMap<String, VecDeque<Script>>>,
    live: AtomicUsize,
    peak: AtomicUsize,
}

impl ScriptedRunner {
    fn new(script: &[(&str, &[Script])]) -> Self {
        Self {
            descriptor: RunnerDescriptor {
                runner_id: "scripted-runner".into(),
                runner_type: RunnerType::Local,
                runner_version: Some("0.0.1".into()),
            },
            capabilities: RunnerCapabilities {
                supports_network_deny_all: true,
                supports_network_allowlist: false,
                supports_side_effect_deny_all: true,
                supports_side_effect_allowlist: false,
                max_wall_time_ms: Some(60_000),
                max_output_bytes: Some(1 << 20),
                max_artifact_bytes: Some(1 << 20),
                max_tool_calls: Some(100),
            },
            script: Mutex::new(
                script
                    .iter()
                    .map(|(trial, steps)| (trial.to_string(), steps.iter().cloned().collect()))
                    .collect(),
            ),
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }
}

impl Runner for ScriptedRunner {
    fn descriptor(&self) -> &RunnerDescriptor {
        &self.descriptor
    }

    fn capabilities(&self) -> &RunnerCapabilities {
        &self.capabilities
    }

    fn execute<'a>(&'a self, request: &'a ExecutionRequest) -> RunnerFuture<'a> {
        let step = self
            .script
            .lock()
            .unwrap()
            .get_mut(&request.trial_ref.record_id)
            .and_then(VecDeque::pop_front)
            .expect("no scripted step left for trial");
        let request_id = request.request_id.clone();
        Box::pin(async move {
            // Stay in flight across a few polls so overlapping attempts are
            // observable, and record the most that ran at once.
            let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            for _ in 0..3 {
                tokio::task::yield_now().await;
            }
            self.live.fetch_sub(1, Ordering::SeqCst);
            match step {
                Script::Unavailable => Err(RunnerError {
                    kind: RunnerErrorKind::Unavailable,
                    retryable: true,
                    message: "sandbox host unreachable at /internal/path".into(),
                }),
                Script::Stdout(stdout) => Ok(RunnerResult {
                    request_id,
                    disposition: ExecutionDisposition::Finished,
                    events: vec![
                        event(1, "2026-11-18T10:00:00.000Z", "execution_started"),
                        event(2, "2026-11-18T10:00:00.250Z", "execution_finished"),
                    ],
                    artifacts: Vec::<ArtifactManifest>::new(),
                    resource_usage: ResourceUsage {
                        wall_time_ms: 250,
                        ..ResourceUsage::default()
                    },
                    output: Some(json!({"stdout": stdout, "output_truncated": false})),
                    failure: None,
                }),
            }
        })
    }
}

fn event(sequence: u64, at: &str, kind: &str) -> RunnerEvent {
    RunnerEvent {
        sequence,
        occurred_at: at.into(),
        actor: "scripted-runner".into(),
        event_type: kind.into(),
        payload: json!({"note": "not persisted"}),
    }
}

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
        task_input: json!({"prompt": "public fixture"}),
        budget: ExecutionBudget {
            max_wall_time_ms: 10_000,
            max_output_bytes: 4_096,
            max_artifact_bytes: 0,
            max_tool_calls: 0,
            max_input_tokens: None,
            max_output_tokens: None,
        },
        network_policy: NetworkPolicy::DenyAll,
        side_effect_policy: SideEffectPolicy::DenyAll,
        capability_grant_refs: vec![],
    }
}

fn schedule(trials: &[&str]) -> EvaluationSchedule {
    EvaluationSchedule {
        evaluation_ref: reference("evaluation", "EVAL-1"),
        plan_ref: reference("evaluation_plan", "PLAN-1"),
        max_active_runs: 1,
        retry_policy: RetryPolicy {
            max_attempts_per_trial: 2,
            retry_retryable_runner_errors: true,
            ..RetryPolicy::default()
        },
        trials: trials.iter().map(|t| trial(t)).collect(),
    }
}

fn identity(trial_id: &str, attempt: u32) -> AttemptIdentity {
    AttemptIdentity {
        run_ref: reference("run", &format!("RUN-{trial_id}-A{attempt}")),
        request_id: format!("REQ-{trial_id}-A{attempt}"),
        idempotency_key: format!("IDEMP-{trial_id}-A{attempt}"),
    }
}

fn verifier() -> StdoutEqualsVerifier {
    StdoutEqualsVerifier::new(BTreeMap::from([
        ("T-PASS".to_string(), "hello\n".to_string()),
        ("T-FAIL".to_string(), "hello\n".to_string()),
        ("T-RETRY".to_string(), "hello\n".to_string()),
    ]))
}

fn config(runner: &ScriptedRunner, store_output: bool) -> RunRecorderConfig {
    RunRecorderConfig::for_runner(runner, reference("project", "PROJECT-1"), store_output)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cybercore-records-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record(store: &LocalStore, id: &str) -> Value {
    store
        .metadata()
        .get_record(id)
        .unwrap()
        .unwrap_or_else(|| panic!("missing record {id}"))
}

#[tokio::test]
async fn finished_trials_become_run_event_and_verification_records() {
    let dir = TempDir::new("full");
    let store = LocalStore::open(&dir.0).unwrap();
    store
        .metadata()
        .create_schedule(&schedule(&["T-PASS", "T-FAIL", "T-RETRY"]))
        .unwrap();
    let runner = ScriptedRunner::new(&[
        ("T-PASS", &[Script::Stdout("hello\n")]),
        ("T-FAIL", &[Script::Stdout("goodbye\n")]),
        ("T-RETRY", &[Script::Unavailable, Script::Stdout("hello\n")]),
    ]);
    let (mut scheduler, _) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    let report = run_ready_trials(
        &store,
        &mut scheduler,
        &runner,
        &config(&runner, true),
        &verifier(),
        identity,
    )
    .await
    .unwrap();
    assert_eq!(report.executed.len(), 4);
    assert!(report.caught_up.written.is_empty());

    let pass = record(&store, "RUN-T-PASS-A1");
    assert_eq!(pass["execution_status"], "finished");
    assert_eq!(pass["verification_state"], "verified_pass");
    assert_eq!(pass["run_outcome"], "pass");
    assert_eq!(pass["attempt_kind"], "initial");
    assert_eq!(pass["started_at"], "2026-11-18T10:00:00.000Z");
    assert_eq!(pass["ended_at"], "2026-11-18T10:00:00.250Z");
    assert_eq!(pass["runner"]["runner_id"], "scripted-runner");
    // Only measured usage; placeholder token/tool-call zeros are not recorded.
    assert_eq!(pass["resource_usage"], json!({"wall_time_ms": 250}));
    assert_eq!(pass["event_refs"].as_array().unwrap().len(), 2);
    assert_eq!(pass["verification_refs"].as_array().unwrap().len(), 1);
    assert_eq!(
        pass["evidence_refs"][0]["evidence_id"],
        "RUN-T-PASS-A1:OUTPUT"
    );

    let verification = record(&store, "RUN-T-PASS-A1:VERIFY:stdout-equals-expected");
    assert_eq!(verification["result"], "pass");
    assert_eq!(verification["verifier"]["verifier_type"], "deterministic");
    assert_eq!(
        verification["evidence_refs"][0]["evidence_id"],
        "RUN-T-PASS-A1:OUTPUT"
    );
    let output = store.read_evidence("RUN-T-PASS-A1:OUTPUT").unwrap();
    assert!(String::from_utf8(output).unwrap().contains("hello"));

    let fail = record(&store, "RUN-T-FAIL-A1");
    assert_eq!(fail["execution_status"], "finished");
    assert_eq!(fail["verification_state"], "verified_fail");
    assert_eq!(fail["run_outcome"], "fail");

    // An infrastructure error is never a task failure.
    let errored = record(&store, "RUN-T-RETRY-A1");
    assert_eq!(errored["execution_status"], "errored");
    assert_eq!(errored["verification_state"], "not_started");
    assert_eq!(errored["run_outcome"], "not_scored");
    assert_eq!(errored["error_summary"], "runner error (unavailable)");
    assert!(
        !errored.to_string().contains("/internal/path"),
        "raw runner messages are not persisted"
    );
    assert!(errored.get("started_at").is_none());

    let retried = record(&store, "RUN-T-RETRY-A2");
    assert_eq!(retried["attempt_kind"], "infrastructure_retry");
    assert_eq!(retried["attempt_number"], 2);
    assert!(retried["attempt_reason"]
        .as_str()
        .unwrap()
        .contains("runner error"));
    assert_eq!(retried["run_outcome"], "pass");

    let meta = store.metadata();
    assert_eq!(meta.count_records(Some("run")).unwrap(), 4);
    assert_eq!(meta.count_records(Some("run_event")).unwrap(), 6);
    assert_eq!(meta.count_records(Some("verification_result")).unwrap(), 3);
    assert_eq!(meta.count_records(Some("evidence_artifact")).unwrap(), 3);
    // Everything points back at its run, so a run's records are one lookup away.
    let linked = meta.references_to("RUN-T-PASS-A1").unwrap();
    assert_eq!(linked.len(), 4); // 2 events, 1 verification, 1 evidence
                                 // And every trial links to its runs (via trial_ref and provenance, so
                                 // count distinct runs).
    let runs_of_trial: std::collections::BTreeSet<String> = meta
        .references_to("T-RETRY")
        .unwrap()
        .into_iter()
        .filter(|r| r.source_type == "run")
        .map(|r| r.source_id)
        .collect();
    assert_eq!(
        runs_of_trial.into_iter().collect::<Vec<_>>(),
        vec!["RUN-T-RETRY-A1".to_string(), "RUN-T-RETRY-A2".to_string()]
    );
}

#[tokio::test]
async fn without_output_evidence_verification_cites_the_terminal_event() {
    let dir = TempDir::new("no-evidence");
    let store = LocalStore::open(&dir.0).unwrap();
    store
        .metadata()
        .create_schedule(&schedule(&["T-PASS"]))
        .unwrap();
    let runner = ScriptedRunner::new(&[("T-PASS", &[Script::Stdout("hello\n")])]);
    let (mut scheduler, _) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    run_ready_trials(
        &store,
        &mut scheduler,
        &runner,
        &config(&runner, false),
        &verifier(),
        identity,
    )
    .await
    .unwrap();

    let verification = record(&store, "RUN-T-PASS-A1:VERIFY:stdout-equals-expected");
    assert_eq!(
        verification["evidence_refs"][0]["evidence_type"],
        "run_event"
    );
    assert_eq!(
        verification["evidence_refs"][0]["evidence_id"],
        "RUN-T-PASS-A1:EVT:0002"
    );
    assert_eq!(
        store
            .metadata()
            .count_records(Some("evidence_artifact"))
            .unwrap(),
        0
    );
    assert_eq!(record(&store, "RUN-T-PASS-A1")["evidence_refs"], json!([]));
}

#[tokio::test]
async fn trials_without_criteria_are_unavailable_not_passed() {
    let dir = TempDir::new("no-criteria");
    let store = LocalStore::open(&dir.0).unwrap();
    store
        .metadata()
        .create_schedule(&schedule(&["T-UNKNOWN"]))
        .unwrap();
    let runner = ScriptedRunner::new(&[("T-UNKNOWN", &[Script::Stdout("anything\n")])]);
    let (mut scheduler, _) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    run_ready_trials(
        &store,
        &mut scheduler,
        &runner,
        &config(&runner, true),
        &verifier(),
        identity,
    )
    .await
    .unwrap();
    let run = record(&store, "RUN-T-UNKNOWN-A1");
    assert_eq!(run["verification_state"], "unavailable");
    assert_eq!(run["run_outcome"], "not_scored");
    let verification = record(&store, "RUN-T-UNKNOWN-A1:VERIFY:stdout-equals-expected");
    assert_eq!(verification["result"], "unavailable");
    assert!(!verification["limitations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn records_lost_to_a_crash_are_rebuilt_from_the_journal() {
    let dir = TempDir::new("catch-up");
    let store = LocalStore::open(&dir.0).unwrap();
    store
        .metadata()
        .create_schedule(&schedule(&["T-PASS", "T-FAIL"]))
        .unwrap();
    let runner = ScriptedRunner::new(&[
        ("T-PASS", &[Script::Stdout("hello\n")]),
        ("T-FAIL", &[Script::Stdout("goodbye\n")]),
    ]);
    {
        // Completed and journaled, but the process died before recording.
        let (mut scheduler, _) =
            DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
        let reservation = scheduler
            .reserve_attempt("T-PASS", identity("T-PASS", 1))
            .unwrap();
        let result =
            cybercore_evaluation_core::execute_with_runner(&runner, &reservation.request).await;
        scheduler
            .complete_attempt("T-PASS", "REQ-T-PASS-A1", result)
            .unwrap();
    }
    assert!(store
        .metadata()
        .get_record("RUN-T-PASS-A1")
        .unwrap()
        .is_none());

    let (mut scheduler, _) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    let report = run_ready_trials(
        &store,
        &mut scheduler,
        &runner,
        &config(&runner, true),
        &verifier(),
        identity,
    )
    .await
    .unwrap();
    assert_eq!(report.caught_up.written, vec!["RUN-T-PASS-A1".to_string()]);
    assert_eq!(report.executed, vec!["RUN-T-FAIL-A1".to_string()]);
    let rebuilt = record(&store, "RUN-T-PASS-A1");
    assert_eq!(rebuilt["run_outcome"], "pass");
    assert_eq!(rebuilt["started_at"], "2026-11-18T10:00:00.000Z");

    // A further pass finds nothing missing.
    let again = store
        .record_completed_runs(&scheduler, &config(&runner, true), &verifier())
        .unwrap();
    assert!(again.written.is_empty());
    assert_eq!(again.already_recorded, 2);
}

#[tokio::test]
async fn interrupted_attempts_are_recorded_as_errored_and_retried() {
    let dir = TempDir::new("interrupted");
    let store = LocalStore::open(&dir.0).unwrap();
    store
        .metadata()
        .create_schedule(&schedule(&["T-PASS"]))
        .unwrap();
    {
        let (mut scheduler, _) =
            DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
        scheduler
            .reserve_attempt("T-PASS", identity("T-PASS", 1))
            .unwrap();
        // Killed mid-run.
    }
    let runner = ScriptedRunner::new(&[("T-PASS", &[Script::Stdout("hello\n")])]);
    let (mut scheduler, recovery) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    assert_eq!(recovery.interrupted_attempts.len(), 1);
    let report = run_ready_trials(
        &store,
        &mut scheduler,
        &runner,
        &config(&runner, true),
        &verifier(),
        identity,
    )
    .await
    .unwrap();
    assert_eq!(report.caught_up.written, vec!["RUN-T-PASS-A1".to_string()]);
    assert_eq!(report.executed, vec!["RUN-T-PASS-A2".to_string()]);

    let interrupted = record(&store, "RUN-T-PASS-A1");
    assert_eq!(interrupted["execution_status"], "errored");
    assert_eq!(interrupted["run_outcome"], "not_scored");
    assert!(interrupted["error_summary"]
        .as_str()
        .unwrap()
        .contains(INTERRUPTED_ATTEMPT_MESSAGE));
    assert_eq!(record(&store, "RUN-T-PASS-A2")["run_outcome"], "pass");
}

fn schedule_with(trials: &[&str], max_active_runs: usize) -> EvaluationSchedule {
    EvaluationSchedule {
        max_active_runs,
        ..schedule(trials)
    }
}

async fn run(
    store: &LocalStore,
    runner: &ScriptedRunner,
) -> cybercore_evaluation_store::DriveReport {
    let (mut scheduler, _) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    run_ready_trials(
        store,
        &mut scheduler,
        runner,
        &config(runner, false),
        &verifier(),
        identity,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn attempts_run_concurrently_up_to_max_active_runs() {
    for (limit, expected_peak) in [(1, 1), (2, 2), (8, 4)] {
        let dir = TempDir::new(&format!("concurrency-{limit}"));
        let store = LocalStore::open(&dir.0).unwrap();
        store
            .metadata()
            .create_schedule(&schedule_with(
                &["T-PASS", "T-FAIL", "T-RETRY", "T-UNKNOWN"],
                limit,
            ))
            .unwrap();
        let runner = ScriptedRunner::new(&[
            ("T-PASS", &[Script::Stdout("hello\n")]),
            ("T-FAIL", &[Script::Stdout("goodbye\n")]),
            ("T-RETRY", &[Script::Unavailable, Script::Stdout("hello\n")]),
            ("T-UNKNOWN", &[Script::Stdout("x\n")]),
        ]);
        let report = run(&store, &runner).await;
        assert_eq!(report.executed.len(), 5, "limit {limit}");
        assert_eq!(report.peak_concurrency, expected_peak, "limit {limit}");
        assert_eq!(
            runner.peak.load(Ordering::SeqCst),
            expected_peak,
            "limit {limit}"
        );
        // Same records whatever the concurrency, including the retry.
        assert_eq!(record(&store, "RUN-T-PASS-A1")["run_outcome"], "pass");
        assert_eq!(record(&store, "RUN-T-FAIL-A1")["run_outcome"], "fail");
        assert_eq!(
            record(&store, "RUN-T-RETRY-A1")["run_outcome"],
            "not_scored"
        );
        assert_eq!(record(&store, "RUN-T-RETRY-A2")["run_outcome"], "pass");
        assert_eq!(store.metadata().count_records(Some("run")).unwrap(), 5);
    }
}

/// A schema-valid task_revision with the given criteria and validity.
fn task_revision(id: &str, criteria: &[(&str, &str)], validity: &str) -> Value {
    let mut record: Value = serde_json::from_str(
        &fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../contracts/fixtures/valid/task-revision.json"),
        )
        .unwrap(),
    )
    .unwrap();
    record["record_id"] = json!(id);
    record["validity_state"] = json!(validity);
    if validity == "invalid" {
        record["validity_notes"] = json!("Expected outputs were found to be wrong.");
    }
    record["acceptance_criteria"] = json!(criteria
        .iter()
        .map(|(cid, criticality)| json!({
            "criterion_id": cid,
            "description": "Captured stdout matches.",
            "verification_requirement": "Compare stdout exactly.",
            "criticality": criticality
        }))
        .collect::<Vec<_>>());
    record["verification_plan"]["required_criterion_ids"] =
        json!(criteria.iter().map(|(cid, _)| cid).collect::<Vec<_>>());
    record
}

async fn outcomes_under(task: Value) -> (Value, Value, Value) {
    let dir = TempDir::new(&format!(
        "policy-{}",
        task["validity_state"].as_str().unwrap()
    ));
    let dir = TempDir(
        dir.0.join(
            task["acceptance_criteria"][0]["criticality"]
                .as_str()
                .unwrap_or("none"),
        ),
    );
    let store = LocalStore::open(&dir.0).unwrap();
    store.metadata().put_record(&task).unwrap();
    store
        .metadata()
        .create_schedule(&schedule(&["T-PASS", "T-FAIL", "T-UNKNOWN"]))
        .unwrap();
    let runner = ScriptedRunner::new(&[
        ("T-PASS", &[Script::Stdout("hello\n")]),
        ("T-FAIL", &[Script::Stdout("goodbye\n")]),
        ("T-UNKNOWN", &[Script::Stdout("x\n")]),
    ]);
    run(&store, &runner).await;
    (
        record(&store, "RUN-T-PASS-A1"),
        record(&store, "RUN-T-FAIL-A1"),
        record(&store, "RUN-T-UNKNOWN-A1"),
    )
}

#[tokio::test]
async fn outcome_rules_follow_the_stored_task_revision() {
    let criterion = "stdout-equals-expected";

    // Mandatory: a failure fails; an unjudged required criterion is
    // inconclusive (the default without a policy would be not_scored).
    let (pass, fail, unknown) = outcomes_under(task_revision(
        "TASK-1",
        &[(criterion, "mandatory")],
        "valid",
    ))
    .await;
    assert_eq!(pass["run_outcome"], "pass");
    assert_eq!(fail["run_outcome"], "fail");
    assert_eq!(unknown["verification_state"], "unavailable");
    assert_eq!(unknown["run_outcome"], "inconclusive");

    // Minor: a failed minor criterion does not fail the run, but the
    // verification state still shows the failure.
    let (_, fail, _) =
        outcomes_under(task_revision("TASK-1", &[(criterion, "minor")], "valid")).await;
    assert_eq!(fail["verification_state"], "verified_fail");
    assert_eq!(fail["run_outcome"], "pass");

    // An invalid task revision makes every outcome invalid.
    let (pass, fail, unknown) = outcomes_under(task_revision(
        "TASK-1",
        &[(criterion, "mandatory")],
        "invalid",
    ))
    .await;
    for run in [pass, fail, unknown] {
        assert_eq!(run["run_outcome"], "invalid");
    }
}

/// A verifier that waits on I/O: only its async form may be used by the loop.
struct SlowAsyncVerifier;

impl cybercore_evaluation_store::Verifier for SlowAsyncVerifier {
    fn identity(&self) -> cybercore_evaluation_store::VerifierIdentity {
        cybercore_evaluation_store::VerifierIdentity {
            verifier_id: "slow-async".into(),
            version: "0".into(),
            verifier_type: cybercore_evaluation_store::VerifierType::ExternalService,
        }
    }

    fn verify(
        &self,
        _input: &cybercore_evaluation_store::VerificationInput<'_>,
    ) -> Vec<cybercore_evaluation_store::CriterionVerdict> {
        panic!("the scheduler loop must use verify_async");
    }

    fn verify_async<'a>(
        &'a self,
        _input: &'a cybercore_evaluation_store::VerificationInput<'a>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Vec<cybercore_evaluation_store::CriterionVerdict>>
                + 'a,
        >,
    > {
        Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            vec![cybercore_evaluation_store::CriterionVerdict {
                criterion_id: "slow-check".into(),
                inspected_target: "runner-output:stdout".into(),
                method: "wait, then pass".into(),
                result: cybercore_evaluation_store::VerdictResult::Pass,
                limitations: vec![],
                judge_rationale: None,
                evidence_paths: vec![],
            }]
        })
    }
}

#[tokio::test]
async fn slow_verifiers_run_inside_each_attempt_without_blocking_the_others() {
    let dir = TempDir::new("slow-verifier");
    let store = LocalStore::open(&dir.0).unwrap();
    store
        .metadata()
        .create_schedule(&schedule_with(
            &["T-PASS", "T-FAIL", "T-RETRY", "T-UNKNOWN"],
            4,
        ))
        .unwrap();
    let runner = ScriptedRunner::new(&[
        ("T-PASS", &[Script::Stdout("a\n")]),
        ("T-FAIL", &[Script::Stdout("b\n")]),
        ("T-RETRY", &[Script::Stdout("c\n")]),
        ("T-UNKNOWN", &[Script::Stdout("d\n")]),
    ]);
    let (mut scheduler, _) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    let started = std::time::Instant::now();
    let report = run_ready_trials(
        &store,
        &mut scheduler,
        &runner,
        &config(&runner, false),
        &SlowAsyncVerifier,
        identity,
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(report.executed.len(), 4);
    // Four 300 ms verifications overlapped instead of running one after another.
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "took {elapsed:?}"
    );
    assert_eq!(
        record(&store, "RUN-T-PASS-A1:VERIFY:slow-check")["result"],
        "pass"
    );
    assert_eq!(record(&store, "RUN-T-PASS-A1")["run_outcome"], "pass");
}
