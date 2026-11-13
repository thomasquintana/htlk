-- Historical projections are published with the journal transaction. They are
-- never a recovery authority and never retain artifact value bytes.
CREATE TABLE inspection_clock (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    position INTEGER NOT NULL CHECK(position>=0)
) STRICT;
INSERT INTO inspection_clock VALUES(1,0);
CREATE TABLE inspection_commits (
    position INTEGER PRIMARY KEY,
    run_upper INTEGER NOT NULL,
    record_upper INTEGER NOT NULL,
    change_upper INTEGER NOT NULL
) STRICT;
INSERT INTO inspection_commits VALUES(0,0,0,0);
CREATE TABLE inspection_runs (
    run_number INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL UNIQUE REFERENCES runs(run_id),
    created_position INTEGER NOT NULL
) STRICT;
CREATE TABLE inspection_headers (
    run_number INTEGER NOT NULL REFERENCES inspection_runs(run_number),
    position INTEGER NOT NULL,
    fingerprint TEXT NOT NULL,
    root_scope TEXT NOT NULL,
    status TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    owner TEXT,
    fence INTEGER NOT NULL,
    lease_expires_ms INTEGER,
    last_time_ms INTEGER NOT NULL,
    PRIMARY KEY(run_number,position)
) STRICT;
CREATE TABLE inspection_record_keys (
    record_number INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    family TEXT NOT NULL,
    record_id TEXT NOT NULL,
    created_position INTEGER NOT NULL,
    UNIQUE(run_id,family,record_id)
) STRICT;
CREATE INDEX inspection_record_page ON inspection_record_keys(run_id,family,record_number);
CREATE TABLE inspection_records (
    record_number INTEGER NOT NULL REFERENCES inspection_record_keys(record_number),
    position INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    final INTEGER NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY(record_number,position)
) STRICT;
CREATE TABLE inspection_changes (
    change_id INTEGER PRIMARY KEY,
    position INTEGER NOT NULL,
    run_number INTEGER NOT NULL REFERENCES inspection_runs(run_number),
    -- Zero denotes the header; record ordinals start at one.
    record_number INTEGER NOT NULL CHECK(record_number>=0),
    sequence INTEGER NOT NULL,
    UNIQUE(position,run_number,record_number)
) STRICT;
