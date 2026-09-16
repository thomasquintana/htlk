# HTLK Executable

The shared Draft 0.1 executable format, verifier, and native expression engine for
the HTLK compiler and runtime. `verify_executable` validates a complete canonical
envelope against host-linked policy and implementations and returns an immutable
`VerifiedExecutable` with derived graph and runtime-boundary plans.

The crate also exposes lower-level canonical records, schema/MCP validators,
expression analysis/evaluation, and SHA-256 primitives in `digest`. Those component
constructors keep their narrower contracts; assembling a `CanonicalDocument` or
decoding an `ExecutableEnvelope` alone does not establish full verification.

Execution-limit and retry-policy records describe the controls later enforced
by the runtime; these are configuration records, not running counters or timers.
Canonical expressions and prompt templates describe calculations and text assembly;
their model APIs do not evaluate expressions or invoke services.
Scope, node, edge, and operation records assemble those declarations into locally
checked graph definitions; full graph verification remains a separate stage.
Execution profiles, library signatures, and MCP binding records describe exact
implementation selections; they do not load engines, open connections, or grant access.
`JsonDocument` and `PolicyDocument` provide canonical external JSON and policy
records. `CanonicalDocument` assembles the complete payload with record identity
and known-reference checks.

Depends on `htlk-cbor` for deterministic encoding and RustCrypto's `sha2` for
SHA-256; the codec does not depend on this crate.

## Verify a complete executable

The host selects its trusted policy and links any required pure native libraries.
The default registry provides the specified core/native engines without inventing
a standard-library inventory. Verification performs no MCP service calls.

```rust
use htlk_cbor::{Limits, Value};
use htlk_executable::{CanonicalDocument, DocumentFields, EvaluationFrame,
    EvaluationValue, EvaluatorLimits, ExecutionLimits, ExpressionSite, NativeRegistry,
    PolicyDocument, PolicyFields, Scope, ScopeContext, ScopeFields, ScopeUse,
    verify_executable};

let limits = Limits::default();
let policy = PolicyDocument::new(PolicyFields {
    cost_unit: "credits".into(),
    defaults: ExecutionLimits::new().with_timeout_ms(1000)?
        .with_attempt_timeout_ms(100)?.with_max_concurrency(1)?,
    evaluator_limits: EvaluatorLimits {
        max_expression_depth: 64, max_value_bytes: 65536,
        max_collection_visits: 10000, max_regex_bytes: 1024,
        max_regex_compiled_bytes: 4096, max_output_bytes: 65536,
        max_steps: 100000,
    },
    maximum_scope_depth: 32, maximum_expanded_nodes: 1000,
}, &limits)?;
let mut registry = NativeRegistry::new(&limits)?;
let profile = registry.link_policy(&policy)?;
let scope = Scope::new(ScopeFields::default(), ScopeContext::Ordinary, &limits)?;
let root = scope.digest(ScopeContext::Ordinary, &limits)?;
let mut fields = DocumentFields::new("example.empty".into(), profile, root);
fields.scopes.insert(root, scope);
fields.documents.insert(policy.digest(), policy.document().clone());
let bytes = CanonicalDocument::new(fields, &limits)?.envelope(&limits)?.encode(&limits)?;

let verified = verify_executable(&bytes, &registry, &limits)?;
let result = verified.evaluate(&ScopeUse::Root, &ExpressionSite::Postconditions,
    &EvaluationFrame::default())?;
assert_eq!(result.value, EvaluationValue::Present(Value::Bool(true)));
assert_eq!(verified.graphs().plans().len(), 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`VerifiedExecutable::evaluate` borrows admitted expression plans and uses their
pinned policy, schemas and registry. It does not rebuild/copy all analysis metadata
or reserialize the expression before each execution. Canonical AST paths identify
node-local checks independently of allocation addresses. `validate_edge_value`
checks a proposed receiving value; the runtime binding reducer owns conditional
selection, uniqueness, absence, and failure precedence.

`verify_scope_graph` checks one ordinary/task or loop-body use. `verify_graphs`
checks every actual document use and shares equivalent immutable plans. They cover
required candidate coverage, unconditional writer conflicts, whole-port types,
all guard branches, admission/input/outcome waits, public/next bindings, hidden
cycles, and explicit observability. Loop bodies are checked against each use's
termination condition. No SAT proof or iteration expansion is required.

`ExecutableVerificationError` preserves stage causes. Graph failures carry scope
definition/use, node/edge expression site, and canonical child-index paths where
applicable. `NativeSchemaDiagnostic` carries original document digests and JSON
Pointers when the failure has a known schema location. Diagnostics do not retain
submitted operand values or native validator message text.

Existing codec limits bound encoded inputs and derived plans; private stage
counters bound inference, schema declaration propagation and aggregate graph work.
Counts include retained runtime checks/call signatures, not just result types.
Temporary representations have independently bounded overhead. Native JSON Schema
validation has the capability contract documented below, not a claimed hard heap
cap, whole-validation fuel counter, or in-process deadline. Runtime registration
transactions, scheduling, authorization, persistence and live MCP I/O belong to
`htlk-rt`; source compilation and composition belong to `htlk-compiler`.

## Local identifiers

`Identifier` is an immutable local name with exact ASCII spelling
`[a-z][a-z0-9]*(_[a-z0-9]+)*`. Names begin with a lowercase letter; later segments
may begin with digits. Leading, trailing, and repeated underscores are invalid.
Parsing performs no trimming, case folding, or Unicode normalization.

```rust
use std::collections::HashMap;
use htlk_executable::{Identifier, ParseIdentifierError};

let name = Identifier::new("draft_2".to_owned())?;
assert_eq!(name.as_str(), "draft_2");
assert!("Draft_2".parse::<Identifier>().is_err());
let lookup = HashMap::from([(name.clone(), 42)]);
assert_eq!(lookup.get("draft_2"), Some(&42));
assert_eq!(name.into_string(), "draft_2");
# Ok::<(), ParseIdentifierError>(())
```

Owned construction (`new` or `TryFrom<String>`) validates without copying.
`FromStr` and `TryFrom<&str>` validate before fallible allocation. The type also
supports Display, Debug, equality, lexical ordering, hashing, `AsRef<str>`, and
`Borrow<str>`. Consuming `into_string` provides the exact name for serialization;
there is no additional CBOR tag or wrapper.

`ParseIdentifierError` distinguishes empty names, invalid first bytes, invalid
characters, invalid underscore placement, and allocation failure. Character and
separator errors carry zero-based byte offsets. Checks proceed left-to-right,
then reject a trailing underscore. Errors contain no submitted name. Callers
apply source/codec size ceilings; the lexical rule has no separate length cap.

Identifier validity is not node/declaration validity. Reserved roots such as
`inputs` and contextual keywords such as `graph` are lexically valid; consumers
apply restrictions for node names, library aliases, declarations, or imports.
Scope resolution and uniqueness are likewise separate checks. Qualified names,
module paths, and arbitrary quoted record-field names need their own handling.
Identifier ordering is ordinary ASCII/UTF-8 order; canonical CBOR map ordering
is independently applied by the codec.

## Canonical types and ports

`ValueType` describes permitted data, whereas `htlk_cbor::Value` holds actual
data. It exposes a read-only `ValueTypeKind`: primitive, list, map, record, union,
enum, schema digest, signature variable, or function type. `PrimitiveType` contains
the exact closed primitive names from the CDDL. Source aliases (including `text`)
must be resolved by the compiler before reaching this canonical model.

```rust
use htlk_cbor::Limits;
use htlk_executable::{Port, PrimitiveType, TypeContext, ValueType, ValueTypeKind};

