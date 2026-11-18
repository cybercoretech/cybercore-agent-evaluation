# ADR-0001: Contract-First, Domain-First Development

- **Status:** Accepted
- **Date:** 2026-11-16

## Context

The Workbench needs a TypeScript API, Rust evaluation core/CLI, asynchronous processing, MCP integration, and local/hosted storage. Implementations in different languages and deployment modes must preserve the same domain semantics. Starting with endpoints or ORM models risks creating incompatible copies of the domain model.

## Decision

Design the logical domain model and state machines first. Treat canonical versioned JSON Schema payload contracts as the shared boundary. Describe the HTTP surface with OpenAPI, asynchronous channels with AsyncAPI when needed, and MCP as an adapter protocol. Generated types or clients are derived artifacts and must be tested against the canonical contracts.

Do not create runtime endpoints, CLI commands, MCP tools, or persistent schema migrations until the underlying records, transitions, and invariants have been reviewed.

## Consequences

- Prevents independent adapters from defining subtly different meanings.
- Requires cross-language fixture and compatibility tests.
- Adds upfront design work but reduces migration risk.
- Schema validation alone is not sufficient for domain/business invariants.
