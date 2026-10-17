# Durable typed input waits and host control

Runtime §11 defines input waits separately from exact-action policy approvals.
The execution engine now opens deterministic generation-zero input waits without
holding a calculation worker, recording topic, request artifact, response type,
expiry, invocation and protection. Requests are addressed by wait ID, never by a
source-authored global correlation string.

`RuntimePolicy::wait_policy` chooses a trusted authority namespace and version for
the admitted question. The wait freezes that policy and the actual requesting
principal. Current reply authorization receives these facts separately from the
source topic. `WaitView::authorization_policy` lets an authorized host route the
request; a source-authored topic or ordinary approval-like field grants no rights.

`Runtime::submit_reply` authenticates through HostContext, checks purpose and current
policy, and routes changes to the local fenced owner. Type validation and current
coordinator deadline checks precede one atomic wait/reply/host-receipt commit.
The node then applies its ordinary postcondition and output publication. Reply
acceptance is distinct from graph-node success and grants no policy authority.

`RuntimePolicy::reply_labels` classifies authenticated response data. Its trusted
labels are saved with the accepted reply and atomically unioned into the invocation's
control protection before any postcondition or output acceptance. Copies and later
boundaries retain them; historical receipt retries cannot reclassify old data.
`approval_labels` similarly protects authenticated dispositions without publishing
an approval Boolean. Approver authorization receives the exact authority, requesting
principal, request digest and sensitivity labels, and is rechecked by the owner.
Unrenewed approval expiry records an expired request and `E_APPROVAL_DENIED` at
the waiting node. Historical accepted dispositions remain available without a live
worker; new late replies and dispositions report their explicit expiry classification.

Identical accepted replies return historical receipts after expiry/terminal outcome,
including after worker shutdown. Conflicting request content under the same key
fails. New replies cannot resolve cancelled/expired or approval-purpose requests.
Input expiry fails with E_WAIT_EXPIRED. Cancellation closes open waits and fences
future output acceptance, preserving spent usage and earlier terminal outcomes.

`Runtime::cancel_run` routes authenticated commands to the same authority, commits
descendant stops and its receipt atomically, and preserves already terminal runs.
Cancellation of an already terminal run saves an immutable no-op receipt without
a worker and retains the winning outcome. Reusing that key with a different reason
conflicts. Local execution must be launched for active mutations; historical receipts and ordinary
read inspection do not require a live worker. Cross-process command forwarding is
an embedding concern; unavailable local authority returns E_FENCED for safe retry.

Authorized `inspect_run`, `inspect_node` and `inspect_graph` expose logical manifests,
status/reasons, open waits, usage, origin/definition identities and graph hierarchy.
Run inspection verifies a stable sequence to avoid torn observations. Raw reply
evidence is not exposed as a successful graph artifact before node acceptance.

Tests cover policy/type rejection without consuming a wait, accepted reply history,
conflicting duplicate, success, expiry, cancellation, closed waits and cancellation
key conflicts, including terminal no-op receipt replay. See [MCP and approvals](runtime-mcp.md)
for exact-action generations and [extension](runtime-extension.md) for atomic planning replies.