let limits = Limits::default();
let context = TypeContext::Value;
let nullable = ValueType::new(ValueTypeKind::Union(vec![
    ValueType::primitive(PrimitiveType::String),
    ValueType::primitive(PrimitiveType::Null),
]), context, &limits)?;
let port = Port::new(nullable, false);
assert!(!port.required());
let bytes = port.encode(context, &limits)?;
assert_eq!(Port::decode(&bytes, context, &limits)?, port);
# Ok::<(), htlk_executable::TypeError>(())
```

Construction with `ValueType::new` normalizes authored shapes: record fields
acquire canonical map order, unions flatten/sort/deduplicate, and enums sort by
decoded UTF-8 bytes. The syntax specification requires **at least two distinct
union members after normalization**: singleton/empty results are errors, not a
coercion to their member. Enums must be nonempty and unique; duplicates are errors.
Enum strings and record field names retain exact Unicode spelling, including
empty strings. A record field is a `Port`; graph/node port-table names will use
`Identifier` in their containing records.

`decode` and `from_value` require canonical records and never normalize malformed
or noncanonical type structure into acceptance. `to_value` produces the canonical
CBOR value; `encode` produces its bytes. All these boundaries take `TypeContext`
and the existing CBOR `Limits`. `Port::new` combines an already normalized type
with its presence flag; context and whole-port limits are checked when it is
encoded, decoded, or embedded in a larger type.

| Model | Canonical representation |
|---|---|
| Primitive string | `"string"` |
| List of strings | `["list", "string"]` |
| String-keyed integer map | `["map", "integer"]` |
| Nullable string | `["union", ["null", "string"]]` |
| String enumeration | `["enum", ["brief", "detailed"]]` |
| Record fields | `["record", { field_name: { type: ..., required: ... } }]` |
| Schema reference | `["schema", "sha256:..."]` |
| Signature variable | `["var", "t"]` |
| Signature function | `["function", [parameter_ports...], return_port]` |

Ports contain exactly `type` and `required`. Both fields are mandatory, including
`required: false`; unknown metadata fields fail. Requiredness is independent of
nullability: optional permits absence, while nullable permits a present null.
Function parameter order is preserved, and result requiredness describes whether
the function can return absence. Union members sort by encoded type bytes, not
by their names; this differs from enum string ordering.

### Context and validation boundary

`TypeContext::Value` recursively prohibits variable and function constructors.
`TypeContext::Signature` permits those shapes for library signatures. Context is
chosen by the consuming schema, not supplied by a field in untrusted data.

Schema lookup, reached-schema-root restrictions, generic-variable declarations,
unification/recursive-type rejection, assignability, and validation of actual
runtime values remain graph-verifier/evaluator work. A shape-valid schema digest
does not prove that its document exists; a parsed signature variable does not
prove that a library declared it.

### Type resource accounting and errors

Before allocating conversion strings or growing collections, a private builder
checks the actual CBOR text/array/map/Boolean representation against codec limits,
including map keys, tag strings, presence flags, encoded lengths, and depth.
Parsing begins only after bounded CBOR decoding (or bounded encoding for an
existing Value). Normalization temporarily holds source members and sorting keys,
bounded by the checked input's total values/bytes; its final result must also fit
the configured limits, including an expanded flattened-union array. Codec byte
string limits do not constrain a type named `bytes`, which is text metadata.

`TypeError` retains codec, digest, and identifier causes and static schema error
categories. Codec errors preserve offsets; a graph/source consumer can attach its
own record location to higher-level type errors. Errors contain no submitted
field names, enum contents, or other input values. Conversion-limit errors are
reported before constructing an oversized temporary CBOR value.

Depth regression tests run in subprocesses on 512 KiB and 2 MiB thread stacks.
They exercise the 128 codec-depth ceiling with list chains, function signatures,
unions, construction/encoding/cloning/formatting, and cleanup after a nested field
failure. Collection-specific parsing/conversion helpers keep recursive dispatch
frames small. These tests provide headroom, not a guarantee for arbitrary caller
stack sizes. Clone/Debug of caller-owned objects are ordinary Rust operations.

## Canonical expressions

### Native evaluation

`evaluate` interprets the existing canonical AST directly in Rust. It returns an
`EvaluationResult` containing a present value, legitimate absence, or pending, plus
`EvaluationUsage`. `evaluate_condition` additionally requires a Boolean and returns
`ConditionValue::Ready(bool)` or `Pending`; it never applies truthiness or changes
an error into false. The coordinator owns node skipping, pre/postcondition error
codes, settled-boundary checks, scheduling, and output acceptance.

`EvaluationFrame` supplies frozen values, outcomes, and templates without requiring
an embedded language runtime. Bindings can include explicit projected-path results
for verified optional fields. Fallback field/index lookup is strict: an unknown key
does not become optional merely because it is missing. `EvaluationContext` connects
the evaluator to runtime data, verified projection plans, and exactly linked Rust
functions. Native functions receive `EvaluationMeter` and must charge their own work.

```rust
use htlk_cbor::{Limits, Value};
use htlk_executable::{ConditionValue, EvaluationFrame, EvaluationValue,
    EvaluatorLimits, Expression, ExpressionContext, ExpressionKind,
    CoreFunction, FunctionId, ValueReference, evaluate_condition};

let codec = Limits::default();
let policy = EvaluatorLimits {
    max_expression_depth: 64, max_value_bytes: 65536, max_collection_visits: 10000,
    max_regex_bytes: 1024, max_regex_compiled_bytes: 4096,
    max_output_bytes: 65536, max_steps: 100000,
};
let mut frame = EvaluationFrame::default();
frame.bind(ValueReference::Input("answer".parse()?), vec![],
    Ok(EvaluationValue::Present(Value::Null)), &codec)?;
let input = Expression::new(ExpressionKind::Ref {
    source: ValueReference::Input("answer".parse()?), path: vec![],
}, ExpressionContext::Preconditions, &codec)?;
let condition = Expression::new(ExpressionKind::Call {
    function: FunctionId::Core(CoreFunction::Present), arguments: vec![input],
}, ExpressionContext::Preconditions, &codec)?;
assert_eq!(evaluate_condition(&condition, ExpressionContext::Preconditions,
    &frame, &codec, &policy)?.value, ConditionValue::Ready(true));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Boolean operators evaluate left-to-right and short circuit. Pending and errors
stop that evaluation path. Absent operands fail outside explicitly optional native
arguments and `present`; record construction omits absent members, while lists
reject absent elements. Present null is retained. Comparisons enforce exact scalar
representations; there is no integer/float coercion or compound-value equality.
`length` counts Unicode scalars, bytes, list elements, or map keys as appropriate.

Rendering checks exact argument coverage and primitive types, preserves ordered
template parts, and bounds growing text. Static function references are direct
call-argument metadata and cannot be returned as ordinary values. The default
frame has no application libraries; their native implementations and signatures
must be supplied by the linked registry, not installed by expression data.

Each evaluation starts fresh policy accounting. Work includes AST visits, inspected
encoded-value bytes, lookup/comparison work, collection traversal, and rendering.
Values are bounded before copying; construction is bounded incrementally. The final
output has its own ceiling, so a small Boolean result may inspect larger permitted
inputs. Regex compilation reserves its configured compiled-size allowance as a
deterministic upper-bound fuel charge before calling the native compiler. Limits
produce `E_EXPRESSION_LIMIT`, and absence misuse produces `E_EXPRESSION_ABSENT`.

The evaluator validates representation/context and actual operand behavior.
`CheckedExpression` connects static name/type analysis to runtime enforcement.
Schema-aware checked evaluation and exact native-registry matching are composed by
`verify_executable`. A standalone evaluation does not establish graph admission.

### Static expression checking

`check_expression` resolves and analyzes every AST branch against an
`ExpressionTypeEnvironment` containing the owning context's value declarations,
permitted outcomes, complete library manifests, and templates. `check_condition`
adds a required Boolean result boundary. Unknown names are rejected even inside
a branch that would short circuit during execution.

```rust
use htlk_cbor::Limits;
use htlk_executable::{Expression, ExpressionContext, ExpressionKind,
    ExpressionTypeEnvironment, Port, PrimitiveType, ValueReference, ValueType,
    check_condition};

let limits = Limits::default();
let source = ValueReference::Input("allowed".parse()?);
let mut environment = ExpressionTypeEnvironment::default();
environment.references.insert(source.clone(),
    Port::new(ValueType::primitive(PrimitiveType::Boolean), true));
let expression = Expression::new(ExpressionKind::Ref { source, path: vec![] },
    ExpressionContext::Preconditions, &limits)?;
let analysis = check_condition(&expression, ExpressionContext::Preconditions,
    &environment, &limits)?;
assert_eq!(analysis.result().value_type(), &ValueType::primitive(PrimitiveType::Boolean));
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ExpressionAnalysis` exposes the inferred root port, per-node type metadata, and
`RuntimeTypeCheck` obligations at child-index AST paths. It preserves requiredness
independently of nullability. Disjoint types fail statically; overlapping JSON,
schema, union, and optional cases retain value/presence/projection checks. Structural
records permit extra fields while required declared destination fields must be
covered. Union projections retain optionality when only some record variants
declare a field. Built-in error, regex, snapshot, and prompt-result fields expose
their known structural information.

Library signatures are instantiated with fresh variables per call and callback,
so identically named generic parameters cannot capture one another. Constraints
from arguments and an optional expected result type drive inference. Deferred
union constraints can be resolved by later arguments. Unresolved, ambiguous, and
recursive generic equations fail. Callbacks require provable parameter/result
compatibility, including contravariant parameters and covariant returns; static
function references remain direct argument metadata, not ordinary values.

`ExpressionAnalysis::calls` exposes each native call's fully instantiated
`ExpressionCallType`, ordered by canonical AST path. It includes the exact library
identity/function name, parameter ports (including concrete callback signatures),
and return port. `CheckedExpression` enforces ordinary arguments before dispatch
and return values afterward. Native contexts receive these constraints through
`EvaluationContext::call_typed`; the default dispatches through `call_at` and `call`.
Invalid arguments prevent invocation, pending dependencies postpone it, and a
native function returning pending is rejected.

Direct callback metadata also retains the callback's independently instantiated
actual signature. Native implementations can call
`ExpressionCallType::invoke_callback` with ordinary value arguments to enforce
both that signature and the receiving parameter's callback constraints. It
dispatches through `EvaluationContext::call_callback`, shares the evaluator meter,
and bounds recursion by the expression-policy and codec depth ceilings. Pending
arguments/results are errors. Callback intermediates use the value-size ceiling;
the enclosing expression still owns the final output ceiling. This entry point
accepts ordinary values. `invoke_callback_with` additionally accepts
`CallbackArgument::Function(slot)`, forwarding only statically admitted callback
slots. Both actual and receiving higher-order signatures are checked before
dispatch, and original canonical function-reference locations are preserved.

### Host-linked native registry and default profile

`NativeRegistry` starts empty: the specified evaluator core and native engines
work without a newly defined standard library. Hosts register complete `Library`
manifests with an exact name-to-`NativeFunction` implementation map. Registration
rejects duplicate implementation identities, missing/extra functions, and aggregate
metadata limits. Each implementation has a positive prepaid dispatch charge and
must charge variable work through the shared evaluator meter. Implementations are
trusted pure Rust functions; this interface does not sandbox host code.

`NativeRegistry::evaluate` checks every supplied environment manifest before
execution and routes native calls through the registry rather than the frozen
data frame. `NativeCallContext::invoke_callback` uses the checked callback helper
and dispatches its admitted identity through the same registry. Complete manifest
comparison includes unused public signatures, versions, generic declarations, and
presence rules.

`native_profile` constructs the shipped core/engine identities for an exact policy
document. `NativeRegistry::verify_profile` compares every core, regex, schema,
URI-template, and policy identity. `link_policy` selects exact trusted host policies;
an embedded policy cannot approve its own identity. Adapter identities include implementation source;
regex 1.13.1, regex-automata 0.4.18, regex-syntax 0.8.11 (Unicode 16.0.0), and
iri-string 0.7.14 are pinned in the published dependency constraints. The native
schema resource contract remains unchanged. `verify_executable` composes profile
admission and registry-backed execution with schema/MCP and whole-graph verification.

Analysis rechecks environment and expression bounds and meters inference work,
type copying/comparison, lookups, and stored metadata paths. Its derived output
must also fit configured limits. Controlled-stack tests cover deep unary and
binary expressions at the codec depth ceiling.

