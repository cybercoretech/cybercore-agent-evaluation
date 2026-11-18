use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn get<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    value.and_then(|item| item.get(key))
}

fn as_text(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

fn number(value: Option<&Value>, fallback: f64) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(fallback)
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_f64().unwrap_or(0.0) != 0.0,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
    }
}

fn has_non_null(value: Option<&Value>) -> bool {
    value.is_some_and(|item| !item.is_null())
}

fn same_value(left: Option<&Value>, right: Option<&Value>) -> bool {
    let left = left.filter(|item| !item.is_null());
    let right = right.filter(|item| !item.is_null());
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn array(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn required_object<'a>(parent: &'a Value, key: &str) -> std::result::Result<&'a Value, String> {
    get(Some(parent), key)
        .filter(|value| value.is_object())
        .ok_or_else(|| format!("required object '{key}' is missing or invalid"))
}

fn object_or_empty<'a>(parent: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    get(parent, key).filter(|value| value.is_object())
}

fn tuple_key(items: &[Option<&Value>]) -> String {
    let values = items
        .iter()
        .map(|item| match *item {
            Some(value) if !value.is_null() => (*value).clone(),
            _ => Value::Null,
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string())
}

const CAUSAL: &[&str] = &[
    "caused",
    "contributed_to",
    "enabled",
    "amplified",
    "propagated_to",
    "prevented_recovery",
];

fn check_graph(graph: &Value) -> std::result::Result<(), String> {
    if !graph.is_object() {
        return Err("causal graph must be an object".to_string());
    }
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    for node in array(get(Some(graph), "nodes")) {
        let id = as_text(get(Some(node), "node_id"))
            .ok_or_else(|| "causal graph node IDs must be present and unique".to_string())?;
        let node_type = as_text(get(Some(node), "node_type"))
            .unwrap_or("")
            .to_string();
        if by_id.insert(id.to_string(), node_type).is_some() {
            return Err("causal graph node IDs must be present and unique".to_string());
        }
    }

    let mut adjacency: BTreeMap<String, Vec<String>> =
        by_id.keys().map(|id| (id.clone(), Vec::new())).collect();

    for edge in array(get(Some(graph), "edges")) {
        let src = as_text(get(Some(edge), "source_node"))
            .ok_or_else(|| "causal graph edge source_node is missing".to_string())?;
        let dst = as_text(get(Some(edge), "target_node"))
            .ok_or_else(|| "causal graph edge target_node is missing".to_string())?;
        let rel = as_text(get(Some(edge), "relationship")).unwrap_or("");

        if !by_id.contains_key(src) || !by_id.contains_key(dst) {
            let edge_id = as_text(get(Some(edge), "edge_id")).unwrap_or("<unknown>");
            return Err(format!("edge {edge_id} references a missing endpoint"));
        }
        if src == dst {
            return Err("self-edges are not permitted".to_string());
        }

        let expected_target = match rel {
            "escaped_detection" => Some("detection_gap"),
            "escaped_containment" => Some("containment_gap"),
            "misreported_as" => Some("reporting_gap"),
            _ => None,
        };
        if let Some(target_type) = expected_target {
            let source_type = by_id.get(src).map(String::as_str).unwrap_or("");
            let source_allowed = ["event", "error", "failure"].contains(&source_type);
            let actual_target = by_id.get(dst).map(String::as_str).unwrap_or("");
            if !source_allowed || actual_target != target_type {
                return Err(format!(
                    "{rel} direction/type mismatch: expected event/error/failure -> {target_type}"
                ));
            }
        }

        if CAUSAL.contains(&rel) {
            adjacency
                .get_mut(src)
                .expect("source was checked")
                .push(dst.to_string());
        }

        if CAUSAL.contains(&rel)
            || matches!(
                rel,
                "escaped_detection" | "escaped_containment" | "misreported_as"
            )
        {
            if !truthy(get(Some(edge), "supporting_evidence")) {
                return Err(format!("material edge {rel} requires supporting evidence"));
            }
            if !truthy(get(Some(edge), "rationale")) {
                return Err(format!("material edge {rel} requires rationale"));
            }
        }
    }

    fn visit(
        node: &str,
        adjacency: &BTreeMap<String, Vec<String>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> std::result::Result<(), String> {
        if visiting.contains(node) {
            return Err("causal edges contain a directed cycle".to_string());
        }
        if visited.contains(node) {
            return Ok(());
        }
        visiting.insert(node.to_string());
        if let Some(next_nodes) = adjacency.get(node) {
            for next in next_nodes {
                visit(next, adjacency, visiting, visited)?;
            }
        }
        visiting.remove(node);
        visited.insert(node.to_string());
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for node in adjacency.keys() {
        visit(node, &adjacency, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn check_attribution_confirmation(obj: &Value) -> std::result::Result<(), String> {
    if as_text(get(Some(obj), "status")) != Some("confirmed") {
        return Ok(());
    }
    let confirmation = object_or_empty(Some(obj), "confirmation");
    let basis = as_text(get(confirmation, "basis"));

    if matches!(basis, Some("direct_mechanistic_evidence" | "combined"))
        && (!truthy(get(confirmation, "mechanism_description"))
            || !truthy(get(confirmation, "mechanism_evidence_refs")))
    {
        return Err(
            "direct confirmation requires a mechanism description and evidence".to_string(),
        );
    }
    if matches!(
        basis,
        Some("controlled_replicated_intervention" | "combined")
    ) && (!truthy(get(confirmation, "intervention_ref"))
        || number(get(confirmation, "replicated_trial_count"), 0.0) < 2.0
        || !truthy(get(confirmation, "control_treatment_summary")))
    {
        return Err("intervention confirmation requires a controlled design and at least two replicated trials".to_string());
    }
    if !matches!(
        basis,
        Some("direct_mechanistic_evidence" | "controlled_replicated_intervention" | "combined")
    ) {
        return Err("confirmed attribution requires an accepted confirmation basis".to_string());
    }
    Ok(())
}

fn check_decision_gates(obj: &Value) -> std::result::Result<(), String> {
    if !matches!(
        as_text(get(Some(obj), "outcome")),
        Some("approved" | "conditionally_approved")
    ) {
        return Ok(());
    }
    for gate in array(get(Some(obj), "gate_results")) {
        if get(Some(gate), "mandatory").and_then(Value::as_bool) == Some(true)
            && as_text(get(Some(gate), "status")) != Some("pass")
        {
            let gate_id = as_text(get(Some(gate), "gate_id")).unwrap_or("<unknown>");
            return Err(format!(
                "mandatory gate {gate_id} must pass for an approval"
            ));
        }
    }
    Ok(())
}

fn check_intervention_design(obj: &Value) -> std::result::Result<(), String> {
    if as_text(get(Some(obj), "purpose")) != Some("causal_confirmation") {
        return Ok(());
    }
    let design = object_or_empty(Some(obj), "design");
    let design_value = design;
    if number(get(design_value, "changed_factor_count"), 0.0) != 1.0 {
        return Err("causal-confirmation intervention must isolate one changed factor".to_string());
    }
    if number(get(design_value, "planned_trials"), 0.0) < 2.0 {
        return Err(
            "causal-confirmation intervention requires replicated trials (minimum two)".to_string(),
        );
    }
    if get(design_value, "independent_verification_required").and_then(Value::as_bool) != Some(true)
    {
        return Err(
            "causal-confirmation intervention requires independent verification".to_string(),
        );
    }
    if !truthy(get(design_value, "control_condition"))
        || !truthy(get(design_value, "treatment_condition"))
    {
        return Err(
            "causal-confirmation intervention requires explicit control and treatment conditions"
                .to_string(),
        );
    }
    Ok(())
}

fn check_passport_denominators(obj: &Value) -> std::result::Result<(), String> {
    let summary = object_or_empty(Some(obj), "summary");
    let summary_value = summary;
    let planned = number(get(summary_value, "planned_trials"), 0.0);
    let eligible = number(get(summary_value, "eligible_trials"), 0.0);
    let excluded = number(get(summary_value, "excluded_trials"), 0.0);
    let resolved = number(get(summary_value, "resolved_trials"), 0.0);
    let outcomes = [
        "pass_trials",
        "fail_trials",
        "mixed_trials",
        "inconclusive_trials",
    ]
    .iter()
    .map(|key| number(get(summary_value, key), 0.0))
    .sum::<f64>();

    if eligible + excluded > planned {
        return Err("eligible plus excluded trials cannot exceed planned trials".to_string());
    }
    if resolved != outcomes {
        return Err(
            "resolved trial count must equal pass + fail + mixed + inconclusive counts".to_string(),
        );
    }
    if resolved > eligible {
        return Err("resolved trials cannot exceed eligible trials".to_string());
    }
    if resolved > number(get(summary_value, "run_attempts"), 0.0) {
        return Err("resolved trials require at least one run attempt each".to_string());
    }
    Ok(())
}

fn check_benchmark_freeze(obj: &Value) -> std::result::Result<(), String> {
    let benchmark = required_object(obj, "benchmark")?;
    let benchmark_value = Some(benchmark);
    if as_text(get(benchmark_value, "lifecycle_state")) != Some("frozen") {
        return Ok(());
    }
    if !truthy(get(benchmark_value, "content_digest")) || !truthy(get(benchmark_value, "frozen_at"))
    {
        return Err("frozen benchmark requires frozen_at and content_digest".to_string());
    }
    let audit = object_or_empty(benchmark_value, "audit");
    let audit_value = audit;
    if !matches!(
        as_text(get(audit_value, "status")),
        Some("passed" | "passed_with_limitations")
    ) {
        return Err("frozen benchmark requires a passed audit".to_string());
    }
    if !truthy(get(audit_value, "evidence_refs")) {
        return Err(
            "frozen benchmark audit requires at least one supporting evidence reference"
                .to_string(),
        );
    }
    let contamination = as_text(get(audit_value, "contamination_assessment"));
    if contamination == Some("confirmed_contamination") {
        return Err("benchmark with confirmed contamination cannot be frozen".to_string());
    }
    if contamination == Some("possible_contamination")
        && !truthy(get(audit_value, "contamination_mitigation"))
    {
        return Err(
            "possible benchmark contamination requires documented mitigation before freezing"
                .to_string(),
        );
    }
    for finding in array(get(audit_value, "findings")) {
        let severity = as_text(get(Some(finding), "severity"));
        if matches!(severity, Some("critical" | "high"))
            && !truthy(get(Some(finding), "resolution"))
        {
            return Err(format!(
                "{:?}-severity benchmark audit finding requires documented resolution before freezing",
                severity.unwrap_or("unknown")
            ));
        }
    }

    let refs = array(get(benchmark_value, "task_revision_refs"));
    let mut keys = BTreeSet::new();
    for reference in refs {
        let key = tuple_key(&[
            get(Some(reference), "record_type"),
            get(Some(reference), "record_id"),
            get(Some(reference), "revision"),
        ]);
        keys.insert(key);
    }
    if refs.is_empty() || keys.len() != refs.len() {
        return Err("benchmark task revision references must be non-empty and unique".to_string());
    }
    Ok(())
}

fn check_metric_definition(obj: &Value) -> std::result::Result<(), String> {
    let definition = required_object(obj, "definition")?;
    let definition_value = Some(definition);
    let kind = as_text(get(definition_value, "metric_kind"));
    if as_text(get(definition_value, "sampling_unit")) != Some("planned_trial") {
        return Err(
            "inferential metric definitions must use planned_trial as the sampling unit"
                .to_string(),
        );
    }
    if kind == Some("binary_rate")
        && (as_text(get(definition_value, "estimator")) != Some("proportion")
            || !truthy(get(definition_value, "numerator_definition"))
            || !truthy(get(definition_value, "denominator_definition")))
    {
        return Err("binary-rate metrics require proportion estimator and explicit numerator/denominator semantics".to_string());
    }
    if as_text(get(definition_value, "value_direction")) == Some("target_range") {
        let target = object_or_empty(definition_value, "target_range");
        let target_value = target;
        if number(get(target_value, "minimum"), 0.0) > number(get(target_value, "maximum"), 0.0) {
            return Err("metric target range minimum cannot exceed maximum".to_string());
        }
    }
    if as_text(get(definition_value, "estimator")) == Some("quantile") {
        let q = get(definition_value, "quantile").and_then(Value::as_f64);
        if !q.is_some_and(|value| value > 0.0 && value < 1.0) {
            return Err(
                "quantile estimator requires a quantile strictly between zero and one".to_string(),
            );
        }
    }
    let policy = object_or_empty(definition_value, "uncertainty_policy");
    let policy_value = policy;
    if get(policy_value, "required_for_inference").and_then(Value::as_bool) == Some(true)
        && (!truthy(get(policy_value, "confidence_level"))
            || as_text(get(policy_value, "method")) == Some("none_descriptive"))
    {
        return Err(
            "inferential metrics require an uncertainty method and confidence level".to_string(),
        );
    }
    Ok(())
}

fn check_metric_result_arithmetic(obj: &Value) -> std::result::Result<(), String> {
    let result = required_object(obj, "result")?;
    let result_value = Some(result);
    let counts = object_or_empty(result_value, "counts");
    let counts_value = counts;
    let planned = number(get(counts_value, "planned_trial_count"), 0.0);
    let eligible = number(get(counts_value, "eligible_trial_count"), 0.0);
    let excluded = number(get(counts_value, "excluded_trial_count"), 0.0);
    let pending = number(get(counts_value, "pending_trial_count"), 0.0);
    let observed = number(get(counts_value, "observed_trial_count"), 0.0);
    let missing = number(get(counts_value, "missing_trial_count"), 0.0);
    let attempts = number(get(counts_value, "run_attempt_count"), 0.0);

    if eligible + excluded + pending != planned {
        return Err(
            "eligible plus excluded plus pending trials must equal planned trials".to_string(),
        );
    }
    if observed + missing != eligible {
        return Err("observed plus missing metric trials must equal eligible trials".to_string());
    }
    if attempts < observed {
        return Err("run attempt count cannot be lower than observed trial count".to_string());
    }

    if as_text(get(Some(obj), "metric_kind")) == Some("binary_rate")
        || as_text(get(result_value, "value_kind")) == Some("binary_rate")
    {
        let numerator = get(result_value, "numerator").and_then(Value::as_f64);
        let denominator = get(result_value, "denominator").and_then(Value::as_f64);
        let (Some(numerator), Some(denominator)) = (numerator, denominator) else {
            return Err(
                "binary-rate numerator must be present and not exceed denominator".to_string(),
            );
        };
        if numerator > denominator {
            return Err(
                "binary-rate numerator must be present and not exceed denominator".to_string(),
            );
        }
        if denominator == 0.0 {
            if as_text(get(result_value, "result_status")) != Some("insufficient_data")
                || has_non_null(get(result_value, "estimate"))
            {
                return Err("zero-observation binary rate must be insufficient_data with no fabricated estimate".to_string());
            }
        } else {
            if denominator != observed {
                return Err("binary-rate denominator must count observed planned trials, never run attempts".to_string());
            }
            let expected = numerator / denominator;
            let estimate = get(result_value, "estimate").and_then(Value::as_f64);
            if !estimate.is_some_and(|value| (value - expected).abs() <= 1e-9) {
                return Err("binary-rate estimate must equal numerator divided by observed-trial denominator".to_string());
            }
        }
    }

    let uncertainty = get(result_value, "uncertainty");
    if truthy(uncertainty) {
        if number(get(uncertainty, "lower_bound"), 0.0)
            > number(get(uncertainty, "upper_bound"), 0.0)
        {
            return Err("uncertainty lower bound cannot exceed upper bound".to_string());
        }
        if as_text(get(result_value, "value_kind")) == Some("binary_rate")
            && (number(get(uncertainty, "lower_bound"), 0.0) < 0.0
                || number(get(uncertainty, "upper_bound"), 1.0) > 1.0)
        {
            return Err("binary-rate confidence bounds must remain within [0, 1]".to_string());
        }
    }
    Ok(())
}

fn check_comparison_design(obj: &Value) -> std::result::Result<(), String> {
    let comparison = required_object(obj, "comparison")?;
    let comparison_value = Some(comparison);
    let status = as_text(get(comparison_value, "comparison_status"));
    let comparability = object_or_empty(comparison_value, "comparability");
    let comparability_value = comparability;
    let sizes = object_or_empty(comparison_value, "sample_sizes");
    let sizes_value = sizes;
    let design = as_text(get(comparison_value, "comparison_design"));
    let pairs = number(get(sizes_value, "matched_pair_count"), 0.0);
    let baseline = number(get(sizes_value, "baseline_trial_count"), 0.0);
    let treatment = number(get(sizes_value, "treatment_trial_count"), 0.0);

    if pairs > baseline.min(treatment) {
        return Err("matched pair count cannot exceed either condition sample size".to_string());
    }
    if matches!(
        design,
        Some("randomized_paired" | "blocked_paired" | "crossover")
    ) && pairs < 2.0
    {
        return Err("paired/crossover comparison requires at least two matched pairs".to_string());
    }
    if matches!(
        design,
        Some("randomized_unpaired" | "observational_unpaired")
    ) && pairs != 0.0
    {
        return Err("unpaired comparison cannot declare matched pairs".to_string());
    }
    if !same_value(
        get(comparison_value, "metric_definition_digest"),
        get(comparability_value, "metric_definition_digest"),
    ) {
        return Err(
            "comparison metric-definition digest must match its comparability record".to_string(),
        );
    }
    if status == Some("comparable") {
        if truthy(get(comparability_value, "mismatch_dimensions")) {
            return Err("comparable result cannot contain comparability mismatches".to_string());
        }
        for key in [
            "benchmark_content_digest",
            "task_population_digest",
            "metric_definition_digest",
            "eligibility_policy_digest",
        ] {
            if !truthy(get(comparability_value, key)) {
                return Err("comparable result requires all compatibility digests".to_string());
            }
        }
    }

    let inference = object_or_empty(comparison_value, "inference");
    let inference_value = inference;
    let inferential_status = as_text(get(comparison_value, "inferential_status"));
    if status == Some("not_comparable")
        && !matches!(inferential_status, Some("descriptive_only" | "prohibited"))
    {
        return Err(
            "not-comparable result cannot make confirmatory or exploratory inferential claims"
                .to_string(),
        );
    }
    if status == Some("not_comparable")
        && (has_non_null(get(comparison_value, "effect"))
            || has_non_null(get(inference_value, "p_value")))
    {
        return Err(
            "not-comparable result cannot publish a comparative effect or p-value".to_string(),
        );
    }
    if status == Some("conditionally_comparable") {
        if !truthy(get(comparability_value, "limitations")) {
            return Err(
                "conditionally comparable result requires explicit limitations".to_string(),
            );
        }
        if inferential_status == Some("confirmatory") {
            return Err(
                "conditionally comparable result cannot make a confirmatory claim".to_string(),
            );
        }
    }
    if same_value(
        get(comparison_value, "baseline_condition_id"),
        get(comparison_value, "treatment_condition_id"),
    ) {
        return Err("baseline and treatment condition IDs must differ".to_string());
    }
    if as_text(get(inference_value, "multiplicity_method")) == Some("none_predeclared_single_test")
        && number(get(inference_value, "comparison_family_size"), 0.0) != 1.0
    {
        return Err(
            "single-test multiplicity declaration requires comparison_family_size=1".to_string(),
        );
    }
    if inferential_status == Some("confirmatory") {
        let effect = object_or_empty(comparison_value, "effect");
        let effect_value = effect;
        if !has_non_null(get(inference_value, "p_value")) || !truthy(get(effect_value, "interval"))
        {
            return Err(
                "confirmatory comparison requires p-value and effect uncertainty interval"
                    .to_string(),
            );
        }
    }

    let resolved_scopes = get(Some(obj), "resolved_metric_result_scopes");
    if truthy(resolved_scopes) {
        let scopes = array(resolved_scopes);
        if scopes.len() != 2 {
            return Err(
                "comparison must resolve exactly baseline and treatment metric results".to_string(),
            );
        }
        let mut expected_conditions = BTreeSet::new();
        expected_conditions.insert(tuple_key(&[get(comparison_value, "baseline_condition_id")]));
        expected_conditions.insert(tuple_key(&[get(
            comparison_value,
            "treatment_condition_id",
        )]));
        let actual_conditions = scopes
            .iter()
            .map(|scope| tuple_key(&[get(Some(scope), "condition_id")]))
            .collect::<BTreeSet<_>>();
        if actual_conditions != expected_conditions {
            return Err(
                "resolved metric results must match baseline and treatment conditions".to_string(),
            );
        }
        for scope in scopes {
            for key in [
                "benchmark_content_digest",
                "task_population_digest",
                "metric_definition_digest",
                "eligibility_policy_digest",
                "aggregation_level",
            ] {
                if !same_value(get(Some(scope), key), get(comparability_value, key)) {
                    return Err(format!(
                        "baseline/treatment metric result mismatch for {key}"
                    ));
                }
            }
        }
    }

    let effect = object_or_empty(comparison_value, "effect");
    let effect_value = effect;
    let interval = get(effect_value, "interval");
    if truthy(interval)
        && number(get(interval, "lower_bound"), 0.0) > number(get(interval, "upper_bound"), 0.0)
    {
        return Err("comparison interval lower bound cannot exceed upper bound".to_string());
    }
    if matches!(status, Some("not_comparable" | "insufficient_evidence"))
        && inferential_status == Some("confirmatory")
    {
        return Err(
            "not-comparable or insufficient-evidence result cannot be confirmatory".to_string(),
        );
    }
    Ok(())
}

fn check_analysis_scope(obj: &Value) -> std::result::Result<(), String> {
    let analysis = required_object(obj, "analysis")?;
    let analysis_value = Some(analysis);
    for item in array(get(analysis_value, "included_result_scope_assertions")) {
        for key in [
            "condition_id",
            "population_key",
            "task_population_digest",
            "system_fingerprint_digest",
        ] {
            if !same_value(get(Some(item), key), get(analysis_value, key)) {
                return Err(format!("included metric result scope mismatch for {key}"));
            }
        }
    }
    let refs = get(analysis_value, "metric_result_refs");
    if !truthy(refs) {
        return Err("analysis requires at least one metric result reference".to_string());
    }
    let refs_array = array(refs);
    let mut keys = BTreeSet::new();
    for reference in refs_array {
        keys.insert(tuple_key(&[
            get(Some(reference), "record_type"),
            get(Some(reference), "record_id"),
            get(Some(reference), "revision"),
        ]));
    }
    if keys.len() != refs_array.len() {
        return Err("analysis metric result references must be unique".to_string());
    }
    Ok(())
}

fn check_fixture(obj: &Value) -> std::result::Result<(), String> {
    match as_text(get(Some(obj), "kind")) {
        Some("causal_graph") => check_graph(
            get(Some(obj), "graph")
                .ok_or_else(|| "required object 'graph' is missing or invalid".to_string())?,
        ),
        Some("attribution_confirmation") => check_attribution_confirmation(obj),
        Some("decision_gates") => check_decision_gates(obj),
        Some("intervention_design") => check_intervention_design(obj),
        Some("passport_denominators") => check_passport_denominators(obj),
        Some("benchmark_freeze") => check_benchmark_freeze(obj),
        Some("metric_definition") => check_metric_definition(obj),
        Some("metric_result_arithmetic") => check_metric_result_arithmetic(obj),
        Some("comparison_design") => check_comparison_design(obj),
        Some("analysis_scope") => check_analysis_scope(obj),
        other => Err(format!("unsupported domain fixture kind {other:?}")),
    }
}

fn collect_json_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in
        fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            files.push(path);
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(anyhow!("no JSON fixtures found in {}", directory.display()));
    }
    Ok(files)
}

fn read_json(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn validate_domain_fixtures(repository_root: &Path) -> Result<(usize, usize)> {
    let fixture_root = repository_root.join("contracts/domain-fixtures");
    let valid_files = collect_json_files(&fixture_root.join("valid"))?;
    let invalid_files = collect_json_files(&fixture_root.join("invalid"))?;

    for path in &valid_files {
        let fixture = read_json(path)?;
        check_fixture(&fixture).map_err(|message| {
            anyhow!(
                "{} unexpectedly failed domain invariants: {message}",
                path.display()
            )
        })?;
    }

    for path in &invalid_files {
        let fixture = read_json(path)?;
        if check_fixture(&fixture).is_ok() {
            return Err(anyhow!(
                "{} unexpectedly passed domain invariants",
                path.display()
            ));
        }
    }

    Ok((valid_files.len(), invalid_files.len()))
}

/// Validate one domain-invariant fixture using the same rules as the shared fixture suite.
///
/// This accepts the fixture envelope (`kind` plus the record fields) used by
/// `contracts/domain-fixtures`; application services should call the more specific
/// domain APIs as they are introduced rather than inventing fixture envelopes.
pub fn validate_domain_fixture(fixture: &Value) -> std::result::Result<(), String> {
    check_fixture(fixture)
}
