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
| Complete four-category catalog API | Server category capabilities | Pending | All-page completion, bounds, repeated cursor, no effect dispatch |
| Host services and derived client capabilities | Installed service + host policy | Pending | Invalid combinations cannot initialize |
| Host-owned sessions | Authenticated owner/generation | Pending | Owner isolation, persistence, explicit restore |
| Idle stdio reception | Transport | Pending | Notification observed before next foreground request |
| HTTP POST JSON/SSE | Transport | Implemented for outstanding RPCs | Transport tests, MCP E2E |
| HTTP GET SSE, resumability, retry, expiry/new epoch | Optional server stream; transport | Pending | 405 fallback, fragmented/priming events, no effectful POST replay |
| ping | Bidirectional base utility | Reverse ping implemented during exchanges | Transport tests |
| notifications/cancelled | Bidirectional base utility | Outgoing cancellation implemented | MCP E2E; incoming callback cancellation pending |
| sampling/createMessage | Client sampling | Pending | Nested model work, text/image/audio, limits, durable result replay |
| Tool-enabled sampling | Client sampling.tools | Pending | Tool choice, balanced tool results, no automatic client tool dispatch |
| Context sampling | Client sampling.context; soft-deprecated | Pending assessment | Explicit authorized bounded context service required |
| elicitation/create, form | Client elicitation.form (empty elicitation also means form) | Pending | Restricted schema, omitted mode, validated addressed replies |
| elicitation/create, URL; -32042 | Client elicitation.url | Pending | Consent, error interactions, no prefetch or credential transit |
| notifications/elicitation/complete | Server to client; known elicitation ID | Pending | Separate consent/completion, ignore unknown/completed IDs |
| resources/subscribe, resources/unsubscribe | Server resources.subscribe | Pending | Durable intent, lifecycle races, explicit restoration/gaps |
| notifications/resources/updated | Server to client | Pending | Idle delivery; zero automatic content reads |
| tools/prompts/resources list_changed | Corresponding server listChanged | Pending | Typed events; zero automatic catalog adoption |
| roots/list, notifications/roots/list_changed | Client roots / roots.listChanged | Pending | Host source, URI constraints, changes |
| completion/complete | Server completions | Pending | Prompt/template references, contextual arguments, bounds |
| logging/setLevel, notifications/message | Server logging | Pending | Level validation, protected bounded idle delivery |
| notifications/progress | Request progressToken; bidirectional | Pending | Token correlation, absolute deadline preserved |
| Task-augmented tools/call | Server tasks.requests.tools.call + descriptor execution | Pending | Required/optional/forbidden, result vs acceptance, no duplicate create |
| Task-augmented sampling/elicitation | Corresponding client tasks.requests | Pending | Durable receiver lifecycle and parent attribution |
| tasks/get, tasks/result, tasks/list, tasks/cancel | Negotiated receiver task support; list/cancel separately gated | Pending | Poll without notifications, TTL, isolation, terminal states |
| notifications/tasks/status and related-task metadata | Experimental task utility | Pending | Input-required callbacks and stream independence |
| Static HTTP credentials, stdio environment | Explicit host deployment | Implemented | Transport tests |
| MCP OAuth discovery, PKCE, resource indicators, refresh/scope | Optional HTTP authorization profile | Pending | Local protected-server/authorization-server integration |
| Preregistration / Client ID Metadata Documents / dynamic registration | Authorization mechanism support | Pending assessment | Each mechanism individually documented and tested |
| Durable callback accounting, addressed host replies | Runtime authority, deadlines and budgets | Pending | Crash points, duplicate replies, fencing, one-worker liveness |
| Protected raw evidence | Current host authorization | Tool attempts implemented; other categories pending | [Evidence tests](../tests/runtime_mcp_results.rs) |

Deprecated HTTP+SSE transport and protocol versions other than 2025-11-25 are outside
this version profile. SSE above means Streamable HTTP. Independently versioned MCP
authorization extensions require separate assessment; they are not implied by
support for base authorization.

Every newly implemented row needs public API documentation, finite byte/work/time
bounds, cancellation and ownership semantics, and independent peer or durable-store
evidence. Handler installation alone does not satisfy a row. See the
[testing guide](testing.md) for required validation and existing fault campaigns.