An analysis report alone does not execute its runtime obligations.
`check_expression_with_schemas` adds conservative representation-family refinement
and rejects proven disjoint boundaries without a general schema-subtyping solver.
Uncertain refinements retain actual-value checks. JSON Schema mathematical integer
constraints do not silently convert a native floating representation to an integer.
`CheckedExpression::new` borrows immutable syntax and declarations, analyzes all
branches, and rejects unresolved schema-projection obligations.
`CheckedExpression::with_schemas` admits them against pinned offline validators. Its `evaluate`
method validates evaluated node results, actual-value boundary constraints, and
declared reference roots, while retaining lazy Boolean and pending behavior.
References must provide root bindings; typed projection derives optional-field
absence from declarations instead of trusting independently supplied projected
bindings. Templates come from the analyzed declaration environment.

`validate_typed_value` admits actual native values without coercion, including
structural records, unions, JSON, protocol values, and schema identities linked
to their prescribed embedded roots. `project_typed_value` applies declared
record/list/map/union projections. Undeclared extra fields cannot become visible
through a union variant that does not declare them; missing map keys and invalid
indices remain errors. Evaluator work and copy limits apply; native JSON Schema
validation retains the native resource contract described above.

`SchemaProjection` validates the complete original schema root before access and
retains native representations and reference/dynamic scope. Permitted additional
and pattern-matched fields remain accessible. Missing dynamic keys error; missing
explicitly declared optional properties yield absence. Private compiler annotations
preserve field declarations through native evaluation without changing original
JCS identities; authored annotations cannot impersonate them.
Schema origin survives record/list materialization. Only computed roots needed
by later projections are retained, under the existing aggregate payload ceiling;
native functions are not reinvoked to recover discarded schema context.

Standalone execution contexts implement the native-call contract themselves.
`NativeRegistry` supplies checked linked dispatch, and `VerifiedExecutable` fixes
that registry and the admitted graph/policy/schema plans for consumer execution.

`Expression` is an immutable normalized tree with a read-only `ExpressionKind`.
Supporting types are `ScalarLiteral`, `ValueReference`, `PathStep`, `FunctionId`,
`CoreFunction`, and `BinaryOperator`. Scalar literals hold text, bytes, signed
integers, finite floats, Booleans, or null; list/record construction has separate
expression forms. Absence, node handles, and callable references are not scalar
application values.

```rust
use htlk_cbor::Limits;
use htlk_executable::{BinaryOperator, CoreFunction, Expression, ExpressionContext,
    ExpressionKind, FunctionId, ScalarLiteral, ValueReference};

let limits = Limits::default();
let context = ExpressionContext::Preconditions;
let question = Expression::new(ExpressionKind::Ref {
    source: ValueReference::Input("question".parse()?), path: vec![],
}, context, &limits)?;
let length = Expression::new(ExpressionKind::Call {
    function: FunctionId::Core(CoreFunction::Length), arguments: vec![question],
}, context, &limits)?;
let condition = Expression::new(ExpressionKind::Binary {
    operator: BinaryOperator::Gt,
    left: Box::new(length),
    right: Box::new(Expression::literal(ScalarLiteral::Integer(0))),
}, context, &limits)?;
let bytes = condition.encode(context, &limits)?;
assert_eq!(Expression::decode(&bytes, context, &limits)?, condition);
# Ok::<(), htlk_executable::ExpressionError>(())
```

This describes `length(inputs.question) > 0`; it does not calculate a result.
The graph verifier must still establish that `question` exists and has a suitable
type. Context checks restrict reference categories, not the result type of a
condition or the existence/accessibility of a particular named node or port.

| Kind | Canonical form |
|---|---|
| Literal | `["literal", scalar]` |
| Regex | `["regex", pattern, flags]` |
| Named value | `["ref", source, path]` |
| Projection | `["get", expression, nonempty_path]` |
| List | `["list", [expressions...]]` |
| Record | `["record", [[key, expression]...]]` |
| Call | `["call", function_id, [arguments...]]` |
| Static callable | `["function_ref", library_digest, identifier]` |
| Render | `["render", template_digest, { name: expression, ... }]` |
| Outcome | `["status", node]` or `["error", node]` |
| Negation | `["not", expression]` |
| Binary | `[operator, left, right]` |

Value roots are input, sibling/body output, proposed scope output, loop-carried,
or proposed next values. Paths contain exact string field names or nonnegative
i64-range indices. Function IDs select core `length`/`present` or a library digest
and identifier. Core calls require one argument; library signatures are resolved
later. Binary operators are exactly and/or/eq/ne/lt/le/gt/ge.

### Expression contexts

The caller supplies a context derived from the owning graph location, never from
an untrusted context field:

| Context | Permitted references beyond literals/static definitions |
|---|---|
| Eval / preconditions | Own inputs |
| Guard | Containing inputs, sibling outputs/outcomes; carried only in loop bodies |
| Primitive / wrapper postconditions | Own inputs and proposed outputs |
| Scope postconditions | Own inputs, proposed outputs, direct-child outcomes |
| Loop until | Inputs, carried, next, proposed outputs, body outputs/outcomes |
| Loop postconditions | Inputs and final proposed outputs |

Restrictions apply recursively, including both Boolean operands. Outcome records
describe `status(@node)`/`error(@node)`; the eventual runtime waits for terminal
outcomes instead of exposing transient attempt failures. Self-reference, sibling
membership, and hidden dependency cycles require full graph verification.

### Normalization and static callable references

`Expression::new` combines adjacent get paths, folds selections into references,
sorts record-expression fields by decoded UTF-8 keys, and normalizes unique regex
flags to ims order. Empty get paths, duplicate fields, and unknown/repeated regex
flags fail. `from_value` and `decode` require these canonical forms already.

Record-expression fields are an ordered array of pairs, not a CBOR map: their
evaluation order is UTF-8 key order. Render arguments are a wire map, with model
iteration exposed in UTF-8 name order independently of encoded map-key ordering.
Function arguments, list elements, and left/right operands are never reordered.
There is no constant folding, including for constant Boolean operands: all
references remain available for static dependency analysis.

`ExpressionKind::FunctionRef` preserves the exact library digest/name without
executing it. It is an AST description, not a callable application-value type.
The linked-library verifier must enforce function-parameter placement and signature
compatibility; arbitrary input strings do not become code. Regex pattern syntax
and compiled-size validation similarly require the pinned regex engine. This
model checks regex text representation and flags, not engine availability.

## Prompt templates

`PromptTemplate` owns parameter ports and ordered `TemplatePart::Text`/`Slot`
parts. Names exactly cover the distinct slots; repeated slots remain in order.
Parameter types must normalize to primitive string, integer, or Boolean. Port
presence metadata is retained; render argument coverage and value compatibility
are checked at the use site by the verifier/evaluator.

```rust
use htlk_cbor::Limits;
use htlk_executable::{Expression, ExpressionContext, ExpressionKind, Port,
    PrimitiveType, PromptTemplate, ScalarLiteral, TemplatePart, ValueType};

let limits = Limits::default();
let template = PromptTemplate::new(
    vec![("name".parse()?, Port::new(ValueType::primitive(PrimitiveType::String), true))],
    vec![TemplatePart::Text("Hi ".into()), TemplatePart::Slot("name".parse()?)],
    &limits,
)?;
assert_eq!(template.digest(&limits)?.to_string(),
    "sha256:b2a86fc68a03809076995b595fe6a6367de25626bab6b82b808857fd4de52313");
let render = Expression::new(ExpressionKind::Render {
    template: template.digest(&limits)?,
    arguments: vec![("name".parse()?, Expression::literal(ScalarLiteral::String("Ada".into())))],
}, ExpressionContext::Eval, &limits)?;
assert!(matches!(render.kind(), ExpressionKind::Render { .. }));
# Ok::<(), htlk_executable::ExpressionError>(())
```

Construction removes empty literal segments and joins adjacent text without
trimming or Unicode normalization. Canonical ingress rejects such redundant
segments. Adjacent/repeated slots are permitted. The two required wire fields
are `parameters` and `parts`; their maps are closed. A template digest is SHA-256
of raw `htlk.template/0.1\n` followed by canonical record bytes, as specified by
the compiler's record_digest formula. It is not the executable-envelope hash.
Rendering text and calling an LLM are separate later operations.

### Expression/template limits and diagnostics

These models use the existing codec Limits for raw and normalized representations.
Private `RecordAccounting` now also counts byte strings and shortest-exact floats
before conversion copies. Normalized combined paths and text segments must still
fit collection/string ceilings. Traversal and normalization temporaries are
bounded by validated input, with fallible large reservations; the counters are
not exact process-heap measurements or runtime evaluator fuel.

`ExpressionError` covers both expressions and templates, preserving codec,
identifier, digest, and type causes. It reports static schema/context descriptions
rather than retaining submitted data. Codec failures preserve byte offsets; later
graph/source consumers can attach their own locations to higher-level failures.
Expression storage is privately boxed and recursive dispatch delegates branch
temporaries to small helpers. Subprocess tests on 512 KiB and 2 MiB stacks cover
the 128 codec-depth ceiling, deep unary/binary/call trees, cloning/formatting,
normalization, and cleanup after partially decoded trees fail.

## Execution limits and retry policies

`ExecutionLimits` represents the optional local `limits` record on a node or
scope. `new()` and `default()` produce an empty map: each omitted field inherits
its applicable ceiling. An explicit zero call/token/cost budget remains present
and is different from omission. Timeouts and concurrency must be positive.
All values are integers in `0..=i64::MAX`; floats, strings, and null are rejected.

| Field | Meaning | Zero permitted? |
|---|---|---|
| `timeout_ms` | Admitted node/scope duration ceiling | No |
| `attempt_timeout_ms` | Duration ceiling for each MCP attempt | No |
| `max_mcp_calls` | Scope/descendant dispatch-count budget | Yes |
| `max_tokens` | Token budget when enforceable | Yes |
| `max_cost_units` | Budget in the profile's cost units | Yes |
| `max_concurrency` | Concurrent descendant MCP dispatch ceiling | No |

