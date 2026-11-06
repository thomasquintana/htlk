-- Owner-scoped client state. No synthetic graph runs are used for host sessions.
CREATE TABLE mcp_owners (
    owner_key TEXT PRIMARY KEY,
    principal TEXT NOT NULL,
    name TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    expires_ms INTEGER NOT NULL CHECK(expires_ms >= 0),
    last_time_ms INTEGER NOT NULL CHECK(last_time_ms >= 0),
    calls_spent INTEGER NOT NULL DEFAULT 0 CHECK(calls_spent >= 0),
    outstanding INTEGER NOT NULL DEFAULT 0 CHECK(outstanding >= 0)
) STRICT;
CREATE TABLE mcp_epochs (
    epoch INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT UNIQUE,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    generation INTEGER NOT NULL CHECK(generation > 0),
    server_digest TEXT NOT NULL,
    closed INTEGER NOT NULL DEFAULT 0 CHECK(closed IN (0,1))
) STRICT;
CREATE TABLE mcp_interactions (
    interaction_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    request_digest TEXT NOT NULL,
    raw_digest TEXT NOT NULL,
    request BLOB NOT NULL,
    origin_invocation TEXT,
    origin_attempt TEXT,
    origin_run TEXT,
    protection BLOB NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    revision INTEGER NOT NULL CHECK(revision > 0),
    state TEXT NOT NULL CHECK(state IN ('pending','intent','result','cancelled')),
    work_delivery TEXT NOT NULL DEFAULT 'not_started' CHECK(work_delivery IN ('not_started','uncertain','completed')),
    deadline_ms INTEGER NOT NULL CHECK(deadline_ms > 0),
    outstanding INTEGER NOT NULL DEFAULT 0 CHECK(outstanding IN (0,1)),
    call_charged INTEGER NOT NULL DEFAULT 0 CHECK(call_charged IN (0,1)),
    reply BLOB,
    reply_digest TEXT,
    usage BLOB,
    send_attempts INTEGER NOT NULL DEFAULT 0 CHECK(send_attempts >= 0),
    transport_accepted INTEGER NOT NULL DEFAULT 0 CHECK(transport_accepted IN (0,1)),
    settlement_digest TEXT,
    CHECK((reply IS NULL AND reply_digest IS NULL) OR
          (reply IS NOT NULL AND reply_digest IS NOT NULL)),
    CHECK(state != 'result' OR reply IS NOT NULL),
    CHECK(reply IS NULL OR state IN ('result','cancelled'))
    ,CHECK((origin_invocation IS NULL) = (origin_attempt IS NULL)),
    CHECK((origin_invocation IS NULL) = (origin_run IS NULL))
) STRICT;
CREATE INDEX mcp_interactions_owner ON mcp_interactions(owner_key,interaction_id);
CREATE INDEX mcp_interactions_origin ON mcp_interactions(origin_run,origin_invocation,origin_attempt);
CREATE TABLE mcp_receipts (
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    principal TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    interaction_id TEXT NOT NULL REFERENCES mcp_interactions(interaction_id),
    reply_digest TEXT NOT NULL,
    work_delivery TEXT NOT NULL CHECK(work_delivery IN ('not_started','uncertain','completed')),
    PRIMARY KEY(owner_key,principal,idempotency_key)
) STRICT;
CREATE TABLE mcp_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    interaction_id TEXT REFERENCES mcp_interactions(interaction_id),
    kind TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    timestamp_ms INTEGER NOT NULL CHECK(timestamp_ms >= 0),
    payload BLOB NOT NULL
) STRICT;
CREATE INDEX mcp_events_owner ON mcp_events(owner_key,sequence);
CREATE TABLE mcp_late_results (
    interaction_id TEXT NOT NULL REFERENCES mcp_interactions(interaction_id),
    digest TEXT NOT NULL,
    reply BLOB NOT NULL,
    protection BLOB NOT NULL,
    raw_digest TEXT NOT NULL,
    work_delivery TEXT NOT NULL CHECK(work_delivery IN ('not_started','uncertain','completed')),
    usage BLOB,
    PRIMARY KEY(interaction_id,digest)
) STRICT;
