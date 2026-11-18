//! Local store: SQLite metadata plus filesystem evidence objects, joined by
//! the ADR 0002 write protocol.

use crate::artifact::{ArtifactStore, FsArtifactStore};
use crate::canonical::sha256_hex;
use crate::error::{StoreError, StoreResult};
use crate::metadata::{now, SqliteMetadataStore, UploadState};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;
use std::time::Duration;

/// Fields the store fills in when it resolves an evidence upload. Callers must
/// not set them in the draft.
const RESOLVED_FIELDS: &[&str] = &[
    "lifecycle_state",
    "byte_length",
    "sha256_digest",
    "object_ref",
    "verified_at",
    "failure_reason",
];

/// What `reconcile` did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Pending uploads whose object was intact and are now `available`.
    pub completed: Vec<String>,
    /// Pending uploads whose object was missing or corrupt, now `failed`.
    pub failed: Vec<String>,
    /// Pending uploads left alone because they are younger than `min_age`.
    pub skipped_recent: Vec<String>,
}

/// The local Workbench store: `<dir>/metadata.sqlite3` and `<dir>/evidence/`.
pub struct LocalStore {
    metadata: SqliteMetadataStore,
    objects: FsArtifactStore,
}

impl LocalStore {
    pub fn open(directory: impl AsRef<Path>) -> StoreResult<Self> {
        let directory = directory.as_ref();
        fs::create_dir_all(directory)?;
        Ok(Self {
            metadata: SqliteMetadataStore::open(directory.join("metadata.sqlite3"))?,
            objects: FsArtifactStore::open(directory.join("evidence"))?,
        })
    }

    /// Record operations (put/get/list/references).
    pub fn metadata(&self) -> &SqliteMetadataStore {
        &self.metadata
    }

    pub fn objects(&self) -> &FsArtifactStore {
        &self.objects
    }

    /// Stores evidence bytes and its `evidence_artifact` record.
    ///
    /// `draft` is a complete evidence_artifact record except for the fields
    /// the store resolves (`lifecycle_state`, `byte_length`, `sha256_digest`,
    /// `object_ref`, `verified_at`, `failure_reason`); `content_type` is
    /// required. Protocol:
    ///
    /// 1. validate the would-be `available` record (nothing written on error);
    /// 2. record a `pending` upload bound to the content digest;
    /// 3. write the object (atomic, content-addressed) and re-verify it;
    /// 4. in one transaction, append the record and mark the upload resolved.
    ///
    /// A crash between steps leaves a `pending` upload that `reconcile`
    /// finishes or fails. Retrying the same call is safe.
    pub fn put_evidence(&self, draft: &Value, bytes: &[u8]) -> StoreResult<Value> {
        let draft = clean_draft(draft)?;
        let record_id = record_id_of(&draft)?;
        let sha256 = sha256_hex(bytes);
        let byte_length = bytes.len() as u64;

        let available = available_record(&draft, &sha256, byte_length, &now());
        self.metadata.validator().validate(&available)?;

        match self
            .metadata
            .begin_upload(&record_id, &sha256, byte_length, &draft)?
        {
            UploadState::Pending => {}
            UploadState::Available | UploadState::Failed => {
                return self
                    .metadata
                    .get_record(&record_id)?
                    .ok_or_else(|| StoreError::NotFound(format!("evidence record {record_id}")));
            }
        }

        let stored = self.objects.put(bytes)?;
        if stored != sha256 || !self.objects.verify(&sha256)? {
            let failed = failed_record(
                &draft,
                &sha256,
                byte_length,
                "object failed verification after write",
            );
            self.metadata.resolve_upload(
                &record_id,
                &failed,
                UploadState::Failed,
                Some("object failed verification after write"),
            )?;
            return Err(StoreError::Integrity(format!(
                "evidence {record_id}: object {sha256} failed verification after write"
            )));
        }

        let available = available_record(&draft, &sha256, byte_length, &now());
        self.metadata
            .resolve_upload(&record_id, &available, UploadState::Available, None)?;
        Ok(available)
    }

    /// Returns an evidence record's bytes, verified against the digest the
    /// record carries.
    pub fn read_evidence(&self, record_id: &str) -> StoreResult<Vec<u8>> {
        let record = self
            .metadata
            .get_record(record_id)?
            .ok_or_else(|| StoreError::NotFound(format!("evidence record {record_id}")))?;
        if record.get("record_type").and_then(Value::as_str) != Some("evidence_artifact") {
            return Err(StoreError::InvalidRecord(format!(
                "{record_id} is not an evidence_artifact"
            )));
        }
        if record.get("lifecycle_state").and_then(Value::as_str) != Some("available") {
            return Err(StoreError::NotFound(format!(
                "evidence {record_id} is not available"
            )));
        }
        let digest = record
            .get("sha256_digest")
            .and_then(Value::as_str)
            .ok_or_else(|| StoreError::Integrity(format!("evidence {record_id} has no digest")))?;
        self.objects.get(digest)
    }

