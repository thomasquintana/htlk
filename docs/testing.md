# Testing and fixture maintenance

Use [CONTRIBUTING.md](../CONTRIBUTING.md#development) for the complete required
repository checks. This guide explains what those checks establish, the behavioral
coverage map, instrumented measurements, and intentional fixture updates.

## Goals and measurement

Coverage baselines guide review and meaningful test selection; they are
recommendations rather than release targets. CI reports per-crate comparisons from
[`coverage-thresholds.json`](../tools/coverage-thresholds.json) and the branch baseline
without failing solely on percentage or uncovered-line counts. Adequacy is judged
alongside independent observable invariants, fault scenarios, and remaining reachable
paths. Per-crate reporting keeps a large crate from concealing another's gaps.

The primary measurement uses Rust **1.98.0** and `cargo-llvm-cov` **0.9.1**.
The separate branch measurement uses **nightly-2026-10-20**, since Rust branch
instrumentation is experimental. Its baselines live in
[`coverage-branch-thresholds.json`](../tools/coverage-branch-thresholds.json).
Toolchain upgrades require reviewing both reports: different LLVM/Rust versions
can assign different line/region counts to the same source.

Reports cover `crates/*/src/` and the facade's `src/`. The facade currently only
reexports APIs and has no executable lines; it is reported as **n/a**, with a
100% floor for any future executable code. Integration tests, peers, examples,
dependencies and generated build output are outside the reported source paths.
Existing crate-local unit tests share their production source files and appear
in LLVM's file totals. All tests still execute. No production coverage-skipping
attributes or coverage-specific behavior are used.

Download the `coverage-line-<SHA>` and `coverage-branch-<SHA>` CI artifacts for
current exact counts, machine-readable JSON, LCOV and annotated HTML. Open
`html/index.html` for the source view. Per-crate percentages and uncovered-line
counts are also written to the GitHub Actions job summary.

## Running locally

Install Node.js 24 for the independent local MCP peers. These tests use loopback
ports, local child processes and isolated temporary SQLite databases; they require
no service credentials or remote MCP server.

The ordinary Rust suite includes every E2E, crash, and stress campaign. The retained
Node tests independently exercise documentation, coverage, and discovery helpers:

```console
cargo test --workspace --all-features --locked
node --test tools/validate-docs.test.mjs tools/coverage.test.mjs tools/discover-mcp.test.mjs
```

To reproduce the primary CI coverage measurement:

```console
rustup toolchain install 1.98.0 --profile minimal --component llvm-tools-preview
cargo +stable install cargo-llvm-cov --version 0.9.1 --locked
mkdir -p target/coverage
cargo +1.98.0 llvm-cov --workspace --all-features --all-targets --locked --json --output-path target/coverage/coverage.json
cargo +1.98.0 llvm-cov report --workspace --html --output-dir target/coverage
node tools/coverage.mjs target/coverage/coverage.json --recommendations tools/coverage-thresholds.json
```

For branch coverage:

```console
rustup toolchain install nightly-2026-10-20 --profile minimal --component llvm-tools-preview
mkdir -p target/coverage
cargo +nightly-2026-10-20 llvm-cov --workspace --all-features --all-targets --locked --branch --json --output-path target/coverage/coverage.json
node tools/coverage.mjs target/coverage/coverage.json --recommendations tools/coverage-branch-thresholds.json
```

Use separate `CARGO_LLVM_COV_TARGET_DIR` directories if keeping stable and nightly
profiles simultaneously. The default coverage command clears old profiles.
`cargo llvm-cov report` exports additional formats from the same run without
executing tests again. Include `--workspace` when exporting reports.

Doctests run in the ordinary workspace suite. The stable coverage collector's
`--all-targets` measurement covers unit, integration and example test targets;
it does not instrument doctests.

## Deep scenarios

[`tests/deep_e2e.rs`](../tests/deep_e2e.rs) runs compiled-graph scenario matrices:

| Campaign | Evidence |
| --- | --- |
| All four MCP operation categories | Source compilation, native admission, durable SQLite, Actix workers and the native adapter over stdio, HTTP JSON and HTTP SSE. Includes paginated discovery, Unicode, present null, normalized binary resource contents, prompt data and protected artifacts. |
| Hostile peers | Identity/descriptor drift, missing operations, repeated pagination cursors, duplicate JSON members, wrong response IDs, correlated RPC errors, explicit tool errors and output-schema violations. Asserts dispatch counts, delivery-aware classifications, unpublished outputs and journal reconstruction. |
| Independent branch oracle | Every four-node activation mask under one, two and four workers. Checks each present/absent public output and full replay. |
| Worked decision brief | Both standalone and modular source, acceptance at review one/two/three and all-rejected fallback, over native HTTP JSON and SSE. Checks exact 4/6/8-call bounds and output values. |
| Repeated joins | Executes one through four preserved operand implementations, checking nested output transformations, scope counts and replay. |
| Exact-action approvals | Authenticated allow/deny, wrong request/kind, duplicate and conflicting disposition keys, terminal history, zero dispatch before approval and immutable disposition labels over every transport. |
| Native cancellation | Observes the actual in-flight request and protocol cancellation notification, preserves spent/ambiguous accounting and replays the terminal result. |
| Non-tool protocol failures | Invalid binary resources and prompt roles fail over all three transports, cannot publish graph outputs, and cannot impersonate tool inference sources. |

The independent wire peer is
[`tests/support/mcp_peer.mjs`](../tests/support/mcp_peer.mjs). It uses Node's
HTTP/JSON implementation, records requests, and emits raw malformed responses.
The Rust harness in [`tests/support/e2e.rs`](../tests/support/e2e.rs) verifies
authoritative record families through two-entry journal/record pages. Polling is
bounded and retries the explicitly transient fenced-snapshot inspection result.
Unexpected inspection failures remain failures.

[`tests/runtime_mcp_sessions.rs`](../tests/runtime_mcp_sessions.rs) additionally
executes stateful browser and sandbox sequences against session-local stdio/HTTP/
SSE peers. It verifies concurrent-run isolation, fresh-session negative controls,
sticky session loss, ownership takeover and terminal/shutdown cleanup. The
[MCP guide](mcp-guide.md) documents the retained opt-in runners.

[`tests/runtime_crash.rs`](../tests/runtime_crash.rs) kills real worker processes
at four observed checkpoints: response persistence before acceptance, an unsafe
in-flight dispatch, an open typed wait, and committed loop feedback followed by
the next wait. A fresh runtime opens the same database with a higher fence and
**no MCP adapter**. It verifies local response acceptance or effect uncertainty,
same wait/definition/input identities, final values, call counts and full replay.
A readiness marker follows durable initialization; checkpoint selection does not
depend on guessing a sleep duration. Killed processes may not flush LLVM counters;
the existing graceful checkpoint tests and recovering parent cover those paths.

[`tests/runtime_stress.rs`](../tests/runtime_stress.rs) starts 72 distinct runs
through 144 competing host starts under one/two/four-worker configurations. Two
independent SQLite connections and execution engines contend for authority. The
campaign checks one run per idempotency key, exact output values, immutable
terminal outcomes, conflicting-key rejection and replay from the other connection.

[`tests/runtime_recovery.rs`](../tests/runtime_recovery.rs) additionally rejects
scope/invocation/wait/accounting and recorded MCP request/response corruptions,
including independently rehashed normalization records and protected inspection.
Corrupt checkpoints cannot publish output artifacts or authorize redispatch.

## Coverage-directed boundary suites

- Compiler tests cover complete closed bundle/join/catalog ingress, structural
  mutations, package and module limits, import diamonds, graph roles, library
  selection, all export kinds, UTF-8 spans, every UTF-8 truncated prefix, lexical
  escapes, malformed expressions/operations, policy/report ceilings and diagnostic
  ordering. Accepted unused catalog mutations preserve executable identity.
- Executable tests mutate every canonical record family and assert exact native
  value/CBOR round trips for every accepted mutation. Policy fields have independent
  range/shape tests; public errors retain redacted typed causes.
- Analyzer tests cover schema constants, alternatives, tuples, open members,
  presence, runtime obligations and stable diagnostic/source-chain families.
- Runtime tests add scalar comparison and actual-type Cartesian products, malformed
  accepted artifact/source metadata, cache configurations, historical reply keys,
  native callback forwarding with one shared meter, and typed failure records.

These supplement the [conformance map](#conformance-map),
exhaustive binding/cycle oracles, codec vectors and controlled-stack tests.

## Coverage-directed recommendations

The annotated reports retain every remaining production gap. The highest-value
follow-ups are additional schema/generic inference combinations, callback variance
and budget failures, approval/reservation checkpoint corruption, policy changes at
additional execution boundaries, and less-common extension/retention failure paths.
Some residual locations are allocation-failure handlers, unreachable assertions,
or error-propagation spans whose failure is prevented by earlier validation; keep
their status distinct from untested reachable behavior when reviewing the report.

New tests should establish independent values, failure classifications, durable
invariants or ordering guarantees. Keep the genuine E2E and fault-injection
assertions as well as the coverage numbers. Review any native source edits against
the existing source-derived identity workflow and independent fixture checker.
Tests never regenerate golden fixtures automatically.

## CI and releases

The reusable [`coverage.yml`](../.github/workflows/coverage.yml) runs both complete
coverage suites in parallel on **every PR and main push**, and during release
validation. Every E2E, subprocess crash and bounded stress campaign runs in these
jobs; none is ignored or deferred to a schedule. Reports upload even after a test
failure. Release publishing depends on both ordinary validation and coverage.

Coverage validation rejects missing/malformed/duplicate core measurements, new
unconfigured production crates, invalid baseline configuration and absent branch
instrumentation. Percentage and uncovered-line comparisons are advisory; `--check`
remains available for explicit strict local comparisons. Node tests also exercise CLI
failure statuses and the facade's transition from reexports to executable code.

## Documentation checks

`node tools/validate-docs.mjs` checks local file links and heading anchors across
tracked/new Markdown, excluding fenced code. Its Node tests cover link resolution,
duplicate heading slugs, code fences, and missing anchors. It does not fetch external URLs.

`node specs/validate-specs.mjs` checks the EBNF copy, fenced HTLK syntax, JSON examples,
diagram captions, standalone/modular correspondence, source-package digests, and
CDDL reference closure. It does not type-check examples or fully validate CDDL
instances. `tests/documentation.rs` separately compiles complete guide programs and
checks their independently observed outputs, parser/grammar ordering, and documented
failure spellings against the public API. The spec checker also verifies complete
failure-code table coverage against runtime source. Rustdoc examples run as doctests; the facade includes the
detailed API reference specifically for that purpose.

## Conformance map

The normative cases are in [runtime §17](../specs/runtime-spec.md#17-required-conformance-scenarios).
The following suites establish observable invariants with real compiler-produced
envelopes, independent admission, SQLite, and execution workers where applicable.
Test-source references describe coverage; CI artifacts establish results for a commit.

| Contract | Regression evidence |
|---|---|
| All guards settle before source selection; late conflicts cannot dispatch | `tests/runtime_execution.rs`, `tests/runtime_conformance.rs`; exhaustive independent three-candidate oracle in `crates/htlk-runtime/tests/binding.rs`. |
| Optional absence, present null, skipped/failed selected sources, zero-input false guards | `tests/runtime_conformance.rs`, `tests/runtime_runs.rs`, crate record/binding tests. |
| Guarded fallback, required unexported effects, final-next checks and loop bounds | `tests/runtime_execution.rs`, `tests/runtime_conformance.rs`, `tests/runtime_deadlines.rs`. |
| Standalone/modular equality, scope depth 3, expanded-node bound 34 | `tests/compiler.rs`; package pins, visibility, cycles, and inspection in compiler tests. |
| Decision-brief acceptance at review one/two/three, rejection fallback, schema/retrieval failure and stop | `tests/runtime_mcp.rs`; native HTTP/SSE standalone/modular cases in `tests/deep_e2e.rs`. |
| Current policy, labels, protected projection/absence, lazy unused reads | `tests/runtime_boundaries.rs`, `tests/runtime_influence.rs`, `tests/runtime_reply_labels.rs`. |
| Independent registration, exact profile, producer trust, immutable artifacts | `tests/runtime_registration.rs`, `tests/runtime_artifacts.rs`; crate verified/type tests and independent identity fixtures. |
| Duplicate replies, terminal no-op receipts, atomic denial and commit-time expiry | `tests/runtime_waits.rs`, `tests/runtime_alignment.rs`, `tests/runtime_deadlines.rs`. |
| Old approval generations, exact arguments and stable deduplication keys | `tests/runtime_mcp.rs`, `tests/runtime_delivery.rs`, native approvals in `tests/deep_e2e.rs`. |
| All MCP operation categories, strict hostile ingress, transport/session ownership | `tests/deep_e2e.rs`, `tests/mcp_wire_results.rs`, `tests/runtime_mcp_sessions.rs`, crate transport tests. |
| Extraction, independent assessments, protected errors, no-call inference, explicit adoption | `tests/runtime_mcp_results.rs`, `tests/runtime_mcp_data.rs`, `tests/mcp_content_library.rs`; crate extraction/normalization/inference tests. |
| Response-recorded recovery without adapter, unsafe intent ambiguity, preserved waits/loop feedback | `tests/runtime_recovery.rs`, process-kill checkpoints in `tests/runtime_crash.rs`. |
| Extension during a live call, unchanged identities, adopted reservations and atomic planning reply | `tests/runtime_extension.rs`, repeated joins in `tests/deep_e2e.rs`. |
| Empty/selected engine scope, unrelated overdue-run exclusion, restart attachment | `tests/runtime_scope.rs`, attached crash recovery in `tests/runtime_crash.rs`. |
| Protected logical/source/history views, causal diagnostics and synthetic joins | `tests/runtime_inspection.rs`, `tests/runtime_sources.rs`, `tests/runtime_alignment.rs`. |
| Saved audit promises, immutable tombstones, shared bytes, expired-ID nonrevival | `tests/runtime_retention.rs`. |
| Transaction rollback, fences, CAS/finality, incompatible schema rejection, bounded replay/outbox | SQLite unit tests in `crates/htlk-runtime/src/store*.rs`; integration replay and stress suites. |
| Concurrent host reply cannot create false invariant failure; stable impossible states still fail | Deterministic interleavings and deadline negative controls in `crates/htlk-runtime/src/execution/step_tests.rs`. |

MCP HTTP authentication is qualified by
`cargo test -p htlk-runtime --test mcp_auth`. Its independent Node resource/AS peer
checks OAuth/OIDC discovery priority, S256, registration authentication methods,
callback/state/resource/issuer validation, refresh rotation and interruption, scope
step-up, trust/byte/time/loop bounds and secret isolation. Native client campaigns
refresh credentials on every POST/control/GET/resumption/DELETE, reject conflicting
static headers before send, retain structured acquisition/challenge concerns, and
prove that authentication and SSE resumption never automatically replay an
effectful POST. The test imports the exported production API directly.

## URL elicitation qualification

The focused URL elicitation campaigns are:

```console
cargo test -p htlk-runtime --test mcp_url_interactions --test mcp_url_live --test mcp_callbacks
```

They cover independent SQLite connections/reopen, owner/epoch fencing, competing
responders, callback-response/completion races, atomic URL-required lists, and exact
original error retention. The independent Node peer exercises stdio, HTTP JSON with
idle GET SSE, and coalesced POST SSE frames. Interactive and polling frontends use
the same authenticated APIs. Peer counters verify zero URL/metadata fetches and
zero automatic retries; events disclose opaque local identities only.

POST SSE qualification ends after the matching response and complete frames in its
current received chunk. `mcp_client_sessions` deliberately splits a trailing event
across that boundary and verifies that it is not reported as delivered. This is not
qualification of ongoing post-result stream reception; MCP recommends server stream
termination after the response. Idle GET reception is qualified independently.

## Experimental task qualification

```console
cargo test -p htlk-runtime --test mcp_tasks --test mcp_task_receiver
cargo test --test runtime_mcp_callbacks
```

These tests import the exported production APIs directly. Independent Node peers
exercise stdio, HTTP JSON with idle GET, and POST SSE. Receiver campaigns use one
execution worker while get/list/result/cancel progress, replay acceptance and simulate
lost HTTP acceptance without another model call. SQLite campaigns use independent
connections, inject a failed terminal write, race completion/cancellation, reopen
all connections, and verify original bytes and uncertain accounting independently.
Outbound campaigns retain intent/acceptance/final evidence across client restart and
prove one task creation after a lost ACK. Runtime campaigns compile task-required
tools and verify final normalization and real ancestor-accounted callback origins
after the create RPC ended, including HTTP callbacks on the idle GET stream.

Sampling qualification uses `cargo test -p htlk-runtime --test mcp_sampling --locked`
and `cargo test --test runtime_mcp_callbacks --locked`. Independent Node peers observe
multimodal tools/context, advisory hints versus actual routing, native child operations,
tool-result continuations and lost HTTP reply acceptance. SQLite reopen verifies
operation claims, original native bytes, known completion reuse and unknown-versus-zero
usage. Runtime campaigns check every real ancestor and sequential budget exhaustion;
an independent Node SQLite reader checks late native results after a dropped waiter.

## Native identity and fixtures

Native implementation identities hash explicit source contributions, package-owned
model/analyzer contributions, pinned dependencies, and the MCP schema asset. Review
the Rust manifests in `crates/htlk-executable/src/implementation.rs`,
`crates/htlk-analyzer/src/implementation.rs`, and
`crates/htlk-runtime/src/native_profile.rs` alongside the independent explicit lists
and hash framing in `tools/validate-executable-fixtures.mjs`.

Adding/removing/relocating a covered Rust module or changing its bytes, including
inline rustdoc, can change native identities. Markdown content included from an
unchanged Rust path is not itself an explicit source contribution. Never refresh a
golden just to conceal an unexplained mismatch.

After reviewing an intentional implementation change:

```console
cargo run -p htlk-runtime --example native_fixture --locked
```

Review the printed envelope/profile and update `native-empty.hex` from that output,
then run:

```console
node tools/validate-executable-fixtures.mjs
cargo test -p htlk-runtime --test verified --test identity --locked
```

Tests consume fixed goldens and must never regenerate them. The independent runtime
identity vectors remain fixed unless their specified preimages change. The fixture
[README](../crates/htlk-runtime/tests/fixtures/README.md) describes their formats.

Source package fixtures deliberately contain both files and captured bundle strings.
After intentionally editing module source, explicitly regenerate snapshots and exact pins:

```console
node specs/regenerate-module-example.mjs --write
node specs/validate-specs.mjs
cargo test --test compiler --locked
```

Without `--write`, the generator prints the proposed bundle. Tests compare disk
sources, manifests, bundle contents, digests, and compiled-byte equivalence. They
never silently repair drift.
