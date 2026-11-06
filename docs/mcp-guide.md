# Using MCP with HTLK

MCP connects a workflow to external tools, resources, and prompt data. Configure an `Arc<dyn McpAdapter>` with
`Runtime::with_mcp` and launch `ExecutionEngine` to execute compiler-pinned calls,
resource reads, URI-template reads and prompt retrievals.

Every tool produces one `McpToolResult` envelope under a frozen host-selected
policy. This guide covers transport integration, content helpers, evidence, and
schema proposals. Exact result/extraction/inference rules are specified in the
[MCP specification](../specs/mcp-spec.md); the [runtime guide](runtime-guide.md)
covers registration and host lifecycle.

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
Logging and resource/catalog notifications arrive through the bounded generic
`McpClientEvent` surface. Dedicated typed resource/catalog/log events, outgoing roots
changes, and progress-token correlation remain pending in the
[conformance inventory](mcp-conformance.md).

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
