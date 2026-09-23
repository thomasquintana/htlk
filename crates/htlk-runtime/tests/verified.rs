//! End-to-end envelope, profile, graph, and checked execution admission.
use htlk_analyzer::{ExpressionSite, ScopeUse, ScopeVerificationErrorKind, embedded_schema_base};
use htlk_cbor::{Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{
    BuiltinType as P, CanonicalDocument, DocumentFields, Edge, EdgeDestination as D,
    EdgeSource as S, EvaluatorLimits, ExecutableEnvelope, ExecutionLimits, ExecutionProfile,
    Expression, ExpressionContext as C, ExpressionKind as E, Node, NodeFields, Operation,
    PolicyDocument, PolicyFields, Port, PortTable, ScalarLiteral as L, Scope, ScopeContext as Role,
    ScopeFields, ValueReference, ValueType, digest::Digest,
};
use htlk_runtime::{
    EvaluationError, EvaluationFrame, EvaluationValue, ExecutableVerificationError as Error,
    NativeRegistry, NativeRegistryError, native_profile, verify_executable,
};
fn policy() -> PolicyDocument {
    PolicyDocument::new(
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
                max_regex_bytes: 1024,
                max_regex_compiled_bytes: 4096,
                max_output_bytes: 65536,
                max_steps: 100000,
            },
            maximum_scope_depth: 32,
            maximum_expanded_nodes: 1000,
        },
        &Limits::default(),
    )
    .unwrap()
}
fn registry() -> NativeRegistry {
    let mut registry = NativeRegistry::new(&Limits::default()).unwrap();
    registry.link_policy(&policy()).unwrap();
    registry
}
fn ports(names: &[&str]) -> PortTable {
    PortTable::new(
        names
            .iter()
            .map(|name| {
                (
                    name.parse().unwrap(),
                    Port::new(ValueType::builtin(P::Boolean), true),
                )
            })
            .collect(),
        &Limits::default(),
    )
    .unwrap()
}
fn scope() -> Scope {
    let limits = Limits::default();
    let mut node = NodeFields::new(
        "compute".parse().unwrap(),
        Operation::Eval(
            Expression::new(
                E::Ref {
                    source: ValueReference::Input("input".parse().unwrap()),
                    path: vec![],
                },
                C::Eval,
                &limits,
            )
            .unwrap(),
        ),
    );
    node.inputs = ports(&["input"]);
    node.outputs = ports(&["value"]);
    Scope::new(
        ScopeFields {
            inputs: ports(&["seed"]),
            outputs: ports(&["result"]),
            nodes: vec![Node::new(node, Role::Ordinary, &limits).unwrap()],
            edges: vec![
                Edge::new(
                    "input".parse().unwrap(),
                    S::Input("seed".parse().unwrap()),
                    D::Input {
                        node: "compute".parse().unwrap(),
                        port: "input".parse().unwrap(),
                    },
                ),
                Edge::new(
                    "result".parse().unwrap(),
                    S::Output {
                        node: "compute".parse().unwrap(),
                        port: "value".parse().unwrap(),
                    },
                    D::Output("result".parse().unwrap()),
                ),
            ],
            ..ScopeFields::default()
        },
        Role::Ordinary,
        &limits,
    )
    .unwrap()
}
fn fields(root: Scope) -> DocumentFields {
    let limits = Limits::default();
    let policy = policy();
    let root_id = root.digest(Role::Ordinary, &limits).unwrap();
    let mut fields = DocumentFields::new(
        "test.graph".into(),
        native_profile(&policy, &limits).unwrap(),
        root_id,
    );
    fields.scopes.insert(root_id, root);
    fields
        .documents
        .insert(policy.digest(), policy.document().clone());
    fields
}
fn bytes(fields: DocumentFields) -> Vec<u8> {
    let limits = Limits::default();
    CanonicalDocument::new(fields, &limits)
        .unwrap()
        .envelope(&limits)
        .unwrap()
        .encode(&limits)
        .unwrap()
}
#[test]
fn complete_envelope_admission_exposes_only_checked_execution_and_boundaries() {
    let limits = Limits::default();
    let registry = registry();
    let bytes = bytes(fields(scope()));
    let verified = verify_executable(&bytes, &registry, &limits).unwrap();
    assert_eq!(
        verified.fingerprint(),
        ExecutableEnvelope::decode(&bytes, &limits)
            .unwrap()
            .fingerprint()
    );
    assert_eq!(
        verified
            .document()
            .envelope(&limits)
            .unwrap()
            .encode(&limits)
            .unwrap(),
        bytes
    );
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            ValueReference::Input("input".parse().unwrap()),
            vec![],
            Ok(EvaluationValue::Present(Value::Bool(true))),
            &limits,
        )
        .unwrap();
    let site = ExpressionSite::Eval("compute".parse().unwrap());
    assert_eq!(
        verified
            .evaluate(&ScopeUse::Root, &site, &frame)
            .unwrap()
            .value,
        EvaluationValue::Present(Value::Bool(true))
    );
    frame
        .bind(
            ValueReference::Input("input".parse().unwrap()),
            vec![],
            Ok(EvaluationValue::Present(Value::Integer(1))),
            &limits,
        )
        .unwrap();
    assert_eq!(
        verified.evaluate(&ScopeUse::Root, &site, &frame),
        Err(EvaluationError::OperandType)
    );
    assert_eq!(
        verified.validate_edge_value(
            &ScopeUse::Root,
            &"result".parse().unwrap(),
            &Value::Integer(1)
        ),
        Err(EvaluationError::OperandType)
    );
    assert_eq!(
        verified.evaluate(
            &ScopeUse::Root,
            &ExpressionSite::Eval("missing".parse().unwrap()),
            &frame
        ),
        Err(EvaluationError::UnknownExpression)
    );
}
#[test]
fn opaque_payload_and_wrong_linked_profile_never_produce_verified_objects() {
    let limits = Limits::default();
    let registry = registry();
    let malformed = ExecutableEnvelope::new(vec![], &limits)
        .unwrap()
        .encode(&limits)
        .unwrap();
    assert!(matches!(
        verify_executable(&malformed, &registry, &limits),
        Err(Error::Document(_))
    ));
    let mut f = fields(scope());
    let p = &f.profile;
    f.profile = ExecutionProfile::new(
        Digest::from_bytes([0; 32]),
        p.regex_engine().clone(),
        p.schema_validator().clone(),
        p.uri_template_engine().clone(),
        p.policy_document(),
        &limits,
    )
    .unwrap();
    assert!(matches!(
        verify_executable(&bytes(f), &registry, &limits),
        Err(Error::Registry(NativeRegistryError::ProfileMismatch(
            "core"
        )))
    ));
}
#[test]
fn valid_record_assembly_is_not_sufficient_for_graph_admission() {
    let limits = Limits::default();
    let registry = registry();
    let mut root = scope().fields().clone();
    root.edges.remove(0);
    let root = Scope::new(root, Role::Ordinary, &limits).unwrap();
    let result = verify_executable(&bytes(fields(root)), &registry, &limits);
    let Err(Error::Analysis(error)) = result else {
        panic!("missing required candidate admitted");
    };
    let htlk_analyzer::DocumentAnalysisError::Graph(error) = *error else {
        panic!("unexpected analysis stage");
    };
    assert_eq!(error.scope_use, Some(ScopeUse::Root));
    assert!(matches!(
        error.cause.kind,
        ScopeVerificationErrorKind::MissingCandidate(_)
    ));
}
#[test]
fn repeated_task_uses_share_plans_and_preserve_use_locations() {
    let limits = Limits::default();
    let registry = registry();
    let mut leaf_node = NodeFields::new(
        "constant".parse().unwrap(),
        Operation::Eval(Expression::literal(L::Boolean(true))),
    );
    leaf_node.outputs = ports(&["value"]);
    let leaf = Scope::new(
        ScopeFields {
            outputs: ports(&["value"]),
            nodes: vec![Node::new(leaf_node, Role::Ordinary, &limits).unwrap()],
            edges: vec![Edge::new(
                "value".parse().unwrap(),
                S::Output {
                    node: "constant".parse().unwrap(),
                    port: "value".parse().unwrap(),
                },
                D::Output("value".parse().unwrap()),
            )],
            ..ScopeFields::default()
        },
        Role::Ordinary,
        &limits,
    )
    .unwrap();
    let leaf_id = leaf.digest(Role::Ordinary, &limits).unwrap();
    let mut root = ScopeFields {
        outputs: ports(&["a", "b"]),
        ..ScopeFields::default()
    };
    for name in ["a", "b"] {
        let mut node = NodeFields::new(name.parse().unwrap(), Operation::Scope(leaf_id));
        node.outputs = ports(&["value"]);
        root.nodes
            .push(Node::new(node, Role::Ordinary, &limits).unwrap());
        root.edges.push(Edge::new(
            name.parse().unwrap(),
            S::Output {
                node: name.parse().unwrap(),
                port: "value".parse().unwrap(),
            },
            D::Output(name.parse().unwrap()),
        ));
    }
    let root = Scope::new(root, Role::Ordinary, &limits).unwrap();
    let mut f = fields(root);
    let root_id = f.root_scope;
    f.scopes.insert(leaf_id, leaf);
    let verified = verify_executable(&bytes(f), &registry, &limits).unwrap();
    assert_eq!(verified.graphs().plans().len(), 3);
    let a = ScopeUse::Node {
        scope: root_id,
        node: "a".parse().unwrap(),
    };
    let b = ScopeUse::Node {
        scope: root_id,
        node: "b".parse().unwrap(),
    };
    assert!(std::ptr::eq(
        verified.graphs().plan(&a).unwrap(),
        verified.graphs().plan(&b).unwrap()
    ));
    assert_eq!(
        verified
            .evaluate(
                &a,
                &ExpressionSite::Eval("constant".parse().unwrap()),
                &EvaluationFrame::default()
            )
            .unwrap()
            .value,
        EvaluationValue::Present(Value::Bool(true))
    );
}

