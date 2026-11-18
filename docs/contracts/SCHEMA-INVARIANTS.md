# Contract-Level and Cross-Record Invariants

JSON Schema checks one payload at a time. These rules require domain-service validation, transactional constraints, or analysis checks.

## Identity and scope

- Every project-owned record must resolve to exactly one project and workspace.
- Every reference must resolve to a record of the declared `record_type` and an allowed revision.
- Authorization is checked on the resolved resource, never inferred from client-supplied scope IDs or opaque record IDs.
- Secret values must not appear in fingerprints, event payloads, evidence metadata, logs, or error messages.

## Evaluation sampling

- Every trial belongs to exactly one evaluation and frozen plan.
- Every run belongs to exactly one trial and identifies the actual fingerprint used.
- A retry is another run attempt for the same sampling unit; it is not an additional independent trial.
- Primary attempt selection follows the frozen plan's retry policy. No attempt may become primary through an undocumented post-hoc choice.
- Denominators derive from planned trial assignments and explicit eligibility/exclusion dispositions, never raw run count.
- A deliberate additional replicate is a new trial with a distinct sampling-unit identity.

## Evidence and verification

- An artifact reference is usable only when the artifact lifecycle state is `available` and the object digest/length have been verified.
- Evidence digests are checked against retrieved content where access is authorized.
- A verification result identifies the criterion, inspected target, verifier/version, method, evidence, result, and limitations.
- `pass`, `fail`, and `mixed` verification results require evidence references; `inconclusive` and `unavailable` results require explicit limitations.
- A fingerprint marked `complete` has no unobserved dimensions; a `partial` fingerprint lists at least one reason a dimension is unknown, unobservable, unavailable, redacted, or not applicable.
- Agent-reported completion is not an independent verification result.
- `run_outcome`, `execution_status`, and `verification_state` remain orthogonal.
- Redacted, missing, unavailable, and not-applicable values must not be silently conflated.

## Lifecycle and history

- Only allowed state transitions are accepted; every transition records actor, timestamp, reason, and source revision.
- Frozen evaluation plans, task revisions, and fingerprints are immutable. Changes create new revisions/records.
- Terminal-state reopening requires an explicit, auditable path.
- Derived projections record their source revisions and analysis version.

## Attribution, intervention, decisions, and passports

- Causal edges require evidence references and rationale; `preceded` and `correlated_with` are not causal edges.
- The causal graph must be acyclic; graph structure and edge semantics need domain validation beyond JSON Schema.
- Detection, containment, and reporting gaps are separate references and classifications.
- Attribution confirmation requires direct mechanistic evidence or a suitably controlled and replicated intervention; a single replay or narrative diagnosis is insufficient.
- Readiness decisions are bounded to a workload/configuration scope. Mandatory safety gates are non-compensatory.

- `escaped_detection` must point from an event/error/failure node to a `detection_gap` node; `escaped_containment` and `misreported_as` follow the corresponding containment/reporting direction.
- The causal subgraph is acyclic. `preceded` and `correlated_with` are temporal/statistical only and are excluded from causal-edge reasoning.
- A confirmed attribution requires direct mechanistic evidence establishing the mechanism, or a suitably controlled and replicated intervention. A controlled-intervention confirmation requires at least two planned/observed replicate trials, explicit control and treatment conditions, and independent verification.
- Causal-confirmation interventions isolate one changed factor, define control/treatment conditions, require independent verification, and have a rollback/safety design.
- An approved or conditionally approved readiness decision requires every mandatory gate to pass. Inconclusive or not-applicable mandatory gates do not count as passes. Aggregate scores cannot compensate for a failed mandatory gate.
- An active Capability Passport must resolve to an approved or conditionally approved decision for the same system fingerprint and a scope no broader than the tested scope. Passport trial denominators must reconcile: resolved = pass + fail + mixed + inconclusive; eligible + excluded must not exceed planned; resolved must not exceed eligible.
- A passport is a versioned, time-bounded evidence summary, not universal certification. Material fingerprint, workload, autonomy, permissions, or environment changes may invalidate its scope.


