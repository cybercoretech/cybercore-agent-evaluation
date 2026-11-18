#!/usr/bin/env python3
"""Validate non-structural Workbench domain invariants using JSON fixtures.

No third-party packages are required. Run from the repository root:
    python3 scripts/validate_domain_invariants.py
"""
from __future__ import annotations
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIX = ROOT / "contracts" / "domain-fixtures"
CAUSAL = {"caused", "contributed_to", "enabled", "amplified", "propagated_to", "prevented_recovery"}
DIAGNOSTIC = {
    "escaped_detection": ( {"event", "error", "failure"}, "detection_gap" ),
    "escaped_containment": ( {"event", "error", "failure"}, "containment_gap" ),
    "misreported_as": ( {"event", "error", "failure"}, "reporting_gap" ),
}

class InvariantError(ValueError): pass

def check_graph(graph):
    nodes = graph.get("nodes", [])
    by_id = {n.get("node_id"): n for n in nodes}
    if len(by_id) != len(nodes) or None in by_id:
        raise InvariantError("causal graph node IDs must be present and unique")
    adjacency = {node_id: [] for node_id in by_id}
    for edge in graph.get("edges", []):
        src, dst, rel = edge.get("source_node"), edge.get("target_node"), edge.get("relationship")
        if src not in by_id or dst not in by_id:
            raise InvariantError(f"edge {edge.get('edge_id', '<unknown>')} references a missing endpoint")
        if src == dst:
            raise InvariantError("self-edges are not permitted")
        if rel in DIAGNOSTIC:
            allowed_source, required_target = DIAGNOSTIC[rel]
            if by_id[src].get("node_type") not in allowed_source or by_id[dst].get("node_type") != required_target:
                raise InvariantError(f"{rel} direction/type mismatch: expected event/error/failure -> {required_target}")
        if rel in CAUSAL:
            adjacency[src].append(dst)
        if rel in CAUSAL | set(DIAGNOSTIC):
            if not edge.get("supporting_evidence"):
                raise InvariantError(f"material edge {rel} requires supporting evidence")
            if not edge.get("rationale"):
                raise InvariantError(f"material edge {rel} requires rationale")
    visiting, visited = set(), set()
    def visit(node_id):
        if node_id in visiting:
            raise InvariantError("causal edges contain a directed cycle")
        if node_id in visited: return
        visiting.add(node_id)
        for nxt in adjacency[node_id]: visit(nxt)
        visiting.remove(node_id); visited.add(node_id)
    for node_id in adjacency: visit(node_id)

def check_case(obj):
    if obj.get("status") != "confirmed": return
    c = obj.get("confirmation") or {}
    basis = c.get("basis")
    if basis in {"direct_mechanistic_evidence", "combined"}:
        if not c.get("mechanism_description") or not c.get("mechanism_evidence_refs"):
            raise InvariantError("direct confirmation requires a mechanism description and evidence")
    if basis in {"controlled_replicated_intervention", "combined"}:
        if not c.get("intervention_ref") or c.get("replicated_trial_count", 0) < 2 or not c.get("control_treatment_summary"):
            raise InvariantError("intervention confirmation requires a controlled design and at least two replicated trials")
    if basis not in {"direct_mechanistic_evidence", "controlled_replicated_intervention", "combined"}:
        raise InvariantError("confirmed attribution requires an accepted confirmation basis")

def check_decision(obj):
    if obj.get("outcome") not in {"approved", "conditionally_approved"}: return
    for gate in obj.get("gate_results", []):
        if gate.get("mandatory") is True and gate.get("status") != "pass":
            raise InvariantError(f"mandatory gate {gate.get('gate_id')} must pass for an approval")

def check_intervention(obj):
    if obj.get("purpose") != "causal_confirmation": return
    d = obj.get("design", {})
    if d.get("changed_factor_count") != 1:
        raise InvariantError("causal-confirmation intervention must isolate one changed factor")
    if d.get("planned_trials", 0) < 2:
        raise InvariantError("causal-confirmation intervention requires replicated trials (minimum two)")
    if d.get("independent_verification_required") is not True:
        raise InvariantError("causal-confirmation intervention requires independent verification")
    if not d.get("control_condition") or not d.get("treatment_condition"):
        raise InvariantError("causal-confirmation intervention requires explicit control and treatment conditions")

