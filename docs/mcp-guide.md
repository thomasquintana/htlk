# Using MCP with HTLK

MCP connects a workflow to external tools, resources, and prompt data. Configure an `Arc<dyn McpAdapter>` with
`Runtime::with_mcp` and launch `ExecutionEngine` to execute compiler-pinned calls,
resource reads, URI-template reads and prompt retrievals.

Every tool produces one `McpToolResult` envelope under a frozen host-selected
policy. This guide covers transport integration, content helpers, evidence, and
schema proposals. Exact result/extraction/inference rules are specified in the
[MCP specification](../specs/mcp-spec.md); the [runtime guide](runtime-guide.md)
covers registration and host lifecycle.

## Run a host-owned catalog browser

The complete [`mcp_client` example](../crates/htlk-runtime/examples/mcp_client.rs)
opens a host-owned session independently of a workflow, enumerates tools, prompts,
resources, and resource templates, and optionally monitors one explicit resource.
It includes persistence, current callback authorization, a roots provider, and a
bounded event consumer. Start with its offline help:

```console
cargo run -p htlk-runtime --example mcp_client --locked -- --help
```

Supply the expected implementation **name and version** from your actual server's
`initialize.serverInfo`. These are identity pins, not the MCP protocol version or
arbitrary labels. The client negotiates MCP 2025-11-25. For stdio, supply an absolute
program path followed by its arguments:

```console
cargo run -p htlk-runtime --example mcp_client --locked -- stdio SERVER_NAME SERVER_VERSION /absolute/path/to/server ARG
```

For Streamable HTTP, supply the exact endpoint:

```console
cargo run -p htlk-runtime --example mcp_client --locked -- http SERVER_NAME SERVER_VERSION https://service.example/mcp
```

Replace those placeholders with your deployment. Options precede `stdio` or `http`;
all arguments after the stdio program belong to the child. Stdio inherits the host
environment. HTTP accepts HTTPS or numeric-loopback HTTP development endpoints,
without userinfo, query parameters, or fragments. `--help` exits before opening a
store, spawning a child, or contacting an endpoint; invalid arguments produce a
fixed diagnostic that excludes argument values.

Initialization defaults to 5,000 ms. Set `--connect-timeout-ms N` before the
transport to allow a slower server to start; accepted values are 1,000–120,000 ms.
For example, give a context stdio provider that loads local indexes 30 seconds:

```console
cargo run -p htlk-runtime --example mcp_client --locked -- --connect-timeout-ms 30000 stdio SERVER_NAME SERVER_VERSION /absolute/path/to/context-server ARG
```

This sets the native client's startup/default timeout, including host callback
deadlines. Raising it does not extend the explicit five-second catalog traversal
and subscription budgets or the 20-second overall discovery deadline. A setting
below 5,000 ms also lowers those operations' effective ceiling through the native
client limits. It never retries initialization or dispatches extra operations.

### Persist intent and receive bounded updates

This invocation uses a SQLite store, exposes one advisory root, and monitors one
operator-selected resource for 15 seconds after discovery:

```console
cargo run -p htlk-runtime --example mcp_client --locked -- --store ./mcp-client.sqlite --root file:///absolute/workspace/ --subscribe context://status --watch-seconds 15 http SERVER_NAME SERVER_VERSION https://service.example/mcp
```

