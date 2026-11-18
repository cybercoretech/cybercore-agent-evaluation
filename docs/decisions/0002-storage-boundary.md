# ADR-0002: Relational Metadata and Separate Evidence Object Storage

- **Status:** Accepted direction; write protocol requires prototype
- **Date:** 2026-11-16

## Context

Evaluation records need relational queries, stable references, and mutable operational projections. Execution traces and artifacts can be large, sensitive, and append-oriented. Local and hosted modes must share the same logical contract without forcing local users to use a cloud service.

## Decision

- Use a metadata-store port. Initial hosted implementation: Cloudflare D1. Proposed local implementation: SQLite behind the same port, with dialect/behavior tests.
- Use an artifact-store port. Initial hosted implementation: private R2. Proposed local implementation: filesystem-backed store.
- Store artifact metadata, provenance, hash, access classification, retention state, and object reference in metadata storage; keep large payloads in object storage.
- Use explicit pending/available/failed/deletion states and reconciliation for cross-store operations. Do not assume a transaction spans D1 and R2.
- Keep raw trace payloads out of ordinary relational rows; store queryable event indexes and references in metadata, with large event segments as versioned artifacts where measurement supports that layout.

## Consequences

- Metadata remains queryable while large evidence storage scales separately.
- Local mode remains independent of Cloudflare credentials.
- Cross-store failures require reconciliation and idempotency.
- D1/R2-specific behavior stays inside adapters; provider-independent domain code sees ports and records only.
