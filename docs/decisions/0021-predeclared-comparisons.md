# ADR-0021: Predeclared Comparisons and Frozen Analysis Plans

- **Status:** Accepted (implemented in the v0 `evaluation_plan` contract, `evaluation-store::analysis` and `workbench-cli`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0020 (cross-run summaries)

## Context

ADR-0020 comparisons are always exploratory. Without a declaration made in advance, nothing stops someone from choosing the baseline, the significance level or the threshold after seeing the results. So no result could support a claim like "agent B is better than agent A". Every schedule already pointed at a `plan_ref`, but no `evaluation_plan` record was ever written.

## Decision

- **Contract (additive, v0):**
  - `evaluation_plan.analysis_plan.comparisons` is an optional list. Each item has `comparison_id`, `metric_id`, `baseline_condition_id`, `treatment_condition_id`, `design`, `hypothesis` (`two_sided`), `alpha` (greater than 0, at most 0.2), `multiplicity_method`, `minimum_matched_units`, and an optional `practical_significance_threshold`.
  - A valid fixture uses it; an invalid fixture (`alpha` 0.5) is rejected by the Python, TypeScript and Rust conformance suites.
- **Evaluation file:** `analysis_plan` holds `decision_question`, `task_selection_method` (default `curated`) and `comparisons`. Each comparison has a baseline evaluation ID, `alpha` (default 0.05), `minimum_matched_tasks` (default 10, at least 5) and an optional threshold. This evaluation is the treatment and the condition IDs are evaluation IDs. Validation:
  - every trial shares one system fingerprint (one condition per evaluation);
  - no self-comparison;
  - each baseline is declared once;
  - every value is in range.
- **Freezing:**
  - `workbench run` writes the frozen `evaluation_plan` under the schedule's `plan_ref` **before any trial runs**: lifecycle `frozen`, `frozen_at`, and a `content_digest` over the plan's content.
  - The plan also records the workload scope (task families), the operating conditions (runner, network policies, verifier), sampling and retry policy, and the primary metric.
  - Freezing again with identical content is a no-op. **Different content is refused**, because a predeclared plan can't change; a changed plan needs a new `plan_ref` and `evaluation_ref`.
  - Summaries record the frozen plan's digest as `predeclared_plan_digest`.
- **When a comparison is confirmatory.** `compare` finds the declaration in the treatment's frozen plan, by baseline, treatment and metric. It applies the declared rule, unchanged, only when all of these hold:
  - the plan's `frozen_at` is no later than the treatment's first run record;
  - the comparison is `comparable` with an interval (both summaries `computed`, at least 5 paired tasks);
  - the declared minimum of matched tasks is met;
  - the declared design is `blocked_paired`.

  Otherwise the result keeps its exploratory or descriptive status, and a limitation names each unmet condition.
- **The rule:**
  - **Level:** `alpha`, split Bonferroni-style across the plan's declared comparisons. Holm needs the other comparisons' p-values, so a declared Holm is applied as Bonferroni (more conservative) and the result says so.
  - **Test:** McNemar's exact test when every task ran once per side; otherwise, the paired bootstrap interval at 1 − level must exclude 0.
  - **Reporting:** the interval is reported at 1 − level.
  - **Direction:** only from a significant result.
  - **Practical significance** (with a threshold): `meaningful` if the interval lies beyond ±threshold, `not_meaningful` if it lies inside, else `uncertain`.
- **CLI output:** the dry run shows the plan and each declared comparison; `compare` prints `inference confirmatory (predeclared as …)`; the JSON carries `inferential_status`, `declared_comparison_id`, the multiplicity method, family size and practical significance.

## Consequences

- An evaluation can now support a confirmatory "B beats A" claim, and the record shows exactly which frozen declaration the claim rests on.
- A baseline may run before or after the treatment's plan is frozen. Only the treatment's results must come after it, since those are what the declaration predicts.
- **Not covered:**
  - one-sided hypotheses;
  - true Holm across a family;
  - declarations made in the baseline's plan;
  - metrics other than pass rate.
