# htlk-compiler

HTLK IR compiler foundations for the [Harness Toolkit](https://crates.io/crates/htlk).

The planned domain API compiles `.htlk` source and source bundles into canonical
executable graphs. Source collection and natural-language planning are host work.
Source parsing, imports, inspection, and graph composition remain to be implemented.

The shared `htlk-cbor` dependency supplies deterministic encoding for future
executable production. `htlk-executable` supplies graph schemas and the composed
`verify_executable` API; compiler production call sites remain pending.
`htlk-executable::digest` supplies typed SHA-256 digests and canonical-CBOR hashing;
the compiler will construct its specified graph identity preimages. The
`ExecutableEnvelope` API packages payload bytes under the shared version-0.1
envelope contract and computes the envelope fingerprint.

Licensed under the Apache License, Version 2.0.