Checked, consuming `with_*` methods set fields; matching read-only getters return
`Option<u64>`. `to_value`/`from_value` and `encode`/`decode` use the existing codec
`Limits`. Unknown fields fail; optional fields are omitted rather than emitted
as null. Numeric validation follows field-name UTF-8 order after unknown-field
checking. An empty local record is valid; the containing policy-profile validator
will require its defaults to include timeout, attempt timeout, and concurrency.

```rust
use htlk_cbor::Limits;
use htlk_executable::{ExecutionLimits, RetryPolicy};

let codec_limits = Limits::default();
let local = ExecutionLimits::new()
    .with_timeout_ms(60000)?
    .with_max_mcp_calls(3)?
    .with_max_concurrency(1)?;
assert_eq!(local.max_tokens(), None); // Inherit; not zero or unlimited.
let wire = local.encode(&codec_limits)?;
assert_eq!(ExecutionLimits::decode(&wire, &codec_limits)?, local);

let retry = RetryPolicy::new(3,
    vec!["MCP_TRANSPORT".into(), "MCP_TIMEOUT".into()],
    vec![1000, 5000], &codec_limits)?;
assert_eq!(retry.on(), ["MCP_TIMEOUT", "MCP_TRANSPORT"]);
assert_eq!(retry.backoff_ms(), [1000, 5000]);
assert_eq!(RetryPolicy::decode(&retry.encode(&codec_limits)?, &codec_limits)?, retry);
# Ok::<(), htlk_executable::ExecutionOptionsError>(())
```

`RetryPolicy` always serializes all three fields: positive `max_attempts`, array
`on`, and array `backoff_ms`. The attempt count includes the initial dispatch;
there must be exactly `max_attempts - 1` delays. Delay entry k-2 precedes attempt
k (one-based). Delays are nonnegative i64-range integers, may decrease or be zero,
and retain their authored order. The parser never allocates from an attempt count.

Construction checks codec limits on the raw policy before sorting/deduplicating
the code set. Codes are opaque exact strings (including custom codes), not
identifiers, patterns, or authority grants. Canonical ingress rejects duplicate
or out-of-order codes rather than repairing them. Unicode spelling is preserved.
`RetryPolicy::no_retry()` and `default()` represent one attempt and empty lists:

```text
{ max_attempts: 1, on: [], backoff_ms: [] }
```

Strict parsing checks unknown fields, missing fields, and top-level native types,
then numeric range, delay cardinality/content, and code types/order. Missing/type
checks use `backoff_ms`, `max_attempts`, `on` order. No exponential policy, jitter,
wildcard matching, or default list contents are inferred from supplied records.

`ExecutionOptionsError` distinguishes schema, numeric, delay-count, order, codec,
conversion-limit, and allocation failures. Errors omit submitted field names and
code strings; codec errors retain byte offsets through their error source.
Private `RecordAccounting` is shared with type conversion and bounds complete
record sizes, text, integers, collections, and depth before conversion copies.
This accounting is per codec operation, not runtime usage accounting.

The runtime still computes effective ceilings, starts persisted deadlines after
admission, reserves and accounts for dispatch usage, checks hard token/cost
enforceability, and authorizes replay based on delivery state, operation policy,
and exact approvals. A valid policy alone does not authorize a retry or revive
a terminal invocation. Retry attachment to MCP-only operations is checked by
the canonical operation schema.

## Canonical graph records

`PortTable` maps validated `Identifier` names to ordinary-value `Port` declarations.
Its read-only iteration is decoded UTF-8/ASCII order, while the codec controls
wire map order. Record-type field names remain arbitrary strings in `ValueType`.

`EdgeSource` selects a complete scope input, node output, or carried value.
`EdgeDestination` selects a node input, public output, or next value. An `Edge`
has an ID and explicit guard; `Edge::new` supplies a literal-true authored default.
There is no edge projection, transform, or implicit input assignment.

`Operation` has the exact canonical variants:

| Rust variant | Wire form |
|---|---|
| Eval | `["eval", expression]` |
| Mcp | `["mcp", binding_digest, retry_policy]` |
| Scope | `["scope", scope_digest]` |
| Loop | `["loop", body_digest, initializers, until, max_iterations]` |
| Wait | `["wait", topic, timeout_ms]` |

Only MCP has a retry slot. Loop/wait bounds are positive signed-i64 integers.
Initializers map carried names to loop-input names; the complete carried table
and type/requiredness equality require the referenced body definition at linkage.
Scopes are referenced by digest, never recursively expanded into parent records.

`NodeFields` and `ScopeFields` are authored field collections. Their constructors
provide true guard/contracts and empty local limits/tables where appropriate.
Pass them to `Node::new` or `Scope::new` to obtain immutable checked definitions.
Read-only `fields()` access permits inspection; cloning fields and rebuilding is
an explicit new definition. Canonical ingress requires every CDDL field and does
not apply authored defaults to incomplete records.

```rust
use htlk_cbor::Limits;
use htlk_executable::{Edge, EdgeDestination, EdgeSource, Expression,
    ExpressionContext, ExpressionKind, Node, NodeFields, Operation, Port,
    PortTable, PrimitiveType, Scope, ScopeContext, ScopeFields, ValueReference,
    ValueType};

let limits = Limits::default();
let string_port = Port::new(ValueType::primitive(PrimitiveType::String), true);
let inputs = PortTable::new(vec![("question".parse()?, string_port.clone())], &limits)?;
let expression = Expression::new(ExpressionKind::Ref {
    source: ValueReference::Input("question".parse()?), path: vec![],
}, ExpressionContext::Eval, &limits)?;
let mut fields = NodeFields::new("echo".parse()?, Operation::Eval(expression));
fields.inputs = inputs.clone();
fields.outputs = PortTable::new(vec![("value".parse()?, string_port.clone())], &limits)?;
let node = Node::new(fields, ScopeContext::Ordinary, &limits)?;
let scope = Scope::new(ScopeFields {
    inputs,
    outputs: PortTable::new(vec![("answer".parse()?, string_port)], &limits)?,
    nodes: vec![node],
    edges: vec![
        Edge::new("supply".parse()?, EdgeSource::Input("question".parse()?),
            EdgeDestination::Input { node: "echo".parse()?, port: "question".parse()? }),
        Edge::new("publish".parse()?,
            EdgeSource::Output { node: "echo".parse()?, port: "value".parse()? },
            EdgeDestination::Output("answer".parse()?)),
    ],
    ..ScopeFields::default()
}, ScopeContext::Ordinary, &limits)?;
let bytes = scope.encode(ScopeContext::Ordinary, &limits)?;
assert_eq!(Scope::decode(&bytes, ScopeContext::Ordinary, &limits)?, scope);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Local checks and use contexts

Authored scope construction sorts node and edge arrays by ASCII ID. Canonical
decoding rejects unsorted or duplicate IDs. Node and edge namespaces are distinct;
a node and an edge may share a name. Reserved expression roots cannot name nodes.
Edges must address declared local nodes/ports and declared boundary destinations,
even when their guard is false. Loop initializer input names must exist on the node.

`ScopeContext::Ordinary` requires empty carried declarations and disallows carried/
next endpoints. `LoopBody` permits them, but requires literal-true local contracts
and empty local limits. The loop node owns its loop contracts and limits. Context
is not serialized or hashed: a role-neutral definition can have the same digest
when used as a task or loop body, with checks performed at each use site.

Node/edge guards select containing-scope expression contexts; eval expressions use
own inputs; loop termination uses settled-body context; scope uses have wrapper
postconditions. Primitive nodes have exactly one `value` output. Eval may declare
optional value presence; MCP/wait success outputs are required. Wait inputs are
exactly required `request: json`. MCP inputs are empty or a required `arguments`
port, with exact binding-specific schema/port matching deferred to linkage.

These are **local record checks**, not complete executable verification. Expression
name/type resolution, self-references, dependency cycles, observability, required
binding coverage/conflicts, referenced scope/binding existence, profile matching,
and loop-body interface compatibility remain shared-verifier responsibilities.
No graph execution or persistence occurs in these constructors.

### Whole-record limits and identities

Every child conversion is bounded by codec Limits. Private accounting then charges
the child at its actual embedded depth and against the containing record's totals
before retaining it. A failed child may temporarily occupy one additional
codec-sized allocation; counters are not exact heap measurements. Scope references
stay digests, and local endpoint lookup is derived rather than serialized.

`Node::digest` and `Scope::digest` cover complete canonical records. The public
`digest::record_digest` helper takes a closed `RecordKind` (scope, node, template,
binding, server) and hashes the raw `htlk.<kind>/0.1\n` prefix plus canonical CBOR.
It validates encoding limits, not arbitrary supplied record schemas. Library
identities and external JSON documents retain their separate digest rules.

`GraphRecordError` retains codec/type/expression/option causes and static structural
diagnostics without copying input names or values into error messages. Tests cover
exact empty-scope bytes, all five record domains, primitive layouts, endpoint and
ordering errors, and whole-scope embedded expression depth/failure cleanup.

## Profiles, libraries, and MCP bindings

`EngineIdentity` records exact `name`, `version`, `data_version`, and
`implementation_digest`. Strings retain their supplied Unicode spelling and
external version labels; the model does not parse version ranges or select a
newer implementation.

`ExecutionProfile` embeds the regex, schema-validator, and URI-template engine
identities, a core implementation digest, and a policy-document digest. Its fixed
format fields are `core_version: "0.1"` and
`mcp_protocol_version: "2025-11-25"`, exposed through `CORE_VERSION` and
`MCP_PROTOCOL_VERSION`. Unsupported profile versions fail rather than being
rewritten. Engine availability, exact implementation matching, and policy-document
JCS/schema validation are later verifier responsibilities.

```rust
use htlk_cbor::Limits;
use htlk_executable::{EngineIdentity, ExecutionProfile};
use htlk_executable::digest::Digest;

let limits = Limits::default();
// Illustrative identities; a linked registry must validate them before execution.
let engine = EngineIdentity::new("example".into(), "1.2.3".into(),
    "data-v1".into(), Digest::from_bytes([1; 32]), &limits)?;
let profile = ExecutionProfile::new(Digest::from_bytes([2; 32]),
    engine.clone(), engine.clone(), engine, Digest::from_bytes([3; 32]), &limits)?;
