# Embedding and operating the runtime

HTLK is a Rust library. Your application is the host: it authenticates callers,
selects implementations and policy, supplies values, and presents results. The
[user guide](user-guide.md) introduces graph authoring; this guide explains the
host lifecycle. The [runtime specification](../specs/runtime-spec.md) is normative.

## Start from a runnable host

```console
cargo run --example runtime_workflow --locked
```

This credential-free example compiles source, registers executables, attaches source
metadata, opens a planning wait, joins graphs, installs an extension atomically,
inspects results, and reads output artifacts. A second workflow uses a scripted
tool and a separately authenticated approver. Read
[`examples/runtime_workflow.rs`](../examples/runtime_workflow.rs) alongside it.
The interactive tutorial's [`host.rs`](../examples/interactive_guide/host.rs) is a
smaller introduction to the same APIs.

## Configure the host

Construct a `NativeRegistry`, link the finite policy with `link_policy`, and use
that profile, policy, complete library manifests, and default native schema options
in the compiler. Runtime verification independently checks the same selections.
An empty registry does not install optional libraries; the [MCP guide](mcp-guide.md)
explains the content library and adapters.

Construct `Runtime` from a backend-neutral `Store`, shared immutable registry, explicit
`RuntimePolicy`, codec limits, and bounded admitted-cache capacity. There is no
default allow policy. `HostContext` must come from your authenticated session or
transport; constructing it validates representation, not identity. Ordinary graph
inputs cannot choose the principal or confer grants.

