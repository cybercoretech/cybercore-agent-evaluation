//! Cross-run summaries (ADR-0020): an evaluation's pass rate with a 95%
//! interval, and a paired comparison of two evaluations over the same tasks,
//! recorded as `metric_definition`, `metric_result`, `analysis_result` and
//! `comparison_result` records.
//!
//! Policy (`workbench-pass-rate`, revision 1):
//! - The unit is the planned trial; its outcome is its latest attempt's.
//! - Trials without a verdict (`inconclusive`, `not_scored`) count as **not
//!   passed** and are reported as missing. `invalid` runs are excluded.
//!   Trials not run yet (or `pending`) are left out and make the result
//!   descriptive only.
//! - The interval is Wilson's when every task has one trial, otherwise a
//!   cluster bootstrap over tasks, so repeated trials of one task do not
//!   pass for independent evidence. Bootstrap seeds come from the data, so
//!   the same results always give the same interval.
//! - Comparisons pair trials by task revision. They are exploratory unless the
//!   treatment's frozen `evaluation_plan` declared them before any treatment
//!   result existed (ADR-0021); then the declared rule is applied unchanged
//!   and the result is confirmatory.

use crate::canonical::{canonical_json, sha256_hex};
use crate::{LocalStore, StoreError, StoreResult};
use chrono::{DateTime, SecondsFormat, Utc};
use cybercore_evaluation_core::EvaluationSchedule;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const METRIC_ID: &str = "workbench-pass-rate";
pub const ANALYSIS_VERSION: &str = "workbench-analysis/1";
pub const CONFIDENCE_LEVEL: f64 = 0.95;
/// Below this many trials with an outcome, results are descriptive only.
pub const MIN_OBSERVATIONS: u64 = 10;
/// Fewest distinct tasks for a bootstrap interval; resampling one or two
/// clusters would report a falsely narrow (even zero-width) interval.
pub const MIN_BOOTSTRAP_TASKS: usize = 5;
const METRIC_RECORD_ID: &str = "METRIC-WORKBENCH-PASS-RATE-R1";
const Z_95: f64 = 1.959_963_984_540_054;
const BOOTSTRAP_REPLICATES: usize = 10_000;
const SCHEMA_VERSION: &str = "0.1.0";
const COMPONENT: &str = "workbench-analysis";
/// The eligibility policy, hashed into every result so mismatches show up.
const ELIGIBILITY_POLICY: &str = "workbench-pass-rate/1: every planned trial is eligible; invalid runs are excluded; trials without a verdict count as not passed; trials not run yet are pending";

/// A planned trial's latest outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialOutcome {
    Pass,
    Fail,
    /// `inconclusive` or `not_scored`: counted as not passed.
    NoVerdict,
    /// `invalid`: excluded.
    Invalid,
    /// No completed attempt yet (or `pending`).
    NotRun,
}

#[derive(Debug, Clone)]
pub struct TrialResult {
    pub trial_id: String,
    pub task_id: String,
    pub outcome: TrialOutcome,
    pub attempts: u64,
}

/// Everything a summary needs about one evaluation, read from the store.
#[derive(Debug, Clone)]
pub struct EvaluationOutcomes {
    pub evaluation_id: String,
    pub trials: Vec<TrialResult>,
    pub plan_digest: String,
    pub benchmark_ref: Value,
    pub benchmark_digest: String,
    pub task_population_digest: String,
    pub system_fingerprint_digest: String,
    /// The schedule's `plan_ref` ID.
    pub plan_id: String,
    /// The frozen `evaluation_plan` record, when one was stored.
    pub frozen_plan: Option<Value>,
    /// When the first run record (a result) was written.
    pub first_result_at: Option<DateTime<Utc>>,
}

