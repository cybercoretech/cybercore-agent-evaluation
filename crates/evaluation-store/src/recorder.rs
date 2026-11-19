//! Run and verification records for resolved attempts (ADR-0011).
//!
//! Each completed attempt becomes ordinary Workbench records, written in one
//! transaction: a `run`, one `run_event` per runner event, and one
//! `verification_result` per criterion an independent [`Verifier`] judged.
//! Pass/fail is never derived from the runner: only verification assigns it.
//!
//! The scheduler journal holds every runner result, so records that a crash
//! prevented from being written are rebuilt by [`LocalStore::record_completed_runs`].

use crate::canonical::canonical_json;
use crate::error::{StoreError, StoreResult};
use crate::local::LocalStore;
use crate::metadata::now;
use crate::scheduler::{Command, DurableScheduler, INTERRUPTED_ATTEMPT_MESSAGE};
use cybercore_evaluation_core::{
    artifact_staging_dir, ArtifactManifest, AttemptIdentity, AttemptStatus, ExecutionDisposition,
    RecordReference, Runner, RunnerError, RunnerErrorKind, RunnerResult, RunnerType,
    TrialDefinition,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: &str = "0.1.0";
const COMPONENT: &str = "cybercore-evaluation-store/run-recorder";

/// Who verified, as recorded on every `verification_result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierIdentity {
    pub verifier_id: String,
    pub version: String,
    pub verifier_type: VerifierType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierType {
    Deterministic,
    ModelJudge,
    Human,
    ExternalService,
    Hybrid,
}

impl VerifierType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::ModelJudge => "model_judge",
            Self::Human => "human",
            Self::ExternalService => "external_service",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictResult {
    Pass,
    Fail,
    Mixed,
    Inconclusive,
    Unavailable,
}

impl VerdictResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Mixed => "mixed",
            Self::Inconclusive => "inconclusive",
            Self::Unavailable => "unavailable",
        }
    }

    fn is_definitive(self) -> bool {
        matches!(self, Self::Pass | Self::Fail | Self::Mixed)
    }
}

/// One criterion's independent judgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionVerdict {
    /// `[A-Za-z0-9][A-Za-z0-9._:-]*`; becomes part of the record ID.
    pub criterion_id: String,
    /// What was inspected, e.g. `runner-output:stdout`.
    pub inspected_target: String,
    /// How it was inspected.
    pub method: String,
    pub result: VerdictResult,
    /// Required (at least one) for `Inconclusive` and `Unavailable`.
    pub limitations: Vec<String>,
    pub judge_rationale: Option<String>,
    /// Workspace output files this verdict inspected (paths relative to
    /// `/workspace/output`). The verification cites their evidence when it is
    /// stored; an empty list cites the runner output.
    pub evidence_paths: Vec<String>,
}

/// What a verifier may inspect for one finished attempt.
pub struct VerificationInput<'a> {
    pub trial: &'a TrialDefinition,
    pub run_ref: &'a RecordReference,
    pub result: &'a RunnerResult,
    /// Collected `/workspace/output` files, by relative path, each verified
    /// against its manifest digest. Empty when outputs were not collected.
    pub files: &'a BTreeMap<String, Vec<u8>>,
}

/// Independent verification port. Implementations hold their own criteria
/// (hidden grader data stays out of `task_input`) and judge each criterion.
/// Returning no verdicts means verification was unavailable.
pub trait Verifier {
    fn identity(&self) -> VerifierIdentity;
    fn verify(&self, input: &VerificationInput<'_>) -> Vec<CriterionVerdict>;
}

/// Deterministic verifier: the runner's captured `stdout` must equal the
/// expected text for the trial. Trials without an expectation are reported
/// as unavailable, never as passing.
pub struct StdoutEqualsVerifier {
    expected: BTreeMap<String, String>,
}

impl StdoutEqualsVerifier {
    /// `expected` maps trial IDs to the exact stdout each must produce.
    pub fn new(expected: BTreeMap<String, String>) -> Self {
        Self { expected }
    }
}

impl Verifier for StdoutEqualsVerifier {
    fn identity(&self) -> VerifierIdentity {
        VerifierIdentity {
            verifier_id: "stdout-equals".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            verifier_type: VerifierType::Deterministic,
        }
    }

