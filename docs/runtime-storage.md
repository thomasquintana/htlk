# Runtime SQLite authority foundation

Authority: [runtime §§1.4 and 14](../specs/runtime-spec.md). `htlk_runtime::store`
is trusted coordination infrastructure, not an authenticated host service. The
caller must validate executable admission, principal, inputs and host idempotency.
Storage methods do not perform those application-level checks.

## Database identity and durability

The private SQLite database uses application ID `0x48544c4b` and schema version 6.
An empty database is initialized in an immediate transaction. Existing unrelated,
nonempty or incompatible-version databases are rejected without relabeling them.
File stores use WAL, synchronous FULL, foreign keys and a bounded busy timeout.
`in_memory` is ephemeral and is only suitable for tests or disposable execution.
Version 1 is upgraded transactionally by adding immutable graph and detached
provenance tables; existing authority/journal/outbox records are preserved.
Migration 3 adds immutable value bytes and producer uniqueness indexes; version 2
is upgraded without changing existing registered envelopes or execution records.
Migration 4 adds durable host identity/time and authenticated receipt tables, plus
a committed-time lookup index. Existing versions are upgraded transactionally.
Migration 5 adds a non-authoritative, protected late-response inbox.
Migration 6 marks the terminal receipt/tombstone and checked-recovery contracts;
its table layout is unchanged, but older readers must not ignore expiry semantics.

The initial layout has run authority headers, a version-0.1 journal, a transactional
outbox, revisioned runtime-record families and internal command receipts. Graph
definitions remain canonical executable records; runtime lookup indexes are derived.
Further storage operations and registration/artifact tables are delivered with
their runtime consumers rather than exposed as unrestricted SQL APIs.

## Run creation and ownership

`create_run` atomically creates an unowned running header, the first `run_created`
event and its outbox notice. `RunSeed` is a trusted service input; uniqueness at
this layer is the run ID. Principal/method/target host idempotency is a separate
service responsibility. Creation failures leave no partial run or notice.

`acquire` permits an unowned or expired run and increments its fence. The owner
name must identify one coordinator instance. A still-live owner cannot be replaced,
including by another request using the same owner string. `renew` keeps the same
fence, cannot shorten its expiry, and rejects expired or superseded authority.
Every ownership change updates the run header, journal and outbox atomically.

Times are nonnegative signed-i64 milliseconds supplied by the trusted coordinator.
An operation uses `max(observed_time, last_committed_time)`, so backwards observations
cannot rewind authoritative time. A lease is expired at its deadline, not after it.
Renewal/reacquisition requires an expiry later than the effective current time.
The host must supply a suitable trusted clock; these rules do not turn an untrusted
caller timestamp into authoritative clock access.

The database worker executes immediate transactions and checks current authority
inside them. Two opened connections therefore cannot both acquire the same live
run. Actor mailbox serialization alone is insufficient. Fencing does not undo an
already-sent external effect.

## Limits and privacy

`StoreConfig` bounds codec metadata, busy timeout and future journal/outbox page
sizes. Journal writes check the full private logical row, including attribution,
causation and encoded payload bytes. Configuration, input and backend failures
return structured `StoreError` categories without database paths, submitted payloads
or raw SQLite diagnostics in Display/Debug.

## Atomic record decisions

`commit(lease, transition)` is the trusted coordinator's compare-and-swap boundary.
Record keys are `(run, RecordFamily, opaque ID)`. An insertion expects no previous
revision and creates revision one; an update requires the exact positive prior
revision. A final record cannot change. The closed family enum covers scopes,
invocations, guards, bindings, manifests, attempts/responses, waits/replies/approvals,
reservations/usage, artifacts, cancellation, extension, loops, timers, observations
and tombstones. Each consumer validates its own typed payload before this boundary.

In one immediate transaction, storage checks the live owner/fence/expiry, command
identity, run status, optional expected fingerprint, every record revision/finality,
and terminal run transition. It writes the whole record set, optional active-root
replacement and run outcome, complete journal event, outbox hint and command
receipt together. Conflict or failure at any point rolls the entire decision back.
Finalizing one record is useful for immutable binding decisions as well as outcomes.

