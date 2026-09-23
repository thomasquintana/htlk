# HTLK Analyzer

Shared semantic analysis for canonical HTLK executables. The analyzer depends on
`htlk-executable` and is usable by both compilers and runtimes without depending
on either. It resolves names and types, infers generic call signatures, checks
scope interfaces and dependencies, and prepares immutable offline schemas.

Analysis produces structured diagnostics and explicit actual-value obligations.
Static success does not authorize a host implementation or replace runtime
enforcement. Whole-document results retain the immutable document they describe;
callers cannot construct a result by attaching arbitrary plans.

## Focused analysis

```rust
use htlk_executable::{Expression, ExpressionContext, ExpressionKind, ValueReference,
    Port, BuiltinType, ValueType, cbor::Limits};
use htlk_analyzer::{ExpressionTypeEnvironment, check_condition};

let limits = Limits::default();
let source = ValueReference::Input("allowed".parse()?);
let expression = Expression::new(ExpressionKind::Ref {
    source: source.clone(), path: vec![],
}, ExpressionContext::Preconditions, &limits)?;
let mut declarations = ExpressionTypeEnvironment::default();
declarations.references.insert(source,
    Port::new(ValueType::builtin(BuiltinType::Boolean), true));
let analysis = check_condition(&expression, ExpressionContext::Preconditions,
    &declarations, &limits)?;
assert_eq!(analysis.result().value_type(), &ValueType::builtin(BuiltinType::Boolean));
# Ok::<(), Box<dyn std::error::Error>>(())
```

`check_expression_diagnostic` retains canonical child-index paths. Name resolution
and contextual legality include every branch, even one runtime evaluation would
short-circuit. Generic inference and callback compatibility preserve optionality,
variance and original callback-reference locations. `ExpressionAnalysis` exposes
read-only inferred types, instantiated calls and explicit runtime obligations.
`check_expression_context` supports lower-level integrations that need contextual
legality without declaration resolution or type inference.
`check_function_signature` and `check_prompt_template` resolve generic declarations
and exact template slot/parameter bindings for focused compiler/runtime admission.

`verify_scope_graph` checks a focused ordinary or loop-body use, including local
endpoints, conditions, interface binding coverage, complete wait dependencies,
cycles and observability. Conditional writers retain runtime uniqueness checks;
analysis does not assume a SAT proof.

## Document-bound results

- `LinkedDocument` checks document references/interfaces and structural bounds
  without claiming schema or complete expression/graph verification.
- The `DocumentAnalysis` extension trait supplies focused schema-catalog, native
  schema-preparation and MCP descriptor stages for a canonical document.
- `analyze_document` combines linkage, schema/MCP checking and every graph use.
  It returns a privately constructed `AnalyzedDocument` retaining the exact
  immutable canonical document, parsed policy, structural summary, schemas and plans.
- `GraphVerification` itself retains its immutable document; composed analysis
  shares the same document allocation. No public constructor attaches arbitrary
  plans or accepts serialized analysis as proof.

The analyzer checks policy structural declarations; runtime separately admits the
exact host-selected policy and native implementation identities. There is no
compiler, evaluator, registry or runtime adapter dependency here.

## Offline schemas and deferred checks

`SchemaLocations`, `SchemaResources`, `SchemaCatalog` and `NativeSchemas` prepare
bounded offline schema state. External retrieval is disabled. Pinned MCP descriptor
validation and protocol snapshot assets live here as well.

Schema inference is conservative: representation-family refinements can reject
proven disjoint types, while uncertain compatibility retains actual-value and
original-root projection obligations. JSON Schema integer constraints do not
coerce native floating representations. Runtime integrates the backend with value
metering, projection and obligation enforcement. Native backend validation retains
its documented input/regex constraints rather than claiming general fuel bounds.

The repository's `docs/executable-api.md` retains the detailed schema, inference,
resource-accounting and runtime-boundary reference. Its examples are doctested.
