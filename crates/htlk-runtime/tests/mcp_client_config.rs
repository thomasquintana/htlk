//! Public client configuration never dispatches work or conflates owner identities.
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Value},
};
use htlk_runtime::{HostContext, identity::RunId, mcp::client::*};
use std::{collections::BTreeMap, sync::Arc};

struct NeverCalled;
impl McpClientHandler for NeverCalled {
    fn handle<'a>(
        &'a self,
        _: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        panic!("configuration must not dispatch host work")
    }
}
fn config(
    services: &[McpClientService],
) -> Result<McpClientConfig, htlk_runtime::failure::RuntimeFailure> {
    McpClientConfig::new(
        McpClientLimits::default(),
        services
            .iter()
            .map(|s| (*s, Arc::new(NeverCalled) as Arc<dyn McpClientHandler>))
            .collect(),
        None,
    )
}

#[test]
fn capabilities_describe_only_installed_services_without_dispatching() {
    use McpClientService::*;
    for (services, expected) in [
        (vec![], "{}"),
        (vec![Sampling], r#"{"sampling":{}}"#),
        (
            vec![Sampling, SamplingTools],
            r#"{"sampling":{"tools":{}}}"#,
        ),
        (
            vec![Sampling, SamplingContext],
            r#"{"sampling":{"context":{}}}"#,
        ),
        (vec![ElicitationUrl], r#"{"elicitation":{"url":{}}}"#),
        (vec![ElicitationForm], r#"{"elicitation":{"form":{}}}"#),
        (
            vec![ElicitationForm, ElicitationUrl],
            r#"{"elicitation":{"form":{},"url":{}}}"#,
        ),
        (vec![Roots], r#"{"roots":{"listChanged":false}}"#),
        (
            vec![Roots, RootsChanged],
            r#"{"roots":{"listChanged":true}}"#,
        ),
    ] {
        let built = config(&services).unwrap();
        assert_eq!(
            built.capabilities().value(),
            JsonDocument::new(expected.as_bytes(), &Limits::default())
                .unwrap()
                .value()
        );
        assert_eq!(
            built.handler(Sampling).is_some(),
            services.contains(&Sampling)
        );
        assert!(built.event_handler().is_none());
    }
    for service in [SamplingTools, SamplingContext, RootsChanged] {
        assert!(config(&[service]).is_err());
    }
}

#[test]
fn impossible_channel_and_time_reservations_fail_before_startup() {
    for change in 0..8 {
        let mut limits = McpClientLimits::default();
        match change {
            0 => limits.callbacks = 0,
            1 => limits.events = usize::MAX,
            2 => limits.pending_requests = limits.requests_per_session + 1,
            3 => limits.retained_bytes = limits.frame_bytes - 1,
            4 => limits.timeout_ms = 0,
            5 => limits.cancellation_grace_ms = limits.timeout_ms + 1,
            6 => limits.timeout_ms = u64::MAX,
            _ => limits.catalog_pages = 0,
        }
        assert!(
            McpClientConfig::new(limits, BTreeMap::new(), None).is_err(),
            "{change}"
        );
    }
}

#[test]
fn owner_identity_is_principal_and_namespace_scoped_but_generation_stable() {
    let limits = Limits::default();
    let owner = |principal: &str, scope, generation| {
        McpOwner::new(
            HostContext::new(principal.into(), &limits).unwrap(),
            scope,
            generation,
            &limits,
        )
    };
    let first = owner("alice", McpOwnerScope::Host("workspace".into()), 1).unwrap();
    let takeover = owner("alice", McpOwnerScope::Host("workspace".into()), 2).unwrap();
    let other = owner("bob", McpOwnerScope::Host("workspace".into()), 1).unwrap();
    let run = owner(
        "alice",
        McpOwnerScope::Run(RunId::new("workspace".into(), &limits).unwrap()),
        1,
    )
    .unwrap();
    assert_eq!(first.key(), takeover.key());
    assert_ne!(first.generation(), takeover.generation());
    assert_ne!(first.key(), other.key());
    assert_ne!(first.key(), run.key());
    assert_eq!(first.host().principal(), "alice");
    assert!(matches!(first.scope(), McpOwnerScope::Host(_)));
    assert!(owner("alice", McpOwnerScope::Host("workspace".into()), 0).is_err());
    assert!(owner("alice", McpOwnerScope::Host(String::new()), 1).is_err());
    assert!(!format!("{first:?}").contains("alice"));
}

#[test]
fn protocol_ids_never_coerce_and_url_required_errors_preserve_protected_data() {
    assert_ne!(
        McpRequestId::from_value(&Value::Integer(1)).unwrap(),
        McpRequestId::from_value(&Value::Text("1".into())).unwrap()
    );
    for value in [Value::Null, Value::Bool(true)] {
        assert!(McpRequestId::from_value(&value).is_err());
    }
    assert_eq!(
        McpRequestId::String(String::new()).to_value(),
        Value::Text(String::new())
    );
    let raw = br#"{"code":-32042,"message":"private-message","data":{"elicitations":[{"mode":"url","elicitationId":"private-id","url":"https://example.test/authorize","message":"Continue"}]}}"#;
    let document = JsonDocument::new(raw, &Limits::default()).unwrap();
    let error = McpRpcError::new(document.clone()).unwrap();
    assert_eq!(error.code(), -32042);
    assert_eq!(error.document().digest(), document.digest());
    assert!(!format!("{error:?}").contains("private"));
    assert!(!format!("{error:?}").contains("example.test"));
    for raw in [
        r#"{"code":null,"message":"bad"}"#,
        r#"{"code":1,"message":false}"#,
    ] {
        assert!(
            McpRpcError::new(JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap())
                .is_err()
        );
    }
}
