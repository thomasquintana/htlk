//! Packaged source contribution for linked implementation identities.
use crate::digest::Digest;
use sha2::{Digest as _, Sha256};

/// Hashes this package's model/codec implementation sources and pinned backend.
/// This is an implementation contribution, not a canonical document identity.
pub fn implementation_digest() -> Digest {
    let sources: &[&[u8]] = &[
        include_bytes!("implementation.rs"),
        include_bytes!("lib.rs"),
        include_bytes!("document.rs"),
        include_bytes!("envelope.rs"),
        include_bytes!("digest.rs"),
        include_bytes!("identifier.rs"),
        include_bytes!("json.rs"),
        include_bytes!("json_pointer.rs"),
        include_bytes!("metadata.rs"),
        include_bytes!("options.rs"),
        include_bytes!("policy.rs"),
        include_bytes!("record_accounting.rs"),
        include_bytes!("uri_template.rs"),
        include_bytes!("serialize.rs"),
        include_bytes!("expression/mod.rs"),
        include_bytes!("expression/wire.rs"),
        include_bytes!("expression/template.rs"),
        include_bytes!("graph/mod.rs"),
        include_bytes!("graph/wire.rs"),
        include_bytes!("types/mod.rs"),
        include_bytes!("types/wire.rs"),
        include_bytes!("cbor/mod.rs"),
        include_bytes!("cbor/accounting.rs"),
        include_bytes!("cbor/decode.rs"),
        include_bytes!("cbor/encode.rs"),
        include_bytes!("cbor/error.rs"),
        include_bytes!("cbor/limits.rs"),
        include_bytes!("cbor/value.rs"),
        include_bytes!("cbor/serde.rs"),
        b"cbor2=1.1.5;half=2.7.1",
    ];
    let mut hash = Sha256::new();
    hash.update(b"htlk.model-implementation/0.1\n");
    for source in sources {
        hash.update((source.len() as u64).to_be_bytes());
        hash.update(source);
    }
    Digest::from_bytes(hash.finalize().into())
}