    fn verify(&self, input: &VerificationInput<'_>) -> Vec<CriterionVerdict> {
        let trial_id = &input.trial.trial_ref.record_id;
        let base = |result, limitations: Vec<String>, rationale: Option<String>| CriterionVerdict {
            criterion_id: "stdout-equals-expected".into(),
            inspected_target: "runner-output:stdout".into(),
            method: "Compare the captured stdout byte-for-byte with the trial's expected text."
                .into(),
            result,
            limitations,
            judge_rationale: rationale,
            evidence_paths: Vec::new(),
        };
        let Some(expected) = self.expected.get(trial_id) else {
            return vec![base(
                VerdictResult::Unavailable,
                vec![format!("No expected stdout is configured for {trial_id}.")],
                None,
            )];
        };
        let output = input.result.output.as_ref();
        let stdout = output.and_then(|o| o.get("stdout")).and_then(Value::as_str);
        let truncated = output
            .and_then(|o| o.get("output_truncated"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match stdout {
            None => vec![base(
                VerdictResult::Unavailable,
                vec!["The runner result has no captured stdout.".into()],
                None,
            )],
            Some(_) if truncated => vec![base(
                VerdictResult::Inconclusive,
                vec!["Captured output was truncated by the runner's output limit.".into()],
                None,
            )],
            Some(actual) if actual == expected => vec![base(VerdictResult::Pass, vec![], None)],
            Some(actual) => vec![base(
                VerdictResult::Fail,
                vec![],
                Some(format!(
                    "Expected {} bytes, got {} bytes with different content.",
                    expected.len(),
                    actual.len()
                )),
            )],
        }
    }
}

/// How the recorder describes runs and evidence.
#[derive(Debug, Clone)]
pub struct RunRecorderConfig {
    /// Recorded as each run's `runner`. One runner per evaluation.
    pub runner_id: String,
    pub runner_type: RunnerType,
    pub runner_version: Option<String>,
    /// `project_ref` for evidence artifacts.
    pub project_ref: RecordReference,
    /// Store each runner `output` as an `evidence_artifact`. The local runner
    /// does not redact, so enabling this is the caller's attestation that task
    /// outputs contain no sensitive data (recorded as `redaction_state:
    /// not_required`). When disabled, verification cites the run's terminal
    /// `run_event` instead.
    pub store_output_evidence: bool,
    /// The runner's `artifact_root`, where collected workspace files are
    /// staged. `None` when the runner does not collect outputs.
    pub artifact_root: Option<PathBuf>,
}

impl RunRecorderConfig {
    /// Fills runner fields from a runner's descriptor.
    pub fn for_runner<R: Runner + ?Sized>(
        runner: &R,
        project_ref: RecordReference,
        store_output_evidence: bool,
    ) -> Self {
        let descriptor = runner.descriptor();
        Self {
            runner_id: descriptor.runner_id.clone(),
            runner_type: descriptor.runner_type,
            runner_version: descriptor.runner_version.clone(),
            project_ref,
            store_output_evidence,
            artifact_root: None,
        }
    }

    /// Where the runner stages collected workspace files.
    pub fn with_artifact_root(mut self, artifact_root: impl Into<PathBuf>) -> Self {
        self.artifact_root = Some(artifact_root.into());
        self
    }
}

/// What a recording pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecordReport {
    /// Run IDs whose records were written now.
    pub written: Vec<String>,
    /// Completed attempts whose run record already existed.
    pub already_recorded: usize,
}

/// An attempt the driver just completed.
#[derive(Clone, Copy)]
pub(crate) struct CompletedAttempt<'a> {
    pub(crate) trial: &'a TrialDefinition,
    pub(crate) identity: &'a AttemptIdentity,
    pub(crate) result: &'a Result<RunnerResult, RunnerError>,
    pub(crate) reserved_at: &'a str,
}

/// Everything needed to describe one resolved attempt.
struct ResolvedAttempt<'a> {
    evaluation_ref: &'a RecordReference,
    trial: &'a TrialDefinition,
    identity: &'a AttemptIdentity,
    attempt_number: u32,
    previous_status: Option<AttemptStatus>,
    status: AttemptStatus,
    result: &'a Result<RunnerResult, RunnerError>,
    reserved_at: &'a str,
    completed_at: &'a str,
}

