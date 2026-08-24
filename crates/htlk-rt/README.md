# htlk-rt

Runtime components for the [Harness Toolkit](https://crates.io/crates/htlk).

This crate provides the actor-based execution environment for Harness Toolkit
bytecode. Runtime capabilities include:

- OpenAI-compatible chat, streaming, structured output, and embeddings.
- Bundled SQLite storage for application data, caches, and metadata.
- Embedded vector search through `sqlite-vec`.
- Embedded Lua 5.4 execution through `mlua`.
- Local and remote MCP servers over child-process stdio and Streamable HTTP.
- OAuth and JWT client credentials for MCP connections.

Its domain API will be introduced as the runtime requirements are defined.

Licensed under the Apache License, Version 2.0.
