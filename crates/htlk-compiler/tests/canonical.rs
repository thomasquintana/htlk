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

#[test]
fn standalone_preserves_native_numbers_and_reports_sorted_record_sources() {
    let (compiler, _) = compiler();
    let source = r#"ir_version = "0.1"
      type Number = union(integer, float)
      graph test.numbers {
        inputs = {} outputs = { result = list(Number) }
        nodes { values = eval(list(Number), [1, 1.0, -0, -0.0]) }
        edges { edge result { from = values.outputs.value to = outputs.result } }
      }"#;
    let output = compiler.compile_source(source, &catalogs()).unwrap();
    assert!(!output.source_map.is_empty());
    let scope =
        &output.canonical_document.fields().scopes[&output.canonical_document.fields().root_scope];
    let Operation::Eval(e) = &scope.fields().nodes[0].fields().operation else {
        panic!()
    };
    let ExpressionKind::List(values) = e.kind() else {
        panic!()
    };
    assert!(matches!(
        values[0].kind(),
        ExpressionKind::Literal(ScalarLiteral::Integer(1))
    ));
    assert!(matches!(
        values[1].kind(),
        ExpressionKind::Literal(ScalarLiteral::Float(_))
    ));
}

#[test]
fn comments_declaration_order_and_unused_valid_definitions_preserve_bytes() {
    let (compiler, _) = compiler();
    let a = r#"ir_version = "0.1" graph test.empty {inputs={} outputs={} nodes{} edges{}}"#;
    let b = r#"ir_version = "0.1" --[[ block comment ]]
      type Unused = string
      prompt unused = "hello {name}"
      task unused_task { inputs={} outputs={} nodes{} edges{} }
      graph test.empty { edges{} nodes{} outputs={} inputs={} description="documentation" }
    "#;
    let a = compiler.compile_source(a, &catalogs()).unwrap();
    let b = compiler.compile_source(b, &catalogs()).unwrap();
    assert_eq!(a.executable_bytes, b.executable_bytes);
    assert_eq!(b.documentation.len(), 1);
}

#[test]
fn invalid_unused_declarations_and_lazy_unknown_names_are_checked() {
    let (compiler, _) = compiler();
    for extra in [
        "type Invalid = union(string,string)",
        "type Recursive = list(Recursive)",
        "task invalid { inputs={} outputs={x=string} nodes{} edges{} }",
        "prompt invalid(x=string) = \"{y}\"",
    ] {
        let source = format!(
            "ir_version = \"0.1\" {extra} graph test.empty {{inputs={{}} outputs={{}} nodes{{}} edges{{}}}}"
        );
        assert!(
            compiler.compile_source(&source, &catalogs()).is_err(),
            "{extra}"
        );
    }
    let source = r#"ir_version="0.1" graph test.bad { inputs={} outputs={} nodes{} edges{} postconditions=false and status(@missing)=="failed" }"#;
    assert!(compiler.compile_source(source, &catalogs()).is_err());
}

#[test]
fn supplied_worked_program_compiles_with_exact_offline_catalogs() {
    let (compiler, _) = compiler();
    let catalogs = MCPCatalogInputs::from_json(
        include_bytes!("../../../specs/examples/decision_brief.catalogs.json"),
        &Limits::default(),
    )
    .unwrap();
    let output = compiler
        .compile_source(
            include_str!("../../../specs/examples/decision_brief.htlk"),
            &catalogs,
        )
        .unwrap();
    let analyzed = analyzer::analyze_document(
        output.canonical_document,
        NativeSchemaOptions::default(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(analyzed.structural_summary().scope_depth(), 3);
    assert_eq!(analyzed.structural_summary().expanded_nodes(), 34);
}

#[test]
fn bounded_frontend_rejects_invalid_lexical_and_block_forms() {
    let (compiler, _) = compiler();
    for source in [
        "\u{feff}ir_version=\"0.1\"",
        "ir_version=\"0.1\" --[[ unterminated",
        "ir_version=\"0.1\" prompt invalid=\"\\uD800\"",
        "ir_version=\"0.1\" prompt invalid=\"unterminated",
        "ir_version=\"0.1\" type Invalid=optional(string)",
        "ir_version=\"0.1\" graph test.bad {inputs={} outputs={} nodes{} edges{} inputs={}}",
    ] {
        assert!(
            compiler.compile_source(source, &catalogs()).is_err(),
            "{source}"
        );
    }
}
