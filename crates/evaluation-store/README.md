# 🗄️ CyberCore Evaluation Store

Local persistence for the CyberCore Agent Evaluation Workbench: a SQLite metadata store and a filesystem evidence store, joined by the ADR-0002 write protocol. Design and trade-offs: [ADR-0009](../../docs/decisions/0009-local-persistence.md).

## 📦 What it gives you

- **`SqliteMetadataStore`:** append-only records, validated against the embedded v0 contracts, with tamper-evident bodies, a reverse reference index, paging and idempotency keys.
- **`FsArtifactStore`:** content-addressed, atomically written, verified-on-read evidence objects behind the `ArtifactStore` port.
- **`LocalStore`:** both together, laid out as `<dir>/metadata.sqlite3` and `<dir>/evidence/`. It adds `put_evidence`, `read_evidence`, `reconcile` and `orphaned_objects`.

## 🚀 Use

```rust
use cybercore_evaluation_store::LocalStore;

let store = LocalStore::open(".workbench")?;
store.metadata().put_records(&records)?;           // all-or-nothing batch
let run = store.metadata().get_record("RUN-0001")?;
let runs_of_trial = store.metadata().references_to("TRIAL-0001")?;

let record = store.put_evidence(&draft, &bytes)?;  // draft = evidence_artifact minus resolved fields
let bytes = store.read_evidence("ART-1")?;         // verified against the record's digest

store.reconcile(std::time::Duration::from_secs(300))?; // after a crash
```

An evidence `draft` is a complete `evidence_artifact` record, including `content_type`, **without** the fields the store resolves: `lifecycle_state`, `byte_length`, `sha256_digest`, `object_ref`, `verified_at` and `failure_reason`.

## ⚡ Performance

Batch your writes. Every commit is fsynced (`synchronous=FULL`), so a batch of 500 costs about the same as a single record. Measure on your own machine with:

```sh
cargo run --release -p cybercore-evaluation-store --example local_store_throughput
```

## 🧪 Test

```sh
cargo test -p cybercore-evaluation-store
```

The tests round-trip every contract fixture and check append-only enforcement in SQLite itself, tamper detection, idempotency, batch atomicity, the evidence protocol, crash reconciliation and orphan reporting.

## 🚧 Not yet

- the D1 and R2 hosted adapters (the migration SQL is ready for D1);
- evidence deletion and retention;
- supersession chains;
- persisted scheduler state.
