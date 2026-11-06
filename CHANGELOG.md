# Changelog

User-visible changes and compatibility boundaries are recorded here, following
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Experimental MCP tasks now execute in both directions: bounded tool task creation,
  polling/results/list/cancel, and durable deferred sampling/elicitation on independent
  control capacity. Task-required graph tools use explicit frozen deployment policy;
  accepted IDs resolve later callbacks to their actual accounted attempts. Creation
  intent prevents blind recreation after a lost ACK, while task cancellation preserves
  uncertain underlying work and replayable receipts. SQLite schema 13 rejects older stores.
- Durable MCP URL elicitation now connects callback consent, URL-required errors,
  and scoped completion notifications to authenticated host inspection/reply APIs.
  Unknown/duplicate completions are ignored; addressed events contain opaque IDs.
  Original RPC errors and delivery remain intact when URL admission fails, and
  neither consent nor completion fetches a URL or retries work. Streamable HTTP
  drains complete trailing SSE frames in the received response chunk. Private
  SQLite schema 12 deliberately rejects earlier stores.
- Native MCP HTTP credential providers bind exact server/resource/principal and run
  on every POST, GET/resumption and DELETE. Bounded OAuth/OIDC discovery, PKCE S256,
  pre-registration, resource indicators, refresh rotation and scope challenges are
  implemented with host-controlled navigation and secret persistence. Protected
  structured authentication concerns survive failed initialization; credentials
  never automatically replay operations. Optional CIMD, dynamic registration and
  independently versioned authorization extensions are explicitly assessed/deferred.
- Bounded MCP tool, prompt, resource, and template catalog browsing, capability-gated
  completion/logging utilities, and four-inventory stdio discovery without content
  acquisition. Durable host subscriptions share consumer intent, fence acknowledgements,
  preserve uncertain sends, and require explicit restoration across observable epoch
  gaps. Private SQLite schema 11 deliberately rejects earlier stores.
- Run-owned MCP callbacks carry runtime-established invocation/attempt origins and
  atomically charge every containing scope and wrapper before dispatch. Callback
  results preserve ancestor protection and uncertainty through the run journal.
- Durable host-owned sampling and addressed form/URL elicitation execute on bounded
  host workers independently of transport reception. Interactive and headless hosts
  share authorized inspection/reply commands; saved responses replay without model
  recall, and blocked callbacks cannot monopolize I/O or erase late evidence.
- Host callback outcomes distinguish underlying work completion from RPC reply
  delivery. Acknowledged timeout errors retain uncertain reservations; explicit
  reconciliation preserves the original reply. Usage receipts retain unknown
  values separately from measured zero.
- Native MCP result/error evidence preserves original payload bytes before JSON
  normalization, including escaped member names. Pending and unconsumed replies
  reserve bounded response capacity before request transmission.
- Correlated MCP JSON-RPC errors retain protected code/message/data in durable
  attempt evidence, including URL-elicitation-required errors. Graph-visible
  failures remain redacted, and inspecting errors cannot dispatch or retry work.
- Independent MCP client-service validation for multimodal sampling, balanced
  tool-use histories and tool choice, form/URL elicitation, and roots. Form numbers
  include fractional values as required by the authoritative MCP TypeScript model.
- Native MCP clients can link durable owner leases and store-scoped session epochs;
  request/event delivery rechecks ownership and expired leases stop reception.
  Run sessions verify actual persisted principals and fences without borrowing
  independent host-work budgets.
- Transactional host MCP ownership, persistent session epochs, callback work intent,
  saved replies and receipts, delivery facts, cumulative usage, and bounded late
  evidence storage. Private SQLite schema 10 deliberately rejects earlier stores.
- Protected MCP evidence inspection now includes prompt, resource, and template
  attempts, with explicit operation kind, bounded original bytes, and delivery
  evidence; tool-only normalization and inference remain independently restricted.
- Run-owned native MCP operations use the same persistent dispatcher as host-owned
  sessions, without exclusive reader leases. Per-attempt ingress limits, exact
  deduplication keys, cooperative stop hints, and sticky ambiguous-session failures
  remain independently enforced.
- Persistent native MCP client sessions with bounded idle event delivery, reverse
  ping, concurrent request correlation, cooperative cancellation, stdio/Streamable
  HTTP transports, SSE resumption, explicit session epochs, and observable overflow.
