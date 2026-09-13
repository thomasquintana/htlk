# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Shared `htlk-cbor-digest` with typed SHA-256 digests, canonical text parsing
  and formatting, exact-byte hashing, and bounded canonical-CBOR hashing.
- Shared `htlk-cbor` foundation with validated values, canonical-order maps,
  configurable limits, structured errors, and the HTLK deterministic CBOR profile.
- Bounded deterministic CBOR encoding with shortest-exact floats, checked resource
  accounting, and a separately tested depth ceiling of 128 (default 64).
- Strict bounded CBOR decoding with canonicality checks, input error offsets,
  incremental collection allocation, and tested nested failure cleanup.

### Changed

- Regenerated the dependency lockfile and notices with compatible releases,
  replacing the yanked `chacha20` 0.10.1 resolution.
- Compiler and runtime share `htlk-cbor`; package validation covers the complete
  workspace, and releases publish the codec before dependent crates.

### Removed

- The placeholder `htlk-ir` crate and its facade export.

## [0.1.0] - TBD

### Added

- Initial Harness Toolkit workspace with compiler, IR, runtime, and facade crates.
