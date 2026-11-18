# Rust Evaluation Core

**Status:** provider-independent domain, run-state, runner, in-memory trial orchestration, and a Linux Bubblewrap local process runner prototype implemented; durable adapters remain unimplemented.

## Responsibility

`crates/evaluation-core` owns reusable Rust domain behavior shared by local and hosted workflows. It is not an API server, database layer, cloud adapter, or sandbox.

Current modules:

- `domain_invariants`: shared Rust implementation of the ten domain-invariant fixture kinds.
- `run_state`: legal per-run execution transitions and independent outcome/verification dimensions.
- `runner`: provider-neutral execution port, request and capability models, preflight, and normalized result checks.
- `orchestration`: in-memory evaluation/trial coordination, run-attempt reservation, explicit retry decisions, and per-instance concurrency caps.
- `local_process`: Linux Bubblewrap process execution with an explicit executable allowlist, isolated namespaces, bounded tmpfs scratch space, read-only task inputs, and bounded combined output capture.

## State dimensions

A run has three independent dimensions:

1. `execution_status`: created, queued, starting, running, finalizing, finished, errored, timed_out, cancelled.
2. `run_outcome`: pending, pass, fail, inconclusive, not_scored, invalid.
3. `verification_state`: not_started, in_progress, verified_pass, verified_fail, mixed, inconclusive, unavailable.

Execution transitions do not infer an outcome. Verification transitions do not modify an outcome. A trial can have multiple run attempts, but a retry is not a new trial or an independent statistical observation.

## Orchestration boundary

See [Evaluation Orchestration](EVALUATION-ORCHESTRATION.md). The coordinator is in-memory and single-owner. It reserves one run attempt at a time and applies a frozen retry policy. A successful runner execution advances the trial to awaiting assessment, not pass. Persistence, global concurrency, durable idempotency, dispatch recovery, evaluation scoring, and independent verification belong to application services/adapters above the core.

## Dependency direction

The crate depends on serialization and domain libraries only. It does not depend on Hono, Workers, Cloudflare SDKs, D1, R2, MCP, or a concrete runner. Adapters call the core; the core never calls adapters.

## Conformance

The Rust conformance executable consumes `validate_domain_fixtures` from this crate. The combined validation script runs core unit tests before cross-language schema and domain-invariant conformance.

## Local execution boundary

The initial local process adapter is intentionally narrower than a production hostile-code sandbox. It accepts only configured executable IDs and deny-all network/side-effect policies. It does not export workspace files as artifacts and does not enforce aggregate cgroup memory, CPU, or process-count quotas. See [Local Process Runner](LOCAL-PROCESS-RUNNER.md) before using it for real workloads. For hostile code or multi-tenant execution, use a disposable VM or a separately hardened execution service.
