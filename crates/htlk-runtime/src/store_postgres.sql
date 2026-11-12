-- Native PostgreSQL layout 1. Canonical CBOR remains authoritative.
-- Arbitrary UTF-8 identities are BYTEA (including NUL). Generated SHA-256 keys
-- bound B-tree entries without narrowing admitted identity sizes. The preimage
-- registry rejects collisions before any ON CONFLICT operation can conflate keys.
CREATE TABLE htlk_metadata (
    singleton BIGINT PRIMARY KEY CHECK(singleton=1),
    application TEXT NOT NULL,
    version BIGINT NOT NULL,
    layout_digest TEXT NOT NULL
);
-- Bound stored fields on the server before transmitting their bytes. Overloads
-- preserve exact SQL types, NULLs and original projected column names.
CREATE FUNCTION htlk_field(value BYTEA, maximum BIGINT) RETURNS BYTEA
LANGUAGE plpgsql IMMUTABLE STRICT AS $$ BEGIN
    IF octet_length(value)>maximum THEN RAISE EXCEPTION 'runtime stored field exceeds configured limit' USING ERRCODE='22001'; END IF;
    RETURN value;
END $$;
CREATE FUNCTION htlk_field(value TEXT, maximum BIGINT) RETURNS TEXT
LANGUAGE plpgsql IMMUTABLE STRICT AS $$ BEGIN
    IF octet_length(value)>maximum THEN RAISE EXCEPTION 'runtime stored field exceeds configured limit' USING ERRCODE='22001'; END IF;
    RETURN value;
END $$;
CREATE FUNCTION htlk_field(value BIGINT, maximum BIGINT) RETURNS BIGINT
LANGUAGE sql IMMUTABLE STRICT AS $$ SELECT value $$;
CREATE TABLE htlk_opaque_keys (
    digest BYTEA PRIMARY KEY CHECK(octet_length(digest)=32),
    original BYTEA NOT NULL
);
CREATE FUNCTION htlk_check_keys() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE key_bytes BYTEA; retained BYTEA; field TEXT;
BEGIN
    FOREACH field IN ARRAY TG_ARGV LOOP
        EXECUTE format('SELECT ($1).%I', field) INTO key_bytes USING NEW;
        IF key_bytes IS NOT NULL THEN
            INSERT INTO htlk_opaque_keys VALUES(sha256(key_bytes),key_bytes)
                ON CONFLICT(digest) DO NOTHING;
            SELECT original INTO STRICT retained FROM htlk_opaque_keys WHERE digest=sha256(key_bytes);
            IF retained <> key_bytes THEN
                RAISE EXCEPTION 'HTLK identity integrity failure' USING ERRCODE='HT001';
            END IF;
        END IF;
    END LOOP;
    RETURN NEW;
END $$;

