//! Name/context errors remain representable and are rejected by semantic analysis.
use ExpressionContext as C;
use ExpressionKind as K;
use ValueReference as R;
use htlk_analyzer::*;
use htlk_executable::{cbor::Limits, digest::Digest, *};
fn make(kind: K) -> Expression {
    Expression::new(kind, C::LoopUntil, &Limits::default()).unwrap()
}
fn truth() -> Expression {
    Expression::literal(ScalarLiteral::Boolean(true))
}
fn ports(name: &str) -> PortTable {
    PortTable::new(
        vec![(
            name.parse().unwrap(),
            Port::new(ValueType::primitive(PrimitiveType::String), true),
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
    let scope = Scope::new(fields, role, &l).unwrap();
    let schemas = NativeSchemas::compile(
        &SchemaCatalog::new(vec![], &l).unwrap(),
        NativeSchemaOptions::default(),
        &l,
    )
    .unwrap();
    let until = truth();
    verify_scope_graph(
        &scope,
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
            Port::new(ValueType::primitive(PrimitiveType::String), true),
        );
    }
    env.outcomes.insert("worker".parse().unwrap());
    for (source, row) in sources.into_iter().zip(allowed) {
        let nested = make(K::List(vec![make(K::Ref {
            source,
            path: vec![],
        })]));
        let encoded = nested.encode(C::LoopUntil, &l).unwrap();
        for (context, expected) in contexts.into_iter().zip(row) {
            let decoded = Expression::decode(&encoded, context, &l).unwrap();
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
        nodes: vec![Node::new(node, ScopeContext::Ordinary, &Limits::default()).unwrap()],
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
    let mut bad = body;
    bad.postconditions = make(K::Not(Box::new(Expression::literal(
        ScalarLiteral::Boolean(false),
    ))));
    assert!(matches!(
        verify(bad, ScopeContext::LoopBody).unwrap_err().kind,
        ScopeVerificationErrorKind::Graph(GraphRecordError::InvalidScopeRole)
    ));
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
        nodes: vec![Node::new(node, ScopeContext::Ordinary, &Limits::default()).unwrap()],
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
                Port::new(ValueType::primitive(PrimitiveType::Boolean), true),
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
            nodes: vec![Node::new(node, ScopeContext::Ordinary, &l).unwrap()],
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
    let null = Port::new(ValueType::primitive(PrimitiveType::Null), true);
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
