//! Failure analysis (ADR-0027): which checks fail, on which tasks, and why
//! runs ended without a verdict, over each trial's latest attempt.
//!
//! Failed criteria (verified `fail`) are grouped by criterion ID; verdicts
//! that are neither pass nor fail are grouped as "no verdict" per criterion;
//! runs that never reached a verifier are grouped by their recorded error.
//! Each verified failure, and each run error with a terminal event, becomes
//! a contract `failure` record, written once (IDs derive from the run and
//! criterion).

use super::{now, provenance, EvaluationOutcomes, TrialOutcome};
use crate::canonical::sha256_hex;
use crate::{LocalStore, StoreResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MAX_RATIONALES: usize = 2;
const MAX_RATIONALE_CHARS: usize = 300;
const SCHEMA_VERSION: &str = "0.1.0";

/// What kind of problem a group describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailureKind {
    /// The verifier judged a criterion `fail`.
    Criterion,
    /// The verifier could not decide (`mixed`, `inconclusive`, `unavailable`).
    NoVerdict,
    /// The run never reached the verifier (crash, timeout, limit, runner error).
    RunError,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Criterion => "criterion",
            Self::NoVerdict => "no_verdict",
            Self::RunError => "run_error",
        }
    }
}

/// One affected trial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub trial_id: String,
    pub task_id: String,
    pub run_id: String,
}

/// Trials sharing one failure: a criterion, or a run-error reason.
#[derive(Debug, Clone, PartialEq)]
pub struct FailureGroup {
    pub kind: FailureKind,
    /// The criterion ID, or the run's error summary.
    pub key: String,
    pub occurrences: Vec<Occurrence>,
    /// For criteria: trials whose latest attempt judged this criterion.
    pub judged: u64,
    /// The task of each judged trial (for per-family denominators).
    pub judged_task_ids: Vec<String>,
    /// The criterion's description from a task revision, if stored.
    pub requirement: Option<String>,
    /// Short model-judge rationales for failed criteria.
    pub rationales: Vec<String>,
}

impl FailureGroup {
    /// Distinct affected task revision IDs, in first-seen order.
    pub fn tasks(&self) -> Vec<&str> {
        let mut tasks: Vec<&str> = Vec::new();
        for o in &self.occurrences {
            if !tasks.contains(&o.task_id.as_str()) {
                tasks.push(&o.task_id);
            }
        }
        tasks
    }
}

/// An evaluation's failures.
#[derive(Debug, Clone, PartialEq)]
pub struct FailureAnalysis {
    pub evaluation_id: String,
    pub planned: u64,
    /// Trials whose latest outcome is fail or without a verdict.
    pub failing_trials: u64,
    /// Largest groups first.
    pub groups: Vec<FailureGroup>,
    /// Task revision ID → (task key, family), from stored task revisions.
    pub tasks: BTreeMap<String, (String, String)>,
}

impl FailureAnalysis {
    /// Per-family (failing, judged) counts for a criterion group.
    pub fn families(&self, group: &FailureGroup) -> Vec<(String, u64, u64)> {
        let family = |task: &str| {
            self.tasks
                .get(task)
                .map_or_else(|| "—".to_string(), |(_, f)| f.clone())
        };
        let mut counts: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for o in &group.occurrences {
            counts.entry(family(&o.task_id)).or_default().0 += 1;
        }
        for task in &group.judged_task_ids {
            if let Some(entry) = counts.get_mut(&family(task)) {
                entry.1 += 1;
            }
        }
        counts.into_iter().map(|(f, (n, d))| (f, n, d)).collect()
    }
}

fn truncate(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_RATIONALE_CHARS).collect();
    if text.chars().count() > MAX_RATIONALE_CHARS {
        out.push('…');
    }
    out.replace(['\n', '\r'], " ")
}

