# htlk-rt

Runtime components for the [Harness Toolkit](https://crates.io/crates/htlk).

This crate is the foundation for the native Rust actor-based execution environment
for Harness Toolkit executable graphs. Its selected dependencies support:

- OpenAI-compatible chat, streaming, structured output, and embeddings.
- Bundled SQLite storage for application data, caches, and metadata.
- Embedded vector search through `sqlite-vec`.
- Local and remote MCP servers over child-process stdio and Streamable HTTP.
- OAuth and JWT client credentials for MCP connections.

Its domain API is still being implemented. Conditions and pure calculations use
HTLK's typed expression records and a native Rust evaluator; prompt rendering uses
the canonical prompt-template model. No scripting runtime is embedded.

`htlk-executable` supplies the canonical model and public `cbor` adapter, and
`htlk-analyzer` supplies shared semantic analysis and offline schema preparation.
This crate owns `verify_executable`, `NativeRegistry`, native expression evaluation,
callback dispatch, actual-value checks, runtime schema projections and enforcement
of analyzer obligations. Registration transactions, deployment authorization,
scheduling, persistence and live MCP I/O remain pending.
`htlk-executable::digest` supplies typed SHA-256 digests and hashing for future
fingerprint checks and runtime identities, using consumer-defined preimages.
Its `ExecutableEnvelope` API validates version-0.1 envelope fields, format,
version, and fingerprint before later registration checks interpret the payload.

## Admission and checked execution

`verify_executable` decodes authoritative canonical envelope bytes, checks the
exact host-linked policy and library manifests, and recomputes whole-document
semantic analysis. `VerifiedExecutable` retains that immutable analyzer result
and borrows the registry, which cannot be mutated while admitted execution uses it.
Serialized compiler analysis is never treated as proof of host admission.

`VerifiedExecutable::evaluate` selects an authored expression by scope use and
expression site, then enforces the retained inferred boundaries and runtime checks.
`validate_edge_value` enforces destination value constraints; scheduling and
conditional binding selection remain coordinator responsibilities.

For focused expression execution:

```rust
use htlk_executable::{Expression, ExpressionContext, ScalarLiteral, EvaluatorLimits,
    cbor::{Limits, Value}};
use htlk_analyzer::ExpressionTypeEnvironment;
use htlk_rt::{CheckedExpression, EvaluationFrame, EvaluationValue};

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

## Native callbacks and schemas

Instantiated `ExpressionCallType` and `ExpressionCallbackType` data belong to the
analyzer. The runtime `CallbackInvocation` extension trait provides invocation;
`NativeCallContext` routes admitted callbacks through the linked registry. Forwarded
callables remain statically identified capabilities, not ordinary application values.
The shared meter charges ordered type preparation, compatibility work, dispatch,
argument/result checks and callback depth.

Offline validators are prepared by the analyzer. Runtime owns conversion and
metering of actual values, schema-origin retention and projection enforcement.
Unknown schema relationships retain runtime obligations. Native backend validation
does not acquire an unsupported fuel/deadline guarantee from static analysis.

Native identities compose package-owned model/analyzer source contributions with
runtime adapters. Relocation or import changes require regenerating the native
fixture and running the independent Node checker. The repository's
`docs/executable-api.md` contains the detailed cross-crate contracts and examples.

Licensed under the Apache License, Version 2.0.
