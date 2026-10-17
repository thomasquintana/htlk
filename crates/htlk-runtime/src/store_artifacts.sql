-- Migration 3: immutable value bytes; metadata stays in journaled final records.
CREATE TABLE value_blobs (
    blob_digest TEXT PRIMARY KEY,
    value_bytes BLOB NOT NULL
) STRICT;
CREATE UNIQUE INDEX accepted_artifact_identity
    ON records(record_id) WHERE family='artifact';
-- Rebuildable uniqueness index over authoritative artifact producer metadata.
CREATE TABLE artifact_producers (
    producer_kind TEXT NOT NULL CHECK(producer_kind IN ('run_input','node_output')),
    producer_id TEXT NOT NULL,
    extension_number INTEGER NOT NULL CHECK(extension_number >= 0),
    port TEXT NOT NULL,
    artifact_id TEXT NOT NULL UNIQUE,
    run_id TEXT NOT NULL,
    family TEXT NOT NULL DEFAULT 'artifact' CHECK(family='artifact'),
    PRIMARY KEY(producer_kind,producer_id,extension_number,port),
    FOREIGN KEY(run_id,family,artifact_id) REFERENCES records(run_id,family,record_id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;
