# Runtime SQLite authority foundation

Authority: [runtime §§1.4 and 14](../specs/runtime-spec.md). `htlk_runtime::store`
is trusted coordination infrastructure, not an authenticated host service. The
caller must validate executable admission, principal, inputs and host idempotency.
Storage methods do not perform those application-level checks.

## Database identity and durability

The private SQLite database uses application ID `0x48544c4b` and schema version 1.
An empty database is initialized in an immediate transaction. Existing unrelated,
nonempty or incompatible-version databases are rejected without relabeling them.
File stores use WAL, synchronous FULL, foreign keys and a bounded busy timeout.
`in_memory` is ephemeral and is only suitable for tests or disposable execution.

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

## Verification and delivery status

Storage unit tests use actual SQLite connections, file reopen, concurrent owners
and aborting triggers. They verify rollback of run/journal/outbox publication and
ownership/fence/sequence/time as a unit, persistence, WAL/FULL/foreign-key settings,
lease expiry, backwards clock observations, and foreign/future schema rejection.

General record transitions, command receipts, journal/outbox delivery APIs and
reconstruction are the next storage slice. Registration, run-input publication and
graph execution are subsequent consumers; this foundation is not a full runtime.
