//! Rough throughput check for the local store on a real database file.
//!
//! cargo run --release -p cybercore-evaluation-store --example local_store_throughput -- [dir] [count]

use cybercore_evaluation_store::{LocalStore, StoreResult};
use serde_json::{json, Value};
use std::time::Instant;

fn main() -> StoreResult<()> {
    let mut args = std::env::args().skip(1);
    let directory = args
        .next()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("cybercore-store-throughput"));
    let count: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(10_000);
    if directory.exists() {
        eprintln!(
            "refusing to reuse existing directory {}",
            directory.display()
        );
        std::process::exit(2);
    }

    let template: Value = serde_json::from_str(include_str!(
        "../../../contracts/fixtures/valid/run.json"
    ))
    .map_err(|error| cybercore_evaluation_store::StoreError::InvalidRecord(error.to_string()))?;

    let opened = Instant::now();
    let store = LocalStore::open(&directory)?;
    println!("open + migrate + compile schemas: {:?}", opened.elapsed());

    let records: Vec<Value> = (0..count)
        .map(|n| {
            let mut run = template.clone();
            run["record_id"] = json!(format!("RUN-{n:08}"));
            run
        })
        .collect();

    let batch_started = Instant::now();
    for chunk in records.chunks(500) {
        store.metadata().put_records(chunk)?;
    }
    let batch = batch_started.elapsed();
    println!(
        "{count} validated records in batches of 500: {batch:?} ({:.0}/s)",
        count as f64 / batch.as_secs_f64()
    );

    let single_started = Instant::now();
    let singles = 200;
    for n in 0..singles {
        let mut run = template.clone();
        run["record_id"] = json!(format!("RUN-SINGLE-{n:04}"));
        store.metadata().put_record(&run)?;
    }
    let single = single_started.elapsed();
    println!(
        "{singles} single-record transactions: {single:?} ({:.0}/s)",
        singles as f64 / single.as_secs_f64()
    );

    let read_started = Instant::now();
    for n in (0..count).step_by(10) {
        store.metadata().get_record(&format!("RUN-{n:08}"))?;
    }
    println!(
        "{} verified reads: {:?}",
        count / 10,
        read_started.elapsed()
    );

    let refs_started = Instant::now();
    let refs = store.metadata().references_to("TRIAL-0001")?;
    println!(
        "reverse lookup over {} references: {:?}",
        refs.len(),
        refs_started.elapsed()
    );

    let evidence: Value = serde_json::from_str(include_str!(
        "../../../contracts/fixtures/valid/evidence-artifact.json"
    ))
    .map_err(|error| cybercore_evaluation_store::StoreError::InvalidRecord(error.to_string()))?;
    let mut draft = evidence;
    for field in [
        "lifecycle_state",
        "byte_length",
        "sha256_digest",
        "object_ref",
        "verified_at",
    ] {
        draft.as_object_mut().map(|map| map.remove(field));
    }
    let payload = vec![b'x'; 64 * 1024];
    let evidence_started = Instant::now();
    for n in 0..100 {
        let mut item = draft.clone();
        item["record_id"] = json!(format!("ART-{n:04}"));
        let mut bytes = payload.clone();
        bytes.extend_from_slice(n.to_string().as_bytes());
        store.put_evidence(&item, &bytes)?;
    }
    println!(
        "100 x 64 KiB evidence uploads (fsynced): {:?}",
        evidence_started.elapsed()
    );
    println!("data left in {}", directory.display());
    Ok(())
}
