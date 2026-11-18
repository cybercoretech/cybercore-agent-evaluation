//! SQLite metadata store (ADR 0002 local implementation).

use crate::canonical::{canonical_json, sha256_hex};
use crate::contracts::ContractValidator;
use crate::error::{StoreError, StoreResult};
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_durable_scheduling.sql"),
];
const MAX_LIST_LIMIT: u32 = 1000;

/// Result of an append-only write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    /// The record was new and is now stored.
    Inserted,
    /// An identical record (same canonical body) was already stored.
    AlreadyPresent,
}

/// A reference from one stored record to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordRef {
    /// The record that contains the reference.
    pub source_id: String,
    pub source_type: String,
    /// JSON Pointer of the reference inside the source body.
    pub json_path: String,
    pub target_id: String,
    pub target_type: String,
}

/// Operational state of an evidence upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadState {
    Pending,
    Available,
    Failed,
}

impl UploadState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Available => "available",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> StoreResult<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "available" => Ok(Self::Available),
            "failed" => Ok(Self::Failed),
            other => Err(StoreError::Integrity(format!(
                "unknown evidence upload state {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Upload {
    pub artifact_record_id: String,
    pub object_sha256: String,
    pub byte_length: u64,
    pub draft: Value,
    pub created_at: String,
}

/// Append-only record store on SQLite. Every record is validated against the
/// embedded v0 contracts before it is written; rows can never be updated or
/// deleted (enforced by triggers, not only by this API).
///
/// The connection is guarded by a mutex, so one store can be shared across
/// threads; open one store per process and per database file.
pub struct SqliteMetadataStore {
    connection: Mutex<Connection>,
    validator: ContractValidator,
}

impl SqliteMetadataStore {
    /// Opens (creating if needed) and migrates a database file.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        Self::with_connection(connection)
    }

    /// A private in-memory database, for tests and throwaway runs.
    pub fn open_in_memory() -> StoreResult<Self> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        Self::with_connection(connection)
    }

    fn with_connection(mut connection: Connection) -> StoreResult<Self> {
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            validator: ContractValidator::embedded()?,
        })
    }

    pub(crate) fn lock(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| StoreError::Integrity("metadata store mutex was poisoned".into()))
    }

    pub fn validator(&self) -> &ContractValidator {
        &self.validator
    }

    /// Validates and appends one record.
    pub fn put_record(&self, record: &Value) -> StoreResult<PutOutcome> {
        let prepared = self.prepare(record)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let outcome = insert_prepared(&transaction, &prepared)?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// Validates every record first, then appends them in one transaction:
    /// either all are stored or none are.
    pub fn put_records(&self, records: &[Value]) -> StoreResult<Vec<PutOutcome>> {
        let prepared = records
            .iter()
            .map(|record| self.prepare(record))
            .collect::<StoreResult<Vec<_>>>()?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let outcomes = prepared
            .iter()
            .map(|record| insert_prepared(&transaction, record))
            .collect::<StoreResult<Vec<_>>>()?;
        transaction.commit()?;
        Ok(outcomes)
    }

    /// Appends a record under an idempotency key. Replaying the same key with
    /// the same record returns `AlreadyPresent`; reusing the key for a
    /// different record is an `IdempotencyConflict`.
    pub fn put_record_idempotent(
        &self,
        idempotency_key: &str,
        record: &Value,
    ) -> StoreResult<PutOutcome> {
        if idempotency_key.is_empty() {
            return Err(StoreError::InvalidRecord(
                "idempotency key must not be empty".into(),
            ));
        }
        let prepared = self.prepare(record)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT request_sha256, record_id FROM idempotency_keys WHERE idempotency_key = ?1",
                params![idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((request_sha256, record_id)) = existing {
            return if request_sha256 == prepared.body_sha256 && record_id == prepared.record_id {
                Ok(PutOutcome::AlreadyPresent)
            } else {
                Err(StoreError::IdempotencyConflict(format!(
                    "key {idempotency_key:?} was used for record {record_id}"
                )))
            };
        }
        let outcome = insert_prepared(&transaction, &prepared)?;
        transaction.execute(
            "INSERT INTO idempotency_keys (idempotency_key, request_sha256, record_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                idempotency_key,
                prepared.body_sha256,
                prepared.record_id,
                now()
            ],
        )?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// Returns a stored record after checking its body digest.
    pub fn get_record(&self, record_id: &str) -> StoreResult<Option<Value>> {
        let connection = self.lock()?;
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT body, body_sha256 FROM records WHERE record_id = ?1",
                params![record_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(body, digest)| decode_body(record_id, &body, &digest))
            .transpose()
    }

    /// Records of one type in `record_id` order, `limit` (≤ 1000) at a time.
    /// Pass the last `record_id` of a page as `after` to get the next page.
    pub fn list_records(
        &self,
        record_type: &str,
        after: Option<&str>,
        limit: u32,
    ) -> StoreResult<Vec<Value>> {
        let limit = limit.clamp(1, MAX_LIST_LIMIT);
        let connection = self.lock()?;
        let mut statement = connection.prepare_cached(
            "SELECT record_id, body, body_sha256 FROM records
             WHERE record_type = ?1 AND record_id > ?2
             ORDER BY record_id LIMIT ?3",
        )?;
        let rows =
            statement.query_map(params![record_type, after.unwrap_or(""), limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
        let mut records = Vec::new();
        for row in rows {
            let (record_id, body, digest) = row?;
            records.push(decode_body(&record_id, &body, &digest)?);
        }
        Ok(records)
    }

    /// Every stored reference that points at `target_id`.
    pub fn references_to(&self, target_id: &str) -> StoreResult<Vec<RecordRef>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare_cached(
            "SELECT r.record_id, records.record_type, r.json_path, r.target_id, r.target_type
             FROM record_refs AS r JOIN records ON records.record_id = r.record_id
             WHERE r.target_id = ?1
             ORDER BY r.record_id, r.json_path",
        )?;
        let rows = statement.query_map(params![target_id], |row| {
            Ok(RecordRef {
                source_id: row.get(0)?,
                source_type: row.get(1)?,
                json_path: row.get(2)?,
                target_id: row.get(3)?,
                target_type: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Number of stored records, optionally of one type.
    pub fn count_records(&self, record_type: Option<&str>) -> StoreResult<u64> {
        let connection = self.lock()?;
        let count: i64 = match record_type {
            Some(record_type) => connection.query_row(
                "SELECT COUNT(*) FROM records WHERE record_type = ?1",
                params![record_type],
                |row| row.get(0),
            )?,
            None => connection.query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))?,
        };
        Ok(u64::try_from(count).unwrap_or(0))
    }

    fn prepare(&self, record: &Value) -> StoreResult<PreparedRecord> {
        self.validator.validate(record)?;
        let field = |name: &str| {
            record
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| StoreError::InvalidRecord(format!("{name} must be a string")))
        };
        let body = canonical_json(record);
        let mut refs = Vec::new();
        collect_refs(record, String::new(), true, &mut refs);
        Ok(PreparedRecord {
            record_id: field("record_id")?,
            record_type: field("record_type")?,
            schema_version: field("schema_version")?,
            created_at: field("created_at")?,
            body_sha256: sha256_hex(body.as_bytes()),
            body,
            refs,
        })
    }

    // ---- evidence upload state (used by LocalStore) ----

    pub(crate) fn begin_upload(
        &self,
        artifact_record_id: &str,
        object_sha256: &str,
        byte_length: u64,
        draft: &Value,
    ) -> StoreResult<UploadState> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT object_sha256, state FROM evidence_uploads WHERE artifact_record_id = ?1",
                params![artifact_record_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let state = match existing {
            Some((sha, _)) if sha != object_sha256 => {
                return Err(StoreError::Conflict(format!(
                    "evidence {artifact_record_id} is already bound to object {sha}"
                )))
            }
            Some((_, state)) => UploadState::parse(&state)?,
            None => {
                let length = i64::try_from(byte_length).map_err(|_| {
                    StoreError::InvalidRecord("evidence is too large to record".into())
                })?;
                let timestamp = now();
                transaction.execute(
                    "INSERT INTO evidence_uploads
                     (artifact_record_id, object_sha256, byte_length, draft, state, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5)",
                    params![
                        artifact_record_id,
                        object_sha256,
                        length,
                        canonical_json(draft),
                        timestamp
                    ],
                )?;
                UploadState::Pending
            }
        };
        transaction.commit()?;
        Ok(state)
    }

    /// Writes the resolved evidence record and the final upload state in one
    /// transaction, so the two can never disagree.
    pub(crate) fn resolve_upload(
        &self,
        artifact_record_id: &str,
        record: &Value,
        state: UploadState,
        failure_reason: Option<&str>,
    ) -> StoreResult<PutOutcome> {
        let prepared = self.prepare(record)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let outcome = insert_prepared(&transaction, &prepared)?;
        let changed = transaction.execute(
            "UPDATE evidence_uploads SET state = ?2, failure_reason = ?3, updated_at = ?4
             WHERE artifact_record_id = ?1",
            params![artifact_record_id, state.as_str(), failure_reason, now()],
        )?;
        if changed != 1 {
            return Err(StoreError::NotFound(format!(
                "evidence upload {artifact_record_id}"
            )));
        }
        transaction.commit()?;
        Ok(outcome)
    }

    pub(crate) fn uploads_in_state(&self, state: UploadState) -> StoreResult<Vec<Upload>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare_cached(
            "SELECT artifact_record_id, object_sha256, byte_length, draft, created_at
             FROM evidence_uploads WHERE state = ?1 ORDER BY created_at",
        )?;
        let rows = statement.query_map(params![state.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut uploads = Vec::new();
        for row in rows {
            let (id, sha, length, draft, created_at) = row?;
            uploads.push(Upload {
                draft: serde_json::from_str(&draft).map_err(|error| {
                    StoreError::Integrity(format!("upload {id} draft is not JSON: {error}"))
                })?,
                artifact_record_id: id,
                object_sha256: sha,
                byte_length: u64::try_from(length).unwrap_or(0),
                created_at,
            });
        }
        Ok(uploads)
    }

    /// Object digests referenced by any upload, whatever its state.
    pub(crate) fn referenced_objects(&self) -> StoreResult<Vec<String>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare_cached("SELECT DISTINCT object_sha256 FROM evidence_uploads ORDER BY 1")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

struct PreparedRecord {
    record_id: String,
    record_type: String,
    schema_version: String,
    created_at: String,
    body: String,
    body_sha256: String,
    refs: Vec<(String, String, String)>,
}

fn insert_prepared(
    transaction: &Transaction<'_>,
    record: &PreparedRecord,
) -> StoreResult<PutOutcome> {
    let existing: Option<String> = transaction
        .query_row(
            "SELECT body_sha256 FROM records WHERE record_id = ?1",
            params![record.record_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(digest) = existing {
        return if digest == record.body_sha256 {
            Ok(PutOutcome::AlreadyPresent)
        } else {
            Err(StoreError::Conflict(format!(
                "{} already exists with a different body; records are append-only, \
                 store a correction as a new record",
                record.record_id
            )))
        };
    }
    transaction.execute(
        "INSERT INTO records (record_id, record_type, schema_version, created_at, body, body_sha256, stored_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            record.record_id,
            record.record_type,
            record.schema_version,
            record.created_at,
            record.body,
            record.body_sha256,
            now()
        ],
    )?;
    let mut insert_ref = transaction.prepare_cached(
        "INSERT INTO record_refs (record_id, json_path, target_id, target_type) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (path, target_id, target_type) in &record.refs {
        insert_ref.execute(params![record.record_id, path, target_id, target_type])?;
    }
    Ok(PutOutcome::Inserted)
}

fn decode_body(record_id: &str, body: &str, digest: &str) -> StoreResult<Value> {
    if sha256_hex(body.as_bytes()) != digest {
        return Err(StoreError::Integrity(format!(
            "stored body of {record_id} does not match its digest"
        )));
    }
    serde_json::from_str(body)
        .map_err(|error| StoreError::Integrity(format!("stored body of {record_id}: {error}")))
}

/// Collects `{record_id, record_type}` objects below the root as references,
/// keyed by JSON Pointer.
fn collect_refs(
    value: &Value,
    path: String,
    is_root: bool,
    out: &mut Vec<(String, String, String)>,
) {
    match value {
        Value::Object(map) => {
            if !is_root {
                if let (Some(Value::String(id)), Some(Value::String(kind))) =
                    (map.get("record_id"), map.get("record_type"))
                {
                    out.push((path.clone(), id.clone(), kind.clone()));
                }
            }
            for (key, child) in map {
                let token = key.replace('~', "~0").replace('/', "~1");
                collect_refs(child, format!("{path}/{token}"), false, out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_refs(child, format!("{path}/{index}"), false, out);
            }
        }
        _ => {}
    }
}

fn migrate(connection: &mut Connection) -> StoreResult<()> {
    let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let target = i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX);
    if current > target {
        return Err(StoreError::Integrity(format!(
            "database schema version {current} is newer than this build supports ({target})"
        )));
    }
    for (index, sql) in MIGRATIONS
        .iter()
        .enumerate()
        .skip(usize::try_from(current).unwrap_or(0))
    {
        let transaction = connection.transaction()?;
        transaction.execute_batch(sql)?;
        let version = i64::try_from(index + 1).unwrap_or(i64::MAX);
        transaction.pragma_update(None, "user_version", version)?;
        transaction.commit()?;
    }
    Ok(())
}

pub(crate) fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
