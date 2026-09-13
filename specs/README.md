# HTLK IR specification bundle — Draft 0.1

HTLK is the Harness Toolkit. HTLK IR describes tasks as explicit graphs that a compiler checks and a runtime executes. These files specify the language and system; they do not contain a released compiler or runtime implementation.

## Start here

- [Modules and source packages](htlk-modules-spec.md): the new compiler design for composing programs across files and package directories, with four numbered diagrams.
- [Multi-file decision-brief example](examples/modules/README.md): five modules, three packages, exact dependency pins, and a complete source bundle.
- [User guide](htlk-ir-user-guide.md): learn graph composition through an LLM research, drafting, and review workflow.
- [Compiler specification](compiler-spec.md): inputs, catalogs, verification, graph composition, and deterministic serialization.
- [Syntax reference](htlk-ir-syntax-reference.md) and [EBNF](htlk-ir.ebnf): exact source forms and grammar.
- [Language specification](htlk-grammar-spec.md): meanings and relationships of the language constructs.
- [Runtime specification](runtime-spec.md) and [executable CDDL](htlk-executable.cddl): execution, recovery, and portable graph records.
- [Change log](CHANGELOG.md): the unified baseline and migration notes.

## Version baseline

Every HTLK-owned format uses version `0.1`: source, manifests, bundles, interface reports, executable records, catalogs, evaluator core, and runtime journal records. External MCP, JSON Schema, and third-party implementation identities retain their actual versions. Modules are resolved by the compiler; the runtime does not load source files.

The executable envelope uses `format: "htlk.executable.graph"` and `version: "0.1"`.
Its fingerprint hashes `UTF8("htlk.executable.graph/0.1\n") || payload` with one LF
byte in the prefix and exact raw payload bytes. No alternate envelope format,
version-field alias, or fingerprint formula is supported.

Contract fields are `preconditions` and `postconditions`, each containing one Boolean expression. This baseline resets earlier draft version labels and renames contract record keys, which changes affected bytes and content identifiers. Earlier source must be migrated and recompiled; stored executables and runtime history must not simply be relabeled.

The separately distributed runtime video is a historical earlier-draft recording. It has not been revised or relabeled and may show previous keywords and versions. These current text specifications are authoritative for the 0.1 baseline.

## Checks

With Node.js installed, run `node validate-specs.mjs` from this directory. The checker verifies grammar/example consistency, local links and diagram captions, source package digests, and the split example's correspondence to the single-file tutorial. It does not type-check or execute HTLK programs. Executable byte equivalence and behavioral conformance remain requirements for the eventual implementation, not claimed test results.

Earlier version archives are retained separately; this archive contains the current specification and source examples, not the video/media bundle.
