# Draft 0.1 specification alignment

The 2026-10-19 review traced the syntax/EBNF, language, compiler/CDDL, modules and
runtime requirements through the executable, analyzer, compiler and runtime crates.
The initial review identified fourteen implementation gaps. This document retains
their resolution and executable evidence independently of temporary work plans.

## Finding register

All findings below are implemented. Test paths are repository-relative; the runtime
alignment suite uses compiler-produced executables, real actors and SQLite authority.

| ID | Obligation and resolution | Regression evidence |
|---|---|---|
| F01 | Denial finalizes the invocation in the authenticated disposition transaction; policy replacement cannot revive it. | `tests/runtime_alignment.rs::denial_is_terminal_in_the_disposition_transaction_despite_policy_replacement` |
| F02 | Canonical and join admission identify embedded schemas by URI/digest pairs and recheck all external retrieval snapshots. | Compiler `canonical::canonical_schema_revalidation_distinguishes_retrieval_context_from_bytes` |
| F03 | Nested projection retains matching outer-variant eligibility; extra inner fields cannot reactivate rejected branches. | Runtime `runtime_type::nested_projection_cannot_reactivate_a_rejected_outer_variant`, direct and checked evaluation |
| F04 | MCP output validation preserves evaluator/engine failure classification; fuel exhaustion cannot become a schema-error retry. | `runtime_alignment::mcp_output_fuel_exhaustion_is_not_a_schema_retry` |
| F05 | SQLite acceptance checks wait expiry at commit after classification, including extension/window closure. | `runtime_alignment::classification_crossing_expiry_cannot_accept_a_reply_or_approval` and `mounted_waits_are_blocked_and_expired_atomic_closures_do_not_install` |
| F06 | Qualified contextual names are recognized before literal/unary syntax. | Compiler `canonical::contextual_node_names_resolve_as_references_in_conditions` and contextual aliases in `linked_generic_callbacks_use_exact_complete_manifests` |
| F07 | Typed bundle source cardinality is rejected before collecting/traversing extra source keys. | Compiler `canonical::typed_bundle_rejects_extra_sources_before_snapshot_processing` |
| F08 | Comparison admission uses available schema representation facts, including union members. | Analyzer `completion::comparisons_use_known_schema_representation_facts` and compiler whole-document regression in F02 |
| F09 | Blocked derives from an actual completed scheduler scan at an unchanged sequence before the earliest timer, including mounted scopes. | `runtime_alignment::blocked_requires_a_quiescent_scheduler_and_first_loop_iteration_is_visible` and mounted-wait test |
| F10 | Protected observations retain contract sites and secondary actual-value validation failures without rewriting graph-visible errors. | `runtime_alignment::inspection_retains_contract_sites_and_every_boundary_validation_failure` |
| F11 | Native discovery rejects task-required tools when the server advertises task-call capability, before operation dispatch. | Runtime `mcp_transport::native_task_required_tools_are_rejected_during_discovery` |
| F12 | HTTP session-bearing 404 never implicitly replays an operation. Run-scoped integration now fails the session without renewal to preserve handle semantics. | Runtime `mcp_transport::expired_http_sessions_fail_without_renewal_or_implicit_operation_replay` |
| F13 | Public port declarations require identifier tokens; retry members use their dedicated literal/list grammar. | Compiler `canonical::public_ports_and_retry_members_follow_their_closed_grammar` |
| F14 | Opening iteration zero persists the invocation iteration exposed by inspection. | First-loop-iteration test in `runtime_alignment` |

The existing runtime §17 scenario matrix remains in
[runtime-conformance.md](runtime-conformance.md). The new regressions complement
its reducer oracles, crash/replay tests and worked-example execution.

## Specification decisions

- **Extension inputs:** retain only inheritance explicitly defined by unconditional
  composed-interface edges to the preserved operand. Unknown inputs and missing
  unmapped required inputs fail; typed content, labels and optional absence remain
  unchanged. Runtime §§12/15 now describe the same mapping.
