# Execution, Events, and Evidence Storage

**Version:** 0.1 — proposed design  
**Status:** storage invariants are foundational; object layout and ingestion batching require a prototype

## 1. Core rule

Execution evidence is a primary product asset, not debug output. Every material result must be traceable to the exact plan, benchmark/task revision, system fingerprint, trial, run, verifier, and evidence. Preserve raw evidence separately from summaries and interpretations.

## 2. Metadata versus payload

### Structured metadata

D1 is the initial hosted metadata store for relational records, status projections, key relationships, indexes, idempotency records, and audit/outbox state.

Proposed local equivalent: SQLite behind the same metadata-store port. Share logical migrations and test actual SQLite/D1 compatibility rather than assuming that every SQLite feature behaves identically in both adapters.

### Large artifacts

R2 is the initial hosted object store for large trace segments, binary outputs, screenshots, attachments, and exported reports. The local adapter uses a filesystem-backed object store with equivalent metadata and integrity behavior.

Keep artifact metadata separate from content. Metadata includes:

- Artifact ID and schema/manifest version.
- Owning workspace/project and source run/event/task.
- Media/content type, encoding, byte size, and capture time.
- Object-store adapter and opaque object reference.
- SHA-256 digest and, where useful, per-chunk digest/manifest.
- Redaction state, access classification, retention policy, and deletion state.
- Provenance/capture version and factual description.

A hash detects content mismatch; it does not prove authorship, authorization, or truth.

## 3. Artifact write protocol

D1 and R2 do not provide one shared atomic transaction. Model artifact creation as a recoverable state machine rather than pretending the two writes are atomic.

Proposed flow:

1. Authorize the caller and validate project scope, quotas, declared content type, and maximum size.
2. Create an artifact metadata record in `pending_upload` state with a unique opaque artifact ID and intended object key.
3. Upload to a private R2 bucket using create-only conditional semantics where supported (`If-None-Match: *`); do not use user-supplied filenames as object keys.
4. Verify committed object metadata, byte length, and checksum against the upload manifest. For ambiguous upload outcomes, reconcile before deciding whether a retry is safe.
5. Transition metadata to `available` only after verification.
6. A reconciliation job handles stale pending records, orphaned objects, failed finalization, and deletion retries.

If an upload fails, preserve an explicit failed/pending state and reason. Never return a usable artifact reference before the object is confirmed available.

R2 currently documents strong global read-after-write and delete consistency and atomic conditional writes. Those guarantees do not eliminate in-flight ambiguous-write handling or cross-service transaction concerns; the metadata workflow must still reconcile them.

## 4. Trace/event representation

Keep event metadata queryable without copying every large payload into the relational database.

Proposed split:

- `RunEvent` or event-index rows contain stable event ID, run ID, per-run sequence, event type, actor/component, event time, correlation/parent IDs, payload artifact reference (when any), redaction metadata, and source/provenance.
- Full request/response bodies, terminal output, large tool results, and raw spans are stored as evidence artifacts.
- High-volume event batches may be stored as versioned, ordered JSONL/NDJSON artifacts with an index in D1. Compression and chunk size are decided by measurement.
- Summaries and extracted fields are derived records; retain a pointer to the raw source and extraction version.

Do not store hidden chain-of-thought as a required trace element. Collect only observable data the evaluator is authorized to retain.

## 5. Event publication and idempotency

- Queue messages carry stable event/message IDs and versioned payload references, not unbounded raw trace content.
- Treat Cloudflare Queues as at-least-once delivery. Handlers must be idempotent; use a stable message ID as an idempotency key and enforce uniqueness at the application/store layer.
- Use a transactional outbox pattern for domain changes that must reliably publish follow-up work: write the state change and outbox row in one metadata transaction/batch, then dispatch. A dispatcher may send more than once; consumers deduplicate.
- Store processing attempts, dead-letter disposition, and replay history. Replays must be visible and must not silently duplicate evaluation samples or interventions.

## 6. Durable workflow

Use Workflows for multi-step jobs whose steps depend on each other: prepare an evaluation, allocate trials, launch runners, collect results, verify outcomes, calculate metrics, build attribution candidates, and finalize reports. Store each step’s durable output and make side effects idempotent.

Use a Queue for decoupled delivery/fan-out and a Workflow for durable orchestration. A Workflow coordinates a runner; it is not itself a sandbox and must not execute untrusted agent code inside the control-plane worker.

## 7. Evidence trust and transformations

Each derived artifact records:

- Source artifact/event IDs and source digests.
- Transformation name/version and configuration revision.
- Actor or service identity and timestamp.
- Redaction applied and fields removed.
- Output digest and limitations.

Model-generated summaries are interpretations, not primary evidence. Preserve the underlying source when permitted, link summaries to exact evidence, and label model-generated content.

## 8. Retention and deletion

Define retention at the workspace/project/artifact policy level before hosted alpha. Distinguish logical deletion, object deletion, backup retention, legal holds, and audit retention. Record deletion requests and completion; avoid making “deleted” claims while object deletion is pending. Deletion must handle metadata and blob storage with reconciliation because they cannot be committed atomically.

## 9. Prototype acceptance tests

- Upload interrupted before object commit.
- Upload committed but response lost; safe reconciliation/retry.
- Object commit succeeds but metadata finalization fails.
- Duplicate queue delivery and duplicate workflow step execution.
- Two concurrent create-only writes to the same artifact key.
- Hash mismatch, truncated payload, wrong declared media type.
- Access to an artifact from another project/workspace.
- Redaction of secrets and personal data before persistence/export.
- Retention expiry and deletion reconciliation.
- Local filesystem adapter and R2 adapter produce equivalent artifact manifests.
