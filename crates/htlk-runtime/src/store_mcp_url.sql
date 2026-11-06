-- Protected URL-mode state. URL, message, elicitationId and arbitrary metadata
-- occur only in bounded protected blobs, never in journal/event payloads.
CREATE TABLE mcp_url_interactions (
    url_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    callback_id TEXT REFERENCES mcp_interactions(interaction_id),
    origin BLOB,
    params BLOB NOT NULL,
    params_digest TEXT NOT NULL,
    protection BLOB NOT NULL,
    deadline_ms INTEGER NOT NULL CHECK(deadline_ms > 0),
    consent TEXT NOT NULL DEFAULT 'pending' CHECK(consent IN ('pending','accept','decline','cancel')),
    result BLOB,
    result_digest TEXT,
    completed_ms INTEGER CHECK(completed_ms >= 0),
    CHECK((result IS NULL) = (result_digest IS NULL)),
    CHECK((consent = 'pending') = (result IS NULL))
) STRICT;
CREATE INDEX mcp_url_owner ON mcp_url_interactions(owner_key,url_id);
CREATE TABLE mcp_url_required_errors (
    error_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    request_id BLOB NOT NULL,
    error BLOB NOT NULL,
    error_digest TEXT NOT NULL,
    protection BLOB NOT NULL,
    origin BLOB
) STRICT;
CREATE TABLE mcp_url_required_links (
    error_id TEXT NOT NULL REFERENCES mcp_url_required_errors(error_id),
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    url_id TEXT NOT NULL REFERENCES mcp_url_interactions(url_id),
    PRIMARY KEY(error_id,ordinal),
    UNIQUE(error_id,url_id)
) STRICT;
CREATE TABLE mcp_url_receipts (
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    principal TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    url_id TEXT NOT NULL REFERENCES mcp_url_interactions(url_id),
    result_digest TEXT NOT NULL,
    PRIMARY KEY(owner_key,principal,idempotency_key)
) STRICT;
