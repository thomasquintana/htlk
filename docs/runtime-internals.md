# Runtime internals

This maintainer reference describes private representations and transactional
invariants. The [runtime specification](../specs/runtime-spec.md) owns portable
semantics; the [runtime guide](runtime-guide.md) covers host integration. Private
records are not an additional supported wire format.

## Storage compatibility

The SQLite application ID is `0x48544c4b`; the private schema version is **14**.
An empty database initializes in one immediate transaction. Versions 1–13, future
versions, foreign application IDs, and nonempty unrecognized databases fail without
relabeling or deletion. There is no automatic migration or legacy binding decoder.
Matching `0.1` format labels do not permit changed interpretation of saved effects.
File stores require WAL, synchronous FULL, foreign keys, and bounded busy timeout.

A private dedicated thread owns each SQLite connection. Cloned store handles share
its FIFO operation queue. Dropping a caller future does not cancel queued storage
work; explicit close drains earlier operations and closes all clones. Independent
connections continue to arbitrate authority through immediate SQLite transactions.

For an intentionally disposable development store: stop its owners, reconcile any
test-owned external resources from retained evidence, identify the exact database
and its `-wal`/`-shm` companions, and remove that identified state explicitly. Build,
compile, and register fresh executables under current selections. Removing local
history neither reverses external effects nor cleans remote resources. Fixture
maintenance is described in [testing](testing.md#native-identity-and-fixtures).

`SqliteStore` is trusted infrastructure. Its caller establishes admission,
authentication, value checks, and application authorization. `StoreConfig` bounds
codec metadata, page sizes, and busy timeout. Structured errors omit database paths,
submitted values, and raw SQLite diagnostics.

### Storage operation boundaries

Backend implementations preserve the following compound operations. Preparation
and representation checking do not replace rechecking authoritative state inside
the committing transaction. The runtime specification owns
[backend equivalence](../specs/runtime-spec.md#141-storage-backend-equivalence).

| Domain | Facts that share an atomic boundary | Independent observable invariant |
| --- | --- | --- |
| Registration | Immutable envelope and detached provenance | Failure publishing provenance leaves no partial registration |
| Run initialization | Run header, initial records/artifacts, creation/initialization journal, outbox, host receipt | Duplicate principal/method/target/key creates one run; changed arguments conflict |
| Authority | Owner, fence, expiry, committed time, ownership journal/outbox | Independent coordinators cannot simultaneously acquire a live run |
| Transitions | Revisions/finality, root/outcome, command receipt, journal/outbox | One CAS winner; rollback publishes no partial output |
| Terminal receipts | Final reply, terminal-only journal entry, outbox | A receipt cannot revive a terminal run |
| Artifacts | Accepted metadata, complete output manifest, producer uniqueness | Staging is not publication; one producer slot has one immutable identity |
| Retention | Reference checks, expiry tombstone, journal/outbox, collection decisions | New aliases cannot race expiry into accepted dangling references |
| MCP work | Owner/epoch/interaction authority, counters, claim/reply, receipts/events | Quotas cannot overspend; an existing intent cannot authorize redispatch |
| Run callbacks | MCP facts plus ancestor reservations and ordinary run transitions | A failed compound commit changes neither accounting domain |
| URL interactions | Protected parameters/error links, consent receipt, reply protection | Consent and server completion remain separate immutable facts |
| Subscriptions | Desired consumers, operation claim/outcome, revision and gap | Concurrent consumers produce one authoritative subscription operation |
| Received tasks | Task state/result, underlying interaction facts, cancel receipt | Cancellation does not release uncertain underlying work |
| Remote tasks | Intent, acceptance, snapshots, final/create/retrieval evidence | Retrieval failure cannot invent an execution outcome |

Record, journal and evidence pages are bounded by rows and aggregate retained
bytes. Multi-query reads requiring a coherent view use a database snapshot. Lazy
expression/source decisions must not become eager database reads when adapting a
backend. Recovery rebuilds derived indexes from authoritative metadata, and replay
does not acquire provider capabilities.

### PostgreSQL layout and execution

The optional `postgres` feature provides native PostgreSQL tables in a dedicated
host-selected schema. `PostgresStore::initialize` transactionally creates an empty
recognized layout; `PostgresStore::open` requires existing state. The metadata
application is `htlk.runtime.postgres`, layout version **2**. Initialization and
opening also compare the exact embedded layout digest; a matching version label
cannot reinterpret a different private layout. Initialization and
runtime operations arbitrate through the same database/schema-scoped transaction
advisory lock. Each admitted operation uses one primary connection and a READ
COMMITTED transaction; domain transaction boundaries use savepoints inside it.
The lock is acquired before domain reads, preserving SQLite's serialized writer
semantics across processes, including retention, callbacks, and journal cursors.
Read operations currently acquire the same gate for coherent multi-query views.
The PostgreSQL driver caches at most 256 prepared statements. Single-row lookups
use their native prepared query directly; scans use server cursors with a bounded
16-row wire buffer and incremental domain decoding/page accounting.
Indexed point predicates retain exact-byte comparisons alongside generated lookup
keys. Identity triggers recheck insertions and identity-column changes; ordinary
revision/lease updates do not repeat unchanged preimage registration.

Cloned handles share a dedicated worker and a queue of at most 256 waiting calls.
Independent opens create independent connections. Timeouts bound connection,
authority lock, statements, and admitted operations. A dropped waiter does not
cancel admitted work. Close drains earlier work and closes every clone of that
handle. There is no automatic replay of database operations. A lost commit reply
returns `StoreError::CommitUnknown` and quarantines the connection; reopen and
reconcile the original durable receipt before further effectful progress.
The worker drives its own Tokio PostgreSQL protocol runtime. The connection
deadline covers TCP, TLS negotiation and authentication; protocol I/O also observes
the operation deadline. Timed-out protocol tasks are aborted and their connections
cannot be reused. Only explicit integrity/serialization/deadlock rollback or a
pre-send refusal establishes a known failed commit; other ambiguous COMMIT failures
retain uncertainty.

Canonical CBOR and protected values remain BYTEA. Arbitrary indexed string
identities also retain exact UTF-8 BYTEA, including NUL. Generated SHA-256 lookup
keys bound PostgreSQL B-tree entries; checked preimages prevent a collision from
becoming identity equality. Pages compare original identity bytes, not lookup
digests or locale collation. Private flags use checked BIGINT 0/1 values, while
counters, revisions, fences, and milliseconds retain signed i64 bounds. Staged
artifact bytes do not publish accepted metadata. The native layout has its own
constraints and deferred producer links; it does not embed a SQLite database.
The driver bounds selected text/byte fields before retaining owned row buffers:
SQLite checks borrowed field lengths before copying, and PostgreSQL guards
projected fields on the server before transmitting them. Oversized fields fail
instead of becoming NULL or optional absence. Metadata compatibility reads are
bounded independently of the stored metadata's claimed version or digest.

### Host MCP interaction storage

The store includes owner-scoped MCP leases, epochs, interactions, receipts, journal
facts, and a bounded late-reply inbox. Host sessions use their own namespace rather
than synthetic graph runs. Immediate transactions arbitrate duplicate work claims
across connections. Only a newly committed `McpWorkClaim::Started` permits dispatch;
an existing intent remains uncertain, and a saved reply is reusable without recall.
Lease takeover does not reset spent calls or outstanding reservations. Reply bytes
and protection are recorded before delivery facts, and transport acceptance is
distinct from application consumption.

Underlying `McpWorkDelivery` is independent of sending a callback reply. An RPC error
can be recorded and acknowledged while model work remains uncertain. Per-interaction
charged/outstanding flags independently reconcile the owner counters before admission.
Usage receipts distinguish unknown tokens/cost from observed zero.

Late replies cannot publish outputs or release reservations. Explicit settlement
against an exact retained known-completion or not-started receipt can release
uncertain concurrency while preserving cancellation, original RPC replies, and spent
calls. Uncertain late evidence cannot settle work. These store APIs are trusted infrastructure;
native callback admission and authenticated frontend APIs require their separate
runtime integration described in the [client contract](../specs/mcp-client-spec.md).

URL lifecycle storage uses owner/server/session/elicitation-ID digests. Protected
parameter/error blobs retain complete metadata; journal and host events carry only
opaque local identities. Callback consent derives from the existing immutable
callback reply rather than a dual-written URL response. Error-originated decisions
have their own principal/key-bound receipts. Consent and first server completion
are independent facts; completion does not release work reservations, replace an
RPC outcome, or authorize an operation retry. Complete required-error lists commit
atomically, and run-origin admission independently rechecks the actual outstanding
attempt, ancestor protection and deadline through the existing run admission seam.
Notifications require the live epoch; historical inspection and exact receipt
lookup survive closure under current authorization and ownership.

## Ownership and atomic transitions

`create_run` publishes header, `run_created`, and outbox hint atomically. `acquire`
requires unowned/expired authority and increments a fence; a live owner cannot be
replaced even under the same owner name. `renew` preserves the fence, cannot shorten
expiry, and rejects expired authority. Time uses the maximum of trusted observations
and committed time. Leases expire at their deadline. Immediate transactions, rather
than actor serialization alone, arbitrate separate connections.

`commit(lease, transition)` checks current owner/fence/expiry, run status, command
identity, expected fingerprint, record revisions/finality, and terminal transition
inside one transaction. Keys are `(run, RecordFamily, opaque ID)`. Insertions create
revision one; updates require the exact previous revision; final records never change.
Records, root changes, outcome, journal, outbox, and command receipt commit together.
Each consumer validates its own typed payload before this boundary.

Reply/approval acceptance and successful invocation/scope boundaries recheck persisted
deadlines at commit time. Denial commits failure with its disposition. Deadline
conflicts roll back acceptance before applying the due recursive stop.
Response persistence deliberately does not compare usage/reservation revisions:
extension can change ancestry. Capacity release retries locally from the recorded
response and never resends the operation.

Internal receipts hash `['htlk.store_command', '0.1', actor, causation, run_id, body]`.
The lookup key, timestamp and owner/fence are excluded so a new owner can retry the
same command. Authority is still checked before receipt lookup. Host receipts use
the separate `htlk.host_request` domain and authenticated principal/method/target.

## Journal and outbox

Rows retain run, contiguous sequence, closed event kind, `record_version='0.1'`,
actor, nondecreasing time, causation, optional command key, and canonical payload.

```text
run_created:         [fingerprint, root_scope_id]
ownership_acquired:  [owner, fence, expires_at_ms]
ownership_renewed:   [owner, fence, expires_at_ms]
records_committed:   [owner, fence, body]
run_initialized:     [body, [principal, method, fingerprint, key, digest, expiry]]
terminal_receipt:    body
artifact_expired:    body

body = [writes, terminal_status_or_null, new_root_or_null, expected_fingerprint_or_null]
new_root = [fingerprint, scope_instance_id]
write = [family, record_id, expected_revision_or_null, final_boolean, canonical_payload]
```

Writes sort by `RecordFamily` enum order then decoded UTF-8 ID; duplicates fail.
Terminal cancellation no-op receipts can insert only one final reply, without
changing lifecycle, root, or ownership. Expiry can insert only the final tombstone.
Replay enforces these restrictions and checks time, authority, finality, command
uniqueness, and root changes. Discard partial reconstruction after any error.
Pages and aggregate retained reconstruction bytes are bounded.

Each journal insertion adds an outbox notice in the same transaction. Hints
redeliver until idempotent acknowledgement. They do not authorize acceptance or make
external effects transactional. Execution recovers from authoritative records,
not from assuming a notification was delivered exactly once.

## Identity and frozen checkpoints

Runtime domains hash deterministic CBOR arrays `['htlk.' + domain, '0.1', ...args]`.
Compiler record domains instead hash a text prefix plus canonical record bytes.
Run IDs remain opaque strings; counters fit signed i64. Attempts/extensions are
one-based, iterations/approval generations zero-based. Full preimage byte/depth
accounting precedes hashing; value trees are borrowed rather than cloned.

`ValueDigest` binds declared source type and value. `ArtifactId` additionally binds
producer/port provenance. Storage addresses hash exact canonical value bytes.
Identical bytes can be shared under different types and producers. Staging publishes
no accepted metadata. Final artifact metadata, complete successful output manifests,
and producer-slot uniqueness commit together. Failed nodes cannot publish ordinary
outputs; accepted metadata and parent lineage remain immutable. Recovery rebuilds
producer indexes from checked metadata, not the reverse.

`InputManifest::freeze` requires exactly one entry for every declared input,
including explicit optional absence. Required absence is a skip decision, not an
admitted manifest. Logical entries are `{state:'absent'}` or
`{state:'value', artifact_id:...}`. Trusted labels are sorted unique decoded UTF-8
strings without Unicode normalization. The input hash preimage is:

```text
['htlk.input', '0.1', [
  {name, port:{type:normalized_type, required:boolean}, state:'absent'}
  OR
  {name, port:{type:normalized_type, required:boolean}, state:'value',
   content_digest:typed_source_content_digest, label_digest:security_label_digest}, ...
], frozen_influence_label_digest]
```

Entries sort by decoded UTF-8 port name. Labels hash
`['htlk.security_labels','0.1',sorted_unique_label_array]`. Artifact IDs are retained
but excluded from content-equivalence hashing; declarations, requiredness, absence,
content, and labels affect it. A matching hash grants no access. Encoding uses a
closed versioned checkpoint with entries, influence labels, and input hash. Decoding
rechecks canonicality, authoritative port declarations, full inventory, and hash;
it cannot repair missing protection or replace declarations silently.

## Binding and influence

`reduce_binding` is a pure proposal. It bounds/sorts candidates, evaluates every
guard in canonical order, and waits for all guards before resolving errors,
conflict, absence, or one source. It never resolves false/conflicting/pending groups.
Source resolvers read local state, not external services. All evaluated guard
influences contribute, including false guards; source labels contribute only after
unique selection. Secondary causal failures are retained in deterministic order.

`reduce_admission` evaluates a node guard before inspecting inputs, then settles
every binding and selects errors before required absence, ordered by UTF-8 port
name. The coordinator integrates these proposals with actual-value checks,
authorization, contracts, frozen manifests, and durable commits.

`InfluenceContext` retains enclosing base/control protection and authorizes only
actual value/outcome reads. Unevaluated lazy branches neither load artifacts nor
inherit unread data. Repeated reads are traced once per evaluation; a new evaluation
rechecks policy. Projection cannot strip root labels. Missing metadata fails closed;
denial and metadata exhaustion are errors, never false, null, or successful absence.
Failures and absent results retain influence as well as values.

## Recovery and concurrent progress

Recovery re-admits all exact origins and checks definition/use IDs, complete frozen
manifests, artifacts, waits, attempts, normalization observations, and adopted
reservations. Saved responses resume local validation; uncertain unsafe sends fail
without blind replay. Pure missing decisions can recompute under exact code and
frozen inputs. Journal reconstruction alone never executes providers.

Before declaring a stable no-progress state `E_INVARIANT`, the reducer must recheck
the journal sequence **after all external-work scans**. A concurrent accepted host
reply may have removed an observed wait while also enabling work. A changed sequence
requires a fresh reduction. Stable impossible states still fail; genuine future
timers/open requests remain blocked. Deterministic interleaving and deadline
controls live in `crates/htlk-runtime/src/execution/step_tests.rs`.

Deadline/cancellation stops fence acceptance before signalling attempts. Calculation
and deadline control share recursive stop semantics. Stale results trigger a fresh
pass under the live owner, preserving fallback liveness. Fences prevent local stale
acceptance, not already-sent remote effects.

`RuntimeFailure` commits a fixed redacted graph-visible code/message. Protected
causal IDs, canonical sites, and secondary validation evidence are separate.
Inspection redaction cannot change errors seen by graph expressions. Invocation
terminal states cannot transition even to themselves; duplicate commands return
receipts. `blocked` is a rebuildable scheduler observation, not a lifecycle state.
