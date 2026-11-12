# PostgreSQL storage implementation plan

## Operating instructions

- Work on branch `feat/postgresql-store`, based on `origin/main` at `1e405c4`.
- Read this file after every completed task, after every compaction, and whenever
  choosing implementation scope. Update progress, evidence, decisions, and next
  steps after each task. Commit intended changes after each completed task.
- Preserve the pre-existing CHANGELOG.md edit introducing the 0.1.0 heading. It is
  user work, separate from PostgreSQL changes; do not stage it accidentally.
- Follow AGENTS.md, CONTRIBUTING.md, docs/README.md, and native fixture procedures.
- Do not delegate unless explicitly authorized. Finish implementation, checks,
  commits, push, and a new PR. Remove this completed temporary plan from the final
  tree in accordance with repository guidance; retain completion evidence in PR.

## Objective and scope

Add PostgreSQL alongside SQLite, covering the entire runtime and durable MCP
client. Preserve independent admission, transactional acceptance, canonical CBOR,
lazy reads, fencing, finality, idempotency, crash recovery, and protected evidence.
SQLite remains the default. PostgreSQL is host-selected through an optional Cargo
feature. Qualify PostgreSQL 16–18. New-store support is the initial deliverable;
cross-database migration of retained SQLite executions is a distinct scope.

## Baseline discoveries

- Runtime::new and McpClientPersistence::new accept concrete SqliteStore handles.
- Store operations span store.rs and domain-specific store_*.rs and ten SQL assets.
- Current SQLite schema version is 13; runtime-internals.md describes 12 (correct
  this during documentation integration).
- Latest main replaced tokio-rusqlite with pinned rusqlite 0.41.1 and a private
  asynchronous worker to fix RUSTSEC-2026-0459 / CVE-2026-119375. The implementation
  must preserve that fix; the original proposal predates it.
- Docker daemon is available. PostgreSQL host command-line tools are absent.
- Native identity contributions explicitly include storage sources and schemas;
  update Rust manifests and independent tools/validate-executable-fixtures.mjs.
- The .opencode tree is ignored. This plan is explicitly tracked temporarily to
  satisfy task-by-task commits, then removed when implementation is complete.

## Milestones and completion gates

### Task 0 — Branch and persistent plan

Status: complete (2026-11-11).

- Fetched origin/main and created feat/postgresql-store from the latest main.
- Temporarily stashed only the existing changelog edit while switching, then
  restored it successfully; stash removed by successful pop.
- Saved this plan and checked available infrastructure.
- Commit: recorded by the plan-initialization commit.

### Task 1 — Storage contract, inventory, and concrete design

Status: complete (2026-11-11).

Inventory every storage operation with atomic boundary, reads, locks, writes,
results/errors, and observable tests. Cover graph/provenance; run identity/time and
host receipts; leases/fences; record transitions, journals, outbox and commands;
artifact staging/acceptance/producers; scans/inspection/late responses; retention;
MCP owners/epochs/interactions/accounting; callbacks tied to run reservations; URL
consent; subscriptions; received and remote tasks.

Write backend-neutral contracts in specs and database mechanics in internals docs.
Finalize driver, SQL execution boundaries, pooling/worker integration, schema
identity, opaque identity mapping, error classification, lock order, and retries.

Gate: every operation has an implementation route and an independent invariant.

Concrete design decision: retain one set of domain transaction bodies behind a
private typed relational interface. SQLite continues using patched rusqlite;
PostgreSQL uses a synchronous native driver on a bounded dedicated worker, so the
embedding APIs remain asynchronous without blocking actors. This supersedes the
SQLx/pool proposal: an explicit finite worker queue and connection-per-handle are
the resource boundary, and independent handles supply independent connections.
PostgreSQL operations use one database/schema-scoped transaction writer gate for
the initial qualified backend. Scoped parallel writers are a subsequent measured
optimization, not a prerequisite to equivalent SQLite concurrency semantics.
Use native PG DDL and reviewed dialect/binding variants, not duplicated domain
reducers. PostgreSQL arbitrary string keys use raw UTF-8 BYTEA, digest-sized
generated index keys, and exact-preimage collision guards; bytewise range queries
retain original cursor order. Snapshot-consistent operations and nested domain
transaction markers must retain rollback/commit behavior.

### Task 2 — Backend-neutral storage and unchanged SQLite contracts

Status: pending.

