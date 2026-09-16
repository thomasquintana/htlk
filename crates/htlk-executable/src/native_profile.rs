//! Identities for the shipped native core and engines.
use crate::{
    EngineIdentity, ExecutionProfile, NativeRegistryError, NativeSchemas, PolicyDocument,
    digest::Digest,
};
use htlk_cbor::Limits;
use sha2::{Digest as _, Sha256};

/// Constructs the shipped native profile for an exact bounded policy document.
/// Engine implementation identities include adapter source; regex implementation
/// and Unicode-data dependencies are pinned in the published package manifest.
/// Libraries are separately admitted against a host-owned NativeRegistry.
///
/// # Errors
/// Returns policy/metadata/codec bounds or invalid policy configuration.
pub fn native_profile(
    policy: &PolicyDocument,
    limits: &Limits,
) -> Result<ExecutionProfile, NativeRegistryError> {
    PolicyDocument::decode(policy.document().as_bytes(), limits)?;
    let core = identity(&[
        include_bytes!("native_profile.rs"),
        include_bytes!("evaluate.rs"),
        include_bytes!("checked_evaluate.rs"),
        include_bytes!("runtime_type.rs"),
        include_bytes!("schema_projection.rs"),
        include_bytes!("schema_hints.rs"),
        include_bytes!("type_check.rs"),
        include_bytes!("native_registry.rs"),
        include_bytes!("graph_verify.rs"),
        include_bytes!("verified.rs"),
    ]);
    let regex = EngineIdentity::new(
        "htlk.native-regex".into(),
        "1.13.1/automata-0.4.18/syntax-0.8.11/adapter-0.1".into(),
        "Unicode-16.0.0".into(),
        identity(&[
            include_bytes!("native_profile.rs"),
            include_bytes!("evaluate.rs"),
        ]),
        limits,
    )?;
    let uri = EngineIdentity::new(
        "htlk.native-uri-template".into(),
        "0.7.14/adapter-0.1".into(),
        "RFC6570-scalar".into(),
        identity(&[
            include_bytes!("native_profile.rs"),
            include_bytes!("uri_template.rs"),
        ]),
        limits,
    )?;
    Ok(ExecutionProfile::new(
        core,
        regex,
        NativeSchemas::identity(limits)?,
        uri,
        policy.digest(),
        limits,
    )?)
}
fn identity(sources: &[&[u8]]) -> Digest {
    let mut hash = Sha256::new();
    hash.update(b"htlk.native-implementation/0.1\n");
    for source in sources {
        hash.update((source.len() as u64).to_be_bytes());
        hash.update(source);
    }
    Digest::from_bytes(hash.finalize().into())
}
