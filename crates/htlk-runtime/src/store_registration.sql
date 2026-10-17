-- Migration 2: immutable graph registrations and detached provenance.
CREATE TABLE graphs (
    fingerprint TEXT PRIMARY KEY,
    graph_id TEXT NOT NULL,
    envelope BLOB NOT NULL
) STRICT;
CREATE TABLE graph_provenance (
    fingerprint TEXT NOT NULL REFERENCES graphs(fingerprint),
    provenance_digest TEXT NOT NULL,
    payload BLOB NOT NULL,
    PRIMARY KEY(fingerprint,provenance_digest)
) STRICT;
