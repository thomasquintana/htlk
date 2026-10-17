-- Migration 5: non-authoritative late-response inbox for diagnostics/reconciliation.
CREATE TABLE late_responses (
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    attempt_id TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    payload BLOB NOT NULL,
    protection BLOB NOT NULL,
    PRIMARY KEY(run_id,attempt_id,content_digest)
) STRICT;
