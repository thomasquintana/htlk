-- Migration 4: durable host identity/time and principal-method-target receipts.
CREATE TABLE runtime_identity (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    namespace TEXT NOT NULL,
    next_run INTEGER NOT NULL CHECK(next_run>=0),
    last_time_ms INTEGER NOT NULL CHECK(last_time_ms>=0)
) STRICT;
INSERT INTO runtime_identity VALUES(1,lower(hex(randomblob(16))),0,0);
CREATE INDEX committed_run_time ON runs(last_time_ms);
CREATE TABLE host_receipts (
    principal TEXT NOT NULL,
    method TEXT NOT NULL,
    target TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms>=0),
    PRIMARY KEY(principal,method,target,idempotency_key)
) STRICT;