impl EvaluationOutcomes {
    /// Reads the stored schedule, runs and task revisions of an evaluation.
    pub fn load(store: &LocalStore, evaluation_id: &str) -> StoreResult<Self> {
        let meta = store.metadata();
        let schedule = meta.get_schedule(evaluation_id)?.ok_or_else(|| {
            StoreError::InvalidRecord(format!("unknown evaluation {evaluation_id}"))
        })?;
        let mut runs: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for reference in meta.references_to(evaluation_id)? {
            if reference.source_type != "run" {
                continue;
            }
            if let Some(run) = meta.get_record(&reference.source_id)? {
                let trial = run["trial_ref"]["record_id"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                runs.entry(trial).or_default().push(run);
            }
        }
        let trials = schedule
            .trials
            .iter()
            .map(|trial| {
                let id = &trial.trial_ref.record_id;
                let attempts = runs.get(id).map(Vec::as_slice).unwrap_or_default();
                let latest = attempts
                    .iter()
                    .max_by_key(|run| run["attempt_number"].as_u64().unwrap_or(0));
                let outcome = match latest.and_then(|run| run["run_outcome"].as_str()) {
                    Some("pass") => TrialOutcome::Pass,
                    Some("fail") => TrialOutcome::Fail,
                    Some("inconclusive") | Some("not_scored") => TrialOutcome::NoVerdict,
                    Some("invalid") => TrialOutcome::Invalid,
                    _ => TrialOutcome::NotRun,
                };
                TrialResult {
                    trial_id: id.clone(),
                    task_id: trial.task_revision_ref.record_id.clone(),
                    outcome,
                    attempts: attempts.len() as u64,
                }
            })
            .collect::<Vec<_>>();

        let schedule_json = serde_json::to_value(&schedule)
            .map_err(|e| StoreError::InvalidRecord(format!("schedule: {e}")))?;
        let plan_digest = digest(&schedule_json);
        let (task_population_digest, benchmark_id) = task_population(store, &schedule)?;
        let first_result_at = runs
            .values()
            .flatten()
            .filter_map(|run| run["created_at"].as_str())
            .filter_map(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
            .min();
        let plan_id = schedule.plan_ref.record_id.clone();
        let frozen_plan = meta.get_record(&plan_id)?.filter(|plan| {
            plan["record_type"] == "evaluation_plan" && plan["lifecycle_state"] == "frozen"
        });
        let benchmark_digest = digest(&json!({
            "benchmark": benchmark_id,
            "task_population": task_population_digest,
        }));
        let fingerprints: BTreeSet<&str> = schedule
            .trials
            .iter()
            .map(|t| t.system_fingerprint_ref.record_id.as_str())
            .collect();
        Ok(Self {
            evaluation_id: evaluation_id.to_string(),
            trials,
            plan_digest,
            benchmark_ref: json!({"record_id": benchmark_id, "record_type": "benchmark_revision"}),
            benchmark_digest,
            task_population_digest,
            system_fingerprint_digest: digest(&json!(fingerprints)),
            plan_id,
            frozen_plan,
            first_result_at,
        })
    }

    /// The frozen plan's content digest, else the schedule's digest.
    fn predeclared_digest(&self) -> String {
        self.frozen_plan
            .as_ref()
            .and_then(|plan| plan["content_digest"].as_str())
            .map_or_else(|| self.plan_digest.clone(), str::to_string)
    }

    /// (passes, counted trials) per task, over trials that count.
    fn per_task(&self) -> BTreeMap<&str, (u64, u64)> {
        let mut tasks: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for trial in &self.trials {
            let entry = tasks.entry(trial.task_id.as_str()).or_default();
            match trial.outcome {
                TrialOutcome::Pass => *entry = (entry.0 + 1, entry.1 + 1),
                TrialOutcome::Fail | TrialOutcome::NoVerdict => entry.1 += 1,
                TrialOutcome::Invalid | TrialOutcome::NotRun => {}
            }
        }
        tasks.retain(|_, (_, n)| *n > 0);
        tasks
    }

    fn data_digest(&self) -> String {
        let outcomes: Vec<Value> = self
            .trials
            .iter()
            .map(|t| {
                json!([
                    t.trial_id,
                    t.task_id,
                    format!("{:?}", t.outcome),
                    t.attempts
                ])
            })
            .collect();
        digest(&json!({
            "analysis": ANALYSIS_VERSION,
            "evaluation": self.evaluation_id,
            "plan": self.plan_digest,
            "population": self.task_population_digest,
            "outcomes": outcomes,
        }))
    }
}

/// An evaluation's pass rate.
#[derive(Debug, Clone, PartialEq)]
pub struct PassRate {
    pub planned: u64,
    pub passes: u64,
    /// Trials with pass or fail.
    pub with_verdict: u64,
    /// Trials without a verdict, counted as not passed.
    pub without_verdict: u64,
    pub excluded: u64,
    pub not_run: u64,
    pub attempts: u64,
    pub denominator: u64,
    pub estimate: Option<f64>,
    /// (lower, upper, method)
    pub interval: Option<(f64, f64, &'static str)>,
    /// `computed`, `descriptive_only` or `insufficient_data`.
    pub status: &'static str,
    pub limitations: Vec<String>,
}

pub fn pass_rate(outcomes: &EvaluationOutcomes) -> PassRate {
    let count = |o: TrialOutcome| outcomes.trials.iter().filter(|t| t.outcome == o).count() as u64;
    let (passes, fails) = (count(TrialOutcome::Pass), count(TrialOutcome::Fail));
    let without_verdict = count(TrialOutcome::NoVerdict);
    let (excluded, not_run) = (count(TrialOutcome::Invalid), count(TrialOutcome::NotRun));
    let denominator = passes + fails + without_verdict;
    let tasks = outcomes.per_task();
    let clustered = tasks.values().any(|(_, n)| *n > 1);
    let (estimate, interval) = if denominator == 0 {
        (None, None)
    } else if clustered && tasks.len() < MIN_BOOTSTRAP_TASKS {
        (Some(passes as f64 / denominator as f64), None)
    } else if clustered {
        let clusters: Vec<(u64, u64)> = tasks.values().copied().collect();
        let seed = seed_from(&outcomes.data_digest());
        let (lower, upper) = cluster_bootstrap(&clusters, seed);
        (
            Some(passes as f64 / denominator as f64),
            Some((lower, upper, "cluster_bootstrap")),
        )
    } else {
        let (lower, upper) = wilson(passes, denominator);
        (
            Some(passes as f64 / denominator as f64),
            Some((lower, upper, "wilson")),
        )
    };
    let mut limitations = Vec::new();
    if without_verdict > 0 {
        limitations.push(format!(
            "{without_verdict} trial(s) had no verdict (inconclusive or not scored) and count as not passed."
        ));
    }
    if excluded > 0 {
        limitations.push(format!(
            "{excluded} trial(s) were excluded because their run was invalidated."
        ));
    }
    if not_run > 0 {
        limitations.push(format!(
            "{not_run} planned trial(s) have not run yet, so this result is provisional."
        ));
    }
    if denominator > 0 && denominator < MIN_OBSERVATIONS {
        limitations.push(format!(
            "Only {denominator} trial(s) have an outcome (fewer than {MIN_OBSERVATIONS}); treat the rate as descriptive."
        ));
    }
    if clustered && interval.is_none() && denominator > 0 {
        limitations.push(format!(
            "Some tasks ran more than once and there are only {} distinct task(s) (fewer than {MIN_BOOTSTRAP_TASKS}), so no interval is reported.",
            tasks.len()
        ));
    } else if clustered {
        limitations.push(format!(
            "Some tasks ran more than once; the interval resamples {} task(s), not trials.",
            tasks.len()
        ));
    }
    let status = if denominator == 0 {
        limitations.push("No trial has an outcome yet.".into());
        "insufficient_data"
    } else if not_run > 0 || denominator < MIN_OBSERVATIONS || interval.is_none() {
        "descriptive_only"
    } else {
        "computed"
    };
    PassRate {
        planned: outcomes.trials.len() as u64,
        passes,
        with_verdict: passes + fails,
        without_verdict,
        excluded,
        not_run,
        attempts: outcomes.trials.iter().map(|t| t.attempts).sum(),
        denominator,
        estimate,
        interval,
        status,
        limitations,
    }
}

/// (passes, trials) for one task in the baseline and the treatment.
type TaskPair = ((u64, u64), (u64, u64));

/// A paired comparison of two evaluations.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    /// `comparable`, `not_comparable` or `insufficient_evidence`.
    pub status: &'static str,
    pub design: &'static str,
    pub matched_tasks: u64,
    /// treatment − baseline, in pass-rate points.
    pub difference: Option<f64>,
    pub interval: Option<(f64, f64)>,
    pub method: &'static str,
    pub p_value: Option<f64>,
    pub direction: &'static str,
    pub mismatches: Vec<String>,
    pub limitations: Vec<String>,
    /// `confirmatory`, `exploratory`, `descriptive_only` or `prohibited`.
    pub inferential_status: &'static str,
    /// Confidence level of `interval` (0.95, or 1 − the declared adjusted alpha).
    pub confidence_level: f64,
    pub multiplicity_method: &'static str,
    pub family_size: u64,
    /// `meaningful`, `not_meaningful`, `uncertain` or `not_assessed`.
    pub practical_significance: &'static str,
    pub practical_significance_threshold: Option<f64>,
    /// The predeclared comparison this result answers, if any.
    pub declared_comparison_id: Option<String>,
}

/// A comparison declared in a frozen plan.
struct Declared {
    id: String,
    alpha: f64,
    multiplicity: &'static str,
    family_size: u64,
    minimum_matched: u64,
    design: String,
    threshold: Option<f64>,
    frozen_at: Option<DateTime<Utc>>,
}

fn declared_comparison(
    baseline: &EvaluationOutcomes,
    treatment: &EvaluationOutcomes,
) -> Option<Declared> {
    let plan = treatment.frozen_plan.as_ref()?;
    let all = plan["analysis_plan"]["comparisons"].as_array()?;
    let item = all.iter().find(|c| {
        c["baseline_condition_id"] == baseline.evaluation_id.as_str()
            && c["treatment_condition_id"] == treatment.evaluation_id.as_str()
            && c["metric_id"] == METRIC_ID
    })?;
    Some(Declared {
        id: item["comparison_id"].as_str()?.to_string(),
        alpha: item["alpha"].as_f64()?,
        multiplicity: match item["multiplicity_method"].as_str()? {
            "bonferroni" => "bonferroni",
            "holm" => "holm",
            _ => "none_predeclared_single_test",
        },
        family_size: all.len() as u64,
        minimum_matched: item["minimum_matched_units"].as_u64()?,
        design: item["design"].as_str()?.to_string(),
        threshold: item["practical_significance_threshold"].as_f64(),
        frozen_at: plan["frozen_at"]
            .as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc)),
    })
}

