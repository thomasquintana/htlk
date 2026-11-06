# Runtime internals

This maintainer reference describes private representations and transactional
invariants. The [runtime specification](../specs/runtime-spec.md) owns portable
semantics; the [runtime guide](runtime-guide.md) covers host integration. Private
records are not an additional supported wire format.

## Storage compatibility

The SQLite application ID is `0x48544c4b`; the private schema version is **12**.
An empty database initializes in one immediate transaction. Versions 1–11, future
versions, foreign application IDs, and nonempty unrecognized databases fail without
relabeling or deletion. There is no automatic migration or legacy binding decoder.
Matching `0.1` format labels do not permit changed interpretation of saved effects.
File stores require WAL, synchronous FULL, foreign keys, and bounded busy timeout.

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
