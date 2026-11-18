# ADR-0013: Concurrent Attempts and Task-Revision Outcome Rules

- **Status:** Accepted (implemented in `crates/evaluation-store` and `crates/workbench-cli`)
- **Date:** 2026-11-18
- **Builds on:** ADR-0010, ADR-0011, ADR-0012

## Context

`run_ready_trials` executed one attempt at a time, although schedules declare `max_active_runs`. Run outcomes used one fixed, conservative aggregation. The contracts already say how much each criterion matters: a `task_revision` lists `acceptance_criteria` with a `criticality` (`mandatory`, `major`, `minor`), `verification_plan.required_criterion_ids`, and a `validity_state`. The metrics contract adds that no average compensates for a mandatory gate failure.

## Decision

### Concurrency

- `run_ready_trials` keeps up to `max_active_runs` attempts executing. It fills free slots, waits for whichever attempt returns first, completes it (journaled), records it, renews the lease, and refills, including retries the policy allows.
- **No task spawning:** in-flight attempts are polled on the caller's task (`std::future::poll_fn`), so runners can borrow from the caller and the store needs no async runtime. The store calls between polls are short synchronous SQLite writes; they briefly pause polling, which is acceptable for local evaluation.
- Every reservation is still journaled before its attempt starts. If the loop stops on an error, executing attempts are dropped (the local runner kills them) and stay reserved, so the next `acquire` records them as interrupted.
- `DriveReport.peak_concurrency` reports the most attempts that ran at once.

### Outcome rules

When the trial's `task_revision` is stored, its criteria decide each run's outcome (`OutcomePolicy`):

1. If the task revision's `validity_state` is `invalid`, the outcome is `invalid`, whatever was observed.
2. Any failed **mandatory** or **major** criterion → `fail`. A mandatory failure is decisive even when other criteria are unknown.
3. Otherwise, any required criterion that is unjudged, inconclusive, unavailable or mixed → `inconclusive`.
4. Otherwise → `pass`. A failed **minor** criterion does not fail the run, but it stays visible in the verification records and in `verification_state` (`verified_fail` or `mixed`).

A required criterion that's missing from `acceptance_criteria` counts as major. A policy with no required criteria and no verdicts gives `not_scored`. Without a stored task revision, ADR-0011's conservative default applies unchanged. `verification_state` always reports the verdicts themselves; only `run_outcome` applies the policy.

### CLI

- Evaluation files gain an optional `task_revisions` array. Each must be a schema-valid `task_revision` whose `record_id` is some trial's `task_revision_ref`. The records are stored before the run as ordinary records: append-only, and a changed revision under the same ID conflicts.
- `--dry-run` prints the outcome rules and the concurrency limit.
- `run` prints how many attempts ran at once.

## Consequences

- Evaluations finish faster up to their declared limit, and the concurrency is observable and tested (peaks of 1, 2 and 4 for limits 1, 2 and 8 over four trials).
- Outcome rules come from contract records the Workbench already defines, not from a new configuration format.
- There is no per-plan override yet; a different policy needs a new task revision, as the immutability rules intend.
