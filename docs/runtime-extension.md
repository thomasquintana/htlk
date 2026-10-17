# Additive live extension

Authority: runtime §12. The host compiles new work, uses `Compiler::join_graphs`
with retained pinned catalog snapshots, registers the composed executable, and calls
`Runtime::extend_run(context, run_id, request, idempotency_key)`.

`ExtensionRequest` contains the expected active fingerprint, registered composed
fingerprint, public input map and optional `CloseWait`. Extension authorization is
distinct from graph registration. Historical identical installation receipts remain
available after later extensions or completion; changed requests under the same key
conflict. A stale expected fingerprint yields `E_EXTENSION_CONFLICT`.

## Checked shape and unchanged work

The runtime independently re-admits both executables and requires identical profiles
and the exact preserved definition closure. The new root must contain exactly two
ordinary scope nodes, literal-true root/wrapper contracts and guards, empty local
limits, and unique unconditional public-boundary bindings. Aliases have no special
meaning. Either side may be preserved, including joins with equal operand definitions;
the selected side must retain every old input value, label identity and optional
absence, without a new sibling-output prerequisite.

Public inputs can explicitly supply tagged inline/artifact values. Inputs routed to
the preserved operand can inherit the original manifest under renamed composed ports.
New public values receive ordinary type/schema, integrity, protection, authorization
and aggregate representation checks. Source cannot introduce hard token/cost limits
that the adapter cannot enforce. New ancestry cannot narrow already-admitted ceilings.

The transaction allocates the one-based extended-root ID, creates fresh wrapper
invocations and a pending sibling scope, and mounts the original scope under its new
wrapper. The old scope keeps its ID, origin executable/use site, frozen input checkpoint,
deadlines, child invocations, attempts, approvals and accepted artifacts. It continues
under its original analysis; the old precondition is not re-evaluated. The fresh sibling
is admitted and opens its body only when its ordinary input requirements settle.

## Budgets and concurrent results

Original run-wide usage and deadline remain authoritative. The new root adopts all
spent usage and outstanding reservations; the preserved wrapper adopts its operand's
aggregate usage. The same transaction adds the new ancestors to each outstanding
reservation's release inventory. A later definitive response releases capacity at
every adopted ancestor, while spent calls stay spent.

Dispatch checks current ancestry and invocation revisions in its intent transaction.
It therefore conflicts with cancellation or a concurrent mount computed from a stale
snapshot. An already-sent attempt keeps its identity and can still submit its fenced
response after installation. Scope finalization and installation compare revisions,
so a stale old-root completion cannot finalize the newly extended run.

## Atomic planning windows

`CloseWait { wait_id, value, idempotency_key }` validates the addressed input wait in
the same run using the normal authenticated reply path. Purpose, type, authorization,
deadline and receipt checks all apply. Installation, wait acceptance, both receipts,
journal and outbox commit together. Failure changes neither authoritative graph nor
wait. The reply remains a proposal through the original node postcondition and never
confers approval authority.

The host deliberately keeps work nonterminal by putting a typed planning wait on a
required completion path, as shown in runtime §12.1. Compilation failures leave that
window open until its existing deadline.

## Verification

`tests/runtime_extension.rs` uses actual compiler joins for either preserved side,
renamed ports, optional absence, explicit same-value inputs, invalid changed values/
presence, compiler-valid nonadditive structure, invalid atomic replies, repeated joins,
stale expectations and historical receipts. A held real actor dispatch verifies that
installation preserves its attempt ID, adopts its spent call/reserved concurrency and
releases all five accounting scopes after the response without a second dispatch.
