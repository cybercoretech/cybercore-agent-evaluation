-- Scheduler state snapshots (ADR-0016). Plain SQLite dialect, D1-compatible.
-- A snapshot is the coordinator state right after journal entry `seq`,
-- written in the same transaction as that entry. Only the latest is kept;
-- the journal stays the source of truth (a bad snapshot is ignored).
CREATE TABLE scheduler_snapshots (
    evaluation_id   TEXT NOT NULL REFERENCES evaluation_schedules (evaluation_id),
    seq             INTEGER NOT NULL CHECK (seq >= 1),
    state           TEXT NOT NULL,  -- canonical JSON of TrialCoordinator::export_state
    state_sha256    TEXT NOT NULL,
    schedule_sha256 TEXT NOT NULL,  -- digest of the schedule the state belongs to
    created_at      TEXT NOT NULL,
    PRIMARY KEY (evaluation_id, seq)
);
