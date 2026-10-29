//! Public whole-document semantic admission without a runtime dependency.
use htlk_analyzer::{NativeSchemaOptions, analyze_document};
use htlk_executable::*;
use htlk_executable::{cbor::Limits, digest::Digest};

fn document(scope: ScopeFields) -> CanonicalDocument {
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
                max_regex_bytes: 4,
                max_regex_compiled_bytes: 4096,
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
    let engine = EngineIdentity::new("test".into(), "1".into(), "1".into(), id, &l).unwrap();
    let profile = ExecutionProfile::new(
        id,
        engine.clone(),
        engine.clone(),
        engine,
        policy.digest(),
        &l,
    )
    .unwrap();
    let scope = Scope::new(scope, &l).unwrap();
    let mut f = DocumentFields::new("test.analysis".into(), profile, scope.digest(&l).unwrap());
    f.scopes.insert(f.root_scope, scope);
    f.documents
        .insert(policy.digest(), policy.document().clone());
    CanonicalDocument::new(f, &l).unwrap()
}

fn ports(name: &str, builtin: BuiltinType) -> PortTable {
    PortTable::new(
        vec![(
            name.parse().unwrap(),
            Port::new(ValueType::builtin(builtin), true),
        )],
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn whole_document_regex_admission_uses_the_declared_policy() {
    use htlk_analyzer::{
        DocumentAnalysisError, ExpressionSite, ExpressionTypeError, RegexError, ScopeUse,
        ScopeVerificationErrorKind,
    };
    let limits = Limits::default();
    let mut n = NodeFields::new(
        "pattern".parse().unwrap(),
        Operation::Eval(
            Expression::new(
                ExpressionKind::Regex {
                    pattern: "abcde".into(),
                    flags: "".into(),
                },
                &limits,
            )
            .unwrap(),
        ),
    );
    n.outputs = ports("value", BuiltinType::Regex);
    let doc = document(ScopeFields {
        outputs: ports("result", BuiltinType::Regex),
        nodes: vec![Node::new(n, &limits).unwrap()],
        edges: vec![Edge::new(
            "publish".parse().unwrap(),
            EdgeSource::Output {
                node: "pattern".parse().unwrap(),
                port: "value".parse().unwrap(),
            },
            EdgeDestination::Output("result".parse().unwrap()),
        )],
        ..Default::default()
    });
    let error = analyze_document(doc, NativeSchemaOptions::default(), &limits)
        .err()
        .unwrap();
    let DocumentAnalysisError::Graph(error) = error else {
        panic!("expected graph failure")
    };
    assert_eq!(error.scope_use, Some(ScopeUse::Root));
    assert_eq!(
        error.cause.site,
        Some(ExpressionSite::Eval("pattern".parse().unwrap()))
    );
    assert_eq!(
        error.cause.kind,
        ScopeVerificationErrorKind::Expression(Box::new(ExpressionTypeError::Regex(
            RegexError::PatternLimit
        )))
    );
}

fn conditional_scope(complement: bool) -> ScopeFields {
    let l = Limits::default();
    let condition = Expression::new(
        ExpressionKind::Ref {
            source: ValueReference::Input("flag".parse().unwrap()),
            path: vec![],
        },
        &l,
    )
    .unwrap();
    let second = if complement {
        Expression::new(ExpressionKind::Not(Box::new(condition.clone())), &l).unwrap()
    } else {
        condition.clone()
    };
    ScopeFields {
        inputs: ports("flag", BuiltinType::Boolean),
        outputs: ports("result", BuiltinType::Boolean),
        edges: vec![
            Edge::new(
                "a".parse().unwrap(),
                EdgeSource::Input("flag".parse().unwrap()),
                EdgeDestination::Output("result".parse().unwrap()),
            )
            .with_guard(condition),
            Edge::new(
                "b".parse().unwrap(),
                EdgeSource::Input("flag".parse().unwrap()),
                EdgeDestination::Output("result".parse().unwrap()),
            )
            .with_guard(second),
        ],
        ..Default::default()
    }
}

#[test]
fn warning_reports_are_deterministic_located_and_keep_runtime_uniqueness() {
    use htlk_analyzer::{DiagnosticSeverity, ScopeUse};
    let l = Limits::default();
    let source = document(conditional_scope(false));
    let canonical = source.to_value(&l).unwrap();
    let fingerprint = source.envelope(&l).unwrap().fingerprint();
    let analysis = analyze_document(source.clone(), NativeSchemaOptions::default(), &l).unwrap();
    assert_eq!(analysis.document(), &source);
    assert!(std::ptr::eq(
        analysis.document(),
        analysis.graphs().document()
    ));
    assert_eq!(analysis.fingerprint(), fingerprint);
    let reports = analysis.diagnostics();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].code, "W_CONDITIONAL_WRITERS");
    assert_eq!(reports[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(reports[0].location.scope_use, Some(ScopeUse::Root));
    assert_eq!(reports[0].related_locations.len(), 1);
    for location in std::iter::once(&reports[0].location).chain(&reports[0].related_locations) {
        location.pointer.resolve_value(&canonical).unwrap();
    }
    let mut reordered = conditional_scope(false);
    reordered.edges.reverse();
    let repeated =
        analyze_document(document(reordered), NativeSchemaOptions::default(), &l).unwrap();
    assert_eq!(reports, repeated.diagnostics());
    let exclusive = analyze_document(
        document(conditional_scope(true)),
        NativeSchemaOptions::default(),
        &l,
    )
    .unwrap();
    assert!(exclusive.diagnostics().is_empty());
    assert!(
        exclusive.graphs().plan(&ScopeUse::Root).unwrap().bindings()[0].needs_uniqueness_check()
    );
}

#[test]
fn distinct_literal_equality_guards_do_not_warn_but_overlapping_groups_do() {
    let l = Limits::default();
    let mut fields = conditional_scope(false);
    fields.inputs = ports("flag", BuiltinType::String);
    fields.outputs = ports("result", BuiltinType::String);
    let selector = Expression::new(
        ExpressionKind::Ref {
            source: ValueReference::Input("flag".parse().unwrap()),
            path: vec![],
        },
        &l,
    )
    .unwrap();
    let equality = |value: &str| {
        Expression::new(
            ExpressionKind::Binary {
                operator: BinaryOperator::Eq,
                left: Box::new(selector.clone()),
                right: Box::new(Expression::literal(ScalarLiteral::String(value.into()))),
            },
            &l,
        )
        .unwrap()
    };
    fields.edges[0] = fields.edges[0].clone().with_guard(equality("a"));
    fields.edges[1] = fields.edges[1].clone().with_guard(equality("b"));
    assert!(
        analyze_document(document(fields.clone()), NativeSchemaOptions::default(), &l)
            .unwrap()
            .diagnostics()
            .is_empty()
    );
    fields.edges.push(
        Edge::new(
            "c".parse().unwrap(),
            EdgeSource::Input("flag".parse().unwrap()),
            EdgeDestination::Output("result".parse().unwrap()),
        )
        .with_guard(equality("b")),
    );
    let a = analyze_document(document(fields), NativeSchemaOptions::default(), &l).unwrap();
    assert_eq!(a.diagnostics()[0].related_locations.len(), 2);
}

#[test]
fn early_nested_linkage_errors_keep_the_originating_expression_pointer() {
    use htlk_analyzer::{DiagnosticSeverity, DocumentAnalysisError, LinkageError};
    let l = Limits::default();
    let call = Expression::new(
        ExpressionKind::Call {
            function: FunctionId::Library {
                library: Digest::from_bytes([9; 32]),
                name: "missing".parse().unwrap(),
            },
            arguments: vec![],
        },
        &l,
    )
    .unwrap();
    let expression =
        Expression::new(ExpressionKind::Record(vec![("a/b~c".into(), call)]), &l).unwrap();
    let mut node = NodeFields::new("worker".parse().unwrap(), Operation::Eval(expression));
    node.outputs = ports("value", BuiltinType::Json);
    let source = document(ScopeFields {
        nodes: vec![Node::new(node, &l).unwrap()],
        ..Default::default()
    });
    let failure = analyze_document(source.clone(), NativeSchemaOptions::default(), &l)
        .err()
        .unwrap();
    let DocumentAnalysisError::LocatedLinkage(ref cause) = failure else {
        panic!()
    };
    assert_eq!(cause.error, LinkageError::MissingRecord("library"));
    let report = failure.diagnostic(&source, &l).unwrap();
    assert_eq!(report.code, "C_LIBRARY");
    assert_eq!(report.severity, DiagnosticSeverity::Error);
    assert!(
        report
            .location
            .pointer
            .to_string()
            .ends_with("/nodes/0/operation/1/1/0/1")
    );
    report
        .location
        .pointer
        .resolve_value(&source.to_value(&l).unwrap())
        .unwrap();
}

#[test]
fn interface_failures_retain_the_invocation_and_definition_locations() {
    use htlk_analyzer::{DocumentAnalysisError, LinkageError};
    let l = Limits::default();
    let child = Scope::new(
        ScopeFields {
            inputs: ports("arg", BuiltinType::String),
            ..Default::default()
        },
        &l,
    )
    .unwrap();
    let id = child.digest(&l).unwrap();
    let wrapper = Node::new(
        NodeFields::new("task".parse().unwrap(), Operation::Scope(id)),
        &l,
    )
    .unwrap();
    let mut fields = document(ScopeFields {
        nodes: vec![wrapper],
        ..Default::default()
    })
    .fields()
    .clone();
    fields.scopes.insert(id, child);
    let source = CanonicalDocument::new(fields, &l).unwrap();
    let failure = analyze_document(source.clone(), NativeSchemaOptions::default(), &l)
        .err()
        .unwrap();
    let DocumentAnalysisError::LocatedLinkage(ref cause) = failure else {
        panic!()
    };
    assert_eq!(cause.error, LinkageError::ScopeInterfaceMismatch);
    let report = failure.diagnostic(&source, &l).unwrap();
    let canonical = source.to_value(&l).unwrap();
    report.location.pointer.resolve_value(&canonical).unwrap();
    assert_eq!(report.related_locations.len(), 1);
    assert_eq!(
        report.related_locations[0].pointer.to_string(),
        format!("/scopes/{id}")
    );
    report.related_locations[0]
        .pointer
        .resolve_value(&canonical)
        .unwrap();
}

#[test]
fn graph_diagnostics_convert_child_paths_to_resolvable_canonical_pointers() {
    let l = Limits::default();
    let invalid = Expression::new(
        ExpressionKind::Not(Box::new(Expression::literal(ScalarLiteral::String(
            "PRIVATE_VALUE".into(),
        )))),
        &l,
    )
    .unwrap();
    let source = document(ScopeFields {
        preconditions: invalid,
        ..Default::default()
    });
    let failure = analyze_document(source.clone(), NativeSchemaOptions::default(), &l)
        .err()
        .unwrap();
    let report = failure.diagnostic(&source, &l).unwrap();
    assert_eq!(report.code, "C_TYPE");
    assert!(
        report
            .location
            .pointer
            .to_string()
            .ends_with("/preconditions/1")
    );
    report
        .location
        .pointer
        .resolve_value(&source.to_value(&l).unwrap())
        .unwrap();
    assert!(!format!("{failure:?} {report:?}").contains("PRIVATE_VALUE"));
}

#[test]
fn loop_until_diagnostics_point_to_the_owner_operation_not_the_body() {
    let l = Limits::default();
    let body = Scope::new(ScopeFields::default(), &l).unwrap();
    let body_id = body.digest(&l).unwrap();
    let node = Node::new(
        NodeFields::new(
            "repeat".parse().unwrap(),
            Operation::Loop {
                body: body_id,
                initializers: vec![],
                max_iterations: 1,
                until: Expression::new(
                    ExpressionKind::Not(Box::new(Expression::literal(ScalarLiteral::Integer(1)))),
                    &l,
                )
                .unwrap(),
            },
        ),
        &l,
    )
    .unwrap();
    let mut fields = document(ScopeFields {
        nodes: vec![node],
        postconditions: Expression::new(
            ExpressionKind::Binary {
                operator: BinaryOperator::Eq,
                left: Box::new(
                    Expression::new(ExpressionKind::Status("repeat".parse().unwrap()), &l).unwrap(),
                ),
                right: Box::new(Expression::literal(ScalarLiteral::String(
                    "succeeded".into(),
                ))),
            },
            &l,
        )
        .unwrap(),
        ..Default::default()
    })
    .fields()
    .clone();
    let owner = fields.root_scope;
    fields.scopes.insert(body_id, body);
    let source = CanonicalDocument::new(fields, &l).unwrap();
    let failure = analyze_document(source.clone(), NativeSchemaOptions::default(), &l)
        .err()
        .unwrap();
    let report = failure.diagnostic(&source, &l).unwrap();
    assert_eq!(
        report.location.pointer.to_string(),
        format!("/scopes/{owner}/nodes/0/operation/3/1")
    );
    report
        .location
        .pointer
        .resolve_value(&source.to_value(&l).unwrap())
        .unwrap();
}

#[test]
fn unconditional_conflicts_locate_the_destination_and_related_writers() {
    let l = Limits::default();
    let mut scope = conditional_scope(false);
    for edge in &mut scope.edges {
        *edge = edge
            .clone()
            .with_guard(Expression::literal(ScalarLiteral::Boolean(true)));
    }
    let source = document(scope);
    let failure = analyze_document(source.clone(), NativeSchemaOptions::default(), &l)
        .err()
        .unwrap();
    let report = failure.diagnostic(&source, &l).unwrap();
    assert_eq!(report.code, "C_BINDING");
    assert!(
        report
            .location
            .pointer
            .to_string()
            .ends_with("/outputs/result")
    );
    assert_eq!(report.related_locations.len(), 2);
    let canonical = source.to_value(&l).unwrap();
    for location in std::iter::once(&report.location).chain(&report.related_locations) {
        location.pointer.resolve_value(&canonical).unwrap();
    }
}

fn tool_document(input: &str) -> CanonicalDocument {
    use htlk_analyzer::embedded_schema_base;
    let l = Limits::default();
    let input = JsonDocument::new(input.as_bytes(), &l).unwrap();
    let output = JsonDocument::new(br#"{"type":"object"}"#, &l).unwrap();
    let descriptor = JsonDocument::new(
        format!(
            r#"{{"name":"tool","inputSchema":{},"outputSchema":{{"type":"object"}}}}"#,
            std::str::from_utf8(input.as_bytes()).unwrap()
        )
        .as_bytes(),
        &l,
    )
    .unwrap();
    let server = ServerIdentity::new(
        "test".into(),
        McpTransport::Stdio,
        "test".into(),
        "1".into(),
        &l,
    )
    .unwrap();
    let binding = McpBinding::new(
        server,
        descriptor.digest(),
        McpBindingKind::Tool {
            name: "tool".into(),
            input_schema: input.digest(),
            output_schema: Some(output.digest()),
            result_policy: Box::default(),
        },
        &l,
    )
    .unwrap();
    let id = binding.digest(&l).unwrap();
    let ty =
        |digest| ValueType::new(ValueTypeKind::Schema(digest), TypeContext::Value, &l).unwrap();
    let mut node = NodeFields::new(
        "tool".parse().unwrap(),
        Operation::Mcp {
            binding: id,
            retry: RetryPolicy::no_retry(),
        },
    );
    node.inputs = PortTable::new(
        vec![(
            "arguments".parse().unwrap(),
            Port::new(ty(input.digest()), true),
        )],
        &l,
    )
    .unwrap();
    node.outputs = PortTable::new(
        vec![(
            "value".parse().unwrap(),
            Port::new(ValueType::builtin(BuiltinType::McpToolResult), true),
        )],
        &l,
    )
    .unwrap();
    let root = ScopeFields {
        inputs: node.inputs.clone(),
        outputs: node.outputs.clone(),
        nodes: vec![Node::new(node, &l).unwrap()],
        edges: vec![
            Edge::new(
                "input".parse().unwrap(),
                EdgeSource::Input("arguments".parse().unwrap()),
                EdgeDestination::Input {
                    node: "tool".parse().unwrap(),
                    port: "arguments".parse().unwrap(),
                },
            ),
            Edge::new(
                "output".parse().unwrap(),
                EdgeSource::Output {
                    node: "tool".parse().unwrap(),
                    port: "value".parse().unwrap(),
                },
                EdgeDestination::Output("value".parse().unwrap()),
            ),
        ],
        ..Default::default()
    };
    let mut f = document(root).fields().clone();
    f.bindings.insert(id, binding);
    f.documents.insert(descriptor.digest(), descriptor);
    for schema in [input, output] {
        f.schema_uris
            .insert(embedded_schema_base(&schema, &l).unwrap(), schema.digest());
        f.documents.insert(schema.digest(), schema);
    }
    CanonicalDocument::new(f, &l).unwrap()
}

#[test]
fn composed_schema_errors_retain_external_document_and_inner_schema_locations() {
    use htlk_analyzer::{DocumentAnalysisError, LinkageError};
    let l = Limits::default();
    let valid = tool_document(r#"{"type":"object"}"#);
    analyze_document(valid, NativeSchemaOptions::default(), &l).unwrap();
    let invalid = tool_document(
        r#"{"type":"object","properties":{"value":{"type":"string","pattern":"(?=PRIVATE_PATTERN)"}}}"#,
    );
    let error = analyze_document(invalid.clone(), NativeSchemaOptions::default(), &l)
        .err()
        .unwrap();
    let DocumentAnalysisError::LocatedLinkage(cause) = &error else {
        panic!()
    };
    let LinkageError::SchemaDiagnostic(cause) = &cause.error else {
        panic!()
    };
    assert_eq!(
        cause.location.as_ref().unwrap().pointer.to_string(),
        "/properties/value"
    );
    let report = error.diagnostic(&invalid, &l).unwrap();
    assert_eq!(report.code, "C_SCHEMA");
    assert!(matches!(
        report
            .location
            .pointer
            .resolve_value(&invalid.to_value(&l).unwrap())
            .unwrap(),
        cbor::Value::Bytes(_)
    ));
    assert!(!format!("{error:?}").contains("PRIVATE_PATTERN"));
}

#[test]
fn missing_nested_scope_reports_the_exact_reference_that_failed() {
    let l = Limits::default();
    let missing = Digest::from_bytes([8; 32]);
    let node = Node::new(
        NodeFields::new("task".parse().unwrap(), Operation::Scope(missing)),
        &l,
    )
    .unwrap();
    let source = document(ScopeFields {
        nodes: vec![node],
        ..Default::default()
    });
    let error = analyze_document(source.clone(), NativeSchemaOptions::default(), &l)
        .err()
        .unwrap();
    let report = error.diagnostic(&source, &l).unwrap();
    assert!(
        report
            .location
            .pointer
            .to_string()
            .ends_with("/nodes/0/operation/1")
    );
    assert_eq!(
        report
            .location
            .pointer
            .resolve_value(&source.to_value(&l).unwrap())
            .unwrap(),
        &cbor::Value::Text(missing.to_string())
    );
}
