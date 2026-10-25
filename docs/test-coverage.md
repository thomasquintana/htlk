# Coverage and deep end-to-end testing

## Goals and measurement

The goal is 100% meaningful coverage, with 99% production line coverage per crate
as the near-term target. **The target has not yet been reached.** CI enforces the
measured per-crate floors and uncovered-line budgets in
[`coverage-thresholds.json`](../tools/coverage-thresholds.json); raise these
ratchets as additional gaps are closed. A large well-covered crate cannot conceal
a regression in another crate.

The primary measurement uses Rust **1.98.0** and `cargo-llvm-cov` **0.9.1**.
The separate branch measurement uses **nightly-2026-10-20**, since Rust branch
instrumentation is experimental. Its floors live in
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

The latest verified Rust 1.98.0 snapshot, including run-scoped MCP integration,
is compared with the pre-expansion baseline below:

| Crate | Baseline lines | Current lines | Uncovered lines |
| --- | --- | --- | --- |
| `htlk-analyzer` | 89.33% | 91.61% | 506 |
| `htlk-compiler` | 86.89% | 94.82% | 193 |
| `htlk-executable` | 95.46% | 98.14% | 100 |
| `htlk-runtime` | 87.86% | 90.55% | 1,619 |
| Workspace | 89.30% | 92.51% | 2,418 |

The initial deep-test expansion exercised 1,017 previously uncovered lines. The
runtime ratchet allows 1,638 uncovered lines, a 19-line margin above the latest
measurement for alternate legal race outcomes across local and clean Linux runs.
Pure-crate uncovered-line budgets match the measured counts. The latest nightly
branch measurement is 80.72% for the analyzer, 87.32% for the compiler, 95.85% for
executable records and 70.61% for the runtime. Its runtime floor likewise allows
a small race margin.

Download the `coverage-line-<SHA>` and `coverage-branch-<SHA>` CI artifacts for
current exact counts, machine-readable JSON, LCOV and annotated HTML. Open
`html/index.html` for the source view. Per-crate percentages and uncovered-line
counts are also written to the GitHub Actions job summary.

## Running locally

Install Node.js 24 for the independent local MCP peers. These tests use loopback
ports, local child processes and isolated temporary SQLite databases; they require
no service credentials or remote MCP server.

The complete ordinary suite, including every E2E, crash and stress campaign, is:

```console
cargo test --workspace --all-features --locked
node --test tools/coverage.test.mjs
```

To reproduce the primary CI coverage measurement:

```console
rustup toolchain install 1.98.0 --profile minimal --component llvm-tools-preview
cargo +stable install cargo-llvm-cov --version 0.9.1 --locked
mkdir -p target/coverage
cargo +1.98.0 llvm-cov --workspace --all-features --all-targets --locked --json --output-path target/coverage/coverage.json
cargo +1.98.0 llvm-cov report --workspace --html --output-dir target/coverage
node tools/coverage.mjs target/coverage/coverage.json --check tools/coverage-thresholds.json
```

For branch coverage:

```console
rustup toolchain install nightly-2026-10-20 --profile minimal --component llvm-tools-preview
mkdir -p target/coverage
cargo +nightly-2026-10-20 llvm-cov --workspace --all-features --all-targets --locked --branch --json --output-path target/coverage/coverage.json
node tools/coverage.mjs target/coverage/coverage.json --check tools/coverage-branch-thresholds.json
```

Use separate `CARGO_LLVM_COV_TARGET_DIR` directories if keeping stable and nightly
profiles simultaneously. The default coverage command clears old profiles.
`cargo llvm-cov report` exports additional formats from the same run without
executing tests again. Include `--workspace` when exporting reports.

Doctests run in the ordinary workspace suite. The stable coverage collector's
`--all-targets` measurement covers unit, integration and example test targets;
it does not instrument doctests.

## Deep scenarios

[`tests/deep_e2e.rs`](../tests/deep_e2e.rs) runs **110 compiled-graph scenarios**:

| Campaign | Evidence |
| --- | --- |
| All four MCP operation categories | Source compilation, native admission, durable SQLite, Actix workers and the native adapter over stdio, HTTP JSON and HTTP SSE. Includes paginated discovery, Unicode, present null, normalized binary resource contents, prompt data and protected artifacts. |
| Hostile peers | Identity/descriptor drift, missing operations, repeated pagination cursors, duplicate JSON members, wrong response IDs, correlated RPC errors, explicit tool errors and output-schema violations. Asserts dispatch counts, delivery-aware classifications, unpublished outputs and journal reconstruction. |
| Independent branch oracle | Every four-node activation mask under one, two and four workers. Checks each present/absent public output and full replay. |
| Worked decision brief | Both standalone and modular source, acceptance at review one/two/three and all-rejected fallback, over native HTTP JSON and SSE. Checks exact 4/6/8-call bounds and output values. |
| Repeated joins | Executes one through four preserved operand implementations, checking nested output transformations, scope counts and replay. |
| Exact-action approvals | Authenticated allow/deny, wrong request/kind, duplicate and conflicting disposition keys, terminal history, zero dispatch before approval and immutable disposition labels over every transport. |
| Native cancellation | Observes the actual in-flight request and protocol cancellation notification, preserves spent/ambiguous accounting and replays the terminal result. |

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
[capability spike report](mcp-capability-spike.md) documents the transport matrix
and opt-in live-server runner.

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
43 scope/invocation/wait/accounting and recorded MCP request/response corruptions.
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

These supplement the existing [runtime conformance matrix](runtime-conformance.md),
exhaustive binding/cycle oracles, codec vectors and controlled-stack tests.

## Remaining work toward 99%

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

The coverage gate rejects missing/malformed/duplicate core measurements, new
unconfigured production crates, inadequate per-crate percentages, excess uncovered
lines and absent branch instrumentation. Its own Node tests also exercise CLI
failure statuses and the facade's transition from reexports to executable code.

## Verification record

The expansion passes workspace tests/doctests, both instrumented coverage suites,
per-crate gates, formatting, warning-free Clippy and rustdoc, specification and
independent fixture validation, workflow linting, and fresh-target verified
packaging of all five crates.

The dependency-policy check also passes after updating the TLS dependencies for
[RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382)
(`rustls-webpki` 0.103.16) and
[RUSTSEC-2026-0383](https://rustsec.org/advisories/RUSTSEC-2026-0383)
(`webpki-root-certs` 1.0.10).
