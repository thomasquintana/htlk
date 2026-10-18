# Compiler implementation roadmap

The compiler is a source frontend and orchestration layer over
`htlk-executable` and `htlk-analyzer`. The normative contracts are the
[compiler specification](../specs/compiler-spec.md),
[module specification](../specs/htlk-modules-spec.md),
[syntax reference](../specs/htlk-ir-syntax-reference.md), and
[EBNF](../specs/htlk-ir.ebnf).

## Delivery sequence

1. **Implemented — Configuration and canonical compilation:** fixed profile/policy and complete
   linked manifests; bounded requests and reports; strict three-catalog inputs;
   exact canonical-input admission; shared analysis and envelope encoding.
2. **Implemented — Standalone source:** spanned lexical/syntax analysis, declaration resolution,
   all operation/type/expression/template lowering, unused-declaration validation,
   precise reachable closure, normalization-aware source maps and descriptions.
3. **Implemented — Source packages:** closed bundle validation, exact package digest calculation,
   direct imports and visibility, defining-module resolution, module inspection,
   monolithic/modular executable equivalence.
4. **Implemented — Graph composition:** revalidated canonical operands, public-only bindings,
   deterministic generated edges, conflict-checked table union and whole-result
   verification.
5. **Implemented — Conformance:** worked fixtures, diagnostics/limits, deterministic identity,
   runtime admission interoperability, documentation and workspace quality checks.

## Architectural requirements

- The source AST is temporary. Canonical records are the sole executable graph.
- Validate all declarations in visited modules, including private/unused ones;
  emit only the selected entry's reached closure.
- Validate every supplied package snapshot and version header, but do not parse
  unvisited module bodies.
- Reuse shared typing, schema resolution, graph verification, canonical JSON/CBOR,
  digest and envelope code. Canonical construction alone is not semantic success.
- Match trusted compiler profile, policy and complete library manifests separately
  from semantic analysis. Compilation does not authorize runtime effects.
- Compilation performs no filesystem discovery, network access or external work.
- Preserve native numeric representation, absence/null distinctions, expression
  order and separate invocation occurrences. Check scope legality at each use.
- Source maps, documentation and diagnostics are non-authoritative sidecars.
- Bound frontend work and storage separately from executable codec limits.
- Complete compilation includes standalone, bundle and canonical inputs,
  `inspect_module`, and repeated self-contained `join_graphs` composition.

## Acceptance

The supplied single-file and modular decision-brief examples must produce equal
canonical documents and executable bytes under the same catalogs/profile. Runtime
admission must accept emitted bytes under the matching host registry. Invalid
requests return neither a partial executable nor a purportedly verified interface.

The compiler integration corpus checks these identities against the independent
JavaScript package digest fixtures, validates the example's depth-three / 34-node
structural bound, exercises all operation categories and generic callbacks, and
passes emitted bytes to native runtime admission through the facade integration
test. Compiler results expose immutable shared analysis for checked boundaries.

The core reads finite snapshots only. Host filesystem collection and its symlink/
stable-read rules, authoring/repair applications, optional compilation caches, and
runtime graph scheduling/installation remain their respective consumers' work.

## Completion review

The implementation plan was completed through `590972a` and pushed to origin.
A follow-up review on 2026-10-18 confirmed that its frontend, module, composition,
diagnostic, provenance and accounting follow-ups were covered by the final
conformance work. All 34 compiler integration tests, the compiler doctest and the
five facade interoperability tests passed again, alongside the spec and independent
fixture checkers. No implementation-plan tasks remain; the temporary local plan
has been removed, with this roadmap and commit history retaining the delivery record.
