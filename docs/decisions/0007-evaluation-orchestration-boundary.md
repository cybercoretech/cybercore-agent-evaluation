# ADR 0007: Evaluation Orchestration Boundary

- **Status:** Accepted for the initial provider-independent core
- **Date:** 2026-11-17

## Context

The Workbench must distinguish planned statistical observations from physical execution attempts. Runner calls may fail transiently and be retried, but retries must not inflate trial counts or statistical denominators. Execution completion also must not be confused with evaluator pass/fail or independent verification.

The first Rust core already defines a runner port and per-run state projection. A minimal orchestration component is needed above those ports, without prematurely choosing a queue, database, async runtime, or hosting platform.

## Decision

Add an in-memory `TrialCoordinator` to `crates/evaluation-core`.

- An evaluation contains a frozen plan reference and a set of planned trials.
- A trial may have one or more distinct run attempts.
- Each attempt receives caller-supplied, globally fresh run, request, and idempotency identifiers.
- Retry policy is explicit and part of the schedule: maximum attempts, retryable runner error classes, explicitly allowed retryable execution failure codes, and timeout policy.
- The coordinator enforces per-instance active-attempt limits and legal trial transitions.
- Runner results pass through the existing result validator before completion is accepted.
- A finished runner execution only yields `awaiting_assessment`; it never assigns evaluator outcome or verifier state.
- The core does not generate identifiers, persist records, launch parallel tasks, or implement distributed locking.

## Consequences

The same domain orchestration logic can be used by a local CLI, hosted worker, or self-hosted service. Durable adapters must add transactional identity reservation, process-independent concurrency limits, persistence-before-dispatch, idempotent dispatch, crash recovery, and reconciliation of ambiguous outcomes. These requirements are not satisfied by the in-memory coordinator alone.

## Alternatives considered

- **Put retries inside `Runner`:** rejected because the runner boundary represents exactly one run attempt; hiding retries there would obscure run identity and sample accounting.
- **Treat each retry as a new trial:** rejected because it biases denominators and treats correlated attempts as independent observations.
- **Implement a distributed scheduler in the core:** deferred because persistence, queue semantics, and hosting would leak into the provider-independent domain layer.
- **Have the runner assign pass/fail:** rejected because the runner executes; evaluators and independent verifiers assess outcomes.
