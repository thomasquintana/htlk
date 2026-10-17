# htlk-compiler

HTLK IR Draft 0.1 compilation for the [Harness Toolkit](https://crates.io/crates/htlk).

`Compiler` compiles standalone `.htlk` text, finite source bundles, and canonical
documents into self-contained deterministic CBOR executables. It also generates
checked module interfaces and joins canonical graphs through public ports.

## Compile source

```rust
use htlk_compiler::{CompilationFailure, Compiler, CompilerConfig, MCPCatalogInputs};

fn compile(config: CompilerConfig, catalogs: &MCPCatalogInputs)
    -> Result<Vec<u8>, CompilationFailure>
{
    let compiler = Compiler::new(config)?;
    let result = compiler.compile_ir(r#"
        ir_version = "0.1"
        graph example.pass {
            inputs = { message = string }
            outputs = { message = string }
            nodes {}
            edges {
                edge message { from = inputs.message to = outputs.message }
            }
        }
    "#, catalogs)?;
    assert_eq!(result.fingerprint, result.analysis().fingerprint());
    Ok(result.executable_bytes)
}
```

The host constructs a fixed `CompilerConfig` with an exact `ExecutionProfile`,
`PolicyDocument`, and complete linked-library manifests. Native hosts can obtain
these from `htlk_runtime::native_profile`, `NativeRegistry::link_policy`, and
`NativeRegistry::library`. Configuration is trusted host/build data. The three
MCP catalogs remain the only operation inventories supplied to compilation.

`MCPCatalogInputs::from_json` parses the complete tools/resources/prompts object,
rejecting duplicate decoded keys, unknown owned fields, conflicting aliases and
descriptors, invalid protocol versions, and digest assertion mismatches. Resolution
uses supplied snapshots exclusively. Unselected protocol-valid tools need not have
HTLK's callable `outputSchema`.

## Public operations

| Operation | Input and result |
|---|---|
| `compile_ir` | `&str`, `&SourceBundle`, or owned `CanonicalDocument`; returns `CompilationSuccess` |
| `compile_source` / `compile_bundle` / `compile_document` | Explicit forms of the same compilation paths |
| `inspect_module` | Validated bundle, exact `ModuleRef`, and selected export names; returns `ModuleInspectionSuccess` |
| `join_graphs` | Two canonical documents and a `JoinSpec`; returns a new self-contained compilation result |

`SourceBundle::from_json` and `JoinSpec::from_json` provide bounded duplicate-aware
closed-record ingress. Typed source-package records are revalidated for each request.
`SourcePackage::digest` computes the specified exact source snapshot identity.

Every visited module's declarations are checked, including unused/private ones;
only the entry graph's reached closure is emitted. Every supplied source's header
and package pin is checked, while unvisited module bodies are not parsed or typed.
Imports resolve exported definitions in their defining module. Inspection validates
the actual target implementation/import closure even for an index-only request;
it does not additionally compile the bundle entry graph.

## Output and diagnostics

`CompilationSuccess` contains the canonical document, envelope bytes, fingerprint,
diagnostics, source maps and retained descriptions. Its `analysis()` accessor
exposes immutable inferred expressions, checked boundaries, runtime obligations,
binding plans and structural bounds for the exact document that was checked.

Diagnostics carry stable specification codes, source spans or canonical/bundle
pointers, and related locations. Spans use exclusive UTF-8 byte ranges. Source
maps follow normalized expression order and retain multiple origins for shared
definitions. Canonical-input compilation has an empty source map. Reports are
sidecars and do not change executable bytes. Syntax checking is fail-fast.

`CompilationFailure` never contains a partial executable or verified interface.

## Shared semantics and resource limits

`htlk-executable` supplies canonical records, normalization, JSON/JCS, CBOR,
digests and envelopes. `htlk-analyzer` supplies expression/schema/graph checking.
Both remain available as the `executable` and `analyzer` re-exports. The compiler
matches fixed implementation/policy/catalog identities and runs whole-document
analysis before emitting an executable. Runtime admission independently rechecks
authoritative bytes against the actual native registry.

`FrontendLimits` bounds source bytes, package/module counts, depth, tokens,
traversal work, cumulative constructed intermediate bytes, and report storage.
The configured executable `Limits` additionally bound records and analysis.
Hard token/cost budgets require explicitly declared trusted host capabilities.
Schema backend options retain the analyzer's documented native capability contract.

Source collection, filesystem/symlink handling, dependency retrieval and MCP
discovery belong to the host. Compilation performs no filesystem/network I/O or
external work. Source caches and interface-driven authoring tools can be built on
these APIs; inspection reports do not replace actual implementation checking.

Licensed under the Apache License, Version 2.0.
