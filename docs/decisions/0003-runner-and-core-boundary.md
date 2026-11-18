# ADR-0003: Provider-Independent Core and Isolated Runner Boundary

- **Status:** Accepted direction; Rust/Workers interoperability and runner implementation deferred
- **Date:** 2026-11-16

## Context

Agent evaluation requires reproducible logic and may execute untrusted code, call tools, change files, or access external systems. The control plane must not inherit the execution environment's permissions. Local users must be able to run without the hosted service.

## Decision

- Keep domain and evaluation logic independent from Cloudflare APIs/types.
- Build the evaluation core and local CLI in Rust, with versioned JSON contracts between Rust and the TypeScript control plane.
- Define a provider-independent runner protocol. A runner executes the workload in a constrained environment and returns normalized run/event/evidence references.
- Cloudflare Workflows coordinate multi-step jobs; they do not serve as the sandbox for evaluated agent code.
- Do not choose WASM, native process, container, or remote RPC as the Rust-to-Workers integration path until a small compatibility/performance spike demonstrates the best option.
- Require deny-by-default egress, scoped credentials, resource limits, per-run workspace isolation, explicit side-effect approval, and cleanup/rollback behavior from every runner implementation.

## Consequences

- Local and hosted runners can share evaluation semantics.
- Rust/TypeScript boundaries need schema and fixture compatibility tests.
- Cloud execution remains optional until isolation and compatibility are proven.
- Runner security is a launch gate, not a later hardening task.
