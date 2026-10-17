# Runtime run admission and host receipts

Authority: [runtime §§3, 9, 13–15](../specs/runtime-spec.md).
`Runtime::start_run` creates durable checked root state. Graph scheduling and child
execution are the next coordinator phase; registration still executes no nodes.

## API and authenticated context

The Rust API takes authenticated `HostContext`, a registered fingerprint, an
identifier-keyed `InputValue` map, a required idempotency key and optional
`ExecutionLimits`. Inline values and accepted artifact references are explicit
variants; a JSON object that resembles a handle remains ordinary inline data.
The principal never comes from an input named `principal` or an approval field.

Current `Authorization::StartRun` is checked before any receipt is returned. Host
policy may supply additional principal ceilings through `execution_limits`.
Omission inherits and grants no effects. Actual effect authorization is rechecked
at dispatch when MCP coordination is implemented.

## Request identity and retention

Host receipts are scoped by `(principal, 'start_run', fingerprint, key)`. The
semantic arguments are the complete tagged input map and normalized requested
limits. Empty requested limits and omitted limits have the same meaning. The
private `htlk.host_request` digest additionally covers principal, method and target;
the lookup key itself is not a semantic argument.

An identical request returns its existing `RunStarted` even if the old executable's
native implementation or input artifact bytes are no longer available. Returning
a historical run ID grants no value access. A changed request under the same key
conflicts before new input/graph evaluation. Different principals or fingerprints
have different key scopes. Authorization is checked again on every retry.

`RuntimeConfig::idempotency_retention_ms` is positive and finite (24 hours by
default); `Runtime::config` exposes it for transport advertisement. After expiry,
a key can identify new work. Input/output artifact retention is a separate policy.

## Durable host identity and time

Migration 4 creates a persistent database namespace using SQLite `randomblob` and
a monotonically increasing run counter. Reserved IDs are never reused when run
creation fails; gaps are harmless. IDs remain opaque to callers. Artifact and
invocation identities use their exact strings under the specified hash domains.

`CoordinatorClock` supplies trusted observed epoch milliseconds outside the pure
evaluator. `SystemClock` is the default; `with_clock` supports controlled host/test
clocks. Storage normalizes observations against both a durable global watermark
and known committed run timestamps. A backwards wall-clock observation therefore
cannot revive an expired receipt or rewind accepted coordinator time.

## New-request admission

After ruling out an existing receipt:

1. Independently load/admit the registered executable under actual native code.
2. Reject unknown host input names and any missing required port.
3. Check requested, deployment, principal, policy, definition and node hard-budget
   requirements. Token/cost enforcement is currently unsupported and rejected with
   `E_BUDGET_UNSUPPORTED`, including explicit zero, before creating graph work.
4. Validate every present value against its destination type/schema without
   coercion. Accepted references are authorized and checked for unchanged content.
5. Assign trusted input/boundary labels. `input_labels(..., Option<&Value>)` exposes
   absence explicitly so policy can protect missing-input decisions too.
6. Bound aggregate materialized native inputs and label metadata. Per-request
   artifact snapshots share immutable storage; per-port validation remains explicit.
7. Reserve a unique run ID, prepare inline-input artifacts and freeze complete
   `InputManifest` entries. Artifact references retain their existing IDs/types.
8. Evaluate the root precondition with the same checked evaluator and immutable
   schemas/library registry, through actual-read influence/boundary authorization.
   No child work or MCP request is started here.
9. Stage checked immutable inline bytes; staging alone accepts no value.
10. In one transaction, recheck host idempotency and accepted input references,
    choose authoritative time, create run/root/input records, initialize usage and
    finite timers, append journal/outbox entries and save the host receipt.

Concurrent identical requests can perform validation but create only one run.
Losing proposals publish no metadata; unreferenced staged bytes can be collected.
Invalid host input creates no run. Auxiliary clock/ID reservations are infrastructure
facts rather than graph work.

Input labels are immutable shared sorted sets. Combining many sets charges set
headers even when empty, bounds copying and sorts once; it does not repeatedly
copy a growing influence union. Present artifact labels and additional boundary
labels both enter the frozen input influence hash. A reference is never relabeled
or rewritten to remove its original protection.

## Root contracts and limits

A valid run whose root precondition is false is created failed with
`E_PRECONDITIONS`; expression errors retain their own classification and canonical
contract location. Its input inventory remains durable, no child invocation is
instantiated and no ordinary root output is published. Root input schema violations
remain host errors rather than failed graph executions.

Run-wide ceilings and definition-local root ceilings are recorded separately.
Current normalization takes tightest configured/profile/run/principal/definition
values. Both run-wide and scope-relative finite deadlines are persisted at admission;
timers themselves are consumed by the forthcoming coordinator. A definition-local
limit must not become a replenished or incorrectly tightened run-wide limit during
future live extension.

## Checkpoints and journal

Initial root scope metadata retains definition digest, origin executable,
`scope_use=['root']`, authenticated principal, logical inputs, versioned input
checkpoint/hash, influence labels, contract evidence/site, active/failed status,
effective/requested limits, admission/deadline times, original run-context pointer
and an uncommitted output boundary. Inline artifact metadata receives the actual
transaction's authoritative acceptance time; content/provenance IDs stay unchanged.

The existing `run_created` event is followed in the same transaction by
`run_initialized` with payload:

```text
[
  [initial_record_writes, failed_status_or_null, null, initial_fingerprint],
  [principal, 'start_run', fingerprint, key, request_digest, expires_at_ms]
]
```

Initial records use the storage write schema and canonical family/key order.
The event reconstructs root/input/usage/timer records and the authenticated start
receipt. `ReplayState` checks initialization ordering, allowed families, root
identity/status, actor/target/key/expiry and ordinary revision invariants without
calling MCP or requiring a clock.

## Verification

Compiler-driven tests cover explicit absence/null, protected boundary aliases,
invalid inputs/no work, false/error root contracts, unsupported budgets, concurrent
idempotency, principal scopes, changed requests, historical receipts without old
native code, retention expiry and backwards clocks. SQLite tests inject host receipt
failure and assert rollback of run/scope/timers/journal/outbox as a unit; they also
verify allocator uniqueness, committed-time normalization and replay equivalence.