assert_eq!(profile.core_version(), "0.1");
assert_eq!(profile.mcp_protocol_version(), "2025-11-25");
assert_eq!(ExecutionProfile::decode(&profile.encode(&limits)?, &limits)?, profile);
# Ok::<(), htlk_executable::MetadataError>(())
```

`FunctionSignature` contains ordered `type_parameters`, positional parameter
`Port` records, and one `returns` port. Port types use signature context, so named
variables and function types are permitted. Generic declaration names must be
unique; every variable referenced anywhere in a parameter/result (including
nested record, union, collection, or function types) must be declared by that
signature. Unused declarations are retained. Call-site inference/unification,
recursive inference constraints, and callback compatibility remain verifier work.
Presence flags and both declaration/argument order are preserved.

`Library` contains exact `library_id`, `version`, `implementation_digest`, and
an identifier-keyed `functions` map. Read-only function iteration is decoded
UTF-8 order and lookup uses exact names. Constructors reject duplicate names,
retain every supplied public signature, and never prune to a call-site subset.
The implementation digest is supplied linked-implementation identity, not
record_digest of caller signatures. The verifier must compare the full manifest
with the linked registry before trusting its completeness or behavior.

`ServerIdentity` contains exactly deployment ID, `McpTransport`, implementation
name, and implementation version. Transport is either `stdio` or
`streamable_http`; spelling aliases fail. Connection URLs, credentials, aliases,
and observation timestamps are not identity fields. The host remains responsible
for trusted deployment selection. Its `digest` method uses record_digest("server").

`McpBinding` embeds that server identity and a descriptor digest, plus one
read-only `McpBindingKind`:

| Kind | Additional canonical fields |
|---|---|
| Tool | `name`, `input_schema`, `output_schema` |
| Resource | `uri` |
| Template | `uri_template` |
| Prompt | `name` |

The binding's `digest` method uses record_digest("binding"). External names,
URIs, and template text remain exact strings; there is no universal URI rewrite.
A server-side prompt binding selects an MCP prompt, distinct from a local
`PromptTemplate` used by render expressions. Descriptor/schema bytes, protocol
schema conformance, URI-template interpretation, authorization, and live drift
checks require later catalog/profile/runtime integration.

All six record types have bounded constructors, read-only accessors, and
`to_value`/`from_value`/`encode`/`decode`. Canonical readers require every declared
field and reject unknown fields. A binding's kind selects its exact closed field
set; another kind's fields are not tolerated. `MetadataError` retains codec,
identifier, digest, and type causes, with static schema/version/signature errors
that do not retain untrusted field contents.

Conversions bound each child and then charge it at its real embedded depth and
against aggregate record limits before retaining it. A failing child may occupy
one extra codec-bounded temporary allocation; these are logical counters, not
exact heap measurements. Signature variable validation occurs after size bounds.
Tests cover exact engine bytes, all binding kinds, variable declaration scope,
closed schemas, and deep library-signature embedding/cleanup on controlled stacks.

## External JSON and policy documents

`JsonDocument::new` parses authored UTF-8 JSON and produces RFC 8785 JCS bytes.
`decode` requires those exact canonical bytes; `from_value` explicitly crosses
the native-to-JSON boundary. Accessors borrow normalized `value()` and exact
`as_bytes()`; `digest()` hashes the JCS bytes directly, without an HTLK prefix.

```rust
use htlk_cbor::{Limits, Value};
use htlk_executable::JsonDocument;

