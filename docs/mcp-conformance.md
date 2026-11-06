# MCP client conformance inventory

Target protocol: **2025-11-25**. Requirements are defined by the
[client specification](../specs/mcp-client-spec.md). “Pending” means implementation
and qualification are required; it is not a supported feature claim. Native client
coverage and custom host implementations are distinct.

| Feature / methods | Gate and direction | Native status | Evidence / qualification |
| --- | --- | --- | --- |
| initialize, initialized, version, server identity | Base; client to server | Implemented, fixed 2025-11-25 | [Transport tests](../crates/htlk-runtime/tests/mcp_transport.rs) |
| tools/list, tools/call | Server tools | Selected descriptor + calls implemented | [MCP E2E](../tests/deep_e2e.rs) |
| prompts/list, prompts/get | Server prompts | Selected descriptor + retrieval implemented | MCP E2E |
| resources/list, resources/read | Server resources | Selected descriptor + read implemented | MCP E2E |
| resources/templates/list, expanded resources/read | Server resources | Selected template + read implemented | MCP E2E |
| Complete four-category catalog API | Server category capabilities | Bounded page/enumeration APIs implemented; stdio discovery utility covers all advertised inventories | [Service tests](../crates/htlk-runtime/tests/mcp_client_services.rs), [discovery tests](../tools/discover-mcp.test.mjs): incomplete prefixes, opaque/cyclic cursors, page/item/byte/time bounds, zero content/effect calls |
| Host services and derived client capabilities | Installed service + host policy | Host-owned services implemented; run callback bridge pending | Invalid combinations cannot initialize; [callback tests](../crates/htlk-runtime/tests/mcp_callbacks.rs) |
| Host-owned sessions | Authenticated owner/generation | Implemented with optional durable ownership/epochs through `McpClientPersistence` | [Client session tests](../crates/htlk-runtime/tests/mcp_client_sessions.rs), [interaction store tests](../crates/htlk-runtime/tests/mcp_interaction_store.rs); restart identity, expiry, isolation, explicit reopen |
| Idle stdio reception | Transport | Implemented in the shared native client and graph adapter dispatcher | Client session tests: notification observed before next foreground request |
| HTTP POST JSON/SSE | Transport | Implemented for outstanding RPCs | Transport tests, MCP E2E |
| HTTP GET SSE, resumability, retry, expiry/new epoch | Optional server stream; transport | Implemented in `NativeMcpClient`; durable replay tracking pending | Client session tests: 405 fallback, fragmented/priming events, no effectful POST replay |
| ping | Bidirectional base utility | Reverse ping implemented during exchanges | Transport tests |
| notifications/cancelled | Bidirectional base utility | Outgoing cancellation implemented | MCP E2E; incoming callback cancellation pending |
| sampling/createMessage | Client sampling | Host-owned execution implemented; run attribution/ancestor accounting pending | Callback tests: nested model work, no session-lock deadlock, durable result replay |
| Tool-enabled sampling | Client sampling.tools | Pending | Tool choice, balanced tool results, no automatic client tool dispatch |
| Context sampling | Client sampling.context; soft-deprecated | Pending assessment | Explicit authorized bounded context service required |
| elicitation/create, form | Client elicitation.form (empty elicitation also means form) | Host-owned addressed replies implemented; run callback bridge pending | Restricted schema, omitted mode, validated addressed replies |
| elicitation/create, URL; -32042 | Client elicitation.url | Host-owned URL requests implemented; -32042 interaction routing pending | Consent, no prefetch or credential transit; error data retained as evidence |
| notifications/elicitation/complete | Server to client; known elicitation ID | Pending | Separate consent/completion, ignore unknown/completed IDs |
| resources/subscribe, resources/unsubscribe | Server resources.subscribe | Bounded RPCs and durable host consumer intent/lifecycle implemented | [Subscription tests](../crates/htlk-runtime/tests/mcp_subscriptions.rs): first/last consumers, competing connections, uncertain claims, fenced ACKs, explicit restoration/gaps; service tests cover unsubscribe update races |
| notifications/resources/updated | Server to client | Generic protected idle event delivery implemented; dedicated typed event pending | Service/subscription tests: idle/update races cause zero resource reads or effect calls |
| tools/prompts/resources list_changed | Corresponding server listChanged | Generic protected delivery implemented; typed/gated invalidation pending | Service tests: no automatic catalog refresh or adoption; templates share resource catalog invalidation |
| roots/list, notifications/roots/list_changed | Client roots / roots.listChanged | Pending | Host source, URI constraints, changes |
| completion/complete | Server completions | Capability-gated checked request/result API implemented | Service tests: prompt/template references, contextual arguments, bounded response/time, 100-value limit, valid totals, protected errors |
| logging/setLevel, notifications/message | Server logging | Capability-gated level API and generic protected log delivery implemented; typed log event pending | Service tests: all eight levels, malformed ACKs, unsupported capability, idle log delivery |
| notifications/progress | Request progressToken; bidirectional | Pending | Token correlation, absolute deadline preserved |
| Task-augmented tools/call | Server tasks.requests.tools.call + descriptor execution | Pending | Required/optional/forbidden, result vs acceptance, no duplicate create |
| Task-augmented sampling/elicitation | Corresponding client tasks.requests | Pending | Durable receiver lifecycle and parent attribution |
| tasks/get, tasks/result, tasks/list, tasks/cancel | Negotiated receiver task support; list/cancel separately gated | Pending | Poll without notifications, TTL, isolation, terminal states |
| notifications/tasks/status and related-task metadata | Experimental task utility | Pending | Input-required callbacks and stream independence |
| Static HTTP credentials, stdio environment | Explicit host deployment | Implemented | Transport tests |
| MCP OAuth discovery, PKCE, resource indicators, refresh/scope | Optional HTTP authorization profile | Implemented through `client::auth` with host navigation/entropy/persistence | [Authentication tests](../crates/htlk-runtime/tests/mcp_auth.rs): independent local resource/AS peers, OAuth/OIDC fallbacks, PKCE rejection, callback validation, refresh rotation/interruption, scope step-up and bounds; actual `NativeMcpClient` rotates credentials across POST/control/GET/resumption/DELETE with no automatic operation replay |
| Native credential acquisition and HTTP challenges | Exact compound server/resource/principal binding | `with_credential_provider` and bounded latest-value `authentication_concerns` implemented | Authentication tests: pre-send failures, conflicting static headers, 401/403 and malformed challenges including failed initialization, idle GET and DELETE; sequence gaps expose overwritten concerns |
| Preregistration | Public `none`, confidential `client_secret_basic` / `client_secret_post` | Implemented | Authentication tests: independent authorization-code and refresh exchanges for all three methods |
| Client ID Metadata Documents | Recommended optional client registration approach | Assessed and deferred | Requires host-published conforming HTTPS client metadata and a separate draft-version contract; no implicit URL-client registration is claimed |
| Dynamic client registration | Optional RFC 7591 | Assessed and deferred | Remote client creation/registration-secret lifecycle requires additional host policy; pre-registration is the supported profile |
| Independently versioned authorization extensions | Optional additive profiles | Assessed and deferred | No extension negotiation, grant or conformance claim is implied by base OAuth support |
| Durable callback accounting, addressed host replies | Runtime authority, deadlines and budgets | Pending | Crash points, duplicate replies, fencing, one-worker liveness |
| Protected raw evidence | Current host authorization | Tool/prompt/resource/template attempts implemented; client interactions pending | [Tool evidence tests](../tests/runtime_mcp_results.rs), [non-tool evidence tests](../tests/runtime_mcp_data.rs) |

Deprecated HTTP+SSE transport and protocol versions other than 2025-11-25 are outside
this version profile. SSE above means Streamable HTTP. Independently versioned MCP
authorization extensions require separate assessment; they are not implied by
support for base authorization.

Every newly implemented row needs public API documentation, finite byte/work/time
bounds, cancellation and ownership semantics, and independent peer or durable-store
evidence. Handler installation alone does not satisfy a row. See the
[testing guide](testing.md) for required validation and existing fault campaigns.
