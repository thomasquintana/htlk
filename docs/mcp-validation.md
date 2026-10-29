# MCP result redesign: validation and handoff

## Delivered contract

Every accepted tool call returns `McpToolResult`, including tools without an output
schema. Exact upstream descriptors remain pinned. Declared contracts default to
Enforce; host-selected Report, explicitly attached inferred Observe/Enforce, and
application constraints remain independent. Normalized JSON is literal bounded
extraction, with provenance, ambiguity, exhaustion and absent/null distinctions.

Inference consumes explicitly authorized recorded observations and proposes immutable
schema/evidence revisions. It performs no extra tool calls and changes no executable
until explicit adoption and recompilation. Tool failures retain protected evidence
without successful output publication. Recovery finishes local processing from
durable responses and decisions under pinned implementations, without provider recall.

The explicit `htlk.mcp-content` version-1 library provides metered content inspection,
candidate selection, JSON/pointer access, checked narrowing, explicit joining,
literal matching and unique regex capture. Private storage is version **7**; old
development stores reject without migration or automatic deletion.

API and operator guidance:

- [Semantic design](mcp-result-design.md)
- [Protected evidence and passive inference](mcp-evidence.md)
- [Content library](mcp-content-library.md)
- [Development-state rebuild](development-state.md)
- [Actual Chrome acceptance and reproduction](mcp-live-acceptance.md)

## Implementation checkpoints

| Commit | Completed task |
| --- | --- |
| `22e3901` | Unified result and inference semantics |
| `ed5f617` | Checked result and policy records |
| `3ae1601` | Complete protocol content admission |
| `75427c0` | Bounded literal-JSON extraction and source evidence |
| `931862c` | Identified-observation schema inference |
| `8d5d645` | Unified bindings, compiler and runtime envelope execution |
| `7849861` | Incompatible development-store rejection without migrations |
| `fd1ec06` | Protected durable normalization and passive sampling |
| `9989cad` | Metered content/JSON library and updated consumers |
| `e580856` | Actual Chrome acceptance and independent schema-less wire tests |

The validation commit containing this document adds negative, diagnostic, boundary
and restart tests; it changes no production implementation or coverage thresholds.
It also makes test polling handle the runtime's explicit transient `Fenced`
inspection category, with a finite deadline and no retry of other failures.

The reviewed fixed native fixture remains
`sha256:ebbe7139c836ed9d390f33ffe72db453428307060ef27e2dfb26eeafad655125`.
Its scope remains
`sha256:a0a913309d437ba674343e89eaeda42314773a4bcb14debced7148df89b0f221`.

## Measured acceptance

The actual clean Chrome DevTools MCP 1.12.0 campaign passed with 32 discovered
schema-less tools, 21 authored calls, zero inference calls, three selected fenced
JSON samples, preserved screenshot content, protected tool-error evidence, Observe
and Enforce drift checks, explicit new-revision adoption, and verified test-owned
tab/session cleanup. Reopening its saved store made zero provider calls. Exact
source/build/protocol/descriptor/policy/run/artifact identities are in the linked
acceptance report.

Independent Node peers separately passed stdio, Streamable HTTP JSON and SSE cases
for prose JSON, arrays/scalars/null, duplicates, ambiguity, structured preference,
multimodal/resource contents, malformed media and authoritative errors. Native
transport checks additionally cover reverse requests, control-message bounds,
session/identity drift, hostile framing, split UTF-8 and known-unsent cancellation.
These fixtures are distinct from the actual Chrome evidence.

## Quality-gate record

On 2026-10-29, the complete ordinary workspace/all-feature suite, including all
doctests, passed. Complete independent instrumented all-target suites passed on
Rust 1.98.0 and nightly-2026-10-20 using cargo-llvm-cov 0.9.1 and Node 24.22.2.
Both unchanged coverage ratchets passed:

| Crate | Primary line coverage | Uncovered lines | Nightly branch coverage |
| --- | --- | --- | --- |
| Analyzer | 92.73% | 505 | 82.29% |
| Compiler | 94.92% | 192 | 87.78% |
| Executable | 98.33% | 99 | 96.30% |
| Runtime | 91.46% | 1,633 | 73.94% |

The reexport-only facade has no executable production lines. The 99% target remains
future work; these are measured passes of the existing floors and budgets.
Reports are retained locally as `target/coverage/mcp-line.json` and
`target/coverage/mcp-branch.json`, with separate coverage target directories.

Also verified: strict all-target/all-feature Clippy, formatting, warnings-denied
workspace rustdoc, specification validation, independent native executable fixture
verification, Node coverage/discovery tests, actionlint, dependency/advisory/license
policy, and cargo-about generation with an exact third-party-notice comparison.
Permitted dependency-version duplication warnings did not fail cargo-deny.

Workspace packaging uses a fresh target directory so Cargo verifies the edited
unpublished workspace dependencies rather than reusing older same-version archives.
The pre-commit run uses `--allow-dirty`; the committed snapshot is then packaged
with the clean command below before handoff.

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
node specs/validate-specs.mjs
node tools/validate-executable-fixtures.mjs
node --test tools/coverage.test.mjs tools/discover-mcp.test.mjs
actionlint
cargo deny check
cargo package --workspace --locked --target-dir "$(mktemp -d)"
```

The full coverage commands and unchanged ratchet files are documented in
[coverage and deep testing](test-coverage.md).

## Development-state inventory

No obsolete persistent HTLK development database was identified in the workspace
at baseline. The temporary campaign inventory contains only the newly created
`campaign-one/runs.sqlite` evidence store. A read-only SQLite check confirmed
application ID `1213484107` (`0x48544c4b`) and schema version **7**. It remains
current protected acceptance evidence. No legacy-store deletion was necessary.

All tracked consumers and fixed executable/source-package examples have been
regenerated or updated. Old binding records have no compatibility decoder. Store
tests verify that versions 1–6, future versions and foreign databases reject without
changing their original authority or contents. New graphs/stores use the rebuild
procedure above; schema revisions still require explicit adoption.