def check_passport(obj):
    s = obj.get("summary", {})
    planned, eligible, excluded = s.get("planned_trials", 0), s.get("eligible_trials", 0), s.get("excluded_trials", 0)
    resolved = s.get("resolved_trials", 0)
    outcomes = sum(s.get(k, 0) for k in ("pass_trials", "fail_trials", "mixed_trials", "inconclusive_trials"))
    if eligible + excluded > planned:
        raise InvariantError("eligible plus excluded trials cannot exceed planned trials")
    if resolved != outcomes:
        raise InvariantError("resolved trial count must equal pass + fail + mixed + inconclusive counts")
    if resolved > eligible:
        raise InvariantError("resolved trials cannot exceed eligible trials")
    if resolved > s.get("run_attempts", 0):
        raise InvariantError("resolved trials require at least one run attempt each")


def check_benchmark_freeze(obj):
    benchmark = obj['benchmark']
    if benchmark.get('lifecycle_state') != 'frozen':
        return
    if not benchmark.get('content_digest') or not benchmark.get('frozen_at'):
        raise InvariantError('frozen benchmark requires frozen_at and content_digest')
    audit = benchmark.get('audit', {})
    if audit.get('status') not in {'passed', 'passed_with_limitations'}:
        raise InvariantError('frozen benchmark requires a passed audit')
    if not audit.get('evidence_refs'):
        raise InvariantError('frozen benchmark audit requires at least one supporting evidence reference')
    if audit.get('contamination_assessment') == 'confirmed_contamination':
        raise InvariantError('benchmark with confirmed contamination cannot be frozen')
    if audit.get('contamination_assessment') == 'possible_contamination' and not audit.get('contamination_mitigation'):
        raise InvariantError('possible benchmark contamination requires documented mitigation before freezing')
    for finding in audit.get('findings', []):
        if finding.get('severity') == 'critical' and not finding.get('resolution'):
            raise InvariantError('critical unresolved benchmark audit finding prevents freezing')
        if finding.get('severity') == 'high' and not finding.get('resolution'):
            raise InvariantError('high-severity benchmark audit finding requires documented resolution before freezing')
    refs = benchmark.get('task_revision_refs', [])
    keys = [(r.get('record_type'), r.get('record_id'), r.get('revision')) for r in refs]
    if not refs or len(keys) != len(set(keys)):
        raise InvariantError('benchmark task revision references must be non-empty and unique')



def check_metric_definition(obj):
    definition = obj['definition']
    kind = definition.get('metric_kind')
    if definition.get('sampling_unit') != 'planned_trial':
        raise InvariantError('inferential metric definitions must use planned_trial as the sampling unit')
    if kind == 'binary_rate':
        if definition.get('estimator') != 'proportion' or not definition.get('numerator_definition') or not definition.get('denominator_definition'):
            raise InvariantError('binary-rate metrics require proportion estimator and explicit numerator/denominator semantics')
    if definition.get('value_direction') == 'target_range':
        target = definition.get('target_range', {})
        if target.get('minimum', 0) > target.get('maximum', 0):
            raise InvariantError('metric target range minimum cannot exceed maximum')
    if definition.get('estimator') == 'quantile':
        q = definition.get('quantile')
        if q is None or not 0 < q < 1:
            raise InvariantError('quantile estimator requires a quantile strictly between zero and one')
    policy = definition.get('uncertainty_policy', {})
    if policy.get('required_for_inference') is True:
        if not policy.get('confidence_level') or policy.get('method') == 'none_descriptive':
            raise InvariantError('inferential metrics require an uncertainty method and confidence level')


