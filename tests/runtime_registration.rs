//! Real compiler output, authenticated registration and independent native reload.
mod support;
use htlk::runtime::{
    Attestation, Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    failure::{FailureCode, RuntimeFailure},
    records::SecurityLabels,
    store::{SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use support::{catalogs, native_compiler};

struct Policy {
    allow: AtomicBool,
    require_evidence: bool,
    registrations: AtomicUsize,
}
impl Policy {
    fn new(require_evidence: bool) -> Self {
        Self {
            allow: AtomicBool::new(true),
            require_evidence,
            registrations: AtomicUsize::new(0),
        }
    }
}
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if context.principal() != "alice" || !self.allow.load(Ordering::SeqCst) {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        if let Authorization::RegisterGraph { attestations, .. } = action {
            self.registrations.fetch_add(1, Ordering::SeqCst);
            // Scripted test evidence; real deployments provide actual verification.
            if self.require_evidence
                && !attestations
                    .iter()
                    .any(|a| a.format == "fixture.signature" && a.evidence == b"trusted")
            {
                return Err(RuntimeFailure::new(FailureCode::Policy));
            }
        }
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::default())
    }
}
fn context(principal: &str) -> HostContext {
    HostContext::new(principal.into(), &Limits::default()).unwrap()
}

#[test]
fn compiled_registration_is_idempotent_with_detached_provenance_and_current_authorization() {
    actix::System::new().block_on(async {
        let (compiler, registry) = native_compiler();
        let output = compiler
            .compile_source(
                r#"ir_version="0.1" graph test.registered{inputs={} outputs={} nodes{} edges{}}"#,
                &catalogs(),
            )
            .unwrap();
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let policy = Arc::new(Policy::new(false));
        let runtime = Runtime::new(
            store,
            Arc::new(registry),
            policy.clone(),
            RuntimeConfig::default(),
        )
        .unwrap();
        let registered = runtime
            .register_graph(&context("alice"), &output.executable_bytes, &[])
            .await
            .unwrap();
        let again = runtime
            .register_graph(
                &context("alice"),
                &output.executable_bytes,
                &[Attestation {
                    format: "observation".into(),
                    evidence: vec![1, 2, 3],
                }],
            )
            .await
            .unwrap();
        assert_eq!(registered, again);
        assert_eq!(registered.fingerprint, output.fingerprint);
        let executable = runtime
            .executable(&context("alice"), registered.fingerprint)
            .await
            .unwrap();
        assert_eq!(executable.document(), &output.canonical_document);
        policy.allow.store(false, Ordering::SeqCst);
        assert_eq!(
            runtime
                .executable(&context("alice"), registered.fingerprint)
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Policy
        );
        assert_eq!(
            runtime
                .register_graph(&context("alice"), &output.executable_bytes, &[])
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Policy
        );
        assert_eq!(
            runtime
                .executable(&context("bob"), registered.fingerprint)
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Policy
        );
    });
}

#[test]
fn trust_policy_is_separate_and_invalid_bytes_never_reach_it() {
    actix::System::new().block_on(async {
        let (compiler, registry) = native_compiler();
        let output = compiler
            .compile_source(
                r#"ir_version="0.1" graph test.signed{inputs={} outputs={} nodes{} edges{}}"#,
                &catalogs(),
            )
            .unwrap();
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let policy = Arc::new(Policy::new(true));
        let runtime = Runtime::new(
            store,
            Arc::new(registry),
            policy.clone(),
            RuntimeConfig::default(),
        )
        .unwrap();
        assert_eq!(
            runtime
                .register_graph(
                    &context("alice"),
                    b"invalid",
                    &[Attestation {
                        format: "fixture.signature".into(),
                        evidence: b"trusted".to_vec()
                    }]
                )
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Executable
        );
        assert_eq!(policy.registrations.load(Ordering::SeqCst), 0);
        assert_eq!(
            runtime
                .register_graph(&context("alice"), &output.executable_bytes, &[])
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Policy
        );
        assert!(
            runtime
                .executable(&context("alice"), output.fingerprint)
                .await
                .is_err()
        );
        runtime
            .register_graph(
                &context("alice"),
                &output.executable_bytes,
                &[Attestation {
                    format: "fixture.signature".into(),
                    evidence: b"trusted".to_vec(),
                }],
            )
            .await
            .unwrap();
        assert!(
            runtime
                .executable(&context("alice"), output.fingerprint)
                .await
                .is_ok()
        );
    });
}

#[test]
fn fresh_service_rechecks_registered_bytes_against_its_actual_registry() {
    actix::System::new().block_on(async {
        let (compiler, registry) = native_compiler();
        let output = compiler
            .compile_ir(
                include_str!("../specs/examples/decision_brief.htlk"),
                &catalogs(),
            )
            .unwrap();
        let store = SqliteStore::in_memory(StoreConfig::default())
            .await
            .unwrap();
        let config = RuntimeConfig {
            max_cached_executables: 0,
            ..RuntimeConfig::default()
        };
        let policy = Arc::new(Policy::new(false));
        let runtime = Runtime::new(
            store.clone(),
            Arc::new(registry),
            policy.clone(),
            config.clone(),
        )
        .unwrap();
        runtime
            .register_graph(&context("alice"), &output.executable_bytes, &[])
            .await
            .unwrap();
        drop(runtime);
        let missing = htlk::runtime::NativeRegistry::new(&Limits::default()).unwrap();
        let fresh = Runtime::new(
            store.clone(),
            Arc::new(missing),
            policy.clone(),
            config.clone(),
        )
        .unwrap();
        assert_eq!(
            fresh
                .executable(&context("alice"), output.fingerprint)
                .await
                .err()
                .unwrap()
                .code(),
            FailureCode::Profile
        );
        let (_, registry) = native_compiler();
        let restored = Runtime::new(store, Arc::new(registry), policy, config).unwrap();
        assert_eq!(
            restored
                .executable(&context("alice"), output.fingerprint)
                .await
                .unwrap()
                .document(),
            &output.canonical_document
        );
    });
}
