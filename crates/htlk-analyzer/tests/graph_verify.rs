//! Complete scope wait dependencies, binding admission and explicit observability.
use htlk_analyzer::{
    NativeSchemaOptions, NativeSchemas, SchemaCatalog, ScopeVerificationErrorKind as Error,
    WaitVertex, verify_scope_graph,
};
use htlk_cbor::Limits;
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{
    BinaryOperator, BuiltinType as P, Edge, EdgeDestination as D, EdgeSource as S, Expression,
    ExpressionContext as C, ExpressionKind as E, Node, NodeFields, Operation, Port, PortTable,
    ScalarLiteral as L, Scope, ScopeContext as Role, ScopeFields, ValueReference, ValueType,
};
fn ports(entries: &[(&str, P, bool)]) -> PortTable {
    PortTable::new(
        entries
            .iter()
            .map(|(name, ty, required)| {
                (
                    name.parse().unwrap(),
                    Port::new(ValueType::builtin(*ty), *required),
                )
            })
            .collect(),
        &Limits::default(),
    )
    .unwrap()
}
fn node(name: &str, input: Option<bool>, guard: Expression) -> Node {
    let mut n = NodeFields::new(
        name.parse().unwrap(),
        Operation::Eval(Expression::literal(L::Boolean(true))),
    );
    n.outputs = ports(&[("value", P::Boolean, true)]);
    if let Some(required) = input {
        n.inputs = ports(&[("input", P::Boolean, required)]);
    }
    n.guard = guard;
    Node::new(n, Role::Ordinary, &Limits::default()).unwrap()
}
fn truth() -> Expression {
    Expression::literal(L::Boolean(true))
}
fn succeeded(name: &str, context: C) -> Expression {
    Expression::new(
        E::Binary {
            operator: BinaryOperator::Eq,
            left: Box::new(
                Expression::new(
                    E::Status(name.parse().unwrap()),
                    context,
                    &Limits::default(),
                )
                .unwrap(),
            ),
            right: Box::new(Expression::literal(L::String("succeeded".into()))),
        },
        context,
        &Limits::default(),
    )
    .unwrap()
}
fn output(node: &str) -> S {
    S::Output {
        node: node.parse().unwrap(),
        port: "value".parse().unwrap(),
    }
}
fn input(node: &str) -> D {
    D::Input {
        node: node.parse().unwrap(),
        port: "input".parse().unwrap(),
    }
}
fn edge(id: &str, source: S, destination: D) -> Edge {
    Edge::new(id.parse().unwrap(), source, destination)
}
fn verify(
    fields: ScopeFields,
) -> Result<htlk_analyzer::ScopeGraphPlan, htlk_analyzer::ScopeVerificationError> {
    let limits = Limits::default();
    let scope = Scope::new(fields, Role::Ordinary, &limits).unwrap();
    let schemas = NativeSchemas::compile(
        &SchemaCatalog::new(vec![], &limits).unwrap(),
        NativeSchemaOptions::default(),
        &limits,
    )
    .unwrap();
    verify_scope_graph(
        &scope,
        Role::Ordinary,
        None,
        &Default::default(),
        &Default::default(),
        &schemas,
        &limits,
    )
}
fn chain() -> ScopeFields {
    ScopeFields {
        nodes: vec![node("a", None, truth()), node("b", Some(true), truth())],
        outputs: ports(&[("result", P::Boolean, true)]),
        edges: vec![
            edge("feed", output("a"), input("b")),
            edge("result", output("b"), D::Output("result".parse().unwrap())),
        ],
        ..ScopeFields::default()
    }
}
#[test]
fn whole_port_chain_has_stable_complete_wait_dependencies() {
    let plan = verify(chain()).unwrap();
    let indexes: std::collections::BTreeMap<_, _> = plan
        .vertices()
        .iter()
        .enumerate()
        .map(|(i, v)| (v, i))
        .collect();
    let a = indexes[&WaitVertex::Outcome("a".parse().unwrap())];
    let b = indexes[&WaitVertex::Input {
        node: "b".parse().unwrap(),
        port: "input".parse().unwrap(),
    }];
    assert!(plan.dependencies().contains(&(a, b)));
    for &(from, to) in plan.dependencies() {
        let position = |v| {
            plan.topological_order()
                .iter()
                .position(|i| *i == v)
                .unwrap()
        };
        assert!(position(from) < position(to));
    }
    assert_eq!(plan.boundaries().len(), 2);
    assert_eq!(plan.bindings().len(), 2);
    assert_eq!(
        plan.topological_order(),
        verify(chain()).unwrap().topological_order()
    );
}
#[test]
fn hidden_cycles_include_skipped_boolean_branches_and_optional_inputs() {
    let context = C::Guard { loop_body: false };
    let guard = Expression::new(
        E::Binary {
            operator: BinaryOperator::And,
            left: Box::new(Expression::literal(L::Boolean(false))),
            right: Box::new(succeeded("b", context)),
        },
        context,
        &Limits::default(),
    )
    .unwrap();
    let mut fields = chain();
    fields.nodes[0] = node("a", None, guard);
    assert_eq!(verify(fields).unwrap_err().kind, Error::Cycle);
    let fields = ScopeFields {
        nodes: vec![node("a", Some(false), truth())],
        outputs: ports(&[("result", P::Boolean, true)]),
        edges: vec![
            edge("self_input", output("a"), input("a"))
                .with_guard(Expression::literal(L::Boolean(false))),
            edge("result", output("a"), D::Output("result".parse().unwrap())),
        ],
        ..ScopeFields::default()
    };
    assert_eq!(verify(fields).unwrap_err().kind, Error::Cycle);
}
#[test]
fn edge_guards_and_self_outcomes_are_checked_as_waits() {
    let mut fields = chain();
    fields.edges[0] = edge("feed", output("a"), input("b"))
        .with_guard(succeeded("b", C::Guard { loop_body: false }));
    assert_eq!(verify(fields).unwrap_err().kind, Error::Cycle);
    let mut fields = chain();
    fields.nodes[0] = node("a", None, succeeded("a", C::Guard { loop_body: false }));
    assert_eq!(verify(fields).unwrap_err().kind, Error::SelfOutcome);
}
#[test]
fn coverage_and_conditional_uniqueness_are_separate() {
    let mut fields = chain();
    fields.edges.remove(0);
    assert!(matches!(
        verify(fields).unwrap_err().kind,
        Error::MissingCandidate(WaitVertex::Input { .. })
    ));
    let mut fields = ScopeFields {
        inputs: ports(&[("a", P::Boolean, true), ("b", P::Boolean, true)]),
        outputs: ports(&[("result", P::Boolean, true)]),
        edges: vec![
            edge(
                "a",
                S::Input("a".parse().unwrap()),
                D::Output("result".parse().unwrap()),
            ),
            edge(
                "b",
                S::Input("b".parse().unwrap()),
                D::Output("result".parse().unwrap()),
            ),
        ],
        ..ScopeFields::default()
    };
    assert!(matches!(
        verify(fields.clone()).unwrap_err().kind,
        Error::UnconditionalWriters(_)
    ));
    let condition = Expression::new(
        E::Ref {
            source: ValueReference::Input("a".parse().unwrap()),
            path: vec![],
        },
        C::Guard { loop_body: false },
        &Limits::default(),
    )
    .unwrap();
    fields.edges[1] = fields.edges[1].clone().with_guard(condition);
    assert!(verify(fields).unwrap().bindings()[0].needs_uniqueness_check());
}
#[test]
fn completion_observation_is_explicit_not_implicit_child_settlement() {
    let mut fields = ScopeFields {
        nodes: vec![node("side_effect", None, truth())],
        ..ScopeFields::default()
    };
    assert_eq!(
        verify(fields.clone()).unwrap_err().kind,
        Error::Unobservable("side_effect".parse().unwrap())
    );
    fields.postconditions = succeeded("side_effect", C::ScopePostconditions);
    verify(fields).unwrap();
}
#[test]
fn loop_body_observability_is_checked_for_each_until_use() {
    let limits = Limits::default();
    let fields = ScopeFields {
        carried: ports(&[("state", P::Boolean, true)]),
        nodes: vec![node("body", None, truth())],
        edges: vec![edge(
            "carry",
            S::Carried("state".parse().unwrap()),
            D::Next("state".parse().unwrap()),
        )],
        ..ScopeFields::default()
    };
    let scope = Scope::new(fields, Role::LoopBody, &limits).unwrap();
    let schemas = NativeSchemas::compile(
        &SchemaCatalog::new(vec![], &limits).unwrap(),
        NativeSchemaOptions::default(),
        &limits,
    )
    .unwrap();
    let observed = succeeded("body", C::LoopUntil);
    verify_scope_graph(
        &scope,
        Role::LoopBody,
        Some(&observed),
        &Default::default(),
        &Default::default(),
        &schemas,
        &limits,
    )
    .unwrap();
    assert!(matches!(
        verify_scope_graph(
            &scope,
            Role::LoopBody,
            Some(&truth()),
            &Default::default(),
            &Default::default(),
            &schemas,
            &limits
        )
        .unwrap_err()
        .kind,
        Error::Unobservable(_)
    ));
}

