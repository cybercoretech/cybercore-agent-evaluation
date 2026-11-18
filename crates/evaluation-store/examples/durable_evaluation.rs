//! End-to-end durable evaluation: the real Bubblewrap runner driven by
//! `run_ready_trials`, producing ordinary Workbench records (`run`,
//! `run_event`, `verification_result`, output `evidence_artifact`) judged by an
//! independent stdout verifier. TRIAL-4's expectation deliberately differs
//! from its output, so the summary shows a verified fail as well as passes.
//!
//!   cargo run -p cybercore-evaluation-store --example durable_evaluation -- <dir> [--crash-after N]
//!
//! `--crash-after N` kills the process (exit 137) while the (N+1)-th attempt
//! is executing, after its reservation was journaled. Running the same command
//! again without the flag resumes: the interrupted attempt is recorded as
//! errored, retried under the schedule's policy, and the rest run.

use cybercore_evaluation_core::{
    AttemptIdentity, EvaluationSchedule, ExecutionBudget, ExecutionRequest, LocalProcessRunner,
    LocalProcessRunnerConfig, NetworkPolicy, RecordReference, RetryPolicy, Runner,
    RunnerCapabilities, RunnerDescriptor, RunnerFuture, SideEffectPolicy, TrialDefinition,
};
use cybercore_evaluation_store::{
    run_ready_trials, DurableScheduler, LocalStore, RunRecorderConfig, StdoutEqualsVerifier,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const EVALUATION_ID: &str = "EVAL-DURABLE-DEMO";
const TRIALS: usize = 4;

fn reference(record_type: &str, record_id: &str) -> RecordReference {
    RecordReference {
        record_id: record_id.into(),
        record_type: record_type.into(),
        schema_version: Some("0.1.0".into()),
    }
}

fn schedule() -> EvaluationSchedule {
    let trials = (1..=TRIALS)
        .map(|n| TrialDefinition {
            trial_ref: reference("trial", &format!("TRIAL-{n}")),
            task_revision_ref: reference("task_revision", "TASK-CAT"),
            system_fingerprint_ref: reference("system_fingerprint", "FP-LOCAL"),
            task_input: json!({
                "executable_id": "cat",
                "args": ["/workspace/input/task.txt"],
                "files": [{"path": "task.txt", "content": format!("trial {n} output\n")}],
                "environment": {}
            }),
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
        })
        .collect();
    EvaluationSchedule {
        evaluation_ref: reference("evaluation", EVALUATION_ID),
        plan_ref: reference("evaluation_plan", "PLAN-DEMO"),
        max_active_runs: 1,
        retry_policy: RetryPolicy {
            max_attempts_per_trial: 2,
            retry_retryable_runner_errors: true,
            ..RetryPolicy::default()
        },
        trials,
    }
}

/// Wraps the real runner and kills the process during the (N+1)-th
/// execution, after its reservation is already journaled.
struct CrashingRunner {
    inner: LocalProcessRunner,
    crash_on: Option<usize>,
    started: AtomicUsize,
}

impl Runner for CrashingRunner {
    fn descriptor(&self) -> &RunnerDescriptor {
        self.inner.descriptor()
    }

    fn capabilities(&self) -> &RunnerCapabilities {
        self.inner.capabilities()
    }

    fn execute<'a>(&'a self, request: &'a ExecutionRequest) -> RunnerFuture<'a> {
        if Some(self.started.fetch_add(1, Ordering::SeqCst)) == self.crash_on {
            println!(
                "simulating a crash while {} is executing",
                request.request_id
            );
            std::process::exit(137);
        }
        self.inner.execute(request)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let usage = "usage: durable_evaluation <dir> [--crash-after N]";
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(args.next().ok_or(usage)?);
    let crash_on: Option<usize> = match (args.next().as_deref(), args.next()) {
        (Some("--crash-after"), Some(n)) => Some(n.parse()?),
        (None, _) => None,
        _ => return Err(usage.into()),
    };

    let runner = CrashingRunner {
        inner: LocalProcessRunner::new(LocalProcessRunnerConfig {
            runner_id: "local-bwrap-durable-demo".into(),
            runner_version: env!("CARGO_PKG_VERSION").into(),
            bubblewrap_path: std::env::var_os("BWRAP")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/usr/bin/bwrap")),
            temporary_root: std::env::temp_dir().join("cybercore-local-runner-inputs"),
            allowed_executables: BTreeMap::from([("cat".into(), PathBuf::from("/usr/bin/cat"))]),
            allowed_environment_keys: BTreeSet::new(),
            max_wall_time_ms: 10_000,
            max_output_bytes: 4_096,
            max_workspace_bytes: 16 * 1024 * 1024,
            max_tmp_bytes: 8 * 1024 * 1024,
            max_input_file_bytes: 1024 * 1024,
            max_input_files: 8,
            max_arguments: 64,
            max_argument_bytes: 64 * 1024,
        })?,
        crash_on,
        started: AtomicUsize::new(0),
    };

    // The verifier's expectations are hidden grader data: they are not part
    // of any task_input the agent sees.
    let verifier = StdoutEqualsVerifier::new(
        (1..=TRIALS)
            .map(|n| {
                let expected = if n == 4 {
                    "trial 4 OUTPUT\n".to_string()
                } else {
                    format!("trial {n} output\n")
                };
                (format!("TRIAL-{n}"), expected)
            })
            .collect(),
    );
    // The demo's outputs are synthetic, so storing them unredacted is safe.
    let config =
        RunRecorderConfig::for_runner(&runner, reference("project", "PROJECT-LOCAL"), true);

    let store = LocalStore::open(&directory)?;
    store.metadata().create_schedule(&schedule())?;
    let (mut scheduler, recovery) = DurableScheduler::acquire(
        store.metadata(),
        EVALUATION_ID,
        // A stable owner name lets a restarted process on the same host take
        // its own lease back at once; fencing still stops any older instance.
        "durable-evaluation-example",
        Duration::from_secs(60),
    )?;
    println!(
        "lease token {}, replayed {} journal commands, interrupted attempts: {:?}",
        scheduler.fencing_token(),
        recovery.replayed_commands,
        recovery.interrupted_attempts
    );

    let report = run_ready_trials(
        &store,
        &mut scheduler,
        &runner,
        &config,
        &verifier,
        |trial_id, attempt| {
            let tag = format!("{trial_id}-A{attempt}");
            AttemptIdentity {
                run_ref: reference("run", &format!("RUN-{tag}")),
                request_id: format!("REQ-{tag}"),
                idempotency_key: format!("IDEMP-{tag}"),
            }
        },
    )
    .await?;
    println!(
        "recorded from journal: {:?}; executed now: {:?}",
        report.caught_up.written, report.executed
    );

    println!("\nrun records:");
    for run in store.metadata().list_records("run", None, 100)? {
        println!(
            "  {:<18} attempt {} {:<9} verification={:<14} outcome={}",
            run["record_id"].as_str().unwrap_or("?"),
            run["attempt_number"],
            run["execution_status"].as_str().unwrap_or("?"),
            run["verification_state"].as_str().unwrap_or("?"),
            run["run_outcome"].as_str().unwrap_or("?"),
        );
    }
    let count = |kind| store.metadata().count_records(Some(kind));
    println!(
        "records: {} run, {} run_event, {} verification_result, {} evidence_artifact",
        count("run")?,
        count("run_event")?,
        count("verification_result")?,
        count("evidence_artifact")?
    );
    scheduler.release()?;
    Ok(())
}