- Introduce cloneable Store façade and private backend dispatch.
- Runtime and McpClientPersistence accept impl Into<Store>; no viral generics.
- Keep existing SqliteStore construction/conversion straightforward.
- Separate common representation/page bounds from backend connection settings.
- Reuse pure domain validation, ordering, digests, arithmetic, authority/deadline
  predicates, artifact/MCP checks, journal encoding, replay, and page accounting.
- Keep authoritative reads/checks/writes inside one transaction, preserving current
  check ordering. Do not eager-load unused checkpoints, artifacts, or branches.
- Never open independent transactions for run-owned MCP callbacks.
- Define shared-handle clone and close behavior.

Gate: SQLite tests pass through façade; independent admission/lazy-read invariants
and transactional checks remain intact.

### Task 3 — PostgreSQL foundation and native layout

Status: pending.

- Default sqlite Cargo feature and optional postgres feature, forwarded by htlk.
- Initially proposed SQLx with Tokio/pooling/rustls; review against new private
  worker before finalizing. Document any justified architecture adjustment here.
- Host-selected connection settings, schema, TLS, pool/queue bounds, connection,
  acquisition, lock, statement, retry and total operation deadlines.
- Dedicated recognized schema; metadata/application identity/version/namespace.
- Transactional empty-schema initialization, initialization arbitration, existing
  schema validation, separate initialization/opening APIs and permissions.
- Reject incompatible/foreign nonempty state without relabeling or deletion.
- CBOR/protected bytes BYTEA; signed counters/timestamps BIGINT; booleans BOOLEAN;
  fixed state strings and all FK/check/unique/deferrable constraints preserved.
- Handle circular subscription FKs through ordered DDL/ALTER TABLE.
- Arbitrary Rust strings may include NUL and exceed PG B-tree entry limits. Preserve
  exact UTF-8 identity bytes, use bounded lookup keys where necessary, retain exact
  preimages and reject digest collisions. Do not impose backend-specific identity
  normalization or accidentally narrow admitted input limits.
- Preserve exact bytewise cursor ordering; never replace ID order with digest order.
- Stable bounded errors; no credentials/raw diagnostics in public formatting.

Gate: lifecycle/compatibility/concurrent-init and representation-boundary tests.

### Task 4 — PostgreSQL core transaction engine

Status: pending.

Port in order: registration/provenance; namespace/run ID and host receipts; creation,
acquire/renew/header; transition CAS/finality/root replacement; command receipt;
journal/outbox/ack; terminal receipts and late responses; execution/inspection scans.

Establish a PostgreSQL transaction-level writer gate as correctness baseline, then
refine scope only under proven tests. A lock is database-scoped across independent
processes, never only an in-process mutex. Define deterministic lock ordering for
maintenance, global identity/time, run, MCP owner, subordinate identities.

Preserve authority-before-command-receipt ordering; leases cannot shorten; exact
expected revisions; immutable terminal states; checked arithmetic; contiguous run
journal sequence allocated transactionally from run state; authoritative deadline
checks after waiting/retry; host clock and nondecreasing committed time.

Retry only complete known-aborted DB transactions with fresh authority and bounded
deadlines. Serialization/deadlock is distinct from semantic revision conflict.
Lost COMMIT response is uncertain: reconcile original receipt before effectful
progress. Database retries must never dispatch network/provider operations.

Gate: independent pools/processes preserve atomicity/fencing/idempotency/replay.

### Task 5 — Artifacts, retention, and recovery

Status: pending.

- Native stage/metadata/bytes/publication/producers and recovery implementation.
- Immutable exact byte equality, full output-manifest acceptance in one commit.
- Expiry/reference/GC protocol: maintenance coordination protects new aliases,
  extensions, staged publication, rebuilding producer indexes, collection.
- Preserve accepted metadata as authoritative, derived producer index rebuildable.
- Snapshot-consistent multi-query reads; bounded decoding and incremental pages.

Gate: expiry versus alias/publication, GC versus staging/publication, reopen and
no-provider-call recovery invariants, including independent processes.

### Task 6 — Complete durable MCP support

Status: pending.

- Owners/generations/epochs and immutable session isolation.
- Interactions, quotas, work claims, replies, cancellations, delivery evidence,
  receipts, late evidence, explicit uncertain-work settlement.
- Run-owned callback admission and reservation release share transaction with
  run transitions; owner counters and per-interaction facts remain consistent.
