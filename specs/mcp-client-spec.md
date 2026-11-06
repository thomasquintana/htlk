# MCP client specification

**Status:** Draft 0.1. This contract targets MCP **2025-11-25**. The
[conformance inventory](../docs/mcp-conformance.md) distinguishes implemented
behavior from requirements awaiting implementation. A requirement here is not a
claim that its implementation has passed qualification.

## Authority and boundaries

The versioned MCP schema and protocol define wire meaning. This specification
defines HTLK's host integration, ownership, bounded execution, and evidence.
The [runtime specification](runtime-spec.md) retains authority over graph
admission, effect dispatch, budgets, protection, and recovery. The
[MCP result specification](mcp-spec.md) defines existing result interpretation.

Tool calls, prompt retrievals, resource reads, and expanded template reads retain
their distinct operations and native result types. Server descriptions, callbacks,
notifications, and catalog changes grant no workflow mutation or execution authority.

## Configuration and negotiation

Client configuration is validated before transport startup. Advertised client
capabilities derive from installed services and supported protocol mechanisms.
Sampling tools/context, elicitation form/URL, roots changes, and each task request
category are independently enabled. Configuration cannot advertise a service
without its handler, bounds, and applicable persistence support.

Negotiated capabilities remain fixed for a session epoch. Policy revocation may
deny subsequent work; adding capabilities requires initialization of a new epoch.
The client proposes 2025-11-25 and rejects other negotiated versions. Subsequent
HTTP requests use the accepted protocol version. Unsupported optional capabilities
do not invalidate otherwise valid base-protocol sessions.

## Ownership

Session ownership is either:

- **Run:** authenticated principal, run ID, durable ownership fence, and execution
  cancellation lifetime.
- **Host:** authenticated principal, opaque host-session ID, durable ownership
  generation, and explicit close/expiry lifetime.

Every session also has a compound server identity and an epoch. Different owners
never share stateful transport resources. Host sessions persist interaction facts
and subscription intent in their own owner scope, without synthetic graph runs.
Ownership generation checks apply transactionally, independently of local actors.
Transport handles are process-local. Durable records are not proof that a remote
session or stateful handle survived a process restart.

## Dispatch and attribution

Reception continues during idle periods and while a handler awaits. Transport
readers and session locks cannot be held across a host callback. Each direction
has its own request-ID namespace; integer and string IDs remain distinct. Outgoing
IDs are never reused within the same epoch. Notifications receive no RPC response.

Ingress limits apply before JSON normalization, including duplicate keys, nesting,
numeric tokens, framing, and bytes. Queues are bounded by bytes and items. Control
responses, cancellation, and shutdown remain serviceable under application-event
or callback saturation. Overflow has an explicit disposition, never silent loss
of an accepted effectful request. Every operation has an absolute deadline; progress
does not reset that bound. Cleanup and noncooperative callback cancellation are finite.

Interaction context identifies owner/generation, server, epoch, incoming request
ID, method, payload digest, and durable interaction ID. Established origins include
run/invocation/attempt, outbound request ID, parent interaction, and related task.
Attribution records its source. There is no universal MCP parent-request ID;
transport association, task metadata, and serialized foreground association remain
distinct evidence. Unrelated idle events carry session-only context. Ambiguous
callbacks cannot spend a guessed run's budget. Host-only work requires its own
explicit authority and budget.

The scheduler must admit required bounded child work without waiting for a parent
to release a worker or session lock that can only be released after the child ends.
Rejecting inadmissible nesting is preferable to unbounded waiting. Child deadlines
and budgets cannot exceed their admitted ancestor allowances.

## Durable interactions

Records distinguish received, admitted, work-intent, work-result, reply-recorded,
reply-send-attempted, and observed transport-acceptance facts. Terminal cancellation,
expiry, and fencing prohibit publication but preserve late protected evidence.
Host completion and reply bytes are recorded before transmission. HTTP 202 confirms
transport acceptance, not server application consumption; a stdio write proves less.

