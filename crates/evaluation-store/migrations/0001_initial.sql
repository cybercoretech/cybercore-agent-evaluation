-- Workbench metadata store, version 1.
-- Plain SQLite dialect so the same file can seed a Cloudflare D1 database
-- (`wrangler d1 migrations apply`). Keep provider-specific statements out.

-- Canonical records. Append-only: corrections are new records that
-- reference the record they supersede; rows are never rewritten.
CREATE TABLE records (
    record_id      TEXT PRIMARY KEY NOT NULL,
    record_type    TEXT NOT NULL,
    schema_version TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    body           TEXT NOT NULL,  -- canonical JSON (sorted keys, no whitespace)
    body_sha256    TEXT NOT NULL,  -- hex SHA-256 of `body`, checked on read
    stored_at      TEXT NOT NULL
);
CREATE INDEX records_by_type ON records (record_type, record_id);

CREATE TRIGGER records_are_append_only_update
BEFORE UPDATE ON records
BEGIN
    SELECT RAISE(ABORT, 'records are append-only');
END;

CREATE TRIGGER records_are_append_only_delete
BEFORE DELETE ON records
BEGIN
    SELECT RAISE(ABORT, 'records are append-only');
END;

-- Every record reference found inside a record body ({record_id, record_type}
-- objects, including provenance.parent_records), indexed for reverse lookup.
CREATE TABLE record_refs (
    record_id   TEXT NOT NULL REFERENCES records (record_id),
    json_path   TEXT NOT NULL,  -- JSON Pointer of the reference inside the body
    target_id   TEXT NOT NULL,
    target_type TEXT NOT NULL,
    PRIMARY KEY (record_id, json_path)
);
CREATE INDEX record_refs_by_target ON record_refs (target_id, target_type);

-- Idempotency keys for externally retried writes. A replay with the same key
-- and the same request returns the original result; a different request under
-- the same key is rejected.
CREATE TABLE idempotency_keys (
    idempotency_key TEXT PRIMARY KEY NOT NULL,
    request_sha256  TEXT NOT NULL,
    record_id       TEXT NOT NULL REFERENCES records (record_id),
    created_at      TEXT NOT NULL
);

-- Operational state of evidence uploads (ADR 0002). The evidence_artifact
-- record itself is written exactly once, when the upload is resolved to
-- `available` or `failed`; this table carries the in-flight state.
CREATE TABLE evidence_uploads (
    artifact_record_id TEXT PRIMARY KEY NOT NULL,
    object_sha256      TEXT NOT NULL,
    byte_length        INTEGER NOT NULL CHECK (byte_length >= 0),
    draft              TEXT NOT NULL,  -- canonical JSON of the record minus resolved fields
    state              TEXT NOT NULL CHECK (state IN ('pending', 'available', 'failed')),
    failure_reason     TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE INDEX evidence_uploads_pending ON evidence_uploads (state, created_at);