- Validated MCP client service configuration, finite session limits, distinct
  host/run owner identities, callback context, and protected structured RPC errors.
  These types form the bidirectional client foundation; native transport integration
  and durable callback execution are tracked in the MCP conformance inventory.

- HTLK IR Draft 0.1 compilation from standalone source, exact source bundles, and
  canonical documents; module interface inspection and public-port graph joins.
- Shared canonical executable records, deterministic CBOR/JCS, content identities,
  bounded codecs, and semantic analysis of types, graphs, expressions, and schemas.
- Native Rust evaluation and explicitly linked pure libraries, including metered
  `htlk.mcp-content` text, JSON, candidate-selection, and checked-narrowing helpers.
- Durable SQLite/Actix execution with fenced ownership, immutable artifacts,
  authenticated idempotent admission, typed waits, exact-action approvals,
  cancellation, additive extension, checked recovery, and protected inspection.
- Source metadata outside executable identity, completion audit promises,
  immutable retention tombstones, and reference-safe garbage collection.
- MCP 2025-11-25 stdio and Streamable HTTP JSON/SSE, pinned descriptors, run-owned
  sessions, delivery-aware retry/deduplication, cancellation, and bounded cleanup.
- Uniform tool-result envelopes for schema-less, text, structured, and multimodal
  results; bounded literal JSON extraction, protected evidence, passive inference,
  immutable schema revisions, and explicit adoption.
- Explicit engine run selection with `ExecutionScope::Attached`, bounded idempotent
  attachment, and a separate intentional store-wide mode.
- Six interactive lessons, reusable workflow/live-session examples, independent
  transport peers, process-kill recovery and concurrency tests, and per-crate
  line/branch coverage gates.
- Coordinated workspace packaging/publication and warning-free API documentation
  artifacts in CI and releases.

### Changed

- Separate bounded executable representation, semantic analysis, and runtime
  admission. Model construction alone does not establish semantic validity.
- Keep compiler results immutable while attached to analysis; detach owned data
  with `CompilationSuccess::into_artifact`.
- Use context-free model expression/node/scope/edge conversion APIs. Contextual
  legality belongs to analyzer checks at actual use sites.
- Use `BuiltinType`, `ValueTypeKind::Builtin`, and `ValueType::builtin`; canonical
  result names are `McpToolResult`, `McpResourceResult`, and `McpPromptResult`.
- Align HTLK-owned formats on `0.1`, contracts on `preconditions`/`postconditions`,
  and envelopes on `htlk.executable.graph`. Earlier draft source needs explicit
  updating and recompilation; bytes and history cannot be relabeled.
- Replace direct tool application outputs with `McpToolResult`. Output schemas are
  optional; trusted compiler policy/document inputs and independent runtime result
  policy selection pin interpretation. Old binding records have no legacy decoder.
- Require private SQLite schema version 7; incompatible databases are rejected
  without automatic migration or deletion. Rebuild disposable development state explicitly.
- Consolidate human guides and technical authorities; separate agent instructions
  and remove completed plans, reports, and workstation-specific acceptance tooling.

### Fixed

- Pin the third-party notice generator consistently across CI, release validation,
  and contributor instructions so an upstream tool release cannot silently change
  the expected license text.
- Capture crash-recovery test baselines after the worker exits so its final commits
  cannot be mistaken for changes made by an unattached recovery engine. Exercise
  skipped-child recovery explicitly, including its absent admission checkpoint and
  unchanged immutable record.
- Recheck progress after external-work scans to avoid false invariant failures
  when host replies commit concurrently; retain stable impossible-state detection.
- Make approval denial and deadline acceptance transactional; preserve classified
  replies, causal diagnostics, terminal winners, and fallback liveness.
- Preserve MCP sessions across nodes in a run; fail expired/ambiguous sessions
  without replacement or unauthorized replay. Resume saved responses locally even
  when extension changes accounting ancestry.
- Correct catalog/schema-context validation, nested union projection, prompt
  parameter checks, conditional-writer analysis, and bounded diagnostic accounting.
- Support explicit float prompt parameters with deterministic shortest-round-trip
  rendering while preserving argument type identity.
- Update pinned Tokio and TLS dependencies for the applicable RustSec advisories.

### Removed

- Placeholder IR, standalone CBOR/digest crates, embedded Lua, unused templating,
  provider, SDK, and vector dependencies. Shared model/analyzer/runtime layers
  provide the active implementation.
