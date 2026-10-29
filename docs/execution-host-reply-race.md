# Execution / host-reply race verification

## Diagnosis

Investigated against `b68689bffff071460b2580f7aba212b26bd3ad5d` using the retained
consumer fixture, read-only, and a deterministic upstream regression.

- Run: `run:35ca70e2bef14e279a2adf0ebe325b90:1`.
- Executable: `sha256:a9edae8870f95ee2a0b4abb83a963b3ffdf5755620824be03e230cbd5e38a1cf`.
- Journal 145 opens `nlp.validate_review`; 146 accepts its reply and updates the
  invocation to `reply_received`; 147 stops the run with `E_INVARIANT` two
  milliseconds later.
- No invocation failed. Unfinished invocations were cancelled by the whole-run
  stop. Recorded lease and execution deadlines had not expired. SQLite integrity
  checking passed.

`execution/step.rs` checked the journal sequence before awaited, paginated wait
and invocation scans. A host reply could commit after that check, closing the
last open wait after execution had already examined its invocation. The final
decision combined stale scope progress with newer external-work observations and
incorrectly declared an invariant failure.

The journal does not capture individual reads. The synchronized regression proves
this interleaving produces the exact reported error under the original ordering.

## Fix and regression

Implementation commit: `eb6ea55f97e0fd5613034c56e32024c9a1d1cc9c`.

Revalidate the original sequence **after every external-work scan**, before either
publishing quiescence or declaring `E_INVARIANT`. A changed sequence invalidates
the observation and returns `Progress::Changed`. Stable no-progress states still
fail normally. This changes the consistency boundary, not retry or lease policy.

`crates/htlk-runtime/src/execution/step_tests.rs` drives a verified wait-to-eval
graph through real fenced SQLite transactions and the host's typed reply/receipt
construction. Private `cfg(test)` probes use `Notify` to force reply acceptance:

1. Before the wait scan, after execution has examined the open wait invocation.
2. Between the wait and invocation scans.
3. After both scans, immediately before final sequence validation.

The first interleaving fails deterministically with the old sequence-check
placement (`E_INVARIANT`); all three pass with the fix. No timing sleeps establish
the ordering; the timeout is only a test watchdog. Single-record pages exercise
the paginated observation boundary. Assertions cover current progress, absence
of an invalid quiescence publication, successful downstream output, immutable
accepted waits, addressed-reply idempotency/conflicts and journal reconstruction.

Negative controls retain stable open-wait blocking and stable impossible-state
invariant failure, including the actual whole-run failure transition. Fixed-clock
controls verify due node and whole-run deadlines still produce `E_TIMEOUT`, close
waits, and replay exactly.

Focused command:

```console
cargo test -p htlk-runtime --lib execution::step::tests --locked -- --nocapture
```

## Verification

All checks below passed locally on macOS, Rust 1.98.0, with independent local
transport peers and no live providers:

- Runtime package tests/doctest and focused facade wait, deadline, alignment,
  extension, recovery and competing-coordinator stress suites.
- `node specs/validate-specs.mjs`.
- `node tools/validate-executable-fixtures.mjs` after reviewing and updating the
  source-derived native fixture from the documented generator.
- `node --test tools/coverage.test.mjs` (10 tests).
- `cargo fmt --all --check`.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.
- `cargo test --workspace --all-features --locked`, including E2E, crash/restart,
  stress campaigns and doctests.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked`.
- Fresh-target `cargo package --workspace --locked`, verifying all five crates.
- `actionlint`, `cargo deny check`, and byte-for-byte third-party notice comparison
  using `cargo about generate about.hbs --workspace --locked --fail --output-file`.
- Complete `cargo-llvm-cov` 0.9.1 workspace/all-features/all-targets suites with
  Rust 1.98.0 (lines) and nightly-2026-10-20 (`--branch`). Both unchanged per-crate
  ratchets pass: runtime line coverage 91.44% (1,638 uncovered), branch 73.68%.
  The initial line run exposed timing-sensitive deadline coverage; fixed-clock
  deadline controls made those paths deterministic without changing thresholds.

Local machine-readable coverage reports are
`target/execution-race-line-coverage.json` and
`target/execution-race-branch-coverage.json`.

## Consumer consequences and evidence preservation

Public APIs, Cargo dependencies/lockfile, SQLite schema version (7), journal and
record formats are unchanged. Native profile identity changes because runtime
source is pinned; compile/register new executables under the updated pin. No
compatibility bypass, store migration, effect redispatch or retained-store reset
was introduced.

The consumer source and stores were not modified. SHA-256 hashes of the retained
files in `.tmpk4KZgl` remained unchanged:

| File | SHA-256 |
| --- | --- |
| `runtime.sqlite` | `9ee0afab3963b924ee11279920d59609e8369df32369a020798a66e28e320912` |
| `builds.sqlite` | `0c8f6899273ec2878c9ab2d3d3198c86f6ddcf76ee4bc843ec203591ad9fd124` |
| `fixture-config.json` | `998a48e6a867a6d728a38b6b87fab5d19668393c9dcff79b7bf56ecb18074eff` |
