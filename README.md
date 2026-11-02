# Harness Toolkit

[![CI](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml/badge.svg)](https://github.com/thomasquintana/htlk/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/htlk.svg)](https://crates.io/crates/htlk)
[![Documentation](https://docs.rs/htlk/badge.svg)](https://docs.rs/htlk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

Harness Toolkit (`htlk`) is a Rust toolkit for building workflows with explicit
steps, typed data connections, and checks on inputs and results. Use it to combine
pure calculations, MCP tools, reusable tasks, bounded revision loops, and human
input in a workflow your application can inspect and recover.

You write the workflow in HTLK IR. The compiler checks it and produces a portable
executable graph. Your Rust application supplies authentication, policy, inputs,
and service connections; the runtime executes the graph and stores durable progress
in SQLite. A checked graph still needs application-specific verification of its
real-world results.

## Try it

Install [Rust](https://rustup.rs/) and [Git](https://git-scm.com/downloads), then:

```console
git clone https://github.com/thomasquintana/htlk.git
cd htlk
cargo run --example interactive_guide --locked
```

The six terminal lessons let you compile a graph, run it with your own inputs, add
a calculation, repair errors, answer a workflow's question, and approve or deny a
scripted tool call. They use the real compiler/runtime and need no provider
credentials or external services. The first build downloads dependencies and needs
your platform's Rust build tools.

Follow the [user guide](docs/user-guide.md) for setup, expected results, source
editing, troubleshooting, and larger workflow patterns. To check all lessons automatically:

```console
cargo run --example interactive_guide --locked -- --check
```

HTLK is a library toolkit; these commands run example host applications from this
repository. For a complete host workflow with composition, live extension, source
inspection, and separately authenticated approval:

```console
cargo run --example runtime_workflow --locked
```

## Use it in Rust

```toml
[dependencies]
htlk = "0.1.0"
```

The facade exposes `htlk::compiler` and `htlk::runtime`. Applications that need only
one layer can depend on its component directly:

| Crate | Responsibility |
|---|---|
| [htlk-compiler](crates/htlk-compiler/README.md) | Source and module compilation, interface inspection, and graph composition. |
| [htlk-runtime](crates/htlk-runtime/README.md) | Execution, durable state, host policy, MCP connections, and recovery. |
| [htlk-executable](crates/htlk-executable/README.md) | Canonical model, bounded codecs, digests, and executable envelopes. |
| [htlk-analyzer](crates/htlk-analyzer/README.md) | Shared linkage, type checking, graph verification, and offline schema analysis. |

The native runtime supports MCP 2025-11-25 over stdio and Streamable HTTP JSON/SSE.
Tool results preserve text, structured, and multimodal content in a uniform envelope.
Host policy governs effects and exact-action approvals. External hard token/cost
budgets require enforcement the native adapter does not currently provide.

## Documentation

- [User guide](docs/user-guide.md): learn and author workflows.
- [Runtime guide](docs/runtime-guide.md): embed, operate, inspect, and recover them.
- [MCP guide](docs/mcp-guide.md): connect services and consume results.
- [Specifications](specs/README.md): authoritative language and system contracts.
- [Detailed API examples](docs/executable-api.md): compiler, model, analyzer, and runtime APIs.
- [Documentation index](docs/README.md): maintainer references and testing.

## Development

Install stable Rust and Node.js 24; the integration tests start independent local
MCP peers. See [CONTRIBUTING.md](CONTRIBUTING.md) for the complete validation and
release workflow, and [testing](docs/testing.md) for coverage and fixture maintenance.
Changes are summarized in [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under Apache-2.0. See [LICENSE](./LICENSE) and the dependency notices in
[THIRD_PARTY_LICENSES](./THIRD_PARTY_LICENSES).
