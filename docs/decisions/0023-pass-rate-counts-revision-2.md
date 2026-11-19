# ADR-0023: Pass-Rate Counts, Revision 2

- **Status:** Accepted (implemented in `evaluation-store::analysis`)
- **Date:** 2026-11-19
- **Amends:** ADR-0020 (cross-run summaries); closes the follow-up noted in ADR-0022

## Context

Revision 1 of `workbench-pass-rate` counted trials without a verdict (inconclusive or not scored) as not passed, which was the owner's choice. So they were in the `denominator`. But the `metric_result` recorded them as `missing_trial_count` and left them out of `observed_trial_count`. The contract's domain invariants require two things:
- observed plus missing equals eligible;
- a binary rate's denominator equals the observed count.

Revision 1 broke the second one whenever any trial had no verdict.

## Decision

- **Revision 2 (`METRIC-WORKBENCH-PASS-RATE-R2`)** records trials without a verdict as **observed as not passed**. So `observed_trial_count` equals the `denominator`, and `missing_trial_count` is 0. The number without a verdict is still stated in the result's limitations, in the analysis result's exclusions summary, and in the CLI's `without_verdict_counted_as_not_passed`.
- **The estimate, interval and decision rule are unchanged.** Only how the counts are labelled changed.
- **`ANALYSIS_VERSION` is now `workbench-analysis/3`**, so summaries write new records instead of reusing revision-1 records with the old counts.
- **Plans frozen under revision 1 stay valid.** `freeze_plan` accepts an existing frozen plan whose only difference is naming the R1 definition, because the declared metric's estimate and rule are the same. Any other change is still refused.

## Consequences

- Recorded pass-rate and speed/cost metric results now satisfy the count invariants. A store test checks them, with trials that have no verdict, are invalid, or haven't run.
- Older stores keep their revision-1 definition and results, which are labelled R1 and `workbench-analysis/1` or `/2`.