## Benchmark, metrics, aggregation, and comparison

- A frozen benchmark revision has an immutable content digest, frozen timestamp, non-empty unique task revision references, and a passed audit. Confirmed contamination prevents freezing; possible contamination must be documented and bounded.
- Metric definitions predeclare the sampling unit as a planned trial. Run attempts are execution diagnostics, never independent metric samples or denominators.
- Trial dispositions reconcile: `eligible + excluded = planned`; metric observations reconcile: `observed + missing = eligible`. Binary-rate estimates use `numerator / observed_trial_count`; numerator cannot exceed denominator.
- Every metric result is scoped to one evaluation plan, benchmark revision, system fingerprint, condition, task population, and metric-definition revision/digest. Do not pool results with different population keys.
- An analysis result describes one condition and one population. Metric results with another condition, fingerprint, task population, or population key must not be silently included.
- Comparisons require a predeclared design and explicit comparability dimensions: benchmark digest, task-population digest, metric-definition digest, eligibility-policy digest, and aggregation level.
- A `comparable` result has no declared mismatch dimensions. A not-comparable result cannot make confirmatory or exploratory inferential claims. Conditional comparability requires explicit limitations and must be visibly qualified.
- Paired/crossover designs report the matched-pair count; it cannot exceed either condition's trial count. Unpaired designs cannot claim matched pairs.
- Confidence intervals must be ordered, use the declared method and confidence level, and be interpreted within the defined sampling population. p-values require a declared comparison family and multiplicity method.
- Practical significance is distinct from statistical significance. Report effect size, uncertainty, and the predeclared practical threshold; do not treat a small p-value as proof of operational importance.
- Exploratory post-hoc metrics and subgroup analyses are marked as such. Selection of metrics, strata, or stopping rules after observing results must be disclosed as a deviation.


## Benchmark, metrics, aggregation, and comparison

- A frozen benchmark revision has an immutable content digest, frozen timestamp, non-empty unique task revision references, and a passed audit. Confirmed contamination prevents freezing; possible contamination must be documented and bounded.
- Metric definitions predeclare the sample unit as a planned trial. Run attempts are execution diagnostics, never independent metric samples or denominators.
- Trial dispositions reconcile: `eligible + excluded + pending = planned`; metric observations reconcile: `observed + missing = eligible`. Binary-rate estimates use `numerator / observed_trial_count`; numerator cannot exceed denominator.
- Every metric result is scoped to an evaluation plan, benchmark revision, system fingerprint, condition, task population, eligibility policy, metric-definition revision/digest, and population key. Do not pool results with different population keys.
- An analysis result describes one condition and one population. The service must resolve all included metric-result references and reject mismatched condition, fingerprint, task population, or population key.
- Comparisons require a predeclared design and explicit comparability dimensions: benchmark digest, task-population digest, metric-definition digest, eligibility-policy digest, and aggregation level. The service resolves baseline and treatment metric results and compares these dimensions rather than trusting client-supplied assertions.
- A `comparable` result has no declared mismatch dimensions. `conditionally_comparable` requires explicit limitations and cannot be labeled confirmatory. A `not_comparable` result cannot publish a comparative effect or p-value. `insufficient_evidence` cannot support a confirmatory claim.
- Paired/crossover designs report matched units, with the count no greater than either condition's trial count. Unpaired designs must not claim matched pairs. Pairing follows predeclared sampling-unit identity, not post-hoc matching of successful attempts.
- Confidence intervals must be ordered, use the declared method and confidence level, and be interpreted within the defined sampling population. p-values require a declared comparison family and multiplicity method.
- Practical significance is distinct from statistical significance. Report effect size, uncertainty, and the predeclared practical threshold; do not treat a small p-value as proof of operational importance.
- Exploratory post-hoc metrics and subgroup analyses are marked as such. Selection of metrics, strata, or stopping rules after observing results must be disclosed as a deviation.
