//! `workbench summary` and `workbench compare` (ADR-0020).

use cybercore_evaluation_store::analysis::{
    compare, pass_rate, record_comparison, record_summary, Comparison, EvaluationOutcomes,
    PassRate, SummaryRecords, CONFIDENCE_LEVEL,
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

fn rate_text(rate: &PassRate) -> String {
    match rate.estimate {
        None => "no data".into(),
        Some(estimate) => format!("{estimate:.3} ({} of {})", rate.passes, rate.denominator),
    }
}

pub fn summary(store_dir: &Path, evaluation_id: &str, as_json: bool) -> Result<(), String> {
    let store = open(store_dir)?;
    let (_, rate, records) = summarise(&store, evaluation_id)?;
    if as_json {
        let out = rate_json(evaluation_id, &rate, &records);
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
    for (index, note) in rate.limitations.iter().enumerate() {
        say!(
            "  {}  {note}",
            if index == 0 {
                "notes     "
            } else {
                "          "
            }
        );
    }
    say!(
        "  recorded    {}, {}",
        records.metric_result_id,
        records.analysis_result_id
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
            "lower": lower, "upper": upper, "method": "paired_bootstrap", "confidence_level": CONFIDENCE_LEVEL,
        })),
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
    if as_json {
        let out = json!({
            "baseline": rate_json(baseline_id, &base_rate, &base_records),
            "treatment": rate_json(treatment_id, &treat_rate, &treat_records),
            "comparison": comparison_json(&comparison, &id),
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
            .map(|(lower, upper)| format!("   95% interval {lower:+.3} to {upper:+.3}"))
            .unwrap_or_default();
        say!(
            "  difference  {difference:+.3}{interval}   (paired over {} task(s))",
            comparison.matched_tasks
        );
        if let Some(p) = comparison.p_value {
            say!("  McNemar     p = {p:.4}");
        }
        say!("  direction   {}", comparison.direction.replace('_', " "));
    }
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
    say!("  recorded    {id}");
    Ok(())
}
