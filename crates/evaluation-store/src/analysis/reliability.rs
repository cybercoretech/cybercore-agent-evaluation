//! Reliability across repeats (ADR-0029): does the agent pass a task every
//! time, or only sometimes?
//!
//! Over tasks with at least two trials with an outcome (each trial valued by
//! its latest attempt): the share that passed every run, the share that
//! passed at least once, the flaky tasks (both passes and failures) and the
//! tasks that failed every run. The unit is the task, so intervals are
//! Wilson intervals over tasks. Computed on demand: the contract's metric
//! records are defined per planned trial, and this metric is per task.

use super::{wilson, EvaluationOutcomes, TrialOutcome, MIN_OBSERVATIONS};
use std::collections::BTreeMap;

/// One task's latest-attempt results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRuns {
    pub task_id: String,
    pub passes: u64,
    /// Trials with an outcome (pass, fail or no verdict).
    pub counted: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reliability {
    /// Tasks with at least two trials with an outcome.
    pub repeated_tasks: u64,
    /// Tasks with exactly one, left out.
    pub single_run_tasks: u64,
    pub all_runs_pass: u64,
    pub all_runs_fail: u64,
    /// Repeated tasks with both passes and failures, most runs first.
    pub flaky: Vec<TaskRuns>,
    /// Share of repeated tasks that passed every run, with a 95% interval.
    pub all_runs_pass_rate: Option<f64>,
    pub all_runs_interval: Option<(f64, f64)>,
    /// Share of repeated tasks that passed at least once.
    pub any_run_pass_rate: Option<f64>,
    pub any_run_interval: Option<(f64, f64)>,
    /// `computed`, `descriptive_only` or `insufficient_data`.
    pub status: &'static str,
    pub limitations: Vec<String>,
}

