//! `workbench`: run and inspect local CyberCore agent evaluations.
//!
//! Local only: no network listener and no remote store. Trials get network
//! access or credentials only through an explicit Podman `runner.egress`
//! allowlist (ADR-0019). It drives the implemented and tested services
//! (store, durable scheduler, run records, runners, verifiers) and nothing
//! else.

mod evaluation_file;

/// `println!` that exits quietly when stdout is closed early (for example
/// `workbench status X | head`) instead of panicking on a broken pipe.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        if let Err(error) = writeln!(std::io::stdout(), $($arg)*) {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                std::process::exit(0);
            }
            return Err(format!("writing output: {error}"));
        }
    }};
}

mod summary;

use clap::{Parser, Subcommand};
use cybercore_evaluation_core::{AttemptIdentity, EvaluationSchedule, RecordReference};
use cybercore_evaluation_store::{
    run_ready_trials, ContractValidator, DurableScheduler, LocalStore, OutcomePolicy, PutOutcome,
    RunRecorderConfig,
};
use evaluation_file::EvaluationFile;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const MAX_RECORD_ID: usize = 128;

#[derive(Parser)]
#[command(
    name = "workbench",
    version,
    about = "Run and inspect local CyberCore agent evaluations.",
    long_about = "Run and inspect local CyberCore agent evaluations.\n\n\
        The store holds every record, the scheduler journal and evidence. \
        Its location is --store, else $WORKBENCH_STORE, else \
        $XDG_DATA_HOME/cybercore-workbench (default ~/.local/share/cybercore-workbench).\n\n\
        Exit codes: 0 success (failed or inconclusive trial outcomes are results, not errors), \
        1 error, 2 usage error."
)]
struct Cli {
    /// Store directory (overrides $WORKBENCH_STORE and the XDG default).
    #[arg(long, global = true, env = "WORKBENCH_STORE", value_name = "DIR")]
    store: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run (or resume) an evaluation from an evaluation file.
    ///
    /// Safe to repeat: completed trials are not re-run, records missing after
    /// a crash are rebuilt, and interrupted attempts are recorded and retried
    /// only if the schedule's retry policy allows it.
    Run {
        /// Evaluation file (format cybercore-workbench-evaluation/v0).
        file: PathBuf,
        /// Validate the file and show the plan without running or writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show an evaluation's trials and recorded runs (read-only).
    Status {
        /// Evaluation record ID, e.g. EVAL-CAT-SMOKE.
        evaluation_id: String,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Pass rate with a 95% interval for one evaluation.
    ///
    /// Trials without a verdict count as not passed; invalidated runs are
    /// excluded; unrun trials make the result provisional. Records a
    /// metric_result and analysis_result (once per distinct set of results).
    Summary {
        /// Evaluation record ID.
        evaluation_id: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Compare two evaluations run on the same tasks (paired by task).
    ///
    /// Evaluations over different task revisions are reported as not
    /// comparable. Records a comparison_result.
    Compare {
        /// Baseline evaluation record ID.
        baseline: String,
        /// Treatment evaluation record ID.
        treatment: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match resolve_store(cli.store) {
        Err(message) => Err(message),
        Ok(store) => match cli.command {
            Command::Run { file, dry_run } => run(&store, &file, dry_run),
            Command::Status {
                evaluation_id,
                json,
            } => status(&store, &evaluation_id, json),
            Command::Summary {
                evaluation_id,
                json,
            } => summary::summary(&store, &evaluation_id, json),
            Command::Compare {
                baseline,
                treatment,
                json,
            } => summary::compare_evaluations(&store, &baseline, &treatment, json),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("workbench: {message}");
            ExitCode::FAILURE
        }
    }
}

/// --store / $WORKBENCH_STORE (handled by clap), else the XDG data dir.
fn resolve_store(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let data_home = match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = std::env::var_os("HOME").filter(|v| !v.is_empty()).ok_or(
                "cannot find the store: set --store, $WORKBENCH_STORE, $XDG_DATA_HOME or $HOME",
            )?;
            PathBuf::from(home).join(".local/share")
        }
    };
    if !data_home.is_absolute() {
        return Err(format!(
            "XDG_DATA_HOME must be absolute, found {}",
            data_home.display()
        ));
    }
    Ok(data_home.join("cybercore-workbench"))
}

fn run(store_dir: &Path, file: &Path, dry_run: bool) -> Result<(), String> {
    let evaluation = EvaluationFile::load(file)?;
    let evaluation_id = evaluation.evaluation_id().to_string();
    // Validates the schedule exactly as the scheduler will.
    cybercore_evaluation_core::TrialCoordinator::new(evaluation.schedule.clone())
        .map_err(|error| format!("{}: schedule: {error}", file.display()))?;
    check_identity_lengths(&evaluation.schedule)?;

    let validator = ContractValidator::embedded().map_err(|e| e.to_string())?;
    for (index, record) in evaluation.task_revisions.iter().enumerate() {
        validator
            .validate(record)
            .map_err(|e| format!("{}: task_revisions[{index}]: {e}", file.display()))?;
    }

    if dry_run {
        return print_plan(store_dir, &evaluation);
    }

    let store = LocalStore::open(store_dir).map_err(|e| format!("{}: {e}", store_dir.display()))?;
    match store
        .metadata()
        .create_schedule(&evaluation.schedule)
        .map_err(|e| format!("{evaluation_id}: {e}"))?
    {
        PutOutcome::Inserted => say!("created {evaluation_id} in {}", store_dir.display()),
        PutOutcome::AlreadyPresent => say!("resuming {evaluation_id} in {}", store_dir.display()),
    }

    if !evaluation.task_revisions.is_empty() {
        store
            .metadata()
            .put_records(&evaluation.task_revisions)
            .map_err(|e| format!("task_revisions: {e}"))?;
    }

    // The analysis plan is frozen before any trial runs (ADR-0021).
    if let Some(declaration) = evaluation.plan_declaration() {
        use cybercore_evaluation_store::analysis::{freeze_plan, FreezeOutcome};
        match freeze_plan(&store, &evaluation.schedule, &declaration)
            .map_err(|e| format!("analysis_plan: {e}"))?
        {
            FreezeOutcome::Frozen { plan_id, frozen_at } => {
                say!("froze analysis plan {plan_id} at {frozen_at}")
            }
            FreezeOutcome::AlreadyFrozen { plan_id, frozen_at } => {
                say!("analysis plan {plan_id} unchanged (frozen at {frozen_at})")
            }
        }
    }

    let artifact_root = store_dir.join("runner-artifacts");
    let runner = evaluation
        .build_runner(store_dir.join("runner-tmp"), artifact_root.clone())
        .map_err(|e| format!("runner: {e}"))?;
    let mut config = RunRecorderConfig::for_runner(
        runner.as_ref(),
        evaluation.project_ref.clone(),
        evaluation.store_output_evidence,
    );
    if evaluation.collects_outputs() {
        config = config.with_artifact_root(artifact_root);
    }
    let verifier = evaluation.verifier();

    let (mut scheduler, recovery) = DurableScheduler::acquire(
        store.metadata(),
        &evaluation_id,
        &owner(),
        lease_ttl(&evaluation.schedule),
    )
    .map_err(|e| format!("{evaluation_id}: {e}"))?;
    if !recovery.interrupted_attempts.is_empty() {
        say!(
            "closed interrupted attempts from a previous run: {}",
            recovery.interrupted_attempts.join(", ")
        );
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let eval_for_ids = evaluation_id.clone();
    let report = runtime
        .block_on(run_ready_trials(
            &store,
            &mut scheduler,
            runner.as_ref(),
            &config,
            verifier.as_ref(),
            move |trial_id, attempt| identity(&eval_for_ids, trial_id, attempt),
        ))
        .map_err(|e| format!("{evaluation_id}: {e}"))?;
    scheduler
        .release()
        .map_err(|e| format!("{evaluation_id}: {e}"))?;

    if !report.caught_up.written.is_empty() {
        say!(
            "recorded runs left unrecorded by an earlier crash: {}",
            report.caught_up.written.join(", ")
        );
    }
    say!(
        "executed {} attempt(s), up to {} at once\n",
        report.executed.len(),
        report.peak_concurrency
    );
    print_status(&store, &evaluation_id, false)
}

fn status(store_dir: &Path, evaluation_id: &str, json: bool) -> Result<(), String> {
    if !store_dir.join("metadata.sqlite3").is_file() {
        return Err(format!("no workbench store at {}", store_dir.display()));
    }
    let store = LocalStore::open(store_dir).map_err(|e| format!("{}: {e}", store_dir.display()))?;
    print_status(&store, evaluation_id, json)
}

fn print_status(store: &LocalStore, evaluation_id: &str, json: bool) -> Result<(), String> {
    let meta = store.metadata();
    let schedule = meta
        .get_schedule(evaluation_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("unknown evaluation {evaluation_id}"))?;

    let mut run_ids = BTreeSet::new();
    for reference in meta
        .references_to(evaluation_id)
        .map_err(|e| e.to_string())?
    {
        if reference.source_type == "run" {
            run_ids.insert(reference.source_id);
        }
    }
    let mut runs = Vec::new();
    for id in run_ids {
        if let Some(run) = meta.get_record(&id).map_err(|e| e.to_string())? {
            runs.push(run);
        }
    }
    let field = |run: &Value, key: &str| run[key].as_str().unwrap_or("-").to_string();

    let trials: Vec<Value> = schedule
        .trials
        .iter()
        .map(|trial| {
            let id = &trial.trial_ref.record_id;
            let mut attempts: Vec<&Value> = runs
                .iter()
                .filter(|run| run["trial_ref"]["record_id"].as_str() == Some(id))
                .collect();
            attempts.sort_by_key(|run| run["attempt_number"].as_u64());
            json!({
                "trial_id": id,
                "runs": attempts.iter().map(|run| json!({
                    "run_id": field(run, "record_id"),
                    "attempt_number": run["attempt_number"],
                    "execution_status": field(run, "execution_status"),
                    "verification_state": field(run, "verification_state"),
                    "run_outcome": field(run, "run_outcome"),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    if json {
        let out = json!({"evaluation_id": evaluation_id, "trials": trials});
        say!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    let rows: Vec<[String; 6]> = trials
        .iter()
        .flat_map(|trial| {
            let trial_id = trial["trial_id"].as_str().unwrap_or("-").to_string();
            let runs = trial["runs"].as_array().cloned().unwrap_or_default();
            if runs.is_empty() {
                return vec![[
                    trial_id,
                    "-".into(),
                    "-".into(),
                    "not run yet".into(),
                    "-".into(),
                    "-".into(),
                ]];
            }
            runs.iter()
                .map(|run| {
                    let text = |key: &str| run[key].as_str().unwrap_or("-").to_string();
                    [
                        trial_id.clone(),
                        text("run_id"),
                        run["attempt_number"].to_string(),
                        text("execution_status"),
                        text("verification_state"),
                        text("run_outcome"),
                    ]
                })
                .collect()
        })
        .collect();
    let header = [
        "TRIAL",
        "RUN",
        "ATTEMPT",
        "EXECUTION",
        "VERIFICATION",
        "OUTCOME",
    ];
    let mut widths = header.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }
    let line = |cells: [&str; 6]| {
        cells
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    say!("{evaluation_id}");
    say!("  {}", line(header));
    for row in &rows {
        say!("  {}", line(row.each_ref().map(String::as_str)));
    }
    let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
    for trial in &trials {
        let runs = trial["runs"].as_array().cloned().unwrap_or_default();
        let latest = runs
            .last()
            .map(|run| run["run_outcome"].as_str().unwrap_or("-").to_string())
            .unwrap_or_else(|| "not run".into());
        *outcomes.entry(latest).or_default() += 1;
    }
    let summary: Vec<String> = outcomes.iter().map(|(k, v)| format!("{v} {k}")).collect();
    say!("\n  latest attempt per trial: {}", summary.join(", "));
    Ok(())
}

fn print_plan(store_dir: &Path, evaluation: &EvaluationFile) -> Result<(), String> {
    let evaluation_id = evaluation.evaluation_id();
    say!("dry run: nothing will be executed or written\n");
    say!("evaluation  {evaluation_id}");
    say!("store       {}", store_dir.display());
    let policy = &evaluation.schedule.retry_policy;
    say!(
        "retries     up to {} attempt(s) per trial; runner errors {}; timeouts {}",
        policy.max_attempts_per_trial,
        if policy.retry_retryable_runner_errors {
            "retried"
        } else {
            "not retried"
        },
        if policy.retry_timeouts {
            "retried"
        } else {
            "not retried"
        },
    );
    match &evaluation.analysis_plan {
        None => say!("analysis    no plan declared (comparisons will be exploratory)"),
        Some(plan) => {
            say!(
                "analysis    frozen at first run as {}: {}",
                evaluation.schedule.plan_ref.record_id,
                plan.decision_question.trim()
            );
            let family = plan.comparisons.len();
            for c in &plan.comparisons {
                say!(
                    "            {}: vs {} at alpha {}{}, at least {} matched tasks{}",
                    c.comparison_id,
                    c.baseline_evaluation_id,
                    c.alpha,
                    if family > 1 {
                        format!(" (Bonferroni over {family})")
                    } else {
                        String::new()
                    },
                    c.minimum_matched_tasks,
                    c.practical_significance_threshold
                        .map(|t| format!(", meaningful beyond ±{t}"))
                        .unwrap_or_default()
                );
            }
        }
    }
    say!(
        "evidence    output evidence {}",
        if evaluation.store_output_evidence {
            "stored (attested non-sensitive)"
        } else {
            "not stored"
        }
    );
    let existing = if store_dir.join("metadata.sqlite3").is_file() {
        let store = LocalStore::open(store_dir).map_err(|e| e.to_string())?;
        store
            .metadata()
            .get_schedule(evaluation_id)
            .map_err(|e| e.to_string())?
    } else {
        None
    };
    match existing {
        Some(stored) if stored == evaluation.schedule => {
            say!("state       already in the store; `run` would resume it")
        }
        Some(_) => {
            return Err(format!(
                "{evaluation_id} is already in the store with a different schedule; \
                 schedules are immutable, use a new evaluation ID"
            ))
        }
        None => say!("state       new evaluation"),
    }
    say!(
        "concurrency up to {} attempt(s) at once",
        evaluation.schedule.max_active_runs
    );
    say!("verifier    {}", evaluation.verifier_summary());
    if evaluation.task_revisions.is_empty() {
        say!("outcomes    default rules (no task_revisions in the file)");
    }
    for record in &evaluation.task_revisions {
        OutcomePolicy::from_task_revision(record).map_err(|e| e.to_string())?;
        let criteria: Vec<String> = record["acceptance_criteria"]
            .as_array()
            .map(|all| {
                all.iter()
                    .map(|c| {
                        format!(
                            "{} ({})",
                            c["criterion_id"].as_str().unwrap_or("?"),
                            c["criticality"].as_str().unwrap_or("?")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        say!(
            "outcomes    {} [{}]: {}",
            record["record_id"].as_str().unwrap_or("?"),
            record["validity_state"].as_str().unwrap_or("?"),
            criteria.join(", ")
        );
    }
    say!("\ntrials ({}):", evaluation.schedule.trials.len());
    let unverifiable = evaluation.unverifiable_trials();
    for trial in &evaluation.schedule.trials {
        let id = &trial.trial_ref.record_id;
        let executable = trial.task_input["executable_id"].as_str().unwrap_or("?");
        let collects = if trial.task_input["collect_outputs"] == serde_json::Value::Bool(true) {
            " collects=/workspace/output"
        } else {
            ""
        };
        let note = if unverifiable.contains(&id.as_str()) {
            "  (no expectation: will be recorded as verification unavailable)"
        } else {
            ""
        };
        let network = match &trial.network_policy {
            cybercore_evaluation_core::NetworkPolicy::DenyAll => String::new(),
            cybercore_evaluation_core::NetworkPolicy::Allowlist { hosts } => {
                format!(" network={}", hosts.join(","))
            }
        };
        let credentials = trial.task_input["credentials"]
            .as_array()
            .map(|names| {
                let names: Vec<&str> = names.iter().filter_map(|n| n.as_str()).collect();
                format!(" credentials={}", names.join(","))
            })
            .unwrap_or_default();
        say!("  {id:<24} executable={executable}{collects}{network}{credentials}{note}");
    }
    Ok(())
}

fn identity(evaluation_id: &str, trial_id: &str, attempt: u32) -> AttemptIdentity {
    let tag = format!("{evaluation_id}-{trial_id}-A{attempt}");
    AttemptIdentity {
        run_ref: RecordReference {
            record_id: format!("RUN-{tag}"),
            record_type: "run".into(),
            schema_version: Some("0.1.0".into()),
        },
        request_id: format!("REQ-{tag}"),
        idempotency_key: format!("IDEMP-{tag}"),
    }
}

/// Run IDs, plus their derived event/verification IDs, must fit the 128-char
/// record ID limit. Checked up front so nothing fails halfway through.
fn check_identity_lengths(schedule: &EvaluationSchedule) -> Result<(), String> {
    let longest_suffix = ":VERIFY:stdout-equals-expected".len();
    for trial in &schedule.trials {
        let run_id = identity(
            &schedule.evaluation_ref.record_id,
            &trial.trial_ref.record_id,
            schedule.retry_policy.max_attempts_per_trial,
        )
        .run_ref
        .record_id;
        if run_id.len() + longest_suffix > MAX_RECORD_ID {
            return Err(format!(
                "evaluation ID plus trial ID {} are too long: derived record IDs would exceed {MAX_RECORD_ID} characters",
                trial.trial_ref.record_id
            ));
        }
    }
    Ok(())
}

/// Stable per host, so a restarted `workbench run` can take its own lease
/// back immediately; another host waits for the lease to expire.
fn owner() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "localhost".into());
    format!("workbench-cli@{host}")
}

/// Long enough that one attempt at the schedule's longest wall time cannot
/// outlive the lease (it is renewed after every attempt).
fn lease_ttl(schedule: &EvaluationSchedule) -> Duration {
    let longest = schedule
        .trials
        .iter()
        .map(|t| t.budget.max_wall_time_ms)
        .max()
        .unwrap_or(0);
    Duration::from_millis(longest.saturating_mul(2).saturating_add(60_000))
        .max(Duration::from_secs(600))
}
