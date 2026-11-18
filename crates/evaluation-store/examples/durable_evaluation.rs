//! End-to-end durable evaluation: the real Bubblewrap runner driven by the
//! durable scheduler, with each run's stdout stored as verified evidence.
//!
//!   cargo run -p cybercore-evaluation-store --example durable_evaluation -- <dir> [--crash-after N]
//!
//! `--crash-after N` exits the process (code 137) right after reserving the
//! (N+1)-th attempt, without completing it, like a kill -9 mid-run. Running
//! the same command again without the flag resumes: the interrupted attempt is
//! closed, retried under the schedule's policy, and the remaining trials run.

use cybercore_evaluation_core::{
    execute_with_runner, AttemptIdentity, EvaluationSchedule, ExecutionBudget, LocalProcessRunner,
    LocalProcessRunnerConfig, NetworkPolicy, RecordReference, RetryPolicy, SideEffectPolicy,
    TrialDefinition,
};
use cybercore_evaluation_store::{DurableScheduler, LocalStore};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::PathBuf;
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

fn evidence_draft(run_id: &str) -> Value {
    json!({
        "record_id": format!("ART-{run_id}"),
        "record_type": "evidence_artifact",
        "schema_version": "0.1.0",
        "created_at": chrono_now(),
        "provenance": {
            "source": {
                "system": "durable-evaluation-example",
                "component": "local-process-runner",
                "component_version": env!("CARGO_PKG_VERSION"),
                "origin_id": run_id
            },
            "captured_by": "durable-evaluation-example",
            "capture_version": env!("CARGO_PKG_VERSION")
        },
        "project_ref": {"record_id": "PROJECT-LOCAL", "record_type": "project"},
        "source_record_ref": {"record_id": run_id, "record_type": "run"},
        "artifact_kind": "agent_output",
        "content_type": "text/plain",
        "redaction_state": "not_required",
        "access_classification": "project_internal",
        "retention": {"policy_id": "local-demo", "legal_hold": false},
        "description": "stdout captured by the local process runner"
    })
}

fn chrono_now() -> String {
    // RFC 3339 UTC with milliseconds, matching the store's own timestamps.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        now.subsec_millis()
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(
        args.next()
            .ok_or("usage: durable_evaluation <dir> [--crash-after N]")?,
    );
    let crash_after: Option<usize> = match (args.next().as_deref(), args.next()) {
        (Some("--crash-after"), Some(n)) => Some(n.parse()?),
        (None, _) => None,
        _ => return Err("usage: durable_evaluation <dir> [--crash-after N]".into()),
    };

    let runner = LocalProcessRunner::new(LocalProcessRunnerConfig {
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
    })?;

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
        "acquired lease (token {}), replayed {} journal commands, interrupted attempts closed: {:?}",
        scheduler.fencing_token(),
        recovery.replayed_commands,
        recovery.interrupted_attempts
    );

    let mut completed_here = 0;
    while let Some(trial_id) = scheduler.coordinator().ready_trial_ids().first().cloned() {
        let attempt = scheduler.coordinator().snapshot(&trial_id)?.attempts.len() + 1;
        let tag = format!("{trial_id}-A{attempt}");
        let reservation = scheduler.reserve_attempt(
            &trial_id,
            AttemptIdentity {
                run_ref: reference("run", &format!("RUN-{tag}")),
                request_id: format!("REQ-{tag}"),
                idempotency_key: format!("IDEMP-{tag}"),
            },
        )?;
        if crash_after == Some(completed_here) {
            println!(
                "simulating a crash with {} reserved and not completed",
                reservation.request.request_id
            );
            std::process::exit(137);
        }

        let result = execute_with_runner(&runner, &reservation.request).await;
        if let Ok(result) = &result {
            let stdout = result
                .output
                .as_ref()
                .and_then(|output| output.get("stdout"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            store.put_evidence(
                &evidence_draft(&reservation.request.run_ref.record_id),
                stdout.as_bytes(),
            )?;
        }
        let decision =
            scheduler.complete_attempt(&trial_id, &reservation.request.request_id, result)?;
        println!(
            "{trial_id} attempt {}: {:?} -> {:?}",
            decision.attempt_number, decision.action, decision.state
        );
        scheduler.renew_lease()?;
        completed_here += 1;
    }

    for n in 1..=TRIALS {
        let snapshot = scheduler.coordinator().snapshot(&format!("TRIAL-{n}"))?;
        let attempts: Vec<String> = snapshot
            .attempts
            .iter()
            .map(|a| format!("{}={:?}", a.request_id, a.status))
            .collect();
        println!("TRIAL-{n}: {:?} [{}]", snapshot.state, attempts.join(", "));
    }
    println!(
        "evidence records: {}",
        store.metadata().count_records(Some("evidence_artifact"))?
    );
    scheduler.release()?;
    Ok(())
}
