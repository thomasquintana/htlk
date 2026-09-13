# htlk-compiler

Natural-language compiler for the [Harness Toolkit](https://crates.io/crates/htlk).

This crate compiles natural-language harness definitions into Harness Toolkit
IR. Its domain API will be introduced as the compiler requirements are defined.

The shared `htlk-cbor` dependency supplies deterministic encoding for future
executable production. Executable schemas and compiler call sites are pending.

Licensed under the Apache License, Version 2.0.
