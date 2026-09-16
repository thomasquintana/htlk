# Harness Toolkit

[![CI](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml/badge.svg)](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/htlk.svg)](https://crates.io/crates/htlk)
[![Documentation](https://docs.rs/htlk/badge.svg)](https://docs.rs/htlk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

Harness Toolkit (`htlk`) is a Rust workspace for compiling and running
harnesses. The root crate is a facade over two focused libraries:

- [`htlk-compiler`](https://crates.io/crates/htlk-compiler) provides foundations for compiling HTLK IR into executable graphs.
- [`htlk-rt`](https://crates.io/crates/htlk-rt) provides foundations for native Rust graph execution with actors, LLMs, SQLite, and MCP servers.

Both components depend on [`htlk-cbor`](crates/htlk-cbor/README.md), the shared
deterministic CBOR codec. It provides validated values, bounded encoding,
strict decoding, and structured errors. They also share
[`htlk-executable`](crates/htlk-executable/README.md), which provides canonical
Draft 0.1 graph records, offline schema/MCP validation, native expression execution,
and the composed `verify_executable` boundary. It returns an immutable verified
graph with checked runtime plans after exact host-policy and implementation linking.
Compiler production and runtime registration call sites remain consumer work.

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
- [Deterministic CBOR profile and API](crates/htlk-cbor/README.md)
- [Executable verifier, native evaluation, and digest API](crates/htlk-executable/README.md)
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
