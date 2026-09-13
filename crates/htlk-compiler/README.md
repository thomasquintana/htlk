# htlk-compiler

Natural-language compiler for the [Harness Toolkit](https://crates.io/crates/htlk).

This crate compiles natural-language harness definitions into Harness Toolkit
IR. Its domain API will be introduced as the compiler requirements are defined.

The shared `htlk-cbor` dependency supplies deterministic encoding for future
executable production. Graph payload schemas and compiler call sites are pending.
`htlk-executable::digest` supplies typed SHA-256 digests and canonical-CBOR hashing;
the compiler will construct its specified graph identity preimages. The
`ExecutableEnvelope` API packages payload bytes under the shared version-0.1
envelope contract and computes the envelope fingerprint.

Licensed under the Apache License, Version 2.0.
