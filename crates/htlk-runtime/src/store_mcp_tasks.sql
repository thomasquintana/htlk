-- Client-direction protocol tasks wrap existing accounted interactions. Task
-- cancellation never settles or releases the underlying uncertain work.
CREATE TABLE mcp_received_tasks (
    task_id TEXT PRIMARY KEY CHECK(length(task_id)=64),
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    interaction_id TEXT NOT NULL UNIQUE REFERENCES mcp_interactions(interaction_id),
    generation INTEGER NOT NULL CHECK(generation>0),
    method TEXT NOT NULL CHECK(method IN ('sampling/createMessage','elicitation/create')),
    created_ms INTEGER NOT NULL CHECK(created_ms>=0),
    updated_ms INTEGER NOT NULL CHECK(updated_ms>=created_ms),
    expires_ms INTEGER NOT NULL CHECK(expires_ms>=created_ms),
    ttl_ms INTEGER NOT NULL CHECK(ttl_ms>=0),
    poll_interval_ms INTEGER NOT NULL CHECK(poll_interval_ms>0),
    status TEXT NOT NULL CHECK(status IN ('working','input_required','completed','failed','cancelled')),
    revision INTEGER NOT NULL CHECK(revision>0),
    result_kind TEXT CHECK(result_kind IN ('result','error')),
    result BLOB,
    result_digest TEXT,
    source_reply_digest TEXT,
    result_capacity INTEGER NOT NULL CHECK(result_capacity>0),
    acceptance_send_attempts INTEGER NOT NULL DEFAULT 0 CHECK(acceptance_send_attempts>=0),
    acceptance_accepted INTEGER NOT NULL DEFAULT 0 CHECK(acceptance_accepted IN (0,1)),
    CHECK(expires_ms=created_ms+ttl_ms),
    CHECK((result IS NULL)=(result_kind IS NULL)),
    CHECK((result IS NULL)=(result_digest IS NULL)),
    CHECK((status IN ('working','input_required'))=(result IS NULL)),
    CHECK(result IS NULL OR length(result)<=result_capacity)
) STRICT;
CREATE INDEX mcp_received_tasks_owner ON mcp_received_tasks(owner_key,server_digest,task_id);
CREATE INDEX mcp_received_tasks_interaction ON mcp_received_tasks(owner_key,interaction_id);
-- One effectful cancellation receipt per task. A redelivered identical RPC gets
-- the committed cancellation response; a new RPC against a terminal task fails.
CREATE TABLE mcp_received_task_cancel_receipts (
    request_key TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    task_id TEXT NOT NULL UNIQUE REFERENCES mcp_received_tasks(task_id),
    request_digest TEXT NOT NULL
) STRICT;
