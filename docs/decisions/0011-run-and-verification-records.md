# ADR-0011: Run and Verification Records from the Scheduler Loop

- **Status:** Accepted (implemented in `crates/evaluation-store`: `run_ready_trials`, `record_completed_runs`, `Verifier`)
- **Date:** 2026-11-18
- **Builds on:** ADR-0009 (local persistence), ADR-0010 (durable scheduling)

## Context

The durable scheduler (ADR-0010) journals every attempt and its runner result, but finished trials were not yet visible as ordinary Workbench records. Three rules constrain how they can appear:

- Records are append-only (ADR-0009), so a `run` cannot be created early and updated as it progresses.
- Independent verification is mandatory. A runner, and the process exit status, never assign pass/fail.
- Outcome follows verification. Errored, timed-out and cancelled executions are not automatically failures, and aggregate verification must keep mixed or unavailable evidence visible (state machines doc).

## Decision

- **Write once, at resolution.** When an attempt completes, the loop writes, in **one transaction**:
  - its `run` record;
  - one `run_event` per runner event (type, actor and time; the event payload is not persisted);
  - one `verification_result` per criterion judged.

  In-flight state stays in the scheduler journal.
- **`Verifier` port.** Independent verifiers hold their own criteria (hidden grader data stays out of `task_input`) and return per-criterion verdicts: `pass`, `fail`, `mixed`, `inconclusive` or `unavailable`. Only executions that finished are verified. `StdoutEqualsVerifier` is a deterministic reference implementation that reports `unavailable` when it has no expectation and `inconclusive` when output was truncated, never `pass`.
- **Aggregation without forcing a binary answer:**
  - all pass → `verified_pass` / outcome `pass`;
  - all fail → `verified_fail` / `fail`;
  - no verdicts or all unavailable → `unavailable` / `not_scored`;
  - pass and fail mixed, or any `mixed` verdict → `mixed` / `inconclusive`;
  - otherwise → `inconclusive` / `inconclusive`.

  Unverified executions are `not_started` / `not_scored`.
- **Evidence.** Optionally, the runner output is stored as an `evidence_artifact` through the ADR-0009 protocol and cited by verifications. Because the local runner does not redact, enabling this is the caller's explicit attestation that outputs are non-sensitive (`redaction_state: not_required`). When it's disabled, verifications cite the run's terminal `run_event`. A definitive verdict with nothing to cite is rejected.
- **Crash-safe projection.** The journal holds every runner result, so `record_completed_runs` rebuilds the records of any completed attempt that has no `run` record (for example after a crash between the journal commit and the record write). `run_ready_trials` runs this catch-up first. Interrupted attempts become `errored` runs whose `error_summary` carries the store's interruption notice.
- **Truthful fields.**
  - `error_summary` keeps failure codes and runner-error kinds, never raw runner messages.
  - `resource_usage` records only measured `wall_time_ms`; the runner's placeholder token and tool-call zeros are omitted.
  - `attempt_kind` is `initial` for attempt 1 and `infrastructure_retry` afterwards, with an `attempt_reason`.
  - Timestamps come from the runner's own events, falling back to the journal's reservation and completion times.

## Consequences

- Finished trials are queryable like any other record: a run's events, verifications and evidence are one `references_to(run_id)` away, and a trial's runs one `references_to(trial_id)` away.
- Demonstrated end to end with the real Bubblewrap runner (`examples/durable_evaluation.rs`): crash mid-run, resume, interrupted attempt recorded and retried, verified passes and a verified fail.
- The loop runs attempts one at a time. Concurrency up to `max_active_runs` is future work.
- Model-judge and human verifiers can implement the same port. Plan-level outcome rules (for example "any failed criterion fails the run") are not modelled yet; the default aggregation is deliberately conservative.
