use cybercore_evaluation_core::{
    artifact_staging_dir, execute_with_runner, ArtifactManifest, AttemptIdentity,
    EvaluationSchedule, ExecutionBudget, ExecutionDisposition, ExecutionRequest, NetworkPolicy,
    RecordReference, RedactionState, ResourceUsage, RetryPolicy, Runner, RunnerCapabilities,
    RunnerDescriptor, RunnerEvent, RunnerFuture, RunnerResult, RunnerType, SideEffectPolicy,
    TrialDefinition,
};
use cybercore_evaluation_store::{
    run_ready_trials, sha256_hex, Check, ChecksVerifier, CriterionCheck, DurableScheduler,
    LocalStore, RunRecorderConfig,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const LONG: Duration = Duration::from_secs(300);

/// Stages files exactly where the local runner would, then reports them.
struct FileRunner {
    descriptor: RunnerDescriptor,
    capabilities: RunnerCapabilities,
    artifact_root: PathBuf,
    stdout: &'static str,
    files: Vec<(&'static str, &'static [u8])>,
    collected: bool,
}

impl FileRunner {
    fn new(
        artifact_root: &Path,
        stdout: &'static str,
        files: Vec<(&'static str, &'static [u8])>,
    ) -> Self {
        Self {
            descriptor: RunnerDescriptor {
                runner_id: "file-runner".into(),
                runner_type: RunnerType::Local,
                runner_version: None,
            },
            capabilities: RunnerCapabilities {
                supports_network_deny_all: true,
                supports_side_effect_deny_all: true,
                ..RunnerCapabilities::default()
            },
            artifact_root: artifact_root.to_path_buf(),
            stdout,
            files,
            collected: true,
        }
    }
}

impl Runner for FileRunner {
    fn descriptor(&self) -> &RunnerDescriptor {
        &self.descriptor
    }

    fn capabilities(&self) -> &RunnerCapabilities {
        &self.capabilities
    }

    fn execute<'a>(&'a self, request: &'a ExecutionRequest) -> RunnerFuture<'a> {
        Box::pin(async move {
            let mut artifacts = Vec::new();
            let mut total = 0;
            if self.collected {
                let staging = artifact_staging_dir(&self.artifact_root, &request.request_id);
                for (path, bytes) in &self.files {
                    let target = staging.join(path);
                    fs::create_dir_all(target.parent().unwrap()).unwrap();
                    fs::write(&target, bytes).unwrap();
                    total += bytes.len() as u64;
                    artifacts.push(ArtifactManifest {
                        artifact_id: path.to_string(),
                        content_type: "application/octet-stream".into(),
                        byte_length: bytes.len() as u64,
                        sha256_hex: sha256_hex(bytes),
                        redaction_state: RedactionState::NotRequired,
                    });
                }
            }
            Ok(RunnerResult {
                request_id: request.request_id.clone(),
                disposition: ExecutionDisposition::Finished,
                events: vec![
                    RunnerEvent {
                        sequence: 1,
                        occurred_at: "2026-11-18T10:00:00.000Z".into(),
                        actor: "file-runner".into(),
                        event_type: "execution_started".into(),
                        payload: json!({}),
                    },
                    RunnerEvent {
                        sequence: 2,
                        occurred_at: "2026-11-18T10:00:01.000Z".into(),
                        actor: "file-runner".into(),
                        event_type: "execution_finished".into(),
                        payload: json!({}),
                    },
                ],
                artifacts,
                resource_usage: ResourceUsage {
                    wall_time_ms: 1000,
                    artifact_bytes: total,
                    ..ResourceUsage::default()
                },
                output: Some(json!({
                    "stdout": self.stdout,
                    "output_truncated": false,
                    "workspace_file_artifact_export": if self.collected { "collected" } else { "not_requested" },
                    "rejected_output_entries": [{"path": "leak", "reason": "symbolic links are not collected"}],
                })),
                failure: None,
            })
        })
    }
}

fn reference(record_type: &str, record_id: &str) -> RecordReference {
    RecordReference {
        record_id: record_id.into(),
        record_type: record_type.into(),
        schema_version: None,
    }
}

fn schedule() -> EvaluationSchedule {
    EvaluationSchedule {
        evaluation_ref: reference("evaluation", "EVAL-1"),
        plan_ref: reference("evaluation_plan", "PLAN-1"),
        max_active_runs: 1,
        retry_policy: RetryPolicy::default(),
        trials: vec![TrialDefinition {
            trial_ref: reference("trial", "T-1"),
            task_revision_ref: reference("task_revision", "TASK-FILES"),
            system_fingerprint_ref: reference("system_fingerprint", "FP-1"),
            task_input: json!({"prompt": "write the report"}),
            budget: ExecutionBudget {
                max_wall_time_ms: 10_000,
                max_output_bytes: 4096,
                max_artifact_bytes: 4096,
                max_tool_calls: 0,
                max_input_tokens: None,
                max_output_tokens: None,
            },
            network_policy: NetworkPolicy::DenyAll,
            side_effect_policy: SideEffectPolicy::DenyAll,
            capability_grant_refs: vec![],
        }],
    }
}

fn checks() -> ChecksVerifier {
    let check = |id: &str, check| CriterionCheck {
        criterion_id: id.into(),
        check,
    };
    ChecksVerifier::new(BTreeMap::from([(
        "T-1".to_string(),
        vec![
            check(
                "report-written",
                Check::FileEquals {
                    path: "out/report.txt".into(),
                    expected: "total: 3\n".into(),
                },
            ),
            check(
                "report-mentions-total",
                Check::FileContains {
                    path: "out/report.txt".into(),
                    text: "total".into(),
                },
            ),
            check(
                "no-scratch-left",
                Check::FileAbsent {
                    path: "scratch.tmp".into(),
                },
            ),
            check(
                "no-leak",
                Check::FileAbsent {
                    path: "leak".into(),
                },
            ),
            check(
                "says-done",
                Check::StdoutContains {
                    text: "done".into(),
                },
            ),
        ],
    )]))
}

fn identity(trial: &str, attempt: u32) -> AttemptIdentity {
    AttemptIdentity {
        run_ref: reference("run", &format!("RUN-{trial}-A{attempt}")),
        request_id: format!("REQ-{trial}-A{attempt}"),
        idempotency_key: format!("IDEMP-{trial}-A{attempt}"),
    }
}

struct Temp(PathBuf);
impl Temp {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "cybercore-files-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn get(store: &LocalStore, id: &str) -> Value {
    store
        .metadata()
        .get_record(id)
        .unwrap()
        .unwrap_or_else(|| panic!("missing {id}"))
}

async fn drive(store: &LocalStore, runner: &FileRunner, store_files: bool) {
    let config = RunRecorderConfig::for_runner(runner, reference("project", "P-1"), store_files)
        .with_artifact_root(&runner.artifact_root);
    let (mut scheduler, _) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    run_ready_trials(store, &mut scheduler, runner, &config, &checks(), identity)
        .await
        .unwrap();
}

#[tokio::test]
async fn file_and_stdout_criteria_are_judged_and_cited_per_verdict() {
    let dir = Temp::new("judged");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let runner = FileRunner::new(
        &dir.0.join("artifacts"),
        "done\n",
        vec![("out/report.txt", b"total: 3\n")],
    );
    drive(&store, &runner, true).await;

    let verdict = |c: &str| get(&store, &format!("RUN-T-1-A1:VERIFY:{c}"));
    assert_eq!(verdict("report-written")["result"], "pass");
    assert_eq!(verdict("report-mentions-total")["result"], "pass");
    assert_eq!(verdict("no-scratch-left")["result"], "pass");
    // The runner reported a rejected symlink at "leak": it is not absent.
    assert_eq!(verdict("no-leak")["result"], "fail");
    assert!(verdict("no-leak")["judge_rationale"]
        .as_str()
        .unwrap()
        .contains("symbolic links"));
    assert_eq!(verdict("says-done")["result"], "pass");

    // File verdicts cite the file's evidence; the stdout verdict cites stdout.
    assert_eq!(
        verdict("report-written")["evidence_refs"][0]["evidence_id"],
        "RUN-T-1-A1:FILE:000"
    );
    assert_eq!(
        verdict("says-done")["evidence_refs"][0]["evidence_id"],
        "RUN-T-1-A1:OUTPUT"
    );
    assert_eq!(
        store.read_evidence("RUN-T-1-A1:FILE:000").unwrap(),
        b"total: 3\n"
    );
    let file = get(&store, "RUN-T-1-A1:FILE:000");
    assert!(file["description"]
        .as_str()
        .unwrap()
        .contains("out/report.txt"));

    let run = get(&store, "RUN-T-1-A1");
    assert_eq!(run["verification_state"], "mixed");
    // The staged copy is removed once recorded.
    assert!(!artifact_staging_dir(&runner.artifact_root, "REQ-T-1-A1").exists());
}

#[tokio::test]
async fn a_wrong_file_fails_its_criterion() {
    let dir = Temp::new("wrong");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let runner = FileRunner::new(
        &dir.0.join("artifacts"),
        "done\n",
        vec![("out/report.txt", b"total: 4\n")],
    );
    drive(&store, &runner, false).await;
    let verdict = get(&store, "RUN-T-1-A1:VERIFY:report-written");
    assert_eq!(verdict["result"], "fail");
    // Without stored file evidence, the verdict cites the terminal event.
    assert_eq!(verdict["evidence_refs"][0]["evidence_type"], "run_event");
    assert_eq!(
        store
            .metadata()
            .count_records(Some("evidence_artifact"))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn uncollected_outputs_make_file_criteria_unavailable() {
    let dir = Temp::new("uncollected");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let mut runner = FileRunner::new(&dir.0.join("artifacts"), "done\n", vec![]);
    runner.collected = false;
    drive(&store, &runner, true).await;
    let verdict = get(&store, "RUN-T-1-A1:VERIFY:report-written");
    assert_eq!(verdict["result"], "unavailable");
    assert!(verdict["limitations"][0]
        .as_str()
        .unwrap()
        .contains("not_requested"));
    assert_eq!(get(&store, "RUN-T-1-A1:VERIFY:says-done")["result"], "pass");
}

#[tokio::test]
async fn tampered_staged_files_are_not_trusted() {
    let dir = Temp::new("tampered");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let runner = FileRunner::new(
        &dir.0.join("artifacts"),
        "done\n",
        vec![("out/report.txt", b"total: 3\n")],
    );
    {
        // Execute and journal, but crash before recording; then tamper.
        let (mut scheduler, _) =
            DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
        let reservation = scheduler
            .reserve_attempt("T-1", identity("T-1", 1))
            .unwrap();
        let result = execute_with_runner(&runner, &reservation.request).await;
        scheduler
            .complete_attempt("T-1", "REQ-T-1-A1", result)
            .unwrap();
    }
    let staged = artifact_staging_dir(&runner.artifact_root, "REQ-T-1-A1").join("out/report.txt");
    fs::write(&staged, b"total: 999\n").unwrap();

    drive(&store, &runner, true).await; // catch-up records the run
    let verdict = get(&store, "RUN-T-1-A1:VERIFY:report-written");
    assert_eq!(verdict["result"], "unavailable");
    assert!(verdict["limitations"][0]
        .as_str()
        .unwrap()
        .contains("does not match its digest"));
    assert_eq!(
        store
            .metadata()
            .count_records(Some("evidence_artifact"))
            .unwrap(),
        1,
        "only stdout stored"
    );
}

#[tokio::test]
async fn staged_files_survive_a_crash_and_are_recorded_by_catch_up() {
    let dir = Temp::new("catch-up");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let runner = FileRunner::new(
        &dir.0.join("artifacts"),
        "done\n",
        vec![("out/report.txt", b"total: 3\n")],
    );
    {
        let (mut scheduler, _) =
            DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
        let reservation = scheduler
            .reserve_attempt("T-1", identity("T-1", 1))
            .unwrap();
        let result = execute_with_runner(&runner, &reservation.request).await;
        scheduler
            .complete_attempt("T-1", "REQ-T-1-A1", result)
            .unwrap();
    }
    drive(&store, &runner, true).await;
    assert_eq!(
        get(&store, "RUN-T-1-A1:VERIFY:report-written")["result"],
        "pass"
    );
    assert_eq!(
        store.read_evidence("RUN-T-1-A1:FILE:000").unwrap(),
        b"total: 3\n"
    );
}

#[tokio::test]
async fn task_revision_criticality_applies_to_file_criteria() {
    let dir = Temp::new("criticality");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    let mut task: Value = serde_json::from_str(
        &fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../contracts/fixtures/valid/task-revision.json"),
        )
        .unwrap(),
    )
    .unwrap();
    task["record_id"] = json!("TASK-FILES");
    let criterion = |id: &str, c: &str| json!({"criterion_id": id, "description": "d", "verification_requirement": "v", "criticality": c});
    task["acceptance_criteria"] = json!([
        criterion("report-written", "mandatory"),
        criterion("report-mentions-total", "major"),
        criterion("no-scratch-left", "minor"),
        criterion("no-leak", "minor"),
        criterion("says-done", "minor"),
    ]);
    task["verification_plan"]["required_criterion_ids"] =
        json!(["report-written", "report-mentions-total"]);
    store.metadata().put_record(&task).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let runner = FileRunner::new(
        &dir.0.join("artifacts"),
        "done\n",
        vec![("out/report.txt", b"total: 3\n")],
    );
    drive(&store, &runner, false).await;
    let run = get(&store, "RUN-T-1-A1");
    // The minor "no-leak" failure is visible but does not fail the run.
    assert_eq!(run["verification_state"], "mixed");
    assert_eq!(run["run_outcome"], "pass");
}

/// Stands in for a model judge: one fixed verdict for one trial.
struct FakeJudge {
    trial: &'static str,
    criterion: &'static str,
    result: cybercore_evaluation_store::VerdictResult,
}

impl cybercore_evaluation_store::Verifier for FakeJudge {
    fn identity(&self) -> cybercore_evaluation_store::VerifierIdentity {
        cybercore_evaluation_store::VerifierIdentity {
            verifier_id: "model-judge:fake".into(),
            version: "0".into(),
            verifier_type: cybercore_evaluation_store::VerifierType::ModelJudge,
        }
    }

    fn covers(&self, trial_id: &str) -> bool {
        trial_id == self.trial
    }

    fn verify(
        &self,
        _input: &cybercore_evaluation_store::VerificationInput<'_>,
    ) -> Vec<cybercore_evaluation_store::CriterionVerdict> {
        vec![cybercore_evaluation_store::CriterionVerdict {
            criterion_id: self.criterion.into(),
            inspected_target: "workspace-output:out/report.txt".into(),
            method: "fake judgment".into(),
            result: self.result,
            limitations: vec!["Model judgment (fake).".into()],
            judge_rationale: Some("fake rationale".into()),
            evidence_paths: vec!["out/report.txt".into()],
        }]
    }
}

async fn drive_with(
    store: &LocalStore,
    runner: &FileRunner,
    verifier: &dyn cybercore_evaluation_store::Verifier,
) {
    let config = RunRecorderConfig::for_runner(runner, reference("project", "P-1"), true)
        .with_artifact_root(&runner.artifact_root);
    let (mut scheduler, _) =
        DurableScheduler::acquire(store.metadata(), "EVAL-1", "w", LONG).unwrap();
    run_ready_trials(store, &mut scheduler, runner, &config, verifier, identity)
        .await
        .unwrap();
}

#[tokio::test]
async fn checks_and_a_judge_combine_with_their_own_identities() {
    use cybercore_evaluation_store::{CompositeVerifier, VerdictResult};
    let dir = Temp::new("composite");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let runner = FileRunner::new(
        &dir.0.join("artifacts"),
        "done\n",
        vec![("out/report.txt", b"total: 3\n")],
    );
    let composite = CompositeVerifier::new(vec![
        Box::new(checks()),
        Box::new(FakeJudge {
            trial: "T-1",
            criterion: "report-is-clear",
            result: VerdictResult::Pass,
        }),
        // Configured for another trial: must add nothing to T-1.
        Box::new(FakeJudge {
            trial: "T-OTHER",
            criterion: "unrelated",
            result: VerdictResult::Fail,
        }),
    ]);
    drive_with(&store, &runner, &composite).await;

    let exact = get(&store, "RUN-T-1-A1:VERIFY:report-written");
    assert_eq!(exact["result"], "pass");
    assert_eq!(exact["verifier"]["verifier_type"], "deterministic");
    assert_eq!(exact["verifier"]["verifier_id"], "workbench-checks");
    let judged = get(&store, "RUN-T-1-A1:VERIFY:report-is-clear");
    assert_eq!(judged["result"], "pass");
    assert_eq!(judged["verifier"]["verifier_type"], "model_judge");
    assert_eq!(judged["judge_rationale"], "fake rationale");
    assert_eq!(
        judged["evidence_refs"][0]["evidence_id"],
        "RUN-T-1-A1:FILE:000"
    );
    assert!(store
        .metadata()
        .get_record("RUN-T-1-A1:VERIFY:unrelated")
        .unwrap()
        .is_none());
    assert!(store
        .metadata()
        .get_record("RUN-T-1-A1:VERIFY:no-verifier-configured")
        .unwrap()
        .is_none());
    // 5 checks + 1 judgment.
    let run = get(&store, "RUN-T-1-A1");
    assert_eq!(run["verification_refs"].as_array().unwrap().len(), 6);
}

#[tokio::test]
async fn a_criterion_judged_twice_or_not_at_all_is_never_a_pass() {
    use cybercore_evaluation_store::{CompositeVerifier, VerdictResult};
    let dir = Temp::new("composite-conflict");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let runner = FileRunner::new(
        &dir.0.join("artifacts"),
        "done\n",
        vec![("out/report.txt", b"total: 3\n")],
    );
    let composite = CompositeVerifier::new(vec![
        Box::new(checks()),
        // Same criterion ID as a check: the two verdicts collide.
        Box::new(FakeJudge {
            trial: "T-1",
            criterion: "report-written",
            result: VerdictResult::Pass,
        }),
    ]);
    drive_with(&store, &runner, &composite).await;
    let conflict = get(&store, "RUN-T-1-A1:VERIFY:report-written");
    assert_eq!(conflict["result"], "unavailable");
    assert_eq!(conflict["verifier"]["verifier_type"], "hybrid");
    assert!(conflict["limitations"][0]
        .as_str()
        .unwrap()
        .contains("more than one verifier"));

    let dir = Temp::new("composite-uncovered");
    let store = LocalStore::open(dir.0.join("store")).unwrap();
    store.metadata().create_schedule(&schedule()).unwrap();
    let runner = FileRunner::new(&dir.0.join("artifacts"), "done\n", vec![]);
    let nobody = CompositeVerifier::new(vec![Box::new(FakeJudge {
        trial: "T-OTHER",
        criterion: "x",
        result: VerdictResult::Pass,
    })]);
    drive_with(&store, &runner, &nobody).await;
    assert_eq!(
        get(&store, "RUN-T-1-A1:VERIFY:no-verifier-configured")["result"],
        "unavailable"
    );
    assert_eq!(get(&store, "RUN-T-1-A1")["run_outcome"], "not_scored");
}