    /// Resolves `pending` uploads at least `min_age` old (younger ones may
    /// still be in flight in another process): intact objects complete as
    /// `available`, missing or corrupt ones become `failed` records.
    pub fn reconcile(&self, min_age: Duration) -> StoreResult<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let cutoff = Utc::now()
            - chrono::Duration::from_std(min_age)
                .map_err(|_| StoreError::InvalidRecord("min_age is too large".into()))?;
        for upload in self.metadata.uploads_in_state(UploadState::Pending)? {
            let created = DateTime::parse_from_rfc3339(&upload.created_at)
                .map_err(|error| StoreError::Integrity(format!("upload timestamp: {error}")))?;
            if created > cutoff {
                report.skipped_recent.push(upload.artifact_record_id);
                continue;
            }
            if self.objects.verify(&upload.object_sha256)? {
                let record = available_record(
                    &upload.draft,
                    &upload.object_sha256,
                    upload.byte_length,
                    &now(),
                );
                self.metadata.resolve_upload(
                    &upload.artifact_record_id,
                    &record,
                    UploadState::Available,
                    None,
                )?;
                report.completed.push(upload.artifact_record_id);
            } else {
                let reason = "object missing or corrupt at reconciliation";
                let record = failed_record(
                    &upload.draft,
                    &upload.object_sha256,
                    upload.byte_length,
                    reason,
                );
                self.metadata.resolve_upload(
                    &upload.artifact_record_id,
                    &record,
                    UploadState::Failed,
                    Some(reason),
                )?;
                report.failed.push(upload.artifact_record_id);
            }
        }
        Ok(report)
    }

    /// Stored objects that no upload references (for example, written just
    /// before a crash that also lost the pending row). Reported, never deleted.
    pub fn orphaned_objects(&self) -> StoreResult<Vec<String>> {
        let referenced = self.metadata.referenced_objects()?;
        Ok(self
            .objects
            .list()?
            .into_iter()
            .filter(|digest| referenced.binary_search(digest).is_err())
            .collect())
    }
}

fn clean_draft(draft: &Value) -> StoreResult<Value> {
    let map = draft
        .as_object()
        .ok_or_else(|| StoreError::InvalidRecord("evidence draft must be an object".into()))?;
    if map.get("record_type").and_then(Value::as_str) != Some("evidence_artifact") {
        return Err(StoreError::InvalidRecord(
            "evidence draft record_type must be evidence_artifact".into(),
        ));
    }
    if let Some(field) = RESOLVED_FIELDS
        .iter()
        .find(|field| map.contains_key(**field))
    {
        return Err(StoreError::InvalidRecord(format!(
            "evidence draft must not set {field}; the store resolves it"
        )));
    }
    if map.get("content_type").and_then(Value::as_str).is_none() {
        return Err(StoreError::InvalidRecord(
            "evidence draft requires content_type".into(),
        ));
    }
    Ok(draft.clone())
}

fn record_id_of(record: &Value) -> StoreResult<String> {
    record
        .get("record_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| StoreError::InvalidRecord("record_id must be a string".into()))
}

fn with_fields(draft: &Value, fields: Vec<(&str, Value)>) -> Value {
    let mut map: Map<String, Value> = draft.as_object().cloned().unwrap_or_default();
    for (key, value) in fields {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

fn available_record(draft: &Value, sha256: &str, byte_length: u64, verified_at: &str) -> Value {
    with_fields(
        draft,
        vec![
            ("lifecycle_state", Value::from("available")),
            ("byte_length", Value::from(byte_length)),
            ("sha256_digest", Value::from(sha256)),
            ("object_ref", Value::from(format!("sha256:{sha256}"))),
            ("verified_at", Value::from(verified_at)),
        ],
    )
}

fn failed_record(draft: &Value, sha256: &str, byte_length: u64, reason: &str) -> Value {
    with_fields(
        draft,
        vec![
            ("lifecycle_state", Value::from("failed")),
            ("byte_length", Value::from(byte_length)),
            ("sha256_digest", Value::from(sha256)),
            ("failure_reason", Value::from(reason)),
        ],
    )
}
