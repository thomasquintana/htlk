# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
- Shared `htlk-cbor` foundation with validated values, canonical-order maps,
  configurable limits, structured errors, and the HTLK deterministic CBOR profile.
- Bounded deterministic CBOR encoding with shortest-exact floats, checked resource
  accounting, and a separately tested depth ceiling of 128 (default 64).
- Strict bounded CBOR decoding with canonicality checks, input error offsets,
  incremental collection allocation, and tested nested failure cleanup.

### Changed

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
- Compiler and runtime share `htlk-cbor`; package validation covers the complete
  workspace, and releases publish the codec before dependent crates.

### Removed

- The placeholder `htlk-ir` crate and its facade export.

## [0.1.0] - TBD

### Added

- Initial Harness Toolkit workspace with compiler, IR, runtime, and facade crates.
