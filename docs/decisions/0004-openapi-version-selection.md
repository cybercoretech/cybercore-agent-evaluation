# ADR-0004: OpenAPI Version Selection

- **Status:** Proposed — compatibility spike required
- **Date:** 2026-11-16

## Context

The prior companion-skill architecture reference selected OpenAPI 3.1.1 as a conservative initial baseline. The current OpenAPI Initiative publication page now lists 3.2.1, 3.2.0, 3.1.2, and 3.1.1. OpenAPI 3.1 aligns its Schema Object with JSON Schema Draft 2020-12. The Workbench needs reliable TypeScript clients, validation, documentation, and potentially Rust client/server tooling.

## Decision under evaluation

Compare OpenAPI 3.2.1 with 3.1.2 using the actual planned toolchain before freezing `openapi.yaml`. Do not select a version solely because it is newest, and do not assume 3.2.1 support across every generator, validator, gateway, and client. If material incompatibilities remain, use the current 3.1.x patch line and document the limitations.

## Compatibility spike

Create a tiny representative description with:

- Path and operation definitions, reusable components, errors, pagination, and idempotency headers.
- Shared JSON Schema Draft 2020-12 references.
- A large artifact upload/download operation (prefer object-storage handoff instead of embedding payloads in JSON).
- Async job/evaluation handle patterns.
- Security scheme and scoped authorization metadata.

Run it through the chosen linter, spec validator, TypeScript client generator, API documentation renderer, and any planned Rust tooling. Compare semantic fidelity and round-trip behavior.

## Consequences

No version-specific OpenAPI document is created until the spike is reviewed. Domain schemas remain independent of this decision.
