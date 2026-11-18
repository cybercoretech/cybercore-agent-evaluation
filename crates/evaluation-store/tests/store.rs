use cybercore_evaluation_store::{
    canonical_json, ArtifactStore, LocalStore, PutOutcome, SqliteMetadataStore, StoreError,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn fixtures(kind: &str) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> =
        fs::read_dir(repo_path(&format!("contracts/fixtures/{kind}")))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                let text = fs::read_to_string(&path).unwrap();
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    serde_json::from_str(&text).unwrap(),
                )
            })
            .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn fixture(name: &str) -> Value {
    let text = fs::read_to_string(repo_path(&format!("contracts/fixtures/valid/{name}"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// A fresh directory under the system temp dir, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cybercore-store-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn evidence_draft(record_id: &str) -> Value {
    let mut draft = fixture("evidence-artifact.json");
    let map = draft.as_object_mut().unwrap();
    for field in [
        "lifecycle_state",
        "byte_length",
        "sha256_digest",
        "object_ref",
        "verified_at",
        "failure_reason",
    ] {
        map.remove(field);
    }
    map.insert("record_id".into(), json!(record_id));
    draft
}

#[test]
fn every_valid_fixture_round_trips_and_rewrites_are_noops() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let valid = fixtures("valid");
    assert_eq!(valid.len(), 19);
    for (name, record) in &valid {
        assert_eq!(
            store.put_record(record).unwrap(),
            PutOutcome::Inserted,
            "{name}"
        );
        let id = record["record_id"].as_str().unwrap();
        let stored = store.get_record(id).unwrap().unwrap();
        assert_eq!(canonical_json(&stored), canonical_json(record), "{name}");
        assert_eq!(
            store.put_record(record).unwrap(),
            PutOutcome::AlreadyPresent,
            "{name}"
        );
    }
    assert_eq!(store.count_records(None).unwrap(), 19);
}

#[test]
fn every_invalid_fixture_is_rejected_and_nothing_is_written() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    for (name, record) in fixtures("invalid") {
        match store.put_record(&record) {
            Err(StoreError::InvalidRecord(_)) => {}
            other => panic!("{name}: expected InvalidRecord, got {other:?}"),
        }
    }
    assert_eq!(store.count_records(None).unwrap(), 0);
}

#[test]
fn a_different_body_under_an_existing_id_is_a_conflict() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let run = fixture("run.json");
    store.put_record(&run).unwrap();
    let mut changed = run.clone();
    changed["created_at"] = json!("2026-11-18T00:00:00Z");
    assert!(matches!(
        store.put_record(&changed),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        canonical_json(&store.get_record("RUN-0001").unwrap().unwrap()),
        canonical_json(&run)
    );
}

#[test]
fn append_only_is_enforced_by_the_database_itself() {
    let dir = TempDir::new("append-only");
    let path = dir.0.join("meta.sqlite3");
    SqliteMetadataStore::open(&path)
        .unwrap()
        .put_record(&fixture("run.json"))
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    assert!(raw
        .execute(
            "UPDATE records SET body = '{}' WHERE record_id = 'RUN-0001'",
            []
        )
        .is_err());
    assert!(raw
        .execute("DELETE FROM records WHERE record_id = 'RUN-0001'", [])
        .is_err());
}

#[test]
fn tampered_bodies_are_detected_on_read() {
    let dir = TempDir::new("tamper");
    let path = dir.0.join("meta.sqlite3");
    let run = fixture("run.json");
    SqliteMetadataStore::open(&path)
        .unwrap()
        .put_record(&run)
        .unwrap();
    {
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch("DROP TRIGGER records_are_append_only_update;")
            .unwrap();
        raw.execute(
            "UPDATE records SET body = replace(body, 'RUN-0001', 'RUN-EVIL') WHERE record_id = 'RUN-0001'",
            [],
        )
        .unwrap();
    }
    let store = SqliteMetadataStore::open(&path).unwrap();
    assert!(matches!(
        store.get_record("RUN-0001"),
        Err(StoreError::Integrity(_))
    ));
}

#[test]
fn idempotency_keys_replay_and_reject_reuse() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let run = fixture("run.json");
    assert_eq!(
        store.put_record_idempotent("req-1", &run).unwrap(),
        PutOutcome::Inserted
    );
    assert_eq!(
        store.put_record_idempotent("req-1", &run).unwrap(),
        PutOutcome::AlreadyPresent
    );
    assert!(matches!(
        store.put_record_idempotent("req-1", &fixture("trial.json")),
        Err(StoreError::IdempotencyConflict(_))
    ));
    assert!(matches!(
        store.put_record_idempotent("", &run),
        Err(StoreError::InvalidRecord(_))
    ));
}

