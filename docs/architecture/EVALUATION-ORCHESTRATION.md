# Evaluation Orchestration and Trial Coordination

**Status:** provider-independent, in-memory Rust coordinator implemented; durable scheduling and execution adapters are not implemented.

## Purpose

The orchestration layer turns a frozen evaluation plan into controlled execution attempts without conflating the statistical unit, the actual run, or a retry.

- **Evaluation:** a campaign with a defined question, plan, benchmark population, and system configuration.
- **Trial:** one planned statistical observation under that evaluation. A trial is the sampling unit used by analysis.
- **Run:** one actual execution attempt for a trial. A retry creates another run within the same trial; it does not create another statistical observation.

The `TrialCoordinator` is a small in-memory domain component. It does not persist records, dispatch jobs, execute concurrency in parallel, or provide a distributed lock. It reserves attempts and validates completion transitions for one coordinator instance.

## Reservation flow

1. Construct the coordinator from an evaluation reference, frozen plan reference, trial definitions, concurrency ceiling, and retry policy.
2. The scheduler chooses a ready trial and supplies fresh run, request, and idempotency identities.
3. `reserve_attempt` atomically checks local concurrency, trial state, attempt limits, and identity uniqueness before returning an `ExecutionRequest`.
4. The caller submits that request through `execute_with_runner`. Runner preflight and normalized-result validation remain mandatory.
5. `complete_attempt` verifies the request correlation again, records a bounded attempt summary, releases the local concurrency slot, and returns a next action.
6. A finished execution moves the trial to `execution_complete` / `awaiting_assessment`. It does not assign pass/fail or independent verification.

## Retry rules

- `max_attempts_per_trial` includes the initial attempt.
- Runner errors are retryable only when marked retryable, enabled by policy, and classified as `Unavailable` or `Internal`. Invalid requests, unsupported policies, and protocol violations are never retried by this policy.
- An execution failure is retryable only when the runner marks it retryable **and** its machine-readable failure code is in the predeclared allowlist.
- Timeouts have their own explicit retry switch.
- Cancellation is terminal and is never automatically retried.
- Every retry requires a new run ID, request ID, and idempotency key. The trial ID remains unchanged.
- Retries are diagnostic execution attempts. They must not inflate the number of independent trials, denominators, or sample size.

Retry policy is part of the frozen evaluation configuration. Changing it mid-evaluation requires a new plan revision or an explicitly versioned continuation policy; do not silently mutate the active schedule.

## State and assessment boundary

`TrialExecutionState` describes orchestration progress only: `ready`, `in_flight`, `retry_ready`, `execution_complete`, `attempts_exhausted`, or `cancelled`.

The `RunState` projection remains responsible for each run's execution lifecycle, evaluator outcome, and independent verification state. These are separate dimensions. The coordinator does not infer task success from `ExecutionDisposition::Finished`.

## Current implementation limits

- State is process-local and lost on restart.
- `max_active_runs` is per coordinator instance, not a distributed concurrency limit.
- Identity uniqueness is process-local; durable idempotency must be enforced transactionally by a persistent adapter.
- The coordinator reserves one attempt at a time; it does not spawn tasks or own an async runtime.
- It does not select trials, randomize order, stratify sampling, schedule wall-clock windows, or calculate metrics.
- It does not implement cancellation of an in-flight runner. That requires a runner cancellation protocol and durable cancellation state.
- It does not write run/trial records or artifacts. Callers must persist reservation and completion records reliably around runner dispatch.

Hosted use requires a durable application service that atomically reserves trial/run identities, enforces global concurrency and idempotency, persists state before dispatch, reconciles ambiguous runner outcomes, and resumes safely after worker restarts.

## Tests

Unit tests cover schedule validation, duplicate trial IDs, local concurrency, run identity uniqueness, retries by failure code, retryable runner errors, timeout policy, terminal cancellation, request/result correlation, and the rule that completed execution awaits separate assessment.
