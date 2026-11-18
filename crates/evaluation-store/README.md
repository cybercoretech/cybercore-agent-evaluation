# 🗄️ CyberCore Evaluation Store

Local persistence for the CyberCore Agent Evaluation Workbench: a SQLite metadata store and a filesystem evidence store, joined by the ADR-0002 write protocol. Design and trade-offs: [ADR-0009](../../docs/decisions/0009-local-persistence.md).

## 📦 What it gives you

- **`SqliteMetadataStore`:** append-only records, validated against the embedded v0 contracts, with tamper-evident bodies, a reverse reference index, paging and idempotency keys.
- **`FsArtifactStore`:** content-addressed, atomically written, verified-on-read evidence objects behind the `ArtifactStore` port.
- **`DurableScheduler`:** the trial coordinator made crash-safe ([ADR-0010](../../docs/decisions/0010-durable-scheduling.md)). It keeps a write-ahead journal that is replayed after a restart, a lease with fencing tokens so only one scheduler drives an evaluation, attempt identities that are unique across processes, and interrupted attempts closed under the schedule's retry policy.
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

### Durable scheduling

```rust
use cybercore_evaluation_store::DurableScheduler;

store.metadata().create_schedule(&schedule)?;                 // immutable
let (mut scheduler, recovery) =
    DurableScheduler::acquire(store.metadata(), "EVAL-1", "worker-1", ttl)?;
while let Some(trial) = scheduler.coordinator().ready_trial_ids().first().cloned() {
    let reservation = scheduler.reserve_attempt(&trial, identity)?; // journaled before return
    let result = execute_with_runner(&runner, &reservation.request).await;
    scheduler.complete_attempt(&trial, &reservation.request.request_id, result)?;
    scheduler.renew_lease()?;
}
scheduler.release()?;
```

`LeaseLost` means another scheduler took over: stop immediately. Use a stable owner name per host, so that a restarted process can take its own lease back at once.

Try a crash and resume with the real Bubblewrap runner:

```sh
cargo run -p cybercore-evaluation-store --example durable_evaluation -- /tmp/demo --crash-after 2
cargo run -p cybercore-evaluation-store --example durable_evaluation -- /tmp/demo
```

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
- journal snapshots (replay cost grows with the journal);
- hosted scheduling (D1 with Workflows or Durable Objects).