#[test]
fn waits_and_loop_boundaries_are_admitted_and_until_uses_the_body_context() {
    let limits = Limits::default();
    let registry = registry();
    let json = Port::new(ValueType::builtin(P::Json), true);
    let mut wait = NodeFields::new(
        "waiter".parse().unwrap(),
        Operation::Wait {
            topic: "reply".into(),
            timeout_ms: 100,
        },
    );
    wait.inputs =
        PortTable::new(vec![("request".parse().unwrap(), json.clone())], &limits).unwrap();
    wait.outputs = ports(&["value"]);
    let root = Scope::new(
        ScopeFields {
            inputs: PortTable::new(vec![("request".parse().unwrap(), json)], &limits).unwrap(),
            outputs: ports(&["result"]),
            nodes: vec![Node::new(wait, Role::Ordinary, &limits).unwrap()],
            edges: vec![
                Edge::new(
                    "request".parse().unwrap(),
                    S::Input("request".parse().unwrap()),
                    D::Input {
                        node: "waiter".parse().unwrap(),
                        port: "request".parse().unwrap(),
                    },
                ),
                Edge::new(
                    "result".parse().unwrap(),
                    S::Output {
                        node: "waiter".parse().unwrap(),
                        port: "value".parse().unwrap(),
                    },
                    D::Output("result".parse().unwrap()),
                ),
            ],
            ..ScopeFields::default()
        },
        Role::Ordinary,
        &limits,
    )
    .unwrap();
    verify_executable(&bytes(fields(root)), &registry, &limits).unwrap();
    let body = Scope::new(
        ScopeFields {
            inputs: ports(&["seed"]),
            outputs: ports(&["result"]),
            carried: ports(&["state"]),
            edges: vec![
                Edge::new(
                    "next".parse().unwrap(),
                    S::Carried("state".parse().unwrap()),
                    D::Next("state".parse().unwrap()),
                ),
                Edge::new(
                    "result".parse().unwrap(),
                    S::Carried("state".parse().unwrap()),
                    D::Output("result".parse().unwrap()),
                ),
            ],
            ..ScopeFields::default()
        },
        Role::LoopBody,
        &limits,
    )
    .unwrap();
    let body_id = body.digest(Role::LoopBody, &limits).unwrap();
    let until = Expression::new(
        E::Ref {
            source: ValueReference::Next("state".parse().unwrap()),
            path: vec![],
        },
        C::LoopUntil,
        &limits,
    )
    .unwrap();
    let mut repeat = NodeFields::new(
        "repeat".parse().unwrap(),
        Operation::Loop {
            body: body_id,
            initializers: vec![("state".parse().unwrap(), "seed".parse().unwrap())],
            until,
            max_iterations: 3,
        },
    );
    repeat.inputs = ports(&["seed"]);
    repeat.outputs = ports(&["result"]);
    let root = Scope::new(
        ScopeFields {
            inputs: ports(&["seed"]),
            outputs: ports(&["result"]),
            nodes: vec![Node::new(repeat, Role::Ordinary, &limits).unwrap()],
            edges: vec![
                Edge::new(
                    "seed".parse().unwrap(),
                    S::Input("seed".parse().unwrap()),
                    D::Input {
                        node: "repeat".parse().unwrap(),
                        port: "seed".parse().unwrap(),
                    },
                ),
                Edge::new(
                    "result".parse().unwrap(),
                    S::Output {
                        node: "repeat".parse().unwrap(),
                        port: "result".parse().unwrap(),
                    },
                    D::Output("result".parse().unwrap()),
                ),
            ],
            ..ScopeFields::default()
        },
        Role::Ordinary,
        &limits,
    )
    .unwrap();
    let mut f = fields(root);
    let root_id = f.root_scope;
    f.scopes.insert(body_id, body);
    let verified = verify_executable(&bytes(f), &registry, &limits).unwrap();
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            ValueReference::Next("state".parse().unwrap()),
            vec![],
            Ok(EvaluationValue::Present(Value::Bool(true))),
            &limits,
        )
        .unwrap();
    assert_eq!(
        verified
            .evaluate(
                &ScopeUse::Node {
                    scope: root_id,
                    node: "repeat".parse().unwrap()
                },
                &ExpressionSite::Until,
                &frame
            )
            .unwrap()
            .value,
        EvaluationValue::Present(Value::Bool(true))
    );
}

