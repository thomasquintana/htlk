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

Resource updates and catalog changes become protected host events with stable
identity and ownership context. They trigger no resource read, model call, catalog
adoption, executable change or workflow mutation. Hosts own notification presentation
and archival. Event overflow and connection gaps are observable.

## Protocol services

- Discovery enumerates tools, prompts, resources and templates with page, item,
  byte and time limits, opaque cursors and cycle detection. Partial enumeration
  is explicit and never masquerades as a complete catalog. Adoption remains explicit.
- Sampling supports declared message modalities, model preferences, tool-choice and
  balanced tool-use/result histories. Server-provided tools are returned as model
  tool-use requests for the server; the client does not automatically execute them.
  Sequential requests share cumulative allowances. Hard token/cost enforcement
  requires a trusted host reservation/metering implementation.
- Form elicitation supports the pinned restricted schema, including enum/multiselect
  forms, validates accepted replies, and distinguishes accept/decline/cancel. URL
  elicitation displays the full server URL for consent without prefetch. Acceptance
  and optional completion are separate facts. Unknown/completed completion IDs are
  ignored. Error -32042 retains its required interactions. Completion cannot bypass
  ordinary retry authorization and budgets.
- Roots are host-provided protocol boundaries, not an OS sandbox. Completion,
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
