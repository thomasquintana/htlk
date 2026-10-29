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
            mcp_result_policies: BTreeMap::new(),
            mcp_result_documents: BTreeMap::new(),
            limits: l,
            frontend: FrontendLimits::default(),
            capabilities: CompilerCapabilities::default(),
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

#[test]
fn module_negative_cases_and_fresh_interfaces_preserve_verification() {
    let (compiler, _) = compiler();
    let main = r#"ir_version="0.1" import lib from "self/library" graph test.module{inputs={}outputs={value=integer}nodes{work=use(lib.public_task)}edges{edge value{from=work.outputs.value to=outputs.value}}}"#;
    let library = r#"ir_version="0.1" prompt hidden="secret" export prompt not_task="text" export task public_task{inputs={}outputs={value=integer}nodes{value=eval(integer,1)}edges{edge value{from=value.outputs.value to=outputs.value}}}"#;
    for changed in [
        main.replace("lib.public_task", "lib.hidden"),
        main.replace("lib.public_task", "lib.not_task"),
        main.replace(
            "graph test.module",
            "prompt lib.local=\"collision\" graph test.module",
        ),
        main.replace("self/library", "self/library.htlk"),
        main.replace("self/library", "self/missing"),
        main.replace("use(lib.public_task)", "use(lib.public_task.value)"),
    ] {
        let bundle = bundle_with_sources(&[("main", &changed), ("library", library)]);
        let error = compiler.compile_bundle(&bundle, &catalogs()).unwrap_err();
        if changed.contains("use(lib.not_task)") {
            assert_eq!(error.diagnostics[0].code, "C_NAME");
            assert!(!error.diagnostics[0].related_locations.is_empty());
        }
    }
    let graph_module = bundle_with_sources(&[
        ("main", main),
        (
            "library",
            r#"ir_version="0.1" export task public_task{inputs={}outputs={}nodes{}edges{}} graph imported{inputs={}outputs={}nodes{}edges{}}"#,
        ),
    ]);
    assert!(compiler.compile_bundle(&graph_module, &catalogs()).is_err());
    let original = bundle_with_sources(&[("main", main), ("library", library)]);
    let bytes = compiler
        .compile_bundle(&original, &catalogs())
        .unwrap()
        .executable_bytes;
    let edited = bundle_with_sources(&[
        ("main", main),
        (
            "library",
            &library.replace("eval(integer,1)", "eval(integer,2)"),
        ),
    ]);
    assert_ne!(
        bytes,
        compiler
            .compile_bundle(&edited, &catalogs())
            .unwrap()
            .executable_bytes
    );
    let mut relocated = original.clone();
    let package = relocated.packages.values_mut().next().unwrap();
    let mut text = package.sources.remove("src/library.htlk").unwrap();
    text.push_str("\n-- moved without semantic changes\n");
    package.sources.insert("other/library.htlk".into(), text);
    package
        .manifest
        .modules
        .insert("library".into(), "other/library.htlk".into());
    let pin = package.digest(&Limits::default()).unwrap();
    let package = relocated.packages.into_values().next().unwrap();
    relocated.packages = BTreeMap::from([(pin, package)]);
    relocated.entry.package_digest = pin;
    assert_ne!(original.entry.package_digest, pin);
    assert_eq!(
        bytes,
        compiler
            .compile_bundle(&relocated, &catalogs())
            .unwrap()
            .executable_bytes
    );
    let typed = r#"ir_version="0.1" export task public_task{inputs={value=integer}outputs={value=integer}nodes{}edges{edge value{from=inputs.value to=outputs.value}}}"#;
    let caller = r#"ir_version="0.1" import lib from "self/library" graph test.caller{inputs={value=string}outputs={value=integer}nodes{work=use(lib.public_task)}edges{edge input{from=inputs.value to=work.inputs.value}edge value{from=work.outputs.value to=outputs.value}}}"#;
    let bundle = bundle_with_sources(&[("main", caller), ("library", typed)]);
    compiler
        .inspect_module(
            &bundle,
            &ModuleRef {
                package_digest: bundle.entry.package_digest,
                module_path: "library".into(),
            },
            &["public_task".into()],
            &catalogs(),
        )
        .unwrap();
    assert_eq!(
        compiler
            .compile_bundle(&bundle, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_TYPE"
    );
    let mut missing = original;
    missing
        .packages
        .values_mut()
        .next()
        .unwrap()
        .sources
        .remove("src/library.htlk");
    assert_eq!(
        compiler
            .compile_bundle(&missing, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_BUNDLE"
    );
    assert!(
        SourceBundle::from_json(
            br#"{"format":"htlk_source_bundle","format":"htlk_source_bundle"}"#,
            &Limits::default()
        )
        .is_err()
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

fn operation_catalogs(
    tools: &str,
    resources: &str,
    templates: &str,
    prompts: &str,
    schemas: &str,
) -> MCPCatalogInputs {
    let header = format!(
        r#""catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{{"service":{{"deployment_id":"fixture","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}}}},"schema_documents":{schemas}"#
    );
    let json = format!(
        r#"{{"tools":{{{header},"tools":{tools}}},"resources":{{{header},"resources":{resources},"templates":{templates}}},"prompts":{{{header},"prompts":{prompts}}}}}"#
    );
    MCPCatalogInputs::from_json(json.as_bytes(), &Limits::default()).unwrap()
}

#[test]
fn all_mcp_categories_waits_and_retry_options_lower_to_exact_interfaces() {
    let (compiler, _) = compiler();
    let catalogs = operation_catalogs(
        r#"[{"server":"service","descriptor":{"name":"tool","inputSchema":{"type":"object"},"outputSchema":{"type":"object"}}}]"#,
        r#"[{"server":"service","descriptor":{"name":"notes","uri":"file:///notes"}}]"#,
        r#"[{"server":"service","descriptor":{"name":"document","uriTemplate":"file:///docs/{customerId}"}}]"#,
        r#"[{"server":"service","descriptor":{"name":"prompt","arguments":[{"name":"Content-Type","required":false}]}}]"#,
        "{}",
    );
    let source = r#"ir_version="0.1"
      graph test.operations {
        inputs={} outputs={tool=json,resource=McpResourceResult,template=McpResourceResult,prompt=McpPromptResult,wait=string}
        nodes{
          arguments=eval(json,{})
          variables=eval(record{"customerId"=string},{"customerId"="customer"})
          tool=call(mcp.tool("service","tool")){retry{max_attempts=2 on=["Z","A","A"] backoff_ms=[0]} limits{attempt_timeout_ms=100 max_mcp_calls=2}}
          resource=read(mcp.resource("service","file:///notes"))
          template=read(mcp.template("service","file:///docs/{customerId}"))
          prompt=fetch(mcp.prompt("service","prompt"))
          approval=wait(string){topic="review" timeout_ms=1000}
        }
        edges{
          edge tool_args{from=arguments.outputs.value to=tool.inputs.arguments}
          edge template_args{from=variables.outputs.value to=template.inputs.arguments}
          edge prompt_args{from=arguments.outputs.value to=prompt.inputs.arguments}
          edge wait_args{from=arguments.outputs.value to=approval.inputs.request}
          edge tool{from=tool.outputs.value to=outputs.tool}
          edge resource{from=resource.outputs.value to=outputs.resource}
          edge template{from=template.outputs.value to=outputs.template}
          edge prompt{from=prompt.outputs.value to=outputs.prompt}
          edge wait{from=approval.outputs.value to=outputs.wait}
        }
      }"#;
    let output = compiler.compile_source(source, &catalogs).unwrap();
    assert_eq!(output.canonical_document.fields().bindings.len(), 4);
    let root =
        &output.canonical_document.fields().scopes[&output.canonical_document.fields().root_scope];
    let node = root
        .fields()
        .nodes
        .iter()
        .find(|n| n.id().as_str() == "tool")
        .unwrap();
    let Operation::Mcp { retry, .. } = &node.fields().operation else {
        panic!()
    };
    assert_eq!(retry.max_attempts(), 2);
    assert_eq!(retry.on(), ["A", "Z"]);
    let template = root
        .fields()
        .nodes
        .iter()
        .find(|n| n.id().as_str() == "template")
        .unwrap();
    let ValueTypeKind::Record(fields) = template
        .fields()
        .inputs
        .get("arguments")
        .unwrap()
        .value_type()
        .kind()
    else {
        panic!()
    };
    assert_eq!(fields[0].0, "customerId");
    assert!(fields[0].1.required());
}

#[test]
fn schema_less_tools_compile_with_exact_descriptors_and_the_uniform_envelope() {
    let (compiler, _) = compiler();
    let catalogs = operation_catalogs(
        r#"[{"server":"service","descriptor":{"name":"text_only","inputSchema":{"type":"object"}}}]"#,
        "[]",
        "[]",
        "[]",
        "{}",
    );
    compiler
        .compile_source(
            "ir_version=\"0.1\" graph test.empty{inputs={} outputs={} nodes{} edges{}}",
            &catalogs,
        )
        .unwrap();
    let source = r#"ir_version="0.1" graph test.text{inputs={} outputs={value=McpToolResult} nodes{args=eval(json,{}) work=call(mcp.tool("service","text_only"))} edges{edge args{from=args.outputs.value to=work.inputs.arguments} edge result{from=work.outputs.value to=outputs.value}}}"#;
    let output = compiler.compile_source(source, &catalogs).unwrap();
    let binding = output
        .canonical_document
        .fields()
        .bindings
        .values()
        .next()
        .unwrap();
    assert!(matches!(
        binding.kind(),
        McpBindingKind::Tool {
            output_schema: None,
            ..
        }
    ));
    let cbor::Value::Map(descriptor) =
        output.canonical_document.fields().documents[&binding.descriptor()].value()
    else {
        panic!()
    };
    assert!(descriptor.get("outputSchema").is_none());
    assert_eq!(binding.result_policy(), Some(&ToolResultPolicy::default()));
    let canonical = compiler
        .compile_document(output.canonical_document.clone(), &catalogs)
        .unwrap();
    assert_eq!(canonical.executable_bytes, output.executable_bytes);
}

#[test]
fn adopted_result_policies_keep_inference_origins_and_source_canonical_admission_exact() {
    use analyzer::{
        InferenceContext, InferencePolicy, InferenceSample, SampleObservation, infer_schema,
    };
    use cbor::{Map, Value};
    let (base, _) = compiler();
    let limits = Limits::default();
    let catalogs = operation_catalogs(
        r#"[{"server":"service","descriptor":{"name":"flex","inputSchema":{"type":"object"}}}]"#,
        "[]",
        "[]",
        "[]",
        "{}",
    );
    let source = r#"ir_version="0.1" graph test.policy{inputs={}outputs={value=McpToolResult}nodes{args=eval(json,{})work=call(mcp.tool("service","flex"))}edges{edge args{from=args.outputs.value to=work.inputs.arguments}edge result{from=work.outputs.value to=outputs.value}}}"#;
    let initial = base.compile_source(source, &catalogs).unwrap();
    let binding = initial
        .canonical_document
        .fields()
        .bindings
        .values()
        .next()
        .unwrap();
    let application =
        JsonDocument::new(br#"{"type":"array","items":{"type":"number"}}"#, &limits).unwrap();
    let mut policy = ToolResultPolicy {
        declared_contract: DeclaredContractPolicy::Report,
        application_schema: Some(application.digest()),
        ..ToolResultPolicy::default()
    };
    policy.extraction.selection = CandidateSelection::Schema(application.digest());
    let context = InferenceContext {
        server: binding.server().digest(&limits).unwrap(),
        descriptor: binding.descriptor(),
        extraction_policy: policy.extraction.digest(&limits).unwrap(),
    };
    let samples: Vec<_> = [
        ("sample:a", r#"{"price":1}"#),
        ("sample:b", r#"{"price":2,"currency":"USD"}"#),
    ]
    .into_iter()
    .map(|(reference, value)| InferenceSample {
        reference: reference.into(),
        value: JsonDocument::new(value.as_bytes(), &limits).unwrap(),
        context,
        observation: SampleObservation {
            successful: true,
            complete: true,
            truncated: false,
            uncertain: false,
        },
        sanitized: false,
        excluded_fields: vec![],
    })
    .collect();
    let revision = infer_schema(&samples, context, &InferencePolicy::default(), &limits).unwrap();
    policy.inferred_schema = Some(revision.adoption(InferredSchemaPolicy::Observe));
    let key = (context.server, context.descriptor);
    let mut config = base.config().clone();
    config.mcp_result_policies.insert(key, policy.clone());
    for document in [
        application,
        revision.schema().clone(),
        revision.evidence().clone(),
    ] {
        config
            .mcp_result_documents
            .insert(document.digest(), document);
    }
    let selected = Compiler::new(config.clone()).unwrap();
    let output = selected.compile_source(source, &catalogs).unwrap();
    assert_eq!(
        selected
            .compile_document(output.canonical_document.clone(), &catalogs)
            .unwrap()
            .executable_bytes,
        output.executable_bytes
    );
    assert_eq!(
        base.compile_document(output.canonical_document.clone(), &catalogs)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_POLICY"
    );
    let binding = output
        .canonical_document
        .fields()
        .bindings
        .values()
        .next()
        .unwrap();
    assert!(matches!(
        binding.kind(),
        McpBindingKind::Tool {
            output_schema: None,
            ..
        }
    ));
    assert_eq!(
        binding.inference_evidence(),
        Some(revision.evidence().digest())
    );
    assert!(
        !output
            .canonical_document
            .fields()
            .schema_uris
            .values()
            .any(|id| *id == revision.evidence().digest())
    );
    let Value::Map(evidence) = revision.evidence().value() else {
        panic!()
    };
    let mut fields: Vec<_> = evidence
        .iter()
        .filter(|(name, _)| *name != "descriptor")
        .map(|(name, value)| (name.into(), value.clone()))
        .collect();
    fields.push((
        "descriptor".into(),
        Value::Text(Digest::from_bytes([9; 32]).to_string()),
    ));
    let forged =
        JsonDocument::from_value(&Value::Map(Map::try_from_entries(fields).unwrap()), &limits)
            .unwrap();
    policy.inferred_schema.as_mut().unwrap().evidence = forged.digest();
    config.mcp_result_documents.insert(forged.digest(), forged);
    config.mcp_result_policies.insert(key, policy);
    assert_eq!(
        Compiler::new(config)
            .unwrap()
            .compile_source(source, &catalogs)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SCHEMA"
    );
}

#[test]
fn external_schema_closure_is_offline_exact_and_omits_unused_retrievals() {
    let (compiler, _) = compiler();
    let catalogs = operation_catalogs(
        r#"[{"server":"service","descriptor":{"name":"tool","inputSchema":{"$id":"https://example.test/input","type":"object","properties":{"name":{"$ref":"common#/$defs/Name"}}},"outputSchema":{"type":"object"}}}]"#,
        "[]",
        "[]",
        "[]",
        r#"{"https://example.test/common":{"$defs":{"Name":{"type":"string"}}},"https://example.test/unused":{"type":"boolean"}}"#,
    );
    let source = r#"ir_version="0.1" graph test.schemas{inputs={} outputs={value=json} nodes{args=eval(json,{}) work=call(mcp.tool("service","tool"))} edges{edge args{from=args.outputs.value to=work.inputs.arguments} edge result{from=work.outputs.value to=outputs.value}}}"#;
    let output = compiler.compile_source(source, &catalogs).unwrap();
    assert!(
        output
            .canonical_document
            .fields()
            .schema_uris
            .contains_key("https://example.test/common")
    );
    assert!(
        !output
            .canonical_document
            .fields()
            .schema_uris
            .contains_key("https://example.test/unused")
    );
    assert_eq!(output.canonical_document.fields().schema_uris.len(), 3);
}

#[test]
fn pure_loop_carried_presence_and_until_are_verified_in_body_context() {
    let (compiler, _) = compiler();
    let source = r#"ir_version="0.1"
      graph test.loop{inputs={seed=integer} outputs={value=integer}
        nodes{repeat=loop{inputs={seed=integer} outputs={value=integer} carried={counter=inputs.seed}
          body{nodes{step=eval(integer,inputs.counter){inputs={counter=integer}}} edges{
            edge carry{from=carried.counter to=step.inputs.counter}
            edge next{from=step.outputs.value to=next.counter}
            edge value{from=step.outputs.value to=outputs.value}
          }} until=next.counter>=0 and repeat_not_present.outputs.value==0 max_iterations=1
        }} edges{edge seed{from=inputs.seed to=repeat.inputs.seed} edge value{from=repeat.outputs.value to=outputs.value}}}
    "#;
    assert!(compiler.compile_source(source, &catalogs()).is_err());
    let source = source.replace(" and repeat_not_present.outputs.value==0", "");
    let output = compiler.compile_source(&source, &catalogs()).unwrap();
    assert_eq!(output.analysis().structural_summary().expanded_nodes(), 2);
}

#[test]
fn sorted_expression_diagnostics_point_to_original_operand_bytes() {
    let (compiler, _) = compiler();
    let source = r#"ir_version="0.1" graph test.bad{inputs={} outputs={value=json} nodes{work=eval(json,{z="valid",a=length(17)})} edges{edge result{from=work.outputs.value to=outputs.value}}}"#;
    let error = compiler.compile_source(source, &catalogs()).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "C_TYPE");
    let DiagnosticLocation::SourceSpan(span) = &error.diagnostics[0].location else {
        panic!("{error:?}")
    };
    assert_eq!(&source[span.start_byte..span.end_byte], "17");
}

#[test]
fn runtime_obligations_are_available_as_an_immutable_analysis_report() {
    let (compiler, _) = compiler();
    let source = r#"ir_version="0.1" graph test.checked{inputs={value=json} outputs={value=string} nodes{work=eval(string,inputs.value){inputs={value=json}}} edges{edge input{from=inputs.value to=work.inputs.value} edge output{from=work.outputs.value to=outputs.value}}}"#;
    let output = compiler.compile_source(source, &catalogs()).unwrap();
    assert!(output.analysis().graphs().plans().any(|(_, _, plan)| {
        plan.expressions()
            .values()
            .any(|e| !e.runtime_checks().is_empty())
    }));
    assert_eq!(output.analysis().fingerprint(), output.fingerprint);
}

#[test]
fn regex_literals_templates_and_native_extreme_numbers_preserve_representation() {
    let (compiler, _) = compiler();
    let source = r#"ir_version="0.1"
      prompt sample(value=float)="{{{value}}}"
      graph test.literals{inputs={} outputs={text=string,pattern=regex,number=integer}
        nodes{rendered=eval(string,render(&sample,{value=1e100})) pattern=eval(regex,/[a\/b]+/mi) number=eval(integer,-9223372036854775808)}
        edges{edge text{from=rendered.outputs.value to=outputs.text} edge pattern{from=pattern.outputs.value to=outputs.pattern} edge number{from=number.outputs.value to=outputs.number}}}
    "#;
    let output = compiler.compile_source(source, &catalogs()).unwrap();
    let root =
        &output.canonical_document.fields().scopes[&output.canonical_document.fields().root_scope];
    let pattern = root
        .fields()
        .nodes
        .iter()
        .find(|n| n.id().as_str() == "pattern")
        .unwrap();
    let Operation::Eval(e) = &pattern.fields().operation else {
        panic!()
    };
    assert!(matches!(e.kind(),ExpressionKind::Regex{flags,..} if flags=="im"));
    assert!(
        compiler
            .compile_source(&source.replace("/mi", "/ii"), &catalogs())
            .is_err()
    );
}

#[test]
fn duplicate_fields_contextual_names_and_illegal_options_are_rejected() {
    let (compiler, _) = compiler();
    for source in [
        "ir_version=\"0.1\" graph test.bad{inputs={x=string,\"x\"=string} outputs={} nodes{} edges{}}",
        "ir_version=\"0.1\" graph test.bad{inputs={} outputs={} nodes{} edges{} limits{timeout_ms=1 timeout_ms=2}}",
        "ir_version=\"0.1\" graph test.bad{inputs={} outputs={} nodes{} edges{} postconditions=1==not true}",
        "ir_version=\"0.1\" graph test.bad{inputs={} outputs={} nodes{inputs=eval(string,\"bad\")} edges{}}",
        "ir_version=\"0.1\" graph test.bad{inputs={} outputs={} nodes{work=wait(optional(string)){topic=\"t\" timeout_ms=1}} edges{}}",
    ] {
        assert!(
            compiler.compile_source(source, &catalogs()).is_err(),
            "{source}"
        );
    }
    let error=compiler.compile_source("ir_version=\"0.1\" type Duplicate=enum(\"x\",\"x\") graph test.empty{inputs={} outputs={} nodes{} edges{}}",&catalogs()).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "C_DUPLICATE");
}

#[test]
fn source_work_intermediate_and_report_ceilings_fail_closed() {
    let (base, _) = compiler();
    let source = r#"ir_version="0.1" graph test.limits{inputs={} outputs={} nodes{} edges{}}"#;
    for kind in 0..5 {
        let mut config = base.config().clone();
        match kind {
            0 => config.frontend.max_source_bytes = 8,
            1 => config.frontend.max_tokens = 3,
            2 => config.frontend.max_work = 1,
            3 => config.frontend.max_intermediate_bytes = 1,
            _ => config.frontend.max_report_bytes = 1,
        };
        let compiler = Compiler::new(config).unwrap();
        assert_eq!(
            compiler
                .compile_source(source, &catalogs())
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_SOURCE_LIMIT",
            "limit {kind}"
        );
    }
    let source = format!(
        r#"ir_version="0.1" graph test.deep{{inputs={{value=json}} outputs={{value=json}} nodes{{value=eval(json,inputs.value{}){{inputs={{value=json}}}}}} edges{{edge input{{from=inputs.value to=value.inputs.value}} edge output{{from=value.outputs.value to=outputs.value}}}}}}"#,
        "[0]".repeat(500)
    );
    assert_eq!(
        base.compile_source(&source, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_LIMIT"
    );
}

#[test]
fn hard_token_and_cost_limits_require_explicit_host_capabilities() {
    let (base, _) = compiler();
    let source = r#"ir_version="0.1" graph test.budget{inputs={} outputs={} nodes{} edges{} limits{max_tokens=0 max_cost_units=0}}"#;
    assert_eq!(
        base.compile_source(source, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_POLICY"
    );
    let mut config = base.config().clone();
    config.capabilities = CompilerCapabilities {
        hard_token_limit: true,
        hard_cost_limit: true,
    };
    Compiler::new(config)
        .unwrap()
        .compile_source(source, &catalogs())
        .unwrap();
}

#[test]
fn linked_generic_callbacks_use_exact_complete_manifests() {
    let (base, _) = compiler();
    let l = Limits::default();
    let t: Identifier = "t".parse().unwrap();
    let variable =
        ValueType::new(ValueTypeKind::Var(t.clone()), TypeContext::Signature, &l).unwrap();
    let port = Port::new(variable.clone(), true);
    let callback = ValueType::new(
        ValueTypeKind::Function {
            parameters: vec![port.clone()],
            returns: Box::new(port.clone()),
        },
        TypeContext::Signature,
        &l,
    )
    .unwrap();
    let identity =
        FunctionSignature::new(vec![t.clone()], vec![port.clone()], port.clone(), &l).unwrap();
    let apply = FunctionSignature::new(
        vec![t],
        vec![port.clone(), Port::new(callback, true)],
        port,
        &l,
    )
    .unwrap();
    let unused = FunctionSignature::new(
        vec![],
        vec![],
        Port::new(ValueType::builtin(BuiltinType::String), true),
        &l,
    )
    .unwrap();
    let digest = Digest::from_bytes([42; 32]);
    let library = Library::new(
        "test.functions".into(),
        "0.1".into(),
        digest,
        vec![
            ("identity".parse().unwrap(), identity),
            ("apply".parse().unwrap(), apply),
            ("unused".parse().unwrap(), unused),
        ],
        &l,
    )
    .unwrap();
    let mut config = base.config().clone();
    config.libraries.insert(digest, library.clone());
    let compiler = Compiler::new(config).unwrap();
    let source = format!(
        r#"ir_version="0.1" predicate_library ops=predicates.library("test.functions"){{version="0.1" digest="{digest}"}} graph test.generics{{inputs={{}} outputs={{value=integer}} nodes{{work=eval(integer,ops.apply(1,&ops.identity))}} edges{{edge result{{from=work.outputs.value to=outputs.value}}}}}}"#
    );
    let output = compiler.compile_source(&source, &catalogs()).unwrap();
    assert_eq!(
        output.canonical_document.fields().libraries[&digest],
        library
    );
    assert_eq!(
        output.canonical_document.fields().libraries[&digest]
            .functions()
            .len(),
        3
    );
    for alias in ["not", "true", "false", "null"] {
        let contextual = source.replace("ops", alias);
        assert_eq!(
            compiler
                .compile_source(&contextual, &catalogs())
                .unwrap()
                .executable_bytes,
            output.executable_bytes,
            "{alias}"
        );
    }
    assert_eq!(
        base.compile_document(output.canonical_document, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_LIBRARY"
    );
}

#[test]
fn contextual_node_names_resolve_as_references_in_conditions() {
    let (compiler, _) = compiler();
    for name in ["true", "false", "null", "not"] {
        let source = format!(
            r#"ir_version="0.1" graph test.contextual {{ inputs={{}} outputs={{value=integer}}
            nodes{{{name}=eval(integer,1)}} edges{{edge result{{from={name}.outputs.value to=outputs.value when={name}.outputs.value==1}}}} }}"#
        );
        compiler.compile_source(&source, &catalogs()).unwrap();
    }
}

#[test]
fn canonical_schema_revalidation_distinguishes_retrieval_context_from_bytes() {
    let (compiler, _) = compiler();
    let tools = r#"[{"server":"service","descriptor":{"name":"tool","inputSchema":{"type":"object","allOf":[{"$ref":"https://example.test/common"}]},"outputSchema":{"type":"object"}}}]"#;
    let make = |schemas| operation_catalogs(tools, "[]", "[]", "[]", schemas);
    let original = make(r#"{"https://example.test/common":{"type":"object"}}"#);
    let source = r#"ir_version="0.1" graph test.schemas{inputs={} outputs={value=json} nodes{args=eval(json,{}) work=call(mcp.tool("service","tool"))} edges{edge args{from=args.outputs.value to=work.inputs.arguments} edge result{from=work.outputs.value to=outputs.value}}}"#;
    let document = compiler
        .compile_source(source, &original)
        .unwrap()
        .canonical_document;
    compiler
        .compile_document(document.clone(), &original)
        .unwrap();
    let comparison = source.replace("outputs={value=json}", "outputs={value=integer}")
        .replace("work=call(mcp.tool(\"service\",\"tool\"))", "work=call(mcp.tool(\"service\",\"tool\")) done=eval(integer,1){when=work.outputs.value==1}")
        .replace("from=work.outputs.value to=outputs.value", "from=done.outputs.value to=outputs.value");
    assert_eq!(
        compiler
            .compile_source(&comparison, &original)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_TYPE"
    );
    for schemas in [
        "{}",
        r#"{"https://example.test/common":{"type":"object","required":["new_field"]}}"#,
    ] {
        let changed = make(schemas);
        assert_eq!(
            compiler
                .compile_document(document.clone(), &changed)
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_SCHEMA"
        );
        assert_eq!(
            compiler
                .join_graphs(&document, &document, &chain_join(), &changed)
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_SCHEMA"
        );
    }
}

#[test]
fn public_ports_and_retry_members_follow_their_closed_grammar() {
    let (compiler, _) = compiler();
    let catalogs = operation_catalogs(
        r#"[{"server":"service","descriptor":{"name":"tool","inputSchema":{"type":"object"},"outputSchema":{"type":"object"}}}]"#,
        "[]",
        "[]",
        "[]",
        "{}",
    );
    let source = r#"ir_version="0.1" graph test.syntax{inputs={} outputs={value=json} nodes{args=eval(json,{}) work=call(mcp.tool("service","tool")){retry{max_attempts=2 on=["MCP_TIMEOUT"] backoff_ms=[0]}}} edges{edge args{from=args.outputs.value to=work.inputs.arguments} edge result{from=work.outputs.value to=outputs.value}}}"#;
    compiler.compile_source(source, &catalogs).unwrap();
    for invalid in [
        source.replace("outputs={value=json}", "outputs={\"value\"=json}"),
        source.replace("max_attempts=2", "max_attempts=(2)"),
        source.replace("[0]", "[(0)]"),
        source.replace("[\"MCP_TIMEOUT\"]", "[\"\"\"MCP_TIMEOUT\"\"\"]"),
    ] {
        assert_eq!(
            compiler
                .compile_source(&invalid, &catalogs)
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_SYNTAX"
        );
    }
}

#[test]
fn typed_bundle_rejects_extra_sources_before_snapshot_processing() {
    let (base, _) = compiler();
    let mut config = base.config().clone();
    config.frontend.max_modules = 1;
    let compiler = Compiler::new(config).unwrap();
    let mut bundle = bundle_with_sources(&[(
        "main",
        r#"ir_version="0.1" graph test.empty{inputs={} outputs={} nodes{} edges{}}"#,
    )]);
    let package = bundle.packages.values_mut().next().unwrap();
    for index in 0..1000 {
        package
            .sources
            .insert(format!("extra/{index}.htlk"), "invalid header".into());
    }
    assert_eq!(
        compiler
            .compile_bundle(&bundle, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_BUNDLE"
    );
}

#[test]
fn conditional_writer_warnings_and_deduplicated_origins_survive_inspection() {
    let (compiler, _) = compiler();
    let source = r#"ir_version="0.1"
      export task choose{inputs={flag=boolean} outputs={value=boolean} nodes{} edges{
        edge first{from=inputs.flag to=outputs.value when=inputs.flag}
        edge second{from=inputs.flag to=outputs.value when=inputs.flag}
      }}"#;
    let main = r#"ir_version="0.1" import library from "self/library" graph test.choose{inputs={flag=boolean} outputs={value=boolean} nodes{choose=use(library.choose)} edges{edge flag{from=inputs.flag to=choose.inputs.flag} edge value{from=choose.outputs.value to=outputs.value}}}"#;
    let bundle = bundle_with_sources(&[("main", main), ("library", source)]);
    let report = compiler
        .inspect_module(
            &bundle,
            &ModuleRef {
                package_digest: bundle.entry.package_digest,
                module_path: "library".into(),
            },
            &[],
            &catalogs(),
        )
        .unwrap();
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].code, "W_CONDITIONAL_WRITERS");
    assert!(matches!(
        report.diagnostics[0].location,
        DiagnosticLocation::SourceSpan(_)
    ));
    let output = compiler.compile_bundle(&bundle, &catalogs()).unwrap();
    assert_eq!(output.diagnostics.len(), 1);
}

#[test]
fn private_implementation_edits_refresh_inspection_without_changing_interface() {
    let (compiler, _) = compiler();
    let main = r#"ir_version="0.1" graph test.empty{inputs={} outputs={} nodes{} edges{}}"#;
    let library = r#"ir_version="0.1" prompt hidden="first" export task value{inputs={} outputs={value=string} nodes{work=eval(string,render(&hidden,{}))} edges{edge value{from=work.outputs.value to=outputs.value}}}"#;
    let mut summaries = vec![];
    for source in [library.to_owned(), library.replace("first", "second")] {
        let bundle = bundle_with_sources(&[("main", main), ("library", &source)]);
        let report = compiler
            .inspect_module(
                &bundle,
                &ModuleRef {
                    package_digest: bundle.entry.package_digest,
                    module_path: "library".into(),
                },
                &["value".into()],
                &catalogs(),
            )
            .unwrap();
        let ExportSummary::Task {
            scope_digest,
            inputs,
            outputs,
            ..
        } = &report.exports[0]
        else {
            panic!()
        };
        summaries.push((*scope_digest, inputs.clone(), outputs.clone()));
    }
    assert_ne!(summaries[0].0, summaries[1].0);
    assert_eq!(summaries[0].1, summaries[1].1);
    assert_eq!(summaries[0].2, summaries[1].2);
}

#[test]
fn closed_source_paths_and_invalid_unused_bodies_are_checked_for_inspection() {
    let (compiler, _) = compiler();
    let main = r#"ir_version="0.1" graph test.empty{inputs={} outputs={} nodes{} edges{}}"#;
    let invalid = r#"ir_version="0.1" export type Valid=string task unused{inputs={} outputs={value=string} nodes{} edges{}}"#;
    let bundle = bundle_with_sources(&[("main", main), ("library", invalid)]);
    assert!(
        compiler
            .inspect_module(
                &bundle,
                &ModuleRef {
                    package_digest: bundle.entry.package_digest,
                    module_path: "library".into()
                },
                &[],
                &catalogs()
            )
            .is_err()
    );
    for path in [
        "../main.htlk",
        "/main.htlk",
        "src\\main.htlk",
        "src/Main.htlk",
        "src/./main.htlk",
    ] {
        let mut package = bundle.packages.values().next().unwrap().clone();
        package.manifest.modules.insert("main".into(), path.into());
        let text = package.sources.remove("src/main.htlk").unwrap();
        package.sources.insert(path.into(), text);
        let pin = package.digest(&Limits::default()).unwrap();
        let invalid = SourceBundle {
            entry: ModuleRef {
                package_digest: pin,
                module_path: "main".into(),
            },
            packages: BTreeMap::from([(pin, package)]),
        };
        assert_eq!(
            compiler
                .compile_bundle(&invalid, &catalogs())
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_SOURCE_BUNDLE",
            "{path}"
        );
    }
}

#[test]
fn catalog_conflicts_and_pin_assertions_are_rejected_before_compilation() {
    let l = Limits::default();
    let duplicate = r#"[{"server":"service","descriptor":{"name":"tool","inputSchema":{"type":"object"}}},{"server":"service","descriptor":{"name":"tool","description":"changed","inputSchema":{"type":"object"}}}]"#;
    let header = r#""catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"service":{"deployment_id":"fixture","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{}"#;
    let json = format!(
        r#"{{"tools":{{{header},"tools":{duplicate}}},"resources":{{{header},"resources":[],"templates":[]}},"prompts":{{{header},"prompts":[]}}}}"#
    );
    assert_eq!(
        MCPCatalogInputs::from_json(json.as_bytes(), &l)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_MCP"
    );
    let pinned = format!(
        r#"[{{"server":"service","descriptor":{{"name":"tool","inputSchema":{{"type":"object"}}}},"expected_descriptor_digest":"{}"}}]"#,
        Digest::from_bytes([0; 32])
    );
    let json = format!(
        r#"{{"tools":{{{header},"tools":{pinned}}},"resources":{{{header},"resources":[],"templates":[]}},"prompts":{{{header},"prompts":[]}}}}"#
    );
    assert!(MCPCatalogInputs::from_json(json.as_bytes(), &l).is_err());
    let changed = header.replace("fixture", "other");
    let json = format!(
        r#"{{"tools":{{{header},"tools":[]}},"resources":{{{changed},"resources":[],"templates":[]}},"prompts":{{{header},"prompts":[]}}}}"#
    );
    assert!(MCPCatalogInputs::from_json(json.as_bytes(), &l).is_err());
}

#[test]
fn two_exact_versions_of_one_source_package_can_coexist() {
    let (compiler, _) = compiler();
    let library = r#"ir_version="0.1" export type Value=string"#;
    let fixture = bundle_with_sources(&[("main", library)]);
    let mut first = fixture.packages.values().next().unwrap().clone();
    first.manifest.package_name = "shared.library".into();
    first.manifest.package_version = "one".into();
    let mut second = first.clone();
    second.manifest.package_version = "two".into();
    let a = first.digest(&Limits::default()).unwrap();
    let b = second.digest(&Limits::default()).unwrap();
    let main = r#"ir_version="0.1" import one from "first/main" import two from "second/main" graph test.versions{inputs={value=one.Value} outputs={value=two.Value} nodes{} edges{edge pass{from=inputs.value to=outputs.value}}}"#;
    let mut app = bundle_with_sources(&[("main", main)])
        .packages
        .values()
        .next()
        .unwrap()
        .clone();
    app.manifest.dependencies = BTreeMap::from([("first".into(), a), ("second".into(), b)]);
    let root = app.digest(&Limits::default()).unwrap();
    let bundle = SourceBundle {
        entry: ModuleRef {
            package_digest: root,
            module_path: "main".into(),
        },
        packages: BTreeMap::from([(a, first), (b, second), (root, app)]),
    };
    compiler.compile_bundle(&bundle, &catalogs()).unwrap();
}

#[test]
fn false_guards_do_not_remove_execution_dependency_cycles() {
    let (compiler, _) = compiler();
    let source = r#"ir_version="0.1" graph test.cycle{inputs={} outputs={a=boolean,b=boolean} nodes{
      a=eval(boolean,true){when=false and status(@b)=="succeeded"}
      b=eval(boolean,true){when=status(@a)=="succeeded"}
    }edges{edge a{from=a.outputs.value to=outputs.a} edge b{from=b.outputs.value to=outputs.b}}}"#;
    assert_eq!(
        compiler
            .compile_source(source, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_DEPENDENCY_CYCLE"
    );
}

#[test]
fn content_deduplication_preserves_multiple_declaration_origins() {
    let (compiler, _) = compiler();
    let body = "{inputs={} outputs={value=integer} nodes{value=eval(integer,1)} edges{edge result{from=value.outputs.value to=outputs.value}}}";
    let source = format!(
        r#"ir_version="0.1" task one {body} task two {body} graph test.shared{{inputs={{}} outputs={{a=integer,b=integer}} nodes{{a=use(one) b=use(two)}} edges{{edge a{{from=a.outputs.value to=outputs.a}} edge b{{from=b.outputs.value to=outputs.b}}}}}}"#
    );
    let output = compiler.compile_source(&source, &catalogs()).unwrap();
    assert_eq!(output.canonical_document.fields().scopes.len(), 2);
    let one = output
        .source_map
        .iter()
        .find(|e| e.declaration_name.as_deref() == Some("one"))
        .unwrap();
    let two = output
        .source_map
        .iter()
        .find(|e| e.declaration_name.as_deref() == Some("two"))
        .unwrap();
    assert_eq!(one.canonical_path, two.canonical_path);
    assert_ne!(one.span, two.span);
}

#[test]
fn triple_strings_are_restricted_to_templates_and_expression_literals() {
    let (compiler, _) = compiler();
    for source in [
        "ir_version=\"\"\"0.1\"\"\" graph test.empty{inputs={} outputs={} nodes{} edges{}}",
        "ir_version=\"0.1\" graph test.empty{inputs={} outputs={} nodes{} edges{} description=\"\"\"invalid\"\"\"}",
        "ir_version=\"0.1\" graph test.empty{inputs={\"\"\"invalid\"\"\"=string} outputs={} nodes{} edges{}}",
    ] {
        assert!(
            compiler.compile_source(source, &catalogs()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn remaining_type_presence_projection_and_scalar_grammar_forms_compile() {
    let (compiler, _) = compiler();
    let source = r#"ir_version="0.1"
      type OpaqueBytes=bytes
      type Headers=map(string)
      type Everything=record{missing=optional(union(string,null)),values=list(enum("a","b")),headers=Headers,error=Error}
      graph test.grammar{inputs={number=integer,item=optional(string)} outputs={value=optional(string),json=json}
        preconditions=inputs.number>=-1 and inputs.number<=1 and inputs.number<2 and inputs.number> -2 or not(inputs.number!=0)
        nodes{
          wait=eval(optional(string),inputs.item){inputs={item=optional(string)}}
          data=eval(json,{"Content-Type"="text/plain",""=[true,false,null,1.25e+2],zero=["a"][0],field={nested="value"}.nested})
        }
        edges{edge optional{from=inputs.item to=wait.inputs.item} edge value{from=wait.outputs.value to=outputs.value} edge data{from=data.outputs.value to=outputs.json}}
        postconditions=present(outputs.value) or not present(inputs.item)
      }"#;
    let output = compiler.compile_source(source, &catalogs()).unwrap();
    let root =
        &output.canonical_document.fields().scopes[&output.canonical_document.fields().root_scope];
    assert!(!root.fields().outputs.get("value").unwrap().required());
}

#[test]
fn repeated_joins_cannot_bypass_structural_policy_ceilings() {
    let (base, _) = compiler();
    let mut config = base.config().clone();
    let mut fields = config.policy.fields().clone();
    fields.maximum_expanded_nodes = 3;
    let policy = PolicyDocument::new(fields, &config.limits).unwrap();
    let p = &config.profile;
    config.profile = ExecutionProfile::new(
        p.core_digest(),
        p.regex_engine().clone(),
        p.schema_validator().clone(),
        p.uri_template_engine().clone(),
        policy.digest(),
        &config.limits,
    )
    .unwrap();
    config.policy = policy;
    let compiler = Compiler::new(config).unwrap();
    let operand = pass(&compiler, false);
    let first = compiler
        .join_graphs(&operand, &operand, &chain_join(), &catalogs())
        .unwrap();
    let error = compiler
        .join_graphs(
            &first.canonical_document,
            &operand,
            &chain_join(),
            &catalogs(),
        )
        .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "C_POLICY");
}

#[test]
fn import_depth_and_missing_dependency_locations_are_checked() {
    let (base, _) = compiler();
    let mut config = base.config().clone();
    config.frontend.max_depth = 3;
    let compiler = Compiler::new(config).unwrap();
    let main = r#"ir_version="0.1" import short from "self/leaf" import long from "self/branch" graph test.depth{inputs={} outputs={} nodes{} edges{}}"#;
    let bundle = bundle_with_sources(&[
        ("main", main),
        ("leaf", "ir_version=\"0.1\" import child from \"self/end\""),
        ("end", "ir_version=\"0.1\""),
        (
            "branch",
            "ir_version=\"0.1\" import leaf from \"self/leaf\"",
        ),
    ]);
    assert_eq!(
        compiler
            .compile_bundle(&bundle, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_LIMIT"
    );
    let mut package = bundle.packages.values().next().unwrap().clone();
    package
        .manifest
        .dependencies
        .insert("missing".into(), Digest::from_bytes([99; 32]));
    let pin = package.digest(&Limits::default()).unwrap();
    let bundle = SourceBundle {
        entry: ModuleRef {
            package_digest: pin,
            module_path: "main".into(),
        },
        packages: BTreeMap::from([(pin, package)]),
    };
    let error = base.compile_bundle(&bundle, &catalogs()).unwrap_err();
    let DiagnosticLocation::SourceBundlePath(p) = &error.diagnostics[0].location else {
        panic!()
    };
    assert_eq!(p, &format!("/packages/{pin}/manifest/dependencies/missing"));
}

#[test]
fn complete_bundle_json_ingress_validates_closed_shapes_versions_and_pins() {
    let l = Limits::default();
    let source = r#"ir_version=\"0.1\" graph test.empty{inputs={}outputs={}nodes{}edges{}}"#;
    let pin = Digest::from_bytes([7; 32]);
    let json = format!(
        r#"{{"format":"htlk_source_bundle","format_version":"0.1","entry":{{"package_digest":"{pin}","module_path":"main"}},"packages":{{"{pin}":{{"manifest":{{"manifest_version":"0.1","package_name":"test.app","package_version":"1","modules":{{"main":"src/main.htlk"}},"dependencies":{{"shared":"{pin}"}}}},"sources":{{"src/main.htlk":"{source}"}}}}}}}}"#
    );
    let bundle = SourceBundle::from_json(json.as_bytes(), &l).unwrap();
    assert_eq!(bundle.entry.package_digest, pin);
    assert_eq!(bundle.packages[&pin].manifest.dependencies["shared"], pin);
    for invalid in [
        "null".to_owned(),
        json.replace("htlk_source_bundle", "other"),
        json.replace("\"format_version\":\"0.1\"", "\"format_version\":\"9\""),
        json.replace("\"manifest_version\":\"0.1\"", "\"manifest_version\":\"9\""),
        json.replace("\"module_path\":\"main\"", "\"module_path\":17"),
        json.replace("\"main\":\"src/main.htlk\"", "\"main\":false"),
        json.replace(
            "\"package_digest\":",
            "\"unexpected\":0,\"package_digest\":",
        ),
        json.replace(
            &format!("\"package_digest\":\"{pin}\""),
            "\"package_digest\":\"invalid\"",
        ),
        json.replace(&format!("\"shared\":\"{pin}\""), "\"shared\":\"invalid\""),
        json.replace(
            &format!("\"packages\":{{\"{pin}\""),
            "\"packages\":{\"invalid\"",
        ),
        json.replace(
            &format!("\"src/main.htlk\":\"{source}\""),
            "\"src/main.htlk\":false",
        ),
    ] {
        let failure = SourceBundle::from_json(invalid.as_bytes(), &l).unwrap_err();
        assert!(
            matches!(
                failure.diagnostics[0].location,
                DiagnosticLocation::SourceBundlePath(_)
            ),
            "{failure:?}"
        );
        assert!(!failure.to_string().is_empty());
    }
    let (compiler, _) = compiler();
    assert_eq!(
        compiler
            .compile_bundle(&bundle, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_PACKAGE_PIN"
    );
}

#[test]
fn typed_snapshots_enforce_every_manifest_and_dependency_boundary() {
    let (base, _) = compiler();
    let main = r#"ir_version="0.1" graph test.empty{inputs={}outputs={}nodes{}edges{}}"#;
    let original = bundle_with_sources(&[("main", main)]);
    for kind in 0..10 {
        let mut package = original.packages.values().next().unwrap().clone();
        match kind {
            0 => package.manifest.package_name = "BadName".into(),
            1 => package.manifest.package_version.clear(),
            2 => package.manifest.modules.clear(),
            3 => {
                package
                    .manifest
                    .modules
                    .insert("second".into(), "src/main.htlk".into());
                package.sources.insert("src/extra.htlk".into(), main.into());
            }
            4 => {
                package
                    .manifest
                    .modules
                    .insert("main".into(), "src/missing.htlk".into());
            }
            5 => {
                package.manifest.modules =
                    BTreeMap::from([("../main".into(), "src/main.htlk".into())]);
            }
            6 => {
                package
                    .manifest
                    .dependencies
                    .insert("self".into(), Digest::from_bytes([9; 32]));
            }
            7 => {
                package
                    .manifest
                    .dependencies
                    .insert("bad-alias".into(), Digest::from_bytes([9; 32]));
            }
            8 => {
                package
                    .manifest
                    .dependencies
                    .insert("missing".into(), Digest::from_bytes([9; 32]));
            }
            _ => {
                package
                    .sources
                    .insert("src/main.htlk".into(), "ir_version=\"9\"".into());
            }
        }
        let pin = package.digest(&Limits::default()).unwrap();
        let bundle = SourceBundle {
            entry: ModuleRef {
                package_digest: pin,
                module_path: "main".into(),
            },
            packages: BTreeMap::from([(pin, package)]),
        };
        assert!(
            base.compile_bundle(&bundle, &catalogs()).is_err(),
            "case {kind}"
        );
    }
    let mut missing = original.clone();
    missing.entry.package_digest = Digest::from_bytes([9; 32]);
    assert_eq!(
        base.compile_bundle(&missing, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_PACKAGE_PIN"
    );
    let mut missing = original.clone();
    missing.entry.module_path = "missing".into();
    assert_eq!(
        base.compile_bundle(&missing, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_IMPORT"
    );
    let mut extra = original.clone();
    let mut package = extra.packages.values().next().unwrap().clone();
    package.manifest.package_name = "unused.package".into();
    extra
        .packages
        .insert(package.digest(&Limits::default()).unwrap(), package);
    assert_eq!(
        base.compile_bundle(&extra, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_BUNDLE"
    );
    for kind in 0..4 {
        let mut config = base.config().clone();
        match kind {
            0 => config.frontend.max_packages = 1,
            1 => config.frontend.max_modules = 1,
            2 => config.frontend.max_source_bytes = 1,
            _ => config.frontend.max_depth = 1,
        }
        let bounded = Compiler::new(config).unwrap();
        let bundle = if kind == 0 {
            extra.clone()
        } else if kind == 1 {
            bundle_with_sources(&[("main", main), ("other", "ir_version=\"0.1\"")])
        } else if kind == 3 {
            let mut app = original.packages.values().next().unwrap().clone();
            let dependency = app.clone();
            let pin = dependency.digest(&Limits::default()).unwrap();
            app.manifest.dependencies.insert("shared".into(), pin);
            let root = app.digest(&Limits::default()).unwrap();
            SourceBundle {
                entry: ModuleRef {
                    package_digest: root,
                    module_path: "main".into(),
                },
                packages: BTreeMap::from([(root, app), (pin, dependency)]),
            }
        } else {
            original.clone()
        };
        assert_eq!(
            bounded
                .compile_bundle(&bundle, &catalogs())
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_SOURCE_LIMIT",
            "case {kind}"
        );
    }
    let tiny = Limits {
        max_document_bytes: 8,
        ..Limits::default()
    };
    assert_eq!(
        original
            .packages
            .values()
            .next()
            .unwrap()
            .digest(&tiny)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_LIMIT"
    );
}

#[test]
fn public_interfaces_include_all_export_kinds_and_enforce_requested_report_bounds() {
    let (base, _) = compiler();
    let bundle = bundle_with_sources(&[
        (
            "main",
            r#"ir_version="0.1" graph test.empty{inputs={}outputs={}nodes{}edges{}}"#,
        ),
        (
            "library",
            r#"ir_version="0.1" export type Value=string export prompt greeting(name=Value)="Hello {name}" export task echo{description="Public task"inputs={value=Value}outputs={value=Value}nodes{}edges{edge value{from=inputs.value to=outputs.value}}preconditions=true postconditions=true limits{timeout_ms=100}}"#,
        ),
    ]);
    let module = ModuleRef {
        package_digest: bundle.entry.package_digest,
        module_path: "library".into(),
    };
    let symbols = vec!["greeting".into(), "Value".into(), "echo".into()];
    let report = base
        .inspect_module(&bundle, &module, &symbols, &catalogs())
        .unwrap();
    assert_eq!(report.interface_version, "0.1");
    assert_eq!(report.module, module);
    assert_eq!(report.profile, base.config().profile);
    assert_eq!(
        report
            .available_exports
            .iter()
            .map(|e| e.kind)
            .collect::<Vec<_>>(),
        [ExportKind::Type, ExportKind::Task, ExportKind::Prompt]
    );
    assert!(
        matches!(&report.exports[0],ExportSummary::Type{name,value_type}if name=="Value"&&*value_type==ValueType::builtin(BuiltinType::String))
    );
    assert!(
        matches!(&report.exports[1],ExportSummary::Task{description,limits,..}if description=="Public task"&&limits.timeout_ms()==Some(100))
    );
    assert!(
        matches!(&report.exports[2],ExportSummary::Prompt{parameters,..}if parameters.len()==1&&parameters[0].0.as_str()=="name")
    );
    assert_eq!(
        base.inspect_module(
            &bundle,
            &module,
            &["echo".into(), "echo".into()],
            &catalogs()
        )
        .unwrap_err()
        .diagnostics[0]
            .code,
        "C_DUPLICATE"
    );
    for missing in [
        ModuleRef {
            package_digest: Digest::from_bytes([0; 32]),
            module_path: "library".into(),
        },
        ModuleRef {
            module_path: "missing".into(),
            ..module.clone()
        },
    ] {
        assert!(
            base.inspect_module(&bundle, &missing, &[], &catalogs())
                .is_err()
        );
    }
    let mut config = base.config().clone();
    config.frontend.max_report_bytes = 1;
    assert_eq!(
        Compiler::new(config)
            .unwrap()
            .inspect_module(&bundle, &module, &symbols, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_LIMIT"
    );
}

#[test]
fn join_json_round_trips_complete_requests_and_rejects_each_malformed_component() {
    let (compiler, _) = compiler();
    let operand = pass(&compiler, false);
    let json = r#"{"id":"test.joined","left_alias":"left","right_alias":"right","edges":[{"id":"pipe","from":{"graph":"left","port":"value"},"to":{"graph":"right","port":"value"}}],"inputs":[{"name":"value","to":[{"graph":"left","port":"value"}]}],"outputs":[{"name":"value","from":{"graph":"right","port":"value"}}]}"#;
    let join = JoinSpec::from_json(json.as_bytes(), &Limits::default()).unwrap();
    assert_eq!(
        compiler
            .join_graphs(&operand, &operand, &join, &catalogs())
            .unwrap()
            .executable_bytes,
        compiler
            .join_graphs(&operand, &operand, &chain_join(), &catalogs())
            .unwrap()
            .executable_bytes
    );
    for invalid in [
        "{".to_owned(),
        "[]".into(),
        json.replace("\"left\"", "\"third\""),
        json.replace("\"port\":\"value\"", "\"port\":null"),
        json.replace("\"graph\":", "\"extra\":1,\"graph\":"),
        json.replace("\"edges\":[", "\"edges\":false,\"unused\":["),
        json.replace("\"inputs\":[", "\"inputs\":null,\"unused\":["),
        json.replace("\"outputs\":[", "\"outputs\":{},\"unused\":["),
    ] {
        assert_eq!(
            JoinSpec::from_json(invalid.as_bytes(), &Limits::default())
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_JOIN",
            "{invalid}"
        );
    }
}

#[test]
fn join_rejects_every_invalid_public_binding_and_preserves_operand_error_context() {
    let (base, _) = compiler();
    let operand = pass(&base, false);
    for kind in 0..13 {
        let mut join = chain_join();
        match kind {
            0 => join.right_alias = join.left_alias.clone(),
            1 => join.left_alias = "bad-name".into(),
            2 => join.edges[0].to.graph = JoinOperand::Left,
            3 => join.edges[0].from.port = "missing".into(),
            4 => join.edges[0].to.port = "missing".into(),
            5 => join.edges.push(join.edges[0].clone()),
            6 => join.inputs.push(join.inputs[0].clone()),
            7 => join.inputs[0].to.clear(),
            8 => join.inputs.clear(),
            9 => join.outputs.push(join.outputs[0].clone()),
            10 => join.outputs[0].from.port = "missing".into(),
            11 => join.edges[0].id = "join_out_0".into(),
            _ => join.inputs[0].name = "bad-name".into(),
        }
        assert_eq!(
            base.join_graphs(&operand, &operand, &join, &catalogs())
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_JOIN",
            "case {kind}"
        );
    }
    let integer=base.compile_source(r#"ir_version="0.1"graph test.integer{inputs={value=integer}outputs={value=integer}nodes{}edges{edge value{from=inputs.value to=outputs.value}}}"#,&catalogs()).unwrap().canonical_document;
    let mut fanout = chain_join();
    fanout.edges.clear();
    fanout.inputs[0].to.push(JoinEndpoint {
        graph: JoinOperand::Right,
        port: "value".into(),
    });
    assert_eq!(
        base.join_graphs(&operand, &integer, &fanout, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_JOIN"
    );
    for kind in 0..2 {
        let mut config = base.config().clone();
        if kind == 0 {
            config.frontend.max_intermediate_bytes = 1
        } else {
            config.frontend.max_work = 1
        }
        assert_eq!(
            Compiler::new(config)
                .unwrap()
                .join_graphs(&operand, &operand, &chain_join(), &catalogs())
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_SOURCE_LIMIT"
        );
    }
    let mut fields = operand.fields().clone();
    fields.documents.clear();
    let invalid = CanonicalDocument::new(fields, &Limits::default()).unwrap();
    for (left, right, label) in [
        (&invalid, &operand, "left operand:"),
        (&operand, &invalid, "right operand:"),
    ] {
        assert!(
            base.join_graphs(left, right, &chain_join(), &catalogs())
                .unwrap_err()
                .diagnostics[0]
                .message
                .starts_with(label)
        );
    }
    let mut cyclic = chain_join();
    cyclic.inputs.clear();
    cyclic.edges.push(JoinEdge {
        id: "back".into(),
        from: JoinEndpoint {
            graph: JoinOperand::Right,
            port: "value".into(),
        },
        to: JoinEndpoint {
            graph: JoinOperand::Left,
            port: "value".into(),
        },
    });
    let error = base
        .join_graphs(&operand, &operand, &cyclic, &catalogs())
        .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "C_DEPENDENCY_CYCLE");
    assert!(error.diagnostics[0].message.starts_with("composed result:"));
}

#[test]
fn compiler_configuration_rejects_each_zero_ceiling_and_mismatched_identity() {
    let (base, document) = compiler();
    for kind in 0..11 {
        let mut config = base.config().clone();
        match kind {
            0 => config.frontend.max_source_bytes = 0,
            1 => config.frontend.max_packages = 0,
            2 => config.frontend.max_modules = 0,
            3 => config.frontend.max_depth = 0,
            4 => config.frontend.max_tokens = 0,
            5 => config.frontend.max_report_bytes = 0,
            6 => config.frontend.max_work = 0,
            7 => config.frontend.max_intermediate_bytes = 0,
            8 => config.schema_options.max_pattern_bytes = 0,
            9 => config.schema_options.max_regex_compiled_bytes = 0,
            _ => config.frontend.max_depth = 129,
        }
        assert_eq!(
            Compiler::new(config).err().unwrap().diagnostics[0].code,
            "C_POLICY",
            "case {kind}"
        );
    }
    let mut config = base.config().clone();
    config.limits.max_depth = 129;
    assert_eq!(
        Compiler::new(config).err().unwrap().diagnostics[0].code,
        "C_POLICY"
    );
    let mut config = base.config().clone();
    let p = &config.profile;
    config.profile = ExecutionProfile::new(
        p.core_digest(),
        p.regex_engine().clone(),
        p.schema_validator().clone(),
        p.uri_template_engine().clone(),
        Digest::from_bytes([9; 32]),
        &config.limits,
    )
    .unwrap();
    assert_eq!(
        Compiler::new(config).err().unwrap().diagnostics[0].code,
        "C_POLICY"
    );
    let output = base.compile_ir(document, &catalogs()).unwrap();
    assert!(format!("{output:?}").contains("CompilationSuccess"));
}

#[test]
fn lexical_and_grammar_failures_are_located_at_valid_original_utf8_boundaries() {
    let (compiler, _) = compiler();
    let empty = "graph test.empty{inputs={}outputs={}nodes{}edges{}}";
    for declaration in [
        "type lower=string",
        "type Error=string",
        "type Bad_Name=string",
        "type inputs.Value=string",
        "task inputs{}",
        "type X=string type X=integer",
        "prompt p=\"{unterminated\"",
        "prompt p=\"unescaped }\"",
        "prompt p=\"{bad-name}\"",
        "prompt p(a=string,a=string)=\"{a}\"",
        "prompt p(a=bytes)=\"{a}\"",
        "export graph test.export{inputs={}outputs={}nodes{}edges{}}",
        "predicate_library lib=predicates.library(\"id\"){version=\"1\"}",
        "predicate_library lib=predicates.library(\"id\"){digest=\"bad\"}",
        "predicate_library lib=predicates.library(\"id\"){version=\"1\" digest=\"bad\"}",
        "predicate_library lib=predicates.library(\"id\"){version=\"1\" version=\"2\" digest=\"bad\"}",
        "export predicate_library lib=predicates.library(\"id\"){version=\"1\" digest=\"bad\"}",
        "unknown declaration=string",
        "import BadAlias from \"self/lib\"",
        "prompt p=\"\\q\"",
        "prompt p=\"\\uZZZZ\"",
        "prompt p=\"\\uDC00\"",
        "prompt p=\"\\uD800x\"",
    ] {
        let source = format!("ir_version=\"0.1\" -- café 🦀\n{declaration}\n{empty}");
        let error = compiler.compile_source(&source, &catalogs()).unwrap_err();
        for diagnostic in &error.diagnostics {
            if let DiagnosticLocation::SourceSpan(s) = &diagnostic.location {
                assert!(s.start_byte <= s.end_byte && s.end_byte <= source.len());
                assert!(
                    source.is_char_boundary(s.start_byte) && source.is_char_boundary(s.end_byte)
                );
            }
        }
        assert!(!error.to_string().is_empty(), "{declaration}");
    }
    for source in [
        "",
        "ir_version=\"9\"",
        "ir_version=\"0.1\" graph",
        "ir_version=\"0.1\" @",
        "ir_version=\"0.1\" prompt p=\"line\ninvalid\"",
        "ir_version=\"0.1\" prompt p=\"\\uD800\\u0041\"",
    ] {
        assert!(
            compiler.compile_source(source, &catalogs()).is_err(),
            "{source:?}"
        );
    }
    let source = r#"ir_version="0.1"prompt p="\b\f\n\r\t\/\\\"\uD83E\uDD80" graph test.literal{inputs={}outputs={value=string}nodes{n=eval(string,render(&p,{}))}edges{edge value{from=n.outputs.value to=outputs.value}}}"#;
    compiler.compile_source(source, &catalogs()).unwrap();
}

#[test]
fn expression_operation_and_scope_error_matrix_rejects_without_partial_artifacts() {
    let (compiler, _) = compiler();
    for expression in [
        "@n",
        "&length",
        "&unknown",
        "unknown.value",
        "status(1)",
        "status(@n, @n)",
        "error()",
        "render(1,{})",
        "render(&missing,{})",
        "length()",
        "unknown.function()",
        "a.b.c()",
        "/[/",
        "1e999",
        "9223372036854775808",
        "{a=1,a=2}",
        "[1][-1]",
    ] {
        let source = format!(
            r#"ir_version="0.1"graph test.expression{{inputs={{}}outputs={{value=json}}nodes{{n=eval(json,{expression})}}edges{{edge value{{from=n.outputs.value to=outputs.value}}}}}}"#
        );
        assert!(
            compiler.compile_source(&source, &catalogs()).is_err(),
            "{expression}"
        );
    }
    for node in [
        "n=eval(integer,1){unknown=true}",
        "n=eval(integer,1){inputs={}inputs={}}",
        "n=eval(integer,1){topic=\"x\"}",
        "n=eval(integer,1){retry{max_attempts=2 on=[] backoff_ms=[0]}}",
        "n=eval(integer,1){limits{unknown=1}}",
        "n=eval(integer,1){limits{timeout_ms=-1}}",
        "n=wait(string){timeout_ms=1}",
        "n=wait(string){topic=\"\" timeout_ms=1}",
        "n=wait(string){topic=\"x\" timeout_ms=0}",
        "n=wait(string){topic=\"x\" timeout_ms=1 limits{timeout_ms=2}}",
        "n=use(missing)",
        "n=read(mcp.tool(\"service\",\"tool\"))",
        "n=call(mcp.resource(\"service\",\"uri\"))",
        "n=eval(integer,1){when=17}",
        "n=loop{inputs={}outputs={}carried={}body{nodes{}edges{}}until=true max_iterations=0}",
    ] {
        let source = format!(
            "ir_version=\"0.1\"graph test.operation{{inputs={{}}outputs={{value=integer}}nodes{{{node}}}edges{{edge value{{from=n.outputs.value to=outputs.value}}}}}}"
        );
        assert!(
            compiler.compile_source(&source, &catalogs()).is_err(),
            "{node}"
        );
    }
    for edge in [
        "from=missing.outputs.value to=outputs.value",
        "from=inputs.value to=next.value",
        "from=next.value to=outputs.value",
        "from=n.inputs.value to=outputs.value",
        "from=n.outputs.value to=inputs.value",
        "from=n.outputs.value to=n.outputs.value",
    ] {
        let source = format!(
            "ir_version=\"0.1\"graph test.edge{{inputs={{}}outputs={{value=integer}}nodes{{n=eval(integer,1)}}edges{{edge value{{{edge}}}}}}}"
        );
        assert!(
            compiler.compile_source(&source, &catalogs()).is_err(),
            "{edge}"
        );
    }
}

#[test]
fn every_utf8_truncated_source_is_rejected_and_comment_variants_are_canonical() {
    let (compiler, _) = compiler();
    let source = r#"ir_version="0.1"prompt p(name=string,)="""Hello 🦀 {name}"""graph test.truncation{inputs={name=string}outputs={value=string}nodes{n=eval(string,render(&p,{name=inputs.name})){inputs={name=string}}}edges{edge input{from=inputs.name to=n.inputs.name}edge value{from=n.outputs.value to=outputs.value}}}"#;
    let original = compiler.compile_source(source, &catalogs()).unwrap();
    for (end, _) in source.char_indices() {
        assert!(
            compiler
                .compile_source(&source[..end], &catalogs())
                .is_err(),
            "prefix ending at {end}"
        );
    }
    for comment in [
        "-- line\n",
        "--[[ block café 🦀 ]]\n",
        "--[[ -- nested-looking text ]]\n",
    ] {
        let changed = format!("{comment}{source}\n{comment}");
        assert_eq!(
            compiler
                .compile_source(&changed, &catalogs())
                .unwrap()
                .executable_bytes,
            original.executable_bytes
        );
    }
}

fn ingress_mutations(value: &cbor::Value, depth: usize) -> Vec<cbor::Value> {
    use cbor::{Map, Value};
    let mut variants = vec![
        Value::Null,
        Value::Bool(false),
        Value::Integer(-1),
        Value::Text(String::new()),
        Value::Array(vec![]),
        Value::Map(Map::new()),
    ];
    if depth == 0 {
        return variants;
    }
    match value {
        Value::Map(fields) => {
            for (key, child) in fields.iter() {
                variants.push(Value::Map(
                    Map::try_from_entries(
                        fields
                            .iter()
                            .filter(|(k, _)| *k != key)
                            .map(|(k, v)| (k.to_owned(), v.clone())),
                    )
                    .unwrap(),
                ));
                for changed in ingress_mutations(child, depth - 1) {
                    variants.push(Value::Map(
                        Map::try_from_entries(fields.iter().map(|(k, v)| {
                            (
                                k.to_owned(),
                                if k == key { changed.clone() } else { v.clone() },
                            )
                        }))
                        .unwrap(),
                    ));
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                for changed in ingress_mutations(child, depth - 1) {
                    let mut items = items.clone();
                    items[index] = changed;
                    variants.push(Value::Array(items));
                }
            }
        }
        _ => {}
    }
    variants
}

#[test]
fn catalog_ingress_mutations_are_panic_free_and_valid_unused_inventory_preserves_identity() {
    let l = Limits::default();
    let (compiler, _) = compiler();
    let empty = r#"ir_version="0.1"graph test.empty{inputs={}outputs={}nodes{}edges{}}"#;
    let valid=JsonDocument::new(br#"{
      "tools":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"service":{"deployment_id":"fixture","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{"https://example.test/common":{"type":"object"}},"tools":[{"server":"service","descriptor":{"name":"echo","description":"fixture","inputSchema":{"type":"object","properties":{"value":{"type":["string","null"]}},"required":["value"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"value":{"type":["string","null"]}},"required":["value"],"additionalProperties":false}}}]},
      "resources":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"service":{"deployment_id":"fixture","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{},"resources":[{"server":"service","descriptor":{"name":"fixed","uri":"context://fixed"}}],"templates":[{"server":"service","descriptor":{"name":"topic","uriTemplate":"context://{topic}"}}]},
      "prompts":{"catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"service":{"deployment_id":"fixture","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{},"prompts":[{"server":"service","descriptor":{"name":"review","arguments":[{"name":"subject","required":true}]}}]}
    }"#,&l).unwrap();
    let original = MCPCatalogInputs::from_json(valid.as_bytes(), &l).unwrap();
    let expected = compiler
        .compile_source(empty, &original)
        .unwrap()
        .executable_bytes;
    let mutations = ingress_mutations(valid.value(), 8);
    assert!(mutations.len() > 500);
    let mut rejected = 0;
    let mut accepted = 0;
    for value in mutations {
        let json = JsonDocument::from_value(&value, &l).unwrap();
        match MCPCatalogInputs::from_json(json.as_bytes(), &l) {
            Ok(catalogs) => {
                accepted += 1;
                assert_eq!(
                    compiler
                        .compile_source(empty, &catalogs)
                        .unwrap()
                        .executable_bytes,
                    expected
                );
            }
            Err(error) => {
                rejected += 1;
                assert!(
                    error
                        .diagnostics
                        .iter()
                        .all(|d| ["C_MCP", "C_VERSION", "C_SCHEMA"].contains(&d.code.as_str())),
                    "{error:?}"
                );
            }
        }
    }
    assert!(rejected > 100 && accepted > 0);
}

#[test]
fn source_bundle_shape_mutations_never_publish_unchecked_implementations() {
    let l = Limits::default();
    let (compiler, _) = compiler();
    let catalog = catalogs();
    let original = bundle_with_sources(&[
        (
            "main",
            r#"ir_version="0.1"import lib from "self/library"graph test.bundle{inputs={value=lib.Value}outputs={value=lib.Value}nodes{}edges{edge value{from=inputs.value to=outputs.value}}}"#,
        ),
        ("library", r#"ir_version="0.1"export type Value=string"#),
    ]);
    let pin = original.entry.package_digest;
    let package = &original.packages[&pin];
    let sources = cbor::Value::Map(
        cbor::Map::try_from_entries(
            package
                .sources
                .iter()
                .map(|(path, text)| (path.clone(), cbor::Value::Text(text.clone()))),
        )
        .unwrap(),
    );
    let sources = JsonDocument::from_value(&sources, &l).unwrap();
    let valid=JsonDocument::new(format!(r#"{{"format":"htlk_source_bundle","format_version":"0.1","entry":{{"package_digest":"{pin}","module_path":"main"}},"packages":{{"{pin}":{{"manifest":{{"manifest_version":"0.1","package_name":"test.package","package_version":"0.1","modules":{{"main":"src/main.htlk","library":"src/library.htlk"}},"dependencies":{{}}}},"sources":{}}}}}}}"#,std::str::from_utf8(sources.as_bytes()).unwrap()).as_bytes(),&l).unwrap();
    let expected = compiler
        .compile_bundle(&original, &catalog)
        .unwrap()
        .executable_bytes;
    let mut failed = 0;
    for value in ingress_mutations(valid.value(), 5) {
        let json = JsonDocument::from_value(&value, &l).unwrap();
        if let Ok(bundle) = SourceBundle::from_json(json.as_bytes(), &l) {
            match compiler.compile_bundle(&bundle, &catalog) {
                Ok(output) => assert_eq!(output.executable_bytes, expected),
                Err(_) => failed += 1,
            }
        } else {
            failed += 1
        }
    }
    assert!(failed > 100);
}

#[test]
fn module_import_aliases_roles_and_diamond_closures_are_exact() {
    let (compiler, _) = compiler();
    for imports in [
        "import x from \"self/library\" import x from \"self/library\"",
        "import x from \"unpinned/library\"",
        "import x from \"library\"",
        "import x from \"self/../library\"",
    ] {
        let main = format!(
            "ir_version=\"0.1\"{imports} graph test.empty{{inputs={{}}outputs={{}}nodes{{}}edges{{}}}}"
        );
        let bundle = bundle_with_sources(&[
            ("main", &main),
            ("library", "ir_version=\"0.1\"export type Value=string"),
        ]);
        assert!(compiler.compile_bundle(&bundle, &catalogs()).is_err());
    }
    for main in [
        "ir_version=\"0.1\"",
        "ir_version=\"0.1\"graph one{inputs={}outputs={}nodes{}edges{}}graph two{inputs={}outputs={}nodes{}edges{}}",
    ] {
        assert!(
            compiler
                .compile_bundle(&bundle_with_sources(&[("main", main)]), &catalogs())
                .is_err()
        );
        assert!(compiler.compile_source(main, &catalogs()).is_err());
    }
    let main = r#"ir_version="0.1"import a from "self/left"import b from "self/right"graph test.diamond{inputs={value=a.Value}outputs={value=b.Value}nodes{}edges{edge value{from=inputs.value to=outputs.value}}}"#;
    let bundle = bundle_with_sources(&[
        ("main", main),
        (
            "left",
            r#"ir_version="0.1"import lib from "self/library"export type Value=lib.Value"#,
        ),
        (
            "right",
            r#"ir_version="0.1"import lib from "self/library"export type Value=lib.Value"#,
        ),
        ("library", r#"ir_version="0.1"export type Value=string"#),
    ]);
    compiler.compile_bundle(&bundle, &catalogs()).unwrap();
    assert_eq!(compiler.compile_source(r#"ir_version="0.1"import lib from "self/library"graph test.empty{inputs={}outputs={}nodes{}edges{}}"#,&catalogs()).unwrap_err().diagnostics[0].code,"C_IMPORT");
}

#[test]
fn task_cycles_mcp_pins_and_retry_numeric_bounds_are_checked_during_lowering() {
    let (compiler, _) = compiler();
    let cyclic = r#"ir_version="0.1"task a{inputs={}outputs={value=integer}nodes{b=use(b)}edges{edge value{from=b.outputs.value to=outputs.value}}}task b{inputs={}outputs={value=integer}nodes{a=use(a)}edges{edge value{from=a.outputs.value to=outputs.value}}}graph test.cycle{inputs={}outputs={value=integer}nodes{a=use(a)}edges{edge value{from=a.outputs.value to=outputs.value}}}"#;
    assert_eq!(
        compiler
            .compile_source(cyclic, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_DEPENDENCY_CYCLE"
    );
    let inventory = operation_catalogs(
        r#"[{"server":"service","descriptor":{"name":"tool","inputSchema":{"type":"object"},"outputSchema":{"type":"object"}}}]"#,
        "[]",
        "[]",
        "[]",
        "{}",
    );
    let source = r#"ir_version="0.1"graph test.retry{inputs={}outputs={value=json}nodes{args=eval(json,{})n=call(mcp.tool("service","tool")){OPTIONS}}edges{edge args{from=args.outputs.value to=n.inputs.arguments}edge value{from=n.outputs.value to=outputs.value}}}"#;
    for options in [
        format!("pin=\"{}\"", Digest::from_bytes([0; 32])),
        "pin=\"bad\"".into(),
        "retry{max_attempts=-1 on=[]backoff_ms=[]}".into(),
        "retry{max_attempts=2 on=[\"x\"]backoff_ms=[-1]}".into(),
        "retry{max_attempts=0 on=[]backoff_ms=[]}".into(),
        "retry{max_attempts=3 on=[]backoff_ms=[0]}".into(),
        "retry{max_attempts=2 on=[]backoff_ms=[]}".into(),
    ] {
        assert!(
            compiler
                .compile_source(&source.replace("OPTIONS", &options), &inventory)
                .is_err(),
            "{options}"
        );
    }
    for changed in [
        source.replace("service", "missing"),
        source.replace("\"tool\"", "\"missing\""),
    ] {
        assert_eq!(
            compiler
                .compile_source(&changed.replace("OPTIONS", ""), &inventory)
                .unwrap_err()
                .diagnostics[0]
                .code,
            "C_MCP"
        );
    }
    let valid = source.replace("OPTIONS", "");
    let output = compiler.compile_source(&valid, &inventory).unwrap();
    let binding = output
        .canonical_document
        .fields()
        .bindings
        .values()
        .next()
        .unwrap();
    assert_eq!(
        compiler
            .compile_source(
                &source.replace("OPTIONS", &format!("pin=\"{}\"", binding.descriptor())),
                &inventory
            )
            .unwrap()
            .executable_bytes,
        output.executable_bytes
    );
    let loop_source = r#"ir_version="0.1"graph test.loop{inputs={}outputs={}nodes{n=loop{inputs={}outputs={}carried={x=inputs.missing}body{nodes{}edges{}}until=true max_iterations=1}}edges{}postconditions=status(@n)=="succeeded"}"#;
    assert_eq!(
        compiler
            .compile_source(loop_source, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_LOOP"
    );
    assert!(
        compiler
            .compile_source(
                &loop_source
                    .replace("carried={x=inputs.missing}", "carried={}")
                    .replace("max_iterations=1", "max_iterations=-1"),
                &catalogs()
            )
            .is_err()
    );
}

#[test]
fn canonical_ingress_rechecks_serialization_catalog_and_diagnostic_allowances() {
    let (base, document) = compiler();
    let mut config = base.config().clone();
    config.limits.max_document_bytes = 1024;
    let bounded = Compiler::new(config).unwrap();
    let mut fields = document.fields().clone();
    fields.graph_id = "long".repeat(2000);
    let oversized = CanonicalDocument::new(fields, &Limits::default()).unwrap();
    assert_eq!(
        bounded
            .compile_document(oversized, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SERIALIZATION"
    );
    let source = r#"ir_version="0.1"graph test.overlap{inputs={flag=boolean}outputs={value=boolean}nodes{}edges{edge a{from=inputs.flag to=outputs.value when=inputs.flag}edge b{from=inputs.flag to=outputs.value when=inputs.flag}}}"#;
    let output = base.compile_source(source, &catalogs()).unwrap();
    let mut config = base.config().clone();
    config.frontend.max_report_bytes = 1;
    assert_eq!(
        Compiler::new(config)
            .unwrap()
            .compile_document(output.canonical_document, &catalogs())
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_LIMIT"
    );
    let schema = r#"{"https://example.test/large":{"description":"TEXT","type":"object"}}"#
        .replace("TEXT", &"x".repeat(2000));
    let large = operation_catalogs("[]", "[]", "[]", "[]", &schema);
    assert_eq!(
        bounded
            .compile_document(document, &large)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "C_SOURCE_LIMIT"
    );
}

#[test]
fn compiler_linked_library_keys_and_unbound_generic_signatures_are_validated() {
    let (base, _) = compiler();
    let l = Limits::default();
    let digest = Digest::from_bytes([44; 32]);
    let signature = FunctionSignature::new(
        vec![],
        vec![],
        Port::new(ValueType::builtin(BuiltinType::String), true),
        &l,
    )
    .unwrap();
    let library = Library::new(
        "test.library".into(),
        "1".into(),
        digest,
        vec![("get".parse().unwrap(), signature)],
        &l,
    )
    .unwrap();
    let mut config = base.config().clone();
    config
        .libraries
        .insert(Digest::from_bytes([45; 32]), library);
    assert_eq!(
        Compiler::new(config).err().unwrap().diagnostics[0].code,
        "C_LIBRARY"
    );
    let signature = FunctionSignature::new(
        vec![],
        vec![],
        Port::new(
            ValueType::new(
                ValueTypeKind::Var("unbound".parse().unwrap()),
                TypeContext::Signature,
                &l,
            )
            .unwrap(),
            true,
        ),
        &l,
    )
    .unwrap();
    let library = Library::new(
        "test.library".into(),
        "1".into(),
        digest,
        vec![("get".parse().unwrap(), signature)],
        &l,
    )
    .unwrap();
    let mut config = base.config().clone();
    config.libraries.insert(digest, library);
    assert_eq!(
        Compiler::new(config).err().unwrap().diagnostics[0].code,
        "C_LIBRARY"
    );
}

#[test]
fn diagnostic_location_order_is_total_and_multi_error_display_retains_each_code() {
    use std::error::Error as _;
    let locations = [
        DiagnosticLocation::CanonicalRecordPath("/a".into()),
        DiagnosticLocation::SourceBundlePath("/a".into()),
        DiagnosticLocation::SourceSpan(SourceSpan {
            source: SourceLocation::Inline,
            start_byte: 0,
            end_byte: 1,
        }),
        DiagnosticLocation::SourceSpan(SourceSpan {
            source: SourceLocation::Module {
                package_digest: Digest::from_bytes([0; 32]),
                module_path: "m".into(),
                source_path: "m.htlk".into(),
            },
            start_byte: 0,
            end_byte: 1,
        }),
    ];
    for (i, a) in locations.iter().enumerate() {
        for (j, b) in locations.iter().enumerate() {
            assert_eq!(a.cmp(b), i.cmp(&j));
            assert_eq!(a.partial_cmp(b), Some(a.cmp(b)));
        }
    }
    let error = CompilationFailure {
        diagnostics: locations
            .into_iter()
            .enumerate()
            .map(|(index, location)| Diagnostic {
                code: format!("C_TEST_{index}"),
                severity: analyzer::DiagnosticSeverity::Error,
                message: "redacted explanation".into(),
                location,
                related_locations: vec![],
            })
            .collect(),
    };
    assert_eq!(error.to_string().lines().count(), 4);
    assert!(error.to_string().contains("C_TEST_3"));
    assert!(error.source().is_none());
}
