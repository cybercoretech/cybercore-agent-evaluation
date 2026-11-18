import { readdir, readFile } from "node:fs/promises";
import path from "node:path";

type JsonObject = Record<string, unknown>;

class InvariantError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "InvariantError";
  }
}

function isObject(value: unknown): value is JsonObject {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requiredObject(parent: JsonObject, key: string): JsonObject {
  const value = parent[key];
  if (!isObject(value)) throw new InvariantError(`required object '${key}' is missing or invalid`);
  return value;
}

function objectOrEmpty(parent: JsonObject | undefined, key: string): JsonObject {
  const value = parent?.[key];
  return isObject(value) ? value : {};
}

function arrayOrEmpty(parent: JsonObject | undefined, key: string): unknown[] {
  const value = parent?.[key];
  return Array.isArray(value) ? value : [];
}

function value(parent: JsonObject | undefined, key: string): unknown {
  return parent?.[key];
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function numberValue(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function truthy(value: unknown): boolean {
  if (value === null || value === undefined || value === false) return false;
  if (typeof value === "number") return value !== 0;
  if (typeof value === "string") return value.length > 0;
  if (Array.isArray(value)) return value.length > 0;
  if (isObject(value)) return Object.keys(value).length > 0;
  return true;
}

function sameValue(left: unknown, right: unknown): boolean {
  const leftMissing = left === undefined || left === null;
  const rightMissing = right === undefined || right === null;
  if (leftMissing && rightMissing) return true;
  return left === right;
}

function tupleKey(items: unknown[]): string {
  return JSON.stringify(items);
}

const CAUSAL = new Set([
  "caused",
  "contributed_to",
  "enabled",
  "amplified",
  "propagated_to",
  "prevented_recovery",
]);

const DIAGNOSTIC = new Map<string, { sources: Set<string>; target: string }>([
  ["escaped_detection", { sources: new Set(["event", "error", "failure"]), target: "detection_gap" }],
  ["escaped_containment", { sources: new Set(["event", "error", "failure"]), target: "containment_gap" }],
  ["misreported_as", { sources: new Set(["event", "error", "failure"]), target: "reporting_gap" }],
]);

function checkGraph(graph: JsonObject): void {
  const nodes = arrayOrEmpty(graph, "nodes").map((node) => {
    if (!isObject(node)) throw new InvariantError("causal graph nodes must be objects");
    return node;
  });
  const byId = new Map<string, string>();
  for (const node of nodes) {
    const id = stringValue(value(node, "node_id"));
    if (id === undefined || byId.has(id)) throw new InvariantError("causal graph node IDs must be present and unique");
    byId.set(id, stringValue(value(node, "node_type")) ?? "");
  }

  const adjacency = new Map<string, string[]>();
  for (const id of byId.keys()) adjacency.set(id, []);

  for (const rawEdge of arrayOrEmpty(graph, "edges")) {
    if (!isObject(rawEdge)) throw new InvariantError("causal graph edges must be objects");
    const src = stringValue(value(rawEdge, "source_node"));
    const dst = stringValue(value(rawEdge, "target_node"));
    const rel = stringValue(value(rawEdge, "relationship")) ?? "";
    if (src === undefined || dst === undefined || !byId.has(src) || !byId.has(dst)) {
      throw new InvariantError(`edge ${String(value(rawEdge, "edge_id") ?? "<unknown>")} references a missing endpoint`);
    }
    if (src === dst) throw new InvariantError("self-edges are not permitted");

    const diagnostic = DIAGNOSTIC.get(rel);
    if (diagnostic) {
      if (!diagnostic.sources.has(byId.get(src) ?? "") || byId.get(dst) !== diagnostic.target) {
        throw new InvariantError(`${rel} direction/type mismatch: expected event/error/failure -> ${diagnostic.target}`);
      }
    }

    if (CAUSAL.has(rel)) adjacency.get(src)?.push(dst);

    if (CAUSAL.has(rel) || diagnostic) {
      if (!truthy(value(rawEdge, "supporting_evidence"))) {
        throw new InvariantError(`material edge ${rel} requires supporting evidence`);
      }
      if (!truthy(value(rawEdge, "rationale"))) {
        throw new InvariantError(`material edge ${rel} requires rationale`);
      }
    }
  }

  const visiting = new Set<string>();
  const visited = new Set<string>();
  const visit = (id: string): void => {
    if (visiting.has(id)) throw new InvariantError("causal edges contain a directed cycle");
    if (visited.has(id)) return;
    visiting.add(id);
    for (const next of adjacency.get(id) ?? []) visit(next);
    visiting.delete(id);
    visited.add(id);
  };
  for (const id of adjacency.keys()) visit(id);
}

function checkAttributionConfirmation(obj: JsonObject): void {
  if (value(obj, "status") !== "confirmed") return;
  const c = objectOrEmpty(obj, "confirmation");
  const basis = stringValue(value(c, "basis"));
  if (basis === "direct_mechanistic_evidence" || basis === "combined") {
    if (!truthy(value(c, "mechanism_description")) || !truthy(value(c, "mechanism_evidence_refs"))) {
      throw new InvariantError("direct confirmation requires a mechanism description and evidence");
    }
  }
  if (basis === "controlled_replicated_intervention" || basis === "combined") {
    if (
      !truthy(value(c, "intervention_ref")) ||
      numberValue(value(c, "replicated_trial_count")) < 2 ||
      !truthy(value(c, "control_treatment_summary"))
    ) {
      throw new InvariantError("intervention confirmation requires a controlled design and at least two replicated trials");
    }
  }
  if (!["direct_mechanistic_evidence", "controlled_replicated_intervention", "combined"].includes(basis ?? "")) {
    throw new InvariantError("confirmed attribution requires an accepted confirmation basis");
  }
}

function checkDecisionGates(obj: JsonObject): void {
  if (value(obj, "outcome") !== "approved" && value(obj, "outcome") !== "conditionally_approved") return;
  for (const rawGate of arrayOrEmpty(obj, "gate_results")) {
    if (!isObject(rawGate)) throw new InvariantError("gate result must be an object");
    if (value(rawGate, "mandatory") === true && value(rawGate, "status") !== "pass") {
      throw new InvariantError(`mandatory gate ${String(value(rawGate, "gate_id"))} must pass for an approval`);
    }
  }
}

function checkInterventionDesign(obj: JsonObject): void {
  if (value(obj, "purpose") !== "causal_confirmation") return;
  const design = objectOrEmpty(obj, "design");
  if (numberValue(value(design, "changed_factor_count")) !== 1) {
    throw new InvariantError("causal-confirmation intervention must isolate one changed factor");
  }
  if (numberValue(value(design, "planned_trials")) < 2) {
    throw new InvariantError("causal-confirmation intervention requires replicated trials (minimum two)");
  }
  if (value(design, "independent_verification_required") !== true) {
    throw new InvariantError("causal-confirmation intervention requires independent verification");
  }
  if (!truthy(value(design, "control_condition")) || !truthy(value(design, "treatment_condition"))) {
    throw new InvariantError("causal-confirmation intervention requires explicit control and treatment conditions");
  }
}

function checkPassportDenominators(obj: JsonObject): void {
  const summary = objectOrEmpty(obj, "summary");
  const planned = numberValue(value(summary, "planned_trials"));
  const eligible = numberValue(value(summary, "eligible_trials"));
  const excluded = numberValue(value(summary, "excluded_trials"));
  const resolved = numberValue(value(summary, "resolved_trials"));
  const outcomes = ["pass_trials", "fail_trials", "mixed_trials", "inconclusive_trials"]
    .reduce((total, key) => total + numberValue(value(summary, key)), 0);

  if (eligible + excluded > planned) {
    throw new InvariantError("eligible plus excluded trials cannot exceed planned trials");
  }
  if (resolved !== outcomes) {
    throw new InvariantError("resolved trial count must equal pass + fail + mixed + inconclusive counts");
  }
  if (resolved > eligible) throw new InvariantError("resolved trials cannot exceed eligible trials");
  if (resolved > numberValue(value(summary, "run_attempts"))) {
    throw new InvariantError("resolved trials require at least one run attempt each");
  }
}

function checkBenchmarkFreeze(obj: JsonObject): void {
  const benchmark = requiredObject(obj, "benchmark");
  if (value(benchmark, "lifecycle_state") !== "frozen") return;

  if (!truthy(value(benchmark, "content_digest")) || !truthy(value(benchmark, "frozen_at"))) {
    throw new InvariantError("frozen benchmark requires frozen_at and content_digest");
  }
  const audit = objectOrEmpty(benchmark, "audit");
  if (!["passed", "passed_with_limitations"].includes(stringValue(value(audit, "status")) ?? "")) {
    throw new InvariantError("frozen benchmark requires a passed audit");
  }
  if (!truthy(value(audit, "evidence_refs"))) {
    throw new InvariantError("frozen benchmark audit requires at least one supporting evidence reference");
  }
  const contamination = value(audit, "contamination_assessment");
  if (contamination === "confirmed_contamination") {
    throw new InvariantError("benchmark with confirmed contamination cannot be frozen");
  }
  if (contamination === "possible_contamination" && !truthy(value(audit, "contamination_mitigation"))) {
    throw new InvariantError("possible benchmark contamination requires documented mitigation before freezing");
  }
  for (const rawFinding of arrayOrEmpty(audit, "findings")) {
    if (!isObject(rawFinding)) throw new InvariantError("benchmark audit finding must be an object");
    const severity = value(rawFinding, "severity");
    if ((severity === "critical" || severity === "high") && !truthy(value(rawFinding, "resolution"))) {
      throw new InvariantError(`${severity}-severity benchmark audit finding requires documented resolution before freezing`);
    }
  }

  const refs = arrayOrEmpty(benchmark, "task_revision_refs");
  const keys = refs.map((rawRef) => {
    if (!isObject(rawRef)) throw new InvariantError("benchmark task revision reference must be an object");
    return tupleKey([value(rawRef, "record_type") ?? null, value(rawRef, "record_id") ?? null, value(rawRef, "revision") ?? null]);
  });
  if (refs.length === 0 || new Set(keys).size !== keys.length) {
    throw new InvariantError("benchmark task revision references must be non-empty and unique");
  }
}

function checkMetricDefinition(obj: JsonObject): void {
  const definition = requiredObject(obj, "definition");
  const kind = value(definition, "metric_kind");
  if (value(definition, "sampling_unit") !== "planned_trial") {
    throw new InvariantError("inferential metric definitions must use planned_trial as the sampling unit");
  }
  if (kind === "binary_rate") {
    if (
      value(definition, "estimator") !== "proportion" ||
      !truthy(value(definition, "numerator_definition")) ||
      !truthy(value(definition, "denominator_definition"))
    ) {
      throw new InvariantError("binary-rate metrics require proportion estimator and explicit numerator/denominator semantics");
    }
  }
  if (value(definition, "value_direction") === "target_range") {
    const target = objectOrEmpty(definition, "target_range");
    if (numberValue(value(target, "minimum")) > numberValue(value(target, "maximum"))) {
      throw new InvariantError("metric target range minimum cannot exceed maximum");
    }
  }
  if (value(definition, "estimator") === "quantile") {
    const q = value(definition, "quantile");
    if (typeof q !== "number" || !(q > 0 && q < 1)) {
      throw new InvariantError("quantile estimator requires a quantile strictly between zero and one");
    }
  }
  const policy = objectOrEmpty(definition, "uncertainty_policy");
  if (value(policy, "required_for_inference") === true) {
    if (!truthy(value(policy, "confidence_level")) || value(policy, "method") === "none_descriptive") {
      throw new InvariantError("inferential metrics require an uncertainty method and confidence level");
    }
  }
}

function checkMetricResultArithmetic(obj: JsonObject): void {
  const result = requiredObject(obj, "result");
  const counts = objectOrEmpty(result, "counts");
  const planned = numberValue(value(counts, "planned_trial_count"));
  const eligible = numberValue(value(counts, "eligible_trial_count"));
  const excluded = numberValue(value(counts, "excluded_trial_count"));
  const pending = numberValue(value(counts, "pending_trial_count"));
  const observed = numberValue(value(counts, "observed_trial_count"));
  const missing = numberValue(value(counts, "missing_trial_count"));
  const attempts = numberValue(value(counts, "run_attempt_count"));

  if (eligible + excluded + pending !== planned) {
    throw new InvariantError("eligible plus excluded plus pending trials must equal planned trials");
  }
  if (observed + missing !== eligible) {
    throw new InvariantError("observed plus missing metric trials must equal eligible trials");
  }
  if (attempts < observed) throw new InvariantError("run attempt count cannot be lower than observed trial count");

  if (value(obj, "metric_kind") === "binary_rate" || value(result, "value_kind") === "binary_rate") {
    const numerator = value(result, "numerator");
    const denominator = value(result, "denominator");
    if (typeof numerator !== "number" || typeof denominator !== "number" || numerator > denominator) {
      throw new InvariantError("binary-rate numerator must be present and not exceed denominator");
    }
    if (denominator === 0) {
      const estimate = value(result, "estimate");
      if (
        value(result, "result_status") !== "insufficient_data" ||
        (estimate !== undefined && estimate !== null)
      ) {
        throw new InvariantError("zero-observation binary rate must be insufficient_data with no fabricated estimate");
      }
    } else {
      if (denominator !== observed) {
        throw new InvariantError("binary-rate denominator must count observed planned trials, never run attempts");
      }
      const expected = numerator / denominator;
      const estimate = value(result, "estimate");
      if (typeof estimate !== "number" || Math.abs(estimate - expected) > 1e-9) {
        throw new InvariantError("binary-rate estimate must equal numerator divided by observed-trial denominator");
      }
    }
  }

  const uncertainty = objectOrEmpty(result, "uncertainty");
  if (truthy(value(result, "uncertainty"))) {
    if (numberValue(value(uncertainty, "lower_bound")) > numberValue(value(uncertainty, "upper_bound"))) {
      throw new InvariantError("uncertainty lower bound cannot exceed upper bound");
    }
    if (
      value(result, "value_kind") === "binary_rate" &&
      (numberValue(value(uncertainty, "lower_bound")) < 0 || numberValue(value(uncertainty, "upper_bound"), 1) > 1)
    ) {
      throw new InvariantError("binary-rate confidence bounds must remain within [0, 1]");
    }
  }
}

function checkComparisonDesign(obj: JsonObject): void {
  const comparison = requiredObject(obj, "comparison");
  const status = value(comparison, "comparison_status");
  const comparability = objectOrEmpty(comparison, "comparability");
  const sizes = objectOrEmpty(comparison, "sample_sizes");
  const design = value(comparison, "comparison_design");
  const pairs = numberValue(value(sizes, "matched_pair_count"));
  const baseline = numberValue(value(sizes, "baseline_trial_count"));
  const treatment = numberValue(value(sizes, "treatment_trial_count"));

  if (pairs > Math.min(baseline, treatment)) {
    throw new InvariantError("matched pair count cannot exceed either condition sample size");
  }
  if (["randomized_paired", "blocked_paired", "crossover"].includes(String(design)) && pairs < 2) {
    throw new InvariantError("paired/crossover comparison requires at least two matched pairs");
  }
  if (["randomized_unpaired", "observational_unpaired"].includes(String(design)) && pairs !== 0) {
    throw new InvariantError("unpaired comparison cannot declare matched pairs");
  }
  if (!sameValue(value(comparison, "metric_definition_digest"), value(comparability, "metric_definition_digest"))) {
    throw new InvariantError("comparison metric-definition digest must match its comparability record");
  }
  if (status === "comparable") {
    if (truthy(value(comparability, "mismatch_dimensions"))) {
      throw new InvariantError("comparable result cannot contain comparability mismatches");
    }
    const digests = [
      value(comparability, "benchmark_content_digest"),
      value(comparability, "task_population_digest"),
      value(comparability, "metric_definition_digest"),
      value(comparability, "eligibility_policy_digest"),
    ];
    if (digests.some((digest) => !truthy(digest))) {
      throw new InvariantError("comparable result requires all compatibility digests");
    }
  }
  const inference = objectOrEmpty(comparison, "inference");
  if (status === "not_comparable" && !["descriptive_only", "prohibited"].includes(String(value(comparison, "inferential_status")))) {
    throw new InvariantError("not-comparable result cannot make confirmatory or exploratory inferential claims");
  }
  if (status === "not_comparable" && (
    (value(comparison, "effect") !== undefined && value(comparison, "effect") !== null) ||
    (value(inference, "p_value") !== undefined && value(inference, "p_value") !== null)
  )) {
    throw new InvariantError("not-comparable result cannot publish a comparative effect or p-value");
  }
  if (status === "conditionally_comparable") {
    if (!truthy(value(comparability, "limitations"))) {
      throw new InvariantError("conditionally comparable result requires explicit limitations");
    }
    if (value(comparison, "inferential_status") === "confirmatory") {
      throw new InvariantError("conditionally comparable result cannot make a confirmatory claim");
    }
  }
  if (sameValue(value(comparison, "baseline_condition_id"), value(comparison, "treatment_condition_id"))) {
    throw new InvariantError("baseline and treatment condition IDs must differ");
  }
  if (
    value(inference, "multiplicity_method") === "none_predeclared_single_test" &&
    numberValue(value(inference, "comparison_family_size")) !== 1
  ) {
    throw new InvariantError("single-test multiplicity declaration requires comparison_family_size=1");
  }
  if (value(comparison, "inferential_status") === "confirmatory") {
    const effect = objectOrEmpty(comparison, "effect");
    if (
      (value(inference, "p_value") === undefined || value(inference, "p_value") === null) ||
      !truthy(value(effect, "interval"))
    ) {
      throw new InvariantError("confirmatory comparison requires p-value and effect uncertainty interval");
    }
  }

  const resolvedScopes = arrayOrEmpty(obj, "resolved_metric_result_scopes");
  if (truthy(value(obj, "resolved_metric_result_scopes"))) {
    if (resolvedScopes.length !== 2) {
      throw new InvariantError("comparison must resolve exactly baseline and treatment metric results");
    }
    const expectedConditions = new Set([
      value(comparison, "baseline_condition_id"),
      value(comparison, "treatment_condition_id"),
    ]);
    const actualConditions = new Set(resolvedScopes.map((rawScope) => {
      if (!isObject(rawScope)) throw new InvariantError("resolved metric result scope must be an object");
      return value(rawScope, "condition_id");
    }));
    if (expectedConditions.size !== actualConditions.size || [...expectedConditions].some((item) => !actualConditions.has(item))) {
      throw new InvariantError("resolved metric results must match baseline and treatment conditions");
    }
    for (const rawScope of resolvedScopes) {
      if (!isObject(rawScope)) throw new InvariantError("resolved metric result scope must be an object");
      for (const field of [
        "benchmark_content_digest",
        "task_population_digest",
        "metric_definition_digest",
        "eligibility_policy_digest",
        "aggregation_level",
      ]) {
        if (!sameValue(value(rawScope, field), value(comparability, field))) {
          throw new InvariantError(`baseline/treatment metric result mismatch for ${field}`);
        }
      }
    }
  }

  const effect = objectOrEmpty(comparison, "effect");
  const interval = objectOrEmpty(effect, "interval");
  if (truthy(value(effect, "interval")) && numberValue(value(interval, "lower_bound")) > numberValue(value(interval, "upper_bound"))) {
    throw new InvariantError("comparison interval lower bound cannot exceed upper bound");
  }
  if (["not_comparable", "insufficient_evidence"].includes(String(status)) && value(comparison, "inferential_status") === "confirmatory") {
    throw new InvariantError("not-comparable or insufficient-evidence result cannot be confirmatory");
  }
}

function checkAnalysisScope(obj: JsonObject): void {
  const analysis = requiredObject(obj, "analysis");
  const assertions = arrayOrEmpty(analysis, "included_result_scope_assertions");
  for (const rawItem of assertions) {
    if (!isObject(rawItem)) throw new InvariantError("included result scope assertion must be an object");
    for (const field of ["condition_id", "population_key", "task_population_digest", "system_fingerprint_digest"]) {
      if (!sameValue(value(rawItem, field), value(analysis, field))) {
        throw new InvariantError(`included metric result scope mismatch for ${field}`);
      }
    }
  }
  const refs = arrayOrEmpty(analysis, "metric_result_refs");
  if (!truthy(value(analysis, "metric_result_refs"))) {
    throw new InvariantError("analysis requires at least one metric result reference");
  }
  const keys = refs.map((rawRef) => {
    if (!isObject(rawRef)) throw new InvariantError("analysis metric result reference must be an object");
    return tupleKey([
      value(rawRef, "record_type") ?? null,
      value(rawRef, "record_id") ?? null,
      value(rawRef, "revision") ?? null,
    ]);
  });
  if (new Set(keys).size !== keys.length) {
    throw new InvariantError("analysis metric result references must be unique");
  }
}

function checkFixture(obj: JsonObject): void {
  switch (value(obj, "kind")) {
    case "causal_graph":
      checkGraph(requiredObject(obj, "graph"));
      return;
    case "attribution_confirmation":
      checkAttributionConfirmation(obj);
      return;
    case "decision_gates":
      checkDecisionGates(obj);
      return;
    case "intervention_design":
      checkInterventionDesign(obj);
      return;
    case "passport_denominators":
      checkPassportDenominators(obj);
      return;
    case "benchmark_freeze":
      checkBenchmarkFreeze(obj);
      return;
    case "metric_definition":
      checkMetricDefinition(obj);
      return;
    case "metric_result_arithmetic":
      checkMetricResultArithmetic(obj);
      return;
    case "comparison_design":
      checkComparisonDesign(obj);
      return;
    case "analysis_scope":
      checkAnalysisScope(obj);
      return;
    default:
      throw new InvariantError(`unsupported domain fixture kind ${String(value(obj, "kind"))}`);
  }
}

async function fixtureFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  return entries
    .filter((entry) => entry.isFile() && entry.name.endsWith(".json"))
    .map((entry) => path.join(directory, entry.name))
    .sort();
}

async function loadFixture(filePath: string): Promise<JsonObject> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(await readFile(filePath, "utf8")) as unknown;
  } catch (error) {
    throw new Error(`could not parse ${filePath}: ${String(error)}`);
  }
  if (!isObject(parsed)) throw new Error(`domain fixture must be a JSON object: ${filePath}`);
  return parsed;
}