- URL interactions/required errors/consent/completion/receipts.
- Resource subscriptions/consumers/operations/restore.
- Received protocol tasks/cancellation receipts/acceptance and remote task records.
- Owner journal sequence allocation cannot let a paging cursor skip a late commit;
  SQL sequences alone do not establish commit order and may have rollback gaps.

Gate: all durable MCP conformance, principal/epoch isolation, quota races,
completion/cancellation/retrieval-error accounting, and reopen scenarios pass.

### Task 7 — Conformance, faults, concurrency, and resource qualification

Status: pending.

- Shared backend fixtures: isolated open, independent connection, reopen, subprocess
  descriptor, fault injection, independent SQL inspection. Unique PG schema/store.
- PostgreSQL coverage requested without service infrastructure fails explicitly.
- Parameterize relevant root runtime_* and deep_e2e and crate MCP suites; keep native
  database-format tests backend-specific.
- Independent observable matrix: single lease owner/higher fence; stale acceptance
  rejected; one CAS winner; original idempotent receipt/conflicting request; journal,
  outbox and deferred-FK failures roll back all state; reply/expiry and terminal
  races; one duplicate start; quota bounds; uncertain work retained; no expired
  accepted references; no missing accepted bytes; persisted-response local recovery;
  lost commit acknowledgment never authorizes duplicate external effect.
- Barrier/checkpoint-controlled races, independent connections/processes.
- Faults: process kill before/after commit; open waits/loop feedback; unsafe intent;
  connection termination/restart; pool/queue exhaustion; lock timeout; deadlock/
  serialization retry; deterministic lost-COMMIT-response harness.
- Differential outputs/stable errors/records/protection/replay/provider call counts
  with only noncontractual IDs/time normalized, plus direct SQL independent checks.
- Validate pool/queue/paging bounds, cancellation rollback, broken connections,
  durable commit settings and primary-only access.
- Benchmark hot run, independent runs, journals, callbacks, blob throughput,
  retention and pool pressure; qualify scoped locks only with correctness evidence.

Gate: complete both-backend behavioral suite and PostgreSQL-specific faults pass.

### Task 8 — Integration, documentation, packaging, and pull request

Status: pending.

- Feature builds/lints: default, SQLite-only, PG-only, both, all-features. Compile,
  docs and packages must not require a live database; tests provision one explicitly.
- Update native source identity manifests/checker together and explicitly regenerate
  reviewed native fixture (never have tests repair goldens). Keep identity source
  contributions feature-independent unless a reviewed contract says otherwise.
- Update Cargo.lock, dependency policy and THIRD_PARTY_LICENSES via cargo-about
  0.9.2 as required. Retain patched SQLite and current CI disk-budget improvements.
- CI/coverage/release provide PostgreSQL service, minimum/current qualified version
  matrix, all-PR crash/stress/deep tests, line and branch evidence, feature checks.
- Update runtime guide/internals/testing/MCP guide/runtime README/CONTRIBUTING and
  changelog; shared semantics in specs, mechanics in internals, procedures in guides.
- Checked PostgreSQL construction and MCP example configuration with environment
  credentials; document initialize/open, TLS/pool bounds, backup/restore/restart and
  deployment durability/replication responsibilities.
- Required checks from CONTRIBUTING, affected examples/doctests, fresh-target Cargo
  workspace packaging, dependency and notice verification, workflow validation.
- Final review status/diff/recent commits/base comparison; stage only intended
  changes; remove completed plan; commit all intended changes; push own branch;
  use gh to open new PR against main and report URL.

Gate: all implementation and checks complete; PR includes commits reviewed as a
whole and clearly records behavior, tests, compatibility, operational limits.

## Task log / next action

- Task 0: complete. Next: inspect main's private worker and finalize inventory and
  concrete architecture before creating the storage façade.
- Task 0 commit: 0b02b16. Re-read plan before task 1. Added portable equivalence
  requirements and domain atomicity/invariant inventory. Latest main already
  corrects the stale SQLite layout documentation. Reviewing a shared typed
  relational execution seam to retain the already-audited transaction bodies and
  private worker instead of duplicating domain state machines.
- Task 1: inventoried all domains and compound boundaries in runtime-internals;
  added portable equivalence requirements in runtime-spec. Concrete worker/driver,
  writer-gate and opaque-key design above recorded before implementation. Next:
  build the typed interface and façade. Validation: node tools/validate-docs.mjs
  passed (30 docs, 213 links); node specs/validate-specs.mjs passed. No Rust examples,
  embedded assets, or doctests were changed by this contract-only task.
