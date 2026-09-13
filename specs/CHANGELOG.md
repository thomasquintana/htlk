# HTLK IR specification baseline — Draft 0.1

**Release:** Draft 0.1\
**Date:** 2026-09-13

## 1. One version for the current specification

This release establishes `0.1` as the version of every current HTLK-owned format. It retains the graph model, modular authoring, and execution rules already developed, while bringing the language, compiler, runtime, examples, and machine-readable references into one baseline.

| HTLK-owned item | Current version |
|---|---|
| Specification and source `ir_version` | `0.1` |
| Source package manifest, source bundle, and module interface report | `0.1` |
| Canonical document, executable envelope, and evaluator core | `0.1` |
| MCP catalog format and runtime journal record format | `0.1` |
| Version labels in HTLK content-hash formulas | `0.1` |
| Example source packages and illustrative built-in function libraries | `0.1` |

External identities do not become HTLK versions. The selected MCP protocol remains `2025-11-25`, JSON Schema retains its `2020-12` dialect identifier, and external server implementation versions retain their reported values.

The executable envelope has exactly `format: "htlk.executable.graph"`,
`version: "0.1"`, `fingerprint`, and byte-string `payload`. Its fingerprint is
SHA-256 of `UTF8("htlk.executable.graph/0.1\n") || payload`, with one LF byte in
the prefix. This is the sole supported envelope contract; other format names,
version-field aliases, and hash formulas are rejected. Source bundles and other
formats retain their own version-field names.

Earlier archives and the runtime video are historical copies, preserved separately without relabeling. They may contain earlier syntax and version numbers. The current text documents define the 0.1 baseline.

## 2. Explicit contract names

A **precondition** checks whether inputs are acceptable before work starts. A **postcondition** checks a proposed result before the runtime accepts and publishes it. Together they form a node or scope's **contract**.

| Earlier field or error code | Current spelling |
|---|---|
| `requires = expression` | `preconditions = expression` |
| `ensures = expression` | `postconditions = expression` |
| `E_REQUIRES` | `E_PRECONDITIONS` |
| `E_ENSURES` | `E_POSTCONDITIONS` |

The names are single words. Each field is optional, occurs at most once, and contains one Boolean expression. Combine checks with `and` or `or`; use parentheses to make grouping explicit. An omitted field means `true`. The plural names do not add a list or block form, and the previous spellings are not aliases.

These names apply consistently to source, canonical executable records, module interface reports, diagrams, and current examples. The [syntax reference](htlk-ir-syntax-reference.md), [EBNF](htlk-ir.ebnf), and [executable CDDL](htlk-executable.cddl) agree on the fields. A false postcondition prevents acceptance of the proposed outputs but cannot reverse an external action that has already happened.

## 3. Migration and content identifiers

The execution mechanisms and contract evaluation order are retained, but this is not a byte-compatible relabeling. Version labels and canonical field names participate in encoded data and hash calculations. Changing them changes the affected executable fingerprints, source package digests, and other content identifiers.

To use material from an earlier draft:

1. Migrate source contract fields and any authored checks that match the renamed error codes.
2. Update HTLK version fields to `0.1` and check the entire source against the current grammar and semantic rules.
3. Rebuild source snapshots and their exact dependency pins, then recompile against the selected catalogs and execution profile.
4. Preserve existing executable bytes and runtime history under the rules that produced them. Do not rewrite old journal versions or assume a new runtime can resume old records without a separately defined migration.

Matching the `0.1` label alone does not establish compatibility or correctness. Readers must still validate the format, contents, and exact execution profile. Within this baseline, moving unchanged definitions between files must preserve executable bytes when the resolved graph, graph ID, catalogs, and profile are unchanged.

## 4. Modular authoring remains part of the baseline

A module is one source file. A source package supplies explicit module-to-file mappings and exact dependency pins. Declarations are private unless exported as a task, type, or prompt. Static imports select only modules present in the supplied source bundle; the compiler does not search directories, fetch dependencies, or run build scripts.

A reusable task remains a composite subgraph, and `use(task_name)` creates a normal scope occurrence. Importing a definition does not execute it. Module interface reports let an author or LLM inspect selected public contracts without reading every implementation, but compilation still checks the actual source rather than trusting those reports as executable substitutes.

The [module specification](htlk-modules-spec.md) explains these relationships with numbered diagrams. The [worked multi-file example](examples/modules/README.md) contains five modules, three packages, regenerated dependency pins, and a synchronized source bundle.

## 5. Validation scope

Run `node validate-specs.mjs` from this directory. The checker verifies the grammar copies, source examples, diagram captions, local links, catalog fixtures, source snapshots and digests, and the correspondence between the single-file and modular tutorial.

These are documentation and source-fixture checks. They do not execute an HTLK compiler or runtime, prove semantic correctness, or establish compiled-byte equivalence. Those remain implementation acceptance requirements in the [compiler](compiler-spec.md), [runtime](runtime-spec.md), and [module](htlk-modules-spec.md) specifications.
