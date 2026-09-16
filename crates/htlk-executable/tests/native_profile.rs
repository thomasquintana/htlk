//! Shipped native identities and exact policy/profile admission.
use htlk_cbor::Limits;
use htlk_executable::{
    EngineIdentity, EvaluatorLimits, ExecutionLimits, ExecutionProfile, NativeRegistry,
    NativeRegistryError as Error, PolicyDocument, PolicyFields, digest::Digest, native_profile,
};

fn policy() -> PolicyDocument {
    PolicyDocument::new(
        PolicyFields {
            cost_unit: "credits".into(),
            defaults: ExecutionLimits::new()
                .with_timeout_ms(1000)
                .unwrap()
                .with_attempt_timeout_ms(100)
                .unwrap()
                .with_max_concurrency(1)
                .unwrap(),
            evaluator_limits: EvaluatorLimits {
                max_expression_depth: 64,
                max_value_bytes: 1024,
                max_collection_visits: 1000,
                max_regex_bytes: 1024,
                max_regex_compiled_bytes: 4096,
                max_output_bytes: 1024,
                max_steps: 10000,
            },
            maximum_scope_depth: 32,
            maximum_expanded_nodes: 1000,
        },
        &Limits::default(),
    )
    .unwrap()
}
#[test]
fn native_profile_is_deterministic_and_matches_every_exact_component() {
    let limits = Limits::default();
    let policy = policy();
    let native = native_profile(&policy, &limits).unwrap();
    let mut registry = NativeRegistry::new(&limits).unwrap();
    registry.link_policy(&policy).unwrap();
    assert_eq!(native_profile(&policy, &limits).unwrap(), native);
    assert_eq!(native.regex_engine().data_version(), "Unicode-16.0.0");
    registry.verify_profile(&native, &policy).unwrap();
    let wrong = EngineIdentity::new(
        "other".into(),
        "1".into(),
        "".into(),
        Digest::from_bytes([0; 32]),
        &limits,
    )
    .unwrap();
    for field in ["core", "regex", "schema", "uri_template", "policy"] {
        let profile = ExecutionProfile::new(
            if field == "core" {
                Digest::from_bytes([0; 32])
            } else {
                native.core_digest()
            },
            if field == "regex" {
                wrong.clone()
            } else {
                native.regex_engine().clone()
            },
            if field == "schema" {
                wrong.clone()
            } else {
                native.schema_validator().clone()
            },
            if field == "uri_template" {
                wrong.clone()
            } else {
                native.uri_template_engine().clone()
            },
            if field == "policy" {
                Digest::from_bytes([0; 32])
            } else {
                native.policy_document()
            },
            &limits,
        )
        .unwrap();
        assert_eq!(
            registry.verify_profile(&profile, &policy),
            Err(Error::ProfileMismatch(field))
        );
    }
}
#[test]
fn profile_policy_identity_and_limits_are_rechecked() {
    let policy = policy();
    let limits = Limits::default();
    let profile = native_profile(&policy, &limits).unwrap();
    let mut changed = policy.fields().clone();
    changed.evaluator_limits.max_steps += 1;
    let changed = PolicyDocument::new(changed, &limits).unwrap();
    assert_eq!(
        NativeRegistry::new(&limits)
            .unwrap()
            .verify_profile(&profile, &changed),
        Err(Error::ProfileMismatch("policy"))
    );
    let tiny = Limits {
        max_document_bytes: 1,
        ..limits
    };
    assert!(native_profile(&policy, &tiny).is_err());
}
