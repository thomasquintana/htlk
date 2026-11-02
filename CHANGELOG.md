# Changelog

User-visible changes and compatibility boundaries are recorded here, following
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
