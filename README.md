# Harness Toolkit

[![CI](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml/badge.svg)](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/htlk.svg)](https://crates.io/crates/htlk)
[![Documentation](https://docs.rs/htlk/badge.svg)](https://docs.rs/htlk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

Harness Toolkit (`htlk`) is a Rust workspace for compiling and running
harnesses. The root crate is a facade over two focused libraries:

- [`htlk-compiler`](https://crates.io/crates/htlk-compiler) compiles natural language into IR.
- [`htlk-rt`](https://crates.io/crates/htlk-rt) executes bytecode with actors, LLMs, SQLite, and MCP servers.

Both components depend on [`htlk-cbor`](crates/htlk-cbor/README.md), the shared
deterministic CBOR codec. It provides validated values, bounded encoding,
strict decoding, and structured errors. They also share
[`htlk-cbor-digest`](crates/htlk-cbor-digest/README.md) for typed SHA-256 digests,
strict digest parsing, and hashing raw bytes or canonical CBOR.
The compiler and runtime domain APIs
are being defined; executable production and registration will use this codec.

## Installation

```toml
[dependencies]
htlk = "0.1.0"
```

The facade exposes the component crates as `htlk::compiler` and
`htlk::rt`. Applications that only need one layer can depend on its crate
directly.

## Documentation

- [IR grammar](docs/ir-grammar.md)
- [Deterministic CBOR profile and API](crates/htlk-cbor/README.md)
- [SHA-256 digest representation and CBOR hashing](crates/htlk-cbor-digest/README.md)

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
