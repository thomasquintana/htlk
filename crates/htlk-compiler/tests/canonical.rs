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

#[test]
fn supplied_modular_program_has_identical_executable_bytes() {
    let (compiler, _) = compiler();
    let l = Limits::default();
    let catalogs = MCPCatalogInputs::from_json(
        include_bytes!("../../../specs/examples/decision_brief.catalogs.json"),
        &l,
    )
    .unwrap();
    let bundle = SourceBundle::from_json(
        include_bytes!("../../../specs/examples/modules/source_bundle.json"),
        &l,
    )
    .unwrap();
    for (pin, p) in &bundle.packages {
        assert_eq!(*pin, p.digest(&l).unwrap());
    }
    let standalone = compiler
        .compile_ir(
            include_str!("../../../specs/examples/decision_brief.htlk"),
            &catalogs,
        )
        .unwrap();
    let modular = compiler.compile_ir(&bundle, &catalogs).unwrap();
    assert_eq!(standalone.canonical_document, modular.canonical_document);
    assert_eq!(standalone.executable_bytes, modular.executable_bytes);
    assert!(
        modular
            .source_map
            .iter()
            .all(|e| matches!(e.span.source, SourceLocation::Module { .. }))
    );
    let module = ModuleRef {
        package_digest: bundle.entry.package_digest,
        module_path: "types".into(),
    };
    let report = compiler
        .inspect_module(&bundle, &module, &["BriefOutcome".into()], &catalogs)
        .unwrap();
    assert_eq!(report.available_exports.len(), 1);
    assert!(matches!(&report.exports[0],ExportSummary::Type{name,..} if name=="BriefOutcome"));
}

fn bundle_with_sources(sources: &[(&str, &str)]) -> SourceBundle {
    let package = SourcePackage {
        manifest: SourcePackageManifest {
            package_name: "test.package".into(),
            package_version: "0.1".into(),
            modules: sources
                .iter()
                .map(|(m, _)| (m.to_string(), format!("src/{m}.htlk")))
                .collect(),
            dependencies: BTreeMap::new(),
        },
        sources: sources
            .iter()
            .map(|(m, s)| (format!("src/{m}.htlk"), s.to_string()))
            .collect(),
    };
    let id = package.digest(&Limits::default()).unwrap();
    SourceBundle {
        entry: ModuleRef {
            package_digest: id,
            module_path: "main".into(),
        },
        packages: BTreeMap::from([(id, package)]),
    }
}

