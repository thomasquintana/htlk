//! Name/context errors remain representable and are rejected by semantic analysis.
use ExpressionContext as C;
use ExpressionKind as K;
use ValueReference as R;
use htlk_analyzer::*;
use htlk_executable::{cbor::Limits, digest::Digest, *};
fn make(kind: K) -> Expression {
    Expression::new(kind, &Limits::default()).unwrap()
}
fn truth() -> Expression {
    Expression::literal(ScalarLiteral::Boolean(true))
}
fn ports(name: &str) -> PortTable {
    PortTable::new(
        vec![(
            name.parse().unwrap(),
            Port::new(ValueType::builtin(BuiltinType::String), true),
        )],
        &Limits::default(),
    )
    .unwrap()
}
fn verify(
    fields: ScopeFields,
    role: ScopeContext,
) -> Result<ScopeGraphPlan, ScopeVerificationError> {
    let l = Limits::default();
    let scope = Scope::new(fields, &l).unwrap();
    verify_definition(&scope, role)
}
fn verify_definition(
    scope: &Scope,
    role: ScopeContext,
) -> Result<ScopeGraphPlan, ScopeVerificationError> {
    let l = Limits::default();
    let schemas = NativeSchemas::compile(
        &SchemaCatalog::new(vec![], &l).unwrap(),
        NativeSchemaOptions::default(),
        &l,
    )
    .unwrap();
    let until = truth();
    verify_scope_graph(
        scope,
        role,
        (role == ScopeContext::LoopBody).then_some(&until),
        &Default::default(),
        &Default::default(),
        &schemas,
        &l,
    )
}
#[test]
fn context_matrix_is_enforced_through_nested_expressions() {
    let l = Limits::default();
    let contexts = [
        C::Eval,
        C::Preconditions,
        C::Guard { loop_body: false },
        C::Guard { loop_body: true },
        C::PrimitivePostconditions,
        C::ScopePostconditions,
        C::WrapperPostconditions,
        C::LoopUntil,
        C::LoopPostconditions,
    ];
    let sources = [
        R::Input("x".parse().unwrap()),
        R::Output {
            node: "worker".parse().unwrap(),
            port: "value".parse().unwrap(),
        },
        R::ScopeOutput("value".parse().unwrap()),
        R::Carried("x".parse().unwrap()),
        R::Next("x".parse().unwrap()),
    ];
    let allowed = [
        [true, true, true, true, true, true, true, true, true],
        [false, false, true, true, false, false, false, true, false],
        [false, false, false, false, true, true, true, true, true],
        [false, false, false, true, false, false, false, true, false],
        [false, false, false, false, false, false, false, true, false],
    ];
    let mut env = ExpressionTypeEnvironment::default();
    for source in &sources {
        env.references.insert(
            source.clone(),
            Port::new(ValueType::builtin(BuiltinType::String), true),
        );
    }
    env.outcomes.insert("worker".parse().unwrap());
    for (source, row) in sources.into_iter().zip(allowed) {
        let nested = make(K::List(vec![make(K::Ref {
            source,
            path: vec![],
        })]));
        let encoded = nested.encode(&l).unwrap();
        for (context, expected) in contexts.into_iter().zip(row) {
            let decoded = Expression::decode(&encoded, &l).unwrap();
            let result = check_expression_diagnostic(&decoded, context, &env, None, None, &l);
            assert_eq!(result.is_ok(), expected);
            if let Err(error) = result {
                assert_eq!(error.expression_path, [0]);
                assert!(matches!(
                    error.error,
                    ExpressionTypeError::Expression(ExpressionError::ForbiddenReference(_))
                ));
            }
        }
    }
    for kind in [
        K::Status("worker".parse().unwrap()),
        K::Error("worker".parse().unwrap()),
    ] {
        let nested = make(K::List(vec![make(kind)]));
        for context in contexts {
            let expected = matches!(
                context,
                C::Guard { .. } | C::ScopePostconditions | C::LoopUntil
            );
            assert_eq!(
                check_expression(&nested, context, &env, None, &l).is_ok(),
                expected
            );
        }
    }
}
#[test]
fn semantic_diagnostics_explain_permissions_and_slot_coverage_without_names() {
    use std::error::Error as _;
    let limits = Limits::default();
    let expression = make(K::List(vec![make(K::Ref {
        source: R::Next("secret_next".parse().unwrap()),
        path: vec![],
    })]));
    let bytes = expression.encode(&limits).unwrap();
    let expression = Expression::decode(&bytes, &limits).unwrap();
    let error = check_expression_context(&expression, C::Eval, &limits).unwrap_err();
    assert_eq!(error.expression_path, [0]);
    let ExpressionTypeError::Expression(cause) = &error.error else {
        panic!()
    };
    assert_eq!(
        cause.to_string(),
        "Proposed next-value references are not permitted at this expression use site."
    );
    assert!(
        error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .is::<ExpressionError>()
    );
    assert!(!format!("{error} {error:?}").contains("secret_next"));
    assert!(check_expression_context(&expression, C::LoopUntil, &limits).is_ok());

    for (parameters, parts) in [
        (
            vec![],
            vec![TemplatePart::Slot("secret_slot".parse().unwrap())],
        ),
        (
            vec![(
                "secret_parameter".parse().unwrap(),
                Port::new(ValueType::builtin(BuiltinType::Float), true),
            )],
            vec![],
        ),
    ] {
        let template = PromptTemplate::new(parameters, parts, &limits).unwrap();
        let template = PromptTemplate::decode(&template.encode(&limits).unwrap(), &limits).unwrap();
        let error = check_prompt_template(&template, &limits).unwrap_err();
        assert_eq!(error, ExpressionError::TemplateParameterMismatch);
        assert_eq!(
            error.to_string(),
            "Template parameter declarations must exactly cover the distinct substitution slots."
        );
        assert!(!format!("{error} {error:?}").contains("secret"));
    }
}
#[test]
fn local_endpoint_membership_is_checked_even_for_false_guards() {
    let good = ScopeFields {
        inputs: ports("q"),
        outputs: ports("q"),
        edges: vec![Edge::new(
            "pass".parse().unwrap(),
            EdgeSource::Input("q".parse().unwrap()),
            EdgeDestination::Output("q".parse().unwrap()),
        )],
        ..ScopeFields::default()
    };
    assert!(verify(good, ScopeContext::Ordinary).is_ok());
    let bad = ScopeFields {
        outputs: ports("q"),
        edges: vec![
            Edge::new(
                "bad".parse().unwrap(),
                EdgeSource::Output {
                    node: "missing".parse().unwrap(),
                    port: "value".parse().unwrap(),
                },
                EdgeDestination::Output("q".parse().unwrap()),
            )
            .with_guard(Expression::literal(ScalarLiteral::Boolean(false))),
        ],
        ..ScopeFields::default()
    };
    assert!(matches!(
        verify(bad, ScopeContext::Ordinary).unwrap_err().kind,
        ScopeVerificationErrorKind::Graph(GraphRecordError::UnknownEndpoint("source node"))
    ));
    let mut node = NodeFields::new(
        "worker".parse().unwrap(),
        Operation::Eval(Expression::literal(ScalarLiteral::String("x".into()))),
    );
    node.outputs = ports("value");
    let bad = ScopeFields {
        inputs: ports("q"),
        nodes: vec![Node::new(node, &Limits::default()).unwrap()],
        edges: vec![Edge::new(
            "feed".parse().unwrap(),
            EdgeSource::Input("q".parse().unwrap()),
            EdgeDestination::Input {
                node: "worker".parse().unwrap(),
                port: "missing".parse().unwrap(),
            },
        )],
        ..ScopeFields::default()
    };
    assert!(matches!(
        verify(bad, ScopeContext::Ordinary).unwrap_err().kind,
        ScopeVerificationErrorKind::Graph(GraphRecordError::UnknownEndpoint("destination input"))
    ));
}
#[test]
fn loop_role_contracts_and_initializer_membership_are_semantic() {
    let body = ScopeFields {
        carried: ports("x"),
        edges: vec![Edge::new(
            "advance".parse().unwrap(),
            EdgeSource::Carried("x".parse().unwrap()),
            EdgeDestination::Next("x".parse().unwrap()),
        )],
        ..ScopeFields::default()
    };
    assert!(verify(body.clone(), ScopeContext::LoopBody).is_ok());
    assert!(matches!(
        verify(body.clone(), ScopeContext::Ordinary)
            .unwrap_err()
            .kind,
        ScopeVerificationErrorKind::Graph(GraphRecordError::InvalidScopeRole)
    ));
    let mut bad = body.clone();
    bad.limits = ExecutionLimits::new().with_timeout_ms(1).unwrap();
    assert!(matches!(
        verify(bad, ScopeContext::LoopBody).unwrap_err().kind,
        ScopeVerificationErrorKind::Graph(GraphRecordError::InvalidScopeRole)
    ));
    for post in [false, true] {
        let mut bad = body.clone();
        let contract = make(K::Not(Box::new(Expression::literal(
            ScalarLiteral::Boolean(false),
        ))));
        if post {
            bad.postconditions = contract;
        } else {
            bad.preconditions = contract;
        }
        assert!(matches!(
            verify(bad, ScopeContext::LoopBody).unwrap_err().kind,
            ScopeVerificationErrorKind::Graph(GraphRecordError::InvalidScopeRole)
        ));
    }
    let node = NodeFields::new(
        "loop_node".parse().unwrap(),
        Operation::Loop {
            body: Digest::from_bytes([0; 32]),
            initializers: vec![("x".parse().unwrap(), "seed".parse().unwrap())],
            until: truth(),
            max_iterations: 1,
        },
    );
    let bad = ScopeFields {
        nodes: vec![Node::new(node, &Limits::default()).unwrap()],
        ..ScopeFields::default()
    };
    assert!(matches!(
        verify(bad, ScopeContext::Ordinary).unwrap_err().kind,
        ScopeVerificationErrorKind::Graph(GraphRecordError::UnknownEndpoint(
            "loop initializer input"
        ))
    ));
}
#[test]
fn one_definition_has_stable_bytes_and_identity_across_role_analysis() {
    let limits = Limits::default();
    for fields in [
        ScopeFields::default(),
        ScopeFields {
            carried: ports("x"),
            edges: vec![Edge::new(
                "advance".parse().unwrap(),
                EdgeSource::Carried("x".parse().unwrap()),
                EdgeDestination::Next("x".parse().unwrap()),
            )],
            ..ScopeFields::default()
        },
    ] {
        let scope = Scope::new(fields, &limits).unwrap();
        let bytes = scope.encode(&limits).unwrap();
        let digest = scope.digest(&limits).unwrap();
        let scope = Scope::decode(&bytes, &limits).unwrap();
        for role in [ScopeContext::Ordinary, ScopeContext::LoopBody] {
            let result = verify_definition(&scope, role);
            if role == ScopeContext::Ordinary && !scope.fields().carried.is_empty() {
                assert!(matches!(
                    result.unwrap_err().kind,
                    ScopeVerificationErrorKind::Graph(GraphRecordError::InvalidScopeRole)
                ));
            } else {
                result.unwrap();
            }
            assert_eq!(scope.encode(&limits).unwrap(), bytes);
            assert_eq!(scope.digest(&limits).unwrap(), digest);
        }
    }
}

