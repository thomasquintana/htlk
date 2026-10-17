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
   timer/reservation/host receipts pending. [Storage boundary](runtime-storage.md).
3. **Pending — Registration and runs:** immutable exact-byte registrations, trust,
   artifact ingress, input validation and atomic run creation.
4. **Pending — Pure graph execution:** deterministic binding reduction, guard-first
   admission, checked contexts, influence labels and atomic output publication.
5. **Pending — Scopes and loops:** complete child settlement, conjunctive contracts,
   output precedence, explicit feedback and durable iteration checkpoints.
6. **Pending — Policy and limits:** authorization, effective deadlines, reservations,
   supported hard budgets and fenced cancellation.
7. **Pending — Waits and approvals:** durable requests, authenticated replies,
   exact-action approval generations, expiry and idempotent receipts.
8. **Pending — Live MCP:** stdio/HTTP, descriptor drift, bounded protocol conversion,
   durable attempts/responses and delivery-state-aware retry.
9. **Pending — Recovery and replay:** higher-fence takeover, state reconstruction,
   ambiguous-effect reconciliation and replay without external dispatch.
10. **Pending — Additive extension:** compiler joins, exact preserved inputs/identity,
    scope mounting and atomic planning-wait closure.
11. **Pending — Operational API:** authorized inspection, source sidecars, retention,
    backpressure, startup/shutdown and host examples.
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