impl LocalStore {
    /// Writes records for every completed attempt of the scheduler's
    /// evaluation that has none yet (for example after a crash between the
    /// journal commit and the record write). Safe to call at any time.
    pub fn record_completed_runs(
        &self,
        scheduler: &DurableScheduler<'_>,
        config: &RunRecorderConfig,
        verifier: &dyn Verifier,
    ) -> StoreResult<RecordReport> {
        let evaluation_id = scheduler.evaluation_id();
        let schedule = scheduler.store().load_schedule(evaluation_id)?;
        let trials: BTreeMap<&str, &TrialDefinition> = schedule
            .trials
            .iter()
            .map(|t| (t.trial_ref.record_id.as_str(), t))
            .collect();

        // Cheap pass first: the coordinator knows every resolved attempt, so
        // only read the journal (for runner results) when some lack records.
        let mut report = RecordReport::default();
        let mut missing: BTreeSet<String> = BTreeSet::new();
        for trial in &schedule.trials {
            let snapshot = scheduler
                .coordinator()
                .snapshot(&trial.trial_ref.record_id)
                .map_err(StoreError::Orchestration)?;
            for attempt in snapshot
                .attempts
                .iter()
                .filter(|a| a.status != AttemptStatus::Reserved)
            {
                if self.metadata().record_exists(&attempt.run_ref.record_id)? {
                    report.already_recorded += 1;
                } else {
                    missing.insert(attempt.request_id.clone());
                }
            }
        }
        if missing.is_empty() {
            return Ok(report);
        }

        // request_id -> (identity, reserved_at)
        let mut reservations: BTreeMap<String, (AttemptIdentity, String)> = BTreeMap::new();
        for entry in scheduler.store().load_journal_entries(evaluation_id)? {
            match entry.command {
                Command::Reserve(reserve) => {
                    reservations.insert(
                        reserve.identity.request_id.clone(),
                        (reserve.identity, entry.recorded_at),
                    );
                }
                Command::Complete(complete) => {
                    let (identity, reserved_at) =
                        reservations.get(&complete.request_id).ok_or_else(|| {
                            StoreError::Integrity(format!(
                                "journal completes unknown request {}",
                                complete.request_id
                            ))
                        })?;
                    if !missing.contains(&complete.request_id) {
                        continue;
                    }
                    let trial = trials.get(complete.trial_id.as_str()).ok_or_else(|| {
                        StoreError::Integrity(format!(
                            "journal names unknown trial {}",
                            complete.trial_id
                        ))
                    })?;
                    let snapshot = scheduler
                        .coordinator()
                        .snapshot(&complete.trial_id)
                        .map_err(StoreError::Orchestration)?;
                    let index = snapshot
                        .attempts
                        .iter()
                        .position(|a| a.request_id == complete.request_id)
                        .ok_or_else(|| {
                            StoreError::Integrity(format!(
                                "coordinator has no attempt {}",
                                complete.request_id
                            ))
                        })?;
                    let attempt = &snapshot.attempts[index];
                    self.write_attempt_records(
                        &ResolvedAttempt {
                            evaluation_ref: &schedule.evaluation_ref,
                            trial,
                            identity,
                            attempt_number: attempt.attempt_number,
                            previous_status: index
                                .checked_sub(1)
                                .map(|i| snapshot.attempts[i].status),
                            status: attempt.status,
                            result: &complete.result,
                            reserved_at,
                            completed_at: &entry.recorded_at,
                        },
                        config,
                        verifier,
                    )?;
                    report.written.push(identity.run_ref.record_id.clone());
                }
                Command::Cancel(_) => {}
            }
        }
        Ok(report)
    }

    /// The outcome policy declared by a stored `task_revision`, if any.
    fn outcome_policy(&self, task_revision_id: &str) -> StoreResult<Option<OutcomePolicy>> {
        match self.metadata().get_record(task_revision_id)? {
            Some(record) if record["record_type"] == "task_revision" => {
                OutcomePolicy::from_task_revision(&record).map(Some)
            }
            _ => Ok(None),
        }
    }

    /// Records one attempt the caller just completed, without re-reading the
    /// journal. `reserved_at` is when the caller reserved it.
    pub(crate) fn record_attempt(
        &self,
        scheduler: &DurableScheduler<'_>,
        attempt: &CompletedAttempt<'_>,
        config: &RunRecorderConfig,
        verifier: &dyn Verifier,
    ) -> StoreResult<()> {
        let CompletedAttempt {
            trial,
            identity,
            result,
            reserved_at,
        } = *attempt;
        let snapshot = scheduler
            .coordinator()
            .snapshot(&trial.trial_ref.record_id)
            .map_err(StoreError::Orchestration)?;
        let index = snapshot
            .attempts
            .iter()
            .position(|a| a.request_id == identity.request_id)
            .ok_or_else(|| {
                StoreError::Integrity(format!(
                    "coordinator has no attempt {}",
                    identity.request_id
                ))
            })?;
        let completed_at = now();
        self.write_attempt_records(
            &ResolvedAttempt {
                evaluation_ref: scheduler.coordinator().evaluation_ref(),
                trial,
                identity,
                attempt_number: snapshot.attempts[index].attempt_number,
                previous_status: index.checked_sub(1).map(|i| snapshot.attempts[i].status),
                status: snapshot.attempts[index].status,
                result,
                reserved_at,
                completed_at: &completed_at,
            },
            config,
            verifier,
        )
    }