#[test]
fn edge_types_and_expression_names_are_checked_in_their_own_contexts() {
    let mut fields = chain();
    fields.outputs = ports(&[("result", P::Integer, true)]);
    assert!(matches!(
        verify(fields).unwrap_err().kind,
        Error::Expression(_)
    ));
    let mut fields = chain();
    let mut bad = fields.nodes[1].fields().clone();
    bad.operation = Operation::Eval(
        Expression::new(
            E::Ref {
                source: ValueReference::Input("missing".parse().unwrap()),
                path: vec![],
            },
            C::Eval,
            &Limits::default(),
        )
        .unwrap(),
    );
    fields.nodes[1] = Node::new(bad, Role::Ordinary, &Limits::default()).unwrap();
    assert_eq!(verify(fields).unwrap_err().kind, Error::UnknownReference);
}
#[test]
fn optional_sources_can_feed_required_destinations_without_expression_presence_errors() {
    let fields = ScopeFields {
        inputs: ports(&[("source", P::Boolean, false)]),
        outputs: ports(&[("result", P::Boolean, true)]),
        edges: vec![edge(
            "value",
            S::Input("source".parse().unwrap()),
            D::Output("result".parse().unwrap()),
        )],
        ..ScopeFields::default()
    };
    let plan = verify(fields).unwrap();
    let boundary = &plan.boundaries()[0];
    assert!(!boundary.source().required());
    assert!(boundary.destination().required());
    assert!(
        !boundary
            .analysis()
            .runtime_checks()
            .iter()
            .any(|check| matches!(check.kind, htlk_analyzer::RuntimeTypeCheckKind::Present))
    );
}

