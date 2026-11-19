//! `workbench summary` and `workbench compare` (ADR-0020).

use cybercore_evaluation_store::analysis::{
    compare, compare_resources, pass_rate, record_comparison, record_resource_comparisons,
    record_summary, resource_summaries, Comparison, EvaluationOutcomes, PassRate,
    ResourceComparison, ResourceMetric, ResourceSummary, SummaryRecords, CONFIDENCE_LEVEL,
};
use cybercore_evaluation_store::LocalStore;
use serde_json::{json, Value};
use std::path::Path;

fn open(store_dir: &Path) -> Result<LocalStore, String> {
    if !store_dir.join("metadata.sqlite3").is_file() {
        return Err(format!("no workbench store at {}", store_dir.display()));
    }
    LocalStore::open(store_dir).map_err(|e| format!("{}: {e}", store_dir.display()))
}

fn summarise(
    store: &LocalStore,
    evaluation_id: &str,
) -> Result<(EvaluationOutcomes, PassRate, SummaryRecords), String> {
    let outcomes = EvaluationOutcomes::load(store, evaluation_id).map_err(|e| e.to_string())?;
    let rate = pass_rate(&outcomes);
    let records = record_summary(store, &outcomes, &rate).map_err(|e| e.to_string())?;
    Ok((outcomes, rate, records))
}

fn rate_json(evaluation_id: &str, rate: &PassRate, records: &SummaryRecords) -> Value {
    json!({
        "evaluation_id": evaluation_id,
        "pass_rate": rate.estimate,
        "interval": rate.interval.map(|(lower, upper, method)| json!({
            "lower": lower, "upper": upper, "method": method, "confidence_level": CONFIDENCE_LEVEL,
        })),
        "status": rate.status,
        "passes": rate.passes,
        "denominator": rate.denominator,
        "planned": rate.planned,
        "with_verdict": rate.with_verdict,
        "without_verdict_counted_as_not_passed": rate.without_verdict,
        "excluded": rate.excluded,
        "not_run": rate.not_run,
        "attempts": rate.attempts,
        "limitations": rate.limitations,
        "metric_result_id": records.metric_result_id,
        "analysis_result_id": records.analysis_result_id,
    })
}

fn resources_json(summaries: &[ResourceSummary], records: &SummaryRecords) -> Value {
    summaries
        .iter()
        .zip(&records.resource_metric_result_ids)
        .map(|(s, id)| {
            json!({
                "metric_id": s.metric.id(),
                "unit": s.metric.unit(),
                "estimator": s.metric.estimator(),
                "self_reported": s.metric.self_reported(),
                "estimate": s.estimate,
                "interval": s.interval.map(|(lower, upper)| json!({
                    "lower": lower, "upper": upper, "method": "cluster_bootstrap", "confidence_level": CONFIDENCE_LEVEL,
                })),
                "p90": s.p90,
                "total": s.total,
                "currency": s.currency,
                "status": s.status,
                "eligible": s.eligible,
                "observed": s.observed,
                "missing": s.missing,
                "excluded": s.excluded,
                "not_run": s.not_run,
                "limitations": s.limitations,
                "metric_result_id": id,
            })
        })
        .collect()
}

/// A value in the metric's unit, for people: seconds, tokens or currency.
fn amount(metric: ResourceMetric, value: f64, currency: Option<&str>) -> String {
    match metric {
        ResourceMetric::WallTime if value.abs() >= 1000.0 => format!("{:.2} s", value / 1000.0),
        ResourceMetric::WallTime => format!("{value:.0} ms"),
        ResourceMetric::Tokens => format!("{value:.0} tokens"),
        ResourceMetric::Cost => format!("{:.4} {}", value / 1e6, currency.unwrap_or("")),
    }
}

fn label(metric: ResourceMetric) -> &'static str {
    match metric {
        ResourceMetric::WallTime => "wall time ",
        ResourceMetric::Tokens => "tokens    ",
        ResourceMetric::Cost => "cost      ",
    }
}

