# Benchmark and Metrics Semantics

**Status:** v0 contract review draft. These payloads do not imply that an analysis engine or dashboard is implemented.

## 1. The sample unit is a planned trial

A trial is the planned statistical observation. A run is an execution attempt associated with a trial. Retries, replays, tool-call retries, and infrastructure restarts do not create additional independent observations. A deliberately planned replicate is a new trial with a distinct sampling-unit identity.

For every metric result, counts reconcile as:

- `eligible_trial_count + excluded_trial_count + pending_trial_count = planned_trial_count`
- `observed_trial_count + missing_trial_count = eligible_trial_count`
- `run_attempt_count` is diagnostic only and never replaces a trial denominator.

For a binary rate, `estimate = numerator / denominator`, where `denominator = observed_trial_count` and `numerator <= denominator`. A zero denominator produces an insufficient-data result, not an invented zero rate. A pending or missing trial is not automatically a failure or pass.

## 2. Metric definitions are predeclared

A metric definition fixes its semantic kind, unit, direction, aggregation level, estimator, eligibility rule, missing-data policy, and uncertainty policy. A metric ID is not enough to establish compatibility: comparisons must resolve the exact definition revision and digest. Binary rates explicitly define numerator and denominator. Numeric summaries declare their estimator and, for quantiles, the quantile.

`higher_is_better`, `lower_is_better`, `target_range`, `non_compensatory`, and `no_directional_preference` are distinct semantics. Safety constraints should generally be modeled as non-compensatory gates, not folded into an average.

## 3. Population identity and aggregation

Each metric result is scoped to an evaluation plan, benchmark revision, metric definition, condition, system fingerprint, task population, eligibility policy, and population key. A result from one condition or population must not be silently pooled with another. Aggregation is explicit (`condition`, `benchmark`, `task_family`, or `stratum`). Preserve stratified results and denominators alongside aggregates; a global mean can hide task-mix changes or critical regressions.

## 4. Uncertainty and missingness

Choose uncertainty methods before analysis when the claim is inferential. Preserve method, confidence level, interval, and (where applicable) standard error. Use the correct resampling unit: trial, task cluster, or matched pair. Repeated tasks clustered within a task family may violate independent-trials assumptions; use a cluster-aware design when appropriate.

Missing, excluded, invalidated, and inconclusive dispositions are distinct. Document the predeclared treatment of missingness and conduct sensitivity analysis where missingness could bias results.

## 5. Comparison and comparability

A comparison preserves baseline/treatment metric-result references, design, sample sizes, matched-pair count, compatibility digests, mismatch dimensions, effect estimate, uncertainty, multiplicity method, and practical-significance assessment.

A comparison is `comparable` only when the benchmark revision, task population, metric definition, eligibility policy, and aggregation level align. `conditionally_comparable` requires explicit limitations and cannot be labeled confirmatory. `not_comparable` cannot publish an inferential effect or p-value. `insufficient_evidence` cannot support a confirmatory claim. Services must resolve the referenced metric results and independently compare the actual digests.

Paired designs report matched units, with the count no greater than either condition's trial count. Unpaired designs must not claim matched pairs. Pairing is determined by predeclared sampling-unit identity, not post-hoc matching successful attempts.

## 6. Statistical and practical significance

A p-value is not an effect size and does not express the probability that a hypothesis is true. Report effect, uncertainty, population scope, and limitations together. When multiple metrics, tasks, or subgroups are inspected, declare the comparison family and multiplicity strategy or mark the analysis exploratory. Do not promote post-hoc discoveries to confirmatory status.

Practical significance is distinct from statistical significance. Compare effect and uncertainty with a predeclared operational threshold. A statistically detectable but negligible change is not necessarily meaningful; a practically important but uncertain change remains promising but inconclusive.

## 7. Dashboard rules

- Never rank agents by one unqualified scalar score.
- Always display sample denominator, missing/excluded/pending counts, and uncertainty with rates.
- Preserve safety gates separately; no average compensates for a mandatory gate failure.
- Show task-family/stratum results and material regressions, not only global averages.
- Distinguish confirmatory, exploratory, and descriptive analyses.
- Record every deviation from the frozen analysis plan.
- Treat cost and latency distributions with tail statistics (p50/p90/p95/p99), not only means.
- Never pool retries as independent observations.

## 8. Service-level checks

JSON Schema cannot prove that a reference resolves to the right record, digest, revision, or scope. The application must resolve metric definitions/results and enforce matching condition, fingerprint, task-population, benchmark, eligibility-policy, and plan identities before analysis or comparison. It must also verify trial selection, paired unit identities, immutable source revisions, and analysis provenance.