/// Groups the failures of an evaluation's latest attempts.
pub fn analyse_failures(
    store: &LocalStore,
    outcomes: &EvaluationOutcomes,
) -> StoreResult<FailureAnalysis> {
    let meta = store.metadata();
    let mut tasks: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut requirements: BTreeMap<String, String> = BTreeMap::new();
    for trial in &outcomes.trials {
        if tasks.contains_key(&trial.task_id) {
            continue;
        }
        let record = meta.get_record(&trial.task_id)?;
        let key = record
            .as_ref()
            .and_then(|r| r["task_key"].as_str())
            .unwrap_or(&trial.task_id)
            .to_string();
        let family = record
            .as_ref()
            .and_then(|r| r["task_family"].as_str())
            .unwrap_or("—")
            .to_string();
        if let Some(criteria) = record
            .as_ref()
            .and_then(|r| r["acceptance_criteria"].as_array())
        {
            for c in criteria {
                if let (Some(id), Some(description)) =
                    (c["criterion_id"].as_str(), c["description"].as_str())
                {
                    requirements
                        .entry(id.to_string())
                        .or_insert_with(|| description.to_string());
                }
            }
        }
        tasks.insert(trial.task_id.clone(), (key, family));
    }

    let mut groups: BTreeMap<(FailureKind, String), FailureGroup> = BTreeMap::new();
    let mut judged: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut failing_trials = 0;
    for trial in &outcomes.trials {
        if matches!(trial.outcome, TrialOutcome::Fail | TrialOutcome::NoVerdict) {
            failing_trials += 1;
        }
        let Some(run_id) = &trial.latest_run_id else {
            continue;
        };
        let Some(run) = meta.get_record(run_id)? else {
            continue;
        };
        let occurrence = Occurrence {
            trial_id: trial.trial_id.clone(),
            task_id: trial.task_id.clone(),
            run_id: run_id.clone(),
        };
        let refs = run["verification_refs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for reference in &refs {
            let Some(id) = reference["record_id"].as_str() else {
                continue;
            };
            let Some(verification) = meta.get_record(id)? else {
                continue;
            };
            let criterion = verification["criterion_id"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            judged
                .entry(criterion.clone())
                .or_default()
                .push(trial.task_id.clone());
            let kind = match verification["result"].as_str() {
                Some("pass") => continue,
                Some("fail") => FailureKind::Criterion,
                _ => FailureKind::NoVerdict,
            };
            let group = groups
                .entry((kind, criterion.clone()))
                .or_insert_with(|| FailureGroup {
                    kind,
                    key: criterion.clone(),
                    occurrences: Vec::new(),
                    judged: 0,
                    judged_task_ids: Vec::new(),
                    requirement: requirements.get(&criterion).cloned(),
                    rationales: Vec::new(),
                });
            group.occurrences.push(occurrence.clone());
            if let Some(rationale) = verification["judge_rationale"].as_str() {
                if group.rationales.len() < MAX_RATIONALES {
                    group.rationales.push(truncate(rationale));
                }
            }
        }
        if refs.is_empty() && trial.outcome == TrialOutcome::NoVerdict {
            let reason = run["error_summary"]
                .as_str()
                .unwrap_or("the run ended without a verdict")
                .to_string();
            groups
                .entry((FailureKind::RunError, reason.clone()))
                .or_insert_with(|| FailureGroup {
                    kind: FailureKind::RunError,
                    key: reason,
                    occurrences: Vec::new(),
                    judged: 0,
                    judged_task_ids: Vec::new(),
                    requirement: None,
                    rationales: Vec::new(),
                })
                .occurrences
                .push(occurrence);
        }
    }
    let mut groups: Vec<FailureGroup> = groups
        .into_values()
        .map(|mut g| {
            if g.kind != FailureKind::RunError {
                g.judged_task_ids = judged.get(&g.key).cloned().unwrap_or_default();
                g.judged = g.judged_task_ids.len() as u64;
            }
            g
        })
        .collect();
    groups.sort_by(|a, b| {
        b.occurrences
            .len()
            .cmp(&a.occurrences.len())
            .then(a.kind.cmp(&b.kind))
            .then(a.key.cmp(&b.key))
    });
    Ok(FailureAnalysis {
        evaluation_id: outcomes.evaluation_id.clone(),
        planned: outcomes.trials.len() as u64,
        failing_trials,
        groups,
        tasks,
    })
}

/// Writes a `failure` record per verified criterion failure and per run
/// error with a terminal event (once each) and returns every failure ID.
pub fn record_failures(store: &LocalStore, analysis: &FailureAnalysis) -> StoreResult<Vec<String>> {
    let meta = store.metadata();
    let mut ids = Vec::new();
    let mut records = Vec::new();
    for group in analysis
        .groups
        .iter()
        .filter(|g| g.kind != FailureKind::NoVerdict)
    {
        for o in &group.occurrences {
            let Some(run) = meta.get_record(&o.run_id)? else {
                continue;
            };
            let Some(terminal) = run["event_refs"].as_array().and_then(|e| e.last()).cloned()
            else {
                continue; // no event to cite: shown in the analysis, not recorded
            };
            let id = format!(
                "FAIL-{}",
                &sha256_hex(
                    format!("{}|{}|{}", o.run_id, group.kind.as_str(), group.key).as_bytes()
                )[..24]
            );
            ids.push(id.clone());
            if meta.get_record(&id)?.is_some() {
                continue;
            }
            let task = analysis
                .tasks
                .get(&o.task_id)
                .map_or(o.task_id.as_str(), |(key, _)| key.as_str());
            let observed_at = run["ended_at"]
                .as_str()
                .or_else(|| run["created_at"].as_str())
                .unwrap_or_default()
                .to_string();
            let mut record = match group.kind {
                FailureKind::Criterion => {
                    let verification_id = format!("{}:VERIFY:{}", o.run_id, group.key);
                    json!({
                        "failure_class": "objective_not_met",
                        "summary": format!("Criterion {} failed on task {task} (trial {}).", group.key, o.trial_id),
                        "violated_requirement": group.requirement.clone().unwrap_or_else(|| format!("Acceptance criterion {}.", group.key)),
                        "evidence_refs": [{"evidence_id": verification_id, "evidence_type": "verification_result", "relationship": "supports"}],
                        "verification_state": "verified",
                        "verification_result_ref": {"record_id": verification_id, "record_type": "verification_result"},
                    })
                }
                _ => json!({
                    "failure_class": "other",
                    "summary": format!("The run of task {task} (trial {}) ended without a verdict: {}", o.trial_id, group.key),
                    "violated_requirement": "The run must finish so its outputs can be verified.",
                    "evidence_refs": [{"evidence_id": terminal["record_id"], "evidence_type": "run_event", "relationship": "supports"}],
                    "verification_state": "unverified",
                    "verification_limitations": ["No verifier ran: the run ended before its outputs could be judged."],
                }),
            };
            if let Some(map) = record.as_object_mut() {
                for (key, value) in [
                    ("record_id", json!(id)),
                    ("record_type", json!("failure")),
                    ("schema_version", json!(SCHEMA_VERSION)),
                    ("created_at", json!(now())),
                    ("provenance", provenance(&analysis.evaluation_id)),
                    (
                        "run_ref",
                        json!({"record_id": o.run_id, "record_type": "run"}),
                    ),
                    ("observed_at", json!(observed_at)),
                    ("decisive_event_refs", json!([terminal])),
                    (
                        "gap_node_refs",
                        json!({"detection": [], "containment": [], "reporting": []}),
                    ),
                ] {
                    map.insert(key.into(), value);
                }
            }
            records.push(record);
        }
    }
    if !records.is_empty() {
        meta.put_records(&records)?;
    }
    Ok(ids)
}

/// A failure group as JSON, for `--json` output.
pub fn group_json(group: &FailureGroup, analysis: &FailureAnalysis) -> Value {
    json!({
        "kind": group.kind.as_str(),
        "key": group.key,
        "failing_trials": group.occurrences.len(),
        "judged_trials": group.judged,
        "tasks": group.tasks().iter().map(|t| analysis.tasks.get(*t).map_or(*t, |(k, _)| k.as_str())).collect::<Vec<_>>(),
        "families": analysis.families(group).into_iter().map(|(f, n, d)| json!({"family": f, "failing": n, "judged": d})).collect::<Vec<_>>(),
        "requirement": group.requirement,
        "rationales": group.rationales,
        "runs": group.occurrences.iter().map(|o| &o.run_id).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rationales_are_shortened_to_one_line() {
        let long = format!("line one\nline two {}", "x".repeat(400));
        let short = truncate(&long);
        assert!(short.ends_with('…'));
        assert!(!short.contains('\n'));
        assert_eq!(short.chars().count(), MAX_RATIONALE_CHARS + 1);
        assert_eq!(truncate("fine"), "fine");
    }

    #[test]
    fn tasks_are_listed_once_in_first_seen_order() {
        let at = |trial: &str, task: &str| Occurrence {
            trial_id: trial.into(),
            task_id: task.into(),
            run_id: format!("RUN-{trial}"),
        };
        let group = FailureGroup {
            kind: FailureKind::Criterion,
            key: "c".into(),
            occurrences: vec![at("1", "B"), at("2", "A"), at("3", "B")],
            judged: 4,
            judged_task_ids: vec!["A".into(), "A".into(), "B".into(), "B".into()],
            requirement: None,
            rationales: vec![],
        };
        assert_eq!(group.tasks(), ["B", "A"]);
        let analysis = FailureAnalysis {
            evaluation_id: "E".into(),
            planned: 4,
            failing_trials: 3,
            groups: vec![group.clone()],
            tasks: BTreeMap::from([
                ("A".to_string(), ("a".to_string(), "fam-1".to_string())),
                ("B".to_string(), ("b".to_string(), "fam-2".to_string())),
            ]),
        };
        assert_eq!(
            analysis.families(&group),
            [("fam-1".to_string(), 1, 2), ("fam-2".to_string(), 2, 2)]
        );
    }
}
