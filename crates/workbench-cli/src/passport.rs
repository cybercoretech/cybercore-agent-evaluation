//! `workbench passport` (ADR-0028): evaluate the readiness gates declared in
//! advance, and record the readiness decision and capability passport.

use crate::summary::{open, summarise};
use cybercore_evaluation_store::analysis::{
    evaluate_readiness, issue_passport, resource_summaries,
};
use serde_json::json;
use std::path::Path;

pub fn passport(
    store_dir: &Path,
    evaluation_id: &str,
    reviewer: &str,
    as_json: bool,
) -> Result<(), String> {
    let store = open(store_dir)?;
    let (outcomes, rate, _) = summarise(&store, evaluation_id)?;
    let resources = resource_summaries(&outcomes);
    let (_, failures, failure_ids) = crate::failures::analyse(&store, evaluation_id)?;
    let readiness = evaluate_readiness(&outcomes, &rate, &resources, &failures).ok_or_else(|| {
        format!(
            "{evaluation_id} declares no readiness gates: add a readiness section to its evaluation file (or suite) before its first run"
        )
    })?;
    let issued = issue_passport(
        &store,
        &outcomes,
        &rate,
        &readiness,
        &failures,
        &failure_ids,
        reviewer,
    )
    .map_err(|e| e.to_string())?;
    let meta = store.metadata();
    let passport = meta
        .get_record(&issued.passport_id)
        .map_err(|e| e.to_string())?;
    if as_json {
        let decision = meta
            .get_record(&issued.decision_id)
            .map_err(|e| e.to_string())?;
        let out = json!({
            "evaluation_id": evaluation_id,
            "outcome": readiness.outcome,
            "reasons": readiness.reasons,
            "declared_in_advance": readiness.declared_in_advance,
            "gates": readiness.gates.iter().map(|g| json!({
                "gate_id": g.gate_id, "category": g.category, "mandatory": g.mandatory,
                "status": g.status, "rationale": g.rationale,
            })).collect::<Vec<_>>(),
            "already_issued": issued.already_issued,
            "decision": decision,
            "passport": passport,
        });
        say!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    say!(
        "{evaluation_id}: readiness for {} ({})",
        readiness.workload,
        readiness.autonomy_level
    );
    for gate in &readiness.gates {
        say!(
            "  gate        {:<18} {:<12} {}{}",
            gate.gate_id,
            gate.status,
            gate.rationale,
            if gate.mandatory { "" } else { " (optional)" }
        );
    }
    say!(
        "  outcome     {}: {}",
        readiness.outcome.replace('_', " "),
        readiness.reasons.join(" ")
    );
    let validity = match &issued.expires_at {
        Some(expiry) => format!("active until {}", &expiry[..10.min(expiry.len())]),
        None => format!("{} (not approved)", issued.status),
    };
    say!(
        "  passport    {} {validity}; reviewer {}{}",
        issued.passport_id,
        issued.reviewer,
        if issued.already_issued {
            " (already issued for these results)"
        } else {
            ""
        }
    );
    say!("  decision    {}", issued.decision_id);
    if let Some(passport) = &passport {
        let dimensions: Vec<String> = passport["capability_dimensions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                format!(
                    "{} {}/{}",
                    d["dimension_id"].as_str().unwrap_or("?"),
                    d["pass_trials"],
                    d["eligible_trials"]
                )
            })
            .collect();
        say!("  capability  {}", dimensions.join(", "));
    }
    say!(
        "  failures    {} known failure record(s)",
        failure_ids.len()
    );
    Ok(())
}
