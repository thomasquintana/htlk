-- Durable host subscription desire is independent of a process-local connection.
CREATE TABLE mcp_resource_subscriptions (
    subscription_id TEXT PRIMARY KEY,
    owner_key TEXT NOT NULL REFERENCES mcp_owners(owner_key),
    server_digest TEXT NOT NULL,
    uri TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    generation INTEGER NOT NULL CHECK(generation > 0),
    revision INTEGER NOT NULL CHECK(revision > 0),
    state TEXT NOT NULL CHECK(state IN ('inactive','active','uncertain','gap')),
    gap_count INTEGER NOT NULL CHECK(gap_count >= 0),
    operation_id TEXT REFERENCES mcp_subscription_operations(operation_id),
    UNIQUE(owner_key,server_digest,uri),
    CHECK((state = 'uncertain') = (operation_id IS NOT NULL))
) STRICT;
CREATE INDEX mcp_subscriptions_owner ON mcp_resource_subscriptions(owner_key,subscription_id);
CREATE TABLE mcp_subscription_consumers (
    subscription_id TEXT NOT NULL REFERENCES mcp_resource_subscriptions(subscription_id),
    subscriber_key TEXT NOT NULL,
    desired INTEGER NOT NULL CHECK(desired IN (0,1)),
    PRIMARY KEY(subscription_id,subscriber_key)
) STRICT;
CREATE TABLE mcp_subscription_operations (
    operation_id TEXT PRIMARY KEY,
    subscription_id TEXT NOT NULL REFERENCES mcp_resource_subscriptions(subscription_id),
    session_id TEXT NOT NULL REFERENCES mcp_epochs(session_id),
    generation INTEGER NOT NULL CHECK(generation > 0),
    action TEXT NOT NULL CHECK(action IN ('subscribe','unsubscribe')),
    prior_state TEXT NOT NULL CHECK(prior_state IN ('inactive','active')),
    outcome TEXT NOT NULL CHECK(outcome IN ('uncertain','acknowledged','not_sent'))
) STRICT;
CREATE INDEX mcp_subscription_operations_resource ON mcp_subscription_operations(subscription_id,operation_id);
