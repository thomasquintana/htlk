# Durable graph execution

Authority: runtime §§4–6 and §§8–10. `ExecutionEngine::launch` starts an explicit
Actix supervisor over an `Arc<Runtime>` inside the application's Actix system.
Admission remains available without workers. The supervisor scans running runs
with bounded cursors, acquires fenced per-run ownership and resumes checkpoints.

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

Execution also integrates [durable waits and host control](runtime-waits.md) and
[MCP dispatch, reservations and exact-action approvals](runtime-mcp.md). The roadmap
tracks remaining recovery, extension, retention and full conformance work.

Five compiler-to-actor tests exercise pure chains/templates, scoped passthrough,
loop checkpoints/bounds/final-next validation, late conditional writer conflict
before admission and successful fallback after terminal child failure.