    fn write_attempt_records(
        &self,
        attempt: &ResolvedAttempt<'_>,
        config: &RunRecorderConfig,
        verifier: &dyn Verifier,
    ) -> StoreResult<()> {
        let run_ref = &attempt.identity.run_ref;
        let run_id = &run_ref.record_id;
        let created_at = attempt.completed_at;
        let provenance = provenance(attempt, run_id);

        // Output evidence (optional, written first through its own protocol).
        let mut evidence_refs = Vec::new();
        if let (true, Ok(result)) = (config.store_output_evidence, attempt.result) {
            if let Some(output) = &result.output {
                let draft = json!({
                    "record_id": format!("{run_id}:OUTPUT"),
                    "record_type": "evidence_artifact",
                    "schema_version": SCHEMA_VERSION,
                    "created_at": created_at,
                    "provenance": provenance,
                    "project_ref": reference_json(&config.project_ref),
                    "source_record_ref": {"record_id": run_id, "record_type": "run"},
                    "artifact_kind": "agent_output",
                    "content_type": "application/json",
                    "redaction_state": "not_required",
                    "access_classification": "project_internal",
                    "retention": {"policy_id": "workbench-default", "legal_hold": false},
                    "description": "Runner output captured for this run (caller-attested non-sensitive)."
                });
                let stored = self.put_evidence(&draft, canonical_json(output).as_bytes())?;
                evidence_refs.push(json!({
                    "evidence_id": format!("{run_id}:OUTPUT"),
                    "evidence_type": "evidence_artifact",
                    "relationship": "source",
                    "sha256_digest": stored["sha256_digest"],
                }));
            }
        }

        // Collected workspace files: verified against their manifests, given to
        // the verifier, and stored as evidence under the same attestation.
        let staging = match (&config.artifact_root, attempt.result) {
            (Some(root), Ok(result)) if !result.artifacts.is_empty() => {
                Some(artifact_staging_dir(root, &attempt.identity.request_id))
            }
            _ => None,
        };
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut file_evidence: BTreeMap<String, Value> = BTreeMap::new();
        if let (Some(staging), Ok(result)) = (&staging, attempt.result) {
            for (index, manifest) in result.artifacts.iter().enumerate() {
                let Some(bytes) = read_staged(staging, manifest) else {
                    continue; // missing or altered: verifiers see it as absent
                };
                if config.store_output_evidence {
                    let evidence_id = format!("{run_id}:FILE:{index:03}");
                    let draft = json!({
                        "record_id": evidence_id,
                        "record_type": "evidence_artifact",
                        "schema_version": SCHEMA_VERSION,
                        "created_at": created_at,
                        "provenance": provenance,
                        "project_ref": reference_json(&config.project_ref),
                        "source_record_ref": {"record_id": run_id, "record_type": "run"},
                        "artifact_kind": "agent_output",
                        "content_type": manifest.content_type,
                        "redaction_state": "not_required",
                        "access_classification": "project_internal",
                        "retention": {"policy_id": "workbench-default", "legal_hold": false},
                        "description": format!("Workspace output file {} (caller-attested non-sensitive).", manifest.artifact_id)
                    });
                    self.put_evidence(&draft, &bytes)?;
                    let reference = json!({
                        "evidence_id": evidence_id,
                        "evidence_type": "evidence_artifact",
                        "relationship": "source",
                        "sha256_digest": manifest.sha256_hex.to_ascii_lowercase(),
                    });
                    evidence_refs.push(reference.clone());
                    file_evidence.insert(manifest.artifact_id.clone(), reference);
                }
                files.insert(manifest.artifact_id.clone(), bytes);
            }
        }
        // The runner-output evidence (stdout) is the first reference, if any.
        let output_evidence: Vec<Value> = evidence_refs
            .first()
            .filter(|r| r["evidence_id"] == json!(format!("{run_id}:OUTPUT")))
            .cloned()
            .into_iter()
            .collect();

        // Runner events.
        let mut records = Vec::new();
        let mut event_refs = Vec::new();
        if let Ok(result) = attempt.result {
            for event in &result.events {
                let event_id = format!("{run_id}:EVT:{:04}", event.sequence);
                records.push(json!({
                    "record_id": event_id,
                    "record_type": "run_event",
                    "schema_version": SCHEMA_VERSION,
                    "created_at": created_at,
                    "provenance": provenance,
                    "run_ref": {"record_id": run_id, "record_type": "run"},
                    "sequence": event.sequence,
                    "occurred_at": event.occurred_at,
                    "event_type": event.event_type,
                    "actor": {"actor_type": "runner", "actor_id": event.actor},
                    "component": config.runner_id,
                    "correlation_id": attempt.identity.request_id,
                    "summary": format!("{} reported {}", event.actor, event.event_type),
                    "redaction": {
                        "state": "not_required",
                        "reason": "The runner event payload is not persisted in this record."
                    }
                }));
                event_refs.push(json!({"record_id": event_id, "record_type": "run_event"}));
            }
        }

        // Independent verification, only for executions that finished.
        let mut verification_refs = Vec::new();
        let mut verdict_results = Vec::new();
        let mut judged: Vec<(String, VerdictResult)> = Vec::new();
        let finished = matches!(
            (attempt.status, attempt.result),
            (AttemptStatus::Finished, Ok(r)) if r.disposition == ExecutionDisposition::Finished
        );
        if let (true, Ok(result)) = (finished, attempt.result) {
            let identity = verifier.identity();
            let verdicts = verifier.verify(&VerificationInput {
                trial: attempt.trial,
                run_ref,
                result,
                files: &files,
            });
            let terminal_event: Vec<Value> = event_refs
                .last()
                .map(|event| {
                    vec![json!({
                        "evidence_id": event["record_id"],
                        "evidence_type": "run_event",
                        "relationship": "source",
                    })]
                })
                .unwrap_or_default();
            let performed_at = now();
            for verdict in verdicts {
                // Cite what the verdict inspected: its files' evidence, else
                // the stdout evidence, else the run's terminal event.
                let mut cited: Vec<Value> = verdict
                    .evidence_paths
                    .iter()
                    .filter_map(|path| file_evidence.get(path).cloned())
                    .collect();
                if cited.is_empty() && verdict.evidence_paths.is_empty() {
                    cited = output_evidence.clone();
                }
                if cited.is_empty() {
                    cited = terminal_event.clone();
                }
                if verdict.result.is_definitive() && cited.is_empty() {
                    return Err(StoreError::InvalidRecord(format!(
                        "{run_id}: verifier returned {} for {} with no inspectable evidence",
                        verdict.result.as_str(),
                        verdict.criterion_id
                    )));
                }
                let verification_id = format!("{run_id}:VERIFY:{}", verdict.criterion_id);
                let mut record = json!({
                    "record_id": verification_id,
                    "record_type": "verification_result",
                    "schema_version": SCHEMA_VERSION,
                    "created_at": created_at,
                    "provenance": provenance,
                    "run_ref": {"record_id": run_id, "record_type": "run"},
                    "criterion_id": verdict.criterion_id,
                    "verifier": {
                        "verifier_id": identity.verifier_id,
                        "version": identity.version,
                        "verifier_type": identity.verifier_type.as_str(),
                    },
                    "inspected_target": verdict.inspected_target,
                    "method": verdict.method,
                    "result": verdict.result.as_str(),
                    "performed_at": performed_at,
                    "evidence_refs": cited,
                });
                insert_if(
                    &mut record,
                    "limitations",
                    !verdict.limitations.is_empty(),
                    json!(verdict.limitations),
                );
                if let Some(rationale) = &verdict.judge_rationale {
                    insert_if(&mut record, "judge_rationale", true, json!(rationale));
                }
                records.push(record);
                verification_refs.push(json!({
                    "record_id": verification_id,
                    "record_type": "verification_result"
                }));
                verdict_results.push(verdict.result);
                judged.push((verdict.criterion_id.clone(), verdict.result));
            }
        }

        // Outcome rules come from the trial's stored task_revision when there
        // is one; otherwise the conservative default aggregation applies.
        let policy = self.outcome_policy(&attempt.trial.task_revision_ref.record_id)?;
        let (verification_state, default_outcome) = if finished {
            aggregate(&verdict_results)
        } else {
            ("not_started", "not_scored")
        };
        let run_outcome = match (&policy, finished) {
            (Some(policy), _) if policy.task_invalid => "invalid",
            (Some(policy), true) => policy.outcome(&judged),
            _ => default_outcome,
        };

        let (started_at, ended_at) = execution_window(attempt);
        let mut runner =
            json!({"runner_id": config.runner_id, "runner_type": runner_type(config.runner_type)});
        if let Some(version) = &config.runner_version {
            insert_if(&mut runner, "runner_version", true, json!(version));
        }
        let mut run = json!({
            "record_id": run_id,
            "record_type": "run",
            "schema_version": SCHEMA_VERSION,
            "created_at": created_at,
            "provenance": provenance,
            "evaluation_ref": reference_json(attempt.evaluation_ref),
            "trial_ref": reference_json(&attempt.trial.trial_ref),
            "task_revision_ref": reference_json(&attempt.trial.task_revision_ref),
            "system_fingerprint_ref": reference_json(&attempt.trial.system_fingerprint_ref),
            "attempt_number": attempt.attempt_number,
            "attempt_kind": if attempt.attempt_number == 1 { "initial" } else { "infrastructure_retry" },
            "runner": runner,
            "execution_status": execution_status(attempt.status),
            "run_outcome": run_outcome,
            "verification_state": verification_state,
            "agent_report": {"completion_claim": "no_claim"},
            "event_refs": event_refs,
            "evidence_refs": evidence_refs,
            "verification_refs": verification_refs,
        });
        if let Some(previous) = attempt.previous_status {
            insert_if(
                &mut run,
                "attempt_reason",
                true,
                json!(format!(
                    "retry under the schedule's retry policy after attempt {} ended {}",
                    attempt.attempt_number - 1,
                    status_word(previous)
                )),
            );
        }
        insert_if(
            &mut run,
            "started_at",
            started_at.is_some(),
            json!(started_at),
        );
        insert_if(&mut run, "ended_at", true, json!(ended_at));
        if let Ok(result) = attempt.result {
            // Only measured usage: the runner's token/tool-call zeros are
            // placeholders, not measurements, so they are not recorded.
            insert_if(
                &mut run,
                "resource_usage",
                true,
                json!({"wall_time_ms": result.resource_usage.wall_time_ms}),
            );
        }
        let summary = match (attempt.status, attempt.result) {
            // The coordinator rejected an Ok result that failed validation.
            (AttemptStatus::RunnerError, Ok(_)) => {
                Some("runner error (protocol_violation): result failed validation".to_string())
            }
            _ => error_summary(attempt.result),
        };
        if let Some(summary) = summary {
            insert_if(&mut run, "error_summary", true, json!(summary));
        }
        records.push(run);

        self.metadata().put_records(&records)?;
        if let Some(staging) = staging {
            // Recorded (and stored as evidence when attested): the staged copy
            // is no longer needed. It stays if recording failed, for catch-up.
            let _ = std::fs::remove_dir_all(staging);
        }
        Ok(())
    }
}

