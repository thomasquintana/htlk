-- Private version-1 SQLite layout, journal records use HTLK record_version 0.1.
CREATE TABLE runs (
    run_id TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    root_scope TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','succeeded','failed','cancelled')),
    sequence INTEGER NOT NULL CHECK(sequence >= 1),
    owner TEXT,
    fence INTEGER NOT NULL CHECK(fence >= 0),
    lease_expires_ms INTEGER,
    last_time_ms INTEGER NOT NULL CHECK(last_time_ms >= 0),
    CHECK((owner IS NULL AND lease_expires_ms IS NULL) OR
          (owner IS NOT NULL AND lease_expires_ms IS NOT NULL))
) STRICT;
CREATE TABLE journal (
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    sequence INTEGER NOT NULL CHECK(sequence >= 1),
    event_type TEXT NOT NULL,
    record_version TEXT NOT NULL CHECK(record_version = '0.1'),
    actor TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL CHECK(timestamp_ms >= 0),
    causation TEXT NOT NULL,
    command_id TEXT,
    payload BLOB NOT NULL,
    PRIMARY KEY(run_id,sequence)
) STRICT;
CREATE TABLE outbox (
    run_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    delivered INTEGER NOT NULL DEFAULT 0 CHECK(delivered IN (0,1)),
    PRIMARY KEY(run_id,sequence),
    FOREIGN KEY(run_id,sequence) REFERENCES journal(run_id,sequence)
) STRICT;
CREATE INDEX outbox_pending ON outbox(delivered,run_id,sequence);
-- Each family retains its typed runtime record schema; these rows are not a
-- second graph. Immutable/final records cannot be revised by normal transitions.
CREATE TABLE records (
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    family TEXT NOT NULL,
    record_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision >= 1),
    final INTEGER NOT NULL CHECK(final IN (0,1)),
    payload BLOB NOT NULL,
    PRIMARY KEY(run_id,family,record_id)
) STRICT;
CREATE TABLE commands (
    run_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    PRIMARY KEY(run_id,command_id),
    FOREIGN KEY(run_id,sequence) REFERENCES journal(run_id,sequence)
) STRICT;