#[test]
fn carried_and_next_endpoints_check_role_before_declared_membership() {
    // No carried declarations: ordinary use must reject each endpoint category
    // independently of the earlier ordinary-scope carried-table check.
    for (source, destination, missing) in [
        (
            EdgeSource::Carried("missing".parse().unwrap()),
            EdgeDestination::Output("x".parse().unwrap()),
            "carried source",
        ),
        (
            EdgeSource::Input("x".parse().unwrap()),
            EdgeDestination::Next("missing".parse().unwrap()),
            "next destination",
        ),
    ] {
        let scope = Scope::new(
            ScopeFields {
                inputs: ports("x"),
                outputs: ports("x"),
                edges: vec![Edge::new("edge".parse().unwrap(), source, destination)],
                ..ScopeFields::default()
            },
            &Limits::default(),
        )
        .unwrap();
        assert!(matches!(
            verify_definition(&scope, ScopeContext::Ordinary)
                .unwrap_err()
                .kind,
            ScopeVerificationErrorKind::Graph(GraphRecordError::InvalidScopeRole)
        ));
        assert_eq!(
            verify_definition(&scope, ScopeContext::LoopBody)
                .unwrap_err()
                .kind,
            ScopeVerificationErrorKind::Graph(GraphRecordError::UnknownEndpoint(missing))
        );
    }
}
#[test]
fn node_contract_contexts_are_analyzed_at_their_actual_site() {
    let l = Limits::default();
    for post in [false, true] {
        let mut node = NodeFields::new(
            "worker".parse().unwrap(),
            if post {
                Operation::Scope(Digest::from_bytes([0; 32]))
            } else {
                Operation::Eval(truth())
            },
        );
        node.outputs = PortTable::new(
            vec![(
                "value".parse().unwrap(),
                Port::new(ValueType::builtin(BuiltinType::Boolean), true),
            )],
            &l,
        )
        .unwrap();
        if post {
            node.postconditions = make(K::Status("worker".parse().unwrap()));
        } else {
            node.preconditions = make(K::Status("worker".parse().unwrap()));
        }
        let fields = ScopeFields {
            nodes: vec![Node::new(node, &l).unwrap()],
            ..ScopeFields::default()
        };
        let error = verify(fields, ScopeContext::Ordinary).unwrap_err();
        assert_eq!(
            error.site,
            Some(if post {
                ExpressionSite::NodePostconditions("worker".parse().unwrap())
            } else {
                ExpressionSite::NodePreconditions("worker".parse().unwrap())
            })
        );
        assert!(
            matches!(error.kind,ScopeVerificationErrorKind::Expression(ref e) if matches!(**e,ExpressionTypeError::Expression(ExpressionError::ForbiddenReference(_))))
        );
    }
}
#[test]
fn undeclared_generic_variables_are_rejected_in_parameters_and_results() {
    let l = Limits::default();
    let t = ValueType::new(
        ValueTypeKind::Var("t".parse().unwrap()),
        TypeContext::Signature,
        &l,
    )
    .unwrap();
    let null = Port::new(ValueType::builtin(BuiltinType::Null), true);
    for signature in [
        FunctionSignature::new(vec![], vec![Port::new(t.clone(), true)], null.clone(), &l).unwrap(),
        FunctionSignature::new(vec![], vec![], Port::new(t, true), &l).unwrap(),
    ] {
        let signature = FunctionSignature::decode(&signature.encode(&l).unwrap(), &l).unwrap();
        let id = Digest::from_bytes([1; 32]);
        let lib = Library::new(
            "test".into(),
            "1".into(),
            id,
            vec![("function".parse().unwrap(), signature)],
            &l,
        )
        .unwrap();
        let mut env = ExpressionTypeEnvironment::default();
        env.libraries.insert(id, lib);
        assert_eq!(
            check_expression(&truth(), C::Eval, &env, None, &l),
            Err(ExpressionTypeError::Metadata(
                MetadataError::UndeclaredTypeVariable
            ))
        );
    }
}
