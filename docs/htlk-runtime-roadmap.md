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
2. **Implemented — Durable authority:** SQLite identity/migration, WAL/FULL settings,
   atomic run creation and fenced coordinator acquisition/renewal implemented.
   Atomic revisioned/final record transitions, internal command receipts, bounded
   journal/outbox delivery, capability-free reconstruction, host receipts, persisted
   deadlines and delivery-aware reservations. [Storage boundary](runtime-storage.md).
3. **Implemented — Registration and runs:** immutable exact-byte graph registration,
   explicit producer/authorization policy, detached provenance, bounded admitted
   cache and independent reload implemented. Typed artifact proposals, immutable
   byte staging, atomic metadata/output crosschecks and authorized integrity-checked
   reads implemented. Authenticated/idempotent start, input admission, root contracts,
   durable identity/time, initial scope/usage/timers and atomic host receipts implemented.
   [Registration API](runtime-registration.md) · [Artifacts](runtime-artifacts.md).
   [Run admission](runtime-runs.md). Boundary/cancellation/dispatch enforcement
   integrates with the execution phases below.
4. **Implemented — Pure graph execution:** deterministic binding selection and
   guard-first input settlement implemented with an independent state oracle.
   Actual-read influence/boundary authorization wraps checked contexts and root
   contracts. Fenced actor execution and atomic pure publication implemented.
   [Execution](runtime-execution.md) · [Reducers](runtime-binding.md) · [Influence](runtime-influence.md).
5. **Implemented — Scopes and loops:** complete child settlement, conjunctive contracts,
   output precedence, explicit feedback and durable iteration checkpoints.
6. **Implemented — Policy and limits:** lazy/current exact-parameter authorization,
   effective deadlines, ancestor accounting, fenced/cooperative cancellation and
   irreversible spent usage with ambiguity retention.
   Unsupported hard external token/cost enforcement is rejected before admission.
7. **Implemented — Waits and approvals:** durable input requests, authenticated replies,
    expiry, host receipts, cancellation and exact-action approval generations
    implemented, with typed/trusted routing, disposition classification, expiry/races
    and historical receipts.
    [Wait/control API](runtime-waits.md) · [MCP/approvals](runtime-mcp.md).
8. **Implemented — Live MCP:** bounded stdio/HTTP JSON/SSE, live session/descriptor
   checks before intent, all four operations, delivery-aware persistence/retry,
   verified dedup, bounded cooperative stop and non-authoritative late evidence.
9. **Implemented — Recovery and replay:** exact-origin/checkpoint/artifact verification,
    producer-index reconstruction, higher-fence takeover, delivery-state reconciliation,
    saved loop/wait resumption, explicit fatal/invariant outcomes and capability-free
    journal reconstruction. [Recovery boundary](runtime-recovery.md).
10. **Implemented — Additive extension:** compiler joins, exact preserved inputs and
     identity, either operand/renamed interfaces, repeated mounting, original budget/
     reservation adoption and atomic planning-wait closure. [Extension API](runtime-extension.md).
11. **Implemented — Operational API:** protected logical manifests/decisions/causes/
     attempts/hierarchy and bounded history, detached source origins/descriptions with
     preserved join paths/synthetic markers, retention, finite execution/cancel bounds
     and a runnable compile/register/planning/approval host example.
     [Inspection/source API](runtime-inspection.md) · [Retention](runtime-retention.md).
12. **Complete — Native conformance and validation:** [runtime §17 evidence matrix](runtime-conformance.md),
    compiled worked-example execution/replay, boundary and crash/race coverage, and
    final workspace/release checks verified. The supported native boundary is explicit
    in the conformance document.

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

Final verification (2026-10-17): spec and independent fixture checks; workspace
format/Clippy/tests/doctests; warning-free workspace rustdoc; `cargo deny check`;
regenerated notices; the runnable host example; and all five workspace crates
packaged and verified together using a fresh external target directory. Native
fixture fingerprint: `sha256:c826cceee971af190f37d9783bdd7d708beaf2e9453e93e71525ec15df31d386`.

## Completion review

The implementation plan was completed through `61d5ace` and pushed to origin.
A follow-up review on 2026-10-18 checked every task and historical follow-up against
the final completion evidence and the conformance matrix. The five actor conformance,
seven MCP and three recovery integration tests passed again, alongside compiler
interoperability and the spec/independent fixture checkers. No implementation-plan
tasks remain within the documented native boundary; the temporary local plan has
been removed, with this roadmap, the conformance matrix and commit history retaining
the delivery record.