#[test]
fn batches_are_all_or_nothing() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let mut broken = fixture("trial.json");
    broken["created_at"] = json!("not-a-timestamp");
    let result = store.put_records(&[fixture("run.json"), broken]);
    assert!(matches!(result, Err(StoreError::InvalidRecord(_))));
    assert_eq!(store.count_records(None).unwrap(), 0);

    let outcomes = store
        .put_records(&[fixture("run.json"), fixture("trial.json")])
        .unwrap();
    assert_eq!(outcomes, vec![PutOutcome::Inserted, PutOutcome::Inserted]);
}

#[test]
fn references_are_indexed_for_reverse_lookup() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    store.put_record(&fixture("run.json")).unwrap();
    let refs = store.references_to("TRIAL-0001").unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].source_id, "RUN-0001");
    assert_eq!(refs[0].source_type, "run");
    assert_eq!(refs[0].json_path, "/trial_ref");
    assert_eq!(refs[0].target_type, "trial");
}

#[test]
fn listing_pages_by_record_id() {
    let store = SqliteMetadataStore::open_in_memory().unwrap();
    let base = fixture("run.json");
    for n in 0..5 {
        let mut run = base.clone();
        run["record_id"] = json!(format!("RUN-{n:04}"));
        store.put_record(&run).unwrap();
    }
    let first = store.list_records("run", None, 2).unwrap();
    assert_eq!(first.len(), 2);
    let last_id = first[1]["record_id"].as_str().unwrap().to_string();
    let second = store.list_records("run", Some(&last_id), 10).unwrap();
    assert_eq!(second.len(), 3);
    assert_eq!(second[0]["record_id"], "RUN-0002");
}

#[test]
fn records_survive_reopen() {
    let dir = TempDir::new("reopen");
    {
        let store = LocalStore::open(&dir.0).unwrap();
        store.metadata().put_record(&fixture("run.json")).unwrap();
    }
    let store = LocalStore::open(&dir.0).unwrap();
    assert!(store.metadata().get_record("RUN-0001").unwrap().is_some());
}

#[test]
fn evidence_is_stored_verified_and_idempotent() {
    let dir = TempDir::new("evidence");
    let store = LocalStore::open(&dir.0).unwrap();
    let bytes = b"agent output\n";
    let record = store.put_evidence(&evidence_draft("ART-1"), bytes).unwrap();
    assert_eq!(record["lifecycle_state"], "available");
    assert_eq!(record["byte_length"], bytes.len());
    let digest = record["sha256_digest"].as_str().unwrap();
    assert_eq!(record["object_ref"], format!("sha256:{digest}"));
    assert_eq!(store.read_evidence("ART-1").unwrap(), bytes);

    // Retrying the same upload returns the stored record without rewriting it.
    let again = store.put_evidence(&evidence_draft("ART-1"), bytes).unwrap();
    assert_eq!(canonical_json(&again), canonical_json(&record));
    // The same id cannot be rebound to different content.
    assert!(matches!(
        store.put_evidence(&evidence_draft("ART-1"), b"other"),
        Err(StoreError::Conflict(_))
    ));
    assert!(store.orphaned_objects().unwrap().is_empty());
}

