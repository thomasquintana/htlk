# htlk-rt

Runtime components for the [Harness Toolkit](https://crates.io/crates/htlk).

This crate is the foundation for the native Rust actor-based execution environment
for Harness Toolkit executable graphs. Its selected dependencies support:

- OpenAI-compatible chat, streaming, structured output, and embeddings.
- Bundled SQLite storage for application data, caches, and metadata.
- Embedded vector search through `sqlite-vec`.
- Local and remote MCP servers over child-process stdio and Streamable HTTP.
- OAuth and JWT client credentials for MCP connections.

Its domain API is still being implemented. Conditions and pure calculations use
HTLK's typed expression records and a native Rust evaluator; prompt rendering uses
the canonical prompt-template model. No scripting runtime is embedded.

The shared `htlk-cbor` dependency supplies strict deterministic-CBOR decoding
for future executable registration and value ingress. Graph payload schemas,
aggregate decoding policy, and runtime call sites are pending.
`htlk-executable::digest` supplies typed SHA-256 digests and hashing for future
fingerprint checks and runtime identities, using consumer-defined preimages.
Its `ExecutableEnvelope` API validates version-0.1 envelope fields, format,
version, and fingerprint before later registration checks interpret the payload.

Licensed under the Apache License, Version 2.0.
