# ADR-0006: Extract Provider-Independent Rust Evaluation Core

- **Status:** Accepted for initial implementation
- **Date:** 2026-11-17

## Context

The Rust conformance executable contained the domain-invariant logic. Leaving the rules inside a test harness would make the application duplicate them or depend on a binary-oriented module. The Workbench also needs a provider-independent runner boundary and explicit run lifecycle behavior.

## Decision

- Create `crates/evaluation-core` as a library crate.
- Move the Rust domain-invariant implementation into the library and make the conformance executable depend on it through a local path dependency.
- Implement a run-state projection with separate execution, outcome, and verification dimensions.
- Define an async, object-safe runner trait using a boxed `Send` future; the core does not mandate Tokio or another runtime.
- Default network and side-effect policies to deny-all. Reject unsupported policy requests before execution.
- Treat one runner invocation as one run attempt. Replicates and retries remain explicit orchestration decisions.
- Keep concrete runner, persistence, HTTP, Cloudflare, MCP, FFI, and sandbox adapters outside the core.

## Consequences

- Rust domain invariants are callable by the future application and tested by the same conformance fixtures.
- Local and hosted runner adapters can implement the same trait.
- The crate is not yet a production executor. Isolation, cancellation, budget enforcement, artifact persistence, authorization, and secret resolution require concrete adapter tests and separate approval.
- The Rust runtime integration and any Workers interoperability mechanism remain open pending a compatibility/performance spike.