Addressed replies bind interaction identity, exact request digest, authenticated
responder, ownership generation, deadline, and idempotency key. Equal submissions
return the recorded receipt; conflicting submissions fail. Reply acceptance is a
transactional authority check, not merely an in-process channel operation. Form,
URL, approval, and graph input replies have distinct purposes and authorization.

Known redelivery uses the saved result/reply without executing the handler again.
Request identity comprises owner, epoch, direction and request ID; payload mismatch
under an existing identity is a conflict. SSE event identity can additionally
identify replay. New IDs or epochs do not prove logical equivalence. Replay
retention is bounded; eviction cannot silently authorize repeating old work.

Work intent without a saved completion retains uncertainty. Recovery can reconcile
against trusted provider idempotency/status evidence, but cannot blindly recall a
model. Request IDs, outboxes, and transport acknowledgements do not establish
global exactly-once external execution. Ownership transfer preserves stable work
identity while changing authority. Report unknown usage as unknown, not zero.

The native host-work wrapper MUST bind its stable claim to one admitted interaction
and its existing call/concurrency reservation. Cloning a handle MUST NOT grant a
second operation. Native child RPC dispatch uses the actual admitting session,
rechecks durable owner/interaction/attempt authority, and commits a separate single-use
native intent. It MUST NOT create an independently budgeted workflow. Child callbacks
on associated HTTP response streams inherit the actual parent interaction, protection,
depth, deadline and cancellation; these facts are retained with their interaction.
Ambiguous stdio nesting grants no inferred parent authority.

The I/O owner MUST retain a bounded exact native result/error and delivery envelope
before returning it to the host mapper, independently of the mapper's waiter. The
wrapper MUST save the mapped sampling outcome before returning to the callback handler.
Inspection distinguishes host routing, native outcome, and the callback reply. A known
saved outcome is reused without constructing another host operation, including after
callback interruption. Unknown provider completion requires explicit reconciliation.
The wrapper does not attach arbitrary `Runtime::start_run` calls to ancestor budgets;
its supported native child operation is the single accounted RPC.

Underlying host/model work delivery is distinct from callback response delivery.
Recording or acknowledging a JSON-RPC timeout error does not establish that a model
operation ended. Its unresolved reservation remains charged until explicit trusted
completion/no-start reconciliation. Reconciliation cannot replace the callback reply
already recorded or sent. Known pre-dispatch refusals spend no host-work call.

Protected evidence covers tools, prompts, resources, templates, interactions and
structured protocol errors. Inspection checks current authority and unioned labels,
returns bounded original bytes/digests and explicit truncation, and never dispatches.
Tool normalization remains a specialized view rather than a universal result type.

## Transport loss and events

Streamable HTTP supports POST JSON/SSE and optional GET SSE. GET 405 reports lack
of that optional stream. Parsers retain bounded incomplete frames, not entire
stream history. Resumption uses GET with the stream's Last-Event-ID, honors retry,
and handles empty priming events. It never repeats an effectful POST as a substitute
for stream resumption.

Session-expiry 404 invalidates old handles. New initialization produces a new epoch;
it neither resumes old remote handles nor reissues the old operation. Subscription
restoration requires explicit host action or configured authorization and emits a
gap/new-epoch event. No delivery guarantee extends over unsupported replay or
expired retention. Closing a frontend differs from stopping the transport owner.

Absolute request deadlines include local queue time. Deadline expiry MUST attempt
a correlated cancellation hint while retaining a bounded completion grace; hints
do not extend the operation's acceptance deadline. Ambiguous run-owned exchanges
MUST poison their session on the I/O owner even if the caller stopped awaiting the
reply. Observed outcomes with abandoned runtime waiters are protected late evidence,
not authority to publish a result into an already expired attempt.

Resource updates and catalog changes become protected host events with stable
identity and ownership context. They trigger no resource read, model call, catalog
adoption, executable change or workflow mutation. Hosts own notification presentation
and archival. Event overflow and connection gaps are observable.