/// How much a criterion matters, from a task revision's acceptance criteria.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Criticality {
    Mandatory,
    Major,
    Minor,
}

/// Run-outcome rules declared by a `task_revision` record: each acceptance
/// criterion's criticality, the criteria verification must cover
/// (`verification_plan.required_criterion_ids`), and the task's validity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomePolicy {
    criteria: BTreeMap<String, Criticality>,
    required: BTreeSet<String>,
    task_invalid: bool,
}

impl OutcomePolicy {
    /// Reads the policy from a stored (schema-valid) task_revision record.
    pub fn from_task_revision(record: &Value) -> StoreResult<Self> {
        let bad = |what: &str| {
            StoreError::InvalidRecord(format!(
                "task_revision {}: {what}",
                record["record_id"].as_str().unwrap_or("?")
            ))
        };
        let mut criteria = BTreeMap::new();
        for criterion in record["acceptance_criteria"]
            .as_array()
            .ok_or_else(|| bad("acceptance_criteria is missing"))?
        {
            let id = criterion["criterion_id"]
                .as_str()
                .ok_or_else(|| bad("criterion_id is missing"))?;
            let criticality = match criterion["criticality"].as_str() {
                Some("mandatory") => Criticality::Mandatory,
                Some("major") => Criticality::Major,
                Some("minor") => Criticality::Minor,
                _ => return Err(bad("criticality must be mandatory, major or minor")),
            };
            criteria.insert(id.to_string(), criticality);
        }
        let required = record["verification_plan"]["required_criterion_ids"]
            .as_array()
            .ok_or_else(|| bad("verification_plan.required_criterion_ids is missing"))?
            .iter()
            .filter_map(|id| id.as_str().map(str::to_string))
            .collect();
        Ok(Self {
            criteria,
            required,
            task_invalid: record["validity_state"] == "invalid",
        })
    }

