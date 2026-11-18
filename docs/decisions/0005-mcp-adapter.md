# ADR-0005: MCP as a Separate Application Adapter

- **Status:** Accepted boundary; protocol/SDK pin deferred to integration spike
- **Date:** 2026-11-16

## Context

Agents need a first-class integration surface, but MCP tools must not create a second domain model or bypass project authorization. MCP's 2026-07-28 specification changes the HTTP model to stateless, self-describing requests and updates authorization and client interactions.

## Decision

- Implement MCP as a distinct adapter over the same application services used by the API and CLI.
- Apply authentication and authorization on every request/tool call; never infer authority from a connection or process identity alone.
- Use explicit stable record/job handles for cross-call state rather than hidden transport session state.
- Separate read-only tools from mutating operations. Risky operations must have human approval and visible scope.
- Return compact structured results with stable IDs, provenance/evidence references, pagination, and status handles; avoid returning full trace payloads by default.
- Pin the MCP spec/SDK version only after a TypeScript SDK compatibility spike and interoperability tests with the intended clients.

## Consequences

- MCP remains optional; the product is usable without an MCP client.
- The same authorization and domain invariants apply through every adapter.
- Protocol version changes remain isolated from persisted domain records.