#[test]
fn module_validation_distinguishes_unvisited_bodies_from_snapshot_headers() {
    let (compiler, _) = compiler();
    let main = r#"ir_version="0.1" graph test.empty{inputs={} outputs={} nodes{} edges{}}"#;
    let valid = bundle_with_sources(&[
        ("main", main),
        ("unused", "ir_version=\"0.1\" THIS IS NOT VALID SOURCE"),
    ]);
    compiler.compile_bundle(&valid, &catalogs()).unwrap();
    let invalid = bundle_with_sources(&[
        ("main", main),
        ("unused", "ir_version=\"0.0\" THIS IS NOT VALID SOURCE"),
    ]);
    assert_eq!(
        compiler
            .compile_bundle(&invalid, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_VERSION"
    );
    let mut modified = valid;
    modified
        .packages
        .values_mut()
        .next()
        .unwrap()
        .sources
        .get_mut("src/main.htlk")
        .unwrap()
        .push_str("-- changed pin");
    assert_eq!(
        compiler
            .compile_bundle(&modified, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_PACKAGE_PIN"
    );
}

#[test]
fn imports_enforce_visibility_and_defining_module_resolution() {
    let (compiler, _) = compiler();
    let main = r#"ir_version="0.1" import lib from "self/library"
      graph test.answer { inputs={} outputs={answer=string} nodes{ work=use(lib.public_task) } edges{edge answer{from=work.outputs.answer to=outputs.answer}} }"#;
    let library = r#"ir_version="0.1" prompt hidden="hello"
      export task public_task { inputs={} outputs={answer=string} nodes{ value=eval(string,render(&hidden,{})) } edges{edge answer{from=value.outputs.value to=outputs.answer}} }"#;
    let bundle = bundle_with_sources(&[("main", main), ("library", library)]);
    compiler.compile_bundle(&bundle, &catalogs()).unwrap();
    let r = ModuleRef {
        package_digest: bundle.entry.package_digest,
        module_path: "library".into(),
    };
    assert!(
        compiler
            .inspect_module(&bundle, &r, &["hidden".into()], &catalogs())
            .is_err()
    );
    let report = compiler
        .inspect_module(&bundle, &r, &[], &catalogs())
        .unwrap();
    assert_eq!(report.available_exports.len(), 1);
    assert!(report.exports.is_empty());
    let cycle = bundle_with_sources(&[
        ("main", main),
        (
            "library",
            "ir_version=\"0.1\" import app from \"self/main\"",
        ),
    ]);
    assert_eq!(
        compiler
            .compile_bundle(&cycle, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_IMPORT_CYCLE"
    );
}

fn chain_join() -> JoinSpec {
    JoinSpec {
        id: "test.joined".into(),
        left_alias: "left".into(),
        right_alias: "right".into(),
        edges: vec![JoinEdge {
            id: "pipe".into(),
            from: JoinEndpoint {
                graph: JoinOperand::Left,
                port: "value".into(),
            },
            to: JoinEndpoint {
                graph: JoinOperand::Right,
                port: "value".into(),
            },
        }],
        inputs: vec![JoinInput {
            name: "value".into(),
            to: vec![JoinEndpoint {
                graph: JoinOperand::Left,
                port: "value".into(),
            }],
        }],
        outputs: vec![JoinOutput {
            name: "value".into(),
            from: JoinEndpoint {
                graph: JoinOperand::Right,
                port: "value".into(),
            },
        }],
    }
}
fn pass(compiler: &Compiler, optional: bool) -> CanonicalDocument {
    let ty = if optional {
        "optional(string)"
    } else {
        "string"
    };
    let source = format!(
        "ir_version=\"0.1\" graph test.pass {{ inputs={{value={ty}}} outputs={{value={ty}}} nodes{{}} edges{{edge pass{{from=inputs.value to=outputs.value}}}} }}"
    );
    compiler
        .compile_source(&source, &catalogs())
        .unwrap()
        .canonical_document
}
#[test]
fn repeated_joins_preserve_operand_definitions_and_are_self_contained() {
    let (compiler, _) = compiler();
    let operand = pass(&compiler, false);
    let root = operand.fields().root_scope;
    let first = compiler
        .join_graphs(&operand, &operand, &chain_join(), &catalogs())
        .unwrap();
    assert!(first.canonical_document.fields().scopes.contains_key(&root));
    let second = compiler
        .join_graphs(
            &first.canonical_document,
            &operand,
            &chain_join(),
            &catalogs(),
        )
        .unwrap();
    assert!(
        second
            .canonical_document
            .fields()
            .scopes
            .contains_key(&first.canonical_document.fields().root_scope)
    );
    let analysis = analyzer::analyze_document(
        second.canonical_document,
        NativeSchemaOptions::default(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(analysis.structural_summary().scope_depth(), 3);
    assert_eq!(analysis.structural_summary().expanded_nodes(), 4);
}
#[test]
fn joins_reject_generated_collisions_double_bindings_and_unobservable_operands() {
    let (compiler, _) = compiler();
    let operand = pass(&compiler, false);
    let mut join = chain_join();
    join.edges[0].id = "join_in_0".into();
    assert_eq!(
        compiler
            .join_graphs(&operand, &operand, &join, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_JOIN"
    );
    let mut join = chain_join();
    join.inputs[0].to.push(JoinEndpoint {
        graph: JoinOperand::Right,
        port: "value".into(),
    });
    assert!(
        compiler
            .join_graphs(&operand, &operand, &join, &catalogs())
            .is_err()
    );
    let mut join = chain_join();
    join.edges.clear();
    join.outputs[0].from.graph = JoinOperand::Left;
    let optional = pass(&compiler, true);
    assert_eq!(
        compiler
            .join_graphs(&operand, &optional, &join, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_UNUSED_NODE"
    );
}
#[test]
fn join_fanout_requiredness_is_combined_and_request_json_is_closed() {
    let (compiler, _) = compiler();
    let left = pass(&compiler, true);
    let right = pass(&compiler, false);
    let mut join = chain_join();
    join.edges.clear();
    join.inputs[0].to.push(JoinEndpoint {
        graph: JoinOperand::Right,
        port: "value".into(),
    });
    join.outputs.push(JoinOutput {
        name: "left_value".into(),
        from: JoinEndpoint {
            graph: JoinOperand::Left,
            port: "value".into(),
        },
    });
    let output = compiler
        .join_graphs(&left, &right, &join, &catalogs())
        .unwrap();
    let root =
        &output.canonical_document.fields().scopes[&output.canonical_document.fields().root_scope];
    assert!(root.fields().inputs.get("value").unwrap().required());
    assert!(JoinSpec::from_json(br#"{"id":"test.join","left_alias":"left","right_alias":"right","edges":[],"inputs":[],"outputs":[],"guard":true}"#,&Limits::default()).is_err());
}
