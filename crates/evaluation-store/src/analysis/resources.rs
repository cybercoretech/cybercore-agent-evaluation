//! Speed and cost metrics (ADR-0022): per-trial wall time (measured by the
//! runner), and tokens and cost (reported by the agent itself, costed with
//! the evaluation's declared prices).
//!
//! Policy (revision 1 of each metric):
//! - The unit is the planned trial, valued by its latest attempt. Trials
//!   with an outcome (pass, fail or no verdict) are eligible: a failed
//!   attempt still took time and spent tokens. Invalid runs are excluded and
//!   trials not run yet are pending, as for the pass rate.
//! - An eligible trial without a value (no measured wall time, no or an
//!   invalid usage report, or an unpriced model) is missing. Missing values
//!   are never imputed; any missing trial keeps the result descriptive.
//! - Wall time is summarised by its median, tokens and cost by their mean.
//!   The 95% interval is a percentile bootstrap that resamples tasks, from
//!   5 distinct tasks up.
//! - Comparisons pair tasks: the effect is the mean over matched tasks of
//!   (treatment − baseline), with a paired bootstrap interval. Lower is
//!   better. They are always exploratory: this revision cannot predeclare
//!   them.

use super::{
    digest, evaluation_ref, now, percentile_interval, population_key, provenance, seed_from,
    EvaluationOutcomes, ReportedUsage, Rng, SummaryRecords, TrialOutcome, TrialResult,
    ANALYSIS_VERSION, BOOTSTRAP_REPLICATES, COMPONENT, CONFIDENCE_LEVEL, MIN_BOOTSTRAP_TASKS,
    MIN_OBSERVATIONS, SCHEMA_VERSION,
};
use crate::canonical::sha256_hex;
use crate::{LocalStore, StoreResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// A speed or cost metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceMetric {
    WallTime,
    Tokens,
    Cost,
}

impl ResourceMetric {
    pub const ALL: [Self; 3] = [Self::WallTime, Self::Tokens, Self::Cost];

