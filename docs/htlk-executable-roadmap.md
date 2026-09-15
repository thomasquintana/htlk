# Completing htlk-executable (Draft 0.1)

This checklist covers the shared executable format and verifier used by the
compiler and runtime. Its authority is `specs/compiler-spec.md`,
`specs/htlk-executable.cddl`, and `specs/runtime-spec.md`.

## Implemented foundation

- Deterministic bounded CBOR, digest domains, exact executable envelopes.
- Identifiers, canonical value types/ports, execution options, expressions and
  templates, graph records, execution profiles, library signatures and MCP bindings.
- Bounded JSON/JCS and policy documents, canonical document assembly, record/table
  identity checks, known-reference checks, and scope-definition closure/interfaces.
- Policy scope-depth and expanded-invocation bounds, including reused bodies/loops.
- MCP binding selection, exact tool-schema identity and port checks, resource/prompt
  interfaces, and RFC 6570 template syntax/variable interfaces.
- JSON Pointers, schema-location/resource/anchor indexes, cross-document catalogs,
  conservative reference closure, embedded schema bases, and an explicit
  `CanonicalDocument::schema_catalog` stage checking known external closure.

These APIs establish representation and substantial structural integrity. A
successfully decoded `CanonicalDocument` is not yet a fully verified runnable graph.

## Remaining work, in recommended order

### 1. Finish schema admission and validation

- [x] Enforce callable input/output **object-root** constraints: accept a direct
  `type: "object"` constraint or an admissible reference chain to one, and reject
  scalar/list-only or otherwise unproven roots under the draft's baseline rule.
- [x] Integrate a shipped native JSON Schema 2020-12 validator through a shared
  interface, including schema/meta-schema keyword validation, required-vocabulary
  support and recursive schemas. Apply input/catalog limits and backend-supported
  regex bounds; report supported capabilities without claiming a general native
  validation-fuel counter or hard in-process execution deadline.
- [x] Implement evaluation-time `$dynamicRef`/`$dynamicAnchor` semantics through
  native validation; standalone resource lookup/closure still records initial targets.
- [x] Validate schema regexes with the native schema validator's dialect; preserve
  `format` as annotation, and reject unsupported required behavior rather than
  silently treating it as successful validation.
- [x] Validate actual JSON/native values against schema constraints without
  introducing coercion, defaults, omitted-null behavior, or renamed external keys.
- [x] Integrate schema bases, resource conflicts, reference
  targets, and exact external closure, including all resources potentially used
  by supported dynamic/vocabulary semantics. Current document-level closure is
  deliberately conservative. `CanonicalDocument::native_schemas` composes these
  checks with native validation; the final unified verifier API remains task 6.

### 2. Complete MCP descriptor conformance

- [x] Validate full selected descriptors against MCP **2025-11-25** protocol schemas,
  including optional metadata/content fields and kind-specific requirements.
  Selection fields, tool schema identity, and operation interfaces are implemented.
- [x] Check consistency of repeated selections of the same compound server and
  descriptor identity across the executable, rejecting conflicting pinned facts.
- [x] Connect resource-template expansion to the native implementation and
  verify scalar expansion, Unicode prefix limits, percent encoding, and modifiers.
  RFC 6570 syntax, interfaces, and bounded scalar expansion are implemented.
- [x] Provide shared validation contracts for normalized `ResourceSnapshot` and
  `McpPromptResult` values; the runtime still owns live MCP calls and drift handling.

### 3. Resolve and type-check expressions

- [ ] Resolve actual node, port, carried/next, and outcome names in each expression
  context. Current category checks do not establish every referenced name's existence.
- [ ] Type-check conditions as Boolean and verify operator operands, projections,
  literals/collections, proposed outputs, and declared eval result types.
- [ ] Enforce absence/requiredness separately from nullability throughout expressions,
  calls, contracts, template rendering, and connected ports.
- [ ] Implement rank-one generic inference/unification for library calls; reject
  unresolved variables, recursive inferred types, and incompatible arguments/results.