pub fn reliability(outcomes: &EvaluationOutcomes) -> Reliability {
    let mut tasks: BTreeMap<&str, TaskRuns> = BTreeMap::new();
    let mut not_run = 0;
    for trial in &outcomes.trials {
        let entry = tasks.entry(&trial.task_id).or_insert_with(|| TaskRuns {
            task_id: trial.task_id.clone(),
            passes: 0,
            counted: 0,
        });
        match trial.outcome {
            TrialOutcome::Pass => {
                entry.passes += 1;
                entry.counted += 1;
            }
            TrialOutcome::Fail | TrialOutcome::NoVerdict => entry.counted += 1,
            TrialOutcome::NotRun => not_run += 1,
            TrialOutcome::Invalid => {}
        }
    }
    let repeated: Vec<&TaskRuns> = tasks.values().filter(|t| t.counted >= 2).collect();
    let single_run_tasks = tasks.values().filter(|t| t.counted == 1).count() as u64;
    let n = repeated.len() as u64;
    let all_runs_pass = repeated.iter().filter(|t| t.passes == t.counted).count() as u64;
    let all_runs_fail = repeated.iter().filter(|t| t.passes == 0).count() as u64;
    let mut flaky: Vec<TaskRuns> = repeated
        .iter()
        .filter(|t| t.passes > 0 && t.passes < t.counted)
        .map(|t| (*t).clone())
        .collect();
    flaky.sort_by(|a, b| b.counted.cmp(&a.counted).then(a.task_id.cmp(&b.task_id)));
    let any = all_runs_pass + flaky.len() as u64;
    let rate = |k: u64| (n > 0).then(|| k as f64 / n as f64);
    let interval = |k: u64| (n > 0).then(|| wilson(k, n));

    let mut limitations = Vec::new();
    if n == 0 {
        limitations
            .push("No task ran more than once: add repeats to measure reliability.".to_string());
    } else if n < MIN_OBSERVATIONS {
        limitations.push(format!(
            "Only {n} task(s) ran more than once (fewer than {MIN_OBSERVATIONS}); treat reliability as descriptive."
        ));
    }
    if single_run_tasks > 0 && n > 0 {
        limitations.push(format!(
            "{single_run_tasks} task(s) ran only once and are left out of reliability."
        ));
    }
    if not_run > 0 {
        limitations.push(format!(
            "{not_run} planned trial(s) have not run yet, so reliability is provisional."
        ));
    }
    if n > 0 {
        limitations.push(
            "Reliability counts tasks, not trials: a task passes every run only if all of its latest attempts passed; trials without a verdict count as not passed."
                .into(),
        );
    }
    let status = if n == 0 {
        "insufficient_data"
    } else if n < MIN_OBSERVATIONS || not_run > 0 {
        "descriptive_only"
    } else {
        "computed"
    };
    Reliability {
        repeated_tasks: n,
        single_run_tasks,
        all_runs_pass,
        all_runs_fail,
        flaky,
        all_runs_pass_rate: rate(all_runs_pass),
        all_runs_interval: interval(all_runs_pass),
        any_run_pass_rate: rate(any),
        any_run_interval: interval(any),
        status,
        limitations,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ReportedUsage, TrialResult};
    use super::*;
    use serde_json::json;

    fn outcomes(tasks: &[(&str, &[TrialOutcome])]) -> EvaluationOutcomes {
        let mut trials = Vec::new();
        for (task, results) in tasks {
            for (i, outcome) in results.iter().enumerate() {
                trials.push(TrialResult {
                    trial_id: format!("{task}-R{i}"),
                    task_id: task.to_string(),
                    outcome: *outcome,
                    attempts: u64::from(*outcome != TrialOutcome::NotRun),
                    wall_time_ms: None,
                    usage: ReportedUsage::Absent,
                    latest_run_id: None,
                });
            }
        }
        EvaluationOutcomes {
            evaluation_id: "E".into(),
            trials,
            plan_digest: "c".repeat(64),
            benchmark_ref: json!({}),
            benchmark_digest: "b".repeat(64),
            task_population_digest: "a".repeat(64),
            system_fingerprint_digest: "f".repeat(64),
            plan_id: "P".into(),
            frozen_plan: None,
            first_result_at: None,
        }
    }

    use TrialOutcome::{Fail, NoVerdict, NotRun, Pass};

    #[test]
    fn tasks_are_classified_by_their_runs() {
        let r = reliability(&outcomes(&[
            ("A", &[Pass, Pass, Pass]),
            ("B", &[Pass, Fail, Pass]),
            ("C", &[Fail, Fail]),
            ("D", &[Pass, NoVerdict]),
            ("E", &[Pass]),
        ]));
        assert_eq!((r.repeated_tasks, r.single_run_tasks), (4, 1));
        assert_eq!((r.all_runs_pass, r.all_runs_fail), (1, 1));
        let flaky: Vec<(&str, u64, u64)> = r
            .flaky
            .iter()
            .map(|t| (t.task_id.as_str(), t.passes, t.counted))
            .collect();
        assert_eq!(flaky, [("B", 2, 3), ("D", 1, 2)]);
        assert_eq!(r.all_runs_pass_rate, Some(0.25));
        assert_eq!(r.any_run_pass_rate, Some(0.75));
        assert_eq!(r.status, "descriptive_only");
        let (lower, upper) = r.all_runs_interval.unwrap();
        assert!(lower < 0.25 && 0.25 < upper);
    }

    #[test]
    fn enough_repeated_tasks_are_computed_and_pending_trials_are_provisional() {
        let many: Vec<(String, Vec<TrialOutcome>)> = (0..10)
            .map(|i| (format!("T{i}"), vec![Pass, Pass]))
            .collect();
        let refs: Vec<(&str, &[TrialOutcome])> = many
            .iter()
            .map(|(t, r)| (t.as_str(), r.as_slice()))
            .collect();
        let r = reliability(&outcomes(&refs));
        assert_eq!((r.status, r.all_runs_pass_rate), ("computed", Some(1.0)));
        let mut pending = refs.clone();
        let extra = [Pass, NotRun];
        pending.push(("T10", &extra));
        assert_eq!(reliability(&outcomes(&pending)).status, "descriptive_only");
    }

    #[test]
    fn no_repeats_means_no_reliability() {
        let r = reliability(&outcomes(&[("A", &[Pass]), ("B", &[Fail])]));
        assert_eq!(
            (r.status, r.all_runs_pass_rate),
            ("insufficient_data", None)
        );
        assert!(r.limitations[0].contains("add repeats"));
    }
}