CREATE TABLE runs (
    run_id BYTEA NOT NULL,
    run_key BYTEA GENERATED ALWAYS AS (sha256(run_id)) STORED PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    root_scope TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','succeeded','failed','cancelled')),
    sequence BIGINT NOT NULL CHECK(sequence>=1),
    owner BYTEA,
    fence BIGINT NOT NULL CHECK(fence>=0),
    lease_expires_ms BIGINT,
    last_time_ms BIGINT NOT NULL CHECK(last_time_ms>=0),
    CHECK((owner IS NULL)=(lease_expires_ms IS NULL))
);
CREATE TRIGGER runs_keys BEFORE INSERT OR UPDATE OF run_id ON runs
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('run_id');
CREATE INDEX committed_run_time ON runs(last_time_ms);
CREATE TABLE journal (
    run_id BYTEA NOT NULL,
    run_key BYTEA GENERATED ALWAYS AS (sha256(run_id)) STORED REFERENCES runs(run_key),
    sequence BIGINT NOT NULL CHECK(sequence>=1),
    event_type TEXT NOT NULL,
    record_version TEXT NOT NULL CHECK(record_version='0.1'),
    actor BYTEA NOT NULL,
    timestamp_ms BIGINT NOT NULL CHECK(timestamp_ms>=0),
    causation BYTEA NOT NULL,
    command_id BYTEA,
    payload BYTEA NOT NULL,
    PRIMARY KEY(run_key,sequence)
);
CREATE TRIGGER journal_keys BEFORE INSERT OR UPDATE OF run_id ON journal
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('run_id');
CREATE TABLE outbox (
    run_id BYTEA NOT NULL,
    run_key BYTEA GENERATED ALWAYS AS (sha256(run_id)) STORED,
    sequence BIGINT NOT NULL,
    delivered BIGINT NOT NULL DEFAULT 0 CHECK(delivered IN (0,1)),
    PRIMARY KEY(run_key,sequence),
    FOREIGN KEY(run_key,sequence) REFERENCES journal(run_key,sequence)
);
CREATE TRIGGER outbox_keys BEFORE INSERT OR UPDATE OF run_id ON outbox
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('run_id');
CREATE INDEX outbox_pending ON outbox(delivered,run_key,sequence);
CREATE TABLE records (
    run_id BYTEA NOT NULL,
    run_key BYTEA GENERATED ALWAYS AS (sha256(run_id)) STORED REFERENCES runs(run_key),
    family TEXT NOT NULL,
    record_id BYTEA NOT NULL,
    record_key BYTEA GENERATED ALWAYS AS (sha256(record_id)) STORED,
    revision BIGINT NOT NULL CHECK(revision>=1),
    final BIGINT NOT NULL CHECK(final IN (0,1)),
    payload BYTEA NOT NULL,
    PRIMARY KEY(run_key,family,record_key)
);
CREATE TRIGGER records_keys BEFORE INSERT OR UPDATE OF run_id,record_id ON records
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('run_id','record_id');
CREATE UNIQUE INDEX accepted_artifact_identity ON records(record_key) WHERE family='artifact';
CREATE INDEX records_by_identity ON records(family,record_key);
CREATE TABLE commands (
    run_id BYTEA NOT NULL,
    run_key BYTEA GENERATED ALWAYS AS (sha256(run_id)) STORED,
    command_id BYTEA NOT NULL,
    command_key BYTEA GENERATED ALWAYS AS (sha256(command_id)) STORED,
    request_digest TEXT NOT NULL,
    sequence BIGINT NOT NULL,
    PRIMARY KEY(run_key,command_key),
    FOREIGN KEY(run_key,sequence) REFERENCES journal(run_key,sequence)
);
CREATE TRIGGER commands_keys BEFORE INSERT OR UPDATE OF run_id,command_id ON commands
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('run_id','command_id');

CREATE TABLE graphs (fingerprint TEXT PRIMARY KEY, graph_id BYTEA NOT NULL, envelope BYTEA NOT NULL);
CREATE TABLE graph_provenance (
    fingerprint TEXT NOT NULL REFERENCES graphs(fingerprint),
    provenance_digest TEXT NOT NULL,
    payload BYTEA NOT NULL,
    PRIMARY KEY(fingerprint,provenance_digest)
);
CREATE TABLE value_blobs (blob_digest TEXT PRIMARY KEY, value_bytes BYTEA NOT NULL);
-- Database encoding is checked at connection time. This conversion explicitly
-- fixes UTF-8 rather than depending on a session-selected encoding.
CREATE FUNCTION htlk_utf8(TEXT) RETURNS BYTEA LANGUAGE sql IMMUTABLE STRICT
    AS $$ SELECT convert_to($1,'UTF8') $$;
