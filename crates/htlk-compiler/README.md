# htlk-compiler

HTLK IR compiler foundations for the [Harness Toolkit](https://crates.io/crates/htlk).

The planned domain API compiles `.htlk` source and source bundles into canonical
executable graphs. Source collection and natural-language planning are host work.
Source parsing, imports, inspection, and graph composition remain to be implemented.

`htlk-executable` supplies canonical graph records, bounded construction, Serde
wire mappings and the public `cbor` codec. `htlk-analyzer` supplies focused
expression/scope analysis, whole-document verification, inferred information and
structured diagnostics for compiler and agent-facing feedback. Both are available
as this crate's `executable` and `analyzer` modules. Compiler production and agent
repair-loop implementation remain outside this extraction.
`htlk-executable::digest` supplies typed SHA-256 digests and canonical-CBOR hashing;
the compiler will construct its specified graph identity preimages. The
`ExecutableEnvelope` API packages payload bytes under the shared version-0.1
envelope contract and computes the envelope fingerprint.

Licensed under the Apache License, Version 2.0.
