# CyberCore Evaluation Core

Provider-independent Rust library for the CyberCore Agent Evaluation Workbench.

## Current scope (0.3 development)

- Shared Rust domain-invariant implementation used by the conformance executable.
- Explicit per-run execution state machine with separate outcome and verification dimensions.
- Provider-neutral asynchronous runner port, request/capability models, default-deny network and side-effect policies, and normalized result validation.
- In-memory evaluation/trial coordinator with per-instance concurrency reservations, explicit retry policy, unique run/request/idempotency identities, and bounded attempt summaries.
- Rootless Podman container runner (`ContainerRunner`, ADR-0015) with per-trial pinned images, no network, dropped capabilities, read-only root, tmpfs workspace/output and verified cgroup memory/CPU/process limits.
- Linux Bubblewrap local process runner with an initial-executable allowlist, isolated namespaces, bounded tmpfs workspace, read-only task inputs, bounded output capture, and explicit wall-time limits.

## Deliberate non-goals

- Durable container/VM runner and production-grade hostile-code isolation.
- Durable scheduler, distributed semaphore, persistence, idempotency store, or crash recovery.
- Cloudflare, HTTP, MCP, database, object-store, or queue adapters.
- Statistical estimation, grading, or independent verification implementation.
- Exporting the whole workspace. Only regular files a task leaves in `/workspace/output` with `collect_outputs` are collected (ADR-0014); everything else stays ephemeral.
- A guarantee that the local runner is suitable for hostile multi-tenant workloads. It has no aggregate cgroup memory/CPU/process-count limits, and the executable allowlist does not prevent child processes from invoking other binaries visible in `/usr`.
- Secret delivery to task arguments/environment. Those values appear in the host-side Bubblewrap argv; stdout/stderr are also unredacted. Use no secrets or sensitive data with this prototype unless the host and evidence handling are explicitly trusted.
- A guarantee that a runner implementation is isolated. Each adapter must prove its own controls and capabilities.

## Design constraints

- One `Runner::execute` call represents one execution attempt. Retries and planned replicates are separate orchestration concepts.
- A retry creates a new run within the same trial; it never creates a new statistical observation.
- A runner cannot assign an evaluation pass/fail or independent verification result.
- The coordinator is process-local and single-owner. Hosted adapters must enforce identity uniqueness and concurrency transactionally across processes.
- The request carries record references, not raw credentials. Capability grants are references resolved by a trusted adapter.
- Network and side effects default to deny-all. A runner must declare and enforce the requested policy or reject the request before launch.
- Hidden chain-of-thought is not required or accepted as a contract requirement.
- Contract validation and domain invariant validation remain separate checks.

## Use

Run unit tests with:

```sh
cargo test --manifest-path crates/evaluation-core/Cargo.toml
```

After installing Bubblewrap and confirming that the host permits the required unprivileged namespaces, run the real process smoke test with:

```sh
cargo run --manifest-path crates/evaluation-core/Cargo.toml --example local_process_runner_smoke
```

Set `BWRAP` if Bubblewrap is not installed at `/usr/bin/bwrap`. The smoke example uses `/usr/bin/cat` and a read-only fixture file; construction fails rather than falling back if namespace preflight is unavailable.

See `docs/architecture/RUST-EVALUATION-CORE.md`, `docs/architecture/RUNNER-PROTOCOL.md`, `docs/architecture/EVALUATION-ORCHESTRATION.md`, and `docs/architecture/LOCAL-PROCESS-RUNNER.md`.
