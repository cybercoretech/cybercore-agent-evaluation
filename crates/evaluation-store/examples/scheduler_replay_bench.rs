//! How long does `DurableScheduler::acquire` take as the journal grows?
//!
//! cargo run --release -p cybercore-evaluation-store --example scheduler_replay_bench -- <new-dir> [trials]
//!
//! Builds an evaluation with `trials` completed attempts (one reserve and one
//! complete journal entry each, with a 4 KiB stdout), then times a cold
//! acquire, which rebuilds the scheduler state.

use cybercore_evaluation_core::{
    AttemptIdentity, EvaluationSchedule, ExecutionBudget, ExecutionDisposition, NetworkPolicy,
    RecordReference, ResourceUsage, RetryPolicy, RunnerEvent, RunnerResult, SideEffectPolicy,
    TrialDefinition,
};
use cybercore_evaluation_store::{DurableScheduler, SqliteMetadataStore, StoreResult};
use serde_json::json;
use std::time::{Duration, Instant};

fn reference(record_type: &str, record_id: &str) -> RecordReference {
    RecordReference {
        record_id: record_id.into(),
        record_type: record_type.into(),
        schema_version: None,
    }
}

fn main() -> StoreResult<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: <new-dir> [trials]"));
    let trials: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(5_000);
    if dir.exists() {
        eprintln!("refusing to reuse {}", dir.display());
        std::process::exit(2);
    }
    std::fs::create_dir_all(&dir)?;
    let store = SqliteMetadataStore::open(dir.join("meta.sqlite3"))?;
    let schedule = EvaluationSchedule {
        evaluation_ref: reference("evaluation", "EVAL-BENCH"),
        plan_ref: reference("evaluation_plan", "PLAN-BENCH"),
        max_active_runs: 1,
        retry_policy: RetryPolicy::default(),
        trials: (0..trials)
            .map(|n| TrialDefinition {
                trial_ref: reference("trial", &format!("T-{n:06}")),
                task_revision_ref: reference("task_revision", "TASK-1"),
                system_fingerprint_ref: reference("system_fingerprint", "FP-1"),
                task_input: json!({"prompt": "bench"}),
                budget: ExecutionBudget {
                    max_wall_time_ms: 10_000,
                    max_output_bytes: 8192,
                    max_artifact_bytes: 0,
                    max_tool_calls: 0,
                    max_input_tokens: None,
                    max_output_tokens: None,
                },
                network_policy: NetworkPolicy::DenyAll,
                side_effect_policy: SideEffectPolicy::DenyAll,
                capability_grant_refs: vec![],
            })
            .collect(),
    };
    store.create_schedule(&schedule)?;
    let stdout = "x".repeat(4096);
    let built = Instant::now();
    {
        let (mut scheduler, _) =
            DurableScheduler::acquire(&store, "EVAL-BENCH", "bench", Duration::from_secs(3600))?;
        for n in 0..trials {
            let trial = format!("T-{n:06}");
            let request_id = format!("REQ-{n:06}");
            scheduler.reserve_attempt(
                &trial,
                AttemptIdentity {
                    run_ref: reference("run", &format!("RUN-{n:06}")),
                    request_id: request_id.clone(),
                    idempotency_key: format!("IDEMP-{n:06}"),
                },
            )?;
            let event = |sequence, kind: &str| RunnerEvent {
                sequence,
                occurred_at: "2026-11-19T00:00:00.000Z".into(),
                actor: "bench".into(),
                event_type: kind.into(),
                payload: json!({}),
            };
            scheduler.complete_attempt(
                &trial,
                &request_id,
                Ok(RunnerResult {
                    request_id: request_id.clone(),
                    disposition: ExecutionDisposition::Finished,
                    events: vec![
                        event(1, "execution_started"),
                        event(2, "execution_finished"),
                    ],
                    artifacts: vec![],
                    resource_usage: ResourceUsage {
                        wall_time_ms: 5,
                        ..ResourceUsage::default()
                    },
                    output: Some(json!({"stdout": stdout, "exit_code": 0})),
                    failure: None,
                }),
            )?;
        }
        scheduler.release()?;
    }
    println!(
        "built {} journal entries in {:?}",
        trials * 2,
        built.elapsed()
    );

    for round in 1..=3 {
        let started = Instant::now();
        let (scheduler, report) =
            DurableScheduler::acquire(&store, "EVAL-BENCH", "bench", Duration::from_secs(3600))?;
        let elapsed = started.elapsed();
        println!(
            "acquire #{round}: {elapsed:?} (replayed {} commands, snapshot {:?})",
            report.replayed_commands, report.snapshot_seq
        );
        scheduler.release()?;
    }

    // Same store without the snapshot: a full journal replay.
    rusqlite::Connection::open(dir.join("meta.sqlite3"))
        .and_then(|c| c.execute("DELETE FROM scheduler_snapshots", []))
        .map_err(cybercore_evaluation_store::StoreError::Database)?;
    let started = Instant::now();
    let (scheduler, report) =
        DurableScheduler::acquire(&store, "EVAL-BENCH", "bench", Duration::from_secs(3600))?;
    println!(
        "acquire without snapshot: {:?} (replayed {} commands)",
        started.elapsed(),
        report.replayed_commands
    );
    scheduler.release()?;
    Ok(())
}