    /// The run outcome for a finished, verified attempt:
    /// - a failed **mandatory** criterion fails the run, whatever else is
    ///   known (no average compensates for a mandatory gate);
    /// - a failed **major** criterion fails the run;
    /// - otherwise a required criterion that is unjudged, inconclusive,
    ///   unavailable or mixed makes the run inconclusive;
    /// - otherwise the run passes; **minor** failures stay visible in the
    ///   verification records and the `mixed` verification state.
    ///
    /// Required criteria not listed in `acceptance_criteria` count as major.
    pub fn outcome(&self, judged: &[(String, VerdictResult)]) -> &'static str {
        use VerdictResult::*;
        let results: BTreeMap<&str, VerdictResult> =
            judged.iter().map(|(id, r)| (id.as_str(), *r)).collect();
        let criticality = |id: &str| self.criteria.get(id).copied().unwrap_or(Criticality::Major);
        let failed = |level: Criticality| {
            results
                .iter()
                .any(|(id, r)| *r == Fail && criticality(id) == level)
        };
        if failed(Criticality::Mandatory) || failed(Criticality::Major) {
            return "fail";
        }
        if self.required.is_empty() && results.is_empty() {
            return "not_scored";
        }
        let unresolved = self
            .required
            .iter()
            .any(|id| !matches!(results.get(id.as_str()), Some(Pass) | Some(Fail)));
        if unresolved {
            return "inconclusive";
        }
        "pass"
    }
}