pub fn compare(
    baseline: &EvaluationOutcomes,
    baseline_rate: &PassRate,
    treatment: &EvaluationOutcomes,
    treatment_rate: &PassRate,
) -> Comparison {
    let mut mismatches = Vec::new();
    if baseline.task_population_digest != treatment.task_population_digest {
        mismatches.push("task_population".to_string());
    }
    if baseline.benchmark_digest != treatment.benchmark_digest {
        mismatches.push("benchmark_content".to_string());
    }
    let declared = declared_comparison(baseline, treatment);
    let mut limitations = Vec::new();
    if declared.is_none() {
        limitations.push(
            "Exploratory: the treatment's frozen plan does not declare this comparison."
                .to_string(),
        );
    }
    if !mismatches.is_empty() {
        limitations.push(
            "The evaluations did not run the same task revisions, so their pass rates are not compared.".into(),
        );
        return Comparison {
            status: "not_comparable",
            design: "observational_unpaired",
            matched_tasks: 0,
            difference: None,
            interval: None,
            method: "none",
            p_value: None,
            direction: "undetermined",
            mismatches,
            limitations,
            inferential_status: "prohibited",
            confidence_level: CONFIDENCE_LEVEL,
            multiplicity_method: "none_predeclared_single_test",
            family_size: 1,
            practical_significance: "not_assessed",
            practical_significance_threshold: None,
            declared_comparison_id: declared.map(|d| d.id),
        };
    }
    let base_tasks = baseline.per_task();
    let treat_tasks = treatment.per_task();
    let pairs: Vec<TaskPair> = base_tasks
        .iter()
        .filter_map(|(task, base)| treat_tasks.get(task).map(|treat| (*base, *treat)))
        .collect();
    let difference = match (baseline_rate.estimate, treatment_rate.estimate) {
        (Some(base), Some(treat)) => Some(treat - base),
        _ => None,
    };
    let single = pairs.iter().all(|((_, bn), (_, tn))| *bn == 1 && *tn == 1);
    let p_value = (single && !pairs.is_empty()).then(|| {
        let only_treat = pairs
            .iter()
            .filter(|((b, _), (t, _))| *b == 0 && *t == 1)
            .count() as u64;
        let only_base = pairs
            .iter()
            .filter(|((b, _), (t, _))| *b == 1 && *t == 0)
            .count() as u64;
        mcnemar_exact(only_base, only_treat)
    });
    let interval = (pairs.len() >= MIN_BOOTSTRAP_TASKS).then(|| {
        let seed = seed_from(&format!(
            "{}{}",
            baseline.data_digest(),
            treatment.data_digest()
        ));
        paired_bootstrap(&pairs, seed)
    });
    let direction = match interval {
        Some((lower, _)) if lower > 0.0 => "favors_treatment",
        Some((_, upper)) if upper < 0.0 => "favors_baseline",
        _ => "undetermined",
    };
    if !pairs.is_empty() && pairs.len() < MIN_BOOTSTRAP_TASKS {
        limitations.push(format!(
            "Only {} task(s) are paired (fewer than {MIN_BOOTSTRAP_TASKS}), so no interval is reported.",
            pairs.len()
        ));
    }
    if base_tasks.len() != pairs.len() || treat_tasks.len() != pairs.len() {
        limitations.push("Some tasks have an outcome in only one evaluation; they are left out of the paired interval.".into());
    }
    let ready = baseline_rate.status == "computed" && treatment_rate.status == "computed";
    if !ready {
        limitations
            .push("At least one evaluation's pass rate is descriptive only or has no data.".into());
    }
    let status = if ready && interval.is_some() {
        "comparable"
    } else {
        "insufficient_evidence"
    };
    let mut comparison = Comparison {
        status,
        design: "blocked_paired",
        matched_tasks: pairs.len() as u64,
        difference,
        interval,
        method: if single {
            "paired bootstrap over tasks; McNemar exact test on discordant tasks"
        } else {
            "paired bootstrap over tasks"
        },
        p_value,
        direction,
        mismatches,
        limitations,
        inferential_status: if status == "comparable" {
            "exploratory"
        } else {
            "descriptive_only"
        },
        confidence_level: CONFIDENCE_LEVEL,
        multiplicity_method: "none_predeclared_single_test",
        family_size: 1,
        practical_significance: "not_assessed",
        practical_significance_threshold: None,
        declared_comparison_id: None,
    };
    if let Some(declared) = declared {
        apply_declared(
            &mut comparison,
            &declared,
            treatment,
            &pairs,
            single,
            baseline,
        );
    }
    comparison
}

/// Applies a predeclared comparison's rule, or records why it cannot be
/// confirmatory. Never changes the rule to fit the data.
fn apply_declared(
    comparison: &mut Comparison,
    declared: &Declared,
    treatment: &EvaluationOutcomes,
    pairs: &[TaskPair],
    single: bool,
    baseline: &EvaluationOutcomes,
) {
    comparison.declared_comparison_id = Some(declared.id.clone());
    comparison.practical_significance_threshold = declared.threshold;
    let mut blockers = Vec::new();
    match (declared.frozen_at, treatment.first_result_at) {
        (Some(frozen), Some(first)) if first < frozen => blockers.push(format!(
            "the plan was frozen at {} but the treatment already had results from {}",
            frozen.to_rfc3339_opts(SecondsFormat::Millis, true),
            first.to_rfc3339_opts(SecondsFormat::Millis, true)
        )),
        (None, _) => blockers.push("the plan has no valid frozen_at".into()),
        _ => {}
    }
    if declared.design != "blocked_paired" {
        blockers.push(format!(
            "it declares design {}, but the workbench applies blocked_paired only",
            declared.design
        ));
    }
    if comparison.status != "comparable" {
        blockers.push("the comparison is not comparable with an interval yet".into());
    }
    if comparison.matched_tasks < declared.minimum_matched {
        blockers.push(format!(
            "only {} task(s) are paired, fewer than the declared minimum of {}",
            comparison.matched_tasks, declared.minimum_matched
        ));
    }
    if !blockers.is_empty() {
        comparison.limitations.push(format!(
            "Declared as {} in plan {}, but not confirmatory: {}.",
            declared.id,
            treatment.plan_id,
            blockers.join("; ")
        ));
        return;
    }
    // Bonferroni across the plan's declared comparisons; Holm needs the
    // other comparisons' p-values, so it is applied as Bonferroni.
    let (level, multiplicity) = match declared.multiplicity {
        "none_predeclared_single_test" => (declared.alpha, "none_predeclared_single_test"),
        other => (declared.alpha / declared.family_size.max(1) as f64, other),
    };
    let confidence = 1.0 - level;
    let seed = seed_from(&format!(
        "{}{}",
        baseline.data_digest(),
        treatment.data_digest()
    ));
    let interval = paired_bootstrap_at(pairs, seed, confidence);
    let difference = comparison.difference.unwrap_or(0.0);
    let significant = match comparison.p_value {
        Some(p) if single => p < level,
        _ => interval.0 > 0.0 || interval.1 < 0.0,
    };
    comparison.interval = Some(interval);
    comparison.confidence_level = confidence;
    comparison.direction = match (significant, difference > 0.0, difference < 0.0) {
        (true, true, _) => "favors_treatment",
        (true, _, true) => "favors_baseline",
        _ => "undetermined",
    };
    comparison.inferential_status = "confirmatory";
    comparison.multiplicity_method = multiplicity;
    comparison.family_size = declared.family_size;
    comparison.practical_significance = match declared.threshold {
        None => "not_assessed",
        Some(t) if interval.0 > t || interval.1 < -t => "meaningful",
        Some(t) if interval.0 > -t && interval.1 < t => "not_meaningful",
        Some(_) => "uncertain",
    };
    let rule = if single {
        "McNemar exact test"
    } else {
        "paired bootstrap interval excluding 0"
    };
    comparison.limitations.push(format!(
        "Confirmatory: predeclared as {} in plan {}; decided by the {rule} at level {level:.4} ({} over {} declared comparison(s)).",
        declared.id, treatment.plan_id, multiplicity.replace('_', " "), declared.family_size
    ));
    if declared.multiplicity == "holm" {
        comparison.limitations.push(
            "Holm was declared; it is applied as Bonferroni, which is more conservative.".into(),
        );
    }
}

