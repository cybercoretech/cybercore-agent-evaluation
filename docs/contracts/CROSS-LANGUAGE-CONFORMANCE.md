# Cross-Language Contract Conformance

**Status:** schema and domain-invariant conformance harnesses implemented; local end-to-end execution remains the acceptance test.

## Goal

The JSON Schema files under `contracts/schemas/v0/` are the sole structural contract source. Python, TypeScript/Node, and Rust validators consume those exact files and shared positive/negative fixtures. Do not hand-maintain language-specific copies of the schemas.

## Selected validator implementations

- **Python:** `jsonschema` 4.x using `Draft202012Validator`, a local `referencing.Registry`, and explicit `FormatChecker` dependency preflight.
- **TypeScript/Node:** Ajv's Draft 2020-12-specific implementation plus `ajv-formats` in full mode. Ajv's default export targets Draft 7, so the harness deliberately imports the 2020-12 implementation.
- **Rust:** `jsonschema` crate 0.61.2, Draft 2020-12-specific options, explicit `.should_validate_formats(true)`, a preloaded in-memory `Registry`, and offline reference resolution.

All implementations must register every local schema before compiling any schema. Missing `$ref` resources must fail closed; validators must not retrieve schema documents from the public network at test time.

## Format assertion is mandatory

Draft 2020-12 separates `format` annotation and assertion vocabularies. The Workbench contract policy treats declared formats used by persisted records (currently `date-time`) as assertions. Each validator must prove this behavior with a deliberately invalid date-time probe before accepting a test run.

Do not treat “the schema contains `format: date-time`” as proof that the validator actually enforces the format. A missing checker is a validation failure, not a warning.

## Shared fixture expectations

Both language runners discover record schemas using the `record_type` constant declared by each record schema. The common envelope schema is not a persisted record type. Each fixture must map to exactly one record schema.

The positive and negative fixture sets must be identical across implementations. Expected outcomes:

- Every file in `contracts/fixtures/valid/` is accepted.
- Every file in `contracts/fixtures/invalid/` is rejected.
- Duplicate schema IDs, unsupported dialects, unresolved references, missing record types, or absent fixtures fail the conformance run.

## Domain-invariant conformance

The Python reference validator, TypeScript validator, and Rust validator now execute the same domain fixtures from `contracts/domain-fixtures/valid/` and `contracts/domain-fixtures/invalid/`. The shared checks cover causal graph integrity and direction, attribution confirmation evidence, mandatory decision gates, causal-confirmation intervention design, capability passport denominators, benchmark freeze/audit rules, metric definition semantics, metric-result arithmetic, comparison design/comparability, and analysis scope.

See `docs/contracts/DOMAIN-INVARIANT-PARITY.md` for the exact invariant list, test workflow, parity discipline, and application-service boundaries. A schema or fixture pass does not replace authorization, persisted-reference resolution, artifact digest verification, lifecycle enforcement, or transactional service-level checks.

## Security and determinism

- Tests must run without network access for schema retrieval.
- Do not permit arbitrary `$ref` retrieval from user-controlled URLs.
- Do not enable coercion, default insertion, or mutation of fixture data.
- Keep validator strictness and format assertion enabled.
- Report schema and fixture counts and compare them between languages.
- Preserve mismatching fixtures as regression tests.

## Upstream conformance

The official JSON Schema Test Suite is a language-agnostic resource for implementations. Workbench-specific fixtures verify our contracts but do not replace the upstream suite. A later CI stage should run the relevant Draft 2020-12 upstream test groups (including format-assertion tests where applicable) against each implementation configuration.

## References

- JSON Schema Draft 2020-12: https://json-schema.org/draft/2020-12
- Ajv JSON Schema versions: https://ajv.js.org/json-schema
- Ajv format validation: https://ajv.js.org/guide/formats.html
- Rust `jsonschema` crate: https://docs.rs/jsonschema/latest/jsonschema/
- JSON Schema Test Suite: https://github.com/json-schema-org/JSON-Schema-Test-Suite

## Static strict-keyword preflight

Before compiling schemas with Ajv, the combined conformance runner executes
`scripts/audit_schema_keyword_types.py`. This standard-library-only check walks
actual schema-valued positions (including nested conditionals, `items`, and
`allOf` branches) and verifies that type-specific keywords have a matching
explicit `type` in the same schema object. It also verifies that each
`required` property name is declared in that schema object's `properties` map.

This preflight is intentionally conservative and does not replace Ajv. Ajv
remains configured with `strict: true`; every schema must still compile and
all positive/negative fixtures must still produce their expected outcomes.


## One-command complete validation

Run `scripts/validate_all_contracts.sh` to execute the Python schema and domain
reference validators followed by the cross-language TypeScript/Ajv and
Rust/jsonschema suites. Set `PYTHON` to the interpreter containing the contract
validation dependencies when they are installed in a virtual environment.
