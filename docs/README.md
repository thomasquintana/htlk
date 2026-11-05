# Documentation

## Learn and build

1. [User guide](user-guide.md): setup, six terminal lessons, and workflow authoring.
2. [Runtime guide](runtime-guide.md): host configuration and the execution lifecycle.
3. [MCP guide](mcp-guide.md): service connections, results, evidence, and schema adoption.
4. [Detailed API examples](executable-api.md): checked Rust examples across the workspace.

Component entry points: [compiler](../crates/htlk-compiler/README.md),
[runtime](../crates/htlk-runtime/README.md),
[executable model](../crates/htlk-executable/README.md),
[CBOR codec](../crates/htlk-executable/src/cbor/README.md), and
[analyzer](../crates/htlk-analyzer/README.md).

## Consult the contracts

[Specifications](../specs/README.md) identify the authoritative language, compiler,
runtime, and MCP rules, with machine-readable grammar/schema assets. The
[worked examples](../specs/examples/README.md) are active compiler/runtime fixtures.

## Maintain the project

- [Contributing](../CONTRIBUTING.md): prerequisites, Rust conventions, and releases.
- [Testing](testing.md): check commands, conformance map, coverage, and fixtures.
- [MCP client conformance](mcp-conformance.md): version-specific native support and qualification gaps.
- [Runtime internals](runtime-internals.md): private persistence and concurrency invariants.
- [Changelog](../CHANGELOG.md): user-visible changes and compatibility boundaries.

## Documentation conventions

README and guides are written for people and organized around tasks. Specifications
own technical requirements; guides link to those authorities. Package READMEs remain
local rustdoc entry points. Current agent-only operating instructions belong in
[AGENTS.md](../AGENTS.md); executable prompt templates remain source assets alongside
their consumers. Completed plans and campaign transcripts are available in Git history.

Document supported behavior in the present tense and distinguish runnable examples
from fragments and illustrative services. Link tests and CI reports for evidence
instead of copying changing coverage totals or fixture fingerprints into prose.
Update references and validation whenever moving or deleting a document.
