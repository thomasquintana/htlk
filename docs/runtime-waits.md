# Durable typed input waits and host control

Runtime §11 defines input waits separately from exact-action policy approvals.
The execution engine now opens deterministic generation-zero input waits without
holding a calculation worker, recording topic, request artifact, response type,
expiry, invocation and protection. Requests are addressed by wait ID, never by a
source-authored global correlation string.

`Runtime::submit_reply` authenticates through HostContext, checks purpose and current
policy, and routes changes to the local fenced owner. Type validation and current
coordinator deadline checks precede one atomic wait/reply/host-receipt commit.
The node then applies its ordinary postcondition and output publication. Reply
acceptance is distinct from graph-node success and grants no policy authority.

Identical accepted replies return historical receipts after expiry/terminal outcome,
including after worker shutdown. Conflicting request content under the same key
fails. New replies cannot resolve cancelled/expired or approval-purpose requests.
Input expiry fails with E_WAIT_EXPIRED. Cancellation closes open waits and fences
future output acceptance, preserving spent usage and earlier terminal outcomes.

`Runtime::cancel_run` routes authenticated commands to the same authority, commits
descendant stops and its receipt atomically, and preserves already terminal runs.
Local execution must be launched for new mutations; historical receipts and ordinary
read inspection do not require a live worker. Cross-process command forwarding is
an embedding concern; unavailable local authority returns E_FENCED for safe retry.

Authorized `inspect_run`, `inspect_node` and `inspect_graph` expose logical manifests,
status/reasons, open waits, usage, origin/definition identities and graph hierarchy.
Run inspection verifies a stable sequence to avoid torn observations. Raw reply
evidence is not exposed as a successful graph artifact before node acceptance.

Tests cover policy/type rejection without consuming a wait, accepted reply history,
conflicting duplicate, success, expiry, cancellation, closed waits and cancellation
key conflicts. Exact-action approval generations and live MCP integration follow
as separately tracked execution tasks.