/// IDs of the records a summary wrote (or found already written).
#[derive(Debug, Clone)]
pub struct SummaryRecords {
    pub metric_result_id: String,
    pub analysis_result_id: String,
}

/// Writes the metric definition (once), a `metric_result` and an
/// `analysis_result`. Record IDs derive from the data, so summarising
/// unchanged results again writes nothing new.
pub fn record_summary(
    store: &LocalStore,
    outcomes: &EvaluationOutcomes,
    rate: &PassRate,
) -> StoreResult<SummaryRecords> {
    let meta = store.metadata();
    let definition = metric_definition();
    if meta.get_record(METRIC_RECORD_ID)?.is_none() {
        meta.put_record(&definition)?;
    }
    let tag = &outcomes.data_digest()[..24];
    let metric_result_id = format!("MR-{tag}");
    let analysis_result_id = format!("AR-{tag}");
    if meta.get_record(&metric_result_id)?.is_none() {
        let now = now();
        let population_key = population_key(outcomes);
        let mut result = json!({
            "record_id": metric_result_id,
            "record_type": "metric_result",
            "schema_version": SCHEMA_VERSION,
            "created_at": now,
            "provenance": provenance(&outcomes.evaluation_id),
            "metric_definition_ref": {"record_id": METRIC_RECORD_ID, "record_type": "metric_definition"},
            "metric_definition_digest": digest(&definition),
            "evaluation_ref": evaluation_ref(&outcomes.evaluation_id),
            "evaluation_plan_digest": outcomes.plan_digest,
            "benchmark_revision_ref": outcomes.benchmark_ref,
            "benchmark_content_digest": outcomes.benchmark_digest,
            "condition_id": outcomes.evaluation_id,
            "system_fingerprint_digest": outcomes.system_fingerprint_digest,
            "task_population_digest": outcomes.task_population_digest,
            "population_key": population_key,
            "aggregation_level": "condition",
            "result_status": rate.status,
            "value_kind": "binary_rate",
            "counts": {
                "planned_trial_count": rate.planned,
                "eligible_trial_count": rate.planned - rate.excluded,
                "excluded_trial_count": rate.excluded,
                "observed_trial_count": rate.with_verdict,
                "missing_trial_count": rate.without_verdict,
                "run_attempt_count": rate.attempts,
                "pending_trial_count": rate.not_run,
            },
            "numerator": rate.passes,
            "denominator": rate.denominator,
            "computed_at": now,
            "analysis_version": ANALYSIS_VERSION,
            "eligibility_policy_digest": sha256_hex(ELIGIBILITY_POLICY.as_bytes()),
        });
        if let (Some(estimate), Some(map)) = (rate.estimate, result.as_object_mut()) {
            map.insert("estimate".into(), json!(estimate));
        }
        if let (Some((lower, upper, method)), Some(map)) = (rate.interval, result.as_object_mut()) {
            map.insert(
                "uncertainty".into(),
                json!({"method": method, "confidence_level": CONFIDENCE_LEVEL, "lower_bound": lower, "upper_bound": upper}),
            );
        }
        if !rate.limitations.is_empty() {
            if let Some(map) = result.as_object_mut() {
                map.insert("limitations".into(), json!(rate.limitations));
            }
        }
        let mut analysis_limitations = rate.limitations.clone();
        analysis_limitations.push(
            "The pass-rate metric is the workbench default, not predeclared in the evaluation file.".into(),
        );
        let analysis = json!({
            "record_id": analysis_result_id,
            "record_type": "analysis_result",
            "schema_version": SCHEMA_VERSION,
            "created_at": now,
            "provenance": provenance(&outcomes.evaluation_id),
            "evaluation_ref": evaluation_ref(&outcomes.evaluation_id),
            "evaluation_plan_digest": outcomes.plan_digest,
            "benchmark_content_digest": outcomes.benchmark_digest,
            "condition_id": outcomes.evaluation_id,
            "system_fingerprint_digest": outcomes.system_fingerprint_digest,
            "task_population_digest": outcomes.task_population_digest,
            "population_key": population_key,
            "analysis_state": "computed",
            "analysis_method_version": ANALYSIS_VERSION,
            "metric_result_refs": [{"record_id": metric_result_id, "record_type": "metric_result"}],
            "primary_metric_ids": [METRIC_ID],
            "predeclared_plan_digest": outcomes.predeclared_digest(),
            "exclusions_summary": format!(
                "{} trial(s) excluded as invalid; {} without a verdict counted as not passed; {} not run yet.",
                rate.excluded, rate.without_verdict, rate.not_run
            ),
            "limitations": analysis_limitations,
            "computed_at": now,
        });
        meta.put_records(&[result, analysis])?;
    }
    Ok(SummaryRecords {
        metric_result_id,
        analysis_result_id,
    })
}

