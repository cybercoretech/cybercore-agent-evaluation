# Canonical JSON Schema Catalog

**Status:** initial review draft (`v0`, schema package `0.1.0`). These schemas define payload shape, not all domain invariants.

## Versioning and identifiers

Schemas use JSON Schema Draft 2020-12. `$id` values currently point to the repository's versioned raw-content paths under `contracts/schemas/v0/`. This is a provisional public identifier strategy; before a stable `v1` release, decide whether to retain repository-hosted identifiers or move to a CyberCore-controlled schema domain. Do not change a published `$id` casually.

The record-level `schema_version` uses pre-1.0 semantic versions for this draft line. `record_id` uses an opaque bounded string pattern; UUIDv7/ULID/other ID generation remains an implementation decision. Do not infer authorization or tenant scope from an ID.

Digest fields currently validate SHA-256 syntax only. The canonical serialization and exact field-inclusion rules for record/fingerprint/plan digests are deliberately unresolved; do not calculate production digests until that rule is frozen. Repeated `a`/`b` values in fixtures are syntactic placeholders, not hashes of fixture content.

## Schemas included in this iteration

| Schema | Record type | Role |
|---|---|---|
| `common/record-envelope.schema.json` | shared | Record identity, schema version, timestamp, provenance |
| `common/record-reference.schema.json` | embedded | Typed reference to another record |
| `common/provenance.schema.json` | embedded | Source and capture lineage |
| `common/evidence-reference.schema.json` | embedded | Evidence pointer with relationship semantics |
| `system/system-fingerprint.schema.json` | `system_fingerprint` | Snapshot of the evaluated configuration |
| `benchmark/task-revision.schema.json` | `task_revision` | Frozen task specification and verification requirements |
| `evaluation/evaluation-plan.schema.json` | `evaluation_plan` | Experiment design and frozen sampling policy; optional `analysis_plan.comparisons` predeclare pairwise comparisons (ADR-0021) and `analysis_plan.readiness` declares readiness gates in advance (ADR-0028; reliability gate metrics ADR-0029) |
| `evaluation/evaluation.schema.json` | `evaluation` | Campaign lifecycle and progress projection |
| `evaluation/trial.schema.json` | `trial` | Planned sampling unit |
| `evaluation/run.schema.json` | `run` | Actual execution attempt; optional `agent_report.reported_usage` holds agent-reported tokens and their declared-price cost (ADR-0022) |
| `evaluation/run-event.schema.json` | `run_event` | Ordered observable event |
| `verification/verification-result.schema.json` | `verification_result` | Criterion-specific independent verification |
| `evidence/evidence-artifact.schema.json` | `evidence_artifact` | Evidence object metadata and availability state |
| `failure/failure.schema.json` | `failure` | Independently evidenced failure event and separate gap-node references |
| `attribution/causal-graph.schema.json` | embedded | Typed evidence-linked nodes and diagnostic/causal edges |
| `attribution/attribution-case.schema.json` | `attribution_case` | Competing hypotheses, graph revision, and confirmation basis |
| `intervention/intervention.schema.json` | `intervention` | Approved controlled test, target asset revisions, and results |
| `decision/decision-record.schema.json` | `decision_record` | Scope-bounded auditable decision and non-compensatory gates |
| `passport/capability-passport.schema.json` | `capability_passport` | Evidence-bounded capability scope, trial denominators, and constraints |
| `benchmark/benchmark-revision.schema.json` | `benchmark_revision` | Audited, versioned task population and benchmark sampling frame |
| `metrics/metric-definition.schema.json` | `metric_definition` | Predeclared metric semantics, sample unit, estimator, direction, and uncertainty policy |
| `metrics/metric-result.schema.json` | `metric_result` | Condition- and population-scoped estimate with trial denominators and uncertainty |
| `metrics/analysis-result.schema.json` | `analysis_result` | One-condition analysis projection with explicit provenance and deviations |
| `metrics/comparison-result.schema.json` | `comparison_result` | Explicit comparability, design, effect, uncertainty, and multiplicity record |

## Deliberately deferred

The benchmark and metrics layer is included in this review draft. The catalog remains provisional; estimator implementations, calibration, cross-language compatibility, canonical digest calculation, and full cross-record comparability checks must be validated before a stable contract release.

## Validation

Run `python3 scripts/validate_contracts.py` from the repository root after applying this package. The validator uses `jsonschema` 4.x with the Draft 2020-12 implementation and a local registry for `$ref` resolution. It checks schema meta-validity, valid/invalid fixtures, and rejects fixture files that are not associated with a schema. Format validation is enabled for date-time fields.

JSON Schema does **not** enforce cross-record referential integrity, authorization, immutable-history behavior, causal-graph acyclicity, denominator integrity, retry/sample selection, benchmark contamination status, metric arithmetic, comparison compatibility, or semantic state transitions. Those remain application/domain invariants and must have independent tests.

## Additional validation

`python3 scripts/validate_domain_invariants.py` runs positive/negative fixtures for graph endpoint resolution, required diagnostic edge directions, causal-edge acyclicity, strict attribution confirmation, controlled intervention design, mandatory decision gates, capability-passport denominators, benchmark freeze/audit requirements, metric semantics and arithmetic, analysis scope consistency, and paired/unpaired comparison constraints.