#[test]
fn generated_guard_graphs_agree_with_an_independent_cycle_oracle() {
    let limits = Limits::default();
    let context = C::Guard { loop_body: false };
    let mut seed = 0x7351_c0de_u64;
    for _ in 0..96 {
        let count = 5;
        let mut adjacency = vec![vec![false; count]; count];
        let mut fields = ScopeFields::default();
        let mut outputs = Vec::new();
        for destination in 0..count {
            let name = format!("n{destination}");
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut guard = Expression::literal(L::Boolean(seed & 1 == 0));
            for (source, row) in adjacency.iter_mut().enumerate() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                if seed >> 61 == 0 {
                    row[destination] = true;
                    guard = Expression::new(
                        E::Binary {
                            operator: BinaryOperator::And,
                            left: Box::new(guard),
                            right: Box::new(succeeded(&format!("n{source}"), context)),
                        },
                        context,
                        &limits,
                    )
                    .unwrap();
                }
            }
            fields.nodes.push(node(&name, None, guard));
            outputs.push((
                name.parse().unwrap(),
                Port::new(ValueType::builtin(P::Boolean), true),
            ));
            fields
                .edges
                .push(edge(&name, output(&name), D::Output(name.parse().unwrap())));
        }
        fields.outputs = PortTable::new(outputs, &limits).unwrap();
        let mut reached = vec![false; count];
        for _ in 0..count {
            for node in 0..count {
                if (0..count).all(|source| !adjacency[source][node] || reached[source]) {
                    reached[node] = true;
                }
            }
        }
        let acyclic = reached.iter().all(|v| *v);
        assert_eq!(
            verify(fields).is_ok(),
            acyclic,
            "guard adjacency {adjacency:?}"
        );
    }
}

