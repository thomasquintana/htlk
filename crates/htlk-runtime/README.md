# htlk-runtime

Execute checked HTLK Draft 0.1 workflows with durable SQLite or PostgreSQL state, fenced Actix
supervision, explicit host policy, and bounded MCP connections over stdio and
Streamable HTTP JSON/SSE. Pure expressions and prompt rendering use native Rust;
the runtime embeds no scripting engine.

## Host lifecycle

1. Build a `NativeRegistry`, link your policy and required pure libraries, and
   compile with the same profile, policy, manifests, and default native schema options.
2. Construct `Runtime` from a backend-neutral `Store`, immutable registry, `RuntimePolicy`,
   finite codec limits, and admitted-cache capacity.
3. Supply authenticated `HostContext` and call `register_graph` with executable
   bytes. Runtime independently verifies authoritative bytes and producer trust.
4. Call `start_run` with explicit inline/artifact inputs and an idempotency key.
5. Launch `ExecutionEngine` inside an Actix system and select the intended runs.
6. Inspect progress, answer waits/approvals, and read accepted output artifacts.

Choose `ExecutionScope::Attached` for request-scoped hosting, then call
`engine.attach_run(&run_id).await?` after durable admission or loading the saved run.
It starts empty and leaves unrelated runs untouched. `ExecutionScope::All` is the
default for intentional store-wide hosting. Attachment grants no authority and
resets no deadlines or usage. `max_attached_runs` bounds lifetime selection;
`max_runs` independently bounds concurrent coordination.

Only one engine may attach to a runtime at a time. Shutdown stops supervision while
preserving durable state; cancellation is a separate authenticated operation.
Historical identical commands return immutable receipts, not repeated effects.
Native hard external token/cost budgets are unsupported and rejected before affected
work starts. Cross-process active-command forwarding belongs to the host.

The repository's `docs/runtime-guide.md` provides the complete host lifecycle;
`examples/runtime_workflow.rs` is runnable without credentials. `docs/mcp-guide.md`
covers adapters, sessions, results, evidence, and explicit schema adoption.

## Storage backends

SQLite is the default `sqlite` Cargo feature. Existing `SqliteStore::open` and
`SqliteStore::in_memory` constructors remain available through the compatible
`SqliteStore` name. Enable `postgres` for native PostgreSQL storage; the `htlk`
facade forwards both features. A PostgreSQL-only dependency can use:

```toml
htlk-runtime = { version = "0.1.0", default-features = false, features = ["postgres"] }
```

This host-construction helper compiles without contacting a database during doctests:

```no_run
#[cfg(feature = "postgres")]
async fn postgres_store() -> Result<htlk_runtime::store::Store, Box<dyn std::error::Error>> {
    use htlk_runtime::store::{PostgresConfig, PostgresStore, StoreConfig};
    let mut settings = PostgresConfig::new(std::env::var("HTLK_DATABASE_URL")?);
    settings.schema = "htlk".into();
    let store = PostgresStore::initialize(settings, StoreConfig::default()).await?;
    Ok(store.into())
}
```

`PostgresStore::initialize` initializes an empty dedicated schema or opens its
recognized layout. `PostgresStore::open` requires existing compatible state and
creates no objects. Both the runtime and `McpClientPersistence` accept the concrete
PostgreSQL handle directly and share its actual worker when cloned. PostgreSQL 16
and 18 are qualified by complete backend campaigns. Use a writable UTF-8 primary
with fsync enabled; connections require verified platform TLS unless explicitly
configured with `sslmode=disable`. Clones share one bounded connection worker;
independent opens create independent connections. Transactions currently serialize
per HTLK schema across processes. Select lease/workflow timeouts for database and
provider latency. The runtime guide covers provisioning, durability and recovery.

## Admission and enforcement

`verify_executable` decodes the canonical envelope/document, matches host-selected
policy and complete library manifests, and recomputes semantic analysis. It does
not trust serialized compiler reports. `verify_executable_owned` retains an
`Arc<NativeRegistry>` and returns a `Send + Sync` owned verified handle for actors
and caches. Both prepare schemas using `NativeSchemaOptions::default()`.

`VerifiedExecutable::evaluate` selects an authored expression by `ScopeUse` and
`ExpressionSite`, enforcing retained inferred boundaries and actual-value checks.
Binding reducers are pure proposals; the coordinator applies authorization,
contracts, frozen manifests, and transactional publication. Actual-read influence
preserves labels through values, absence, outcomes, and lazy branches.

`Store` is trusted infrastructure, not the authenticated host API. SQLite file stores
use WAL/FULL synchronization; PostgreSQL uses native tables and durable transactions.
Both reject incompatible state without migration or deletion. Accepted artifacts preserve typed content identity separately from producer
provenance; staged bytes become readable only after atomic metadata/output acceptance.
`read_artifact` authorizes metadata before loading and checking bytes.

Recovery verifies exact origins and checkpoints, resumes recorded responses without
MCP recall, and rejects unsafe ambiguous replay. Additive extension preserves old
work and adopts usage/reservations. Protected source reports and retention tombstones
remain separate from executable meaning. `docs/runtime-internals.md` records private
checkpoint/storage invariants; `docs/testing.md` maps contracts to regression suites.

## Focused evaluation

```rust
use htlk_executable::{Expression, ExpressionContext, ScalarLiteral, EvaluatorLimits,
    cbor::{Limits, Value}};
use htlk_analyzer::ExpressionTypeEnvironment;
use htlk_runtime::{CheckedExpression, EvaluationFrame, EvaluationValue};

let limits = Limits::default();
let policy = EvaluatorLimits {
    max_expression_depth: 64, max_value_bytes: 65536, max_collection_visits: 10000,
    max_regex_bytes: 1024, max_regex_compiled_bytes: 4096,
    max_output_bytes: 65536, max_steps: 100000,
};
let expression = Expression::literal(ScalarLiteral::Integer(42));
let declarations = ExpressionTypeEnvironment::default();
let checked = CheckedExpression::new(&expression, ExpressionContext::Eval,
    &declarations, None, &limits)?;
let result = checked.evaluate(&EvaluationFrame::default(), None, &policy)?;
assert_eq!(result.value, EvaluationValue::Present(Value::Integer(42)));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Native callbacks remain static capabilities rather than application values.
`NativeCallContext` routes checked callbacks through the linked registry and shared
meter. Schema validation retains the backend's actual supported limits; static
analysis does not manufacture a whole-validation fuel or deadline guarantee.

The repository's `docs/executable-api.md` contains detailed doctested cross-crate
examples. Source-derived implementation identity and fixture maintenance are
documented in `docs/testing.md`.

Licensed under the Apache License, Version 2.0.