Recognized resource/catalog/logging notifications are pinned-schema validated and
capability-gated before typed delivery. Unsupported categories/sub-capabilities have
an explicit ignored disposition; malformed supported messages fail protocol admission.
Resource updates distinguish unknown/unsolicited URIs, subscription transitions,
acknowledged state and uncertainty within the source epoch. Receipt performs no read
or adoption even when a URI is subscribed. HTTP stream ordering can race RPC ACKs.

Progress tokens preserve integer/string identity and are single-use for outgoing
requests within an epoch. Correlation uses the actual registered request/token, never
an unrelated active operation. Progress is finite and nonnegative, increases between
accepted updates, and cannot exceed a known total; totals cannot decrease and omitted
totals preserve prior knowledge. Unknown, expired, wrong-stream and non-monotonic
updates are explicitly ignored. Accepted tasks can retain their original creation
token until terminal observation, TTL, cancellation or the original deadline. This
retention grants no task-adoption or completion authority. Host callback reporters
bind the actual admitted request and expire at handler completion or cancellation.
Outgoing callback progress rechecks current disclosure authorization and unions its
classification with the admitted request's protection before transmission.
Neither incoming nor outgoing progress changes an absolute deadline.

## Protocol services

- Discovery enumerates tools, prompts, resources and templates with page, item,
  byte and time limits, opaque cursors and cycle detection. Partial enumeration
  is explicit and never masquerades as a complete catalog. Adoption remains explicit.
- Sampling supports declared message modalities, model preferences, tool-choice and
  balanced tool-use/result histories. Server-provided tools are returned as model
  tool-use requests for the server; the client does not automatically execute them.
  Sequential requests share cumulative allowances. Hard token/cost enforcement
  requires a trusted host reservation/metering implementation.
  Installing both tools and context requires an explicit combined host service;
  combined requests MUST NOT silently select a partial implementation. Tool names
  are unique and declared schemas are prepared offline before model dispatch.
- Form elicitation supports the pinned restricted schema, including enum/multiselect
  forms, validates accepted replies, and distinguishes accept/decline/cancel. URL
  elicitation displays the full server URL for consent without prefetch. Acceptance
  and optional completion are separate facts. Unknown/completed completion IDs are
  ignored. Error -32042 retains its required interactions. Completion cannot bypass
  ordinary retry authorization and budgets.
- Roots are host-provided protocol boundaries with pinned `file://` URI rules, not
  an OS sandbox. Explicit roots-change notifications require the installed provider
  and advertised change service and do not invoke the provider or read files. Completion,
  logging and progress are capability-aware, bounded services. Progress correlates
  by its token and does not imply completion.
- Tasks are experimental and independently negotiated by direction/request category.
  Tool task-required/optional/forbidden declarations are respected. Task creation
  is distinct from final operation results. Polling works without notifications;
  input-required, related-task metadata, TTL, cancellation, and terminal-state rules
  follow the pinned protocol. An interrupted create response does not authorize a
  second task creation. When augmentation is unsupported for an otherwise supported
  method, task metadata is ignored as required by MCP.
- MCP-server OAuth uses the versioned authorization requirements: protected-resource
  and authorization-server discovery, PKCE, resource indicators, principal-scoped
  credentials, bounded challenge/refresh and supported registration mechanisms.
  Static headers or a credential hook alone do not establish OAuth conformance.
  Server-managed third-party credentials obtained through URL elicitation do not
  transit the client. Connection credentials are applied on every relevant HTTP
  request and excluded from ordinary evidence and executable meaning.

## Native experimental tasks

Task dispatch MUST distinguish creation intent, accepted identity, status observations,
and the underlying result/error. With persistence configured, the native client MUST
commit a logical creation intent before send and register a valid accepted identity
before delivering that RPC response or processing later messages. Reusing that owner/
server-scoped key MUST NOT send another creation, including after a lost response or
transport-owner restart. Protected original acceptance, creation error and deferred
result/error bytes MUST remain separate and integrity checked.