#[test]
fn expression_failures_report_canonical_subtrees_without_literal_contents() {
    let context = C::Guard { loop_body: false };
    let limits = Limits::default();
    let mut f = chain();
    let bad = Expression::new(
        E::Binary {
            operator: BinaryOperator::Eq,
            left: Box::new(Expression::literal(L::String("TOP_SECRET_OPERAND".into()))),
            right: Box::new(
                Expression::new(
                    E::Ref {
                        source: ValueReference::Input("missing".parse().unwrap()),
                        path: vec![],
                    },
                    context,
                    &limits,
                )
                .unwrap(),
            ),
        },
        context,
        &limits,
    )
    .unwrap();
    f.nodes[0] = node("a", None, bad);
    let error = verify(f).unwrap_err();
    assert_eq!(
        error.site,
        Some(htlk_analyzer::ExpressionSite::NodeGuard(
            "a".parse().unwrap()
        ))
    );
    assert_eq!(error.expression_path, Some(vec![1]));
    assert!(!format!("{error:?}").contains("TOP_SECRET_OPERAND"));
    let mut f = chain();
    let bad = Expression::new(
        E::Binary {
            operator: BinaryOperator::And,
            left: Box::new(truth()),
            right: Box::new(Expression::literal(L::Integer(1))),
        },
        context,
        &limits,
    )
    .unwrap();
    f.nodes[0] = node("a", None, bad);
    assert_eq!(verify(f).unwrap_err().expression_path, Some(vec![1]));
}

#[test]
fn derived_plan_limits_count_projection_types_not_only_result_types() {
    let limits = Limits::default();
    let mut ty = ValueType::new(
        htlk_executable::ValueTypeKind::Record(vec![(
            "flag".into(),
            Port::new(ValueType::builtin(P::Boolean), true),
        )]),
        htlk_executable::TypeContext::Value,
        &limits,
    )
    .unwrap();
    for _ in 0..16 {
        ty = ValueType::new(
            htlk_executable::ValueTypeKind::Record(vec![("nested".into(), Port::new(ty, true))]),
            htlk_executable::TypeContext::Value,
            &limits,
        )
        .unwrap();
    }
    let mut path = vec![htlk_executable::PathStep::Field("nested".into()); 16];
    path.push(htlk_executable::PathStep::Field("flag".into()));
    let guard = Expression::new(
        E::Ref {
            source: ValueReference::Input("payload".parse().unwrap()),
            path,
        },
        C::Guard { loop_body: false },
        &limits,
    )
    .unwrap();
    let mut f = chain();
    f.nodes[0] = node("a", None, guard);
    f.inputs = PortTable::new(
        vec![("payload".parse().unwrap(), Port::new(ty, true))],
        &limits,
    )
    .unwrap();
    let scope = Scope::new(f, Role::Ordinary, &limits).unwrap();
    let tight = Limits {
        max_document_bytes: scope.encode(Role::Ordinary, &limits).unwrap().len(),
        ..limits.clone()
    };
    scope.to_value(Role::Ordinary, &tight).unwrap();
    let schemas = NativeSchemas::compile(
        &SchemaCatalog::new(vec![], &limits).unwrap(),
        NativeSchemaOptions::default(),
        &limits,
    )
    .unwrap();
    verify_scope_graph(
        &scope,
        Role::Ordinary,
        None,
        &Default::default(),
        &Default::default(),
        &schemas,
        &limits,
    )
    .unwrap();
    assert!(
        verify_scope_graph(
            &scope,
            Role::Ordinary,
            None,
            &Default::default(),
            &Default::default(),
            &schemas,
            &tight
        )
        .is_err()
    );
}
