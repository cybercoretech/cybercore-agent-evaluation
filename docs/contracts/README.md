# Contract Strategy

**Status:** canonical JSON Schema payload contracts are now present in the initial `v0` review draft. No runtime API endpoints, database migrations, or CLI commands are implemented by this contract package.

## 1. Contract hierarchy

- **Canonical domain payloads:** JSON Schema Draft 2020-12, under `contracts/schemas/v0/`.
- **HTTP API contract:** OpenAPI; compare current 3.2.x and 3.1.x tool support before selecting the version.
- **Asynchronous channel/message contract:** AsyncAPI 3.1.0 when actual asynchronous channels are implemented.
- **Event envelope:** CloudEvents 1.0 where suitable; domain-specific payload remains separately versioned.
- **Agent integration:** MCP adapter over application services; select and pin protocol/SDK versions after compatibility tests.

These specifications are complementary. None replaces the domain model or cross-record business invariants.

## 2. Current schema slice

The current review draft covers common identity/provenance/evidence references, system fingerprints, task revisions, evaluation plans/campaigns/trials/runs/events, verification results, evidence artifacts, failure records, attribution cases and causal graphs, interventions, decision records, capability passports, benchmark revisions, metric definitions/results, single-condition analysis results, and explicit comparison results. See `SCHEMA-CATALOG.md` for the catalog and `METRICS-AND-COMPARISONS.md` for denominator, aggregation, uncertainty, and comparison semantics. Schema fixtures use intentionally synthetic digests and identifiers; they are examples, not production evidence.

## 3. Schema rules

- Every persisted record includes an explicit `$schema`, `$id`, `record_type`, `schema_version`, stable record ID, creation timestamp, and provenance.
- Schema IDs are currently repository-hosted under the versioned `v0` path. Their final namespace must be reviewed before stable `v1` publication.
- Model unknown, unavailable, redacted, and not-applicable dimensions explicitly; do not encode all four as `null`.
- Enumerations are versioned contracts. Never change an enum's meaning in place.
- Input validation is strict at trust boundaries. Unknown-property handling is explicit per consumer.
- JSON Schema does not enforce authorization, cross-record reference resolution, immutable revision rules, graph acyclicity, causal truth, denominator integrity, decision-to-passport scope alignment, benchmark audit status, metric arithmetic, comparison compatibility, or provenance authenticity.

## 4. Compatibility and code generation

Before selecting validators/generators:

1. Validate the positive and negative fixtures in TypeScript and Rust.
2. Test `$ref`, `unevaluatedProperties`, conditional validation, date-time formats, arrays, and nested records.
3. Check behavior for missing/null values, unknown fields, numeric edge cases, and serialization round trips.
4. Evaluate generated types for semantic fidelity; hand-written runtime models may be preferable for complex domain types when checked against schemas.
5. Ensure future OpenAPI and AsyncAPI documents reference these canonical schemas rather than copying payload definitions.

## 5. Contract testing gates

- Every schema validates against the Draft 2020-12 meta-schema.
- All fixtures validate through the same local reference registry.
- Positive fixtures pass and negative fixtures fail for the intended reason.
- Schema changes are diffed for breaking changes and reviewed.
- Generated types/clients are reproducible and checked for drift.
- API, queue/workflow, MCP, local CLI, and storage adapters are tested against shared fixtures when implemented.

## 6. Validation commands

Install the declared format-aware dependencies, then run both validators from the repository root:

```sh
python3 -m pip install -r requirements-contracts.txt
python3 scripts/validate_contracts.py
python3 scripts/validate_domain_invariants.py
```

The structural validator checks all canonical schemas and record fixtures. The domain validator checks graph direction/acyclicity, confirmation thresholds, intervention design constraints, non-compensatory mandatory gates, and passport denominator arithmetic. These fixture tests are not a replacement for transactional cross-record checks in the application services.


## Cross-language conformance

See [Cross-Language Contract Conformance](CROSS-LANGUAGE-CONFORMANCE.md) for the TypeScript/Ajv and Rust validation harnesses and current parity boundaries.
