//! Packaged source contribution for shared semantic and schema implementations.
use htlk_executable::digest::Digest;
use sha2::{Digest as _, Sha256};

/// Hashes this package's semantic/backend sources and pinned protocol data.
/// Callers compose this contribution into host-linked native identities.
pub fn implementation_digest() -> Digest {
    let model = htlk_executable::implementation_digest();
    let sources:&[&[u8]]=&[
        model.as_bytes(),
        include_bytes!("implementation.rs"),include_bytes!("lib.rs"),include_bytes!("analyzed.rs"),
        include_bytes!("error.rs"),include_bytes!("context.rs"),include_bytes!("linkage.rs"),include_bytes!("linked.rs"),
        include_bytes!("binding_validation.rs"),include_bytes!("graph_verify.rs"),
        include_bytes!("structure.rs"),include_bytes!("type_check.rs"),
        include_bytes!("native_schema.rs"),include_bytes!("schema_catalog.rs"),include_bytes!("schema_hints.rs"),
        include_bytes!("schema_locations.rs"),include_bytes!("schema_resources.rs"),include_bytes!("schema_projection.rs"),
        include_bytes!("mcp_protocol.rs"),include_bytes!("../assets/mcp-2025-11-25.schema.json"),
        b"jsonschema=0.56.0;regex=1.13.1;regex-automata=0.4.18;regex-syntax=0.8.11;iri-string=0.7.14",
    ];
    let mut hash = Sha256::new();
    hash.update(b"htlk.analyzer-implementation/0.1\n");
    for source in sources {
        hash.update((source.len() as u64).to_be_bytes());
        hash.update(source);
    }
    Digest::from_bytes(hash.finalize().into())
}