#[test]
fn malformed_payloads_with_valid_outer_fingerprints_are_rejected_without_panics() {
    let limits = Limits::default();
    let registry = registry();
    let mut seed = 0x5352_u64;
    for length in 0..256 {
        let payload: Vec<_> = (0..length)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (seed >> 32) as u8
            })
            .collect();
        let bytes = ExecutableEnvelope::new(payload, &limits)
            .unwrap()
            .encode(&limits)
            .unwrap();
        assert!(matches!(
            verify_executable(&bytes, &registry, &limits),
            Err(Error::Document(_))
        ));
    }
}

#[test]
fn all_mcp_binding_kinds_pass_the_complete_offline_admission_boundary() {
    use htlk_executable::{
        JsonDocument, McpBinding, McpBindingKind as K, McpTransport, RetryPolicy, ServerIdentity,
        TypeContext, ValueTypeKind,
    };
    let limits = Limits::default();
    let registry = registry();
    let server = ServerIdentity::new(
        "fixture".into(),
        McpTransport::Stdio,
        "fixture".into(),
        "1".into(),
        &limits,
    )
    .unwrap();
    let schema = JsonDocument::new(
        br#"{"type":"object","properties":{"flag":{"type":"boolean"}}}"#,
        &limits,
    )
    .unwrap();
    let schema_type = ValueType::new(
        ValueTypeKind::Schema(schema.digest()),
        TypeContext::Value,
        &limits,
    )
    .unwrap();
    let record = |name: &str| {
        ValueType::new(
            ValueTypeKind::Record(vec![(
                name.into(),
                Port::new(ValueType::builtin(P::String), true),
            )]),
            TypeContext::Value,
            &limits,
        )
        .unwrap()
    };
    let schema_text = std::str::from_utf8(schema.as_bytes()).unwrap();
    let cases = [
        (
            K::Tool {
                name: "tool".into(),
                input_schema: schema.digest(),
                output_schema: schema.digest(),
            },
            format!(
                r#"{{"name":"tool","inputSchema":{schema_text},"outputSchema":{schema_text}}}"#
            ),
            Some(schema_type.clone()),
            schema_type,
        ),
        (
            K::Resource {
                uri: "file:///note".into(),
            },
            r#"{"name":"note","uri":"file:///note"}"#.into(),
            None,
            ValueType::builtin(P::McpResourceResult),
        ),
        (
            K::Template {
                uri_template: "file:///{id}".into(),
            },
            r#"{"name":"by_id","uriTemplate":"file:///{id}"}"#.into(),
            Some(record("id")),
            ValueType::builtin(P::McpResourceResult),
        ),
        (
            K::Prompt {
                name: "prompt".into(),
            },
            r#"{"name":"prompt","arguments":[{"name":"text","required":true}]}"#.into(),
            Some(record("text")),
            ValueType::builtin(P::McpPromptResult),
        ),
    ];
    for (kind, descriptor, input, output) in cases {
        let tool = matches!(kind, K::Tool { .. });
        let descriptor = JsonDocument::new(descriptor.as_bytes(), &limits).unwrap();
        let binding = McpBinding::new(server.clone(), descriptor.digest(), kind, &limits).unwrap();
        let binding_id = binding.digest(&limits).unwrap();
        let mut n = NodeFields::new(
            "operation".parse().unwrap(),
            Operation::Mcp {
                binding: binding_id,
                retry: RetryPolicy::no_retry(),
            },
        );
        n.outputs = PortTable::new(
            vec![("value".parse().unwrap(), Port::new(output.clone(), true))],
            &limits,
        )
        .unwrap();
        let mut root = ScopeFields {
            outputs: PortTable::new(
                vec![("result".parse().unwrap(), Port::new(output, true))],
                &limits,
            )
            .unwrap(),
            ..ScopeFields::default()
        };
        if let Some(input) = input {
            n.inputs = PortTable::new(
                vec![("arguments".parse().unwrap(), Port::new(input.clone(), true))],
                &limits,
            )
            .unwrap();
            root.inputs = PortTable::new(
                vec![("request".parse().unwrap(), Port::new(input, true))],
                &limits,
            )
            .unwrap();
            root.edges.push(Edge::new(
                "arguments".parse().unwrap(),
                S::Input("request".parse().unwrap()),
                D::Input {
                    node: "operation".parse().unwrap(),
                    port: "arguments".parse().unwrap(),
                },
            ));
        }
        root.nodes
            .push(Node::new(n, Role::Ordinary, &limits).unwrap());
        root.edges.push(Edge::new(
            "result".parse().unwrap(),
            S::Output {
                node: "operation".parse().unwrap(),
                port: "value".parse().unwrap(),
            },
            D::Output("result".parse().unwrap()),
        ));
        let root = Scope::new(root, Role::Ordinary, &limits).unwrap();
        let mut f = fields(root);
        f.bindings.insert(binding_id, binding);
        f.documents.insert(descriptor.digest(), descriptor);
        if tool {
            f.schema_uris.insert(
                embedded_schema_base(&schema, &limits).unwrap(),
                schema.digest(),
            );
            f.documents.insert(schema.digest(), schema.clone());
        }
        let verified = verify_executable(&bytes(f), &registry, &limits).unwrap();
        assert_eq!(verified.graphs().plans().len(), 1);
        assert_eq!(
            verified.evaluate(
                &ScopeUse::Root,
                &ExpressionSite::Eval("operation".parse().unwrap()),
                &EvaluationFrame::default()
            ),
            Err(EvaluationError::UnknownExpression)
        );
    }
}