Native graph task dispatch requires explicit `McpGraphTaskPolicy` plus the runtime's
actual persistence/clock. Descriptor and policy selection MUST freeze before dispatch;
authored arguments MUST NOT change. A creation response MUST NOT be normalized as tool
output. Only the underlying deferred tool result enters ordinary tool-result admission.
Polling/result retrieval share the original absolute attempt deadline and response
bounds. Cancellation MUST use `tasks/cancel`, not only RPC wait cancellation.

Related-task callbacks MUST resolve against the retained owner/server/session/generation
registry, including when the original creating RPC is no longer pending. Its recorded
run/invocation/attempt, deadline and protection MUST be used; unrelated pending work MUST
NOT supply attribution. Run admission MUST independently recheck the actual outstanding
attempt and ancestor accounting. Unknown, expired or terminal associations grant no work
authority. New epochs do not automatically inherit old task association authority.

The installed client receiver MUST commit its own opaque identity against an admitted
interaction before execution intent and CreateTaskResult delivery. It MUST derive its
capabilities from installed handlers, persistence, authorization and finite quotas.
Unsupported augmentation MUST be removed from semantic validation, even if malformed,
while original request bytes remain protected evidence. Task controls MUST use bounded
capacity separate from execution workers and the reader; cancellation capacity MUST
remain available while result waits occupy other control slots.
The native wire reserves 64 KiB for cancellation, with a 4 KiB cancel-request and
8 KiB cancel-response ceiling independently of ordinary task-control saturation.

Receiver cancellation MUST atomically commit terminal task state and cancellation of
the wrapped interaction before responding, without releasing uncertain work reservations
or refunding calls. An identical cancellation RPC replays its committed receipt; a new
RPC against a terminal task returns -32602. Late completion cannot replace that result.
The receiver MUST return saved underlying payloads with matching related-task metadata,
and MUST retain original interaction reply bytes separately. Initial acceptance delivery
MUST NOT be recorded as delivery of the underlying reply. Result wait cancellation does
not itself cancel the task. Both directions MUST work without status notifications.

Finite TTL controls retrievability, not proof of work cancellation. Receiver IDs and
bounded result-capacity reservations remain quota-counted tombstones after expiry, so
eviction cannot authorize duplicate work. Remote identities persist for protected
inspection, but recovery does not dispatch, recreate work, or silently adopt a new epoch.
Explicit cross-epoch remote-task adoption/reconciliation remains a host integration
responsibility; no automatic graph task recovery is claimed.

## Native URL elicitation lifecycle

The native client MUST bind completion identities to the authenticated owner,
compound server and durable session epoch. Admitted callback URL identities MUST
be registered before exposing their event or dispatching the handler. Callback
consent MUST derive from the saved callback reply; acceptance MUST NOT establish
external completion. Known completion notifications record only the first server
observation. Unknown and already-completed IDs MUST be ignored without forwarding
their raw parameters or inspecting unrelated protected URL values.

For error `-32042`, the native client MUST validate every entry of the required URL
list before atomically retaining its original error bytes and registering its
addressed requirements. Registration MUST precede processing later frames on the
same stream. Complete trailing SSE frames already received in the response chunk
MUST be processed even when they follow the correlated response. Host events MUST
carry opaque local identities rather than the URL, remote elicitation ID or metadata.

URL admission failure MUST be exposed separately from the original correlated RPC
error and delivery classification. It MUST NOT replace that error with a local
success/failure outcome, mutate the original attempt, or retry the operation.
Inspection and consent submission MUST check current purpose-specific grants and
unioned protection. Run attribution MUST use the exact pending operation context
and independently checked persisted attempt; it MUST NOT guess an origin from
unrelated concurrent work. URL consent carries no in-band content. The host owns
secure presentation/navigation and separately authorized manual continuation.
Expiry/closure remain inspectable, and historical receipts grant no new delivery.

## Native HTTP authorization profile