Internal command receipts are scoped by run and command key. The private semantic
digest hashes `['htlk.store_command', '0.1', actor, causation, run_id, body]`.
Timestamp and owner/fence are excluded, allowing a new fenced coordinator to retry
the exact persisted command after takeover. The command key is the receipt lookup,
not a semantic argument. Changed records, expected revisions, terminal/root choice,
fingerprint, actor or causation conflict. A duplicate returns its original sequence
without another journal entry, including after that command terminated the run.
Authority is still checked before receipt lookup; stale owners cannot publish.

These are internal coordinator receipts. Separate principal/method/target-scoped
[host start receipts](runtime-runs.md) are committed with initial run state.

## Journal schemas and replay

Every row stores run, sequence, closed event type, `record_version='0.1'`, actor,
nondecreasing timestamp, causation, optional command key and canonical payload.
Private payload shapes are:

```text
run_created:        [fingerprint, root_scope_id]
ownership_acquired: [owner, fence, expires_at_ms]
ownership_renewed:  [owner, fence, expires_at_ms]
records_committed:  [owner, fence, body]
run_initialized:    [body, [principal, method, fingerprint, key, digest, expiry]]
terminal_receipt:   body  // one final host reply; no status/root replacement
artifact_expired:   body  // one final artifact tombstone; no status/root replacement

body = [writes, terminal_status_or_null, new_root_or_null, expected_fingerprint_or_null]
new_root = [fingerprint, scope_instance_id]
write = [family, record_id, expected_revision_or_null, final_boolean, canonical_payload]
```

Writes are sorted by the closed `RecordFamily` enum order then decoded UTF-8 ID;
duplicates fail. The event records full changes, not only notification IDs. The
coordinator's operation-specific records retain dispatch, scheduler, policy and
acceptance evidence as their consumers are implemented.

`journal` returns bounded pages with exclusive sequence cursors and aggregate
private-row byte limits. `ReplayState::begin/apply` can consume those pages without
database or external-operation capabilities. It checks version, contiguous sequence,
run identity, time, owner/fence transitions, record revisions/finality, active-root
expectations, terminal results and command uniqueness. It reconstructs current rows,
authority header and command receipts. Retained reconstructed record/receipt data
is bounded by the supplied logical byte allowance. Discard reconstruction after
any error rather than continuing from a partially checked event.

This reconstructs storage decisions; actual graph recovery additionally re-admits
executables, checks referenced artifacts and reconciles dispatch state.

Terminal cancellation no-ops append an authenticated immutable receipt without
requiring an execution lease. This special transaction can insert only one final
reply record; it cannot change status, root, ownership or graph work. The journal
reconstructor enforces the same restriction. Identical retries do not add events,
and changed requests under a recorded key still conflict after completion.

## Outbox delivery

Each journal insert adds one notice in the same transaction. `pending_outbox`
returns bounded hints, redelivering until `acknowledge` succeeds. Acknowledgement
is itself idempotent and changes neither graph state nor journal outcomes. Delivery
workers have no acceptance authority merely because they receive a notice. The
outbox makes notification delivery recoverable, not external effects transactional.

## Verification and delivery status

Storage unit tests use actual SQLite connections, file reopen, concurrent owners
and aborting triggers. They verify rollback of run/journal/outbox publication and
ownership/fence/sequence/time as a unit, persistence, WAL/FULL/foreign-key settings,
lease expiry, backwards clock observations, and foreign/future schema rejection.
Transition tests additionally cover multi-record publication, final outcomes,
stale/final record rollback, takeover receipts, concurrent revisions, bounded pages,
duplicate outbox delivery, corrupt payloads, replay equivalence and altered history.

[Immutable graph registration](runtime-registration.md) and
[artifact storage/read integrity](runtime-artifacts.md) are implemented consumers.
[Atomic run-input publication](runtime-runs.md), host receipts,
[actor execution](runtime-execution.md), [MCP reservations](runtime-mcp.md) and
[extension accounting](runtime-extension.md) are implemented consumers.