#[test]
fn embedded_policy_cannot_select_itself_as_a_host_linked_policy() {
    let limits = Limits::default();
    let mut registry = NativeRegistry::new(&limits).unwrap();
    let bytes = bytes(fields(scope()));
    assert!(matches!(
        verify_executable(&bytes, &registry, &limits),
        Err(Error::Registry(NativeRegistryError::PolicyNotLinked))
    ));
    registry.link_policy(&policy()).unwrap();
    verify_executable(&bytes, &registry, &limits).unwrap();
}

#[test]
fn composed_verification_and_execution_on_controlled_stacks() {
    const CHILD: &str = "HTLK_VERIFIED_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(|| {
                let limits = Limits {
                    max_depth: 128,
                    ..Limits::default()
                };
                let mut p = policy().fields().clone();
                p.evaluator_limits.max_expression_depth = 128;
                let policy = PolicyDocument::new(p, &limits).unwrap();
                let mut registry = NativeRegistry::new(&limits).unwrap();
                let profile = registry.link_policy(&policy).unwrap();
                let mut encoded = b"\x82\x63not".repeat(100);
                encoded.extend_from_slice(b"\x82\x67literal\xf5");
                let expression = Expression::decode(&encoded, C::Eval, &limits).unwrap();
                let mut n =
                    NodeFields::new("compute".parse().unwrap(), Operation::Eval(expression));
                n.outputs = ports(&["value"]);
                let scope = Scope::new(
                    ScopeFields {
                        outputs: ports(&["result"]),
                        nodes: vec![Node::new(n, Role::Ordinary, &limits).unwrap()],
                        edges: vec![Edge::new(
                            "result".parse().unwrap(),
                            S::Output {
                                node: "compute".parse().unwrap(),
                                port: "value".parse().unwrap(),
                            },
                            D::Output("result".parse().unwrap()),
                        )],
                        ..ScopeFields::default()
                    },
                    Role::Ordinary,
                    &limits,
                )
                .unwrap();
                let root = scope.digest(Role::Ordinary, &limits).unwrap();
                let mut f = DocumentFields::new("depth".into(), profile, root);
                f.scopes.insert(root, scope);
                f.documents
                    .insert(policy.digest(), policy.document().clone());
                let bytes = CanonicalDocument::new(f, &limits)
                    .unwrap()
                    .envelope(&limits)
                    .unwrap()
                    .encode(&limits)
                    .unwrap();
                let verified = verify_executable(&bytes, &registry, &limits).unwrap();
                assert_eq!(
                    verified
                        .evaluate(
                            &ScopeUse::Root,
                            &ExpressionSite::Eval("compute".parse().unwrap()),
                            &EvaluationFrame::default()
                        )
                        .unwrap()
                        .value,
                    EvaluationValue::Present(Value::Bool(true))
                );
            })
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "composed_verification_and_execution_on_controlled_stacks",
                "--nocapture",
            ])
            .env(CHILD, size.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stack {size}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn independent_complete_fixture_round_trips_and_rejects_single_bit_mutations() {
    let mut lines = include_str!("fixtures/native-empty.hex").lines();
    let fingerprint: Digest = lines
        .next()
        .unwrap()
        .strip_prefix("fingerprint=")
        .unwrap()
        .parse()
        .unwrap();
    let root: Digest = lines
        .next()
        .unwrap()
        .strip_prefix("root_scope=")
        .unwrap()
        .parse()
        .unwrap();
    let hex: String = lines.collect();
    let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty());
    let bytes: Vec<u8> = pairs
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let limits = Limits::default();
    let registry = registry();
    let verified = verify_executable(&bytes, &registry, &limits).unwrap();
    assert_eq!(verified.fingerprint(), fingerprint);
    assert_eq!(verified.document().fields().root_scope, root);
    assert_eq!(
        verified
            .document()
            .envelope(&limits)
            .unwrap()
            .encode(&limits)
            .unwrap(),
        bytes
    );
    for index in 0..bytes.len() {
        for bit in 0..8 {
            let mut mutated = bytes.clone();
            mutated[index] ^= 1 << bit;
            assert!(
                verify_executable(&mutated, &registry, &limits).is_err(),
                "accepted mutation at byte {index}, bit {bit}"
            );
        }
    }
}

#[test]
fn canonical_construction_order_is_irrelevant_and_unused_documents_are_rejected() {
    let limits = Limits::default();
    let registry = registry();
    let first = scope();
    let mut reversed = first.fields().clone();
    reversed.edges.reverse();
    let second = Scope::new(reversed, Role::Ordinary, &limits).unwrap();
    let original = bytes(fields(first));
    assert_eq!(original, bytes(fields(second)));
    let mut extra = fields(scope());
    let unused = htlk_executable::JsonDocument::new(br#"{"unused":true}"#, &limits).unwrap();
    extra.documents.insert(unused.digest(), unused);
    assert!(
        matches!(verify_executable(&bytes(extra),&registry,&limits),Err(Error::Analysis(error)) if matches!(*error,htlk_analyzer::DocumentAnalysisError::Linkage(ref cause) if matches!(**cause,htlk_analyzer::LinkageError::UnreachableRecord(_))))
    );
}
