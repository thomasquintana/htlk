# htlk-executable completion checklist (Draft 0.1)

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

These component APIs establish representation and structural integrity.
`verify_executable` is the composed admission boundary; a successful lower-level
`CanonicalDocument` decode alone does not establish a fully verified runnable graph.

## Completion status and remaining work

Tasks 1–7 are implemented and covered by component and composed verification tests.
The authoritative result is `VerifiedExecutable`, which pins host policy, schemas and the native
registry and borrows admitted execution plans without per-evaluation plan copying.

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
  checks with native validation; `verify_executable` includes this stage.

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

- [x] Resolve references/outcomes against supplied expression environments in all
  branches, including branches skipped by lazy execution.
- [x] Type-check Boolean conditions, operators, structural projections, collections,
  templates, and explicitly supplied result boundaries.
- [x] Enforce expression/call/template presence separately from nullability through
  checked evaluation, preserving absence, pending, and operational errors.
- [x] Implement rank-one generic inference/unification for library calls; reject
  unresolved variables, recursive inferred types, and incompatible arguments/results.
- [x] Enforce static function-reference placement and callback signature compatibility.
  Function references must not become ordinary application values or dynamic code.
- [x] Compile/check HTLK regex literals with the pinned native engine and configured
  size/work ceilings. Exact engine/Unicode/profile identity matching belongs to task 5.
- [x] Retain independently instantiated native-call and callback signatures; provide
  metered checked callback invocation for ordinary value arguments and results.
- [x] Build graph-derived expression environments and apply checking to every guard,
  contract, eval result, public/proposed output, and loop-until use site (with task 4).
- [x] Complete higher-order callable argument handling and route native/callback
  dispatch through an exactly linked checked registry (with task 5).
- [x] Produce enforceable value-validation/boundary plans for uncertain schema
  refinements; reject known disjoint shapes without requiring general schema subtyping.
  Schema-root/location-aware projections preserve native representations and dynamic
  context. Permitted additional/pattern properties are accessible; missing dynamic
  keys error and explicitly declared optional properties produce absence.

### 4. Complete scope-level graph verification

- [x] Check candidate binding coverage for every required node input, public output,
  and loop-next destination; reject multiple unconditional writers.
- [x] Check whole-port assignability and presence across edges, retaining conditional
  uniqueness checks for execution rather than requiring SAT/SMT proofs.
- [x] Construct the full **wait-dependency graph**, including admission, outcomes,
  input binding, public-output binding, loop-next binding, optional references,
  and every condition branch. Reject hidden cycles, not just data-edge cycles.
- [x] Reject illegal self-outcome dependencies and finish use-site contract checks.
- [x] Enforce observability: every child must reach a public output, a permitted
  scope-completion outcome check, or loop-next/termination behavior.
- [x] Finish loop-until, next-binding, and wrapper semantics in the shared verifier.
  Definition acyclicity, finite bounds, initializer/interface equality, and policy
  expansion/depth accounting already exist.

### 5. Match linked implementations and policy support

- [x] Settle the default inventory: the specified core and native engines, plus an
  extensible checked registry for exact host-linked libraries. A newly designed
  standard library is outside the Draft 0.1 completion scope (user option 1).
- [x] Define registry/profile interfaces and compare exact core, regex, schema,
  URI-template, and library implementation identities against linked implementations.
- [x] Compare complete supplied library manifests with the registry, including
  signatures, presence rules, generic declarations, purity, and deterministic work
  charging. Caller-supplied implementation digests alone are not verification.
  Host-linked implementations carry the trusted purity/work contract; the registry
  enforces exact function coverage, positive prepaid dispatch, and checked callback
  routing, including higher-order forwarding of admitted static callback slots.
- [x] Validate policy/evaluator ceilings against capabilities the chosen profile
  can actually enforce; keep configuration separate from execution counters.
  Host policy selection is exact and explicit through `NativeRegistry::link_policy`;
  submitted policy bytes cannot select themselves. The native-schema capability
  contract explicitly excludes whole-validation fuel/hard in-process deadline claims.

### 6. Expose one complete verification boundary

- [x] Compose envelope, canonical document, schema catalog, descriptor, expression,
  graph, and linked-profile checks into one shared compiler/runtime verification API.
- [x] Return an immutable verified result that cannot be constructed through the
  ordinary record constructors; make the distinction from assembly explicit.
- [x] Attach structured canonical locations to diagnostics (scope/node/edge,
  expression path, schema document/pointer), retaining underlying causes while
  keeping public failures free of submitted secrets/content.
- [x] Produce deterministic validation reports, checked-boundary plans, and any
  necessary derived indexes without serializing a competing graph representation.
- [x] Define complete-operation allocation/work accounting and failure cleanup
  across composed stages; reuse existing finite limits rather than introducing
  a public stateful codec session or an unnecessary `RegistrationLimits` type.

### 7. Finish conformance and release documentation

- [x] Add independently checked complete executable byte/hash fixtures and a
  valid/invalid verifier corpus covering all operation, binding, and context forms.
- [x] Add adversarial/property/fuzz coverage for the composed verification boundary,
  especially cyclic references, dynamic resources, aliases, hidden dependencies,
  malformed inputs, work amplification, and exact limit boundaries.
- [x] Verify deterministic results across construction order and unused declarations,
  plus rejection of malformed nested payloads inside otherwise valid envelopes.
- [x] Document the verified API, supported profile/validator behavior, diagnostics,
  resource ceilings, and the final implemented-versus-pending boundary.

## Consumer integration

Compiler source parsing, modules/imports, inspection, and graph joins belong to
`htlk-compiler`; actor execution, persistence, registration transactions, deadlines,
budgets, authorization, and live MCP I/O belong to `htlk-rt`. Their integration
tests must exercise the same shared verifier before compiler output or runtime
registration is accepted. These consumer implementations are separate from the
completed shared format/verifier work listed above.

## Completion criterion

`htlk-executable` is complete for Draft 0.1 when it can independently validate a
bounded executable envelope against the supplied linked profile, return a fully
verified immutable graph with enforceable boundary plans, and pass the complete
conformance corpus. Publishing the current package or building its API docs does
not by itself establish that milestone.

## Verification evidence

- `tests/verified.rs` exercises complete envelope admission, all operation and MCP
  binding kinds, exact host policy/profile linking, immutable checked execution,
  reuse, malformed nested payloads, and controlled-stack verification/cleanup.
- `tests/graph_verify.rs` covers hidden cycles, all condition branches, optional
  inputs, writer/coverage rules, explicit observability, per-use loop termination,
  structured expression locations, derived-plan amplification, and an independent
  generated-graph cycle oracle.
- `tests/schema_projection.rs` covers permitted dynamic fields, optional declarations,
  original reference/dynamic context, numeric representation preservation, work
  boundaries, schema-family refinement and redacted schema diagnostics.
- `tests/checked_calls.rs` covers ordinary/higher-order callback boundaries, generic
  inference, original callable locations, exact manifest dispatch and prepaid work.
- `tests/fixtures/native-empty.hex` is a fixed complete executable. The Rust suite
  round-trips it and rejects every single-bit mutation. The independent Node checker
  verifies its canonical CBOR, JCS, envelope/scope/policy hashes and native identities.
- CI and release validation run the independent fixture checker alongside the Rust
  and specification suites. The workspace has 272 tests and 27 documentation examples;
  formatting, Clippy, rustdoc, package verification and dependency policy are checked.