export async function validateDomainFixtures(repositoryRoot: string): Promise<void> {
  const fixtureRoot = path.join(repositoryRoot, "contracts/domain-fixtures");
  const validFiles = await fixtureFiles(path.join(fixtureRoot, "valid"));
  const invalidFiles = await fixtureFiles(path.join(fixtureRoot, "invalid"));
  if (validFiles.length === 0 || invalidFiles.length === 0) {
    throw new Error("domain fixture valid and invalid directories must both be populated");
  }

  for (const filePath of validFiles) {
    const fixture = await loadFixture(filePath);
    try {
      checkFixture(fixture);
    } catch (error) {
      throw new Error(`${path.relative(repositoryRoot, filePath)} unexpectedly failed domain invariants: ${String(error)}`);
    }
  }

  for (const filePath of invalidFiles) {
    const fixture = await loadFixture(filePath);
    let rejected = false;
    try {
      checkFixture(fixture);
    } catch (error) {
      if (error instanceof InvariantError) rejected = true;
      else throw error;
    }
    if (!rejected) {
      throw new Error(`${path.relative(repositoryRoot, filePath)} unexpectedly passed domain invariants`);
    }
  }

  console.log(`PASS: ${validFiles.length} domain-valid fixtures; ${invalidFiles.length} domain-invalid fixtures rejected.`);
  console.log("Checked graph integrity/direction/acyclicity, attribution confirmation, intervention design, mandatory gates, passport denominators, benchmark freeze/audit, metric semantics/arithmetic, analysis scope, and comparison design/comparability.");
}
