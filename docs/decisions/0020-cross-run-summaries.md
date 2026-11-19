# ADR-0020: Cross-Run Summaries and Paired Comparisons

- **Status:** Accepted (implemented in `evaluation-store::analysis` and `workbench summary` / `workbench compare`)
- **Date:** 2026-11-19
- **Builds on:** the v0 metrics contracts (`metric_definition`, `metric_result`, `analysis_result`, `comparison_result`), ADR-0011 (run records), ADR-0013 (outcome rules)

## Context

The workbench recorded every run, but nothing added the runs up. There was no pass rate, no measure of uncertainty, and no way to say whether one agent did better than another on the same tasks.

## Decision

- **Metric `workbench-pass-rate`, revision 1:**
  - **The unit is the planned trial**, judged by its latest attempt.
  - **Pass rate** = passes ÷ trials with an outcome.
  - **Trials without a verdict** (`inconclusive`, `not_scored`) **count as not passed** and are reported as `missing_trial_count`. This was the owner's choice: it's conservative, so an agent that crashes on hard tasks can't look better by skipping them.
  - **`invalid` runs are excluded.**
  - **Trials not run yet**, or still `pending`, are left out and make the result provisional.
- **Uncertainty (95%):**
  - **Wilson's score interval** when every task has one counted trial.
  - **Otherwise a percentile bootstrap that resamples tasks** (a cluster bootstrap), so repeated trials of one task don't count as independent evidence.
  - **No bootstrap interval with fewer than 5 distinct tasks.** Resampling one or two clusters gives a falsely narrow, even zero-width, interval; the first trial run showed exactly that.
  - **Fixed seeds.** Bootstrap seeds derive from the data, so the same results always give the same interval.
- **Result status:**
  - **`computed`** needs at least 10 trials with an outcome, no unrun trials, and an interval.
  - **`descriptive_only`** keeps the estimate and gives the reason.
  - **`insufficient_data`** means no trial has an outcome, so there's no estimate.
- **Comparisons are paired by task revision (`blocked_paired`):**
  - The effect is treatment minus baseline, with a paired bootstrap interval over tasks (also from 5 tasks up).
  - McNemar's exact test is added when every task has one trial on each side.
  - The direction is read from the interval: favors one side only when the interval excludes 0.
  - The comparison is labelled `exploratory`, because nothing about it was predeclared.
  - Evaluations whose task-population or benchmark fingerprints differ are `not_comparable` (`prohibited` inference) and get no effect.
  - The comparison is `insufficient_evidence` when either summary isn't `computed` or no interval exists.
- **Fingerprints:**
  - **Plan:** the canonical stored schedule.
  - **Task population:** the task revisions' canonical records, or their IDs when they aren't stored.
  - **Benchmark:** the shared `benchmark_ref` when there is exactly one, else an ID derived from the task set, so evaluations over the same tasks match.
  - **System fingerprint:** the trials' fingerprint references.
  - **Eligibility policy:** the policy text, hashed into every record so policy changes show up as mismatches.
- **Records:**
  - The metric definition is written once.
  - `metric_result` and `analysis_result` IDs (`MR-`/`AR-` plus a data digest) and `comparison_result` IDs (`CR-` plus a digest of the two results) derive from content, so repeating a command over unchanged results writes nothing new, and new results get new records.
  - Every record passes the store's contract validation.
- **CLI:** `workbench summary EVAL [--json]` and `workbench compare BASELINE TREATMENT [--json]`.

## Consequences

- An evaluation now has a defensible headline number, and two agents or versions can be compared on the same tasks, with the caveats recorded next to the numbers.
- **Not yet covered:**
  - Metrics other than pass rate (cost, latency, tokens).
  - Strata or task-family breakdowns.
  - Predeclared metrics and comparisons in the evaluation file, which would allow `confirmatory` inference.
  - Multiple-comparison correction across families.
  - Practical-significance thresholds, which are recorded as `not_assessed`.
  - Comparisons that use different verifiers on the same tasks aren't detected unless the task revisions differ; the verifier isn't part of the fingerprint yet.