- [ ] Enforce static function-reference placement and callback signature compatibility.
  Function references must not become ordinary application values or dynamic code.
- [ ] Compile/check HTLK regex literals using the pinned Rust-regex-compatible
  engine, Unicode data, implementation identity, and configured size/work ceilings.
- [ ] Produce enforceable value-validation/boundary plans for uncertain schema
  refinements; reject known disjoint shapes without requiring general schema subtyping.

### 4. Complete scope-level graph verification

- [ ] Check candidate binding coverage for every required node input, public output,
  and loop-next destination; reject multiple unconditional writers.
- [ ] Check whole-port assignability and presence across edges, retaining conditional
  uniqueness checks for execution rather than requiring SAT/SMT proofs.
- [ ] Construct the full **wait-dependency graph**, including admission, outcomes,
  input binding, public-output binding, loop-next binding, optional references,
  and every condition branch. Reject hidden cycles, not just data-edge cycles.
- [ ] Reject illegal self-outcome dependencies and finish use-site contract checks.
- [ ] Enforce observability: every child must reach a public output, a permitted
  scope-completion outcome check, or loop-next/termination behavior.
- [ ] Finish loop-until, next-binding, and wrapper semantics in the shared verifier.
  Definition acyclicity, finite bounds, initializer/interface equality, and policy
  expansion/depth accounting already exist.

### 5. Match linked implementations and policy support

- [ ] Define registry/profile interfaces and compare exact core, regex, schema,
  URI-template, and library implementation identities against linked implementations.
- [ ] Compare complete supplied library manifests with the registry, including
  signatures, presence rules, generic declarations, purity, and deterministic work
  charging. Caller-supplied implementation digests alone are not verification.
- [ ] Validate policy/evaluator ceilings against capabilities the chosen profile
  can actually enforce; keep configuration separate from execution counters.

### 6. Expose one complete verification boundary

- [ ] Compose envelope, canonical document, schema catalog, descriptor, expression,
  graph, and linked-profile checks into one shared compiler/runtime verification API.
- [ ] Return an immutable verified result that cannot be constructed through the
  ordinary record constructors; make the distinction from assembly explicit.
- [ ] Attach structured canonical locations to diagnostics (scope/node/edge,
  expression path, schema document/pointer), retaining underlying causes while
  keeping public failures free of submitted secrets/content.
- [ ] Produce deterministic validation reports, checked-boundary plans, and any
  necessary derived indexes without serializing a competing graph representation.
- [ ] Define complete-operation allocation/work accounting and failure cleanup
  across composed stages; reuse existing finite limits rather than introducing
  a public stateful codec session or an unnecessary `RegistrationLimits` type.

### 7. Finish conformance and release documentation

- [ ] Add independently checked complete executable byte/hash fixtures and a
  valid/invalid verifier corpus covering all operation, binding, and context forms.
- [ ] Add adversarial/property/fuzz coverage for the composed verification boundary,
  especially cyclic references, dynamic resources, aliases, hidden dependencies,
  malformed inputs, work amplification, and exact limit boundaries.
- [ ] Verify deterministic results across construction order and unused declarations,
  plus rejection of malformed nested payloads inside otherwise valid envelopes.
- [ ] Document the verified API, supported profile/validator behavior, diagnostics,
  resource ceilings, and the final implemented-versus-pending boundary.

## Consumer integration

Compiler source parsing, modules/imports, inspection, and graph joins belong to
`htlk-compiler`; actor execution, persistence, registration transactions, deadlines,
budgets, authorization, and live MCP I/O belong to `htlk-rt`. Their integration
tests must exercise the same shared verifier before compiler output or runtime
registration is accepted. These consumer implementations are separate from the
remaining format/verifier work listed above.

## Completion criterion

`htlk-executable` is complete for Draft 0.1 when it can independently validate a
bounded executable envelope against the supplied linked profile, return a fully
verified immutable graph with enforceable boundary plans, and pass the complete
conformance corpus. Publishing the current package or building its API docs does
not by itself establish that milestone.
