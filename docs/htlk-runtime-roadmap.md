# Runtime implementation roadmap

The target is complete Draft 0.1 execution under
[the runtime specification](../specs/runtime-spec.md), with canonical records from
`htlk-executable`, semantic plans from `htlk-analyzer`, and executable bytes from
the implemented `htlk-compiler`.

## Existing foundation

- Exact native profile/policy/library admission through `verify_executable`.
- Checked pure evaluation, callbacks, schemas, projections and native value checks.
- MCP resource/prompt result checking and pinned URI-template expansion.
- Source/bundle compilation, module inspection and binary graph composition.
- Facade tests checking compiler/native admission and monolithic/modular equality.

## Delivery sequence

1. **Implemented — Runtime foundations:** shared-owned admission, compiler/native
   configuration integration, typed runtime/content/artifact identities, immutable
   input manifests, trusted labels, lifecycle vocabulary and stable failure records.
   [Private record/hash contracts](runtime-records.md) document their boundaries.
2. **In progress — Durable authority:** SQLite identity/migration, WAL/FULL settings,
   atomic run creation and fenced coordinator acquisition/renewal implemented.
   Atomic revisioned/final record transitions, internal command receipts, bounded
   journal/outbox delivery and capability-free reconstruction implemented. Consumer
    host receipts and dispatch reservations implemented; complete recovery/timer
    auditing remains. [Storage boundary](runtime-storage.md).
3. **In progress — Registration and runs:** immutable exact-byte graph registration,
   explicit producer/authorization policy, detached provenance, bounded admitted
   cache and independent reload implemented. Typed artifact proposals, immutable
   byte staging, atomic metadata/output crosschecks and authorized integrity-checked
   reads implemented. Authenticated/idempotent start, input admission, root contracts,
   durable identity/time, initial scope/usage/timers and atomic host receipts implemented.
   [Registration API](runtime-registration.md) · [Artifacts](runtime-artifacts.md).
   [Run admission](runtime-runs.md). Full boundary/cancellation/dispatch enforcement
   is delivered with the execution phases below.
4. **In progress — Pure graph execution:** deterministic binding selection and
   guard-first input settlement implemented with an independent state oracle.
   Actual-read influence/boundary authorization wraps checked contexts and root
   contracts. Fenced actor execution and atomic pure publication implemented.
   [Execution](runtime-execution.md) · [Reducers](runtime-binding.md) · [Influence](runtime-influence.md).
5. **Implemented foundation — Scopes and loops:** complete child settlement, conjunctive contracts,
   output precedence, explicit feedback and durable iteration checkpoints.
6. **In progress — Policy and limits:** lazy authorization, effective deadlines,
   ancestor call/concurrency accounting and fenced cancellation implemented.
   Unsupported hard external token/cost enforcement is rejected before admission.
7. **In progress — Waits and approvals:** durable input requests, authenticated replies,
    expiry, host receipts, cancellation and exact-action approval generations
    implemented. Complete expiry/race audit remains.
    [Wait/control API](runtime-waits.md) · [MCP/approvals](runtime-mcp.md).
8. **Implemented foundation — Live MCP:** bounded stdio/HTTP JSON/SSE, exact descriptor
   drift checks, all four operations, durable intent/responses, replay-safe retry,
   verified deduplication contracts and non-authoritative late-response retention.
9. **Pending — Recovery and replay:** higher-fence takeover, state reconstruction,
   ambiguous-effect reconciliation and replay without external dispatch.
10. **Implemented — Additive extension:** compiler joins, exact preserved inputs and
     identity, either operand/renamed interfaces, repeated mounting, original budget/
     reservation adoption and atomic planning-wait closure. [Extension API](runtime-extension.md).
11. **Pending — Operational API:** authorized inspection, source sidecars, retention,
     backpressure, startup/shutdown and host examples.
     [Protected retention](runtime-retention.md), immutable tombstones and bounded
     shared-byte collection are implemented; remaining operational audit continues.
12. **Pending — Conformance:** runtime §17 matrix, compiled worked-example execution,
    crash/race coverage and workspace/release validation.

## Invariants

- Runtime rechecks authoritative bytes; compiler reports never replace admission.
- Database transactions establish authority; workers only propose results.
- Definitions, uses, instances, attempts, content and provenance stay distinct.
- Pending, absence, null and unavailable sources have different meanings.
- A result becomes visible only with the complete accepted manifest and outcome.
- Label protection includes control influence, absence and scope boundaries.
- Replay never sends MCP requests; ambiguous effects require trusted replay policy.
- Extension preserves work and incurred usage, including in-flight identities.
- Actual-value and native-schema limits remain honest about backend capabilities.

## Verification

Extend `tests/compiler.rs` for source-to-execution interoperability. Compile source
fixtures from the first pure milestone, use real `join_graphs` results for extension,
and retain handcrafted records/bytes for adversarial boundary tests. Exercise the
standalone and modular decision-brief example under scripted MCP responses.

Each delivered slice requires focused semantic tests, updated public documentation
and applicable formatting/lint checks. Final acceptance includes runtime §17,
reopened SQLite/journal replay agreement, fixture/spec checkers, workspace tests,
Clippy, rustdoc, packaging and dependency policy.
