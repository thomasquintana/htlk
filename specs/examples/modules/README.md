# One decision-brief program, five files, three packages

This is the modular form of the [single-file decision-brief example](../decision_brief.htlk), using the unified **HTLK IR 0.1** baseline for source, packages, executables, catalogs, and runtime records. The source organization changes; its intended execution does not.

## Start with the entry module

Open [app/src/main.htlk](app/src/main.htlk). It imports the definitions it needs and composes task occurrences through ordinary edges. The compiler receives [source_bundle.json](source_bundle.json), not a request to search your disk for those imports.

| File | Public role |
|---|---|
| [app/src/main.htlk](app/src/main.htlk) | Entry graph and connections between tasks |
| [app/src/types.htlk](app/src/types.htlk) | Exported `BriefOutcome` structural type |
| [app/src/prompts.htlk](app/src/prompts.htlk) | Three exported prompt templates used by the entry graph |
| [research/src/search.htlk](research/src/search.htlk) | Exported `find_evidence` task |
| [writing/src/refine.htlk](writing/src/refine.htlk) | Exported `refine` task containing the bounded LLM drafting/review loop |

The three manifests are [app/htlk_package.json](app/htlk_package.json), [research/htlk_package.json](research/htlk_package.json), and [writing/htlk_package.json](writing/htlk_package.json). The app's dependency values are real content digests of the supplied research and writing snapshots, not placeholders. Logical module keys map explicitly to source paths.

An ordinary nested directory can contain several modules inside one package. A separate package boundary requires its own manifest and a dependency pin; directory nesting alone does not create that boundary.

## Compiler and run inputs

Use this bundle as the first argument to `compile_ir`, with the existing [three catalogs](../decision_brief.catalogs.json) as the second argument. The existing [tagged input map](../decision_brief.inputs.json) supplies the question when the resulting executable is started. These are logical API instructions, not a claim that a compiler SDK or command-line application is installed.

For inspection, request the `find_evidence` export from the research package's `search` module using `inspect_module`. Take its exact `package_digest` from the app manifest. The interface describes ports, contracts, and implementation identity without including the task body. Give that report and the caller module to an LLM when composing a caller; provide the body only when editing or investigating the research task itself.

## What should remain identical

The module form retains the same root graph name, child node/edge IDs, tool selections, prompts, predicates, and loop bounds. Exported task names and import aliases resolve to the same underlying definitions. Its required acceptance test is byte-identical canonical output to the single-file program under the same catalogs and execution profile.

The documentation checker parses all five files, checks imports/exports in this fixture, reconstructs the original single-file declarations for a token comparison, and recomputes the package-map digests using the supplied digest helper. It also compares every manifest and source in the JSON bundle with its disk counterpart. It does not compile these graphs or establish executable byte equivalence; that requires the eventual HTLK compiler.

## Editing the example

The bundle is a captured copy of the source, not a live filesystem view. After editing a file, rebuild its source package snapshot and digest. If a dependency changed, update its exact pin in the app manifest and rebuild the app snapshot too. A changed comment can change source digests without changing the executable's meaning.

Keep the checked-in files, manifests, and bundle synchronized. The document checker intentionally fails if they diverge. The [module specification](../../htlk-modules-spec.md) defines the precise collection, digest, visibility, and version rules. No edit automatically changes an existing runtime execution.