    pub fn id(self) -> &'static str {
        match self {
            Self::WallTime => "workbench-wall-time",
            Self::Tokens => "workbench-reported-tokens",
            Self::Cost => "workbench-reported-cost",
        }
    }

    fn record_id(self) -> &'static str {
        match self {
            Self::WallTime => "METRIC-WORKBENCH-WALL-TIME-R1",
            Self::Tokens => "METRIC-WORKBENCH-REPORTED-TOKENS-R1",
            Self::Cost => "METRIC-WORKBENCH-REPORTED-COST-R1",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::WallTime => "wall-time",
            Self::Tokens => "tokens",
            Self::Cost => "cost",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::WallTime => "milliseconds",
            Self::Tokens => "tokens",
            Self::Cost => "currency micro-units",
        }
    }

    /// `median` or `arithmetic_mean`.
    pub fn estimator(self) -> &'static str {
        match self {
            Self::WallTime => "median",
            Self::Tokens | Self::Cost => "arithmetic_mean",
        }
    }

    pub fn self_reported(self) -> bool {
        self != Self::WallTime
    }

    fn policy(self) -> &'static str {
        match self {
            Self::WallTime => "workbench-wall-time/1: trials with an outcome are eligible, valued by the latest attempt's runner-measured wall time; invalid runs are excluded; trials not run yet are pending; trials without a measurement are missing",
            Self::Tokens => "workbench-reported-tokens/1: trials with an outcome are eligible, valued by the latest attempt's agent-reported input plus output tokens; invalid runs are excluded; trials not run yet are pending; absent or invalid reports are missing",
            Self::Cost => "workbench-reported-cost/1: trials with an outcome are eligible, valued by the latest attempt's agent-reported tokens priced with the evaluation's declared prices; invalid runs are excluded; trials not run yet are pending; absent, invalid or unpriced reports are missing",
        }
    }

    fn value(self, trial: &TrialResult) -> Option<f64> {
        match (self, &trial.usage) {
            (Self::WallTime, _) => trial.wall_time_ms.map(|ms| ms as f64),
            (Self::Tokens, ReportedUsage::Reported { tokens, .. }) => Some(*tokens as f64),
            (
                Self::Cost,
                ReportedUsage::Reported {
                    cost: Some(cost), ..
                },
            ) => Some(cost.microunits as f64),
            _ => None,
        }
    }

    fn estimate(self, values: &mut [f64]) -> f64 {
        match self {
            Self::WallTime => median(values),
            Self::Tokens | Self::Cost => values.iter().sum::<f64>() / values.len() as f64,
        }
    }

    fn definition(self) -> Value {
        let (name, description, kind) = match self {
            Self::WallTime => (
                "Workbench wall time",
                "Median wall time of a planned trial's latest attempt, measured by the runner.",
                "latency",
            ),
            Self::Tokens => (
                "Agent-reported tokens",
                "Mean input plus output tokens per planned trial, as reported by the agent in cybercore-usage.json. Self-reported and unverified.",
                "mean",
            ),
            Self::Cost => (
                "Agent-reported cost",
                "Mean cost per planned trial: agent-reported tokens priced with the evaluation's declared prices, in millionths of the currency unit. Self-reported tokens, so unverified.",
                "cost",
            ),
        };
        json!({
            "record_id": self.record_id(),
            "record_type": "metric_definition",
            "schema_version": SCHEMA_VERSION,
            "created_at": "2026-11-19T00:00:00Z",
            "provenance": {
                "source": {
                    "system": "cybercore-agent-evaluation",
                    "component": COMPONENT,
                    "component_version": "1",
                    "origin_id": self.id()
                },
                "captured_by": COMPONENT,
                "capture_version": "1"
            },
            "metric_id": self.id(),
            "revision": 1,
            "lifecycle_state": "active",
            "name": name,
            "description": description,
            "metric_kind": kind,
            "unit": self.unit(),
            "value_direction": "lower_is_better",
            "sampling_unit": "planned_trial",
            "aggregation_level": "condition",
            "estimator": self.estimator(),
            "eligibility_rule": self.policy(),
            "missing_data_policy": "Eligible trials without a value are reported as missing_trial_count and never imputed; any missing trial makes the result descriptive only.",
            "uncertainty_policy": {
                "required_for_inference": true,
                "method": "cluster_bootstrap",
                "confidence_level": CONFIDENCE_LEVEL,
                "resampling_unit": "task_cluster",
                "minimum_observations": MIN_OBSERVATIONS
            }
        })
    }
}

/// One speed or cost metric for an evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSummary {
    pub metric: ResourceMetric,
    pub planned: u64,
    /// Trials with an outcome (pass, fail or no verdict).
    pub eligible: u64,
    /// Eligible trials with a value.
    pub observed: u64,
    /// Eligible trials without a value.
    pub missing: u64,
    pub excluded: u64,
    pub not_run: u64,
    pub attempts: u64,
    pub estimate: Option<f64>,
    /// 95% cluster-bootstrap interval of the estimate.
    pub interval: Option<(f64, f64)>,
    /// 90th percentile (nearest rank), for wall time.
    pub p90: Option<f64>,
    /// Sum over observed trials, for tokens and cost.
    pub total: Option<f64>,
    /// For cost: the one currency every cost is in.
    pub currency: Option<String>,
    /// For cost: the one price table every cost used.
    pub pricing_digest: Option<String>,
    /// `computed`, `descriptive_only` or `insufficient_data`.
    pub status: &'static str,
    pub limitations: Vec<String>,
}

