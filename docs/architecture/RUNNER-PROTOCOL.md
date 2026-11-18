# Provider-Independent Runner Protocol

**Status:** Rust port and DTO foundation implemented; an initial Linux Bubblewrap local runner prototype is implemented. Durable/hosted runner adapters are not implemented.

## Contract

A runner accepts one `ExecutionRequest` and returns one normalized `RunnerResult`. Each call maps to one run attempt. Planned trial replication and retry policy belong to evaluation orchestration, not hidden runner behavior.

The request identifies the run, evaluation, trial, task revision, and system fingerprint through stable record references. It carries agent-visible task input, an idempotency key, explicit budgets, network and side-effect policy, and references to scoped capability grants. It must not carry raw credentials or hidden grader material.

## Security defaults

- Network policy defaults to `deny_all`.
- Side-effect policy defaults to `deny_all`.
- A requested allowlist is rejected unless the runner advertises support.
- Side-effect allowlists require an explicit approval reference.
- The runner must enforce budgets; the core checks the normalized result against declared limits.
- Event payloads must contain only observable, authorized, appropriately redacted data.
- Artifact manifests require a content type, unique ID, byte length, and SHA-256 digest.

Capability flags default to false and must be explicitly declared by an adapter; a capability declaration is not proof of isolation. A production adapter must demonstrate filesystem/workspace isolation, process/container boundaries, network enforcement, resource limits, secret scoping, side-effect approval, cleanup, and evidence capture through security tests. Explicit cancellation signaling is not part of this v0.1 port and must be designed and tested before a production adapter claims cancellation support.

## Result semantics

The runner echoes the request ID so the application can reject cross-request result mix-ups. It reports only execution disposition (`finished`, `errored`, `timed_out`, `cancelled`), observable events, output/artifact manifests, resource usage, and an optional sanitized failure description.

The runner does not decide `run_outcome` or `verification_state`. Those are assigned by evaluation and independent verification services.

## Retry and idempotency

`idempotency_key` identifies the requested attempt. A repeated request with the same key must be handled idempotently by the adapter or its caller. This interface does not create a new trial on retry. The orchestrator creates distinct run records and attempt numbers for authorized retries/replays.

## Async boundary

`Runner::execute` returns a boxed `Send` future, making the port usable by different async runtimes without requiring Tokio or another runtime in the core crate. The concrete adapter owns its runtime integration.

## Current local implementation

See [Local Process Runner](LOCAL-PROCESS-RUNNER.md) for the first concrete adapter. It supports only deny-all network and external side-effect policies, uses a read-only input mount and ephemeral bounded tmpfs workspace, and enforces configured wall-time and combined output-capture limits. It does not export workspace files as artifacts and is not sufficient for hostile multi-tenant workloads because aggregate cgroup resource limits are not yet implemented. Do not pass secrets in task arguments or environment values: they are supplied to the host-side Bubblewrap command line.