CREATE TABLE artifact_producers (
    producer_kind TEXT NOT NULL CHECK(producer_kind IN ('run_input','node_output')),
    producer_id BYTEA NOT NULL,
    producer_key BYTEA GENERATED ALWAYS AS (sha256(producer_id)) STORED,
    extension_number BIGINT NOT NULL CHECK(extension_number>=0),
    port BYTEA NOT NULL,
    port_key BYTEA GENERATED ALWAYS AS (sha256(port)) STORED,
    artifact_id TEXT NOT NULL UNIQUE,
    artifact_key BYTEA GENERATED ALWAYS AS (sha256(htlk_utf8(artifact_id))) STORED,
    run_id BYTEA NOT NULL,
    run_key BYTEA GENERATED ALWAYS AS (sha256(run_id)) STORED,
    family TEXT NOT NULL DEFAULT 'artifact' CHECK(family='artifact'),
    PRIMARY KEY(producer_kind,producer_key,extension_number,port_key),
    FOREIGN KEY(run_key,family,artifact_key) REFERENCES records(run_key,family,record_key)
        DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER producers_keys BEFORE INSERT OR UPDATE OF producer_id,port,run_id ON artifact_producers
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('producer_id','port','run_id');
CREATE TABLE runtime_identity (
    singleton BIGINT PRIMARY KEY CHECK(singleton=1),
    namespace TEXT NOT NULL,
    next_run BIGINT NOT NULL CHECK(next_run>=0),
    last_time_ms BIGINT NOT NULL CHECK(last_time_ms>=0)
);
INSERT INTO runtime_identity VALUES(1,replace(gen_random_uuid()::text,'-',''),0,0);
CREATE TABLE host_receipts (
    principal BYTEA NOT NULL,
    principal_key BYTEA GENERATED ALWAYS AS (sha256(principal)) STORED,
    method TEXT NOT NULL,
    target BYTEA NOT NULL,
    target_key BYTEA GENERATED ALWAYS AS (sha256(target)) STORED,
    idempotency_key BYTEA NOT NULL,
    idempotency_key_hash BYTEA GENERATED ALWAYS AS (sha256(idempotency_key)) STORED,
    request_digest TEXT NOT NULL,
    run_id BYTEA NOT NULL,
    run_key BYTEA GENERATED ALWAYS AS (sha256(run_id)) STORED REFERENCES runs(run_key),
    expires_at_ms BIGINT NOT NULL CHECK(expires_at_ms>=0),
    PRIMARY KEY(principal_key,method,target_key,idempotency_key_hash)
);
CREATE TRIGGER host_receipts_keys BEFORE INSERT OR UPDATE OF principal,target,idempotency_key,run_id ON host_receipts
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('principal','target','idempotency_key','run_id');
CREATE TABLE late_responses (
    run_id BYTEA NOT NULL,
    run_key BYTEA GENERATED ALWAYS AS (sha256(run_id)) STORED REFERENCES runs(run_key),
    attempt_id TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    payload BYTEA NOT NULL,
    protection BYTEA NOT NULL,
    PRIMARY KEY(run_key,attempt_id,content_digest)
);
CREATE TRIGGER late_keys BEFORE INSERT OR UPDATE OF run_id ON late_responses
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('run_id');

CREATE TABLE mcp_owners (
    owner_key TEXT PRIMARY KEY,
    principal BYTEA NOT NULL,
    name BYTEA NOT NULL,
    generation BIGINT NOT NULL CHECK(generation>0),
    expires_ms BIGINT NOT NULL CHECK(expires_ms>=0),
    last_time_ms BIGINT NOT NULL CHECK(last_time_ms>=0),
    calls_spent BIGINT NOT NULL DEFAULT 0 CHECK(calls_spent>=0),
    outstanding BIGINT NOT NULL DEFAULT 0 CHECK(outstanding>=0)
);
CREATE TABLE mcp_epochs (
    epoch BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    session_id TEXT UNIQUE,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    generation BIGINT NOT NULL CHECK(generation>0),
    server_digest TEXT NOT NULL,
    closed BIGINT NOT NULL DEFAULT 0 CHECK(closed IN (0,1))
);
CREATE TABLE mcp_interactions (
    interaction_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    request_digest TEXT NOT NULL,
    raw_digest TEXT NOT NULL,
    request BYTEA NOT NULL,
    origin_invocation TEXT,
    origin_attempt TEXT,
    origin_run BYTEA,
    protection BYTEA NOT NULL,
    generation BIGINT NOT NULL CHECK(generation>0),
    revision BIGINT NOT NULL CHECK(revision>0),
    state TEXT NOT NULL CHECK(state IN ('pending','intent','result','cancelled')),
    work_delivery TEXT NOT NULL DEFAULT 'not_started' CHECK(work_delivery IN ('not_started','uncertain','completed')),
    deadline_ms BIGINT NOT NULL CHECK(deadline_ms>0),
    outstanding BIGINT NOT NULL DEFAULT 0 CHECK(outstanding IN (0,1)),
    call_charged BIGINT NOT NULL DEFAULT 0 CHECK(call_charged IN (0,1)),
    reply BYTEA,
    reply_digest TEXT,
    usage BYTEA,
    send_attempts BIGINT NOT NULL DEFAULT 0 CHECK(send_attempts>=0),
    transport_accepted BIGINT NOT NULL DEFAULT 0 CHECK(transport_accepted IN (0,1)),
    settlement_digest TEXT,
    CHECK((reply IS NULL)=(reply_digest IS NULL)),
    CHECK(state<>'result' OR reply IS NOT NULL),
    CHECK(reply IS NULL OR state IN ('result','cancelled')),
    CHECK((origin_invocation IS NULL)=(origin_attempt IS NULL)),
    CHECK((origin_invocation IS NULL)=(origin_run IS NULL))
);
CREATE INDEX mcp_interactions_owner ON mcp_interactions(owner_key,interaction_id);
CREATE INDEX mcp_interactions_origin ON mcp_interactions(sha256(origin_run),origin_invocation,origin_attempt);
CREATE TABLE mcp_receipts (
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    principal BYTEA NOT NULL,
    idempotency_key BYTEA NOT NULL,
    principal_key BYTEA GENERATED ALWAYS AS (sha256(principal)) STORED,
    idempotency_key_hash BYTEA GENERATED ALWAYS AS (sha256(idempotency_key)) STORED,
    interaction_id TEXT NOT NULL REFERENCES mcp_interactions(interaction_id),
    reply_digest TEXT NOT NULL,
    work_delivery TEXT NOT NULL CHECK(work_delivery IN ('not_started','uncertain','completed')),
    PRIMARY KEY(owner_key,principal_key,idempotency_key_hash)
);
CREATE TRIGGER mcp_receipts_keys BEFORE INSERT OR UPDATE OF principal,idempotency_key ON mcp_receipts
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('principal','idempotency_key');
CREATE TABLE mcp_events (
    sequence BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    interaction_id TEXT REFERENCES mcp_interactions(interaction_id),
    kind TEXT NOT NULL,
    generation BIGINT NOT NULL CHECK(generation>0),
    timestamp_ms BIGINT NOT NULL CHECK(timestamp_ms>=0),
    payload BYTEA NOT NULL
);
CREATE INDEX mcp_events_owner ON mcp_events(owner_key,sequence);
CREATE TABLE mcp_late_results (
    interaction_id TEXT NOT NULL REFERENCES mcp_interactions(interaction_id),
    digest TEXT NOT NULL,
    reply BYTEA NOT NULL,
    protection BYTEA NOT NULL,
    raw_digest TEXT NOT NULL,
    work_delivery TEXT NOT NULL CHECK(work_delivery IN ('not_started','uncertain','completed')),
    usage BYTEA,
    PRIMARY KEY(interaction_id,digest)
);
CREATE TABLE mcp_url_interactions (
    url_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    callback_id TEXT REFERENCES mcp_interactions(interaction_id),
    origin BYTEA,
    params BYTEA NOT NULL,
    params_digest TEXT NOT NULL,
    protection BYTEA NOT NULL,
    deadline_ms BIGINT NOT NULL CHECK(deadline_ms>0),
    consent TEXT NOT NULL DEFAULT 'pending' CHECK(consent IN ('pending','accept','decline','cancel')),
    result BYTEA,
    result_digest TEXT,
    completed_ms BIGINT CHECK(completed_ms>=0),
    CHECK((result IS NULL)=(result_digest IS NULL)),
    CHECK((consent='pending')=(result IS NULL))
);
CREATE INDEX mcp_url_owner ON mcp_url_interactions(owner_key,url_id);
CREATE TABLE mcp_url_required_errors (
    error_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    request_id BYTEA NOT NULL,
    error BYTEA NOT NULL,
    error_digest TEXT NOT NULL,
    protection BYTEA NOT NULL,
    origin BYTEA
);
CREATE TABLE mcp_url_required_links (
    error_id TEXT NOT NULL REFERENCES mcp_url_required_errors(error_id),
    ordinal BIGINT NOT NULL CHECK(ordinal>=0),
    url_id TEXT NOT NULL REFERENCES mcp_url_interactions(url_id),
    PRIMARY KEY(error_id,ordinal), UNIQUE(error_id,url_id)
);
CREATE TABLE mcp_url_receipts (
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    principal BYTEA NOT NULL,
    idempotency_key BYTEA NOT NULL,
    principal_key BYTEA GENERATED ALWAYS AS (sha256(principal)) STORED,
    idempotency_key_hash BYTEA GENERATED ALWAYS AS (sha256(idempotency_key)) STORED,
    url_id TEXT NOT NULL REFERENCES mcp_url_interactions(url_id),
    result_digest TEXT NOT NULL,
    PRIMARY KEY(owner_key,principal_key,idempotency_key_hash)
);
CREATE TRIGGER url_receipts_keys BEFORE INSERT OR UPDATE OF principal,idempotency_key ON mcp_url_receipts
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('principal','idempotency_key');
CREATE TABLE mcp_resource_subscriptions (
    subscription_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    uri BYTEA NOT NULL,
    uri_key BYTEA GENERATED ALWAYS AS (sha256(uri)) STORED,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    generation BIGINT NOT NULL CHECK(generation>0),
    revision BIGINT NOT NULL CHECK(revision>0),
    state TEXT NOT NULL CHECK(state IN ('inactive','active','uncertain','gap')),
    gap_count BIGINT NOT NULL CHECK(gap_count>=0),
    operation_id TEXT,
    UNIQUE(owner_key,server_digest,uri_key),
    CHECK((state='uncertain')=(operation_id IS NOT NULL))
);
CREATE TRIGGER subscription_keys BEFORE INSERT OR UPDATE OF uri ON mcp_resource_subscriptions
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('uri');
CREATE INDEX mcp_subscriptions_owner ON mcp_resource_subscriptions(owner_key,subscription_id);
CREATE TABLE mcp_subscription_consumers (
    subscription_id TEXT NOT NULL REFERENCES mcp_resource_subscriptions(subscription_id),
    subscriber_key BYTEA NOT NULL,
    subscriber_key_hash BYTEA GENERATED ALWAYS AS (sha256(subscriber_key)) STORED,
    desired BIGINT NOT NULL CHECK(desired IN (0,1)),
    PRIMARY KEY(subscription_id,subscriber_key_hash)
);
CREATE TRIGGER consumer_keys BEFORE INSERT OR UPDATE OF subscriber_key ON mcp_subscription_consumers
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('subscriber_key');
CREATE TABLE mcp_subscription_operations (
    operation_id TEXT PRIMARY KEY,
    subscription_id TEXT NOT NULL REFERENCES mcp_resource_subscriptions(subscription_id),
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    generation BIGINT NOT NULL CHECK(generation>0),
    action TEXT NOT NULL CHECK(action IN ('subscribe','unsubscribe')),
    prior_state TEXT NOT NULL CHECK(prior_state IN ('inactive','active')),
    outcome TEXT NOT NULL CHECK(outcome IN ('uncertain','acknowledged','not_sent'))
);
ALTER TABLE mcp_resource_subscriptions ADD FOREIGN KEY(operation_id) REFERENCES mcp_subscription_operations(operation_id);
CREATE TABLE mcp_received_tasks (
    task_id TEXT PRIMARY KEY CHECK(length(task_id)=64),
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    interaction_id TEXT NOT NULL UNIQUE REFERENCES mcp_interactions(interaction_id),
    generation BIGINT NOT NULL CHECK(generation>0),
    method TEXT NOT NULL CHECK(method IN ('sampling/createMessage','elicitation/create')),
    created_ms BIGINT NOT NULL CHECK(created_ms>=0),
    updated_ms BIGINT NOT NULL CHECK(updated_ms>=created_ms),
    expires_ms BIGINT NOT NULL CHECK(expires_ms>=created_ms),
    ttl_ms BIGINT NOT NULL CHECK(ttl_ms>=0),
    poll_interval_ms BIGINT NOT NULL CHECK(poll_interval_ms>0),
    status TEXT NOT NULL CHECK(status IN ('working','input_required','completed','failed','cancelled')),
    revision BIGINT NOT NULL CHECK(revision>0),
    result_kind TEXT CHECK(result_kind IN ('result','error')),
    result BYTEA,
    result_digest TEXT,
    source_reply_digest TEXT,
    result_capacity BIGINT NOT NULL CHECK(result_capacity>0),
    acceptance_send_attempts BIGINT NOT NULL DEFAULT 0 CHECK(acceptance_send_attempts>=0),
    acceptance_accepted BIGINT NOT NULL DEFAULT 0 CHECK(acceptance_accepted IN (0,1)),
    CHECK(expires_ms=created_ms+ttl_ms),
    CHECK((result IS NULL)=(result_kind IS NULL)),
    CHECK((result IS NULL)=(result_digest IS NULL)),
    CHECK((status IN ('working','input_required'))=(result IS NULL)),
    CHECK(result IS NULL OR length(result)<=result_capacity)
);
CREATE INDEX mcp_received_tasks_owner ON mcp_received_tasks(owner_key,server_digest,task_id);
CREATE TABLE mcp_received_task_cancel_receipts (
    request_key TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    task_id TEXT NOT NULL UNIQUE REFERENCES mcp_received_tasks(task_id),
    request_digest TEXT NOT NULL
);
CREATE TABLE mcp_remote_tasks (
    intent_key TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    generation BIGINT NOT NULL,
    descriptor BYTEA NOT NULL,
    params BYTEA NOT NULL,
    origin_run BYTEA,
    origin_invocation TEXT,
    origin_attempt TEXT,
    protection BYTEA NOT NULL,
    deadline_ms BIGINT NOT NULL,
    task_id BYTEA,
    task_key BYTEA GENERATED ALWAYS AS (sha256(task_id)) STORED,
    snapshot BYTEA,
    snapshot_digest TEXT,
    acceptance BYTEA,
    acceptance_digest TEXT,
    expires_ms BIGINT,
    final_kind TEXT CHECK(final_kind IN ('result','error')),
    final_payload BYTEA,
    final_digest TEXT,
    result_status TEXT CHECK(result_status IN ('completed','failed','cancelled')),
    create_error BYTEA,
    create_error_digest TEXT,
    CHECK((task_id IS NULL)=(snapshot IS NULL)),
    CHECK((snapshot IS NULL)=(acceptance IS NULL)),
    CHECK((snapshot IS NULL)=(snapshot_digest IS NULL)),
    CHECK((acceptance IS NULL)=(acceptance_digest IS NULL)),
    CHECK((final_kind IS NULL)=(final_payload IS NULL)),
    CHECK((final_payload IS NULL)=(final_digest IS NULL)),
    CHECK((create_error IS NULL)=(create_error_digest IS NULL)),
    CHECK(create_error IS NULL OR task_id IS NULL),
    CHECK(result_status IS NULL OR final_kind='result'),
    CHECK((origin_run IS NULL)=(origin_invocation IS NULL)),
    CHECK((origin_run IS NULL)=(origin_attempt IS NULL)),
    UNIQUE(owner_key,server_digest,session_id,task_key)
);
CREATE TRIGGER remote_task_keys BEFORE INSERT OR UPDATE OF task_id ON mcp_remote_tasks
    FOR EACH ROW EXECUTE FUNCTION htlk_check_keys('task_id');
CREATE INDEX mcp_remote_tasks_owner ON mcp_remote_tasks(owner_key,server_digest);