fn eligible(trial: &TrialResult) -> bool {
    matches!(
        trial.outcome,
        TrialOutcome::Pass | TrialOutcome::Fail | TrialOutcome::NoVerdict
    )
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Nearest-rank percentile of sorted values.
fn nearest_rank(sorted: &[f64], quantile: f64) -> f64 {
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Every speed and cost metric, in [`ResourceMetric::ALL`] order.
pub fn resource_summaries(outcomes: &EvaluationOutcomes) -> Vec<ResourceSummary> {
    ResourceMetric::ALL
        .iter()
        .map(|metric| resource_summary(outcomes, *metric))
        .collect()
}

pub fn resource_summary(outcomes: &EvaluationOutcomes, metric: ResourceMetric) -> ResourceSummary {
    let trials = &outcomes.trials;
    let count = |o: TrialOutcome| trials.iter().filter(|t| t.outcome == o).count() as u64;
    let (excluded, not_run) = (count(TrialOutcome::Invalid), count(TrialOutcome::NotRun));
    let eligible_trials: Vec<&TrialResult> = trials.iter().filter(|t| eligible(t)).collect();
    let mut by_task: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for trial in &eligible_trials {
        if let Some(value) = metric.value(trial) {
            by_task
                .entry(trial.task_id.as_str())
                .or_default()
                .push(value);
        }
    }
    let observed = by_task.values().map(Vec::len).sum::<usize>() as u64;
    let eligible_count = eligible_trials.len() as u64;
    let missing = eligible_count - observed;
    let attempts = trials.iter().map(|t| t.attempts).sum();
    let mut limitations = Vec::new();

    // Costs from different price tables (or currencies) cannot be pooled.
    let price_tables: BTreeSet<(&str, &str)> = eligible_trials
        .iter()
        .filter_map(|t| match &t.usage {
            ReportedUsage::Reported { cost: Some(c), .. } => {
                Some((c.currency.as_str(), c.pricing_digest.as_str()))
            }
            _ => None,
        })
        .collect();
    let mixed_prices = metric == ResourceMetric::Cost && price_tables.len() > 1;
    let (currency, pricing_digest) = match (metric, price_tables.iter().next()) {
        (ResourceMetric::Cost, Some((c, d))) if !mixed_prices => {
            (Some(c.to_string()), Some(d.to_string()))
        }
        _ => (None, None),
    };

    let mut all: Vec<f64> = by_task.values().flatten().copied().collect();
    let pooled = observed > 0 && !mixed_prices;
    let estimate = pooled.then(|| metric.estimate(&mut all.clone()));
    all.sort_by(f64::total_cmp);
    let p90 = (pooled && metric == ResourceMetric::WallTime).then(|| nearest_rank(&all, 0.9));
    let total = (pooled && metric.self_reported()).then(|| all.iter().sum());
    let interval = (pooled && by_task.len() >= MIN_BOOTSTRAP_TASKS).then(|| {
        let clusters: Vec<&Vec<f64>> = by_task.values().collect();
        let seed = seed_from(&sha256_hex(
            format!("{}|{}", outcomes.data_digest(), metric.id()).as_bytes(),
        ));
        cluster_bootstrap_values(&clusters, metric, seed)
    });

    if metric.self_reported() {
        limitations.push("Self-reported: token counts come from the agent's own usage file (cybercore-usage.json) and are not verified by the workbench.".into());
    }
    if metric == ResourceMetric::Cost {
        limitations.push(
            "Cost is reported tokens times the evaluation's declared prices, not a bill.".into(),
        );
    }
    if mixed_prices {
        limitations.push(format!(
            "Costs were computed under {} different price tables or currencies, so they are not pooled.",
            price_tables.len()
        ));
    }
    if missing > 0 {
        limitations.push(match metric {
            ResourceMetric::WallTime => format!(
                "{missing} trial(s) with an outcome have no measured wall time (the runner failed before measuring)."
            ),
            _ => {
                let absent = eligible_trials
                    .iter()
                    .filter(|t| t.usage == ReportedUsage::Absent)
                    .count();
                let invalid = eligible_trials
                    .iter()
                    .filter(|t| t.usage == ReportedUsage::Invalid)
                    .count();
                let mut note = format!(
                    "{missing} trial(s) are missing: {absent} left no usage report and {invalid} left an invalid one"
                );
                if metric == ResourceMetric::Cost {
                    let unpriced = missing as usize - absent - invalid;
                    note.push_str(&format!(
                        "; {unpriced} reported a model without a declared price"
                    ));
                }
                note.push('.');
                note
            }
        });
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
    let attempted = trials.iter().filter(|t| t.attempts > 0).count() as u64;
    if attempts > attempted {
        limitations.push(format!(
            "Only each trial's latest attempt counts; {} earlier attempt(s) are not included.",
            attempts - attempted
        ));
    }
    if observed > 0 && observed < MIN_OBSERVATIONS {
        limitations.push(format!(
            "Only {observed} trial(s) have a value (fewer than {MIN_OBSERVATIONS}); treat the result as descriptive."
        ));
    }
    if pooled && interval.is_none() {
        limitations.push(format!(
            "Only {} distinct task(s) have a value (fewer than {MIN_BOOTSTRAP_TASKS}), so no interval is reported.",
            by_task.len()
        ));
    }
    let status = if !pooled {
        if observed == 0 {
            limitations.push("No trial has a value yet.".into());
        }
        "insufficient_data"
    } else if missing > 0 || not_run > 0 || observed < MIN_OBSERVATIONS || interval.is_none() {
        "descriptive_only"
    } else {
        "computed"
    };
    ResourceSummary {
        metric,
        planned: trials.len() as u64,
        eligible: eligible_count,
        observed,
        missing,
        excluded,
        not_run,
        attempts,
        estimate,
        interval,
        p90,
        total,
        currency,
        pricing_digest,
        status,
        limitations,
    }
}

/// Percentile bootstrap of the metric's estimator, resampling tasks.
fn cluster_bootstrap_values(
    clusters: &[&Vec<f64>],
    metric: ResourceMetric,
    seed: u64,
) -> (f64, f64) {
    let mut rng = Rng(seed);
    let mut sample = Vec::new();
    let replicates = (0..BOOTSTRAP_REPLICATES)
        .map(|_| {
            sample.clear();
            for _ in 0..clusters.len() {
                sample.extend_from_slice(clusters[rng.below(clusters.len())]);
            }
            metric.estimate(&mut sample)
        })
        .collect();
    percentile_interval(replicates)
}

/// Writes each speed and cost `metric_result` (once; IDs derive from the
/// data) and returns their IDs in [`ResourceMetric::ALL`] order.
pub(super) fn record_resource_results(
    store: &LocalStore,
    outcomes: &EvaluationOutcomes,
    tag: &str,
) -> StoreResult<Vec<String>> {
    let meta = store.metadata();
    let mut ids = Vec::new();
    let mut records = Vec::new();
    for summary in resource_summaries(outcomes) {
        let metric = summary.metric;
        let definition = metric.definition();
        if meta.get_record(metric.record_id())?.is_none() {
            meta.put_record(&definition)?;
        }
        let id = format!("MR-{tag}:{}", metric.suffix());
        if meta.get_record(&id)?.is_none() {
            let now = now();
            let mut result = json!({
                "record_id": id,
                "record_type": "metric_result",
                "schema_version": SCHEMA_VERSION,
                "created_at": now,
                "provenance": provenance(&outcomes.evaluation_id),
                "metric_definition_ref": {"record_id": metric.record_id(), "record_type": "metric_definition"},
                "metric_definition_digest": digest(&definition),
                "evaluation_ref": evaluation_ref(&outcomes.evaluation_id),
                "evaluation_plan_digest": outcomes.plan_digest,
                "benchmark_revision_ref": outcomes.benchmark_ref,
                "benchmark_content_digest": outcomes.benchmark_digest,
                "condition_id": outcomes.evaluation_id,
                "system_fingerprint_digest": outcomes.system_fingerprint_digest,
                "task_population_digest": outcomes.task_population_digest,
                "population_key": population_key(outcomes),
                "aggregation_level": "condition",
                "result_status": summary.status,
                "value_kind": "numeric_summary",
                "counts": {
                    "planned_trial_count": summary.planned,
                    "eligible_trial_count": summary.eligible,
                    "excluded_trial_count": summary.excluded,
                    "observed_trial_count": summary.observed,
                    "missing_trial_count": summary.missing,
                    "run_attempt_count": summary.attempts,
                    "pending_trial_count": summary.not_run,
                },
                "computed_at": now,
                "analysis_version": ANALYSIS_VERSION,
                "eligibility_policy_digest": sha256_hex(metric.policy().as_bytes()),
            });
            if let Some(map) = result.as_object_mut() {
                if let Some(estimate) = summary.estimate {
                    map.insert("estimate".into(), json!(estimate));
                }
                if let Some((lower, upper)) = summary.interval {
                    map.insert(
                        "uncertainty".into(),
                        json!({"method": "cluster_bootstrap", "confidence_level": CONFIDENCE_LEVEL, "lower_bound": lower, "upper_bound": upper}),
                    );
                }
                if !summary.limitations.is_empty() {
                    map.insert("limitations".into(), json!(summary.limitations));
                }
            }
            records.push(result);
        }
        ids.push(id);
    }
    if !records.is_empty() {
        meta.put_records(&records)?;
    }
    Ok(ids)
}

/// A paired comparison of one speed or cost metric.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceComparison {
    pub metric: ResourceMetric,
    /// `comparable`, `not_comparable` or `insufficient_evidence`.
    pub status: &'static str,
    pub matched_tasks: u64,
    /// Mean over matched tasks of (treatment − baseline), in the metric's unit.
    pub difference: Option<f64>,
    /// Treatment total over baseline total on matched tasks, minus 1.
    pub relative: Option<f64>,
    pub interval: Option<(f64, f64)>,
    pub direction: &'static str,
    /// `exploratory`, `descriptive_only` or `prohibited`.
    pub inferential_status: &'static str,
    pub mismatches: Vec<String>,
    pub limitations: Vec<String>,
}

/// Per-task mean value over a metric's eligible trials.
fn task_means(outcomes: &EvaluationOutcomes, metric: ResourceMetric) -> BTreeMap<&str, f64> {
    let mut sums: BTreeMap<&str, (f64, u64)> = BTreeMap::new();
    for trial in outcomes.trials.iter().filter(|t| eligible(t)) {
        if let Some(value) = metric.value(trial) {
            let entry = sums.entry(trial.task_id.as_str()).or_default();
            *entry = (entry.0 + value, entry.1 + 1);
        }
    }
    sums.into_iter()
        .map(|(task, (sum, n))| (task, sum / n as f64))
        .collect()
}

/// Paired comparisons of every speed and cost metric.
pub fn compare_resources(
    baseline: &EvaluationOutcomes,
    treatment: &EvaluationOutcomes,
) -> Vec<ResourceComparison> {
    ResourceMetric::ALL
        .iter()
        .map(|metric| compare_resource(baseline, treatment, *metric))
        .collect()
}

fn compare_resource(
    baseline: &EvaluationOutcomes,
    treatment: &EvaluationOutcomes,
    metric: ResourceMetric,
) -> ResourceComparison {
    let base_summary = resource_summary(baseline, metric);
    let treat_summary = resource_summary(treatment, metric);
    let mut mismatches = Vec::new();
    if baseline.task_population_digest != treatment.task_population_digest {
        mismatches.push("task_population".to_string());
    }
    if baseline.benchmark_digest != treatment.benchmark_digest {
        mismatches.push("benchmark_content".to_string());
    }
    if metric == ResourceMetric::Cost
        && base_summary.pricing_digest.is_some()
        && treat_summary.pricing_digest.is_some()
        && base_summary.pricing_digest != treat_summary.pricing_digest
    {
        mismatches.push("pricing".to_string());
    }
    let mut limitations = vec![
        "Exploratory: speed and cost comparisons cannot be predeclared in this revision."
            .to_string(),
    ];
    if metric.self_reported() {
        limitations.push("Self-reported: both sides' token counts come from the agents' own usage files and are not verified.".into());
    }
    if !mismatches.is_empty() {
        limitations.push(if mismatches == ["pricing"] {
            "The evaluations priced tokens with different price tables, so their costs are not compared.".into()
        } else {
            "The evaluations did not run the same task revisions, so they are not compared.".into()
        });
        return ResourceComparison {
            metric,
            status: "not_comparable",
            matched_tasks: 0,
            difference: None,
            relative: None,
            interval: None,
            direction: "undetermined",
            inferential_status: "prohibited",
            mismatches,
            limitations,
        };
    }
    let base = task_means(baseline, metric);
    let treat = task_means(treatment, metric);
    let pairs: Vec<(f64, f64)> = base
        .iter()
        .filter_map(|(task, b)| treat.get(task).map(|t| (*b, *t)))
        .collect();
    let difference = (!pairs.is_empty())
        .then(|| pairs.iter().map(|(b, t)| t - b).sum::<f64>() / pairs.len() as f64);
    let base_total: f64 = pairs.iter().map(|(b, _)| b).sum();
    let relative =
        (base_total > 0.0).then(|| pairs.iter().map(|(_, t)| t).sum::<f64>() / base_total - 1.0);
    let interval = (pairs.len() >= MIN_BOOTSTRAP_TASKS).then(|| {
        let seed = seed_from(&sha256_hex(
            format!(
                "{}|{}|{}",
                baseline.data_digest(),
                treatment.data_digest(),
                metric.id()
            )
            .as_bytes(),
        ));
        paired_mean_bootstrap(&pairs, seed)
    });
    // Lower is better: a difference entirely below 0 favours the treatment.
    let direction = match interval {
        Some((_, upper)) if upper < 0.0 => "favors_treatment",
        Some((lower, _)) if lower > 0.0 => "favors_baseline",
        _ => "undetermined",
    };
    if !pairs.is_empty() && pairs.len() < MIN_BOOTSTRAP_TASKS {
        limitations.push(format!(
            "Only {} task(s) have a value on both sides (fewer than {MIN_BOOTSTRAP_TASKS}), so no interval is reported.",
            pairs.len()
        ));
    }
    if base.len() != pairs.len() || treat.len() != pairs.len() {
        limitations.push(
            "Some tasks have a value in only one evaluation; they are left out of the pairing."
                .into(),
        );
    }
    let ready = base_summary.status == "computed" && treat_summary.status == "computed";
    if !ready {
        limitations.push(format!(
            "At least one evaluation's {} result is descriptive only or has no data.",
            metric.suffix().replace('-', " ")
        ));
    }
    let status = if ready && interval.is_some() {
        "comparable"
    } else {
        "insufficient_evidence"
    };
    ResourceComparison {
        metric,
        status,
        matched_tasks: pairs.len() as u64,
        difference,
        relative,
        interval,
        direction,
        inferential_status: if status == "comparable" {
            "exploratory"
        } else {
            "descriptive_only"
        },
        mismatches,
        limitations,
    }
}

/// Percentile bootstrap of the mean paired difference, resampling tasks.
fn paired_mean_bootstrap(pairs: &[(f64, f64)], seed: u64) -> (f64, f64) {
    let mut rng = Rng(seed);
    let replicates = (0..BOOTSTRAP_REPLICATES)
        .map(|_| {
            let mut sum = 0.0;
            for _ in 0..pairs.len() {
                let (b, t) = pairs[rng.below(pairs.len())];
                sum += t - b;
            }
            sum / pairs.len() as f64
        })
        .collect();
    percentile_interval(replicates)
}

/// Writes a `comparison_result` per speed and cost metric (once per pair of
/// metric results) and returns their IDs in [`ResourceMetric::ALL`] order.
pub fn record_resource_comparisons(
    store: &LocalStore,
    baseline: (&EvaluationOutcomes, &SummaryRecords),
    treatment: (&EvaluationOutcomes, &SummaryRecords),
    comparisons: &[ResourceComparison],
) -> StoreResult<Vec<String>> {
    let meta = store.metadata();
    let mut ids = Vec::new();
    for (index, comparison) in comparisons.iter().enumerate() {
        let metric = comparison.metric;
        let (Some(base_result), Some(treat_result)) = (
            baseline.1.resource_metric_result_ids.get(index),
            treatment.1.resource_metric_result_ids.get(index),
        ) else {
            continue;
        };
        let tag = sha256_hex(format!("{base_result}|{treat_result}|{ANALYSIS_VERSION}").as_bytes());
        let id = format!("CR-{}", &tag[..24]);
        ids.push(id.clone());
        if meta.get_record(&id)?.is_some() {
            continue;
        }
        let definition = metric.definition();
        let now = now();
        let mut record = json!({
            "record_id": id,
            "record_type": "comparison_result",
            "schema_version": SCHEMA_VERSION,
            "created_at": now,
            "provenance": provenance(&treatment.0.evaluation_id),
            "metric_definition_ref": {"record_id": metric.record_id(), "record_type": "metric_definition"},
            "metric_definition_digest": digest(&definition),
            "baseline_metric_result_ref": {"record_id": base_result, "record_type": "metric_result"},
            "treatment_metric_result_ref": {"record_id": treat_result, "record_type": "metric_result"},
            "baseline_condition_id": baseline.0.evaluation_id,
            "treatment_condition_id": treatment.0.evaluation_id,
            "comparison_status": comparison.status,
            "comparison_design": if comparison.status == "not_comparable" { "observational_unpaired" } else { "blocked_paired" },
            "comparability": {
                "benchmark_content_digest": treatment.0.benchmark_digest,
                "task_population_digest": treatment.0.task_population_digest,
                "metric_definition_digest": digest(&definition),
                "eligibility_policy_digest": sha256_hex(metric.policy().as_bytes()),
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
                "unit": format!("{} per trial (treatment − baseline)", metric.unit()),
                "direction_interpretation": comparison.direction,
            });
            if let (Some((lower, upper)), Some(effect)) =
                (comparison.interval, effect.as_object_mut())
            {
                effect.insert(
                    "interval".into(),
                    json!({"method": "paired_bootstrap", "confidence_level": CONFIDENCE_LEVEL, "lower_bound": lower, "upper_bound": upper}),
                );
            }
            map.insert("effect".into(), effect);
            map.insert(
                "inference".into(),
                json!({
                    "method": "paired bootstrap over tasks (mean difference)",
                    "multiplicity_method": "none_predeclared_single_test",
                    "comparison_family_size": 1,
                    "practical_significance": "not_assessed",
                }),
            );
        }
        meta.put_record(&record)?;
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::super::ReportedCost;
    use super::*;

    fn trial(
        task: &str,
        outcome: TrialOutcome,
        ms: Option<u64>,
        usage: ReportedUsage,
    ) -> TrialResult {
        TrialResult {
            trial_id: format!("T-{task}-{ms:?}"),
            task_id: task.into(),
            outcome,
            attempts: u64::from(outcome != TrialOutcome::NotRun),
            wall_time_ms: ms,
            usage,
            latest_run_id: None,
        }
    }

    fn reported(tokens: u64, cost: u64, digest: &str) -> ReportedUsage {
        ReportedUsage::Reported {
            tokens,
            cost: Some(ReportedCost {
                microunits: cost,
                currency: "USD".into(),
                pricing_digest: digest.repeat(64),
            }),
        }
    }

    fn outcomes(id: &str, trials: Vec<TrialResult>) -> EvaluationOutcomes {
        EvaluationOutcomes {
            evaluation_id: id.into(),
            trials,
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

    fn twelve(id: &str, scale: u64) -> EvaluationOutcomes {
        outcomes(
            id,
            (0..12)
                .map(|i| {
                    trial(
                        &format!("TASK-{i:02}"),
                        if i % 3 == 0 {
                            TrialOutcome::Fail
                        } else {
                            TrialOutcome::Pass
                        },
                        Some((1000 + i * 100) * scale),
                        reported(
                            (10_000 + i * 1000) * scale,
                            (50_000 + i * 5000) * scale,
                            "a",
                        ),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn complete_data_gives_computed_summaries() {
        let summaries = resource_summaries(&twelve("E", 1));
        let time = &summaries[0];
        assert_eq!(time.status, "computed", "{:?}", time.limitations);
        // Wall times 1000..2100 by 100: the median is (1500 + 1600) / 2.
        assert_eq!(time.estimate, Some(1550.0));
        assert_eq!(time.p90, Some(2000.0));
        let (lower, upper) = time.interval.unwrap();
        assert!(lower <= 1550.0 && 1550.0 <= upper);
        let tokens = &summaries[1];
        assert_eq!(tokens.status, "computed");
        assert_eq!(tokens.estimate, Some(15_500.0));
        assert_eq!(tokens.total, Some(186_000.0));
        let cost = &summaries[2];
        assert_eq!(cost.status, "computed");
        assert_eq!(cost.currency.as_deref(), Some("USD"));
        assert!(cost.limitations.iter().any(|l| l.contains("Self-reported")));
    }

    #[test]
    fn missing_reports_keep_results_descriptive_and_are_counted() {
        let mut data = twelve("E", 1);
        data.trials[0].usage = ReportedUsage::Absent;
        data.trials[1].usage = ReportedUsage::Invalid;
        data.trials[2].usage = ReportedUsage::Reported {
            tokens: 5,
            cost: None,
        };
        data.trials[3].outcome = TrialOutcome::NotRun;
        data.trials[3].attempts = 0;
        let summaries = resource_summaries(&data);
        assert_eq!(summaries[0].status, "descriptive_only");
        let tokens = &summaries[1];
        assert_eq!(
            (
                tokens.eligible,
                tokens.observed,
                tokens.missing,
                tokens.not_run
            ),
            (11, 9, 2, 1)
        );
        assert_eq!(
            tokens.eligible + tokens.excluded + tokens.not_run,
            tokens.planned
        );
        assert_eq!(tokens.status, "descriptive_only");
        let cost = &summaries[2];
        assert_eq!((cost.observed, cost.missing), (8, 3));
        assert!(cost
            .limitations
            .iter()
            .any(|l| l.contains("1 reported a model without a declared price")));
    }

    #[test]
    fn mixed_price_tables_are_not_pooled() {
        let mut data = twelve("E", 1);
        data.trials[0].usage = reported(1, 1, "b");
        let cost = resource_summary(&data, ResourceMetric::Cost);
        assert_eq!(cost.status, "insufficient_data");
        assert_eq!(cost.estimate, None);
        // Tokens are unaffected by prices.
        assert_eq!(
            resource_summary(&data, ResourceMetric::Tokens).status,
            "computed"
        );
    }

    #[test]
    fn a_faster_cheaper_treatment_is_favoured() {
        let comparisons = compare_resources(&twelve("A", 2), &twelve("B", 1));
        for comparison in &comparisons {
            assert_eq!(
                comparison.status, "comparable",
                "{:?}",
                comparison.limitations
            );
            assert_eq!(comparison.direction, "favors_treatment");
            assert_eq!(comparison.inferential_status, "exploratory");
            assert_eq!(comparison.matched_tasks, 12);
            assert!((comparison.relative.unwrap() + 0.5).abs() < 1e-9);
        }
        let same = compare_resources(&twelve("A", 1), &twelve("B", 1));
        assert!(same
            .iter()
            .all(|c| c.direction == "undetermined" && c.difference == Some(0.0)));
    }

    #[test]
    fn different_prices_or_tasks_are_not_comparable() {
        let mut other = twelve("B", 1);
        for t in &mut other.trials {
            t.usage = reported(1, 1, "c");
        }
        let comparisons = compare_resources(&twelve("A", 1), &other);
        assert_eq!(comparisons[0].status, "comparable");
        assert_eq!(comparisons[2].status, "not_comparable");
        assert_eq!(comparisons[2].mismatches, ["pricing"]);
        let mut moved = twelve("B", 1);
        moved.task_population_digest = "d".repeat(64);
        assert!(compare_resources(&twelve("A", 1), &moved)
            .iter()
            .all(|c| c.inferential_status == "prohibited"));
    }
}