The store defaults to in-memory when `--store` is omitted. A file store retains
interaction evidence and subscription intent across runs; the example acquires a
300-second owner lease, covering the maximum 120-second startup, 20-second discovery,
60-second watch, bounded subscription requests and cleanup with margin. It opens a
fresh transport epoch and, on normal completion, releases its subscription consumer,
closes that exact epoch, shuts down the native client, and releases ownership. The
parent directory for the store must already exist. Reopening does not automatically
restore old subscriptions or retry uncertain operations. Monitoring a retained URI
from an earlier epoch reports `Gap` until an explicit restoration command binds that
record to the new session. A host building continuous monitoring should renew
ownership and authorize restoration through the
[durable subscription APIs](#durable-resource-monitoring).

Each inventory has a five-second traversal budget, 16-page and 512-item ceilings,
256 KiB per page, and 2 MiB cumulative result bytes. All four share a 20-second
discovery deadline. Output reports complete, incomplete, or unsupported inventories
and prints validated descriptor JSON. An incomplete result exits unsuccessfully
after cleanup. Catalog pagination is not a snapshot of a changing server.

`--watch-seconds` accepts 0–60; monitoring a resource requires a positive value.
The selected server must advertise `resources.subscribe`. Subscription reports
distinguish desired presence from acknowledged remote state and expose unconfirmed
release. Notifications produce value-free event identifiers. Receipt never reads
resource content, refreshes catalogs, invokes tools, gets prompt messages, expands
templates, or adopts compiler contracts. HTTP idle reception depends on the server's
optional GET/SSE support; the example reports whether it is available.

### Roots, headless elicitation, and credentials

The roots provider returns only the explicitly supplied `--root`, or an empty list.
It does not inspect the filesystem. Roots are advisory and do not establish a sandbox.
The example installs `McpAwaitReply` for form and URL elicitation. Its separate event
consumer authorizes inspection and submits an addressed **decline**, bound to the
saved request digest and a stable idempotency key. It never fabricates form answers,
accepts URL consent, navigates a URL, or retries the requesting operation. The
example's module rustdoc explains where an interactive frontend or a real accounted
sampling handler connects. Sampling remains unadvertised in this runnable browser.

For an operator-managed static HTTP bearer token, add `--bearer-env TOKEN_VARIABLE`
before `http`; the value is read from that environment variable rather than a command
argument. Keep the credential out of the store path and URL. Authentication concerns
are observed even if initialization fails; the example does not refresh credentials
or replay requests. For host-managed OAuth, exact credential binding and refresh,
use the [authorization integration](#mcp-server-authorization). The example's
single-principal callback policy is the starting point for application-specific
actor authentication and protection-label enforcement.

## Trusted transport configuration

`NativeMcpAdapter` routes deployment IDs to operator-supplied `McpEndpoint::Stdio`
or `McpEndpoint::Http` configurations. Executables contain compound server identity
and operation/schema pins; credentials and connection locations remain host data.
Native sessions are scoped to the run, authenticated principal, ownership fence
and compound server identity. Different tools on that server share its session;
different runs, including those by the same principal, remain isolated. A dedicated
I/O thread retains transport resources across the execution workers' temporary
reactors. One session negotiates MCP 2025-11-25 once and checks implementation
name/version; every attempt checks the selected capability and live descriptor
through bounded pagination. `McpConnection` retains a checked operation handle;
the common bidirectional dispatcher owns the reader independently. No session mutex
is held across invocation, and no renegotiation remains after final policy/approval
and intent.
Failed negotiation cannot turn into a tool send.
HTTP redirects and automatic decompression are disabled. HTTP JSON/SSE and newline-
framed stdio responses are bounded before strict JSON parsing, preserving duplicate-
key, depth, Unicode and numeric-token rejection. Complete `Limits` reach the parser;
RPC matching and session/content-type checks precede completion claims. Native
clients answer reverse `ping` and reject unadvertised sampling instead of invoking
a model. Native processes are killed on shutdown; HTTP cleanup is best-effort.

Closing an attempt's connection releases its operation handle. Sessions remain live between
nodes and while a locally owned run waits. Run completion, failure, cancellation,
shutdown or ownership loss cancels their lifetime and triggers bounded transport
cleanup. A higher ownership fence cancels the old local execution; old cleanup is
keyed to its own generation and cannot close a new owner's session. The dispatcher
correlates concurrent requests independently and continues receiving while idle.
The host can call
`NativeMcpAdapter::shutdown().await` after stopping execution to acknowledge cleanup
before process exit (two-second caller bound). This permanently stops that adapter.

An expired HTTP session or ambiguous/interrupted exchange poisons the session.
Later connections fail `MCP_TRANSPORT`; the adapter never initializes a replacement
under existing handles or silently resends an operation. A correlated JSON-RPC
error completes the exchange and does not by itself poison the session. Per-call
response limits are reset for each request rather than shrinking future calls.
Sessions are process-local: durable graph recovery does not reconstruct browser
pages or sandbox handles after restart or ownership transfer. Reconciliation and
recreation belong in explicit graph operations supported by the external service.

For host-owned discovery/monitoring, `mcp::client::NativeMcpClient` provides explicit
open, inspection, close, and shutdown. `McpOwner` distinguishes host and run
namespaces and principal/generation identity. Explicitly reopening a closed host
session creates a new epoch; old handles stay closed. `McpClientPersistence`, installed
through `McpClientConfig::with_persistence`, retains owner leases and store-scoped
session identities. Acquire/renew/release host ownership explicitly through that
service. Run-owned persisted sessions independently check the actual run principal,
fence and lease; they cannot spend host-only callback budgets. Both outgoing requests
and event delivery recheck durable ownership. Transport handles remain process-local,
and restarting a transport owner allocates a fresh durable epoch. Without configured
persistence the client uses ephemeral ownership. See the
[conformance inventory](mcp-conformance.md) for callback implementation status.

`McpClientConfig` derives client capabilities from installed services and validates
queue, byte, frame, cumulative-message, and timeout bounds. Effectful host-owned
services require persistence and a `McpClientAuthorizer`. Run-owned callback
admission is tracked separately in the conformance inventory. Hosts
can install an `McpEventHandler` and use `NativeMcpAdapter::with_client_config` for
run-owned events. Event handling neither reads resources nor mutates workflows.
`McpClientSession::ended` reports overflow or consumer failure even when its event
queue cannot accept another event.

Requests reserve their bounded original/canonical response capacity before sending.
Unconsumed `McpClientReply` values retain that reservation, so callers should consume
or drop each reply after processing it. `raw_result` and `McpRpcError::as_bytes`
preserve exact payload bytes; their `JsonDocument` views provide canonical values
separately. Byte-span extraction occurs only after strict full-message admission.

### Accounted sampling and native host work

Sampling handlers receive the full pinned request, including text/image/audio,
system prompt, advisory model preferences, tools, choice and matching tool history.
Install `SamplingToolsContext` as well as the three individual sampling services
when supporting tools and authorized context together. That combined handler owns
bounded context selection and model routing; hints do not choose the actual model.
Return the actual selected model in the result and measured usage through `McpUsage`.
Absent usage remains unknown; `Some(0)` means measured zero. Provider format support
and hard token/cost enforcement belong to the installed host integration.

Use `context.work` to wrap one actual model operation with `McpHostWork::run`.
Supply a bounded protected descriptor identifying routing and context/input digests.
The wrapper commits a stable claim under the interaction's existing reservation,
then supplies an immutable `McpHostWorkReceipt`. Its `invoke` method can execute one
native child RPC on the actual callback session. Copies cannot pay for more calls.
The RPC inherits the parent deadline and cancellation; associated child callbacks
retain real interaction ancestry and consume additional independently checked
ancestor reservations. Ambiguous stdio callback nesting is refused rather than
assigned to an unrelated request.

Native result/error bytes are retained before returning to the model mapper, and
the mapped sampling result/usage are saved before `run` returns to the callback.
`McpInteractionHost::inspect_work` inspects routing and actual native outcome evidence;
`recover_work` retrieves a known sampling outcome after callback interruption or
store reopen without dispatch. Unknown completion stays uncertain. This wrapper
supports one accounted native operation; independent child workflows started with
`Runtime::start_run` do not inherit its budget.

After explicit reconciliation, recovery returns the exact selected late sampling
result and usage. The original callback timeout/cancellation reply remains unchanged.
Inspection authorizes evidence and its protection from one consistent store snapshot.

Retired transports release native client session slots. Highest owner generations
remain in a separate bounded `McpClientLimits::owner_identities` inventory (4096 by
default) for the client's lifetime. A full inventory refuses new owner identities;
it never forgets a fence to make room. Broken run sessions remain sticky until their
original execution lifetime ends.

The [sampling campaigns](../crates/htlk-runtime/tests/mcp_sampling.rs) demonstrate
multimodal tools/context, explicit routing, native child work and HTTP ancestry,
lost reply acknowledgement, tool-result continuation and interrupted-callback recovery.
The [runtime campaigns](../tests/runtime_mcp_callbacks.rs) run that work inside a
waiting graph tool with one graph worker, inspect every ancestor's accounting and
prove that sequential requests share the cumulative allowance.

### MCP-server authorization

Install `McpClientConfig::with_credential_provider` for host-managed connection
credentials. The shared native transport asks the provider before every HTTP POST,
including notifications and callback responses, every GET/SSE resumption, and
best-effort DELETE. `CredentialBinding::for_server` derives the exact compound
server digest and binds it to the trusted owner's principal and resource URL.
Provider-backed endpoints use query-free resource URLs, and combining a provider
with a static Authorization header is rejected before network dispatch. Stdio
continues to use the configured host environment.

`mcp::client::auth::OAuthClient` implements the pre-registered OAuth
authorization-code profile. A host connects it as follows:

1. Subscribe to `authentication_concerns()` on the configuration or native client
   before opening a session. A failed open has no session handle, but its concern
   remains available. Inspect the protected binding, epoch, HTTP method, optional
   outgoing RPC ID, and `McpAuthenticationReason`.
2. Pass the received `OAuthChallenge` to `OAuthClient::discover`. It uses a
   header-provided resource metadata URL or the path-specific/root RFC 9728
   well-known fallbacks, then the registered issuer's OAuth/OIDC metadata. Host
   `OAuthEndpointPolicy` separately approves resource metadata, AS metadata,
   authorization navigation and token destinations. HTTPS is required except for
   an explicit numeric-loopback development exception. Redirects are not followed.
3. Supply two independently generated CSPRNG 32-byte values through `OAuthEntropy`.
   Call `begin_authorization`, present its protected URL for host-controlled consent
   and navigation, and deliver the complete callback URL to `finish_authorization`.
   The mechanism checks the registered redirect, state, optional resource echo and
   issuer response, and exchanges the code once using S256 and the resource
   indicator. The pending transaction is single-use and has a finite lifetime.
4. Persist `OAuthTokens` in the host's protected store, keyed by server, principal,
   resource, issuer and client ID. `restore_tokens` imports trusted stored values
   using a host-computed remaining lifetime; unknown expiry permits refresh but
   not access-token dispatch. `OAuthTokens` itself is a read-only credential
   provider. A refreshing host provider serializes `OAuthClient::refresh` and
   atomically replaces rotated secrets. Mark a persisted refresh token
   unusable/uncertain before dispatch so an interrupted refresh cannot reuse it.
5. For an insufficient-scope concern, request the challenge's authoritative scopes
   through a new authorization flow. Keep an `OAuthChallengeBudget` for the original
   operation across refreshes/step-ups. No token exchange, refresh or concern
   automatically dispatches the original MCP request.

Credential acquisition is bounded by the native request/cleanup deadline and
session timeout. An acquisition failure returns redacted `E_POLICY` before send;
an interrupted acquisition is a distinct concern. HTTP 401/403 returns `E_POLICY`
with conservative delivery evidence and retains the parsed challenge (or its
redacted parsing failure). Neither a challenge nor fresh credentials proves that
an effectful request was not executed. Existing runtime admission, replay and
budget checks still govern every explicit retry. An idle GET authentication
failure ends reception with `McpSessionEnd::Authentication`.

The concern monitor retains only the latest process-local concern, independently
of the ordinary event queue. Its monotonic sequence exposes overwritten concerns;
the host controls protected presentation and permanent recording. Debug/errors
exclude credential values and remote diagnostic text. Credentials are not placed
in executable meaning, ordinary MCP evidence or request URLs.

Pre-registration supports public clients and confidential clients using
`client_secret_basic` or `client_secret_post`. Client ID Metadata Documents are
assessed and deferred pending host HTTPS publication and a draft-version contract.
Optional dynamic registration is assessed and deferred pending remote-registration
and secret-lifecycle policy. Independently versioned authorization extensions are
not implemented. OIDC discovery supplies OAuth endpoints; this profile does not
perform ID-token login. The [authentication campaigns](../crates/htlk-runtime/tests/mcp_auth.rs)
exercise these mechanisms against an independent local HTTP implementation,
including the native transport path.

### Catalog browsing and utility services

An initialized `McpClientSession` exposes `list_tools`, `list_prompts`,
`list_resources`, and `list_resource_templates` for one page, plus corresponding
`enumerate_*` methods for a bounded traversal. Each method checks the advertised
server category capability. Pages and descriptors are checked against the pinned
offline analyzer validators; browsing does not invoke tools, retrieve prompt
messages, read resources, expand templates, or adopt compiler contracts.

`McpServiceLimits` bounds a complete response and its elapsed RPC time.
`McpCatalogLimits` additionally bounds pages, retained items, and cumulative original
result-object bytes across the traversal. Session configuration imposes its own
ceilings. Enumeration keeps complete validated pages in server order, including
duplicates from changing catalogs. Inspect `McpCatalog::status` before treating the
result as complete: page/item/byte/deadline exhaustion, cancellation, cursor cycles,
and later protocol failures retain an explicitly incomplete prefix. Opaque cursors
preserve empty strings. `next_cursor` identifies the next unadopted page; an absent
cursor on an incomplete first page does not mean completion.

`complete` accepts a checked `McpCompletionRequest` targeting a prompt or resource
template, an argument name/prefix, and optional already-resolved arguments. It checks
the `completions` capability, the pinned request/result shapes, the 100-value ceiling,
and valid totals. `set_logging_level` accepts a closed `McpLoggingLevel` and requires
the `logging` capability. Both preserve protected remote errors and delivery facts.
Notifications arrive through the bounded `McpClientEvent` surface. Its
`notification` field contains a checked `McpNotification` for resource updates,
catalog changes, structured logging, and progress. Resource updates require
`resources.subscribe`; catalog changes require the corresponding `listChanged`
bit; logs require `logging`. Unsupported recognized events have an explicit
`Ignored` disposition, while malformed supported notifications fail pinned protocol
admission. Other protocol and local events, including task and addressed URL events,
retain their existing method/params representation. No event refreshes catalogs or
adopts compiler contracts.

### Roots and progress

Install a `McpClientService::Roots` handler that returns a bounded `roots` array.
The native callback path validates both the request and result against the pinned
protocol and enforces `file://` root URIs without accessing the filesystem. Roots
are advisory protocol boundaries, not a sandbox or permission to read files. The
handler uses the configured persistence, current authorizer, and bounded host worker
execution. Installing `RootsChanged` alongside `Roots` advertises `roots.listChanged`.
Call `McpClientSession::roots_changed` explicitly when host roots change. It sends
one bounded notification; it neither invokes the provider nor fetches any roots.
The peer can subsequently request `roots/list` through that installed handler.

Request progress by supplying `_meta.progressToken` in an explicit outgoing RPC.
`McpProgressToken` preserves integer versus string identity, including empty strings.
Outgoing tokens are single-use within a session epoch, preventing late updates from
aliasing a later operation. A typed progress event identifies the actual outgoing
request ID and carries a run origin only when that request already had verified
runtime context. Idle notifications never borrow the most recently active origin.
Unknown, late, wrong-response-stream, expired, and non-monotonic updates have explicit
ignored dispositions. Finite progress must increase, totals cannot decrease, and
work cannot exceed the last known total. Omitting a total preserves the known value.
Progress never proves completion or extends an absolute request deadline.

Accepted task progress retains the original creation request/token and monotonic
cursor until a terminal result/status observation, TTL, cancellation or the original
deadline. It can arrive on an idle stream or a matching task-control response stream;
it never adopts a task or changes its recorded state. Progress retention failures
are reported separately and cannot replace persisted acceptance or correlated errors.

For an incoming host callback that requested progress, `McpInteractionContext::progress`
contains an opaque `McpProgressReporter`. Construct a checked `McpProgress` with its
exact token and call `report`; clones share one monotonic cursor. The dispatcher
checks the actual admitted request, epoch, cancellation and deadline before sending
on separate bounded I/O-control capacity, with current disclosure authorization and
unioned request/progress protection. A reporter becomes unusable when its
handler completes or expires. Notifications return `McpNotificationReply`: success
means local flush or HTTP acceptance, not an RPC reply or proof of consumption.

### Durable resource monitoring

Use `McpSubscriptionHost` with the same `McpClientPersistence` that opened a durable
host-owned session. Construct a checked `McpSubscriptionConsumer` from the exact URI
and a stable local subscriber key. `subscribe` records that consumer's desired
presence before dispatch; repeating the same key does not add a reference. Other
consumers share one remote subscription. `unsubscribe` removes only the selected
consumer's desire and sends the remote request only after the final release.
Both protocol actions require `resources.subscribe`.

Each report exposes the durable record, attempted RPC count, and any delivery-aware
failure. Desired consumer count and remote state are independent. States distinguish
`Inactive`, acknowledged `Active`, `Uncertain`, and an epoch delivery `Gap`.
Acknowledgements recheck live owner generation, exact epoch, and persisted operation
identity. A dropped caller, remote error, or ambiguous send cannot blindly dispatch
the same claim again. A definitely not-sent operation can be attempted by a later
explicit command. Concurrent desire changes converge through bounded follow-up
transitions; inspect `converged()` and use explicit `reconcile` when that allowance
is exhausted. Reconciliation does not retry an uncertain operation.

`disconnect` durably closes the exact epoch and performs bounded transport cleanup,
while preserving desired consumers. `inspect` and paged `list` remain passive and
report gaps after closure or generation takeover. Opening a new transport alone
does not restore monitoring. An explicitly authorized embedding-host call to
`restore` binds the selected subscription and its previous epoch to the new session;
the durable journal records both epochs and the gap count. Old uncertainty remains
historical evidence. Stable consumer keys and intent survive process restart; remote
replay or uninterrupted delivery is not implied.

`McpSubscriptionLimits` bounds resource rows, consumer tombstones, retained operation
claims, accounted key/row bytes, and transitions per command. Inspection and mutation
are trusted embedding-host APIs: authenticate frontends before granting access to
the session or protected URI inventory. The manager supports host owners; it does
not borrow run budgets. Updates racing unsubscribe remain ordinary host events,
including updates already in flight after acknowledgement. They never trigger a
resource read, model call, catalog refresh/adoption, or workflow mutation.
Typed resource-update events also expose session-local subscription knowledge:
unknown/unsolicited URI, subscribe in flight, subscribed, unsubscribe in flight,
unsubscribed, or uncertain after failed/overlapping RPCs. Those observations remain
separate from durable desired consumers and never grant read authority. Independent
HTTP streams can race an acknowledgement; the event reflects state known at receipt.

### Experimental protocol tasks

MCP 2025-11-25 tasks are remote execution handles, separate from HTLK workflow
`task` declarations. For host tool calls, choose `McpToolTaskMode` explicitly with
`McpClientSession::prepare_tool_task_call`, then consume its `dispatch`. The API
checks server capabilities and descriptor required/optional/forbidden rules before
send. An `Accepted` reply is a handle, not tool output: use its bounded `get`,
`poll`, `result`, `cancel` or `wait` operations. `input_required` opens the result
stream for related callbacks; optional status events are unnecessary.

Link persistence before creation. Give retryable host commands a stable work key
using `McpPreparedToolCall::with_logical_key`; durable intent rejects another create
under that key after uncertain delivery or restart. Preparing a fresh key denotes
a new operation. Inspect retained acceptance, creation errors and final bytes through
the trusted store's `inspect_mcp_remote_task_intent` under your host authorization.
No restart or list scan automatically recreates an unacknowledged task.
Durable sessions reject raw task-augmented `tools/call` sends that bypass the
prepared descriptor/intent API.

Graph tools keep ordinary execution by default. To enable task-required descriptors,
share the runtime's actual store/clock and select deployment policy before constructing
the native adapter. This configuration fragment assumes persistence is already linked:

```rust
let config = config.with_graph_tasks(
    McpGraphTaskPolicy::Required { ttl_ms: 60_000 },
)?;
```

`Prefer` also selects tasks for optional descriptors. The exact descriptor/policy
choice freezes during discovery. Authored arguments remain unchanged; only final
`tasks/result` output reaches normal tool admission/normalization. Related callbacks
resolve their original invocation/attempt from durable identity, including on HTTP
idle GET after the creation RPC ended, and retain ordinary ancestor budget checks.
Automatic cross-epoch graph-task recovery is not implemented; retained identities
support explicit host reconciliation without permitting another create.

For incoming sampling/elicitation tasks, install ordinary handlers, persistence and
authorization, then call `with_task_receiver` with checked `McpTaskReceiverSettings`.
Set request-type flags explicitly; derived capabilities include only those installed
methods. The receiver saves acceptance before dispatching work, while get/list/result/
cancel use separate bounded control capacity. `tasks/cancel` persists cancellation
before replying and propagates a cooperative signal; uncertain underlying work stays
charged until explicit reconciliation. Cancelling a result wait alone leaves work
running. Unsupported task augmentation is ignored even when malformed.

Configure receiver result capacity at least `store.max_document_bytes / 8 + 256`,
control response capacity at least another 256 bytes larger, and client `frame_bytes`
large enough for that response. Retained row quotas reserve payload capacity plus
1 KiB per identity. For the default 16 MiB store limit, a 4 MiB client frame and
32 MiB client retained-byte budget can admit a full callback and bounded controls;
choose task row/retained quotas independently. Expired identities remain bounded
deduplication tombstones and continue counting toward those quotas. Expiry does not
prove that external work stopped.

The live dispatcher reserves 64 KiB of that budget for cancellation independently
of other task controls. Wire `tasks/cancel` requests are limited to 4 KiB and their
generated correlated responses to 8 KiB, so waiting result requests cannot consume
the bytes needed to confirm cancellation.

See [task qualification](testing.md#experimental-task-qualification) for runnable
independent-peer campaigns and the [task contract](../specs/mcp-client-spec.md#native-experimental-tasks)
for persistence, isolation and terminal-state guarantees.

### Addressed host interactions

Install `McpClientHandler` implementations for sampling and other services, link
`McpClientPersistence` and current authorization with `with_authorizer`, and create
`McpInteractionHost` from that same configuration. Host work and event consumers use
separate bounded worker pools so human waits cannot consume the event-delivery
capacity needed to answer them. Without explicit parent attribution, the configured
nesting allowance conservatively bounds active callbacks, including parallel ones.

`McpAwaitReply` implements form/URL elicitation by waiting for the addressed host API.
The `htlk/interaction` event identifies the interaction, request digest, and kind;
it contains no question/answer values. A polling/headless frontend can enumerate the
same identities with `list`. An authenticated frontend calls `inspect`,
then `respond` with the exact digest and an idempotency key. Responses are validated
against the original request schema and mode. The same API is used by terminal,
API, and CLI frontends. New human replies require a live original session; saved
receipts remain historical facts. `cancel` and `reconcile` have distinct grants.

Sampling handlers receive the complete structured request and return
`McpServiceOutcome`: underlying work delivery, a protocol result/error, optional
actual usage, and additional protection. Model routing and the actual generation
belong to the host. Tool-use requests are returned to the server, never automatically
executed by the client. An uncertain model operation retains its reservation even
if its callback error reply was delivered. Known saved replies can be retransmitted
after a lost acknowledgement without recalling the handler.

Form acceptance carries validated content; URL acceptance carries no content and
does not establish external completion. Applications present the requesting server
and full URL, obtain consent, and perform secure navigation themselves. The native
client never fetches the URL or its metadata as part of elicitation.

#### Durable URL consent and completion

Install `ElicitationUrl` with bound persistence and authorization for URL lifecycle
tracking. The callback event adds `url_interaction_id` to its usual callback identity.
`McpInteractionHost::list_urls` supplies the same `McpUrlAddress` values to polling
hosts. Call `inspect_url` to authorize and inspect the full URL, server/session
identity, request digest, protection, consent, deadline, session closure and optional
server completion timestamp. Submit `respond_url` with that parameter digest and an
idempotency key. Callback-originated responses still use the existing callback
ledger; error-originated consent saves a local receipt without fabricating a reply
to the original outgoing request. Decline and cancel are valid decisions too.

A correlated `-32042` error atomically registers its validated URL requirements before
the next frame on that stream is processed. `McpClientReply::url_interactions` reports
the addressed identities or an independent redacted registration failure. The
original `result`, error bytes and delivery classification remain unchanged. An
`htlk/url_interaction` event announces each requirement; use
`inspect_url_required_error` with `McpClientReply::request_id` to inspect its
exact protected error evidence. Request IDs preserve integer/string distinction.

Known `notifications/elicitation/complete` IDs are scoped to the authenticated owner,
compound server and durable transport epoch. The first observation persists and
emits `htlk/url_completed`; unknown and already-completed IDs are ignored. Both local
URL events carry only `interaction_id`, with server/session context on the event
envelope. Raw completion parameters, URLs and metadata are not forwarded. HTTP
processes complete trailing SSE frames already received in the response chunk,
including completion frames following the correlated error.

`htlk/url_registration_failed` carries the outgoing `request_id` and a redacted
failure `code`; the reply's separate registration field remains available when an
event cannot be delivered. Storage interruption may leave admission uncertain, so
inspect/list retained identities rather than blindly creating another interaction.
Receipt lookup remains available under current host grants and ownership after
deadline/session closure; it grants no new delivery. `expired(now_ms)` and
`session_closed` let frontends present missing-notification and disconnect states.
Manual continuation uses a separately authorized ordinary operation, with the normal
replay and budget rules. Neither consent nor completion automatically dispatches it.
Third-party user input stays outside HTLK; MCP-server connection authorization is
the separate mechanism described above. The [live URL campaigns](../crates/htlk-runtime/tests/mcp_url_live.rs)
exercise interactive and headless consumers over stdio and both HTTP response forms.

Streamable HTTP supports optional idle GET SSE, incremental bounded framing, and
GET resumption using the failed stream's event cursor. Resumption never repeats
the original operation POST. A GET 405 reports lack of idle-stream support while
ordinary POST requests remain available. `with_server_event_stream(false)` disables
the optional GET; the default graph adapter uses this setting when no host event
consumer is configured. Structured RPC errors remain available through the client
session API and protected runtime evidence; graph-visible failures retain redacted
runtime classifications.

`McpAdapter` is a trusted integration contract. Its connection resolver receives the
`McpRunContext`, containing the actual run ID, host principal, ownership fence and
execution-lifetime cancellation signal. All adapter methods that discover or send
operations receive this context. Runtime independently checks descriptor
digests and result values and imposes finite discovery/attempt timeouts. An adapter
returns raw result-object JSON; it cannot substitute text for structured tool output.
Resource results preserve text/decoded-byte order and separately authorize every
returned URI against the exact requested/expanded URI and principal, including
legitimate URIs different from the requested URI. Prompt messages remain
untrusted data and never trigger model or tool execution.

## Durable dispatch and retry

An admitted invocation freezes arguments and its input hash. Each one-based attempt
persists the exact method, parameters, encoded request, requested URI, timeout,
deduplication mechanism and ownership fence in `prepared`. Exact method, native
parameters, canonical encoded parameters and protocol key reach current authorization
before acquiring a connection lease. `mcp_policy_for_request` supports trusted
argument-specific approval, safety and protection decisions. Approval opens before
acquiring an attempt lease; policy/disposition are rechecked after discovery. The transaction that
records `possibly_sent` also spends a call and reserves concurrency in the run,
invocation and every containing scope/wrapper. Only then can the adapter send.

Raw responses or classified failures are persisted before local validation and atomic
output acceptance. Result protection is saved with the response; later policy changes
cannot remove those labels. Raw digests protect saved response bytes. Runtime response
bytes are bounded by one eighth of the tighter runtime/store document ceiling for
private journal/envelope headroom, independently of the evaluator's value bound.
Delivery is explicit: `not_sent`, `possibly_sent` or `response_received`. Malformed
uncorrelated protocol ingress cannot masquerade as completion. Known no-send and
correlated replies release concurrency, never spent calls; connection failures before
intent spend no calls. Ambiguity retains outstanding reservations even for trusted
replay-safe operations. A separate non-authoritative late-response inbox retains
late evidence: fenced or terminal results cannot publish outputs.

Retries require the authored count, exact failure code, explicit backoff, remaining
deadline/budget, current trusted replay safety and approval. Every attempt reuses the
frozen arguments. An unsafe `possibly_sent` takeover fails `E_EFFECT_UNCERTAIN`.
Recorded responses resume local validation without another external call.
Known no-send failures can retry unsafe operations only under authored operational
codes/count/delay; listing deterministic identity/descriptor/input/policy failures in
a retry set never repairs them. Intent saves policy version, replay permission, labels,
input hash and the exact approval pointer for audit.
An explicitly listed MCP postcondition rejection can retry only under that same
intersection before the invocation becomes terminal. Each local retry/failure
decision records its attempt, classification, policy snapshot and protection;
inspection distinguishes rejected/retried/accepted proposals from RPC delivery.

Independent replay safety defaults to false in `RuntimePolicy::mcp_policy`. A trusted
adapter may instead verify an actual enforced `McpDeduplication` contract. Protocol
keys use the invocation-stable effect digest; authored argument keys must already be
present and unchanged, with scope/uniqueness verified by the adapter. Native HTTP
`with_protocol_idempotency` accepts exact compound binding digests only when the
operator has verified endpoint enforcement of `Idempotency-Key`. Tool annotations
never confer replay safety or deduplication support.

Hard external token/cost budgets remain rejected before run admission because the
native adapter does not advertise trusted hard metering/reservation enforcement.

## Cancellation and shutdown

`McpRequest::cancellation` is a cooperative hint, subordinate to durable fences.
Recursive stops first commit cancellation/timeout and then signal local attempts.
Native HTTP/stdio clients send `notifications/cancelled` for the actual RPC ID while
keeping its reader live for correlated completion. Ignoring adapters are dropped
after finite `RuntimeConfig::cancellation_grace_ms` (default 100 ms), within the
attempt deadline. No completion acknowledgement means uncertainty and retained
reservations; cancellation cannot undo a completed external action.

Hints inherit execution-stop signals, including shutdown before tracking or queued
work. Shutdown bounds local network work without inventing terminal outcomes; saved
facts remain available for higher-fence recovery. Tracking registrations are removed
on completion even if an adapter retains a request/token for its own audit.

## Approval generations

Trusted `McpPolicy` can require an `approval_authority` and supplies a policy version.
The durable approval request binds the origin executable, node definition, frozen
input hash, invocation, compound operation, authority, requester, version and expiry.
`inspect_run` exposes the protected request and digest to authorized callers.

`submit_approval(context, wait_id, request_digest, allow, idempotency_key)` checks
authenticated approver policy, exact request, current generation and deadline, and
records an immutable disposition and approver. `submit_reply` cannot resolve approval
waits. Approval never produces graph-visible Boolean authority.

Changed policy requests create a fresh generation within the original invocation
deadline. An old accepted record stays immutable; its repeated submission returns a
historical receipt without authorizing the new request. Denial fails the invocation.
Hosts requiring renewal change their trusted request version rather than changing
frozen arguments or resetting the invocation deadline.

## Regression coverage

Compiler-to-actor tests execute decision-brief acceptance at reviews one/two/three
(4/6/8 calls), loop-limit fallback (8 calls), schema-failure retries, zero/exhausted
call budgets, unsafe takeover with late results, approval renewal/history/denial,
mixed resource snapshots, returned-URI authorization, templates and prompt messages.
Actual HTTP JSON/SSE and stdio peers exercise negotiation, pagination, exact requests,
raw duplicate keys, unsafe integer tokens, byte/depth bounds, cancellation, correlated
errors, reverse ping and refusal of sampling. Delivery tests cover known-unsent retry,
ambiguous protocol capacity, ignoring-adapter/shutdown grace, deterministic drift,
pre-connection parameter scope and invocation-stable dedup keys.

`tests/runtime_mcp_sessions.rs` executes browser
open → inspect and sandbox create → execute → inspect → destroy through compiled
graphs on all three transports. Session-local fixtures, fresh-session negative
controls, concurrent-run isolation and teardown evidence verify actual continuity.

See [testing](testing.md) for maintained commands and the conformance map.

## Consume result content

The explicitly linked pure library `htlk.mcp-content`, version `1`, provides
uniform access to text, structured, and multimodal tool envelopes. An empty registry
does not install it. In a host with `registry`, compiler `config`, and codec `limits`:

```rust,ignore
let library = link_mcp_content_library(&mut registry, &limits)?;
config.libraries.insert(library.implementation_digest(), library.clone());
```

Use that returned digest in a source `predicate_library` declaration selecting
`htlk.mcp-content` version `1`. Print the current complete manifest with:

```console
cargo run -p htlk-runtime --example mcp_content_manifest --locked
```

The following source **fragment** assumes that exact library is declared as
`content`, and its enclosing scope supplies/exports the node's ports:

```htlk
nodes {
    title = eval(string, content.checked(
        content.json_at(content.data(inputs.response), "/title")
    )) {
        inputs = { response = McpToolResult }
    }
}
```

`checked` uses the explicit string result annotation and validates the actual value.
Missing data fails the required check; it is not converted to an empty title.
Runnable consumer examples are checked in
[`tests/mcp_content_library.rs`](../tests/mcp_content_library.rs).

| Function | Signature | Behavior |
|---|---|---|
| `text_blocks` | `McpToolResult -> list(string)` | Original direct text blocks in order, without joining. |
| `content_kinds` | `McpToolResult -> list(string)` | Original content kinds, including media/resources. |
| `structured_content` | `McpToolResult -> optional(json)` | Original `structuredContent`. |
| `candidates` | `McpToolResult -> list(json)` | Candidate records and source locations. |
| `data` | `McpToolResult -> optional(json)` | Only the pinned policy's selected data. |
| `validation` | `McpToolResult -> json` | Independent schema assessments. |
| `select_candidate` | `(McpToolResult, string) -> optional(json)` | Exact candidate ID; does not rewrite policy. |
| `parse_json` | `string -> json` | Strict duplicate-aware JSON, without semantic repair. |
| `json_at` | `(json, string) -> optional(json)` | RFC 6901 pointer; missing target is absent, malformed/type/index traversal fails. |
| `checked` | `json -> T` | Identity with actual instantiated return-type validation. |
| `join_text` | `(list(string), string) -> string` | Explicit separator joining. |
| `text_contains` | `(string, string) -> boolean` | Literal substring check. |
| `regex_capture` | `(string, regex, integer) -> optional(string)` | At most one full match, then select capture. |

Regex capture zero is the whole match; no match/unmatched optional capture is
absent. Multiple matches and invalid capture indexes fail. Functions charge scan,
parse, copy, collection, and regex work before performing it. Exhaustion remains
`E_EXPRESSION_LIMIT`. The registry pins signatures and implementation; evaluator
and regex profiles remain independently pinned. No helper reads ambient services,
declassifies data, establishes declared compliance from recovered text, changes a
tool error into success, adopts a schema, or authorizes replay.

## Protected evidence

The runtime persists original bounded response bytes before local processing. An
immutable `<attempt-id>:normalization` observation binds response/binding/envelope
digests, assessments, extraction state, candidate/span references, failure, delivery,
effect uncertainty, and protection. Detailed diagnostics share the pinned allowance;
truncation is explicit. Recovery checks this saved decision without provider recall.

`Runtime::inspect_mcp_evidence` selects one exact tool, prompt, resource, or template
run/invocation/attempt and reports its `McpEvidenceKind`. It checks
current node/evidence authorization and all source/response/decision/outcome labels.
It returns original failure and normalization, raw digest and authorized bytes,
truncation flags, reconstructed envelope, separate reconstruction failure, response
receipt, and effect uncertainty. It performs no dispatch, retry, publication, or adoption.
Tool normalization is available only for tool attempts. Prompt/resource inspection
returns the same protected raw-byte/digest/delivery evidence and never reads tool
normalization checkpoints or invokes a reconstruction engine.

`McpEvidencePayload` distinguishes result objects from correlated JSON-RPC error
objects. Protected error bytes retain the remote code, message, and optional data,
including `-32042` URL-required interactions. Errors never enter tool normalization
or become successful graph artifacts. Runtime recovery independently rechecks their
digest, shape, and delivery association; inspection and recovery do not retry them.

The positive byte ceiling cannot exceed the runtime codec ceiling. A truncated raw
prefix is not a complete result to parse or salvage. The allowance bounds returned
representations, not exact heap usage. Debug output omits values; applications must
preserve returned protection when handling evidence. Raw content is excluded from
ordinary public history and graph-visible errors. A tool-error envelope may contain
useful diagnostic JSON while the invocation remains failed with no successful output.

## Propose and adopt inferred schemas

`Runtime::infer_mcp_results` takes explicit `McpInferenceSelection` values and an
`InferencePolicy`. Successful-result sampling requires complete unambiguously selected
JSON from genuine accepted MCP node outputs. Runtime checks producer, port, native
type, executable, binding, and current access. Ordinary JSON claiming to be a tool
response cannot supply producer authority.

The returned `ProtectedInferenceRevision` includes immutable schema/evidence,
unioned labels, exact source selections, and separate effect uncertainty. Reports
contain structural observations and identities rather than private sample values.
`InferenceSampleKind::ToolError` deliberately selects a separate error-body path;
successful sampling never silently mixes errors. Inference reads recorded samples
and makes no MCP calls. A proposal or mismatch cannot replay its source action.

Adopt a revision through trusted compiler `mcp_result_policies` and
`mcp_result_documents`, recompile, then independently select the same
`McpPolicy.result_policy` in the runtime host. Observe records deviations without
granting refined types; Enforce requires selected data to match. Mandatory application
checks always apply. Existing executables and recovered attempts keep their frozen
revision. `InferenceRevision::difference` reports bounded changed paths. Open-object
schemas often already accept additional fields; new observations do not mandate adoption.

## Discover catalogs

The maintained helper captures actual **stdio tool, prompt, resource, and resource-
template descriptors** from the categories advertised by the server:

```console
node tools/discover-mcp.mjs HOST.json NEW_CATALOGS.json
```

The host file contains `transport: "stdio"`, `alias`, `deployment_id`, `program`,
`arguments`, and an optional `environment` overlay. This helper inherits its parent
environment before applying that overlay; it differs from the live-session runner's
explicit environment selection. Optional `discovery_limits` can tighten the defaults:
128 pages per inventory, 4096 items across all inventories, 2 MiB per frame, 16 MiB
total stdout bytes, and one 30-second absolute discovery deadline. The fields are
`pages`, `items`, `frame_bytes`, `total_bytes`, and `timeout_ms`; all must be positive
integers no larger than their defaults. Shutdown has a separate two-second bound.

The output contains the three compiler catalog objects, including resource
templates, plus a `.discovery.json` report distinguishing unsupported inventories
from completed traversals. Empty opaque cursors are preserved; repeated cursors,
budget exhaustion, and malformed pages fail before publishing partial catalogs.
Output overwrite is refused. Discovery never calls tools, retrieves prompt messages,
reads resources, expands templates, or adopts contracts. HTTP discovery uses the
native session APIs rather than this stdio helper. Native compiler/runtime admission
independently validates captured catalogs and live data.

## Run a durable live session

The generic runner accepts actual catalogs and host routing, compiles ordinary
bounded graphs, grants each exact tool/argument selection, and retains evidence:

```console
cargo run --example mcp_live_session --locked -- HOST.json CATALOGS.json NEW_OUTPUT_DIR
```

For a stdio deployment, the host configuration has this shape; replace all example
identities, paths, selected names, and environment with the actual catalog/deployment:

```json
{
  "alias": "service",
  "deployment_id": "local-service",
  "transport": "stdio",
  "program": "/absolute/path/to/server",
  "arguments": [],
  "environment": {},
  "selected_tools": ["lookup"],
  "max_calls": 10,
  "attempt_timeout_ms": 30000
}
```

For HTTP, use `transport: "streamable_http"`, `url`, and optional `headers` or
`header_environment` mappings. `inherit_environment` lists explicitly inherited
stdio variables. Select 1–128 actual tools, a call budget of 1–500, and timeout
1,000–600,000 ms. The output directory must be new. After `ready`, send one JSON
object per line, for example `{"tool":"lookup","arguments":{"query":"example"}}`.
Wait for each result before the next command; end with `{"stop":true}`.

Inspect saved evidence without an adapter or provider credentials:

```console
cargo run --example mcp_live_session --locked -- --inspect OUTPUT_DIR
```

This is local, no-dispatch inspection; it opens the normal write-capable SQLite
store and is not a filesystem-read-only utility. Its offline tests check continuity,
access control, evidence tampering, and absence of provider calls.

### Run an authored capability sequence

The `mcp_capability_spike` example runs a complete authored graph with an in-memory
store. Its browser and sandbox graphs are active regression fixtures:

```console
cargo run --example mcp_capability_spike --locked -- HOST.json
```

This runner uses a different host format from the durable session runner:

```json
{
  "source": "browser.htlk",
  "catalogs": "live.catalogs.json",
  "inputs": { "marker": "unique-live-page-marker" },
  "endpoints": {
    "browser-deployment": {
      "transport": "streamable_http",
      "url": "http://127.0.0.1:8931/mcp",
      "headers": {}
    }
  }
}
```

Source/catalog paths resolve relative to the host JSON. Deployment keys must match
actual catalog identities; adapt the graph's tool names, arguments, projections,
and postconditions to those descriptors. The demonstration policy is permissive;
supply your intended operation policy in an application host. Graph operations must
explicitly destroy test-owned remote objects; transport teardown is separate cleanup.

## Dated interoperability findings

The [2026-11-06 sibling campaign](mcp-interoperability-2026-11-06.md) exercises the
current bidirectional client against all five sibling projects over their supported
stdio/HTTP transports, including real browser, search, model, context, and sandbox
operations. It records the refreshed OpenAI results, actual Chrome roots/list-change
delivery, independent durable evidence and cleanup, upstream failures, and the bounded
startup-timeout example improvement. Unsupported services are not counted as passes.

The 2026-10-29 campaign exercised 75 advertised tools; 73 had successful normal
cases. These observations are version-specific samples, not exhaustive support claims:

| Server revision | Normal successes / tools |
|---|---|
| Chrome `ada7963246c0c91eae185125a57aecd13cbe0376` | 32 / 32 |
| Brave `af08e0d48e812a3966ea2211854db5f0e8eee32d` | 6 / 8 |
| llm `a9b6d2fe3fed686dd2ca2fb67591613f0cf44cb8` | 4 / 4 |
| context `7ecad1fe51de6d57a35f1c3792fa16e2b96ddcb3` | 23 / 23 |
| sandbox `8c649a415ba7be6aa6e15a4dc51e56dc5990eae0` | 8 / 8 |

Brave Place Search violated its advertised output schema; a permissive sibling
parser did not override that contract. Normal summary was blocked because searches
provided no summary key; the invalid-key error case was not normal summary acceptance.
Browser filesystem/recording cases needed explicit supported isolated configuration.
Sandbox restart lost lookup state, and a destroyed receipt did not establish physical
container removal. Async receipts likewise did not prove completed processing.

Reusable lessons: assert actual terminal outputs, verify test-owned external cleanup,
and distinguish local budget reservations, observed usage, upstream requests, and
invoices. Transport teardown does not destroy remote application objects. Full
campaign traces remain in Git history; maintained local peers provide independent
stdio/HTTP JSON/SSE and recovery regressions without credentials.