/// Writes a `comparison_result` (once per pair of metric results).
pub fn record_comparison(
    store: &LocalStore,
    baseline: (&EvaluationOutcomes, &SummaryRecords),
    treatment: (&EvaluationOutcomes, &SummaryRecords),
    comparison: &Comparison,
) -> StoreResult<String> {
    let meta = store.metadata();
    let tag = sha256_hex(
        format!(
            "{}|{}|{ANALYSIS_VERSION}",
            baseline.1.metric_result_id, treatment.1.metric_result_id
        )
        .as_bytes(),
    );
    let id = format!("CR-{}", &tag[..24]);
    if meta.get_record(&id)?.is_some() {
        return Ok(id);
    }
    let now = now();
    let mut record = json!({
        "record_id": id,
        "record_type": "comparison_result",
        "schema_version": SCHEMA_VERSION,
        "created_at": now,
        "provenance": provenance(&treatment.0.evaluation_id),
        "metric_definition_ref": {"record_id": METRIC_RECORD_ID, "record_type": "metric_definition"},
        "metric_definition_digest": digest(&metric_definition()),
        "baseline_metric_result_ref": {"record_id": baseline.1.metric_result_id, "record_type": "metric_result"},
        "treatment_metric_result_ref": {"record_id": treatment.1.metric_result_id, "record_type": "metric_result"},
        "baseline_condition_id": baseline.0.evaluation_id,
        "treatment_condition_id": treatment.0.evaluation_id,
        "comparison_status": comparison.status,
        "comparison_design": comparison.design,
        "comparability": {
            "benchmark_content_digest": treatment.0.benchmark_digest,
            "task_population_digest": treatment.0.task_population_digest,
            "metric_definition_digest": digest(&metric_definition()),
            "eligibility_policy_digest": sha256_hex(ELIGIBILITY_POLICY.as_bytes()),
            "aggregation_level": "condition",
            "mismatch_dimensions": comparison.mismatches,
            "limitations": [],
        },
        "sample_sizes": {
            "baseline_trial_count": baseline.0.trials.len(),
            "treatment_trial_count": treatment.0.trials.len(),
            "matched_pair_count": comparison.matched_tasks,
        },
        "inferential_status": comparison.inferential_status,
        "limitations": comparison.limitations,
        "computed_at": now,
    });
    if let (Some(difference), Some(map)) = (comparison.difference, record.as_object_mut()) {
        let mut effect = json!({
            "estimate": difference,
            "unit": "pass-rate difference (treatment − baseline)",
            "direction_interpretation": comparison.direction,
        });
        if let (Some((lower, upper)), Some(effect)) = (comparison.interval, effect.as_object_mut())
        {
            effect.insert(
                "interval".into(),
                json!({"method": "paired_bootstrap", "confidence_level": comparison.confidence_level, "lower_bound": lower, "upper_bound": upper}),
            );
        }
        map.insert("effect".into(), effect);
        let mut inference = json!({
            "method": comparison.method,
            "multiplicity_method": comparison.multiplicity_method,
            "comparison_family_size": comparison.family_size,
            "practical_significance": comparison.practical_significance,
        });
        if let (Some(t), Some(inference)) = (
            comparison.practical_significance_threshold,
            inference.as_object_mut(),
        ) {
            inference.insert("practical_significance_threshold".into(), json!(t));
        }
        if let (Some(p), Some(inference)) = (comparison.p_value, inference.as_object_mut()) {
            inference.insert("p_value".into(), json!(p));
        }
        map.insert("inference".into(), inference);
    }
    meta.put_record(&record)?;
    Ok(id)
}

/// (task-population digest, benchmark ID) for a schedule's task revisions.
fn task_population(
    store: &LocalStore,
    schedule: &EvaluationSchedule,
) -> StoreResult<(String, String)> {
    let meta = store.metadata();
    let task_ids: BTreeSet<&str> = schedule
        .trials
        .iter()
        .map(|t| t.task_revision_ref.record_id.as_str())
        .collect();
    let mut task_digests = Vec::new();
    let mut benchmarks = BTreeSet::new();
    for task in &task_ids {
        match meta.get_record(task)? {
            Some(record) => {
                if let Some(id) = record["benchmark_ref"]["record_id"].as_str() {
                    benchmarks.insert(id.to_string());
                }
                task_digests.push(json!([task, digest(&record)]));
            }
            None => task_digests.push(json!([task, Value::Null])),
        }
    }
    let population = digest(&Value::Array(task_digests));
    let benchmark = match benchmarks.len() {
        1 => benchmarks.into_iter().next().unwrap_or_default(),
        // No (single) stored benchmark: name it after the task set, so
        // evaluations over the same tasks share it.
        _ => format!("BENCH-TASKSET-{}", &population[..24]),
    };
    Ok((population, benchmark))
}

/// A comparison to declare in an evaluation's plan; this evaluation is the
/// treatment.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonDeclaration {
    pub comparison_id: String,
    pub baseline_evaluation_id: String,
    pub alpha: f64,
    pub minimum_matched_tasks: u64,
    pub practical_significance_threshold: Option<f64>,
}

/// What an evaluation file declares before it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanDeclaration {
    pub decision_question: String,
    /// `census`, `random`, `stratified_random`, `curated` or `other`.
    pub task_selection_method: String,
    pub task_families: Vec<String>,
    pub operating_conditions: Vec<String>,
    pub comparisons: Vec<ComparisonDeclaration>,
}

/// Result of [`freeze_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreezeOutcome {
    Frozen { plan_id: String, frozen_at: String },
    AlreadyFrozen { plan_id: String, frozen_at: String },
}

