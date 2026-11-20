//! Readiness decisions and capability passports (ADR-0028).
//!
//! Gates are declared in advance in the evaluation file and frozen in the
//! plan's `analysis_plan.readiness`; nothing here accepts a threshold from
//! anywhere else. The workbench evaluates the gates; a named reviewer signs.
//!
//! Outcome:
//! - gates frozen after the first result, or the pass rate not `computed`:
//!   `insufficient_evidence`;
//! - any mandatory gate fails: `rejected`;
//! - any mandatory gate is inconclusive: `insufficient_evidence`;
//! - any optional gate fails or is inconclusive: `conditionally_approved`;
//! - otherwise `approved`.
//!
//! Only an approved decision issues an `active` passport (with an expiry);
//! every other outcome gives a `draft` that says why.

use super::{
    evaluation_ref, provenance, wilson, EvaluationOutcomes, FailureAnalysis, FailureKind, PassRate,
    ResourceMetric, ResourceSummary, TrialOutcome, SCHEMA_VERSION,
};
use crate::canonical::sha256_hex;
use crate::{LocalStore, StoreError, StoreResult};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// One evaluated gate.
#[derive(Debug, Clone, PartialEq)]
pub struct GateResult {
    pub gate_id: String,
    pub category: String,
    pub mandatory: bool,
    /// `pass`, `fail` or `inconclusive`.
    pub status: &'static str,
    pub rationale: String,
    pub evidence: Vec<Value>,
    /// The criteria a criteria gate covers (empty for metric gates).
    pub criteria: Vec<String>,
}

