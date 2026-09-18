# Harness Toolkit

[![CI](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml/badge.svg)](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/htlk.svg)](https://crates.io/crates/htlk)
[![Documentation](https://docs.rs/htlk/badge.svg)](https://docs.rs/htlk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

Harness Toolkit (`htlk`) is a Rust workspace for compiling and running
harnesses. The root crate is a facade over two focused libraries:

- [`htlk-compiler`](https://crates.io/crates/htlk-compiler) provides foundations for compiling HTLK IR into executable graphs.
- [`htlk-rt`](https://crates.io/crates/htlk-rt) provides foundations for native Rust graph execution with actors, LLMs, SQLite, and MCP servers.

Both components share [`htlk-executable`](crates/htlk-executable/README.md) for
canonical Draft 0.1 records, bounded construction, Serde wire mappings, the public
`cbor` adapter, canonical JSON, digests and envelopes. They also share
[`htlk-analyzer`](crates/htlk-analyzer/README.md) for semantic linkage, typing,
graph verification, offline schema preparation and immutable document-bound results.
The analyzer depends on the model and on neither compiler nor runtime.

`htlk-rt::verify_executable` performs exact host-policy/implementation admission and
recomputes semantic analysis from authoritative canonical bytes. Runtime owns
evaluation, callback dispatch and actual-value enforcement. Compiler production,
agent repair loops and runtime registration transactions remain future work.

## Installation

```toml
[dependencies]
htlk = "0.1.0"
```

The facade exposes the component crates as `htlk::compiler` and
`htlk::rt`. Applications that only need one layer can depend on its crate
directly.

## Documentation

- [Draft 0.1 specifications and IR grammar](specs/README.md)
- [Canonical executable model and digests](crates/htlk-executable/README.md)
- [Deterministic CBOR profile and API](crates/htlk-executable/src/cbor/README.md)
- [Shared semantic analysis](crates/htlk-analyzer/README.md)
- [Runtime admission and native evaluation](crates/htlk-rt/README.md)
- [Detailed cross-crate API reference](docs/executable-api.md)
- [Executable/verifier completion checklist](docs/htlk-executable-roadmap.md)
- [CI documentation artifacts and crates.io releases](CONTRIBUTING.md#releases)

## Development

The workspace follows latest stable Rust. Install the configured toolchain and
run the standard checks:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

See `CONTRIBUTING.md` for contribution and release guidance.

## License

Licensed under the Apache License, Version 2.0. See the `LICENSE` file.
Third-party dependency notices are provided in `THIRD_PARTY_LICENSES`.