Use a file-backed SQLite store or PostgreSQL for durable operation. SQLite in-memory stores disappear when their last
handle closes. File stores use SQLite WAL, FULL synchronization, foreign keys,
and transactional fencing. Only the current private schema version is accepted;
see [storage compatibility](runtime-internals.md#storage-compatibility).

The native adapter cannot enforce hard external token/cost budgets. Explicit hard
limits, including zero, are rejected with `E_BUDGET_UNSUPPORTED` before starting or
installing affected work. Usage estimates and compiler capability flags do not
establish a trusted hard meter.

## Choose and provision storage

SQLite is enabled by default. `SqliteStore` is the compatible name for the shared
store handle's `open`/`in_memory` constructors. To enable PostgreSQL through the
facade, select its Cargo feature:

```toml
htlk = { version = "0.1.0", features = ["postgres"] }
```

For PostgreSQL-only builds, add `default-features = false`. The same feature names
are available on `htlk-runtime`. Pure evaluation can be built with neither backend;
storage construction then returns an invalid-configuration error.

Use `PostgresConfig::new` with a host-owned connection string, usually loaded from
an environment variable or secret configuration. Set its dedicated `schema` and
finite connection/lock/statement/operation deadlines. `StoreConfig` retains shared
codec/page bounds; its legacy busy timeout applies to SQLite. Native PostgreSQL
storage is qualified on PostgreSQL 16 and 18 and supports all runtime and durable
MCP operations. Select a writable primary with UTF-8 database encoding and fsync
enabled. The connection sets synchronous commit on. Verified TLS uses the host's
platform certificate trust; `sslmode=disable` explicitly selects an unencrypted
local connection.

1. Provision a database and a dedicated empty HTLK schema, or grant the provisioning
   role permission to create that schema.
2. Call `PostgresStore::initialize(settings, config).await?` with the provisioning
   role. Empty state is initialized transactionally; foreign or incompatible state
   is rejected. The store does not automatically migrate or delete state.
3. Grant the runtime role schema USAGE, table SELECT/INSERT/UPDATE/DELETE,
   sequence USAGE/SELECT, and function EXECUTE within that HTLK schema.
4. Open the recognized state with `PostgresStore::open`. This path creates no
   objects. Pass the handle to `Runtime::new` and clone the same handle for durable
   `McpClientPersistence`; use the runtime's actual trusted clock as well.

The checked PostgreSQL construction helper is in the
[runtime crate README](../crates/htlk-runtime/README.md#storage-backends).
Clones share one connection worker and at most 256 queued operations. Independently
opened handles use independent connections and arbitrate through database
transactions. The current PostgreSQL implementation serializes reads/writes per
HTLK schema, preserving the same fenced authority and compound callback-accounting
boundaries as SQLite. Size host connection counts, leases and workflow deadlines
for observed database/provider latency; reopening does not extend saved deadlines.

For backup/restore, retain the complete schema, including namespace, counters,
sequences, receipts, journal, artifact bytes and protected MCP evidence. Use
PostgreSQL logical backup/restore or physical/PITR procedures appropriate to the
deployment. Quiesce owners before logical restoration and resume only one
authoritative destination. Replication/failover configuration must retain the
committed database copy on which recovery relies. Recovery reacquires higher
fences and reconciles stored work; it does not recall providers just because the
database or runtime restarted. SQLite-to-PostgreSQL transfer is not an automatic
schema upgrade; use a fresh PostgreSQL store for new deployments.

An operational storage failure is separate from a confirmed workflow outcome.
`StoreError::CommitUnknown` means the server may have committed despite losing its
acknowledgement. The connection is quarantined. Open a healthy handle and reconcile
the original command/host idempotency receipt or MCP work intent before proceeding.
Do not translate that uncertainty into another external dispatch. See
[PostgreSQL internals](runtime-internals.md#postgresql-layout-and-execution) for
layout and protocol boundaries.

## Register executable bytes

`Runtime::register_graph(context, bytes, attestations)` independently validates the
canonical envelope, identities, document, schemas, expressions, structure, and exact
native profile before consulting producer-trust policy. A signature does not bypass
structural verification. Policy verifies deployment-specific attestations; a digest
alone identifies content, not authorship.

Registration atomically stores immutable bytes under their fingerprint and detached
provenance separately. Equal bytes are idempotent; conflicting bytes under the same
identity fail integrity checks. Source maps, descriptions, signatures, and timestamps
do not enter executable meaning. Registration executes no nodes.

`Runtime::executable` authorizes inspection on every call, including cache hits.
Reloads independently re-admit saved bytes under the linked implementations. A zero
cache capacity disables caching; eviction has no execution-semantic significance.

## Start and select runs

`Runtime::start_run` takes authenticated context, a registered fingerprint, an
identifier-keyed map of `InputValue`, a required idempotency key, and optional
execution limits. Inline values and accepted artifact references are explicit
variants. JSON that resembles a reference is still ordinary inline data.

Admission rejects unknown/missing required inputs, validates actual values and
artifact integrity/access without coercion, freezes labels and manifests, evaluates
the root precondition, and commits root state, timers, usage, and receipt atomically.
Invalid inputs create no run. A valid input whose root precondition is false creates
a failed run with no child work. Registration and admission can be used without an
execution engine.

Launch `ExecutionEngine` inside an Actix system. For a request-scoped service, use:

```rust,ignore
let engine = ExecutionEngine::launch(
    runtime.clone(),
    ExecutionConfig {
        scope: ExecutionScope::Attached,
        ..ExecutionConfig::default()
    },
)?;
// After admission, attach exactly the durable run selected by this host request.
engine.attach_run(&run_id).await?;
```

This is an integration fragment; `runtime` is an `Arc<Runtime>` and `run_id` is an
existing `RunId`. The [API reference](executable-api.md) and runnable hosts show
complete configuration. `ExecutionScope::All` is the default and intentionally
supervises every eligible nonterminal run in the store.

Attached mode starts empty. Admission, inspection, and host-command notifications
never attach work. Unattached runs receive no ownership, recovery, deadline changes,
or dispatch. Active commands for a run without local coordination return `E_FENCED`.
Attachment is idempotent and grants no policy authority or deadline reset.
`max_attached_runs` bounds the additive lifetime selection; terminal runs consume
no new entry, but completed prior attachments remain. `max_runs` separately bounds
concurrent coordinators. Restarting attached mode requires attaching again.

Only one live engine may attach to a given runtime. A duplicate launch returns
`E_PROFILE`; after acknowledged shutdown a replacement can attach.

## Idempotency, time, and lifecycle

Host receipts are scoped by principal, method, and target. The request digest
includes all semantic arguments, not the lookup key. Repeat an identical request
to retrieve its receipt; changed arguments under the same key fail
`E_IDEMPOTENCY_CONFLICT`. Current authorization is checked on retries. Historical
receipts grant no artifact access and can survive unavailable old implementations.

`RuntimeConfig::idempotency_retention_ms` is positive and finite (24 hours by
default). A key may identify new work after expiry. Artifact retention is separate.
Trusted coordinator time is normalized against durable watermarks; wall-clock
rollback cannot revive expired requests. Run IDs are opaque and not reused after
failed reservations.

Execution uses immutable inputs, lazy reads, durable guard/binding decisions, and
fenced atomic acceptance. A scope settles all children before publishing outputs;
failed children may feed authored fallbacks. Task definition and wrapper contracts
both apply. Loop feedback and final-next checks persist across restart. See the
[language reference](../specs/htlk-ir-syntax-reference.md) for exact semantics.

## Answer waits and approve actions

Input waits expose addressed requests with topic, request artifact, response type,
deadline, and trusted authority/version. The topic helps routing. The frozen
requester, current policy, authenticated responder, and wait ID govern acceptance.
`submit_reply` validates purpose/type/deadline before an atomic reply/receipt commit.
The node still has to pass its postcondition before output publication.

Reply and approval labels are trusted policy outputs, persisted with the accepted
disposition and retained by subsequent boundaries. Duplicate accepted replies can
return historical receipts after expiry or completion. New replies cannot resolve
closed, expired, cancelled, or approval-purpose waits. Input expiry is
`E_WAIT_EXPIRED`; approval denial/expiry is `E_APPROVAL_DENIED`.

`submit_approval` is a separate exact-action API binding operation, frozen arguments,
principal, invocation, request digest, generation, policy, and expiry. A typed reply
does not authorize a tool. The [MCP guide](mcp-guide.md#approval-generations) explains
renewal and dispatch ordering.

## Extend a running workflow

Compile new work, join it with the retained canonical document using pinned
catalogs, register the composition, then call `extend_run` with an `ExtensionRequest`.
It supplies the expected active fingerprint, composed fingerprint, public inputs,
and optional `CloseWait`. A stale expected fingerprint fails
`E_EXTENSION_CONFLICT`; a nonadditive shape fails `E_EXTENSION`.

The runtime independently verifies both executables, identical profiles and exact
preserved closure. The new root contains two ordinary scopes with true wrapper/root
contracts/guards, empty local limits, and unique unconditional boundary bindings.
Either operand can be preserved. Existing values, labels, optional absence, origin,
IDs, checkpoints, deadlines, approvals, attempts, and artifacts remain unchanged.
Old inputs may inherit through explicitly mapped renamed composed ports; new values
receive ordinary validation. New work cannot become an old prerequisite or narrow
already-admitted ceilings.

Usage and outstanding reservations are adopted into new ancestors, not replenished.
An already-sent response releases all adopted capacity without a second dispatch.
Installation races with cancellation/finalization through transactional revisions.

Put a typed planning wait on a required completion path to keep the run open.
`CloseWait { wait_id, value, idempotency_key }` installs the extension and accepts
that reply, with both receipts, journal, and outbox, in one transaction. Invalid
compilation or installation leaves the wait unchanged until its original deadline.
A terminal run cannot extend; an identical historical installation may return its receipt.

## Inspect results and retain source metadata

Use `inspect_run`, `inspect_node`, and `inspect_graph` for authorized logical views.
They expose scope/definition/origin identities, frozen manifests, status, waits,
usage, bindings, guard causes, protected diagnostics, and delivery-aware attempts.
They exclude raw unvalidated proposals; [MCP evidence](mcp-guide.md#protected-evidence)
has a separate authorized API. IDs never grant value access.

`inspect_history` pages bounded summaries rather than private journal payloads.
Readers verify stable committed sequences; concurrent mutation can return retryable
`E_FENCED`. `blocked` reflects a completed eligibility scan at an unchanged sequence
before the next timer, not a guess from pending-node reasons.

`SourceSidecar` attaches exact source origins, UTF-8 spans, declarations and
descriptions outside executable identity. `attach_source` validates targets and
applies current policy/labels. Several origins can describe one deduplicated target.
`inspect_sources` pages authorized reports; `inherit_sources` copies only
byte-equivalent targets while retaining old protection. Generated wrappers use
synthetic origins. These reports do not alter committed errors or authorize execution.

Accepted values are read with `read_artifact`: authorize metadata first, then verify
canonical bytes and raw/typed/provenance identities. Type narrowing never rewrites
the source artifact. Content bytes can be shared by distinct producers.

## Recover, stop, and collect

Recovery acquires fenced ownership, re-admits every exact origin, and verifies
manifests, hashes, artifacts, waits, checkpoints, and reservations before resuming.
Prepared operations still need ordinary dispatch checks; ambiguous sends need trusted
replay/deduplication and authored retry or fail `E_EFFECT_UNCERTAIN`. Recorded responses
resume local validation without recalling providers. Process-local browser/sandbox
sessions do not survive restart as durable external handles.

`ReplayState::begin/apply` reconstructs journal decisions without execution or MCP
capabilities. It is separate from recovery, which can resume work.

`cancel_run` commits descendant stops and a receipt; it preserves prior terminal
outcomes. Terminal cancellation records an idempotent no-op. `engine.shutdown()`
stops local supervision and preserves durable state; it does not cancel the run.
Cross-process forwarding of active commands belongs to the embedding application.
After stopping execution, await native adapter shutdown to acknowledge transport cleanup.

`audit_retention_ms` promises a minimum completion audit window at least as long as
idempotency retention. A shorter later configuration cannot reduce a saved promise.
`expire_artifact` returns Protected or an immutable expiry timestamp only after the
producing run is terminal, the window elapsed, and authoritative references permit it.
Time passing alone does not expire a value. Expiry records a final tombstone.

`collect_garbage` uses bounded pages and trusted housekeeping authorization. Shared
bytes stay while any unexpired artifact protects them; collection preserves metadata,
producer uniqueness, labels, tombstones, and journal history. Expired reads return
`E_RETENTION_EXPIRED`; missing/corrupt retained bytes return `E_DIGEST`. Reappearing
bytes cannot revive an expired artifact ID.