def check_metric_result(obj):
    result = obj['result']
    counts = result.get('counts', {})
    planned = counts.get('planned_trial_count', 0)
    eligible = counts.get('eligible_trial_count', 0)
    excluded = counts.get('excluded_trial_count', 0)
    pending = counts.get('pending_trial_count', 0)
    observed = counts.get('observed_trial_count', 0)
    missing = counts.get('missing_trial_count', 0)
    attempts = counts.get('run_attempt_count', 0)
    if eligible + excluded + pending != planned:
        raise InvariantError('eligible plus excluded plus pending trials must equal planned trials')
    if observed + missing != eligible:
        raise InvariantError('observed plus missing metric trials must equal eligible trials')
    if attempts < observed:
        raise InvariantError('run attempt count cannot be lower than observed trial count')
    if obj.get('metric_kind') == 'binary_rate' or result.get('value_kind') == 'binary_rate':
        numerator, denominator = result.get('numerator'), result.get('denominator')
        if numerator is None or denominator is None or numerator > denominator:
            raise InvariantError('binary-rate numerator must be present and not exceed denominator')
        if denominator == 0:
            if result.get('result_status') != 'insufficient_data' or result.get('estimate') is not None:
                raise InvariantError('zero-observation binary rate must be insufficient_data with no fabricated estimate')
        else:
            if denominator != observed:
                raise InvariantError('binary-rate denominator must count observed planned trials, never run attempts')
            expected = numerator / denominator
            if result.get('estimate') is None or abs(result.get('estimate') - expected) > 1e-9:
                raise InvariantError('binary-rate estimate must equal numerator divided by observed-trial denominator')
    uncertainty = result.get('uncertainty')
    if uncertainty and uncertainty.get('lower_bound', 0) > uncertainty.get('upper_bound', 0):
        raise InvariantError('uncertainty lower bound cannot exceed upper bound')
    if result.get('value_kind') == 'binary_rate' and uncertainty:
        if uncertainty.get('lower_bound', 0) < 0 or uncertainty.get('upper_bound', 1) > 1:
            raise InvariantError('binary-rate confidence bounds must remain within [0, 1]')


def check_comparison(obj):
    comparison = obj['comparison']
    status = comparison.get('comparison_status')
    comp = comparison.get('comparability', {})
    sizes = comparison.get('sample_sizes', {})
    design = comparison.get('comparison_design')
    pairs = sizes.get('matched_pair_count', 0)
    baseline, treatment = sizes.get('baseline_trial_count', 0), sizes.get('treatment_trial_count', 0)
    if pairs > min(baseline, treatment):
        raise InvariantError('matched pair count cannot exceed either condition sample size')
    if design in {'randomized_paired', 'blocked_paired', 'crossover'} and pairs < 2:
        raise InvariantError('paired/crossover comparison requires at least two matched pairs')
    if design in {'randomized_unpaired', 'observational_unpaired'} and pairs != 0:
        raise InvariantError('unpaired comparison cannot declare matched pairs')
    if comparison.get('metric_definition_digest') != comp.get('metric_definition_digest'):
        raise InvariantError('comparison metric-definition digest must match its comparability record')
    if status == 'comparable':
        if comp.get('mismatch_dimensions'):
            raise InvariantError('comparable result cannot contain comparability mismatches')
        digests = [comp.get(k) for k in ('benchmark_content_digest','task_population_digest','metric_definition_digest','eligibility_policy_digest')]
        if any(not d for d in digests):
            raise InvariantError('comparable result requires all compatibility digests')
    if status == 'not_comparable' and comparison.get('inferential_status') not in {'descriptive_only','prohibited'}:
        raise InvariantError('not-comparable result cannot make confirmatory or exploratory inferential claims')
    if status == 'not_comparable' and (comparison.get('effect') is not None or comparison.get('inference', {}).get('p_value') is not None):
        raise InvariantError('not-comparable result cannot publish a comparative effect or p-value')
    if status == 'conditionally_comparable':
        if not comp.get('limitations'):
            raise InvariantError('conditionally comparable result requires explicit limitations')
        if comparison.get('inferential_status') == 'confirmatory':
            raise InvariantError('conditionally comparable result cannot make a confirmatory claim')
    if comparison.get('baseline_condition_id') == comparison.get('treatment_condition_id'):
        raise InvariantError('baseline and treatment condition IDs must differ')
    inference = comparison.get('inference', {})
    if inference.get('multiplicity_method') == 'none_predeclared_single_test' and inference.get('comparison_family_size') != 1:
        raise InvariantError('single-test multiplicity declaration requires comparison_family_size=1')
    if comparison.get('inferential_status') == 'confirmatory':
        if inference.get('p_value') is None or not comparison.get('effect', {}).get('interval'):
            raise InvariantError('confirmatory comparison requires p-value and effect uncertainty interval')
    resolved_scopes = obj.get('resolved_metric_result_scopes', [])
    if resolved_scopes:
        if len(resolved_scopes) != 2:
            raise InvariantError('comparison must resolve exactly baseline and treatment metric results')
        expected_conditions = {comparison.get('baseline_condition_id'), comparison.get('treatment_condition_id')}
        actual_conditions = {scope.get('condition_id') for scope in resolved_scopes}
        if actual_conditions != expected_conditions:
            raise InvariantError('resolved metric results must match baseline and treatment conditions')
        for scope in resolved_scopes:
            for field in ('benchmark_content_digest','task_population_digest','metric_definition_digest','eligibility_policy_digest','aggregation_level'):
                if scope.get(field) != comp.get(field):
                    raise InvariantError(f'baseline/treatment metric result mismatch for {field}')
    effect = comparison.get('effect', {})
    interval = effect.get('interval')
    if interval and interval.get('lower_bound', 0) > interval.get('upper_bound', 0):
        raise InvariantError('comparison interval lower bound cannot exceed upper bound')
    if status in {'not_comparable','insufficient_evidence'} and comparison.get('inferential_status') == 'confirmatory':
        raise InvariantError('not-comparable or insufficient-evidence result cannot be confirmatory')