- **Native records:** required destination fields need declared source coverage.
  Open-record extra values do not authorize undeclared projection or structural
  narrowing. JSON/schema uncertainty remains explicitly runtime-checked.
- **Schema discovery:** closure is retrieval-root-driven. Nested resource IDs are
  resolved within reached containing documents, preserving resource pointers; an
  unreached catalog document is not discovered through its nested ID alone.
- **Inspection digests:** a scope digest hashes its scope/reference structure, not
  document-level schema retrieval bindings or the profile. The complete executable
  fingerprint binds that environment; hosts regenerate reports when it changes.
- **Source prompt parameters:** source uses required value-type declarations.
  Canonical presence metadata does not permit rendering absence.
- **Responsibilities:** report freshness and filesystem/symlink collection are host
  integration requirements; compiler APIs do not consume interface reports or
  discover files. Selected-tool `outputSchema` requirements do not apply to unused
  text-only catalog tools.

These decisions preserve the portable Draft 0.1 record shapes and hash domains.
Corrected native implementation sources change exact profile identities. Existing
executable bytes/history are never relabeled; recompile for the corrected profile.

## Additional review concerns

- **Response/extension CAS:** confirmed and fixed. Response persistence no longer
  shares compare-and-swap writes with reservation release. A later release conflict
  resumes from durable `response_recorded`, without another external call.
  `runtime_alignment::response_arriving_during_extension_is_durable_and_never_redispatched`
  races reply arrival with installation and verifies one call and released accounting.
- **Scope completion crossing expiry:** fresh checks now precede ordinary scope and
  iteration publication, and SQLite also rejects success at/after the persisted
  deadline even if time crossed after the worker check. A storage regression checks
  rollback at the exact deadline. The scope-boundary regression advances the clock
  during artifact authorization and verifies `E_TIMEOUT` with no published output.
- **Accumulated diagnostics:** compiler declaration checking now accounts for retained
  diagnostic bytes incrementally, deduplicates warnings before retention and charges
  lookup work. The finite report/work limits include intermediate diagnostic retention.
- **Dependency-only identity changes:** semantic model backends for JSON parsing,
  shortest-number formatting, URI parsing and hashing are exact-pinned and named in
  implementation contributions; analyzer contributions include serde identities.
  The independent fixture checker mirrors explicit backend/source coverage.
- **Native schema recursion:** isolated subprocess regression covers direct,
  applicator-mediated and mutually recursive non-consuming references without a
  crash/hang. Productive and dynamic recursion remain covered by existing native
  schema tests. Backend validation still makes no general hard fuel/deadline promise.
- **Module evidence:** additional source fixtures cover private/wrong-kind imports,
  namespace collisions, missing sources/modules, graph-bearing imports, source
  relocation with repinning, private-body changes and inspect-then-invalid-caller.

## Validation

Completed on 2026-10-19: all **544 tests/doctests** passed; specification and
independent fixture checks, formatting, warning-free Clippy and rustdoc passed.
All five workspace crates were packaged and verified together with `--allow-dirty`
in a fresh external target directory. `cargo deny check` passed advisories, bans,
licenses and sources; it reported the existing permitted duplicate-version warnings.

Reproduction commands:

```console
node specs/validate-specs.mjs
node tools/validate-executable-fixtures.mjs
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
```

Native source identity changes require refreshing the fixed fixture through the
documented generator and independently checking it; tests never regenerate it.
The corrected fixture fingerprint is
`sha256:a78c7bb2ff215a2fd55fa75f3fd3091e2c33fab8f28c5547227630934ad65472`.
Conformance is supported by the requirement-specific tests, not by syntax checks
or an unqualified percentage claim.

## Remaining specification implementation

No confirmed requirement from this review remains unimplemented. The supported
native boundary still rejects hard external token/cost budgets without trusted
enforcement and unsupported task-required MCP calls. General schema-validation
fuel, a rich standard library, filesystem collection, universal exactly-once
effects and a separate MCP task scheduler are not mandatory missing subsystems.
