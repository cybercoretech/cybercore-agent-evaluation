# Workbench Domain Contract Extension — v0 Review Draft

This extension adds the second domain-contract slice to the existing Workbench repository. It is intentionally a schema and fixture package; it does not add runtime code, API endpoints, storage migrations, UI, or hosted-service assumptions.

## Added structural schemas

- `failure`
- `causal-graph` (embedded reusable graph structure)
- `attribution_case`
- `intervention`
- `decision_record`
- `capability_passport`

## Validation coverage

- Structural validation: all schemas are checked against JSON Schema Draft 2020-12; positive and negative record fixtures are checked using the same local reference registry and installed format checkers.
- Domain validation: endpoint resolution, required diagnostic edge direction, causal-edge acyclicity, attribution confirmation evidence, causal-confirmation intervention design, non-compensatory mandatory gates, and passport denominator arithmetic.
- The checks deliberately do not claim database referential integrity, authorization, cryptographic authenticity, or causal truth. Those require implemented domain services, trusted evidence capture, and controlled execution.

## Apply

Extract this package at the Workbench repository root. It contains repository-relative paths and an updated fail-closed `scripts/validate_contracts.py`. Install dependencies with `python3 -m pip install -r requirements-contracts.txt`, then run:

```sh
python3 scripts/validate_contracts.py
python3 scripts/validate_domain_invariants.py
```

All fixtures use synthetic IDs and evidence digests. They are examples only.