fn resource_lines(summaries: &[ResourceSummary]) -> Vec<String> {
    summaries
        .iter()
        .map(|s| {
            let currency = s.currency.as_deref();
            let Some(estimate) = s.estimate else {
                return format!(
                    "  {}  no data ({})",
                    label(s.metric),
                    s.status.replace('_', " ")
                );
            };
            let mut line = format!(
                "  {}  {} {}",
                label(s.metric),
                if s.metric.estimator() == "median" {
                    "median"
                } else {
                    "mean"
                },
                amount(s.metric, estimate, currency)
            );
            if let Some((lower, upper)) = s.interval {
                line.push_str(&format!(
                    "   95% interval {} – {}",
                    amount(s.metric, lower, currency),
                    amount(s.metric, upper, currency)
                ));
            }
            if let Some(p90) = s.p90 {
                line.push_str(&format!("   p90 {}", amount(s.metric, p90, currency)));
            }
            if let Some(total) = s.total {
                line.push_str(&format!("   total {}", amount(s.metric, total, currency)));
            }
            line.push_str(&format!(
                "   ({} of {} trial(s){}; {})",
                s.observed,
                s.eligible,
                if s.metric.self_reported() {
                    ", self-reported"
                } else {
                    ""
                },
                s.status.replace('_', " ")
            ));
            line
        })
        .collect()
}

/// Notes from every summary, each said once, in order.
fn notes<'a>(lists: impl IntoIterator<Item = &'a Vec<String>>) -> Vec<&'a String> {
    let mut seen = std::collections::BTreeSet::new();
    lists
        .into_iter()
        .flatten()
        .filter(|note| seen.insert(note.as_str()))
        .collect()
}

fn say_notes(notes: &[&String]) -> Result<(), String> {
    for (index, note) in notes.iter().enumerate() {
        say!(
            "  {}  {note}",
            if index == 0 {
                "notes     "
            } else {
                "          "
            }
        );
    }
    Ok(())
}

fn rate_text(rate: &PassRate) -> String {
    match rate.estimate {
        None => "no data".into(),
        Some(estimate) => format!("{estimate:.3} ({} of {})", rate.passes, rate.denominator),
    }
}

