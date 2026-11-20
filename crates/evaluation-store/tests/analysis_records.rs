//! Recorded summaries keep the metric-result domain invariants, and plans
//! frozen under pass-rate revision 1 stay valid after revision 2.

use cybercore_evaluation_core::EvaluationSchedule;
use cybercore_evaluation_store::analysis::{
    freeze_plan, pass_rate, record_summary, EvaluationOutcomes, FreezeOutcome, PlanDeclaration,
    ReportedUsage, TrialOutcome, TrialResult,
};
use cybercore_evaluation_store::{canonical_json, sha256_hex, LocalStore};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cybercore-analysis-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record(store: &LocalStore, id: &str) -> Value {
    store.metadata().get_record(id).unwrap().unwrap()
}

#[test]
fn pass_rate_records_keep_the_count_invariants() {
    let dir = TempDir::new("counts");
    let store = LocalStore::open(&dir.0).unwrap();
    let outcomes_list = [
        TrialOutcome::Pass,
        TrialOutcome::Pass,
        TrialOutcome::Fail,
        TrialOutcome::NoVerdict,
        TrialOutcome::NoVerdict,
        TrialOutcome::Invalid,
        TrialOutcome::NotRun,
    ];
    let outcomes = EvaluationOutcomes {
        evaluation_id: "EVAL-COUNTS".into(),
        trials: outcomes_list
            .iter()
            .enumerate()
            .map(|(i, outcome)| TrialResult {
                trial_id: format!("T{i}"),
                task_id: format!("TASK-{i}"),
                outcome: *outcome,
                attempts: u64::from(*outcome != TrialOutcome::NotRun),
                wall_time_ms: Some(100),
                usage: ReportedUsage::Absent,
                latest_run_id: None,
            })
            .collect(),
        plan_digest: "c".repeat(64),
        benchmark_ref: json!({"record_id": "BENCH", "record_type": "benchmark_revision"}),
        benchmark_digest: "b".repeat(64),
        task_population_digest: "a".repeat(64),
        system_fingerprint_digest: "f".repeat(64),
        plan_id: "PLAN-COUNTS".into(),
        frozen_plan: None,
        first_result_at: None,
    };
    let rate = pass_rate(&outcomes);
    let records = record_summary(&store, &outcomes, &rate).unwrap();
    let ids = std::iter::once(&records.metric_result_id).chain(&records.resource_metric_result_ids);
    for id in ids {
        let result = record(&store, id);
        let c = &result["counts"];
        let n = |k: &str| c[k].as_u64().unwrap();
        assert_eq!(
            n("eligible_trial_count") + n("excluded_trial_count") + n("pending_trial_count"),
            n("planned_trial_count"),
            "{id}"
        );
        assert_eq!(
            n("observed_trial_count") + n("missing_trial_count"),
            n("eligible_trial_count"),
            "{id}"
        );
        assert!(n("run_attempt_count") >= n("observed_trial_count"), "{id}");
    }
    let main = record(&store, &records.metric_result_id);
    // Two passes out of five trials with an outcome; the two without a
    // verdict are observed as not passed.
    assert_eq!(main["denominator"], main["counts"]["observed_trial_count"]);
    assert_eq!(
        (main["numerator"].as_u64(), main["denominator"].as_u64()),
        (Some(2), Some(5))
    );
    assert_eq!(main["counts"]["missing_trial_count"], 0);
    assert!(main["limitations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l.as_str().unwrap().contains("2 trial(s) had no verdict")));
    let definition = record(&store, "METRIC-WORKBENCH-PASS-RATE-R2");
    assert_eq!(definition["revision"], 2);
}

fn schedule() -> EvaluationSchedule {
    let example: Value = serde_json::from_str(include_str!(
        "../../workbench-cli/evaluations/cat-smoke.json"
    ))
    .unwrap();
    serde_json::from_value(example["schedule"].clone()).unwrap()
}

fn declaration(question: &str) -> PlanDeclaration {
    PlanDeclaration {
        decision_question: question.into(),
        task_selection_method: "curated".into(),
        task_families: vec![],
        operating_conditions: vec!["runner local_bubblewrap".into()],
        comparisons: vec![],
        readiness: None,
    }
}

#[test]
fn plans_frozen_under_revision_one_stay_valid() {
    let schedule = schedule();
    let current = TempDir::new("plan-current");
    let store = LocalStore::open(&current.0).unwrap();
    let plan_id = schedule.plan_ref.record_id.clone();
    assert!(matches!(
        freeze_plan(&store, &schedule, &declaration("Does it cat?")).unwrap(),
        FreezeOutcome::Frozen { .. }
    ));

    // The same plan as revision 1 froze it: it named the R1 definition.
    let mut legacy = record(&store, &plan_id);
    assert_eq!(
        legacy["metrics"][0]["definition_ref"]["record_id"],
        "METRIC-WORKBENCH-PASS-RATE-R2"
    );
    legacy["metrics"][0]["definition_ref"]["record_id"] = json!("METRIC-WORKBENCH-PASS-RATE-R1");
    let mut body = legacy.clone();
    for field in ["created_at", "provenance", "frozen_at", "content_digest"] {
        body.as_object_mut().unwrap().remove(field);
    }
    legacy["content_digest"] = json!(sha256_hex(canonical_json(&body).as_bytes()));

    let old = TempDir::new("plan-legacy");
    let old_store = LocalStore::open(&old.0).unwrap();
    old_store.metadata().put_record(&legacy).unwrap();
    assert!(matches!(
        freeze_plan(&old_store, &schedule, &declaration("Does it cat?")).unwrap(),
        FreezeOutcome::AlreadyFrozen { .. }
    ));
    // A plan that really changed is still refused.
    let changed = freeze_plan(&old_store, &schedule, &declaration("Something else?"));
    assert!(changed.unwrap_err().to_string().contains("cannot change"));
}
