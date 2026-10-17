# MCP execution and exact-action approvals

Authority: runtime §§7, 11, 13–14. Configure an `Arc<dyn McpAdapter>` with
`Runtime::with_mcp` and launch `ExecutionEngine` to execute compiler-pinned calls,
resource reads, URI-template reads and prompt retrievals.

## Trusted transport configuration

`NativeMcpAdapter` routes deployment IDs to operator-supplied `McpEndpoint::Stdio`
or `McpEndpoint::Http` configurations. Executables contain compound server identity
and operation/schema pins; credentials and connection locations remain host data.
Every session negotiates MCP 2025-11-25 and checks implementation name/version.
Descriptor discovery follows bounded pagination, and invocation rechecks the exact
advertised descriptor. HTTP redirects are disabled. HTTP JSON/SSE and newline-framed
stdio responses are byte-bounded before strict JSON parsing, preserving duplicate-key,
depth, Unicode and numeric-token rejection. Native process sessions are killed on drop.

`McpAdapter` is a trusted integration contract. Runtime independently checks descriptor
digests and result values and imposes finite discovery/attempt timeouts. An adapter
returns raw result-object JSON; it cannot substitute text for structured tool output.
Resource results preserve text/decoded-byte order and separately authorize every
returned URI, including URIs different from the requested URI. Prompt messages remain
untrusted data and never trigger model or tool execution.

## Durable dispatch and retry

An admitted invocation freezes arguments and its input hash. Each one-based attempt
persists the exact method, parameters, encoded request, requested URI, timeout,
deduplication mechanism and ownership fence in `prepared`. Dispatch policy and exact
approval are checked after asynchronous descriptor discovery. The transaction that
records `possibly_sent` also spends a call and reserves concurrency in the run,
invocation and every containing scope/wrapper. Only then can the adapter send.

Raw responses or classified failures are persisted before local validation and atomic
output acceptance. Result protection is saved with the response; later policy changes
cannot remove those labels. Definitive replies release concurrency, never spent calls.
Ambiguous delivery retains outstanding reservations. Migration 5 adds a separate,
non-authoritative late-response inbox: fenced or terminal results cannot publish outputs.

Retries require the authored count, exact failure code, explicit backoff, remaining
deadline/budget, current trusted replay safety and approval. Every attempt reuses the
frozen arguments. An unsafe `possibly_sent` takeover fails `E_EFFECT_UNCERTAIN`.
Recorded responses resume local validation without another external call.

Independent replay safety defaults to false in `RuntimePolicy::mcp_policy`. A trusted
adapter may instead verify an actual enforced `McpDeduplication` contract. Protocol
keys use the invocation-stable effect digest; authored argument keys must already be
present and unchanged, with scope/uniqueness verified by the adapter. Native HTTP
`with_protocol_idempotency` accepts exact compound binding digests only when the
operator has verified endpoint enforcement of `Idempotency-Key`. Tool annotations
never confer replay safety or deduplication support.

Hard external token/cost budgets remain rejected before run admission because the
native adapter does not advertise trusted hard metering/reservation enforcement.

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
raw duplicate keys, unsafe integer tokens and oversized responses.

The complete runtime roadmap still includes exhaustive crash/reopen and cancellation
race coverage, additive extension, retention, richer inspection and final conformance.