The optional native OAuth profile supports pre-registered clients using `none`,
`client_secret_basic`, or `client_secret_post` token endpoint authentication.
Registration pins the authorization server and exact redirect URI. Client ID
Metadata Documents, RFC 7591 dynamic client registration, and independently
versioned authorization extensions are assessed but not implemented: the former
requires host HTTPS metadata publication and a draft-version contract; dynamic
registration requires remote client/secret lifecycle policy. No fallback silently
registers a client or advertises an unsupported mechanism.

Credential lookup MUST bind the exact compound server digest, trusted owner
principal and resource URL. The transport MUST acquire credentials before each
POST, including control messages and callback replies, each GET/resumption, and
DELETE. Provider-backed destinations are query-free resource URLs. Static
Authorization headers and a credential provider MUST NOT be combined. Acquisition
failure MUST precede send and retain `NotSent` for that HTTP attempt; an interrupted
resumption does not erase an original POST's uncertain delivery. Redirects and
automatic HTTP request retries are disabled.

Protected-resource discovery MUST use the `WWW-Authenticate` resource metadata URL
when provided, otherwise path-specific then root RFC 9728 well-known URLs. AS
discovery MUST attempt OAuth path insertion, OIDC path insertion, then OIDC path
appending for path issuers, and OAuth then OIDC well-known endpoints for root
issuers. Only unavailable (404/405) locations fall through; malformed, mismatched,
untrusted or redirected metadata fails closed. Protected-resource identifiers,
the pre-registered issuer and metadata issuer MUST agree. Metadata MUST advertise
S256, code responses and the configured authentication method. Trust approval is
independent for resource metadata, AS metadata, authorization and token URLs.

The mechanism MUST include the resource indicator in authorization, code exchange
and refresh, use host-supplied independent cryptographic entropy for PKCE/state,
and validate state, the registered callback, any resource/issuer echo and a required
advertised issuer response. Pending authorization is single-use and expires.
Network time, bytes, discovery requests and per-original-operation challenges are
finite. Challenge scopes take precedence over protected-resource scope metadata
without assuming a subset relationship. The host owns navigation, protected token
storage, trusted expiry restoration, refresh serialization and replacement of
rotated secrets. Interrupted exchanges/refreshes are not retried automatically.

Native clients MUST retain a structured protected authentication concern for
credential failures, interrupted acquisition and HTTP 401/403, including malformed
challenges. The bounded latest-value monitor survives failed initialization and
ordinary event overflow; configuration-scoped sequence gaps expose overwritten
concerns. Concerns identify binding, epoch, HTTP method and outgoing RPC ID where
known, without ordinary Debug/error disclosure of secrets or remote diagnostic
text. HTTP challenges are not JSON-RPC replies or proof of no effects. Redacted
`E_POLICY` and conservative delivery evidence remain separate from protected
concerns. A concern or renewed credential MUST NOT automatically replay an MCP
operation. Idle GET authorization failure terminates reception explicitly.

## Generated-schema interpretation

The authoritative MCP TypeScript definition at the
[pinned source revision](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/c4c367f9f58296a7053f5c78a52fd02bfbb56a49/schema/2025-11-25/schema.ts)
defines `ElicitResult.content` scalars as `string | number | boolean`, alongside
string arrays. The generated JSON Schema snapshot incorrectly narrows that scalar
number branch to integer. HTLK's client-service validator admits finite fractional
numbers and validates them against the actual requested form schema. It separately
enforces content presence only for accepted form responses. Other protocol shapes
use the pinned schema plus applicable cross-message rules. Sampling `stopReason`
remains an optional open string, including provider-specific values.

## Compatibility and implementation identity

This green-field revision imposes no database or public API backward-compatibility
requirement. A changed private schema rejects incompatible databases without
deletion or relabeling. Native implementation identities include all covered
sources, independently checked by the fixture validator. Frozen executable policy
is independently admitted at compilation, runtime and transactional boundaries.

## Protocol references

- [Base and lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic)
- [Transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [Sampling](https://modelcontextprotocol.io/specification/2025-11-25/client/sampling)
- [Elicitation](https://modelcontextprotocol.io/specification/2025-11-25/client/elicitation)
- [Tasks](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks)
- [Authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)
