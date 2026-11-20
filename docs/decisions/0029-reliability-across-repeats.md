# ADR-0029: Reliability Across Repeats

- **Status:** Accepted (implemented in `evaluation-store::analysis` (`reliability`), `workbench summary`, `workbench report` and readiness gates)
- **Date:** 2026-11-19
- **Builds on:** ADR-0020 (summaries), ADR-0024 (suites with repeats), ADR-0028 (readiness gates)

## Context

The pass rate counts trials. An agent that solves every task half the time and one that solves half the tasks every time can have the same pass rate, but only the second is predictable. A passport is only meaningful if the agent is consistent, and suites already repeat tasks.

## Decision

- **Measure tasks, not trials.** Consider tasks with at least two trials with an outcome, each trial valued by its latest attempt; tasks that ran once are left out and counted. Trials without a verdict count as not passed, as for the pass rate. The measures:
  - **Passes every run:** the share of repeated tasks whose every trial passed.
  - **Passes at least once:** the share with some passing trial.
  - **Flaky tasks:** tasks with both passes and failures, listed with their counts.
  - **Fails every run:** tasks no trial passed.
- **Uncertainty and status.** Ranges are 95% Wilson intervals with the task as the unit. The result is `computed` with at least 10 repeated tasks and no unrun trials, `descriptive_only` otherwise, and `insufficient_data` when no task ran twice (the notes say to add repeats).
- **Computed on demand, not recorded.** Contract metric definitions fix `sampling_unit: planned_trial`, and their count fields are trial counts. A per-task metric would stretch that contract, so reliability isn't written as `metric_result` records, like the per-family capability lines in passports. A per-task sampling unit would be a separate contract decision.
- **Shown in:**
  - `workbench summary`: a reliability line, plus a `reliability` object in `--json`;
  - `workbench report`: a **Reliability** section, with its notes among the caveats.
- **Readiness gates (additive contract enum values):** `all_runs_pass_rate` and `all_runs_pass_rate_lower_bound`, judged only when reliability is `computed`. Their category defaults to `reliability`, and thresholds must be in [0, 1].

## Consequences

- Inconsistent agents are visible, and a passport can require consistency, not just a high average.
- **Not covered:**
  - comparing reliability between two agents (paired by task);
  - pass^k for a chosen k other than "all runs";
  - weighting tasks by their number of runs.