/// The frozen readiness declaration and the evaluated gates.
#[derive(Debug, Clone, PartialEq)]
pub struct Readiness {
    pub workload: String,
    pub autonomy_level: String,
    pub permitted_effects: Vec<String>,
    pub excluded_effects: Vec<String>,
    pub valid_for_days: u64,
    pub operating_conditions: Vec<String>,
    pub gates: Vec<GateResult>,
    /// `approved`, `conditionally_approved`, `rejected` or `insufficient_evidence`.
    pub outcome: &'static str,
    /// Why the outcome is what it is, one sentence each.
    pub reasons: Vec<String>,
    pub declared_in_advance: bool,
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn metric_value(
    metric: &str,
    outcomes: &EvaluationOutcomes,
    rate: &PassRate,
    resources: &[ResourceSummary],
) -> Result<(f64, String), String> {
    let resource = |which: ResourceMetric| resources.iter().find(|s| s.metric == which);
    let computed = |s: Option<&ResourceSummary>, what: &str| -> Result<(), String> {
        match s {
            Some(s) if s.status == "computed" => Ok(()),
            Some(s) => Err(format!(
                "the {what} result is {}",
                s.status.replace('_', " ")
            )),
            None => Err(format!("no {what} result")),
        }
    };
    match metric {
        "pass_rate" | "pass_rate_lower_bound" => {
            if rate.status != "computed" {
                return Err(format!(
                    "the pass rate is {}",
                    rate.status.replace('_', " ")
                ));
            }
            if metric == "pass_rate" {
                let v = rate.estimate.ok_or("no pass rate")?;
                Ok((v, format!("pass rate {v:.3}")))
            } else {
                let (lower, _, method) = rate.interval.ok_or("no interval")?;
                Ok((
                    lower,
                    format!(
                        "pass rate lower bound {lower:.3} ({})",
                        method.replace('_', " ")
                    ),
                ))
            }
        }
        "all_runs_pass_rate" | "all_runs_pass_rate_lower_bound" => {
            let r = super::reliability(outcomes);
            if r.status != "computed" {
                return Err(format!("reliability is {}", r.status.replace('_', " ")));
            }
            if metric == "all_runs_pass_rate" {
                let v = r.all_runs_pass_rate.ok_or("no reliability")?;
                Ok((v, format!("{v:.3} of repeated tasks passed every run")))
            } else {
                let (lower, _) = r.all_runs_interval.ok_or("no interval")?;
                Ok((
                    lower,
                    format!(
                        "passes-every-run lower bound {lower:.3} (Wilson over {} tasks)",
                        r.repeated_tasks
                    ),
                ))
            }
        }
        "wall_time_median_ms" | "wall_time_p90_ms" => {
            let s = resource(ResourceMetric::WallTime);
            computed(s, "wall time")?;
            let s = s.ok_or("no wall time")?;
            if metric == "wall_time_median_ms" {
                let v = s.estimate.ok_or("no median")?;
                Ok((v, format!("median wall time {v:.0} ms")))
            } else {
                let v = s.p90.ok_or("no p90")?;
                Ok((v, format!("p90 wall time {v:.0} ms")))
            }
        }
        "tokens_per_trial" => {
            let s = resource(ResourceMetric::Tokens);
            computed(s, "token")?;
            let v = s.and_then(|s| s.estimate).ok_or("no token result")?;
            Ok((v, format!("{v:.0} agent-reported tokens per trial")))
        }
        "cost_per_trial" => {
            let s = resource(ResourceMetric::Cost);
            computed(s, "cost")?;
            let s = s.ok_or("no cost")?;
            let v = s.estimate.ok_or("no cost result")? / 1e6;
            Ok((
                v,
                format!(
                    "{v:.4} {} per trial (estimated)",
                    s.currency.as_deref().unwrap_or("")
                ),
            ))
        }
        other => Err(format!("unknown metric {other}")),
    }
}

/// Evaluates the frozen readiness gates, or `None` when the evaluation's
/// frozen plan declares none.
pub fn evaluate_readiness(
    outcomes: &EvaluationOutcomes,
    rate: &PassRate,
    resources: &[ResourceSummary],
    failures: &FailureAnalysis,
) -> Option<Readiness> {
    let plan = outcomes.frozen_plan.as_ref()?;
    let declared = plan["analysis_plan"].get("readiness")?;
    let frozen_at = plan["frozen_at"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    let declared_in_advance = match (frozen_at, outcomes.first_result_at) {
        (Some(frozen), Some(first)) => frozen <= first,
        (Some(_), None) => true,
        _ => false,
    };
    let eligible = outcomes
        .trials
        .iter()
        .filter(|t| {
            matches!(
                t.outcome,
                TrialOutcome::Pass | TrialOutcome::Fail | TrialOutcome::NoVerdict
            )
        })
        .count() as u64;
    let mut gates = Vec::new();
    for gate in declared["gates"].as_array().into_iter().flatten() {
        let gate_id = gate["gate_id"].as_str().unwrap_or("gate").to_string();
        let category = gate["gate_category"]
            .as_str()
            .unwrap_or("other")
            .to_string();
        let mandatory = gate["mandatory"].as_bool().unwrap_or(true);
        let criteria = strings(&gate["criteria"]);
        let (status, rationale, evidence) = if let Some(metric) = gate["metric"].as_str() {
            match metric_value(metric, outcomes, rate, resources) {
                Err(why) => (
                    "inconclusive",
                    format!("Cannot judge {metric}: {why}."),
                    vec![],
                ),
                Ok((value, observed)) => {
                    match (gate["at_least"].as_f64(), gate["at_most"].as_f64()) {
                        (Some(min), _) if value >= min => {
                            ("pass", format!("{observed} is at least {min}."), vec![])
                        }
                        (Some(min), _) => ("fail", format!("{observed} is below {min}."), vec![]),
                        (_, Some(max)) if value <= max => {
                            ("pass", format!("{observed} is at most {max}."), vec![])
                        }
                        (_, Some(max)) => ("fail", format!("{observed} is above {max}."), vec![]),
                        _ => (
                            "inconclusive",
                            "The gate has no threshold.".to_string(),
                            vec![],
                        ),
                    }
                }
            }
        } else {
            let allowed = gate["max_failures"].as_u64().unwrap_or(0);
            let mut failed = 0_u64;
            let mut undecided = 0_u64;
            let mut evidence = Vec::new();
            for group in failures.groups.iter().filter(|g| criteria.contains(&g.key)) {
                match group.kind {
                    FailureKind::Criterion => {
                        failed += group.occurrences.len() as u64;
                        evidence.extend(group.occurrences.iter().map(|o| {
                            json!({"evidence_id": format!("{}:VERIFY:{}", o.run_id, group.key), "evidence_type": "verification_result", "relationship": "supports"})
                        }));
                    }
                    FailureKind::NoVerdict => undecided += group.occurrences.len() as u64,
                    FailureKind::RunError => {}
                }
            }
            let least_judged = criteria
                .iter()
                .map(|c| failures.judged_criteria.get(c).copied().unwrap_or(0))
                .min()
                .unwrap_or(0);
            let list = criteria.join(", ");
            if failed > allowed {
                (
                    "fail",
                    format!("{failed} failure(s) of {list}, more than the {allowed} allowed."),
                    evidence,
                )
            } else if undecided > 0 {
                (
                    "inconclusive",
                    format!("{undecided} judgment(s) of {list} had no verdict."),
                    evidence,
                )
            } else if least_judged < eligible {
                (
                    "inconclusive",
                    format!("{list} was judged on only {least_judged} of {eligible} trial(s) with an outcome."),
                    evidence,
                )
            } else {
                ("pass", format!("{failed} failure(s) of {list} in {eligible} trial(s); at most {allowed} allowed."), evidence)
            }
        };
        gates.push(GateResult {
            gate_id,
            category,
            mandatory,
            status,
            rationale,
            evidence,
            criteria,
        });
    }

    let mut reasons = Vec::new();
    let failed_mandatory: Vec<&str> = gates
        .iter()
        .filter(|g| g.mandatory && g.status == "fail")
        .map(|g| g.gate_id.as_str())
        .collect();
    let open_mandatory: Vec<&str> = gates
        .iter()
        .filter(|g| g.mandatory && g.status == "inconclusive")
        .map(|g| g.gate_id.as_str())
        .collect();
    let open_optional: Vec<&str> = gates
        .iter()
        .filter(|g| !g.mandatory && g.status != "pass")
        .map(|g| g.gate_id.as_str())
        .collect();
    if !declared_in_advance {
        reasons.push("The readiness gates were frozen after the evaluation's first result, so they were not declared in advance.".into());
    }
    if rate.status != "computed" {
        reasons.push(format!(
            "The pass rate is {}.",
            rate.status.replace('_', " ")
        ));
    }
    if !failed_mandatory.is_empty() {
        reasons.push(format!(
            "Mandatory gate(s) failed: {}.",
            failed_mandatory.join(", ")
        ));
    }
    if !open_mandatory.is_empty() {
        reasons.push(format!(
            "Mandatory gate(s) could not be judged: {}.",
            open_mandatory.join(", ")
        ));
    }
    if !open_optional.is_empty() {
        reasons.push(format!(
            "Optional gate(s) not passed: {}.",
            open_optional.join(", ")
        ));
    }
    let outcome = if !declared_in_advance || rate.status != "computed" {
        "insufficient_evidence"
    } else if !failed_mandatory.is_empty() {
        "rejected"
    } else if !open_mandatory.is_empty() {
        "insufficient_evidence"
    } else if !open_optional.is_empty() {
        "conditionally_approved"
    } else {
        reasons.push("Every gate passed.".into());
        "approved"
    };
    Some(Readiness {
        workload: declared["workload"]
            .as_str()
            .unwrap_or("unspecified")
            .to_string(),
        autonomy_level: declared["autonomy_level"]
            .as_str()
            .unwrap_or("supervised")
            .to_string(),
        permitted_effects: strings(&declared["permitted_effects"]),
        excluded_effects: strings(&declared["excluded_effects"]),
        valid_for_days: declared["valid_for_days"].as_u64().unwrap_or(30),
        operating_conditions: strings(&plan["workload_scope"]["operating_conditions"]),
        gates,
        outcome,
        reasons,
        declared_in_advance,
    })
}

/// What `issue_passport` wrote (or found already written).
#[derive(Debug, Clone, PartialEq)]
pub struct IssuedPassport {
    pub decision_id: String,
    pub passport_id: String,
    /// `active` or `draft`.
    pub status: String,
    pub expires_at: Option<String>,
    pub reviewer: String,
    /// True when this exact passport already existed (nothing new written).
    pub already_issued: bool,
}

fn reference(id: &str, record_type: &str) -> Value {
    json!({"record_id": id, "record_type": record_type})
}

/// Writes the readiness decision and the capability passport. Their IDs
/// derive from the results, the frozen plan and the reviewer, so issuing
/// again over unchanged results returns the existing passport.
pub fn issue_passport(
    store: &LocalStore,
    outcomes: &EvaluationOutcomes,
    rate: &PassRate,
    readiness: &Readiness,
    failures: &FailureAnalysis,
    failure_ids: &[String],
    reviewer: &str,
) -> StoreResult<IssuedPassport> {
    let reviewer = reviewer.trim();
    if reviewer.is_empty() || reviewer.len() > 256 {
        return Err(StoreError::InvalidRecord(
            "the reviewer must be 1 to 256 characters".into(),
        ));
    }
    let meta = store.metadata();
    let plan_digest = outcomes.predeclared_digest();
    let tag =
        &sha256_hex(format!("{}|{plan_digest}|{reviewer}", outcomes.data_digest()).as_bytes())
            [..24];
    let decision_id = format!("DEC-{tag}");
    let passport_id = format!("PASSPORT-{tag}");
    if let Some(existing) = meta.get_record(&passport_id)? {
        return Ok(IssuedPassport {
            decision_id,
            passport_id,
            status: existing["passport_status"]
                .as_str()
                .unwrap_or("draft")
                .to_string(),
            expires_at: existing["expires_at"].as_str().map(str::to_string),
            reviewer: reviewer.to_string(),
            already_issued: true,
        });
    }

    // Evidence: every latest attempt's verification results, else the
    // runs' terminal events.
    let mut by_family: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut all_evidence = Vec::new();
    for trial in &outcomes.trials {
        let Some(run_id) = &trial.latest_run_id else {
            continue;
        };
        let Some(run) = meta.get_record(run_id)? else {
            continue;
        };
        let family = failures
            .tasks
            .get(&trial.task_id)
            .map_or("—".to_string(), |(_, f)| f.clone());
        let mut evidence: Vec<Value> = run["verification_refs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["record_id"].as_str())
            .map(|id| json!({"evidence_id": id, "evidence_type": "verification_result", "relationship": "supports"}))
            .collect();
        if evidence.is_empty() {
            evidence.extend(
                run["event_refs"].as_array().and_then(|e| e.last()).and_then(|e| e["record_id"].as_str()).map(
                    |id| json!({"evidence_id": id, "evidence_type": "run_event", "relationship": "context"}),
                ),
            );
        }
        by_family
            .entry(family)
            .or_default()
            .extend(evidence.iter().cloned());
        all_evidence.extend(evidence);
    }
    // Every gate cites evidence: a criteria gate its criteria's verification
    // results (the failing ones when it failed), a metric gate every result.
    let gate_evidence = |g: &super::GateResult| -> Vec<Value> {
        if !g.evidence.is_empty() {
            return g.evidence.clone();
        }
        if g.criteria.is_empty() {
            return all_evidence.clone();
        }
        let judged: Vec<Value> = all_evidence
            .iter()
            .filter(|e| {
                e["evidence_id"].as_str().is_some_and(|id| {
                    g.criteria
                        .iter()
                        .any(|c| id.ends_with(&format!(":VERIFY:{c}")))
                })
            })
            .cloned()
            .collect();
        if judged.is_empty() {
            all_evidence.clone()
        } else {
            judged
        }
    };
    if all_evidence.is_empty() {
        return Err(StoreError::InvalidRecord(format!(
            "{}: no finished run to cite as evidence yet",
            outcomes.evaluation_id
        )));
    }
    let fingerprint = meta
        .get_schedule(&outcomes.evaluation_id)?
        .and_then(|s| {
            s.trials
                .first()
                .map(|t| t.system_fingerprint_ref.record_id.clone())
        })
        .ok_or_else(|| StoreError::InvalidRecord("the evaluation has no trials".into()))?;
    let task_families: Vec<String> = failures
        .tasks
        .values()
        .map(|(_, family)| family.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let operating_conditions = if readiness.operating_conditions.is_empty() {
        vec!["as recorded in the evaluation's frozen plan".to_string()]
    } else {
        readiness.operating_conditions.clone()
    };

    let now_time = Utc::now();
    let decided_at = now_time.to_rfc3339_opts(SecondsFormat::Millis, true);
    let approved = readiness.outcome == "approved";
    let expires_at = approved.then(|| {
        (now_time + Duration::days(readiness.valid_for_days as i64))
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    });
    let conditions: Vec<String> = readiness
        .gates
        .iter()
        .filter(|g| !g.mandatory && g.status != "pass")
        .map(|g| format!("Optional gate {} not passed: {}", g.gate_id, g.rationale))
        .collect();

    let mut decision = json!({
        "record_id": decision_id,
        "record_type": "decision_record",
        "schema_version": SCHEMA_VERSION,
        "created_at": decided_at,
        "provenance": provenance(&outcomes.evaluation_id),
        "decision_type": "readiness",
        "outcome": readiness.outcome,
        "scope": {
            "system_fingerprint_ref": reference(&fingerprint, "system_fingerprint"),
            "workload_families": [readiness.workload],
            "operating_conditions": operating_conditions,
            "autonomy_level": readiness.autonomy_level,
        },
        "gate_results": readiness.gates.iter().map(|g| json!({
            "gate_id": g.gate_id,
            "gate_category": g.category,
            "mandatory": g.mandatory,
            "status": g.status,
            "evidence_refs": gate_evidence(g),
            "rationale": g.rationale,
        })).collect::<Vec<_>>(),
        "evidence_refs": all_evidence,
        "reason": format!("Gates declared in advance in plan {} and evaluated by the workbench. {}", outcomes.plan_id, readiness.reasons.join(" ")),
        "reviewer": reviewer,
        "decided_at": decided_at,
    });
    if let Some(map) = decision.as_object_mut() {
        if !conditions.is_empty() {
            map.insert("conditions".into(), json!(conditions));
        }
        if let Some(expiry) = &expires_at {
            map.insert("review_by".into(), json!(expiry));
        }
    }

    let count = |o: TrialOutcome| outcomes.trials.iter().filter(|t| t.outcome == o).count() as u64;
    let (passes, fails, undecided) = (
        count(TrialOutcome::Pass),
        count(TrialOutcome::Fail),
        count(TrialOutcome::NoVerdict),
    );
    let resolved = passes + fails + undecided;
    let mut dimensions = Vec::new();
    for family in &task_families {
        let in_family = |o: TrialOutcome| {
            outcomes
                .trials
                .iter()
                .filter(|t| {
                    t.outcome == o
                        && failures
                            .tasks
                            .get(&t.task_id)
                            .is_some_and(|(_, f)| f == family)
                })
                .count() as u64
        };
        let (p, f, u) = (
            in_family(TrialOutcome::Pass),
            in_family(TrialOutcome::Fail),
            in_family(TrialOutcome::NoVerdict),
        );
        let Some(evidence) = by_family.get(family).filter(|e| !e.is_empty()) else {
            continue;
        };
        let n = p + f + u;
        let (lower, upper) = wilson(p, n);
        dimensions.push(json!({
            "dimension_id": family,
            "description": format!("Tasks in the {family} family"),
            "eligible_trials": n,
            "pass_trials": p,
            "fail_trials": f,
            "inconclusive_trials": u,
            "uncertainty_summary": format!("{p} of {n} passed; 95% Wilson interval {lower:.2}–{upper:.2} (repeated trials of a task are not independent)."),
            "evidence_refs": evidence,
        }));
    }
    if dimensions.is_empty() {
        return Err(StoreError::InvalidRecord(
            "no task family has evidence to summarise".into(),
        ));
    }
    let mut limitations: Vec<String> = Vec::new();
    for note in rate.limitations.iter().chain(&readiness.reasons) {
        if !limitations.contains(note) {
            limitations.push(note.clone());
        }
    }
    limitations.push(format!(
        "Evidence covers system fingerprint {fingerprint}, the {} evaluated task family(ies) and the operating conditions above only.",
        task_families.len()
    ));
    let mut constraints = vec![
        format!("Only system fingerprint {fingerprint} (this agent version and configuration)."),
        format!(
            "Only the evaluated task families: {}.",
            task_families.join(", ")
        ),
        format!(
            "Only at autonomy level {} under the evaluated operating conditions.",
            readiness.autonomy_level
        ),
    ];
    constraints.extend(conditions.iter().cloned());
    let mut passport = json!({
        "record_id": passport_id,
        "record_type": "capability_passport",
        "schema_version": SCHEMA_VERSION,
        "created_at": decided_at,
        "provenance": provenance(&outcomes.evaluation_id),
        "passport_status": if approved { "active" } else { "draft" },
        "system_fingerprint_ref": reference(&fingerprint, "system_fingerprint"),
        "readiness_decision_ref": reference(&decision_id, "decision_record"),
        "scope": {
            "workload_families": [readiness.workload],
            "task_families": task_families,
            "operating_conditions": operating_conditions,
            "autonomy_level": readiness.autonomy_level,
            "permitted_effects": readiness.permitted_effects,
            "excluded_effects": readiness.excluded_effects,
        },
        "evaluation_refs": [evaluation_ref(&outcomes.evaluation_id)],
        "trial_summary": {
            "planned_trials": outcomes.trials.len(),
            "eligible_trials": resolved,
            "excluded_trials": count(TrialOutcome::Invalid),
            "resolved_trials": resolved,
            "pass_trials": passes,
            "fail_trials": fails,
            "mixed_trials": 0,
            "inconclusive_trials": undecided,
            "run_attempts": outcomes.trials.iter().map(|t| t.attempts).sum::<u64>(),
        },
        "capability_dimensions": dimensions,
        "known_failure_refs": failure_ids.iter().map(|id| reference(id, "failure")).collect::<Vec<_>>(),
        "limitations": limitations,
        "permitted_use_constraints": constraints,
        "issued_at": decided_at,
    });
    if let (Some(expiry), Some(map)) = (&expires_at, passport.as_object_mut()) {
        map.insert("expires_at".into(), json!(expiry));
        map.insert("review_by".into(), json!(expiry));
    }
    meta.put_records(&[decision, passport])?;
    Ok(IssuedPassport {
        decision_id,
        passport_id,
        status: if approved {
            "active".into()
        } else {
            "draft".into()
        },
        expires_at,
        reviewer: reviewer.to_string(),
        already_issued: false,
    })
}

/// The newest passport recorded for an evaluation, if any.
pub fn latest_passport(
    store: &LocalStore,
    evaluation_id: &str,
) -> StoreResult<Option<(Value, Option<Value>)>> {
    let meta = store.metadata();
    let mut newest: Option<Value> = None;
    for reference in meta.references_to(evaluation_id)? {
        if reference.source_type != "capability_passport" {
            continue;
        }
        if let Some(passport) = meta.get_record(&reference.source_id)? {
            let newer = newest
                .as_ref()
                .is_none_or(|n| passport["created_at"].as_str() > n["created_at"].as_str());
            if newer {
                newest = Some(passport);
            }
        }
    }
    let Some(passport) = newest else {
        return Ok(None);
    };
    let decision = match passport["readiness_decision_ref"]["record_id"].as_str() {
        Some(id) => meta.get_record(id)?,
        None => None,
    };
    Ok(Some((passport, decision)))
}

#[cfg(test)]
mod tests {
    use super::super::{
        pass_rate, resource_summaries, FailureGroup, Occurrence, ReportedUsage, TrialResult,
    };
    use super::*;

    fn outcomes(passes: usize, gates: Value, frozen_at: &str) -> EvaluationOutcomes {
        EvaluationOutcomes {
            evaluation_id: "E".into(),
            trials: (0..12)
                .map(|i| TrialResult {
                    trial_id: format!("T{i}"),
                    task_id: format!("TASK-{i}"),
                    outcome: if i < passes {
                        TrialOutcome::Pass
                    } else {
                        TrialOutcome::Fail
                    },
                    attempts: 1,
                    wall_time_ms: Some(100),
                    usage: ReportedUsage::Absent,
                    latest_run_id: Some(format!("RUN-{i}")),
                })
                .collect(),
            plan_digest: "c".repeat(64),
            benchmark_ref: json!({"record_id": "B", "record_type": "benchmark_revision"}),
            benchmark_digest: "b".repeat(64),
            task_population_digest: "a".repeat(64),
            system_fingerprint_digest: "f".repeat(64),
            plan_id: "PLAN-E".into(),
            frozen_plan: Some(json!({
                "frozen_at": frozen_at,
                "content_digest": "d".repeat(64),
                "workload_scope": {"operating_conditions": ["runner: local_bubblewrap"]},
                "analysis_plan": {"readiness": {
                    "workload": "w", "autonomy_level": "supervised",
                    "permitted_effects": [], "excluded_effects": [], "valid_for_days": 30,
                    "gates": gates,
                }},
            })),
            first_result_at: Some("2026-11-19T12:00:00Z".parse().unwrap()),
        }
    }

    fn failures(groups: Vec<FailureGroup>, judged: &[(&str, u64)]) -> FailureAnalysis {
        FailureAnalysis {
            evaluation_id: "E".into(),
            planned: 12,
            failing_trials: 0,
            groups,
            tasks: BTreeMap::new(),
            judged_criteria: judged.iter().map(|(c, n)| (c.to_string(), *n)).collect(),
        }
    }

    fn failed(criterion: &str, kind: FailureKind, n: usize) -> FailureGroup {
        FailureGroup {
            kind,
            key: criterion.into(),
            occurrences: (0..n)
                .map(|i| Occurrence {
                    trial_id: format!("T{i}"),
                    task_id: format!("TASK-{i}"),
                    run_id: format!("RUN-{i}"),
                })
                .collect(),
            judged: 12,
            judged_task_ids: vec![],
            requirement: None,
            rationales: vec![],
        }
    }

    const BEFORE: &str = "2026-11-19T11:00:00Z";

    fn judge(data: &EvaluationOutcomes, f: &FailureAnalysis) -> Readiness {
        evaluate_readiness(data, &pass_rate(data), &resource_summaries(data), f).unwrap()
    }

    #[test]
    fn every_gate_passing_is_approved() {
        let gates = json!([
            {"gate_id": "rate", "gate_category": "capability", "mandatory": true, "metric": "pass_rate", "at_least": 0.9},
            {"gate_id": "safe", "gate_category": "safety", "mandatory": true, "criteria": ["c"], "max_failures": 0}
        ]);
        let r = judge(
            &outcomes(12, gates, BEFORE),
            &failures(vec![], &[("c", 12)]),
        );
        assert_eq!(r.outcome, "approved", "{:?}", r.gates);
        assert!(r.declared_in_advance);
        assert!(r.gates.iter().all(|g| g.status == "pass"));
    }

    #[test]
    fn mandatory_and_optional_failures_decide_the_outcome() {
        let gates = |mandatory: bool| {
            json!([
                {"gate_id": "rate", "gate_category": "capability", "mandatory": mandatory, "metric": "pass_rate", "at_least": 0.9}
            ])
        };
        let rejected = judge(&outcomes(6, gates(true), BEFORE), &failures(vec![], &[]));
        assert_eq!(rejected.outcome, "rejected");
        assert!(rejected.gates[0].rationale.contains("is below 0.9"));
        let conditional = judge(&outcomes(6, gates(false), BEFORE), &failures(vec![], &[]));
        assert_eq!(conditional.outcome, "conditionally_approved");
    }

    #[test]
    fn criteria_gates_count_failures_and_coverage() {
        let gate = json!([{"gate_id": "safe", "gate_category": "safety", "mandatory": true, "criteria": ["c"], "max_failures": 1}]);
        let data = outcomes(12, gate, BEFORE);
        let two = judge(
            &data,
            &failures(vec![failed("c", FailureKind::Criterion, 2)], &[("c", 12)]),
        );
        assert_eq!((two.outcome, two.gates[0].status), ("rejected", "fail"));
        assert_eq!(two.gates[0].evidence.len(), 2);
        let one = judge(
            &data,
            &failures(vec![failed("c", FailureKind::Criterion, 1)], &[("c", 12)]),
        );
        assert_eq!(one.gates[0].status, "pass");
        let undecided = judge(
            &data,
            &failures(vec![failed("c", FailureKind::NoVerdict, 1)], &[("c", 12)]),
        );
        assert_eq!(
            (undecided.outcome, undecided.gates[0].status),
            ("insufficient_evidence", "inconclusive")
        );
        let partly = judge(&data, &failures(vec![], &[("c", 7)]));
        assert!(partly.gates[0].rationale.contains("judged on only 7 of 12"));
    }

    #[test]
    fn late_gates_and_descriptive_metrics_are_insufficient() {
        let gate = json!([{"gate_id": "rate", "gate_category": "capability", "mandatory": true, "metric": "pass_rate", "at_least": 0.5}]);
        let late = judge(
            &outcomes(12, gate.clone(), "2026-11-19T13:00:00Z"),
            &failures(vec![], &[]),
        );
        assert_eq!(late.outcome, "insufficient_evidence");
        assert!(!late.declared_in_advance);
        assert!(late.reasons[0].contains("not declared in advance"));
        // Tokens were never reported: the gate cannot be judged.
        let tokens = json!([{"gate_id": "tok", "gate_category": "operational", "mandatory": true, "metric": "tokens_per_trial", "at_most": 1000}]);
        let open = judge(&outcomes(12, tokens, BEFORE), &failures(vec![], &[]));
        assert_eq!(
            (open.outcome, open.gates[0].status),
            ("insufficient_evidence", "inconclusive")
        );
    }

    #[test]
    fn no_declared_gates_means_no_readiness() {
        let mut data = outcomes(12, json!([]), BEFORE);
        data.frozen_plan = Some(json!({"analysis_plan": {}}));
        assert!(
            evaluate_readiness(&data, &pass_rate(&data), &[], &failures(vec![], &[])).is_none()
        );
    }
}
