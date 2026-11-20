# ADR-0027: Failure Analysis

- **Status:** Accepted (implemented in `evaluation-store::analysis` (`failures`) and `workbench failures` / `workbench report`)
- **Date:** 2026-11-19
- **Builds on:** ADR-0011 (run and verification records), ADR-0020 (summaries), ADR-0026 (reports)

## Context

Summaries said how often an agent failed, but not what failed. The by-task table in reports showed which tasks failed, but not which check, why, or whether the run had even reached a verifier. The contract defines a `failure` record (and readiness passports cite known failures), but nothing created one.

## Decision

- **Scope.** The analysis covers each trial's **latest attempt**, the same as summaries. The trial data now carries that attempt's run ID (`TrialResult::latest_run_id`).
- **Grouping:**
  - **Criterion failures:** verification results with `fail`, grouped by criterion ID. Each group gives failing trials out of judged trials, the affected tasks (by task key, in first-seen order), failing out of judged per task family, the criterion description from a stored task revision, and up to two model-judge rationales (cut to 300 characters on one line).
  - **No verdict:** `mixed`, `inconclusive` and `unavailable` results, grouped by criterion.
  - **Run errors:** trials without a verdict whose run has no verification results, grouped by the run's `error_summary`.
  - Groups are sorted largest first.
- **Records.** `failure` records are written once, with ID `FAIL-` plus a digest of the run, kind and key:
  - **Criterion failure:** `objective_not_met`, `verified`, with the verification result as evidence and as `verification_result_ref`. The violated requirement is the criterion description.
  - **Run error:** `other`, `unverified`, with the terminal run event as evidence and a verification limitation explaining that no verifier ran.
  - **Common fields:** the decisive event is always the run's terminal event, and `gap_node_refs` is empty (attribution comes later). Runs with no events are shown but not recorded, since the contract needs a decisive event.
  - **No-verdict groups aren't recorded as failures,** because the verifier didn't judge them failed.
- **CLI:**
  - `workbench failures EVAL [--json]` prints the groups and the number of failure records.
  - `workbench report` adds a **Failures** section with the top five groups per evaluation and points to `workbench failures` for the rest. Its provenance table counts the failure records.

## Consequences

- "The sloppy agent failed 10 of 12" becomes "total-written failed on every non-trivial sum, in both families", and broken runs are kept apart from wrong answers.
- Failure records exist for readiness passports to cite.
- **Not covered:**
  - root-cause attribution (`attribution_case`) and gap nodes;
  - clustering free-text judge rationales;
  - failures in attempts other than the latest.