/// Reads a staged workspace file if it is a regular file whose length and
/// SHA-256 match its manifest. Anything else is treated as absent.
fn read_staged(staging: &Path, manifest: &ArtifactManifest) -> Option<Vec<u8>> {
    let relative = Path::new(&manifest.artifact_id);
    let safe = relative
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    if !safe {
        return None;
    }
    let path = staging.join(relative);
    if !std::fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let digest = crate::canonical::sha256_hex(&bytes);
    (bytes.len() as u64 == manifest.byte_length
        && digest.eq_ignore_ascii_case(&manifest.sha256_hex))
    .then_some(bytes)
}

/// Combines criterion results without forcing a binary answer:
/// all pass → verified_pass/pass, all fail → verified_fail/fail, none or all
/// unavailable → unavailable/not_scored, anything else → mixed or
/// inconclusive with an inconclusive outcome.
pub fn aggregate(results: &[VerdictResult]) -> (&'static str, &'static str) {
    use VerdictResult::*;
    if results.is_empty() || results.iter().all(|r| *r == Unavailable) {
        return ("unavailable", "not_scored");
    }
    if results.iter().all(|r| *r == Pass) {
        return ("verified_pass", "pass");
    }
    if results.iter().all(|r| *r == Fail) {
        return ("verified_fail", "fail");
    }
    let has = |kind| results.contains(&kind);
    if has(Mixed) || (has(Pass) && has(Fail)) {
        return ("mixed", "inconclusive");
    }
    ("inconclusive", "inconclusive")
}

fn provenance(attempt: &ResolvedAttempt<'_>, run_id: &str) -> Value {
    json!({
        "source": {
            "system": "cybercore-agent-evaluation",
            "component": COMPONENT,
            "component_version": env!("CARGO_PKG_VERSION"),
            "origin_id": format!("{}:{run_id}", attempt.evaluation_ref.record_id)
        },
        "parent_records": [
            reference_json(attempt.evaluation_ref),
            reference_json(&attempt.trial.trial_ref)
        ],
        "captured_by": COMPONENT,
        "capture_version": env!("CARGO_PKG_VERSION")
    })
}

fn reference_json(reference: &RecordReference) -> Value {
    let mut value = json!({"record_id": reference.record_id, "record_type": reference.record_type});
    if let Some(version) = &reference.schema_version {
        insert_if(&mut value, "schema_version", true, json!(version));
    }
    value
}

fn insert_if(target: &mut Value, key: &str, condition: bool, value: Value) {
    if let (true, Some(map)) = (condition, target.as_object_mut()) {
        map.insert(key.to_string(), value);
    }
}

fn runner_type(kind: RunnerType) -> &'static str {
    match kind {
        RunnerType::Local => "local",
        RunnerType::Cloud => "cloud",
        RunnerType::SelfHosted => "self_hosted",
        RunnerType::External => "external",
    }
}

fn execution_status(status: AttemptStatus) -> &'static str {
    match status {
        AttemptStatus::Finished => "finished",
        AttemptStatus::TimedOut => "timed_out",
        AttemptStatus::Cancelled => "cancelled",
        // A reserved attempt is never recorded; runner errors are execution errors.
        AttemptStatus::Errored | AttemptStatus::RunnerError | AttemptStatus::Reserved => "errored",
    }
}

fn status_word(status: AttemptStatus) -> &'static str {
    match status {
        AttemptStatus::Finished => "finished",
        AttemptStatus::Errored => "with an execution error",
        AttemptStatus::TimedOut => "by timing out",
        AttemptStatus::Cancelled => "cancelled",
        AttemptStatus::RunnerError => "with a runner error",
        AttemptStatus::Reserved => "unresolved",
    }
}

/// Start/end from the runner's own events when present, else the journal's
/// reservation and completion times.
fn execution_window(attempt: &ResolvedAttempt<'_>) -> (Option<String>, String) {
    if let Ok(result) = attempt.result {
        let first = result.events.first().map(|e| e.occurred_at.clone());
        let last = result.events.last().map(|e| e.occurred_at.clone());
        return (
            first.or_else(|| Some(attempt.reserved_at.to_string())),
            last.unwrap_or_else(|| attempt.completed_at.to_string()),
        );
    }
    // Runner errors: the process may never have started.
    (None, attempt.completed_at.to_string())
}

