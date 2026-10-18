# Draft 0.1 native runtime conformance evidence

Authority: [runtime §17](../specs/runtime-spec.md#17-required-conformance-scenarios). Tests use
real compiler-produced envelopes, independent native admission, SQLite authority and
Actix workers. Handcrafted records are confined to adversarial storage/crash checkpoints;
the reducer oracle and Node fixture checker are independent implementations.

## Required scenario matrix

| Runtime §17 scenario | Executable evidence |
|---|---|
| Late second true guard | `tests/runtime_execution.rs::late_conditional_writer_conflicts_before_consumer_admission`; independent three-candidate oracle and late guard tests in `crates/htlk-runtime/tests/binding.rs`. |
| Optional input with no active candidate | `tests/runtime_conformance.rs::optional_absence_skipped_sources_and_zero_input_false_guards_do_not_dispatch`; run/record absence checkpoints in `runtime_runs.rs` and `tests/records.rs`. |
| Present nullable null across MCP/CBOR | `runtime_conformance.rs::present_null_remains_a_value_through_mcp_projection_and_cbor` asserts a present argument, structured result, projected null and CBOR `f6`. |
| Sole selected failed source, including optional destination | Binding source-state/oracle and admission precedence tests; `runtime_inspection.rs` verifies actual dependency failure with original protected producer cause. |
| Sole selected skipped source | Actor conformance verifies required consumer skip and required scope `E_OUTPUT_MISSING`; optional absence remains successful. |
| False zero-input effect guard | Actor conformance uses a zero-input pinned MCP resource with a false guard and asserts zero discovery and zero invocation. Operation names/kinds do not confer replay/effect safety. |
| Primary failure with guarded fallback | `runtime_execution.rs::terminal_failure_fallback_can_succeed_without_propagating_every_failed_child`; deadline/late-writer fallback in `runtime_deadlines.rs`. |
| Unexported required side effect | Actor conformance runs the same failed tool under explicit success/failure scope contracts; only the declared postcondition determines required completion. |
| Crash after response persistence | Actual file reopen in `runtime_recovery.rs` finishes a recorded response with **no MCP adapter** and compares reconstructed header/record families. |
| Crash after unsafe intent | `runtime_mcp.rs::owner_takeover_never_replays_unsafe_possibly_sent_work_or_accepts_late_success` preserves one call and ambiguity reservation. |
| Duplicate acceptance/reply | SQLite CAS/finality/rollback/concurrent-owner tests; authenticated reply/history/key-conflict tests in `runtime_waits.rs` and terminal receipt replay. |
| Old approval after generation replacement | MCP approval-generation test changes policy during connection preparation, opens a new wait/digest, returns only old history, and spends zero calls before the new allow. |
| Proposed argument-key insertion | `runtime_delivery.rs::verified_protocol_keys_are_invocation_stable_and_authored_arguments_cannot_be_inserted` rejects a mismatched authored key before connection/send; protocol retry preserves one enforced server effect. |
| Several input failures in different orders | Independent exhaustive oracle and UTF-8 port precedence tests in `tests/binding.rs`; settled failures remain durable while another guard is pending. |
| Protected projection or scope transfer | Actor conformance projects a classified record through an ordinary task and denies the receiving artifact/run view; actual-read, lazy RHS and schema projection tests in `runtime_influence.rs`. |
| Last iteration satisfies until | Decision-brief scripted accept-at-three run succeeds with 8 calls; loop task-boundary tests and saved-loop file reopen verify final feedback/output checkpoints. |
| Pure loop never satisfies until | `runtime_execution.rs::pure_loop_exact_bound_fails_and_final_iteration_still_requires_next` checks exact bound and final required-next validation. |
| Extension during old MCP | `runtime_extension.rs` holds a live attempt, mounts it unchanged, adopts/releases all accounting ancestors, and verifies one stable attempt/call. |
| Install plus planning-wait closure | Compiler-join actor tests reject invalid type/changed value/presence/nonadditive proposals without changing graph or wait; valid install commits both and retains both receipts. |
| Terminal extension/reply | Extension tests preserve terminal result/history; wait tests preserve accepted history and reject late/conflicting mutations; terminal cancellation records a no-op receipt without reviving work. |
| Rebuild local indexes | SQLite producer-slot deletion/reconstruction leaves journal unchanged; file-reopened recovery and full decision-brief replay reconstruct identical bindings/outcomes without MCP. Readiness/timers derive from authoritative records. |

## Worked-example interoperability

`tests/compiler.rs` checks standalone versus modular decision-brief byte equality,
native admission and analysis bounds. `tests/runtime_mcp.rs` executes the compiled
example with acceptance on review one/two/three (4/6/8 calls), all rejected (8 calls
and `needs_attention`), retrieval tool errors, invalid structured output, empty question
and explicit stop. Each complete review scenario also reconstructs its full journal
in bounded pages, compares scope/invocation/guard/binding records and verifies no
additional MCP call. Generic response-recorded and loop-feedback file reopen cases
exercise the same durable engine paths used by this example.

## Other normative boundaries

- **Admission/identities:** exact policy/library/native implementation, source/bundle
  interoperability, owned thread-safe handles, complete bounded hash preimages,
  independent runtime identity vectors and native envelope/source hashing.
- **Authority/storage:** migrations through compatibility version 6, WAL/FULL/foreign
  keys, fenced takeover/renewal, record CAS/finality, duplicate notices/receipts,
  rollback and COMMIT-time fault injection, reconstruction and corruption rejection.
- **Values/artifacts:** strict recursive native types/schema/open fields, actual-read
  labels, complete manifests, immutable producer/content distinction, staged-byte
  invisibility and atomic complete publication, independent authorized reads.
- **MCP:** actual stdio/HTTP JSON/SSE negotiation/pagination, pinned identity/capability/
  drift, numeric/Unicode/duplicate/byte/depth checks before normalization, exact frozen
  parameters, all four operation results, explicit delivery and deterministic retry
  exclusions, stable verified dedup, per-returned-URI scope and data-only prompts.
- **Stops/policy:** current exact parameter grants before connection, rechecked
  approvals after connection, classified replies/dispositions, spent-call and ancestor
  concurrency accounting, ambiguity retention, bounded cooperative native/ignoring
  cancellation, descendant stops, terminal winners and safe shutdown.
- **Operations:** old scope/use/input/attempt preservation across repeated joins;
  protected bounded views/causes/history and detached source reports; saved retention
  promises, active/audited cross-run protection, tombstones, shared-byte collection
  and producer-ID nonreuse; public runnable planning/approval workflow.

## Supported native boundary

The native adapter supports stdio and Streamable HTTP MCP 2025-11-25. Host connection
credentials, actual principals, grants, classifications, approval routing and replay/
deduplication promises are trusted integration inputs. Descriptors and annotations
cannot attest an unchanged remote image or create universal MCP exactly-once effects.

External hard token/cost budgets are rejected with `E_BUDGET_UNSUPPORTED` before run
start or affected extension installation: this adapter supplies no trusted hard
meter/reservation bound. Compiler capability flags and usage estimates do not change
that fact. Compiler/native schema options must use the same supported default pairing.
Finite configured codec/evaluator/store/worker/deadline limits remain enforceable
implementation bounds. Cross-process mutating-command forwarding is an embedding
concern; unavailable local ownership returns retryable `E_FENCED`.

## Release checks

Final evidence is recorded in the tracked runtime roadmap and repository commit
history: spec/independent fixture checks, formatting, warning-free workspace Clippy,
all workspace tests/doctests, warning-free rustdoc, dependency/license policy and
fresh-target verified workspace packaging. Runtime behavior claims are established
by the tests above; syntax/spec checks alone are not conformance proof.