def check_analysis_scope(obj):
    analysis = obj['analysis']
    assertions = analysis.get('included_result_scope_assertions', [])
    for item in assertions:
        for field in ('condition_id','population_key','task_population_digest','system_fingerprint_digest'):
            if item.get(field) != analysis.get(field):
                raise InvariantError(f'included metric result scope mismatch for {field}')
    result_refs = analysis.get('metric_result_refs', [])
    if not result_refs:
        raise InvariantError('analysis requires at least one metric result reference')
    ids = [(r.get('record_type'),r.get('record_id'),r.get('revision')) for r in result_refs]
    if len(ids) != len(set(ids)):
        raise InvariantError('analysis metric result references must be unique')

CHECKS = {
    "causal_graph": lambda o: check_graph(o["graph"]),
    "attribution_confirmation": check_case,
    "decision_gates": check_decision,
    "intervention_design": check_intervention,
    "passport_denominators": check_passport,
    "benchmark_freeze": check_benchmark_freeze,
    "metric_definition": check_metric_definition,
    "metric_result_arithmetic": check_metric_result,
    "comparison_design": check_comparison,
    "analysis_scope": check_analysis_scope,
}

def main():
    valid = sorted((FIX / "valid").glob("*.json"))
    invalid = sorted((FIX / "invalid").glob("*.json"))
    if not valid or not invalid:
        raise InvariantError("domain fixture valid and invalid directories must both be populated")
    def run(path):
        obj = json.loads(path.read_text(encoding="utf-8"))
        kind = obj.get("kind")
        if kind not in CHECKS: raise InvariantError(f"{path.name}: unsupported fixture kind {kind!r}")
        CHECKS[kind](obj)
    for path in valid:
        run(path)
    for path in invalid:
        try:
            run(path)
        except InvariantError:
            continue
        raise InvariantError(f"{path.relative_to(ROOT)} unexpectedly passed domain invariants")
    print(f"PASS: {len(valid)} domain-valid fixtures; {len(invalid)} domain-invalid fixtures rejected.")
    print("Checked graph integrity/direction/acyclicity, attribution confirmation, intervention design, mandatory gates, passport denominators, benchmark freeze/audit, metric semantics/arithmetic, analysis scope, and comparison design/comparability.")
    return 0

if __name__ == "__main__":
    try: raise SystemExit(main())
    except (InvariantError, OSError, json.JSONDecodeError, KeyError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr); raise SystemExit(1) from exc
