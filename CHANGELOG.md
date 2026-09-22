# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Install actionlint through its Go module in CI instead of falling back to a
  nonexistent crates.io package.

### Added

- Shared `htlk-analyzer` with focused expression/scope/linkage checks, offline
  schema preparation, structured diagnostics, explicit runtime obligations and
  immutable document-bound analysis results.
- Explicit canonical conversion and codec APIs for model records and values,
  using pinned cbor2 primitives behind the bounded `htlk_executable::cbor` adapter.
  Public model `serde::Serialize` support has been removed; internal JSON
  processing retains `serde_json`.
- Composed `verify_executable` admission with an immutable verified graph, borrowed
  checked execution plans, structured diagnostics, and exact host policy/profile linking.
- Full scope binding coverage, whole-port compatibility, wait-dependency cycles,
  observability, and per-use loop-body verification with deterministic derived plans.
- Native host-library registry with complete manifest checks, prepaid work charges,
  and checked higher-order callback forwarding without callable application values.
- Schema-context-preserving projections, permitted dynamic-field access, optional
  declaration tracking, and conservative schema-family refinement without coercion.
- Static expression name/type analysis with generic inference, callback compatibility,
  presence tracking, bounded metadata, and explicit actual-value obligations.
- Native expression evaluation with lazy conditions, explicit absence/pending/errors,
  prompt rendering, outcomes, Rust function dispatch, and deterministic evaluator limits.
- Native validation against pinned MCP 2025-11-25 protocol data, consistent binding
  selections, normalized result checks, and bounded RFC 6570 scalar expansion.
- Native JSON Schema 2020-12 compilation and validation, offline resource linking,
  object-root admission, dynamic references, and supported pattern/input limits.
- Downloadable CI/release rustdoc artifacts, manual release dry runs, verified crate
  archives, workflow/specification checks, and all-feature docs.rs build metadata.
- A focused Draft 0.1 completion checklist for the shared executable/verifier crate.
- Prescribed embedded-schema base selection and explicit executable schema-catalog
  validation for required roots, reference closure, unused entries, and snapshot-copy limits.
- Conservative offline schema reference closure with explicit retrieval-context
  tracking, cycle-safe traversal, borrowed reference records, and bounded pointer work.
- Bounded multi-document schema catalogs with cross-document resource/anchor
  lookup, context-checked resource-copy merging, and borrowed complete-document targets.
- Per-document schema resource/base and anchor indexing with bounded RFC 3986
  reference lookup relative to actual nested resource roots, without network I/O.
- Bounded JSON Schema 2020-12 location discovery with exact JSON Pointers,
  standard applicator traversal, instance-data exclusion, and derived-index limits.
- Bounded RFC 6901 JSON Pointer and URI-fragment parsing with exact token decoding,
  strict array indices, borrowed JSON lookup, and structured location errors.
- RFC 6570 resource-template syntax and exact variable-interface validation, with
  bounded discovery, preserved percent-encoded names, and required string arguments.
- Fixed-resource and prompt interface validation, including exact protocol output
  types and descriptor-derived prompt argument names, string types, and presence.
- Cross-record MCP descriptor selection and extracted tool-schema integrity checks,
  exact tool port/schema matching, and reached-tool-root restrictions on schema types.
- Policy scope-depth and expanded-invocation checks over the scope-definition DAG,
  including reused bodies, nested loop multipliers, checked arithmetic, and an
  immutable derived structural summary outside the executable wire format.
- Bounded external JSON/JCS documents with duplicate-key and HTLK numeric checks,
  exact raw-JSON digests, and closed policy documents with required defaults.
- Canonical document assembly and envelope integration with record-key integrity,
  scope-definition closure/roles/interfaces, and known template/library/MCP references.
- Canonical execution profiles, engine identities, library/function signatures,
  compound server identities, and MCP bindings with bounded record validation.
- Canonical scopes, nodes, edges, operations, and identifier-keyed port tables,
  with local structural checks and the closed compiler record-digest domains.
- Canonical expressions, scoped reference-category checks, static library function
  references, and prompt templates with normalization, bounded codecs, and digests.
- Canonical `ExecutionLimits` and `RetryPolicy` records with checked integer
  ranges, explicit inheritance/zero semantics, and strict retry cardinality/order.
- Canonical `ValueType` and `Port` records, bounded normalization and serialization,
  strict type decoding, and explicit value/signature context checks.
- Validated local `Identifier` names and structured `ParseIdentifierError`
  diagnostics, following the canonical ASCII spelling rule without normalization.
- Version-0.1 executable envelopes with immutable payload access, explicit
  format/version checks, domain-separated fingerprints, and structured validation
  errors using the existing codec limits.
- Shared `htlk-executable::digest` with typed SHA-256 digests, canonical text parsing
  and formatting, exact-byte hashing, and bounded canonical-CBOR hashing.
- Shared `htlk_executable::cbor` foundation with validated values, canonical-order maps,
  configurable limits, structured errors, and the HTLK deterministic CBOR profile.
- Bounded deterministic CBOR encoding with shortest-exact floats, checked resource
  accounting, and a separately tested depth ceiling of 128 (default 64).
- Strict bounded CBOR decoding with canonicality checks, input error offsets,
  incremental collection allocation, and tested nested failure cleanup.

### Changed

- Separated canonical executable representation from semantic analysis and runtime
  admission/execution. Local name/use-context checks and structural summaries now
  belong to the analyzer; runtime recomputes analysis from authoritative input.
- Native identities compose package-owned model/analyzer/runtime source coverage;
  fixtures and release order follow the extracted crate boundaries.
- Defined the native-engine direction and its supported-limit contract separately
  from deterministic work accounting in the Rust expression evaluator.
- Updated the locked `rustls` dependency to 0.23.45 to address RUSTSEC-2026-0285.
- Aligned executable envelopes and draft specifications on `htlk.executable.graph`,
  `version: "0.1"`, and SHA-256 of the newline-terminated format/version prefix
  plus raw payload bytes. Alternate schemas and hash formulas are rejected.
- Updated active CBOR profile documentation and runtime-identity examples to the
  unified HTLK 0.1 baseline.
- Consolidated digest support into `htlk-executable::digest`, replacing the
  `htlk-cbor-digest` package while preserving digest behavior and wire results.
- Regenerated the dependency lockfile and notices with compatible releases,
  replacing the yanked `chacha20` 0.10.1 resolution.
- Compiler and runtime share the executable model and analyzer; releases publish
  executable, analyzer, compiler/runtime, then the facade, after workspace verification.

### Removed

- The standalone `htlk-cbor` package, merged into `htlk_executable::cbor`.
- Embedded Lua and the unused Tera scripting/template dependency. The execution
  plan uses native Rust expression evaluation, rendering, and JSON Schema validation.
- The placeholder `htlk-ir` crate and its facade export.

## [0.1.0] - TBD

### Added

- Initial Harness Toolkit workspace with compiler, IR, runtime, and facade crates.