let limits = Limits::default();
let json = JsonDocument::new(br#" { "z": 1.0, "a": "\u0061" } "#, &limits)?;
assert_eq!(json.as_bytes(), br#"{"a":"a","z":1}"#);
assert_eq!(JsonDocument::decode(json.as_bytes(), &limits)?, json);
assert_eq!(JsonDocument::new(b"-0", &limits)?.value(), &Value::Integer(0));
assert!(JsonDocument::new(b"9007199254740991.5", &limits).is_err());
# Ok::<(), htlk_executable::JsonError>(())
```

Duplicate decoded keys, malformed UTF-8/escapes, and unpaired surrogates fail.
Strings preserve Unicode spelling; JCS object keys sort by UTF-16 code units.
Mathematically integral numeric tokens are checked against ±(2^53−1) before
binary64 rounding. Fractional conversion is correctly rounded; nonfinite results
and rounded unsafe integers fail. Underflow may yield zero. Safe integral JSON
numbers become native integers, including numbers authored as integral floats at
this explicit boundary. Native bytes cannot cross the JSON boundary.

Parsing checks decoded string sizes before string allocation. `serde_json`
handles string decoding/escaping; `ryu-js` supplies ECMAScript float formatting.
Private per-operation accounting applies existing `Limits` to JSON input/output
bytes, depth, collection entries, values (including keys), individual decoded
UTF-8 strings, and aggregate decoded string payload. These are logical ceilings,
not exact heap counters. JSON is not charged CBOR headers. The parser, writer,
clone, and failure cleanup have depth-128 tests on 512 KiB and 2 MiB stacks.

`PolicyFields` and `EvaluatorLimits` describe the closed policy schema.
`PolicyDocument::new` validates them into JCS; `decode` checks stored policy bytes.
Defaults must supply positive timeout, attempt timeout, and concurrency. All
seven evaluator ceilings and both structural ceilings must be positive safe
JSON integers. Local execution limits still allow signed-i64 values; the tighter
range applies when included in external JSON. Canonical-document assembly enforces
the two structural ceilings; runtime metering and evaluator limits are subsequent work.

## JSON Pointer locations

`JsonPointer::new` parses RFC 6901 plain strings; `from_fragment` parses URI
fragments beginning with `#`. Both use existing codec `Limits`. Immutable decoded
tokens are available through `tokens()`, and Display reconstructs the escaped
plain-pointer form. `resolve(&JsonDocument)` returns a borrowed value without
copying it or recursively traversing the document.

```rust
use htlk_cbor::{Limits, Value};
use htlk_executable::{JsonDocument, JsonPointer};

let limits = Limits::default();
let document = JsonDocument::new(br#"{"a/b":[null,"found"]}"#, &limits)?;
let pointer = JsonPointer::from_fragment("#/a~1b/1", &limits)?;
assert_eq!(pointer.tokens(), &["a/b", "1"]);
assert_eq!(pointer.resolve(&document)?, &Value::Text("found".into()));
assert_eq!(JsonPointer::new("/a~1b/0", &limits)?.resolve(&document)?, &Value::Null);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The empty plain pointer selects the document root; `/` selects an empty object
key. `~1` decodes to slash and `~0` to tilde in one pass, so `~01` means the key
`~1`. Percent encoding is decoded only in URI fragments, before pointer escapes;
`+` is never treated as space. Fragment Unicode uses UTF-8 percent encoding.
Object keys preserve exact Unicode spelling and may contain arbitrary strings.
Array indices require unsigned decimal without leading zeros, except `0` itself.
The `-` token and oversized/out-of-range indices name no existing array element.

Input byte limits apply before parsing. Depth, collection-entry, and total-value
limits each bound token count; text/payload limits bound individual/aggregate
decoded token UTF-8 bytes. Tokens are sized before copying. Fragment decoding
temporarily retains one input-size-bounded buffer before token construction;
logical counters are not exact process-heap measurements. Lookup is iterative,
including at the supported depth ceiling of 128.

`JsonPointerError` reports syntax/fragment byte offsets or the failing zero-based
token index, without retaining submitted keys. A missing target differs from a
present null; traversing a scalar is another explicit failure. Named anchors such
as `#Name` are not pointers. This API is a location primitive for the forthcoming
offline schema resolver: it does not select nested `$id` resources, resolve
anchors, fetch documents, or establish that a target is a schema-bearing location.

## JSON Schema location discovery

`SchemaLocations::new(&JsonDocument, &Limits)` builds a derived index of standard
JSON Schema 2020-12 schema-bearing positions. It exposes the complete document's
`document_digest()`, decoded-token-sorted `pointers()`, and `contains(&JsonPointer)`.
The index preserves document context rather than extracting independent subschemas.

```rust
use htlk_cbor::Limits;
use htlk_executable::{JsonDocument, JsonPointer, SchemaLocations};

let limits = Limits::default();
let document = JsonDocument::new(br#"{
  "properties": { "a/b": { "type": "string" } },
  "examples": [{ "$id": "urn:instance-data" }]
}"#, &limits)?;
let locations = SchemaLocations::new(&document, &limits)?;
assert_eq!(locations.document_digest(), document.digest());
assert!(locations.contains(&JsonPointer::new("/properties/a~1b", &limits)?));
assert!(!locations.contains(&JsonPointer::new("/examples/0", &limits)?));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Object and Boolean schemas are accepted. Discovery follows single-schema keywords
such as `items`, `not`, `contains`, `contentSchema`, and `unevaluatedProperties`;
schema arrays `allOf`, `anyOf`, `oneOf`, and `prefixItems`; and schema maps `$defs`,
`properties`, `patternProperties`, and `dependentSchemas`. Container maps/arrays
are not themselves schema locations. Malformed containers, empty schema arrays,
and scalar/non-schema children fail. `examples`, `default`, `const`, `enum`, and
unknown annotation contents are data and are not traversed for schema declarations.

An omitted `$schema` selects 2020-12. Explicit declarations must identify
`JSON_SCHEMA_DIALECT` (`https://json-schema.org/draft/2020-12/schema`); an empty URI
fragment identifies the same resource. Other dialects fail, including declarations
at nested schema positions. This check does not validate schema keywords beyond
those needed for standard location discovery or establish vocabulary support.

Input JSON is rechecked under effective limits. Discovery is iterative and charges
all queued/indexed locations before allocating pointers: `max_collection_entries`
bounds location count, `max_total_values` counts locations plus retained path tokens,
and `max_total_payload_bytes` bounds their aggregate escaped-pointer bytes.
Each pointer also observes its own pointer limits. Thus an individually valid
JSON document may still exceed derived-index ceilings. Depth-128 discovery and
cleanup are tested on 512 KiB and 2 MiB stacks. `SchemaLocationError` retains static
categories and underlying JSON/pointer errors, without submitted schema contents.

Location discovery is separate from document assembly. `SchemaResources`,
`SchemaCatalog`, and `NativeSchemas` supply resource/anchor interpretation, offline
closure, vocabulary admission and dynamic evaluation; `verify_executable` composes them.

## Schema resources and initial reference targets

`SchemaResources::new(&JsonDocument, retrieval_uri, &Limits)` indexes one schema
document using an absolute, fragment-free retrieval context. It reuses standard
schema-location discovery, follows `$id` only at those locations, and records
the effective base for each location. `resources()` exposes URI-to-pointer roots;
`base_uri()` retrieves a location's effective base, and `document_digest()` ties
all returned locations to the complete source document.

```rust
use htlk_cbor::Limits;
use htlk_executable::{JsonDocument, JsonPointer, SchemaResources};

let limits = Limits::default();
let document = JsonDocument::new(br#"{
  "$defs": { "child": { "$id": "child", "$defs": { "item": true } } }
}"#, &limits)?;
let resources = SchemaResources::new(&document, "https://example.test/root", &limits)?;
let root = JsonPointer::new("", &limits)?;
let target = resources.resolve(&root, "child#/$defs/item", &limits)?;
assert_eq!(target.to_string(), "/$defs/child/$defs/item");
# Ok::<(), Box<dyn std::error::Error>>(())
```

The retrieval URI remains a root alias if the document declares a different root
`$id`. Nested IDs establish resources at their actual subschema pointers. A pointer
fragment is applied relative to the selected resource, then checked against known
schema locations; pointers into `examples` or other instance data fail. `$anchor`
and `$dynamicAnchor` names are scoped to their owning resource. Different locations
claiming the same resource URI or anchor fail; both anchor keywords at the same
location may share a name. `is_dynamic_anchor()` exposes the dynamic declaration.

URI syntax uses `iri-string`'s RFC 3986 parser. Reference resolution follows RFC
3986 §5.2 with bounded intermediate/output strings and literal dot-segment removal.
It preserves percent-encoded dot segments, URI case, and other exact spelling;
it performs no browser-style URL fixups or universal URI-equivalence rewriting.
An ambiguous authority-less `//` result is rejected. Local fragments work against
opaque bases such as synthetic schema URNs; non-fragment relative references
against a nonempty rootless base fail. Percent decoding of fragments occurs before
pointer/anchor lookup, with no form-URL plus-to-space conversion.

Location discovery and resource metadata have separately bounded derived indexes.
The resource index counts stored base/resource/anchor entries against collection
and total-value ceilings, and copied URI/name bytes against aggregate payload.
Individual URI input, intermediate, and output strings observe text, document-byte,
and payload limits. Reference calls apply their supplied limits to query construction;
they do not rebuild the immutable index. Tests cover RFC resolution vectors, exact
limits, nested resources, anchor conflicts, and depth-128 indexing/cleanup on
512 KiB and 2 MiB stacks. Errors retain static categories and underlying location
or pointer errors, without submitted content.

This is a **single-document resource index**, not full executable verification.
Missing external resources return an error without I/O. `resolve` returns the
initial target for either static or dynamic references; evaluation-time dynamic
rebinding remains validator work. Cross-document merging is provided by the catalog
below. Graph-specific reference closure, embedded-tool base selection,
vocabulary/keyword validation, and integration with
the pinned validator and canonical-document verifier remain subsequent steps.

## Cross-document schema catalog

`SchemaCatalog::new(Vec<(String, JsonDocument)>, &Limits)` indexes an explicitly
supplied set of retrieval snapshots. Retrieval URIs are sorted; repeated entries
with identical canonical bytes coalesce, and conflicting retrieval content fails.
All documents receive bounded resource indexes before their URI claims are merged.

```rust
use htlk_cbor::Limits;
use htlk_executable::{JsonDocument, JsonPointer, SchemaCatalog};

let limits = Limits::default();
let catalog = SchemaCatalog::new(vec![
    ("https://example.test/root".into(), JsonDocument::new(b"{}", &limits)?),
    ("https://example.test/other".into(),
        JsonDocument::new(br#"{"$defs":{"item":{"$anchor":"Item","type":"object"}}}"#, &limits)?),
], &limits)?;
let target = catalog.resolve("https://example.test/root",
    &JsonPointer::new("", &limits)?, "other#Item", &limits)?;
assert_eq!(target.retrieval_uri(), "https://example.test/other");
assert_eq!(target.pointer().to_string(), "/$defs/item");
assert_eq!(target.document_digest(), target.document().digest());
# Ok::<(), Box<dyn std::error::Error>>(())
```

`resolve` takes an explicit source retrieval context and schema pointer, resolves
the reference against that location's base, and selects the target resource across
the catalog. `ResolvedSchema` borrows the representative retrieval URI, complete
document, and actual pointer; `value()` borrows the schema at that location.
`retrieval_uris()`, `resource_uris()`, and `document()` expose read-only catalog
contents. `is_dynamic_anchor()` reports initial-target metadata across documents.

Two claims for one resource URI must have identical canonical resource-root
content and equal effective base contexts. This permits an identical resource
copy stored at a document root or nested pointer. Equivalent copies use the first
sorted retrieval URI as their deterministic representative. Different descriptions,
schema content, or base contexts fail with `ResourceConflict`; identical raw JSON
alone is insufficient when relative IDs resolve differently. Explicit retrieval
aliases retain their own indexed contexts. `RetrievalConflict` and `UnknownDocument`
distinguish conflicting snapshots from missing source contexts.

Aggregate accounting includes submitted URI/JSON bytes before duplicate coalescing,
retained child-index paths/bases/anchors, and merged URI keys. Total values and
payload ceilings bound derived storage, while collection limits bound the input
snapshot and merged resource tables. Child indexes retain their individual limits;
a failing child may temporarily occupy one additional bounded index. Returned
targets borrow indexed data rather than allocating extracted schema copies.

The catalog indexes the supplied set and performs initial target lookup without
I/O. Graph-specific closure and used-retrieval-context checks are performed by
`CanonicalDocument::schema_catalog`; unused entries are rejected rather than pruned.
`NativeSchemas` provides evaluation-time dynamic rebinding and pinned validation.

## Conservative schema reference closure

`SchemaCatalog::reference_closure(&[root_retrieval_uris], &Limits)` discovers
document-level dependencies from explicit supplied roots. `SchemaClosure` exposes
sorted reached `retrieval_uris()` and borrowed `references()`. Each `SchemaReference`
retains its source retrieval context, source schema pointer, exact reference string,
`SchemaReferenceKind`, and initial `ResolvedSchema` target.

```rust
use htlk_cbor::Limits;
use htlk_executable::{JsonDocument, SchemaCatalog, SchemaReferenceKind};

let limits = Limits::default();
let catalog = SchemaCatalog::new(vec![
    ("https://example.test/root".into(),
        JsonDocument::new(br#"{"$ref":"other#Item"}"#, &limits)?),
    ("https://example.test/other".into(),
        JsonDocument::new(br#"{"$anchor":"Item","type":"object"}"#, &limits)?),
], &limits)?;
let closure = catalog.reference_closure(&["https://example.test/root"], &limits)?;
assert_eq!(closure.retrieval_uris().len(), 2);
assert_eq!(closure.references()[0].kind(), SchemaReferenceKind::Ref);
assert_eq!(closure.references()[0].target().retrieval_uri(), "https://example.test/other");
# Ok::<(), Box<dyn std::error::Error>>(())
```

Traversal scans `$ref` and `$dynamicRef` at every standard schema location in each
reached complete document, including `$defs`. Instance/annotation data is ignored.
References must be strings. Recursive schema references are permitted: each
retrieval context is visited once, with an ordered iterative work queue rather
than recursive expansion. Reference records are sorted by source URI, decoded
pointer tokens, and keyword, independently of root argument order.

Resources become available when their retrieval context is reached. An unknown
resource URI can load only its exact supplied retrieval entry, never an arbitrary
unreached parent document or equivalent catalog alias. Target checks are deferred
until discovery finishes, so a nested resource may be provided through another
reached reference path. Targets prefer the source's own resource context, then a
deterministic reached context. Consequently a local reference does not pull an
unused equivalent alias into the closure merely because that alias sorts earlier.

Every call uses fresh limits and rechecks reached JSON snapshots. Collection
ceilings bound roots, contexts, known resource URIs, and reference records; total
values account contexts, resources, schema visits, and reference processing.
Payload accounting covers reached URI/snapshot bytes, constructed absolute
references, source pointer paths, and target-root paths needed for pointer joins.
This bounds repeated short-reference lookups into large resource-root pointers.
No document bytes or extracted schema values are copied into returned references.

This is **conservative document-level closure**, not minimal validation-path
reachability or final executable closure enforcement. All standard subschemas of
a reached document contribute references. Dynamic references retain their initial
targets; evaluation-time rebinding remains validator work. Embedded-tool base
selection and canonical `documents`/`schema_uris` integration still require the
shared executable verifier.

## Embedded bases and executable schema checks

`embedded_schema_base(&JsonDocument, &Limits)` selects the prescribed base for an
embedded tool input/output schema. An absolute root `$id` is used exactly, with
an empty fragment removed. Otherwise the base is `urn:htlk:schema:` followed by
the 64 lowercase hexadecimal digits of the schema's raw JCS digest. The JSON
bytes are preserved, and a catalog alias cannot influence this selection.

```rust
use htlk_cbor::Limits;
use htlk_executable::{JsonDocument, embedded_schema_base};

let limits = Limits::default();
let schema = JsonDocument::new(br#"{"type":"object"}"#, &limits)?;
let base = embedded_schema_base(&schema, &limits)?;
assert_eq!(base, format!("urn:htlk:schema:{}", &schema.digest().to_string()[7..]));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Malformed URI metadata and absolute IDs with nonempty fragments fail. A relative
root ID selects the synthetic base, after which normal resource indexing applies:
a non-fragment relative ID cannot resolve against that opaque base. Similarly,
non-fragment relative `$ref` values require a declared hierarchical base; adding
an HTTP catalog alias cannot repair an embedded schema lacking that base.

After record assembly or decoding, call `CanonicalDocument::schema_catalog(&Limits)`
to perform the explicit offline schema stage and obtain a reusable `SchemaCatalog`.
It requires every tool input/output schema at its prescribed base in `schema_uris`,
with the correct document digest. It then traverses reference closure from those
roots, requires every supplied schema retrieval context to be reached, and rejects
JSON documents outside the policy, descriptor, and reached-schema roles.
Entries are checked as assertions, never silently pruned or rekeyed.

The stage rechecks whole-document limits and preflights the aggregate URI/snapshot
bytes before copying catalog inputs, including repeated copies of a document under
multiple aliases. The catalog and closure retain their own derived-index/work
ceilings. `SchemaRootMismatch` distinguishes missing or incorrect root placement;
`DocumentError::Schema` preserves schema/resource/reference failures, and unused
entries use `UnreachableRecord`.

This schema-catalog stage remains explicit rather than being implied by successful
record decoding. `verify_executable` composes callable object-root admission,
keyword/vocabulary validation, dynamic validation support, linked-profile checks,
and graph semantics. Runtime registration remains consumer work. Closure follows all standard schema locations
in reached complete documents, as described above.

## Canonical document assembly

### Native MCP conformance and URI expansion

`validate_mcp_descriptor` checks Tool, Resource, ResourceTemplate, or Prompt
descriptors against the published MCP 2025-11-25 JSON Schema. The pinned source
commit, checksum, and upstream license accompany the declarative snapshot in
`assets/`. Native validators compile this trusted data once; no schema is fetched
while checking submitted descriptors. Protocol extension fields remain preserved.

`CanonicalDocument::validate_mcp_descriptors` composes this with existing binding
integrity and rejects conflicting descriptors selected under the same compound
server/kind/name. Identical descriptor checks are shared across bindings.
`validate_mcp_prompt_result` checks complete prompt messages/content and validates
binary content encoding with a bounded streaming decoder. `validate_resource_snapshot`
checks the closed normalized snapshot fields against a frozen binding/request;
binary content uses native `data: bytes`, and timestamps are not added.

`expand_uri_template` uses the native `iri-string` RFC 6570 implementation with
the exact required string argument record and bounded output. Unicode prefixes,
reserved expansion, percent encoding, named parameters, empty values, and scalar
explode modifiers follow the pinned implementation. There is no script runtime.

```rust
use htlk_cbor::{Limits, Map, Value};
use htlk_executable::expand_uri_template;

let arguments = Value::Map(Map::try_from_entries([
    ("query".into(), Value::Text("hello world".into())),
])?);
assert_eq!(expand_uri_template("https://e.test/search{?query}", &arguments, &Limits::default())?,
    "https://e.test/search?query=hello%20world");
# Ok::<(), Box<dyn std::error::Error>>(())
```

These checks establish protocol/value conformance. Live capability negotiation,
authorization, delivery/retry decisions, and service drift remain runtime work.

### Native schema validation

`NativeSchemas::compile(&SchemaCatalog, NativeSchemaOptions, &Limits)` compiles the
supplied catalog with the shipped native Rust `jsonschema` 0.56.0 dependency. Its
HTTP/file retrieval features are disabled, and both registry preparation and
validator construction use an offline retriever. Each required retrieval root is
compiled explicitly; compilation failures are propagated.

```rust
use htlk_cbor::Limits;
use htlk_executable::{JsonDocument, NativeSchemaOptions, NativeSchemas, SchemaCatalog};

let limits = Limits::default();
let catalog = SchemaCatalog::new(vec![("urn:example".into(),
    JsonDocument::new(br#"{"type":"object","required":["name"]}"#, &limits)?)], &limits)?;
let schemas = NativeSchemas::compile(&catalog, NativeSchemaOptions::default(), &limits)?;
schemas.require_object_root("urn:example")?;
assert!(schemas.validate("urn:example", &JsonDocument::new(br#"{"name":"Ada"}"#, &limits)?, &limits)?);
assert!(!schemas.validate("urn:example", &JsonDocument::new(b"{}", &limits)?, &limits)?);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The backend performs 2020-12 meta-schema and instance validation, including
conditional applicators, unevaluated properties/items, recursive references, and
evaluation-time dynamic-reference rebinding. `format` stays an annotation. Unknown
required vocabularies, including format-assertion, fail; optional unknown keywords
retain annotation behavior. Original JCS data and document digests remain intact.

HTLK resolves resource IDs/references before native compilation. Private compiler
copies replace resource URIs with opaque keys so the native library cannot change
their meaning by normalizing percent-encoded dot segments. Optional unknown
annotations are removed from those compiler copies to prevent legacy keyword
extensions from introducing unindexed resources. These copies are not serialized
executables or alternate document identities.

Schema patterns use the library's ECMA conversion with its linear `regex` backend.
Every declared pattern is checked, including patterns in unused definitions.
`NativeSchemaOptions` bounds pattern text and supported compiled-regex storage;
constructs requiring the backtracking backend are rejected at compilation. There
is no general native validation-fuel, memory, or deadline guarantee. Input/catalog
limits and private compilation-input byte ceilings are enforced, and backend
failures are errors rather than ordinary instance rejection.

`validate_value` uses a temporary JSON representation to apply JSON numeric semantics
without mutating the original native value or replacing its integer/float type.
`document_digest` reports the original root document identity; `identity` describes
the shipped adapter/profile, separate from caller-supplied schema identities.

`CanonicalDocument::native_schemas` composes the executable schema-catalog stage
with native compilation and callable object-root admission. A callable root needs
an explicit `type: "object"` constraint, locally or through a direct static `$ref`
chain; general schema implication is not attempted. The returned compiled validators
are reusable. Graph/expression verification and linked-profile admission are still
separate stages.

`DocumentFields::new(graph_id, profile, root_scope)` starts empty authored tables.
Insert scope, template, binding, complete supplied library-manifest, schema-URI,
and JSON-document entries as needed, then call `CanonicalDocument::new`.
Digest keys are assertions: assembly checks them rather than silently rekeying
or pruning entries. The immutable document exposes borrowed `fields()` plus
`to_value`/`from_value` and `encode`/`decode` with fresh codec limits.

The exact ten wire fields are `ir_version: "0.1"`, `graph_id`, `profile`,
`root_scope`, `scopes`, `templates`, `bindings`, `libraries`, `schema_uris`,
and `documents`. Document values are canonical JCS byte strings. Graph names
use dot-separated identifiers. Retrieval roots are absolute, fragment-free URIs
preserved exactly; URL parsing is a syntax check, not a resolver or rewrite.

Assembly checks root/policy existence, record hashes, raw JCS identities, library
keys against supplied implementation identities, scope-definition acyclicity,
and exact scope/template/binding/library reachability. Scope roles are derived
from use sites before decoding bodies; a shared definition must satisfy every
role in which it is used. Scope interfaces and loop initializer coverage,
types, and requiredness must match. All expression branches contribute static
references, including function references; library function names/arity and
render argument names are checked. Descriptor, tool-schema, schema-URI, and
type-schema references must identify stored documents. Schema types must name
reached tool input/output roots, including types nested inside library signatures.
Complete library manifests
are retained even when only one public function is used.

### MCP descriptor and schema-root integrity

Every reached binding's descriptor must be a JSON object whose selection field
matches exactly: tool/prompt `name`, resource `uri`, or template `uriTemplate`.
These are protocol-owned strings; trimming, case folding, and URI rewriting
are not applied. Descriptor descriptions, extensions, and other contents remain
part of the exact stored JCS identity.

Tools require both `inputSchema` and HTLK-required `outputSchema` objects in the
descriptor. Assembly canonicalizes each extracted object under the JSON limits
and verifies its digest and bytes against the binding's stored schema document.
Supplying individually valid documents under correct hashes does not permit a
binding to substitute a different schema. Missing, null, scalar, or Boolean
schema roots fail this callable-schema boundary. Full object-root admission,
including reference chains, is performed by `NativeSchemas`; an object-shaped
schema alone does not establish that constraint.

Tool nodes require one required `arguments` input with the exact input schema
type and one required `value` output with the exact output schema type. Primitive
or projected substitutes fail. Serialized schema types anywhere in the document
must identify a reached tool input/output root; merely storing an arbitrary
schema or extracting a nested subschema does not make it an admissible type root.
The library manifest remains complete, so this rule also applies to unused public
function signatures in a reached library.

Failures report `InvalidDescriptor` with a static field label,
`ToolSchemaMismatch`, `McpInterfaceMismatch`, or `UnreachedSchemaType`.
Fixed resource reads require no inputs and one required `value: ResourceSnapshot`
output. Prompt fetches require one required `arguments` record and one required
`value: McpPromptResult` output. Prompt record fields exactly match descriptor
argument names, are primitive strings, and use descriptor `required` flags;
omitted flags mean optional. External names are preserved, including names that
are not HTLK identifiers. Omitted or empty descriptor argument arrays still
require a present empty argument record. Duplicate names, malformed argument
objects, non-Boolean required flags, and non-string descriptions fail.

Per-node prompt matching first compares argument counts, then validates and
indexes the matching-size descriptor arguments by borrowed names. Every reached
prompt descriptor is also validated independently. This bounds repeated-use
work by the encoded node interfaces. Complete protocol descriptor validation is
performed by `validate_mcp_descriptors` and included in `verify_executable`.

### Resource-template interfaces

Resource-template nodes require one required `arguments` record containing exactly
the template's distinct variables, all required primitive strings, and one required
`value: ResourceSnapshot` output. Even a template with no variables requires a
present empty arguments record. Repeated references contribute one interface field
while remaining unchanged in the exact template string and binding identity.

The syntax checker follows [RFC 6570](https://www.rfc-editor.org/rfc/rfc6570)
Sections 1.5 and 2: simple expansion and the defined `+`, `#`, `.`, `/`, `;`, `?`,
`&` operators, multiple variables, prefix lengths 1–9999, and explode modifiers.
HTLK arguments are scalar strings even with an explode modifier. Future-reserved
operators, malformed percent triplets, nested/unbalanced braces, invalid names,
invalid modifiers, and forbidden literal characters fail. Relative templates and
the RFC's Unicode literal ranges are accepted; no URI parser rewrites the template.

Variable names are exact and case-sensitive. A dot is part of a name, not field
projection; percent triplets are not decoded or case-folded. `%61` and `a` are
different variables. Names are borrowed from bounded binding strings, collected
once per binding, and reused for node matching. Distinct-variable counts use
`max_collection_entries`; occurrence counts, including repetitions, use
`max_total_values` per template. Encoding under tighter limits rechecks those
derived ceilings. Invalid syntax reports `InvalidUriTemplate` with a byte offset.

This checks syntax and interface metadata. `expand_uri_template` supplies native
scalar expansion, Unicode prefixes, and percent encoding. The composed verifier
matches the pinned URI-template implementation; the runtime owns the resource call.

### Policy structural bounds

Construction and canonical ingress enforce `maximum_scope_depth` and
`maximum_expanded_nodes` from the pinned policy. `structural_summary()` returns an
immutable `StructuralSummary` with `scope_depth()` and `expanded_nodes()` getters.
The summary is derived metadata and adds no canonical fields or digest inputs.

The root counts as depth one. Each nested task or loop body adds one level;
iterations do not add nesting. Every node occurrence counts once, including use
and loop wrappers. Shared bodies count at each use, and each loop multiplies its
body's count by `max_iterations`. False guards and literal-true termination tests
do not reduce the conservative count. An empty root has zero node invocations;
a loop with an empty body contributes only its wrapper at that use site.

For example, a one-node body repeated three times costs four invocations (one
loop wrapper plus three body invocations). Repeating that loop body five times
costs 21: one outer wrapper plus five times four. Scope depth is three.

Leaf-to-root propagation uses iterative reverse dependencies over the stored DAG,
with one contribution per use. Time is O((definitions + uses) log definitions)
and auxiliary storage is O(definitions + uses), independent of expanded counts.
Checked arithmetic rejects overflows as exceeded policy ceilings. Codec nesting
and semantic scope nesting remain separate: flat digest references can describe
a 256-level definition chain without nesting CBOR 256 levels deep. Failures use
`DocumentError::StructuralLimitExceeded` with `StructuralLimit::ScopeDepth` or
`ExpandedNodes` and the exact policy ceiling. These checks reserve no runtime
invocations or storage.

### Assembly boundaries

Each embedded record is bounded before aggregate CBOR accounting charges its real
depth and size. External JSON is independently rechecked under each call's limits.
Scope references remain digests; iterative definition traversal does not expand
task invocations or loop iterations. `envelope()` packages the canonical payload;
`from_envelope()` checks the nested document of an already checked envelope.

**This assembly stage is not full executable verification.** `verify_executable`
composes name/type, wait-dependency, observability, linked-profile, complete MCP,
object-root and offline schema-closure checks. Record assembly accepts extra
external documents/URI roots; the explicit `schema_catalog` stage rejects those
outside its rooted schema closure and policy/descriptor roles.
Registration and authorization remain runtime work. `DocumentError` preserves
structured causes and static diagnostics without retaining submitted values.

## Executable envelope API

```rust
use htlk_cbor::Limits;
use htlk_executable::ExecutableEnvelope;

let limits = Limits::default();
// Payload is opaque here; CBOR null is not a compiled graph.
let envelope = ExecutableEnvelope::new(vec![0xf6], &limits)?;
assert_eq!(envelope.format(), "htlk.executable.graph");
assert_eq!(envelope.version(), "0.1");
assert_eq!(envelope.fingerprint().to_string(),
    "sha256:9b039893d4db25c42f0765d6a31877987d90ff86f07ca593af128ca23500120d");
let bytes = envelope.encode(&limits)?;
let decoded = ExecutableEnvelope::decode(&bytes, &limits)?;
assert_eq!(decoded.payload(), &[0xf6]);
assert_eq!(decoded, envelope);
# Ok::<(), htlk_executable::EnvelopeError>(())
```

`ExecutableEnvelope` has private fields and read-only `format`, `version`,
`fingerprint`, and `payload` accessors. It retains a checked record for encoding
without copying its payload; Debug shows metadata and payload length only.
`EnvelopeError` exposes structured codec, schema, format/version, and fingerprint
failures. `EXECUTABLE_FORMAT` and
`EXECUTABLE_VERSION` expose the supported constants.

## Envelope wire contract — version 0.1

Exactly four required fields form a canonical CBOR map:

| Field | Required representation |
|---|---|
| `format` | Text, exactly `htlk.executable.graph` |
| `version` | Text, exactly `0.1` |
| `fingerprint` | Text, `sha256:` plus exactly 64 lowercase hex digits |
| `payload` | Byte string, preserved exactly; empty is permitted |

Unknown fields are rejected. Canonical wire key order is `format`, `payload`,
`version`, `fingerprint`. The fingerprint is:

```text
SHA256(UTF8("htlk.executable.graph/0.1\n") || payload)
```

The prefix ends in exactly one LF byte; the raw payload follows it directly,
without a CBOR array or byte-string header. Payload bytes are never rewritten
before hashing. All current HTLK-owned format and identity version labels use
`0.1`. Only this envelope contract is supported: alternate format names, version
field aliases, and fingerprint formulas are rejected.

### Golden vectors

An empty payload has fingerprint
`sha256:ebbad418b6ddd9ead246e32dc337b19276b2c709701d2ad69a9992098f9fa14c`.
Payload `f6` has fingerprint
`sha256:9b039893d4db25c42f0765d6a31877987d90ff86f07ca593af128ca23500120d`.
Its complete 137-byte envelope is the following hex (concatenate lines):

```text
a466666f726d61747568746c6b2e65786563757461626c652e6772617068
677061796c6f616441f6
6776657273696f6e63302e31
6b66696e6765727072696e747847
7368613235363a39623033393839336434646232356334326630373635643661333138373739383764393066663836663037636135393361663132386361323335303031323064
```

### Validation and resource accounting

Decode first applies `htlk_cbor::decode` with the supplied `Limits`. Schema
errors then follow this precedence: non-record, unknown fields, missing fields,
wrong field types. Missing/type failures choose the first of `fingerprint`,
`format`, `payload`, `version` (decoded UTF-8 order). Format checking precedes
version checking, then fingerprint parsing, then fingerprint comparison.
Errors do not retain untrusted names, metadata values, or payload contents.
Wrapped codec errors preserve their offsets and remain accessible via Error's
`source`; fingerprint parsing errors likewise retain their structured cause.

Construction takes ownership of the payload and checks its bounds using the
codec before hashing, then verifies that the complete envelope encodes within
the supplied limits. The temporary payload encoding is discarded and never
used as the hash preimage. Encoding borrows the checked record. Decoding hashes
the payload directly after outer validation. A private incremental SHA-256
operation processes the prefix and payload without concatenating or copying
them. Codec encoding allocates bounded temporary output; fixed-size metadata
and hash state are additional overhead, not exact heap accounting.

Every codec operation has fresh accounting. No `RegistrationLimits` type or
shared registration allowance is introduced. A valid outer envelope can contain
malformed CBOR or an invalid graph;
payload interpretation and aggregate registration accounting belong to later
registration stages.

## Digest API

Import these items from `htlk_executable::digest`:

- `Digest`: exactly 32 bytes, with byte construction/access, equality, ordering,
  hashing, canonical Display/Debug, and strict FromStr parsing.
- `ParseDigestError`: invalid prefix, length, or hexadecimal text.
- `hash_bytes`: SHA-256 of exact supplied bytes.
- `hash_cbor`: bounded deterministic CBOR encoding followed by SHA-256.

```rust
use htlk_cbor::{Limits, Value};
use htlk_executable::digest::{Digest, hash_bytes, hash_cbor};

let digest = hash_bytes(b"abc");
assert_eq!(digest.to_string(),
    "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
assert_eq!(digest.to_string().parse::<Digest>()?, digest);
assert_eq!(Digest::from_bytes(*digest.as_bytes()), digest);

let value = Value::Text("abc".into());
let limits = Limits::default();
let encoded = htlk_cbor::encode(&value, &limits)?;
assert_eq!(hash_cbor(&value, &limits)?, hash_bytes(&encoded));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Canonical text

A digest is `sha256:` followed by exactly 64 lowercase hexadecimal digits
(71 ASCII bytes total). Parsing checks the prefix, then byte length, then hex
digits. Uppercase, whitespace, other algorithm prefixes, and non-ASCII hex
lookalikes are rejected. Errors retain no input text. Display and parsing do not
allocate internally; `to_string()` allocates the caller's resulting string.

## Preimages and domain separation

These functions add no labels or versions implicitly. Consumers construct the
exact preimage required by their specification. For example, the runtime's
root-scope identity formula uses this canonical list:

```rust
use htlk_cbor::{Limits, Value};
use htlk_executable::digest::hash_cbor;

let preimage = Value::Array(vec![
    Value::Text("htlk.root_scope".into()),
    Value::Text("0.1".into()),
    Value::Text("example-run".into()),
]);
let root_scope_id = hash_cbor(&preimage, &Limits::default())?;
assert!(root_scope_id.to_string().starts_with("sha256:"));
# Ok::<(), htlk_cbor::Error>(())
```

Hashing `Value::Text("abc")` is different from hashing the raw UTF-8 bytes
`b"abc"`: the former includes the CBOR text header. Integers and floats also
remain distinct. Canonical map ordering makes construction order irrelevant.
The envelope API constructs its agreed preimage above. Artifact and other
runtime-identity preimages remain consumer responsibilities; digest functions
alone do not impose an executable schema or validation policy.

## Limits and trust

`hash_cbor` uses the existing codec limits and allocates a bounded temporary
encoding. Configuration, limit, and allocation errors propagate unchanged; an
unsuccessful encoding returns no digest. Each call starts fresh accounting.
`hash_bytes` borrows its input and uses fixed-size hash state; callers bound raw
input size and work. A valid parsed digest is a representation, not evidence of
matching content, authorization, or producer authenticity.

Licensed under the Apache License, Version 2.0. See `LICENSE`.
