//! `workbench failures` (ADR-0027): which checks fail, on which tasks, and
//! why runs ended without a verdict.

use crate::summary::open;
use cybercore_evaluation_store::analysis::{
    analyse_failures, group_json, record_failures, EvaluationOutcomes, FailureAnalysis,
    FailureGroup, FailureKind,
};
use cybercore_evaluation_store::LocalStore;
use serde_json::json;
use std::path::Path;

/// Analyses and records an evaluation's failures.
pub(crate) fn analyse(
    store: &LocalStore,
    evaluation_id: &str,
) -> Result<(EvaluationOutcomes, FailureAnalysis, Vec<String>), String> {
    let outcomes = EvaluationOutcomes::load(store, evaluation_id).map_err(|e| e.to_string())?;
    let analysis = analyse_failures(store, &outcomes).map_err(|e| e.to_string())?;
    let ids = record_failures(store, &analysis).map_err(|e| e.to_string())?;
    Ok((outcomes, analysis, ids))
}

/// One line per group, for people: what failed, how often, where.
pub(crate) fn group_lines(group: &FailureGroup, analysis: &FailureAnalysis) -> Vec<String> {
    let tasks: Vec<&str> = group
        .tasks()
        .iter()
        .map(|t| analysis.tasks.get(*t).map_or(*t, |(key, _)| key.as_str()))
        .collect();
    let count = match group.kind {
        FailureKind::Criterion => format!(
            "failed {} of {} trial(s)",
            group.occurrences.len(),
            group.judged
        ),
        FailureKind::NoVerdict => format!(
            "no verdict on {} of {} trial(s)",
            group.occurrences.len(),
            group.judged
        ),
        FailureKind::RunError => {
            format!("{} run(s) ended without a verdict", group.occurrences.len())
        }
    };
    let mut lines = vec![format!(
        "{}: {count} · {} task(s): {}",
        group.key,
        tasks.len(),
        tasks.join(", ")
    )];
    if group.kind != FailureKind::RunError {
        let families: Vec<String> = analysis
            .families(group)
            .into_iter()
            .map(|(family, failing, judged)| format!("{family} {failing}/{judged}"))
            .collect();
        lines.push(format!("families: {}", families.join(", ")));
    }
    if let Some(requirement) = &group.requirement {
        lines.push(format!("requirement: {requirement}"));
    }
    for rationale in &group.rationales {
        lines.push(format!("judge: {rationale}"));
    }
    lines
}

pub fn failures(store_dir: &Path, evaluation_id: &str, as_json: bool) -> Result<(), String> {
    let store = open(store_dir)?;
    let (_, analysis, ids) = analyse(&store, evaluation_id)?;
    if as_json {
        let out = json!({
            "evaluation_id": evaluation_id,
            "planned_trials": analysis.planned,
            "failing_trials": analysis.failing_trials,
            "groups": analysis.groups.iter().map(|g| group_json(g, &analysis)).collect::<Vec<_>>(),
            "failure_record_ids": ids,
        });
        say!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    say!(
        "{evaluation_id}: {} of {} trial(s) failed or had no verdict (latest attempts)",
        analysis.failing_trials,
        analysis.planned
    );
    if analysis.groups.is_empty() {
        say!("  no failures");
    }
    for group in &analysis.groups {
        say!("");
        for (index, line) in group_lines(group, &analysis).iter().enumerate() {
            say!("  {}{line}", if index == 0 { "" } else { "  " });
        }
    }
    say!("");
    say!("  recorded    {} failure record(s)", ids.len());
    Ok(())
}