/// Codes and kinds only; raw runner messages are not persisted, except the
/// store's own interruption notice.
fn error_summary(result: &Result<RunnerResult, RunnerError>) -> Option<String> {
    match result {
        Ok(result) => result
            .failure
            .as_ref()
            .map(|f| format!("execution failure: {}", f.code)),
        Err(error) if error.message == INTERRUPTED_ATTEMPT_MESSAGE => Some(format!(
            "runner error (internal): {INTERRUPTED_ATTEMPT_MESSAGE}"
        )),
        Err(error) => Some(format!("runner error ({})", kind_word(error.kind))),
    }
}

fn kind_word(kind: RunnerErrorKind) -> &'static str {
    match kind {
        RunnerErrorKind::InvalidRequest => "invalid_request",
        RunnerErrorKind::UnsupportedPolicy => "unsupported_policy",
        RunnerErrorKind::Unavailable => "unavailable",
        RunnerErrorKind::ProtocolViolation => "protocol_violation",
        RunnerErrorKind::Internal => "internal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use VerdictResult::*;

    #[test]
    fn aggregation_never_forces_a_binary_outcome() {
        assert_eq!(aggregate(&[]), ("unavailable", "not_scored"));
        assert_eq!(
            aggregate(&[Unavailable, Unavailable]),
            ("unavailable", "not_scored")
        );
        assert_eq!(aggregate(&[Pass, Pass]), ("verified_pass", "pass"));
        assert_eq!(aggregate(&[Fail]), ("verified_fail", "fail"));
        assert_eq!(aggregate(&[Pass, Fail]), ("mixed", "inconclusive"));
        assert_eq!(aggregate(&[Mixed]), ("mixed", "inconclusive"));
        assert_eq!(
            aggregate(&[Pass, Inconclusive]),
            ("inconclusive", "inconclusive")
        );
        assert_eq!(
            aggregate(&[Pass, Unavailable]),
            ("inconclusive", "inconclusive")
        );
    }

    fn policy(criteria: &[(&str, &str)], required: &[&str], validity: &str) -> OutcomePolicy {
        OutcomePolicy::from_task_revision(&json!({
            "record_id": "TASK-1",
            "validity_state": validity,
            "acceptance_criteria": criteria.iter().map(|(id, c)| json!({
                "criterion_id": id, "criticality": c,
                "description": "d", "verification_requirement": "v"
            })).collect::<Vec<_>>(),
            "verification_plan": {"required_criterion_ids": required}
        }))
        .unwrap()
    }

    fn judged(items: &[(&str, VerdictResult)]) -> Vec<(String, VerdictResult)> {
        items.iter().map(|(id, r)| (id.to_string(), *r)).collect()
    }

    #[test]
    fn mandatory_failures_are_decisive_and_minor_failures_do_not_fail() {
        let p = policy(
            &[("gate", "mandatory"), ("core", "major"), ("style", "minor")],
            &["gate", "core", "style"],
            "valid",
        );
        assert_eq!(
            p.outcome(&judged(&[("gate", Pass), ("core", Pass), ("style", Pass)])),
            "pass"
        );
        assert_eq!(
            p.outcome(&judged(&[("gate", Pass), ("core", Pass), ("style", Fail)])),
            "pass"
        );
        assert_eq!(
            p.outcome(&judged(&[("gate", Pass), ("core", Fail), ("style", Pass)])),
            "fail"
        );
        // A mandatory failure decides even when other criteria are unknown.
        assert_eq!(p.outcome(&judged(&[("gate", Fail)])), "fail");
        assert_eq!(
            p.outcome(&judged(&[
                ("gate", Pass),
                ("core", Inconclusive),
                ("style", Pass)
            ])),
            "inconclusive"
        );
        assert_eq!(
            p.outcome(&judged(&[("gate", Pass), ("style", Pass)])),
            "inconclusive"
        );
    }

    #[test]
    fn unlisted_required_criteria_count_as_major_and_empty_policies_do_not_pass() {
        let p = policy(&[], &["extra"], "valid");
        assert_eq!(p.outcome(&judged(&[("extra", Fail)])), "fail");
        assert_eq!(p.outcome(&judged(&[("extra", Pass)])), "pass");
        assert_eq!(policy(&[], &[], "valid").outcome(&[]), "not_scored");
        assert!(policy(&[], &[], "invalid").task_invalid);
    }

    #[test]
    fn error_summaries_keep_codes_not_raw_messages() {
        let error = RunnerError {
            kind: RunnerErrorKind::Unavailable,
            retryable: true,
            message: "secret path /home/x".into(),
        };
        assert_eq!(
            error_summary(&Err(error)).unwrap(),
            "runner error (unavailable)"
        );
    }
}
