//! Canonical compilation must establish semantics and exact compiler selections.
use htlk_compiler::executable::{cbor::Limits, digest::Digest};
use htlk_compiler::{analyzer::NativeSchemaOptions, executable::*, *};
use std::collections::BTreeMap;

fn compiler() -> (Compiler, CanonicalDocument) {
    let l = Limits::default();
    let policy = PolicyDocument::new(
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
                max_value_bytes: 65536,
                max_collection_visits: 10000,
                max_regex_bytes: 4096,
                max_regex_compiled_bytes: 65536,
                max_output_bytes: 65536,
                max_steps: 100000,
            },
            maximum_scope_depth: 32,
            maximum_expanded_nodes: 1000,
        },
        &l,
    )
    .unwrap();
    let id = Digest::from_bytes([1; 32]);
    let e = EngineIdentity::new("fixture".into(), "1".into(), "1".into(), id, &l).unwrap();
    let profile = ExecutionProfile::new(id, e.clone(), e.clone(), e, policy.digest(), &l).unwrap();
    let scope = Scope::new(ScopeFields::default(), &l).unwrap();
    let mut fields = DocumentFields::new(
        "test.empty".into(),
        profile.clone(),
        scope.digest(&l).unwrap(),
    );
    fields.scopes.insert(fields.root_scope, scope);
    fields
        .documents
        .insert(policy.digest(), policy.document().clone());
    let document = CanonicalDocument::new(fields, &l).unwrap();
    (
        Compiler::new(CompilerConfig {
            profile,
            policy,
            libraries: BTreeMap::new(),
            limits: l,
            frontend: FrontendLimits::default(),
            schema_options: NativeSchemaOptions::default(),
        })
        .unwrap(),
        document,
    )
}

fn catalogs() -> MCPCatalogInputs {
    MCPCatalogInputs::from_json(br#"{
      "tools":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"tools":[]},
      "resources":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"resources":[],"templates":[]},
      "prompts":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{},"schema_documents":{},"prompts":[]}
    }"#, &Limits::default()).unwrap()
}

#[test]
fn canonical_success_is_exact_enveloped_and_source_free() {
    let (compiler, document) = compiler();
    let output = compiler
        .compile_document(document.clone(), &catalogs())
        .unwrap();
    assert_eq!(output.canonical_document, document);
    assert!(output.source_map.is_empty());
    let envelope =
        ExecutableEnvelope::decode(&output.executable_bytes, &Limits::default()).unwrap();
    assert_eq!(envelope.fingerprint(), output.fingerprint);
    assert_eq!(
        CanonicalDocument::from_envelope(&envelope, &Limits::default()).unwrap(),
        document
    );
}

#[test]
fn semantic_invalidity_and_unselected_policy_are_rejected() {
    let (compiler, document) = compiler();
    let mut fields = document.fields().clone();
    fields.root_scope = Digest::from_bytes([2; 32]);
    assert!(
        compiler
            .compile_document(
                CanonicalDocument::new(fields, &Limits::default()).unwrap(),
                &catalogs()
            )
            .is_err()
    );
    let mut fields = document.fields().clone();
    fields.documents.clear();
    assert_eq!(
        compiler
            .compile_document(
                CanonicalDocument::new(fields, &Limits::default()).unwrap(),
                &catalogs()
            )
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_POLICY"
    );
}

#[test]
fn raw_catalog_duplicate_keys_are_not_discarded() {
    assert!(
        MCPCatalogInputs::from_json(
            br#"{"tools":{},"tools":{},"resources":{},"prompts":{}}"#,
            &Limits::default()
        )
        .is_err()
    );
}
