# Domain Invariant Parity

The Workbench has three executable domain-invariant validators:

- `scripts/validate_domain_invariants.py` — the existing Python reference implementation.
- `contracts/conformance/typescript/src/domain-invariants.ts` — TypeScript implementation executed as part of the Ajv conformance test.
- `contracts/conformance/rust/src/domain_invariants.rs` — Rust implementation executed as part of the Rust JSON Schema conformance binary.

All three consume the same fixtures from `contracts/domain-fixtures/valid/` and
`contracts/domain-fixtures/invalid/`. The fixtures are not copied into language-
specific test directories.

## Invariant kinds covered

1. `causal_graph`: endpoint integrity, unique node IDs, no self-edges, diagnostic
   edge direction/type, evidence and rationale for material edges, and acyclicity
   of causal edges.
2. `attribution_confirmation`: confirmed attribution must have direct mechanistic
   evidence or controlled, replicated intervention evidence.
3. `decision_gates`: every mandatory gate must pass before approval or conditional
   approval.
4. `intervention_design`: causal-confirmation interventions isolate one changed
   factor, include at least two planned trials, require independent verification,
   and define control and treatment conditions.
5. `passport_denominators`: planned, eligible, excluded, resolved, outcome, and run
   attempt counts must reconcile under the reference rules.
6. `benchmark_freeze`: frozen benchmarks require a digest, timestamp, passing audit,
   audit evidence, acceptable contamination disposition, resolved critical/high
   findings, and unique non-empty task revision references.
7. `metric_definition`: inferential metrics use planned trials, binary-rate semantics
   are explicit, target ranges are ordered, quantiles are valid, and inferential
   metrics declare uncertainty.
8. `metric_result_arithmetic`: planned/eligible/excluded/pending and
   observed/missing counts reconcile; retries cannot become the metric denominator;
   binary-rate estimates and uncertainty bounds are coherent.
9. `comparison_design`: matched-pair counts, comparison design, compatibility
   digests, inferential restrictions, multiplicity declarations, effect intervals,
   and resolved baseline/treatment scopes are checked.
10. `analysis_scope`: included result scope assertions match the analysis scope and
    metric-result references are present and unique.

## Run all contract checks

Use the environment containing the `jsonschema` and `rfc3339-validator`
dependencies for the Python reference checks:

```bash
PYTHON=/path/to/python-with-contract-dependencies   bash scripts/validate_all_contracts.sh
```

For a machine where the Python dependencies are installed in the active
`python3`, this is simply:

```bash
bash scripts/validate_all_contracts.sh
```

The script runs, in order:

1. Python Draft 2020-12 schema validation.
2. Python domain-invariant reference validation.
3. The existing cross-language runner, which runs the strict-keyword audit,
   TypeScript/Ajv structural and domain checks, and Rust/jsonschema structural
   and domain checks.

The same domain fixture files are consumed by each implementation. Each valid
fixture must be accepted and each invalid fixture must be rejected. The current
fixture set contains 12 valid and 20 invalid domain cases; counts are discovered
from disk rather than hard-coded.

## Parity discipline

When adding or changing a domain invariant:

1. Update the Python reference validator.
2. Add at least one valid fixture and one invalid fixture demonstrating the rule.
3. Implement the same behavior in TypeScript and Rust.
4. Run `scripts/validate_all_contracts.sh`.
5. Review any language-specific differences in numeric handling, null/missing
   values, and collection equality.

Do not duplicate or transform fixtures for a particular language.

## Important boundary

These checks implement the ten invariant kinds represented in the current Python
reference validator and shared domain fixtures. They do **not** yet enforce every
application-level rule listed in `SCHEMA-INVARIANTS.md`.

In particular, authorization and project/workspace ownership, resolving references
to persisted records and immutable revisions, artifact digest verification against
retrieved bytes, lifecycle transition authorization/history, primary-run selection
under a retry policy, full passport-to-decision/fingerprint/scope resolution, and
database transaction constraints still belong in application services and storage
boundaries. Passing these fixture tests is not proof that those service-level
controls have been implemented.
