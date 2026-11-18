# Shared Contract Fixtures

These payloads test individual JSON Schema shape and format constraints. They are not a self-contained database snapshot. Record references such as benchmark, verifier, project, and metric-definition IDs are illustrative and are not guaranteed to resolve to fixture records.

- `valid/` contains records expected to pass their corresponding schema.
- `invalid/` contains records expected to fail (missing required fields, invalid date-time, or unknown persisted-record property).

The sample SHA-256 strings and IDs are synthetic placeholders. They are not real integrity hashes and must never be treated as production evidence. Cross-record referential integrity, authorization, immutable revision behavior, retry selection, sampling denominators, and state-transition rules require separate domain tests.


The benchmark/metrics extension adds `benchmark_revision`, `metric_definition`, `metric_result`, `analysis_result`, and `comparison_result` records. Negative examples include invalid confidence levels, frozen revisions without digests, invalid comparison status/claim combinations, missing primary metric identifiers, and inconsistent trial counts.


The benchmark/metrics extension adds benchmark revision, metric definition, metric result, analysis result, and comparison result records. Negative examples cover required field omissions, invalid confidence bounds, invalid status/claim combinations, and frozen revisions without required digests. Domain-only fixtures cover cross-record and arithmetic invariants.