#[test]
fn evidence_drafts_cannot_preset_resolved_fields() {
    let dir = TempDir::new("draft");
    let store = LocalStore::open(&dir.0).unwrap();
    let mut draft = evidence_draft("ART-2");
    draft["lifecycle_state"] = json!("available");
    assert!(matches!(
        store.put_evidence(&draft, b"x"),
        Err(StoreError::InvalidRecord(_))
    ));
    let mut no_type = evidence_draft("ART-3");
    no_type.as_object_mut().unwrap().remove("content_type");
    assert!(matches!(
        store.put_evidence(&no_type, b"x"),
        Err(StoreError::InvalidRecord(_))
    ));
    assert_eq!(store.metadata().count_records(None).unwrap(), 0);
}

#[test]
fn corrupted_evidence_objects_are_detected_on_read() {
    let dir = TempDir::new("corrupt");
    let store = LocalStore::open(&dir.0).unwrap();
    let record = store
        .put_evidence(&evidence_draft("ART-4"), b"original")
        .unwrap();
    let digest = record["sha256_digest"].as_str().unwrap();
    let object = dir
        .0
        .join("evidence/objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    fs::write(&object, b"tampered").unwrap();
    assert!(matches!(
        store.read_evidence("ART-4"),
        Err(StoreError::Integrity(_))
    ));
}

/// Simulates a crash after step 2 of the protocol: a pending upload row exists
/// and the object may or may not have been written.
fn insert_stale_pending(dir: &Path, record_id: &str, bytes: &[u8]) {
    let raw = Connection::open(dir.join("metadata.sqlite3")).unwrap();
    raw.execute(
        "INSERT INTO evidence_uploads
         (artifact_record_id, object_sha256, byte_length, draft, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'pending', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        params![
            record_id,
            cybercore_evaluation_store::sha256_hex(bytes),
            bytes.len() as i64,
            canonical_json(&evidence_draft(record_id)),
        ],
    )
    .unwrap();
}

#[test]
fn reconcile_completes_or_fails_interrupted_uploads() {
    let dir = TempDir::new("reconcile");
    let store = LocalStore::open(&dir.0).unwrap();

    insert_stale_pending(&dir.0, "ART-WRITTEN", b"made it to disk");
    store.objects().put(b"made it to disk").unwrap();
    insert_stale_pending(&dir.0, "ART-LOST", b"never written");

    let report = store.reconcile(Duration::from_secs(60)).unwrap();
    assert_eq!(report.completed, vec!["ART-WRITTEN".to_string()]);
    assert_eq!(report.failed, vec!["ART-LOST".to_string()]);
    assert!(report.skipped_recent.is_empty());

    assert_eq!(
        store.read_evidence("ART-WRITTEN").unwrap(),
        b"made it to disk"
    );
    let lost = store.metadata().get_record("ART-LOST").unwrap().unwrap();
    assert_eq!(lost["lifecycle_state"], "failed");
    assert!(lost["failure_reason"].as_str().unwrap().contains("missing"));

    // Nothing left to do on a second pass.
    assert_eq!(
        store.reconcile(Duration::from_secs(60)).unwrap(),
        Default::default()
    );
}

#[test]
fn reconcile_leaves_recent_uploads_alone() {
    let dir = TempDir::new("recent");
    let store = LocalStore::open(&dir.0).unwrap();
    let raw = Connection::open(dir.0.join("metadata.sqlite3")).unwrap();
    raw.execute(
        "INSERT INTO evidence_uploads
         (artifact_record_id, object_sha256, byte_length, draft, state, created_at, updated_at)
         VALUES ('ART-NEW', ?1, 1, ?2, 'pending', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![
            cybercore_evaluation_store::sha256_hex(b"x"),
            canonical_json(&evidence_draft("ART-NEW"))
        ],
    )
    .unwrap();
    let report = store.reconcile(Duration::from_secs(3600)).unwrap();
    assert_eq!(report.skipped_recent, vec!["ART-NEW".to_string()]);
    assert!(store.metadata().get_record("ART-NEW").unwrap().is_none());
}

#[test]
fn unreferenced_objects_are_reported_as_orphans() {
    let dir = TempDir::new("orphans");
    let store = LocalStore::open(&dir.0).unwrap();
    let orphan = store.objects().put(b"no upload row").unwrap();
    store
        .put_evidence(&evidence_draft("ART-5"), b"tracked")
        .unwrap();
    assert_eq!(store.orphaned_objects().unwrap(), vec![orphan]);
}
