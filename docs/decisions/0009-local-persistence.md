# ADR-0009: Local Persistence Adapter and Evidence Write Protocol

- **Status:** Accepted (implemented in `crates/evaluation-store` 0.1)
- **Date:** 2026-11-18
- **Builds on:** ADR-0002 (storage boundary), ADR-0006 (Rust core)

## Context

ADR-0002 chose a metadata-store port (SQLite locally, D1 hosted) and an artifact-store port (filesystem locally, R2 hosted), and said the cross-store write protocol needed a prototype. The domain model makes most records immutable or append-only, and the in-memory `TrialCoordinator` leaves identifier uniqueness and idempotency to persistence.

The core is Rust; the hosted control plane is planned as TypeScript on Workers, and the Rust↔Workers integration path is still an open spike (ADR-0003). One Rust trait cannot therefore be "the" port for both modes.

## Decision

- **Separate crate.** Persistence lives in `crates/evaluation-store`; `evaluation-core` does not depend on it (ADR-0006).
- **Shared schema, not shared code.** The cross-language contract is the SQL in `crates/evaluation-store/migrations/` (plain SQLite dialect, applicable to D1 with `wrangler d1 migrations apply`) plus the behaviours below. A future D1 adapter must pass equivalent tests.
- **Validated, append-only records.** Every record is validated against the v0 schemas compiled into the binary before it is written. Records are stored as canonical JSON (sorted keys) with a SHA-256 that is checked on every read. UPDATE and DELETE on `records` are rejected by triggers. Rewriting an identical record is a no-op; a different body under an existing `record_id` is a conflict. Corrections are new records.
- **Reference index.** Every nested `{record_id, record_type}` object (including `provenance.parent_records`) is indexed by JSON Pointer for reverse lookup.
- **Idempotency keys.** A key binds to one request digest and record; replays succeed, reuse for a different request is rejected.
- **Content-addressed evidence objects.** Objects are keyed by SHA-256, written to a temp file, fsynced, atomically renamed, and the directory fsynced. Reads re-verify the digest.
- **Evidence write protocol.** Upload state lives in an operational table (`evidence_uploads`), so the `evidence_artifact` record stays append-only and is written exactly once:
  1. validate the would-be `available` record;
  2. insert a `pending` upload bound to the content digest;
  3. write and verify the object;
  4. in one transaction, append the record (`available`) and mark the upload resolved.

  `reconcile(min_age)` resolves stale `pending` uploads: an intact object completes as `available`, a missing or corrupt one is recorded as `failed` with a reason. Uploads younger than `min_age` are skipped because another process may still be writing them. Orphaned objects are reported, never deleted.
- **Durability over single-write speed.** WAL mode with `synchronous=FULL`. Bulk writers use `put_records` (one transaction per batch).

## Consequences

- Local evaluations persist without Cloudflare. Measured on blackbox (release build): ~14k validated records/s in batches of 500, ~420 single-record transactions/s, ~14 µs verified reads, ~10 ms per fsynced 64 KiB evidence upload.
- `deletion_pending`/`deleted` evidence states, retention enforcement, and record supersession chains are not implemented; they need superseding records and an authorization model first.
- Cross-record domain invariants (`validate_domain_fixture`) are not run on single writes; they need a whole-evaluation view and belong to application services.
- The store API is synchronous (rusqlite). Async callers should use a blocking-task boundary.
- Trial coordinator state is not persisted yet; durable scheduling (the next roadmap item) builds on this store.