pub fn summary(store_dir: &Path, evaluation_id: &str, as_json: bool) -> Result<(), String> {
    let store = open(store_dir)?;
    let (outcomes, rate, records) = summarise(&store, evaluation_id)?;
    let resources = resource_summaries(&outcomes);
    if as_json {
        let mut out = rate_json(evaluation_id, &rate, &records);
        if let Some(map) = out.as_object_mut() {
            map.insert(
                "speed_and_cost".into(),
                resources_json(&resources, &records),
            );
        }
        say!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    say!("{evaluation_id}");
    let interval = rate
        .interval
        .map(|(lower, upper, method)| {
            format!(
                "   95% interval {lower:.3}–{upper:.3} ({})",
                method.replace('_', " ")
            )
        })
        .unwrap_or_default();
    say!("  pass rate   {}{interval}", rate_text(&rate));
    say!(
        "  trials      {} planned: {} pass, {} fail, {} without a verdict (counted as not passed), {} excluded, {} not run yet; {} attempt(s)",
        rate.planned,
        rate.passes,
        rate.with_verdict - rate.passes,
        rate.without_verdict,
        rate.excluded,
        rate.not_run,
        rate.attempts
    );
    say!("  status      {}", rate.status.replace('_', " "));
    for line in resource_lines(&resources) {
        say!("{line}");
    }
    say_notes(&notes(
        std::iter::once(&rate.limitations).chain(resources.iter().map(|s| &s.limitations)),
    ))?;
    say!(
        "  recorded    {}, {} and {} speed and cost result(s)",
        records.metric_result_id,
        records.analysis_result_id,
        records.resource_metric_result_ids.len()
    );
    Ok(())
}

fn comparison_json(c: &Comparison, id: &str) -> Value {
    json!({
        "status": c.status,
        "design": c.design,
        "matched_tasks": c.matched_tasks,
        "difference": c.difference,
        "interval": c.interval.map(|(lower, upper)| json!({
            "lower": lower, "upper": upper, "method": "paired_bootstrap", "confidence_level": c.confidence_level,
        })),
        "inferential_status": c.inferential_status,
        "declared_comparison_id": c.declared_comparison_id,
        "multiplicity_method": c.multiplicity_method,
        "family_size": c.family_size,
        "practical_significance": c.practical_significance,
        "practical_significance_threshold": c.practical_significance_threshold,
        "p_value": c.p_value,
        "method": c.method,
        "direction": c.direction,
        "mismatches": c.mismatches,
        "limitations": c.limitations,
        "comparison_result_id": id,
    })
}

pub fn compare_evaluations(
    store_dir: &Path,
    baseline_id: &str,
    treatment_id: &str,
    as_json: bool,
) -> Result<(), String> {
    let store = open(store_dir)?;
    let (base, base_rate, base_records) = summarise(&store, baseline_id)?;
    let (treat, treat_rate, treat_records) = summarise(&store, treatment_id)?;
    let comparison = compare(&base, &base_rate, &treat, &treat_rate);
    let id = record_comparison(
        &store,
        (&base, &base_records),
        (&treat, &treat_records),
        &comparison,
    )
    .map_err(|e| e.to_string())?;
    let resources = compare_resources(&base, &treat);
    let resource_ids = record_resource_comparisons(
        &store,
        (&base, &base_records),
        (&treat, &treat_records),
        &resources,
    )
    .map_err(|e| e.to_string())?;
    if as_json {
        let out = json!({
            "baseline": rate_json(baseline_id, &base_rate, &base_records),
            "treatment": rate_json(treatment_id, &treat_rate, &treat_records),
            "comparison": comparison_json(&comparison, &id),
            "speed_and_cost": resources
                .iter()
                .zip(&resource_ids)
                .map(|(c, id)| resource_comparison_json(c, id))
                .collect::<Vec<_>>(),
        });
        say!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    say!("{baseline_id} (baseline) vs {treatment_id} (treatment)");
    say!("  baseline    {}", rate_text(&base_rate));
    say!("  treatment   {}", rate_text(&treat_rate));
    if let Some(difference) = comparison
        .difference
        .filter(|_| comparison.status != "not_comparable")
    {
        let interval = comparison
            .interval
            .map(|(lower, upper)| {
                let level = comparison.confidence_level * 100.0;
                format!("   {level:.1}% interval {lower:+.3} to {upper:+.3}")
            })
            .unwrap_or_default();
        say!(
            "  difference  {difference:+.3}{interval}   (paired over {} task(s))",
            comparison.matched_tasks
        );
        if let Some(p) = comparison.p_value {
            say!("  McNemar     p = {p:.4}");
        }
        say!("  direction   {}", comparison.direction.replace('_', " "));
        if comparison.practical_significance != "not_assessed" {
            say!(
                "  practical   {}",
                comparison.practical_significance.replace('_', " ")
            );
        }
    }
    let inference = match &comparison.declared_comparison_id {
        Some(id) if comparison.inferential_status == "confirmatory" => {
            format!("confirmatory (predeclared as {id})")
        }
        _ => comparison.inferential_status.replace('_', " "),
    };
    say!("  inference   {inference}");
    say!("  status      {}", comparison.status.replace('_', " "));
    for (index, note) in comparison.limitations.iter().enumerate() {
        say!(
            "  {}  {note}",
            if index == 0 {
                "notes     "
            } else {
                "          "
            }
        );
    }
    say!("  speed and cost, paired by task (lower is better; exploratory)");
    let currency = resource_summaries(&base)
        .into_iter()
        .find_map(|s| s.currency);
    for c in &resources {
        let line = match c.difference.filter(|_| c.status != "not_comparable") {
            None => format!(
                "  {}  {}{}",
                label(c.metric),
                c.status.replace('_', " "),
                if c.mismatches.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", c.mismatches.join(", ").replace('_', " "))
                }
            ),
            Some(difference) => {
                let money = currency.as_deref();
                let mut line = format!(
                    "  {}  {}{} per trial",
                    label(c.metric),
                    if difference > 0.0 { "+" } else { "" },
                    amount(c.metric, difference, money)
                );
                if let Some(relative) = c.relative {
                    line.push_str(&format!(" ({:+.1}%)", relative * 100.0));
                }
                if let Some((lower, upper)) = c.interval {
                    line.push_str(&format!(
                        "   95% interval {} to {}",
                        amount(c.metric, lower, money),
                        amount(c.metric, upper, money)
                    ));
                }
                line.push_str(&format!(
                    "   {}; {} over {} task(s)",
                    c.direction.replace('_', " "),
                    c.status.replace('_', " "),
                    c.matched_tasks
                ));
                line
            }
        };
        say!("{line}");
    }
    say_notes(&notes(resources.iter().map(|c| &c.limitations)))?;
    say!(
        "  recorded    {id} and {} speed and cost comparison(s)",
        resource_ids.len()
    );
    Ok(())
}

fn resource_comparison_json(c: &ResourceComparison, id: &str) -> Value {
    json!({
        "metric_id": c.metric.id(),
        "unit": c.metric.unit(),
        "status": c.status,
        "matched_tasks": c.matched_tasks,
        "difference": c.difference,
        "relative_difference": c.relative,
        "interval": c.interval.map(|(lower, upper)| json!({
            "lower": lower, "upper": upper, "method": "paired_bootstrap", "confidence_level": CONFIDENCE_LEVEL,
        })),
        "direction": c.direction,
        "inferential_status": c.inferential_status,
        "mismatches": c.mismatches,
        "limitations": c.limitations,
        "comparison_result_id": id,
    })
}
