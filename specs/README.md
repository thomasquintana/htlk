# HTLK specifications — Draft 0.1

These documents define the language and system contracts. For a practical starting
point, use the [user guide](../docs/user-guide.md).

| Authority | Owns |
|---|---|
| [Language reference](htlk-ir-syntax-reference.md) | Source syntax, types, scope, binding, completion, loops, and expression semantics. |
| [Compiler specification](compiler-spec.md) | Catalogs, source packages/modules, interface reports, static verification, composition, canonical representation, and identity. |
| [Runtime specification](runtime-spec.md) | Execution, effects, authorization, persistence, recovery, extension, and stable failures. |
| [MCP result specification](mcp-spec.md) | Result envelopes, extraction, assessments, and schema inference/adoption. |
| [MCP client specification](mcp-client-spec.md) | Bidirectional client services, session ownership, interactions, delivery, and recovery; consult the linked conformance inventory for implementation status. |
| [EBNF](htlk-ir.ebnf) | Machine-readable source grammar, checked against the language reference. |
| [Executable CDDL](htlk-executable.cddl) | Machine-readable portable record shapes; semantic verification is additionally required. |

The [worked examples](examples/README.md) include standalone and modular source,
catalogs, and inputs consumed by the compiler/runtime test suites. Examples and
tutorials explain the contracts; they do not introduce competing rules. Agent
repository instructions live in [AGENTS.md](../AGENTS.md).

## Version baseline

All HTLK-owned formats use `0.1`: source, source manifests/bundles, logical interface
reports, canonical/executable records, catalogs, evaluator core, and runtime journal
records. External MCP (`2025-11-25`), JSON Schema (`2020-12`), library versions, and
third-party implementation identities retain their actual versions. The private
SQLite schema version is independent of these format labels.

The executable envelope has exactly `format`, `version`, `fingerprint`, and byte-string
`payload`. Its format is `htlk.executable.graph`, its version is `0.1`, and its
fingerprint hashes `UTF8("htlk.executable.graph/0.1\n") || payload` with one LF byte
and the exact raw payload. Alternate formats, aliases, and formulas are rejected.

Contract fields are `preconditions` and `postconditions`, each one optional Boolean
expression. Omission means true. Older `requires`/`ensures` spellings and their error
codes are not aliases. Earlier draft source must be explicitly updated, package
snapshots/pins rebuilt, and executables recompiled. Never relabel stored executable
bytes, fingerprints, or journal history. Matching version labels alone cannot
establish compatibility; the runtime requires exact implementation/profile selection
and rejects incompatible private stores.

## Validation

From the repository root:

```console
node tools/validate-docs.mjs
node specs/validate-specs.mjs
node tools/validate-executable-fixtures.mjs
```

These check Markdown links/anchors, grammar/examples, synchronized module snapshots
and package digests, and independent executable/hash fixtures. The spec checker is
not a semantic compiler or full CDDL instance validator. Rust compilation,
doctests, and runtime scenarios provide separate evidence; see [testing](../docs/testing.md).
