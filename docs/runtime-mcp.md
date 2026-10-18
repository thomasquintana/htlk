# MCP execution and exact-action approvals

Authority: runtime §§7, 11, 13–14. Configure an `Arc<dyn McpAdapter>` with
`Runtime::with_mcp` and launch `ExecutionEngine` to execute compiler-pinned calls,
resource reads, URI-template reads and prompt retrievals.

## Trusted transport configuration

`NativeMcpAdapter` routes deployment IDs to operator-supplied `McpEndpoint::Stdio`
or `McpEndpoint::Http` configurations. Executables contain compound server identity
and operation/schema pins; credentials and connection locations remain host data.
Every session negotiates MCP 2025-11-25, checks implementation name/version and the
operation capability, and follows bounded descriptor pagination. `McpConnection`
owns the live checked session: no renegotiation remains after the final policy/
approval decision and intent. Failed negotiation cannot turn into a tool send.
HTTP redirects and automatic decompression are disabled. HTTP JSON/SSE and newline-
framed stdio responses are bounded before strict JSON parsing, preserving duplicate-
key, depth, Unicode and numeric-token rejection. Complete `Limits` reach the parser;
RPC matching and session/content-type checks precede completion claims. Native
clients answer reverse `ping` and reject unadvertised sampling instead of invoking
a model. Native processes are killed on drop; HTTP cleanup is best-effort.

`McpAdapter` is a trusted integration contract. Its connection resolver receives the
actual run principal. Runtime independently checks descriptor
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
before opening a connection. `mcp_policy_for_request` supports trusted argument-specific
approval, safety and protection decisions. Approval opens without holding a live
session; policy/disposition are rechecked after connection and discovery. The transaction that
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
replay-safe operations. Migration 5 adds a separate,
non-authoritative late-response inbox: fenced or terminal results cannot publish outputs.

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

## Verification and remaining audit

Compiler-to-actor tests execute decision-brief acceptance at reviews one/two/three
(4/6/8 calls), loop-limit fallback (8 calls), schema-failure retries, zero/exhausted
call budgets, unsafe takeover with late results, approval renewal/history/denial,
mixed resource snapshots, returned-URI authorization, templates and prompt messages.
Actual HTTP JSON/SSE and stdio peers exercise negotiation, pagination, exact requests,
raw duplicate keys, unsafe integer tokens, byte/depth bounds, cancellation, correlated
errors, reverse ping and refusal of sampling. Delivery tests cover known-unsent retry,
ambiguous protocol capacity, ignoring-adapter/shutdown grace, deterministic drift,
pre-connection parameter scope and invocation-stable dedup keys.

The roadmap tracks remaining source metadata, richer inspection, host examples and
final conformance/release checks. [Recovery](runtime-recovery.md),
[extension](runtime-extension.md) and [retention](runtime-retention.md) are implemented.