/// Writes the schedule's `plan_ref` as a frozen `evaluation_plan` (ADR-0021).
/// Freezing again with identical content is a no-op; different content is
/// refused, because a predeclared plan must not change once frozen.
pub fn freeze_plan(
    store: &LocalStore,
    schedule: &EvaluationSchedule,
    declaration: &PlanDeclaration,
) -> StoreResult<FreezeOutcome> {
    let meta = store.metadata();
    let plan_id = schedule.plan_ref.record_id.clone();
    let evaluation_id = schedule.evaluation_ref.record_id.clone();
    let fingerprint = schedule
        .trials
        .first()
        .map(|t| &t.system_fingerprint_ref)
        .ok_or_else(|| StoreError::InvalidRecord("the schedule has no trials".into()))?;
    let fingerprint =
        json!({"record_id": fingerprint.record_id, "record_type": fingerprint.record_type});
    let (_, benchmark) = task_population(store, schedule)?;
    let mut per_task: BTreeMap<&str, u64> = BTreeMap::new();
    for trial in &schedule.trials {
        *per_task
            .entry(trial.task_revision_ref.record_id.as_str())
            .or_default() += 1;
    }
    let family = declaration.comparisons.len();
    let comparisons: Vec<Value> = declaration
        .comparisons
        .iter()
        .map(|c| {
            let mut item = json!({
                "comparison_id": c.comparison_id,
                "metric_id": METRIC_ID,
                "baseline_condition_id": c.baseline_evaluation_id,
                "treatment_condition_id": evaluation_id,
                "design": "blocked_paired",
                "hypothesis": "two_sided",
                "alpha": c.alpha,
                "multiplicity_method": if family > 1 { "bonferroni" } else { "none_predeclared_single_test" },
                "minimum_matched_units": c.minimum_matched_tasks,
            });
            if let (Some(t), Some(map)) = (c.practical_significance_threshold, item.as_object_mut()) {
                map.insert("practical_significance_threshold".into(), json!(t));
            }
            item
        })
        .collect();
    let gates: Vec<String> = declaration
        .comparisons
        .iter()
        .map(|c| {
            format!(
                "{}: {evaluation_id} vs baseline {}, paired by task, two-sided at alpha {}{}, at least {} matched tasks.",
                c.comparison_id,
                c.baseline_evaluation_id,
                c.alpha,
                if family > 1 { format!(" (Bonferroni over {family})") } else { String::new() },
                c.minimum_matched_tasks
            )
        })
        .collect();
    let mut analysis_plan = json!({
        "primary_metric_ids": [METRIC_ID],
        "uncertainty_method": "Wilson score interval; task-clustered bootstrap when tasks repeat; paired bootstrap and McNemar exact test for comparisons (ADR-0020).",
        "missing_data_policy": "Trials without a verdict count as not passed and are reported as missing.",
        "exclusion_policy": "Only runs recorded as invalid are excluded.",
    });
    if let Some(map) = analysis_plan.as_object_mut() {
        if !gates.is_empty() {
            map.insert("decision_gates".into(), json!(gates));
            map.insert("comparisons".into(), json!(comparisons));
        }
    }
    let task_families = if declaration.task_families.is_empty() {
        vec!["unspecified".to_string()]
    } else {
        declaration.task_families.clone()
    };
    let body = json!({
        "record_id": plan_id,
        "record_type": "evaluation_plan",
        "schema_version": SCHEMA_VERSION,
        "plan_key": plan_id,
        "revision": 1,
        "lifecycle_state": "frozen",
        "decision_question": declaration.decision_question,
        "workload_scope": {
            "description": declaration.decision_question,
            "task_families": task_families,
            "operating_conditions": declaration.operating_conditions,
        },
        "system_fingerprint_refs": [fingerprint],
        "benchmark_revision_ref": {"record_id": benchmark, "record_type": "benchmark_revision"},
        "conditions": [{
            "condition_id": evaluation_id,
            "role": if family > 0 { "treatment" } else { "comparison" },
            "system_fingerprint_ref": fingerprint,
        }],
        "sampling": {
            "sampling_unit": "task_condition_replicate",
            "task_selection_method": declaration.task_selection_method,
            "planned_trial_count": schedule.trials.len(),
            "replicates_per_task_condition": per_task.values().copied().max().unwrap_or(1),
            "randomization_method": "none",
            "concurrency": schedule.max_active_runs.max(1),
        },
        "retry_policy": {
            "max_run_attempts_per_trial": schedule.retry_policy.max_attempts_per_trial.max(1),
            "retryable_reasons": schedule.retry_policy.retryable_execution_failure_codes,
            "eligible_attempt_selection": "last_valid_attempt",
            "replay_policy": "prohibited",
        },
        "metrics": [{
            "metric_id": METRIC_ID,
            "role": "primary",
            "definition_ref": {"record_id": METRIC_RECORD_ID, "record_type": "metric_definition"},
            "aggregation": "proportion",
        }],
        "analysis_plan": analysis_plan,
    });
    let content_digest = digest(&body);
    if let Some(existing) = meta.get_record(&plan_id)? {
        let frozen_at = existing["frozen_at"].as_str().unwrap_or("").to_string();
        if existing["content_digest"] == content_digest.as_str() {
            return Ok(FreezeOutcome::AlreadyFrozen { plan_id, frozen_at });
        }
        return Err(StoreError::InvalidRecord(format!(
            "plan {plan_id} was frozen at {frozen_at} with different content; a frozen plan cannot change, so give a changed plan a new plan_ref (and evaluation_ref)"
        )));
    }
    if meta.get_record(METRIC_RECORD_ID)?.is_none() {
        meta.put_record(&metric_definition())?;
    }
    let frozen_at = now();
    let mut record = body;
    if let Some(map) = record.as_object_mut() {
        map.insert("created_at".into(), json!(frozen_at));
        map.insert("provenance".into(), provenance(&evaluation_id));
        map.insert("frozen_at".into(), json!(frozen_at));
        map.insert("content_digest".into(), json!(content_digest));
    }
    meta.put_record(&record)?;
    Ok(FreezeOutcome::Frozen { plan_id, frozen_at })
}

fn metric_definition() -> Value {
    json!({
        "record_id": METRIC_RECORD_ID,
        "record_type": "metric_definition",
        "schema_version": SCHEMA_VERSION,
        "created_at": "2026-11-19T00:00:00Z",
        "provenance": {
            "source": {
                "system": "cybercore-agent-evaluation",
                "component": COMPONENT,
                "component_version": "1",
                "origin_id": METRIC_ID
            },
            "captured_by": COMPONENT,
            "capture_version": "1"
        },
        "metric_id": METRIC_ID,
        "revision": 1,
        "lifecycle_state": "active",
        "name": "Workbench pass rate",
        "description": "Proportion of planned trials whose latest attempt was independently verified as pass.",
        "metric_kind": "binary_rate",
        "unit": "proportion",
        "value_direction": "higher_is_better",
        "sampling_unit": "planned_trial",
        "aggregation_level": "condition",
        "estimator": "proportion",
        "numerator_definition": "Trials whose latest attempt's run_outcome is pass.",
        "denominator_definition": "Trials whose latest attempt is pass, fail, inconclusive or not_scored; invalid runs are excluded and trials not run yet are pending.",
        "eligibility_rule": ELIGIBILITY_POLICY,
        "missing_data_policy": "Trials without a verdict (inconclusive or not scored) count as not passed; their number is reported as missing_trial_count.",
        "uncertainty_policy": {
            "required_for_inference": true,
            "method": "wilson",
            "confidence_level": CONFIDENCE_LEVEL,
            "resampling_unit": "trial",
            "minimum_observations": MIN_OBSERVATIONS
        }
    })
}

fn population_key(outcomes: &EvaluationOutcomes) -> String {
    digest(&json!({
        "benchmark": outcomes.benchmark_digest,
        "population": outcomes.task_population_digest,
        "aggregation_level": "condition",
    }))
}

fn provenance(evaluation_id: &str) -> Value {
    json!({
        "source": {
            "system": "cybercore-agent-evaluation",
            "component": COMPONENT,
            "component_version": env!("CARGO_PKG_VERSION"),
            "origin_id": evaluation_id
        },
        "parent_records": [evaluation_ref(evaluation_id)],
        "captured_by": COMPONENT,
        "capture_version": env!("CARGO_PKG_VERSION")
    })
}

