# Harness Toolkit

[![CI](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml/badge.svg)](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/htlk.svg)](https://crates.io/crates/htlk)
[![Documentation](https://docs.rs/htlk/badge.svg)](https://docs.rs/htlk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

Harness Toolkit (`htlk`) is a Rust workspace for compiling and running
harnesses. The root crate is a facade over three focused libraries:

- [`htlk-compiler`](https://crates.io/crates/htlk-compiler) compiles natural language into IR.
- [`htlk-ir`](https://crates.io/crates/htlk-ir) parses and compiles IR into bytecode.
- [`htlk-rt`](https://crates.io/crates/htlk-rt) executes bytecode with actors, LLMs, SQLite, and MCP servers.

The component APIs are intentionally minimal while their domain requirements
are being defined.

## Installation

```toml
[dependencies]
htlk = "0.1.0"
```

The facade exposes the component crates as `htlk::compiler`, `htlk::ir`, and
`htlk::rt`. Applications that only need one layer can depend on its crate
directly.

## Documentation

- [IR grammar](docs/ir-grammar.md)

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
