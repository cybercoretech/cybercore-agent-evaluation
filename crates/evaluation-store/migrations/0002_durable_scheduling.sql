-- Durable scheduling (ADR-0010). Plain SQLite dialect, D1-compatible.

-- The frozen schedule an evaluation runs under. Immutable.
CREATE TABLE evaluation_schedules (
    evaluation_id   TEXT PRIMARY KEY NOT NULL,
    schedule        TEXT NOT NULL,  -- canonical JSON of EvaluationSchedule
    schedule_sha256 TEXT NOT NULL,
    created_at      TEXT NOT NULL
);

CREATE TRIGGER evaluation_schedules_immutable_update
BEFORE UPDATE ON evaluation_schedules
BEGIN
    SELECT RAISE(ABORT, 'evaluation schedules are immutable');
END;

CREATE TRIGGER evaluation_schedules_immutable_delete
BEFORE DELETE ON evaluation_schedules
BEGIN
    SELECT RAISE(ABORT, 'evaluation schedules are immutable');
END;

-- Write-ahead journal of coordinator commands. Replaying it in `seq` order
-- through a fresh TrialCoordinator reproduces the scheduler state exactly.
CREATE TABLE scheduler_journal (
    evaluation_id TEXT NOT NULL REFERENCES evaluation_schedules (evaluation_id),
    seq           INTEGER NOT NULL CHECK (seq >= 1),
    command       TEXT NOT NULL CHECK (command IN ('reserve', 'complete', 'cancel')),
    payload       TEXT NOT NULL,  -- canonical JSON
    fencing_token INTEGER NOT NULL,
    recorded_at   TEXT NOT NULL,
    PRIMARY KEY (evaluation_id, seq)
);

CREATE TRIGGER scheduler_journal_append_only_update
BEFORE UPDATE ON scheduler_journal
BEGIN
    SELECT RAISE(ABORT, 'the scheduler journal is append-only');
END;

CREATE TRIGGER scheduler_journal_append_only_delete
BEFORE DELETE ON scheduler_journal
BEGIN
    SELECT RAISE(ABORT, 'the scheduler journal is append-only');
END;

-- Run IDs, request IDs and idempotency keys are unique across every
-- evaluation and process, not only within one coordinator instance.
CREATE TABLE attempt_identities (
    identity_kind TEXT NOT NULL CHECK (identity_kind IN ('run_id', 'request_id', 'idempotency_key')),
    identity      TEXT NOT NULL,
    evaluation_id TEXT NOT NULL REFERENCES evaluation_schedules (evaluation_id),
    trial_id      TEXT NOT NULL,
    PRIMARY KEY (identity_kind, identity)
);

-- At most one scheduler drives an evaluation. Every acquisition increments
-- the fencing token; journal writes must present the current token, so a
-- scheduler whose lease was taken over can no longer write.
CREATE TABLE scheduler_leases (
    evaluation_id TEXT PRIMARY KEY NOT NULL REFERENCES evaluation_schedules (evaluation_id),
    owner         TEXT NOT NULL,
    fencing_token INTEGER NOT NULL CHECK (fencing_token >= 1),
    expires_at    TEXT NOT NULL
);

-- Highest token ever issued per evaluation, kept after a lease is released so
-- tokens never repeat.
CREATE TABLE scheduler_fencing (
    evaluation_id TEXT PRIMARY KEY NOT NULL REFERENCES evaluation_schedules (evaluation_id),
    last_token    INTEGER NOT NULL CHECK (last_token >= 1)
);