fn evaluation_ref(evaluation_id: &str) -> Value {
    json!({"record_id": evaluation_id, "record_type": "evaluation"})
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn digest(value: &Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

fn seed_from(hex: &str) -> u64 {
    u64::from_str_radix(&hex[..16.min(hex.len())], 16).unwrap_or(0x9e37_79b9_7f4a_7c15)
}

/// Wilson score interval for `successes` out of `trials` (95%).
pub fn wilson(successes: u64, trials: u64) -> (f64, f64) {
    if trials == 0 {
        return (0.0, 1.0);
    }
    let n = trials as f64;
    let p = successes as f64 / n;
    let z2 = Z_95 * Z_95;
    let centre = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let half = Z_95 * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt() / (1.0 + z2 / n);
    // At 0 and n successes the bounds are exactly 0 and 1; avoid float residue.
    let lower = if successes == 0 {
        0.0
    } else {
        (centre - half).max(0.0)
    };
    let upper = if successes == trials {
        1.0
    } else {
        (centre + half).min(1.0)
    };
    (lower, upper)
}

/// Two-sided exact McNemar test: a binomial test on the discordant pairs.
pub fn mcnemar_exact(only_baseline: u64, only_treatment: u64) -> f64 {
    let n = only_baseline + only_treatment;
    if n == 0 {
        return 1.0;
    }
    let k = only_baseline.min(only_treatment);
    // Sum P(X <= k) for X ~ Binomial(n, 0.5) in log space.
    let mut log_pmf = n as f64 * 0.5_f64.ln();
    let mut tail = log_pmf.exp();
    for i in 0..k {
        log_pmf += ((n - i) as f64).ln() - ((i + 1) as f64).ln();
        tail += log_pmf.exp();
    }
    (2.0 * tail).min(1.0)
}

/// splitmix64: small, deterministic, good enough for resampling indices.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn percentile_interval(samples: Vec<f64>) -> (f64, f64) {
    percentile_interval_at(samples, CONFIDENCE_LEVEL)
}

fn percentile_interval_at(mut samples: Vec<f64>, confidence: f64) -> (f64, f64) {
    let tail = (1.0 - confidence) / 2.0;
    samples.sort_by(f64::total_cmp);
    let at = |q: f64| {
        samples[((q * (samples.len() - 1) as f64).round() as usize).min(samples.len() - 1)]
    };
    (at(tail), at(1.0 - tail))
}

/// Percentile bootstrap of a ratio of sums, resampling (passes, n) clusters.
pub fn cluster_bootstrap(clusters: &[(u64, u64)], seed: u64) -> (f64, f64) {
    let mut rng = Rng(seed);
    let samples = (0..BOOTSTRAP_REPLICATES)
        .map(|_| {
            let (mut passes, mut n) = (0, 0);
            for _ in 0..clusters.len() {
                let (p, c) = clusters[rng.below(clusters.len())];
                passes += p;
                n += c;
            }
            passes as f64 / n.max(1) as f64
        })
        .collect();
    percentile_interval(samples)
}

/// Percentile bootstrap of the paired difference, resampling tasks (95%).
pub fn paired_bootstrap(pairs: &[TaskPair], seed: u64) -> (f64, f64) {
    paired_bootstrap_at(pairs, seed, CONFIDENCE_LEVEL)
}

/// Percentile bootstrap of the paired difference at a given confidence level.
pub fn paired_bootstrap_at(pairs: &[TaskPair], seed: u64, confidence: f64) -> (f64, f64) {
    let mut rng = Rng(seed);
    let samples = (0..BOOTSTRAP_REPLICATES)
        .map(|_| {
            let (mut bp, mut bn, mut tp, mut tn) = (0, 0, 0, 0);
            for _ in 0..pairs.len() {
                let ((b_pass, b_n), (t_pass, t_n)) = pairs[rng.below(pairs.len())];
                bp += b_pass;
                bn += b_n;
                tp += t_pass;
                tn += t_n;
            }
            tp as f64 / tn.max(1) as f64 - bp as f64 / bn.max(1) as f64
        })
        .collect();
    percentile_interval_at(samples, confidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 5e-4
    }

    #[test]
    fn wilson_matches_reference_values() {
        let (l, u) = wilson(2, 3);
        assert!(close(l, 0.2077) && close(u, 0.9385), "{l} {u}");
        let (l, u) = wilson(0, 10);
        assert!(l == 0.0 && close(u, 0.2775), "{u}");
        let (l, u) = wilson(10, 10);
        assert!(close(l, 0.7225) && u == 1.0, "{l}");
        let (l, u) = wilson(45, 100);
        assert!(close(l, 0.3561) && close(u, 0.5476), "{l} {u}");
    }

    #[test]
    fn mcnemar_matches_reference_values() {
        assert!(close(mcnemar_exact(5, 0), 0.0625));
        assert!(close(mcnemar_exact(3, 3), 1.0));
        assert!(close(mcnemar_exact(2, 10), 158.0 / 4096.0));
        assert_eq!(mcnemar_exact(0, 0), 1.0);
        assert!(mcnemar_exact(0, 2000) < 1e-300 || mcnemar_exact(0, 2000) == 0.0);
    }

    #[test]
    fn bootstraps_are_deterministic_and_sensible() {
        let clusters = vec![(3, 3), (0, 3), (2, 3), (1, 3), (3, 3)];
        let a = cluster_bootstrap(&clusters, 7);
        assert_eq!(a, cluster_bootstrap(&clusters, 7));
        assert!(
            a.0 <= 9.0 / 15.0 && 9.0 / 15.0 <= a.1 && a.0 >= 0.0 && a.1 <= 1.0,
            "{a:?}"
        );
        // Clustering widens the interval compared with treating 15 trials as independent.
        let (wl, wu) = wilson(9, 15);
        assert!(a.1 - a.0 >= (wu - wl) * 0.9, "{a:?} vs {wl} {wu}");

        let same: Vec<_> = (0..20).map(|i| ((i % 2, 1), (i % 2, 1))).collect();
        assert_eq!(paired_bootstrap(&same, 1), (0.0, 0.0));
        let better: Vec<_> = (0..20).map(|_| ((0, 1), (1, 1))).collect();
        assert_eq!(paired_bootstrap(&better, 1), (1.0, 1.0));
    }

    fn outcomes(id: &str, results: &[(&str, TrialOutcome)]) -> EvaluationOutcomes {
        EvaluationOutcomes {
            evaluation_id: id.into(),
            trials: results
                .iter()
                .enumerate()
                .map(|(i, (task, outcome))| TrialResult {
                    trial_id: format!("T{i}"),
                    task_id: task.to_string(),
                    outcome: *outcome,
                    attempts: u64::from(*outcome != TrialOutcome::NotRun),
                })
                .collect(),
            plan_digest: "p".repeat(64),
            benchmark_ref: json!({"record_id": "BENCH", "record_type": "benchmark_revision"}),
            benchmark_digest: "b".repeat(64),
            task_population_digest: "a".repeat(64),
            system_fingerprint_digest: "f".repeat(64),
            plan_id: format!("PLAN-{id}"),
            frozen_plan: None,
            first_result_at: None,
        }
    }

    #[test]
    fn missing_verdicts_count_as_not_passed() {
        use TrialOutcome::*;
        let o = outcomes(
            "E",
            &[
                ("t1", Pass),
                ("t2", Fail),
                ("t3", NoVerdict),
                ("t4", Invalid),
                ("t5", NotRun),
            ],
        );
        let rate = pass_rate(&o);
        assert_eq!(
            (
                rate.passes,
                rate.denominator,
                rate.without_verdict,
                rate.excluded,
                rate.not_run
            ),
            (1, 3, 1, 1, 1)
        );
        assert_eq!(rate.estimate, Some(1.0 / 3.0));
        assert_eq!(rate.interval.map(|i| i.2), Some("wilson"));
        assert_eq!(rate.status, "descriptive_only");
        assert!(rate
            .limitations
            .iter()
            .any(|l| l.contains("count as not passed")));

        let few: Vec<_> = (0..12)
            .map(|i| (if i < 6 { "a" } else { "b" }, Pass))
            .collect();
        let few = pass_rate(&outcomes("E", &few));
        assert_eq!((few.interval, few.status), (None, "descriptive_only"));
        assert!(few.limitations.iter().any(|l| l.contains("no interval")));

        let empty = pass_rate(&outcomes("E", &[("t1", NotRun)]));
        assert_eq!((empty.status, empty.estimate), ("insufficient_data", None));

        let many: Vec<_> = (0..12)
            .map(|i| {
                (
                    ["a", "b", "c", "d", "e", "f"][i / 2],
                    if i % 3 == 0 { Fail } else { Pass },
                )
            })
            .collect();
        let clustered = pass_rate(&outcomes("E", &many));
        assert_eq!(clustered.status, "computed");
        assert_eq!(clustered.interval.map(|i| i.2), Some("cluster_bootstrap"));
    }

    #[test]
    fn comparisons_pair_by_task_and_refuse_different_tasks() {
        use TrialOutcome::*;
        let tasks: Vec<String> = (0..12).map(|i| format!("t{i}")).collect();
        let base: Vec<_> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), if i < 4 { Pass } else { Fail }))
            .collect();
        let treat: Vec<_> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), if i < 11 { Pass } else { Fail }))
            .collect();
        let (b, t) = (outcomes("BASE", &base), outcomes("TREAT", &treat));
        let c = compare(&b, &pass_rate(&b), &t, &pass_rate(&t));
        assert_eq!(
            (c.status, c.design, c.matched_tasks),
            ("comparable", "blocked_paired", 12)
        );
        assert!(close(c.difference.unwrap(), 7.0 / 12.0));
        assert!(close(c.p_value.unwrap(), mcnemar_exact(0, 7)));
        assert_eq!(c.direction, "favors_treatment");

        let mut other = outcomes("OTHER", &treat);
        other.task_population_digest = "c".repeat(64);
        let n = compare(&b, &pass_rate(&b), &other, &pass_rate(&other));
        assert_eq!((n.status, n.difference), ("not_comparable", None));
        assert_eq!(n.mismatches, vec!["task_population".to_string()]);
    }

    fn plan(comparisons: Value, frozen_at: &str) -> Value {
        json!({
            "record_type": "evaluation_plan",
            "lifecycle_state": "frozen",
            "frozen_at": frozen_at,
            "content_digest": "d".repeat(64),
            "analysis_plan": {"comparisons": comparisons},
        })
    }

    fn declared(
        id: &str,
        alpha: f64,
        multiplicity: &str,
        minimum: u64,
        threshold: Option<f64>,
    ) -> Value {
        let mut item = json!({
            "comparison_id": id,
            "metric_id": METRIC_ID,
            "baseline_condition_id": "BASE",
            "treatment_condition_id": "TREAT",
            "design": "blocked_paired",
            "hypothesis": "two_sided",
            "alpha": alpha,
            "multiplicity_method": multiplicity,
            "minimum_matched_units": minimum,
        });
        if let Some(t) = threshold {
            item["practical_significance_threshold"] = json!(t);
        }
        item
    }

    #[test]
    fn predeclared_comparisons_are_confirmatory_only_when_frozen_first() {
        use TrialOutcome::*;
        let tasks: Vec<String> = (0..12).map(|i| format!("t{i}")).collect();
        let base: Vec<_> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), if i < 4 { Pass } else { Fail }))
            .collect();
        let treat: Vec<_> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), if i < 11 { Pass } else { Fail }))
            .collect();
        let b = outcomes("BASE", &base);
        let after = |t: &str| Some(DateTime::parse_from_rfc3339(t).unwrap().with_timezone(&Utc));
        let run = |plan_value: Value, first: &str| {
            let mut t = outcomes("TREAT", &treat);
            t.frozen_plan = Some(plan_value);
            t.first_result_at = after(first);
            compare(&b, &pass_rate(&b), &t, &pass_rate(&t))
        };

        // Frozen before the first treatment result: confirmatory, declared rule applied.
        let single = json!([declared(
            "b-vs-a",
            0.05,
            "none_predeclared_single_test",
            10,
            Some(0.2)
        )]);
        let c = run(
            plan(single.clone(), "2026-11-19T10:00:00.000Z"),
            "2026-11-19T10:05:00.000Z",
        );
        assert_eq!(c.inferential_status, "confirmatory", "{:?}", c.limitations);
        assert_eq!(c.declared_comparison_id.as_deref(), Some("b-vs-a"));
        assert_eq!(c.direction, "favors_treatment");
        assert!((c.confidence_level - 0.95).abs() < 1e-12);
        assert_eq!(c.practical_significance, "meaningful");
        assert!(c
            .limitations
            .iter()
            .any(|l| l.contains("McNemar exact test at level 0.0500")));

        // Frozen after results already existed: stays exploratory and says why.
        let late = run(
            plan(single, "2026-11-19T10:10:00.000Z"),
            "2026-11-19T10:05:00.000Z",
        );
        assert_eq!(late.inferential_status, "exploratory");
        assert!(
            late.limitations
                .iter()
                .any(|l| l.contains("already had results")),
            "{:?}",
            late.limitations
        );

        // Declared minimum not met.
        let strict = json!([declared(
            "b-vs-a",
            0.05,
            "none_predeclared_single_test",
            50,
            None
        )]);
        let few = run(
            plan(strict, "2026-11-19T10:00:00.000Z"),
            "2026-11-19T10:05:00.000Z",
        );
        assert_eq!(few.inferential_status, "exploratory");
        assert!(few
            .limitations
            .iter()
            .any(|l| l.contains("declared minimum of 50")));

        // Two declared comparisons: Bonferroni halves alpha; p = 0.0156 still passes 0.025.
        let two = json!([
            declared("b-vs-a", 0.05, "bonferroni", 10, None),
            {"comparison_id": "c-vs-a", "metric_id": METRIC_ID, "baseline_condition_id": "BASE", "treatment_condition_id": "OTHER", "design": "blocked_paired", "hypothesis": "two_sided", "alpha": 0.05, "multiplicity_method": "bonferroni", "minimum_matched_units": 10}
        ]);
        let c = run(
            plan(two, "2026-11-19T10:00:00.000Z"),
            "2026-11-19T10:05:00.000Z",
        );
        assert_eq!(
            (c.inferential_status, c.family_size, c.multiplicity_method),
            ("confirmatory", 2, "bonferroni")
        );
        assert!((c.confidence_level - 0.975).abs() < 1e-12);
        assert_eq!(c.direction, "favors_treatment");

        // A stricter alpha the data cannot meet: confirmatory, but no direction.
        let tight = json!([declared(
            "b-vs-a",
            0.01,
            "none_predeclared_single_test",
            10,
            None
        )]);
        let c = run(
            plan(tight, "2026-11-19T10:00:00.000Z"),
            "2026-11-19T10:05:00.000Z",
        );
        assert_eq!(
            (c.inferential_status, c.direction),
            ("confirmatory", "undetermined")
        );
    }
}
