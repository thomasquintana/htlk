# Durable graph execution

Authority: runtime §§4–6 and §§8–10. `ExecutionEngine::launch` starts an explicit
Actix supervisor over an `Arc<Runtime>` inside the application's Actix system.
Admission remains available without workers. The supervisor pages its eligible
runs, acquires fenced per-run ownership and resumes checkpoints.

## Host-selected run scope

`ExecutionConfig.scope` selects intentional store-wide `ExecutionScope::All`
(the default) or explicit `ExecutionScope::Attached`. The latter starts with an
empty set and performs no background run work until the host calls
`engine.attach_run(&run_id).await`. Admission, inspection and routed host commands
never widen that selection. The host can acquire its own request/budget ownership
before attaching the exact durable admission.

Attachment validates an existing ID, is idempotent and grants no graph/policy
authority. `max_attached_runs` bounds the additive lifetime selection to 1..4096
IDs, independently of `max_runs` concurrent coordinators. Already terminal IDs add
no entries; previously attached IDs remain in the set after completion. A new
nonterminal attachment beyond the bound returns `E_BUDGET`; unknown IDs fail
without attaching or modifying a run. To choose a different selection, shut down
and launch a new engine. There is no implicit detach/cancel/retry.

Explicit-scope polling pages only selected IDs. Unattached runs are excluded before
lease acquisition, recovery, deadline/wait expiry, pure work and MCP dispatch.
Their headers and journals remain unchanged even when overdue. A host command for
an unattached nonterminal run has no coordinator and returns `E_FENCED`; it does not
attach the run. Existing immutable receipt lookup remains read-only. Explicit
terminal host operations retain their existing authenticated receipt behavior.

Attachment does not stop an admitted clock: selecting an overdue run enforces its
original absolute deadline. Native fences and delivery-aware recovery remain in
force; selecting a saved run does not authorize repeating committed effects or
uncertain deliveries. Restarting in explicit mode requires attaching again.

`tests/runtime_scope.rs` covers empty startup, ready/overdue exclusion, selected
execution, unknown/duplicate/bounded attachment, dispatch-policy denial, small-page
fairness, host-command exclusion and exact wait/effect preservation after restart.
The killed-process campaign in `tests/runtime_crash.rs` recovers all four response,
unsafe-intent, input-wait and loop checkpoints through explicit attachment. It
asserts unchanged headers before selection, retained native fences/replay and no
replacement provider dispatch.

Run-scope checkpoint verification:

- Formatting, strict all-target/all-feature workspace Clippy, complete locked
  workspace tests and warnings-denied Rustdoc passed.
- Specification validation, the independent CBOR/JCS/executable-identity checker,
  ten coverage-validator tests and four offline live-controller-helper tests passed.
  The native empty-graph golden was deliberately reviewed after the source-bound
  core identity changed; its new fingerprint is
  `sha256:776150caf58bc884dca6197d9f21bef62181e1480d289c5114fba83b36051289`.
- Complete all-target instrumented suites passed both unchanged coverage gates:
  Rust1.98 runtime line coverage91.47% (1637 uncovered, limit1638);
  nightly2026-10-20 runtime branch coverage74.09%. Reports remain local under
  `target/coverage/run-scope-{line,branch}.json`. No threshold was relaxed.
- All five workspace packages were verified in a fresh isolated target directory
  with `cargo package --workspace --locked --allow-dirty` before committing.

These are credential-free fixture and build checks, not new live-provider
acceptance claims. Consumer adoption must select runs explicitly and use the
resulting source-bound native profile; old effects are not reexecuted to adopt it.

Calculation workers execute one durable transition at a time per run. Actor
mailboxes remain responsive while calculation workers run; separate deadline
checks and lease renewal do not wait for a pure calculation to finish. Results
commit against expected revisions, current owner/fence and immutable outcomes.

## Scheduling and value access

Scopes instantiate deterministic pending invocations once. Node and edge guards
are evaluated using the admitted use/site plan and committed before dependent
admission. Settled guard errors survive pending sibling guards and restart.
All input selection settles before error/required-absence precedence. Types and
preconditions precede one-time manifest admission; active node checkpoints retain
input hashes, origin and definition digests, relative/ancestor deadlines and limits.

Worker frames lazily load only admitted immutable artifact references. Authorization,
integrity, source labels and boundary protection are preserved through the existing
checked evaluator and InfluenceContext. An unused lazy branch neither loads its
value nor fails because that value is unavailable. Pure functions receive values,
not ambient storage or host credentials.

Pure nodes validate complete output inventories and postconditions before metadata,
outputs and success publish atomically. Optional absence is explicit. Scope results
alias accepted artifacts; containing task/loop invocations publish their own typed
artifacts with input and child lineage. A scope waits for every child to settle,
allows outcome-based fallback, and uses binding/type, required-output, then contract
error precedence. Failed children do not automatically fail the containing scope.

## Tasks and loops

Task definition contracts and local wrapper contracts both apply. Definition
preconditions run before body creation. Iterations use deterministic IDs and only
committed next manifests for feedback. Both next and output boundaries validate
before until, including required next values on the final iteration. A true final
predicate succeeds; false at the exact bound yields E_LOOP_LIMIT. Body checkpoints
and transitions survive restart without repeating committed iterations.

## Operational boundary

Execution configuration bounds worker/run counts, polling and ownership leases.
Deadline stops fence acceptance and stop descendant scopes/nodes before finalizing
their enclosing outcome. Shutdown stops local actors while preserving durable state;
takeover advances fencing after leases expire. Notification delivery is a hint;
authoritative records drive progress.

The calculation path and asynchronous deadline controller use the same recursive
stop transaction. A stale result conflicting with a terminal child causes a fresh
reducer pass under the existing owner; it does not abandon a live run or delay a
valid fallback until ownership expiry. Approval-wait expiry uses its distinct
denial classification and closes the outstanding request atomically.

Execution also integrates [durable waits and host control](runtime-waits.md) and
[MCP dispatch, reservations and exact-action approvals](runtime-mcp.md). The roadmap
tracks remaining recovery, extension, retention and full conformance work.

Five compiler-to-actor tests exercise pure chains/templates, scoped passthrough,
loop checkpoints/bounds/final-next validation, late conditional writer conflict
before admission and successful fallback after terminal child failure.
