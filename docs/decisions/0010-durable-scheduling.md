# ADR-0010: Durable Scheduling with a Fenced Write-Ahead Journal

- **Status:** Accepted (implemented in `crates/evaluation-store`, `DurableScheduler`)
- **Date:** 2026-11-18
- **Builds on:** ADR-0007 (in-memory orchestration), ADR-0009 (local persistence)

## Context

`TrialCoordinator` (ADR-0007) makes reservation and retry decisions in memory. It explicitly leaves durability, cross-process identity uniqueness, and multi-process concurrency to a persistence adapter. Losing the process loses the evaluation's progress, and an attempt that was running at the time leaves no trace.

The coordinator is deterministic: the same sequence of `reserve_attempt`, `complete_attempt` and `cancel_trial` calls on the same schedule always produces the same state.

## Decision

- **Journal, not snapshots.** Persist the frozen `EvaluationSchedule` (immutable) and an append-only journal of coordinator commands with their full inputs, including the runner's result. State is rebuilt by replaying the journal through a fresh coordinator. The core stays unchanged apart from serde derives on its schedule/identity/error types.
- **Write-ahead.** The coordinator applies a command in memory, then the journal entry is committed, and only then is the result returned. An `ExecutionRequest` therefore never reaches a runner unless its reservation is durable. If the commit fails, the in-memory coordinator is rebuilt from the journal so memory never runs ahead of disk.
- **One driver per evaluation.** A lease row with an owner, an expiry and a monotonically increasing fencing token (never reused, even after release). Every journal write checks, in the same transaction, that the caller's token is current and unexpired. A scheduler whose lease expired or was taken over gets `LeaseLost` and cannot write. A different owner cannot take an unexpired lease (`LeaseHeld`). The same owner (a restarted process) can take it back at once, which fences out any older instance.
- **Global identity uniqueness.** Run IDs, request IDs and idempotency keys are claimed in `attempt_identities` within the reservation transaction, unique across all evaluations and processes.
- **Interrupted attempts.** On `acquire`, any attempt still reserved after replay is closed as a retryable `Internal` runner error with a fixed message (`INTERRUPTED_ATTEMPT_MESSAGE`). The outcome is genuinely unknown, so the schedule's existing retry policy decides: with `retry_retryable_runner_errors` and attempts left, the trial becomes `RetryReady` with a new identity; otherwise it is `AttemptsExhausted`. There is no hidden or automatic re-execution.
- **Tamper evidence.** Schedules carry a digest checked on load; the journal must be gap-free from `seq` 1, and every entry must replay cleanly, or `acquire` fails with `Integrity`. Triggers make schedules immutable and the journal append-only.

## Consequences

- An evaluation survives process crashes and restarts. Demonstrated end to end with the real Bubblewrap runner (`examples/durable_evaluation.rs --crash-after 2`, then resume).
- Replay cost grows with the journal (one entry per reserve/complete/cancel). This is fine for local evaluations; periodic snapshots can be added later without changing the journal.
- Lease expiry uses wall-clock time on the database host; schedulers must `renew_lease` well inside `lease_ttl`, especially around long runs.
- Run and verification records are still written by the application; the journal is operational state, not a substitute for domain records.
- The hosted equivalent (D1 + Workflows/Durable Objects) can reuse the same tables and protocol; it is not implemented here.
