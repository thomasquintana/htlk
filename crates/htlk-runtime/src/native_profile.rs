//! Identities for the shipped native core and engines.
use crate::{
    EngineIdentity, ExecutionProfile, NativeRegistryError, NativeSchemas, PolicyDocument,
    digest::Digest,
};
use htlk_cbor::Limits;
use htlk_executable::cbor as htlk_cbor;
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
        htlk_executable::implementation_digest().as_bytes(),
        htlk_analyzer::implementation_digest().as_bytes(),
        include_bytes!("lib.rs"),
        include_bytes!("native_profile.rs"),
        include_bytes!("evaluate.rs"),
        include_bytes!("checked_evaluate.rs"),
        include_bytes!("runtime_type.rs"),
        include_bytes!("schema_projection.rs"),
        include_bytes!("native_registry.rs"),
        include_bytes!("verified.rs"),
        include_bytes!("identity.rs"),
        include_bytes!("failure.rs"),
        include_bytes!("records.rs"),
        include_bytes!("store.rs"),
        include_bytes!("store_transitions.rs"),
        include_bytes!("store_replay.rs"),
        include_bytes!("store.sql"),
        include_bytes!("runtime.rs"),
        include_bytes!("store_registration.rs"),
        include_bytes!("store_registration.sql"),
        include_bytes!("artifact.rs"),
        include_bytes!("store_artifacts.rs"),
        include_bytes!("store_artifacts.sql"),
        include_bytes!("clock.rs"),
        include_bytes!("run.rs"),
        include_bytes!("store_runs.rs"),
        include_bytes!("store_runs.sql"),
        include_bytes!("binding.rs"),
        include_bytes!("influence.rs"),
        include_bytes!("store_execution.rs"),
        include_bytes!("execution/mod.rs"),
        include_bytes!("execution/model.rs"),
        include_bytes!("execution/frame.rs"),
        include_bytes!("execution/step.rs"),
        include_bytes!("execution/composite.rs"),
        include_bytes!("execution/control.rs"),
        include_bytes!("execution/waits.rs"),
        include_bytes!("execution/host.rs"),
        include_bytes!("execution/inspection.rs"),
        include_bytes!("mcp.rs"),
        include_bytes!("execution/mcp.rs"),
        include_bytes!("execution/approval.rs"),
        include_bytes!("store_late.sql"),
        include_bytes!("mcp_protocol.rs"),
        include_bytes!("uri_template.rs"),
    ]);
    let regex = EngineIdentity::new(
        "htlk.native-regex".into(),
        "1.13.1/automata-0.4.18/syntax-0.8.11/adapter-0.1".into(),
        "Unicode-16.0.0".into(),
        identity(&[
            include_bytes!("native_profile.rs"),
            include_bytes!("evaluate.rs"),
            htlk_analyzer::implementation_digest().as_bytes(),
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
            htlk_executable::implementation_digest().as_bytes(),
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
