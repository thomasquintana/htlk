//! Focused semantic document linkage, structural analysis and offline schema stages.
use htlk_analyzer::{DocumentAnalysis, LinkageError as Error, LinkedDocument as Doc};
use htlk_cbor::{LimitKind, Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::digest::Digest;
use htlk_executable::{
    DocumentFields, EngineIdentity, EvaluatorLimits, ExecutableEnvelope, ExecutionLimits,
    ExecutionProfile, Expression, ExpressionContext, ExpressionKind, FunctionId, FunctionSignature,
    JsonDocument, Library, McpBinding, McpBindingKind, McpTransport, Node, NodeFields, Operation,
    PolicyDocument, PolicyFields, Port, PortTable, PrimitiveType, PromptTemplate, RetryPolicy,
    ScalarLiteral, Scope, ScopeContext as C, ScopeFields, ServerIdentity, ValueType,
};

fn d(n: u8) -> Digest {
    Digest::from_bytes([n; 32])
}
fn policy_fields() -> PolicyFields {
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
            max_value_bytes: 1024,
            max_collection_visits: 1000,
            max_regex_bytes: 1024,
            max_regex_compiled_bytes: 4096,
            max_output_bytes: 1024,
            max_steps: 10000,
        },
        maximum_scope_depth: 32,
        maximum_expanded_nodes: 1000,
    }
}
fn base(l: &Limits) -> DocumentFields {
    let policy = PolicyDocument::new(policy_fields(), l).unwrap();
    let engine = EngineIdentity::new("engine".into(), "1".into(), "1".into(), d(0), l).unwrap();
    let profile = ExecutionProfile::new(
        d(1),
        engine.clone(),
        engine.clone(),
        engine,
        policy.digest(),
        l,
    )
    .unwrap();
    let scope = Scope::new(ScopeFields::default(), C::Ordinary, l).unwrap();
    let mut f = DocumentFields::new(
        "test.entry".into(),
        profile,
        scope.digest(C::Ordinary, l).unwrap(),
    );
    f.scopes.insert(f.root_scope, scope);
    f.documents
        .insert(policy.digest(), policy.document().clone());
    f
}
fn replace(v: &Value, key: &str, replacement: Option<Value>) -> Value {
    let Value::Map(m) = v else { panic!() };
    let mut fields: Vec<_> = m
        .iter()
        .filter(|(k, _)| *k != key)
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect();
    if let Some(v) = replacement {
        fields.push((key.into(), v));
    }
    Value::Map(Map::try_from_entries(fields).unwrap())
}
fn ports(name: &str, l: &Limits) -> PortTable {
    PortTable::new(
        vec![(
            name.parse().unwrap(),
            Port::new(ValueType::primitive(PrimitiveType::String), true),
        )],
        l,
    )
    .unwrap()
}
fn root(f: &mut DocumentFields, fields: ScopeFields, l: &Limits) {
    f.scopes.remove(&f.root_scope);
    let scope = Scope::new(fields, C::Ordinary, l).unwrap();
    f.root_scope = scope.digest(C::Ordinary, l).unwrap();
    f.scopes.insert(f.root_scope, scope);
}
fn eval(f: &mut DocumentFields, kind: ExpressionKind, l: &Limits) {
    let e = Expression::new(kind, ExpressionContext::Eval, l).unwrap();
    let mut n = NodeFields::new("worker".parse().unwrap(), Operation::Eval(e));
    n.outputs = ports("value", l);
    root(
        f,
        ScopeFields {
            nodes: vec![Node::new(n, C::Ordinary, l).unwrap()],
            ..ScopeFields::default()
        },
        l,
    );
}

#[test]
fn roots_and_policy_references_are_resolved_by_analysis() {
    let l = Limits::default();
    let mut f = base(&l);
    f.root_scope = d(9);
    assert_eq!(Doc::new(f, &l).unwrap_err(), Error::MissingRecord("scope"));
    let mut f = base(&l);
    f.documents.clear();
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::MissingRecord("policy document")
    );
}

#[test]
fn scope_closure_roles_cycles_and_interfaces_are_checked() {
    let l = Limits::default();
    let mut f = base(&l);
    let child = Scope::new(
        ScopeFields {
            inputs: ports("arg", &l),
            ..ScopeFields::default()
        },
        C::Ordinary,
        &l,
    )
    .unwrap();
    let id = child.digest(C::Ordinary, &l).unwrap();
    f.scopes.insert(id, child);
    assert_eq!(
        Doc::new(f.clone(), &l).unwrap_err(),
        Error::UnreachableRecord("scope")
    );
    let mut n = NodeFields::new("task".parse().unwrap(), Operation::Scope(id));
    n.inputs = ports("arg", &l);
    root(
        &mut f,
        ScopeFields {
            nodes: vec![Node::new(n.clone(), C::Ordinary, &l).unwrap()],
            ..ScopeFields::default()
        },
        &l,
    );
    let doc = Doc::new(f.clone(), &l).unwrap();
    assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
    n.inputs = PortTable::default();
    root(
        &mut f,
        ScopeFields {
            nodes: vec![Node::new(n, C::Ordinary, &l).unwrap()],
            ..ScopeFields::default()
        },
        &l,
    );
    assert_eq!(Doc::new(f, &l).unwrap_err(), Error::ScopeInterfaceMismatch);
    // Fabricated cyclic keys fail the model's identity assertion before semantic
    // analysis. The analyzer's cycle walk is independently covered with symbolic keys.
    let mut f = base(&l);
    f.scopes.clear();
    f.root_scope = d(7);
    let n = Node::new(
        NodeFields::new("again".parse().unwrap(), Operation::Scope(d(7))),
        C::Ordinary,
        &l,
    )
    .unwrap();
    f.scopes.insert(
        d(7),
        Scope::new(
            ScopeFields {
                nodes: vec![n],
                ..ScopeFields::default()
            },
            C::Ordinary,
            &l,
        )
        .unwrap(),
    );
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::Representation(Box::new(htlk_executable::DocumentError::DigestMismatch(
            "scope"
        )))
    );
    let mut f = base(&l);
    let body = Scope::new(
        ScopeFields {
            inputs: ports("arg", &l),
            carried: ports("state", &l),
            ..ScopeFields::default()
        },
        C::LoopBody,
        &l,
    )
    .unwrap();
    let id = body.digest(C::LoopBody, &l).unwrap();
    f.scopes.insert(id, body);
    let mut n = NodeFields::new(
        "repeat".parse().unwrap(),
        Operation::Loop {
            body: id,
            initializers: vec![("state".parse().unwrap(), "arg".parse().unwrap())],
            until: Expression::literal(ScalarLiteral::Boolean(true)),
            max_iterations: 2,
        },
    );
    n.inputs = ports("arg", &l);
    root(
        &mut f,
        ScopeFields {
            nodes: vec![Node::new(n.clone(), C::Ordinary, &l).unwrap()],
            ..ScopeFields::default()
        },
        &l,
    );
    let doc = Doc::new(f.clone(), &l).unwrap();
    assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
    n.operation = Operation::Scope(id);
    root(
        &mut f,
        ScopeFields {
            nodes: vec![Node::new(n, C::Ordinary, &l).unwrap()],
            ..ScopeFields::default()
        },
        &l,
    );
    assert!(matches!(Doc::new(f, &l), Err(Error::Graph(_))));
}

#[test]
fn expressions_reach_templates_and_complete_library_manifests() {
    let l = Limits::default();
    let mut f = base(&l);
    let t = PromptTemplate::new(vec![], vec![], &l).unwrap();
    let td = t.digest(&l).unwrap();
    f.templates.insert(td, t);
    assert_eq!(
        Doc::new(f.clone(), &l).unwrap_err(),
        Error::UnreachableRecord("template")
    );
    eval(
        &mut f,
        ExpressionKind::Render {
            template: td,
            arguments: vec![],
        },
        &l,
    );
    assert!(Doc::new(f.clone(), &l).is_ok());
    eval(
        &mut f,
        ExpressionKind::Render {
            template: td,
            arguments: vec![(
                "extra".parse().unwrap(),
                Expression::literal(ScalarLiteral::Null),
            )],
        },
        &l,
    );
    assert_eq!(Doc::new(f, &l).unwrap_err(), Error::TemplateArguments);
    let mut f = base(&l);
    let sig = FunctionSignature::new(
        vec![],
        vec![],
        Port::new(ValueType::primitive(PrimitiveType::String), true),
        &l,
    )
    .unwrap();
    let lib = Library::new(
        "test".into(),
        "1".into(),
        d(3),
        vec![
            ("used".parse().unwrap(), sig.clone()),
            ("unused".parse().unwrap(), sig),
        ],
        &l,
    )
    .unwrap();
    f.libraries.insert(d(3), lib);
    assert_eq!(
        Doc::new(f.clone(), &l).unwrap_err(),
        Error::UnreachableRecord("library")
    );
    eval(
        &mut f,
        ExpressionKind::Call {
            function: FunctionId::Library {
                library: d(3),
                name: "used".parse().unwrap(),
            },
            arguments: vec![],
        },
        &l,
    );
    let doc = Doc::new(f.clone(), &l).unwrap();
    assert_eq!(doc.fields().libraries[&d(3)].functions().len(), 2);
    assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
    eval(
        &mut f,
        ExpressionKind::Call {
            function: FunctionId::Library {
                library: d(3),
                name: "used".parse().unwrap(),
            },
            arguments: vec![Expression::literal(ScalarLiteral::Null)],
        },
        &l,
    );
    assert_eq!(Doc::new(f, &l).unwrap_err(), Error::FunctionArity);
    let mut f = base(&l);
    eval(
        &mut f,
        ExpressionKind::FunctionRef {
            library: d(3),
            name: "used".parse().unwrap(),
        },
        &l,
    );
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::MissingRecord("library")
    );
}

#[test]
fn binding_and_schema_references_require_document_identities() {
    let l = Limits::default();
    let mut f = base(&l);
    let server = ServerIdentity::new(
        "prod".into(),
        McpTransport::Stdio,
        "server".into(),
        "1".into(),
        &l,
    )
    .unwrap();
    let json = JsonDocument::new(br#"{"type":"object"}"#, &l).unwrap();
    let descriptor = JsonDocument::new(
        br#"{"name":"tool","inputSchema":{"type":"object"},"outputSchema":{"type":"object"}}"#,
        &l,
    )
    .unwrap();
    let jd = json.digest();
    let binding = McpBinding::new(
        server,
        descriptor.digest(),
        McpBindingKind::Tool {
            name: "tool".into(),
            input_schema: jd,
            output_schema: jd,
        },
        &l,
    )
    .unwrap();
    let id = binding.digest(&l).unwrap();
    f.bindings.insert(id, binding);
    assert_eq!(
        Doc::new(f.clone(), &l).unwrap_err(),
        Error::UnreachableRecord("binding")
    );
    let retry = RetryPolicy::new(1, vec![], vec![], &l).unwrap();
    let mut n = NodeFields::new(
        "call".parse().unwrap(),
        Operation::Mcp { binding: id, retry },
    );
    n.inputs = schema_ports("arguments", jd, &l);
    n.outputs = schema_ports("value", jd, &l);
    let n = Node::new(n, C::Ordinary, &l).unwrap();
    root(
        &mut f,
        ScopeFields {
            nodes: vec![n],
            ..ScopeFields::default()
        },
        &l,
    );
    f.documents.insert(jd, json);
    assert_eq!(
        Doc::new(f.clone(), &l).unwrap_err(),
        Error::MissingRecord("descriptor document")
    );
    f.documents.insert(descriptor.digest(), descriptor);
    f.schema_uris
        .insert("https://example.test/schema".into(), jd);
    let doc = Doc::new(f.clone(), &l).unwrap();
    assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
    for uri in [
        "relative",
        "https://example.test/schema#",
        " https://example.test/schema",
        "https://example.test/%xz",
        "https://example.test/%",
        "https://example.test/a\\b",
    ] {
        let mut bad = f.clone();
        bad.schema_uris.clear();
        bad.schema_uris.insert(uri.into(), jd);
        assert_eq!(
            Doc::new(bad, &l).unwrap_err(),
            Error::Representation(Box::new(htlk_executable::DocumentError::InvalidSchemaUri))
        );
    }
    f.schema_uris.insert("urn:missing".into(), d(9));
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::MissingRecord("schema URI document")
    );
}

fn structural_policy(f: &mut DocumentFields, depth: u64, nodes: u64, l: &Limits) {
    let mut fields = policy_fields();
    fields.maximum_scope_depth = depth;
    fields.maximum_expanded_nodes = nodes;
    let policy = PolicyDocument::new(fields, l).unwrap();
    let p = &f.profile;
    let profile = ExecutionProfile::new(
        p.core_digest(),
        p.regex_engine().clone(),
        p.schema_validator().clone(),
        p.uri_template_engine().clone(),
        policy.digest(),
        l,
    )
    .unwrap();
    f.documents.remove(&p.policy_document());
    f.documents
        .insert(policy.digest(), policy.document().clone());
    f.profile = profile;
}

// Retain the previous root as a shared child definition. Each wrapper has the
// child's empty interface; loop bodies are role-neutral in these fixtures.
fn wrap(f: &mut DocumentFields, copies: usize, iterations: Option<u64>, l: &Limits) {
    let child = f.root_scope;
    let scope = f.scopes[&child].clone();
    let nodes = (0..copies)
        .map(|i| {
            let op = iterations.map_or(Operation::Scope(child), |max_iterations| Operation::Loop {
                body: child,
                max_iterations,
                initializers: vec![],
                until: Expression::literal(ScalarLiteral::Boolean(true)),
            });
            let mut fields = NodeFields::new(format!("use_{i}").parse().unwrap(), op);
            // All guarded branches and all bounded iterations count, even though
            // this guard is false and each loop's until is literal true.
            fields.guard = Expression::literal(ScalarLiteral::Boolean(false));
            Node::new(fields, C::Ordinary, l).unwrap()
        })
        .collect();
    root(
        f,
        ScopeFields {
            nodes,
            ..ScopeFields::default()
        },
        l,
    );
    f.scopes.insert(child, scope);
}

#[test]
fn structural_counts_include_root_depth_wrappers_reuse_and_loop_products() {
    let l = Limits::default();
    let mut f = base(&l);
    structural_policy(&mut f, 1, 1, &l);
    let empty = Doc::new(f.clone(), &l).unwrap().structural_summary();
    assert_eq!((empty.scope_depth(), empty.expanded_nodes()), (1, 0));
    eval(
        &mut f,
        ExpressionKind::Literal(ScalarLiteral::String("leaf".into())),
        &l,
    );
    let leaf = Doc::new(f.clone(), &l).unwrap().structural_summary();
    assert_eq!((leaf.scope_depth(), leaf.expanded_nodes()), (1, 1));
    structural_policy(&mut f, 4, 30, &l);
    wrap(&mut f, 2, None, &l); // Two uses: 2 * (wrapper + leaf) = 4.
    wrap(&mut f, 1, Some(3), &l); // One loop: 1 + 3 * 4 = 13.
    wrap(&mut f, 2, None, &l); // Shared loop: 2 * (1 + 13) = 28.
    let doc = Doc::new(f, &l).unwrap();
    assert_eq!(doc.fields().scopes.len(), 4);
    assert_eq!(doc.structural_summary().scope_depth(), 4);
    assert_eq!(doc.structural_summary().expanded_nodes(), 28);
    assert_eq!(
        Doc::decode(&doc.encode(&l).unwrap(), &l)
            .unwrap()
            .structural_summary(),
        doc.structural_summary()
    );
    let mut f = base(&l);
    eval(
        &mut f,
        ExpressionKind::Literal(ScalarLiteral::String("leaf".into())),
        &l,
    );
    wrap(&mut f, 1, Some(3), &l);
    wrap(&mut f, 1, Some(5), &l);
    let summary = Doc::new(f, &l).unwrap().structural_summary();
    // Outer wrapper + five occurrences of (inner wrapper + three leaves).
    assert_eq!((summary.scope_depth(), summary.expanded_nodes()), (3, 21));
}

#[test]
fn structural_policy_boundaries_apply_to_construction_and_canonical_ingress() {
    use htlk_analyzer::StructuralLimit as S;
    let l = Limits::default();
    let mut f = base(&l);
    eval(
        &mut f,
        ExpressionKind::Literal(ScalarLiteral::String("leaf".into())),
        &l,
    );
    wrap(&mut f, 2, Some(3), &l); // 2 loop wrappers + 2*3 leaf invocations = 8.
    structural_policy(&mut f, 2, 8, &l);
    let doc = Doc::new(f.clone(), &l).unwrap();
    assert_eq!(doc.structural_summary().expanded_nodes(), 8);
    for (depth, nodes, limit, maximum) in [(1, 8, S::ScopeDepth, 1), (2, 7, S::ExpandedNodes, 7)] {
        let mut bad = f.clone();
        structural_policy(&mut bad, depth, nodes, &l);
        let expected = Error::StructuralLimitExceeded { limit, maximum };
        assert_eq!(Doc::new(bad.clone(), &l).unwrap_err(), expected);
        // Produce canonical bytes with matching profile and policy identities,
        // bypassing authored construction to exercise ingress independently.
        let value = doc.to_value(&l).unwrap();
        let docs = Value::Map(
            Map::try_from_entries(
                bad.documents
                    .iter()
                    .map(|(d, j)| (d.to_string(), Value::Bytes(j.as_bytes().to_vec()))),
            )
            .unwrap(),
        );
        let value = replace(
            &replace(&value, "profile", Some(bad.profile.to_value(&l).unwrap())),
            "documents",
            Some(docs),
        );
        let bytes = htlk_cbor::encode(&value, &l).unwrap();
        assert_eq!(Doc::decode(&bytes, &l).unwrap_err(), expected);
        assert_eq!(
            Doc::from_envelope(&ExecutableEnvelope::new(bytes, &l).unwrap(), &l).unwrap_err(),
            expected
        );
    }
}

#[test]
fn structural_arithmetic_rejects_overflow_without_expanding_invocations() {
    use htlk_analyzer::StructuralLimit;
    let l = Limits::default();
    let mut f = base(&l);
    structural_policy(&mut f, 10, (1 << 53) - 1, &l);
    // Huge iteration count with an empty body only counts its one loop node.
    wrap(&mut f, 1, Some(i64::MAX as u64), &l);
    assert_eq!(
        Doc::new(f, &l)
            .unwrap()
            .structural_summary()
            .expanded_nodes(),
        1
    );
    let mut f = base(&l);
    structural_policy(&mut f, 10, (1 << 53) - 1, &l);
    eval(
        &mut f,
        ExpressionKind::Literal(ScalarLiteral::String("leaf".into())),
        &l,
    );
    wrap(&mut f, 2, None, &l); // Four invocations.
    wrap(&mut f, 1, Some(i64::MAX as u64), &l); // Multiplication overflows u64.
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::StructuralLimitExceeded {
            limit: StructuralLimit::ExpandedNodes,
            maximum: (1 << 53) - 1,
        }
    );
    let mut f = base(&l);
    structural_policy(&mut f, 3, (1 << 53) - 1, &l);
    eval(
        &mut f,
        ExpressionKind::Literal(ScalarLiteral::String("leaf".into())),
        &l,
    );
    wrap(&mut f, 2, Some(4_000_000_000_000_000), &l);
    wrap(&mut f, 1, Some(2), &l); // Child fits policy; parent exceeds it.
    assert!(matches!(
        Doc::new(f, &l),
        Err(Error::StructuralLimitExceeded {
            limit: StructuralLimit::ExpandedNodes,
            ..
        })
    ));
}

fn definition_depth_exercise() {
    let l = Limits::default();
    let mut f = base(&l);
    structural_policy(&mut f, 256, 255, &l);
    for _ in 0..255 {
        wrap(&mut f, 1, None, &l);
    }
    // Definition depth is independent of the CBOR nesting limit (default 64).
    let doc = Doc::new(f.clone(), &l).unwrap();
    assert_eq!(doc.structural_summary().scope_depth(), 256);
    assert_eq!(doc.structural_summary().expanded_nodes(), 255);
    assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
    structural_policy(&mut f, 255, 255, &l);
    assert!(matches!(
        Doc::new(f, &l),
        Err(Error::StructuralLimitExceeded {
            limit: htlk_analyzer::StructuralLimit::ScopeDepth,
            ..
        })
    ));
}
#[test]
fn structural_definitions_on_controlled_stacks() {
    const CHILD: &str = "HTLK_STRUCTURAL_DEPTH_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(definition_depth_exercise)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "structural_definitions_on_controlled_stacks",
                "--nocapture",
            ])
            .env(CHILD, size.to_string())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stack {size}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn schema_ports(name: &str, digest: Digest, l: &Limits) -> PortTable {
    let ty = ValueType::new(
        htlk_executable::ValueTypeKind::Schema(digest),
        htlk_executable::TypeContext::Value,
        l,
    )
    .unwrap();
    PortTable::new(vec![(name.parse().unwrap(), Port::new(ty, true))], l).unwrap()
}
fn select_binding(f: &mut DocumentFields, binding: McpBinding, l: &Limits) {
    let id = binding.digest(l).unwrap();
    let mut n = NodeFields::new(
        "call".parse().unwrap(),
        Operation::Mcp {
            binding: id,
            retry: RetryPolicy::no_retry(),
        },
    );
    if let McpBindingKind::Tool {
        input_schema,
        output_schema,
        ..
    } = binding.kind()
    {
        n.inputs = schema_ports("arguments", *input_schema, l);
        n.outputs = schema_ports("value", *output_schema, l);
    } else if matches!(binding.kind(), McpBindingKind::Resource { .. }) {
        n.outputs = primitive_ports("value", PrimitiveType::ResourceSnapshot, l);
    } else if matches!(binding.kind(), McpBindingKind::Prompt { .. }) {
        n.inputs = prompt_ports(&[], l);
        n.outputs = primitive_ports("value", PrimitiveType::McpPromptResult, l);
    } else if matches!(binding.kind(), McpBindingKind::Template { .. }) {
        n.inputs = prompt_ports(&[], l);
        n.outputs = primitive_ports("value", PrimitiveType::ResourceSnapshot, l);
    } else {
        n.outputs = ports("value", l);
    }
    root(
        f,
        ScopeFields {
            nodes: vec![Node::new(n, C::Ordinary, l).unwrap()],
            ..ScopeFields::default()
        },
        l,
    );
    f.bindings.clear();
    f.bindings.insert(id, binding);
}
fn tool_fixture(l: &Limits) -> DocumentFields {
    let mut f = base(l);
    let descriptor = JsonDocument::new(br#"{"name":"Tool/Exact","description":"Planning text","inputSchema":{"type":"object"},"outputSchema":{"type":"object","description":"result"},"extension":{"unchanged":true}}"#, l).unwrap();
    let input = JsonDocument::new(br#"{"type":"object"}"#, l).unwrap();
    let output = JsonDocument::new(br#"{"type":"object","description":"result"}"#, l).unwrap();
    let server = ServerIdentity::new(
        "prod".into(),
        McpTransport::Stdio,
        "server".into(),
        "1".into(),
        l,
    )
    .unwrap();
    let binding = McpBinding::new(
        server,
        descriptor.digest(),
        McpBindingKind::Tool {
            name: "Tool/Exact".into(),
            input_schema: input.digest(),
            output_schema: output.digest(),
        },
        l,
    )
    .unwrap();
    for j in [descriptor, input, output] {
        f.documents.insert(j.digest(), j);
    }
    select_binding(&mut f, binding, l);
    f
}
fn change_descriptor(f: &mut DocumentFields, replacement: Value, l: &Limits) {
    let inputs = f.scopes[&f.root_scope].fields().nodes[0]
        .fields()
        .inputs
        .clone();
    let old = f.bindings.values().next().unwrap().clone();
    let json = JsonDocument::from_value(&replacement, l).unwrap();
    let binding =
        McpBinding::new(old.server().clone(), json.digest(), old.kind().clone(), l).unwrap();
    f.documents.remove(&old.descriptor());
    f.documents.insert(json.digest(), json);
    select_binding(f, binding, l);
    set_prompt_inputs(f, inputs, l);
}
// Encode internally consistent table hashes without calling document assembly.
// This exercises canonical ingress as well as authored rejection.
fn expect_integrity_failure(f: DocumentFields, expected: Error, l: &Limits) {
    let mut value = Doc::new(base(l), l).unwrap().to_value(l).unwrap();
    for (key, table) in [
        (
            "libraries",
            Map::try_from_entries(
                f.libraries
                    .iter()
                    .map(|(d, b)| (d.to_string(), b.to_value(l).unwrap())),
            )
            .unwrap(),
        ),
        (
            "scopes",
            Map::try_from_entries(
                f.scopes
                    .iter()
                    .map(|(d, s)| (d.to_string(), s.to_value(C::Ordinary, l).unwrap())),
            )
            .unwrap(),
        ),
        (
            "bindings",
            Map::try_from_entries(
                f.bindings
                    .iter()
                    .map(|(d, b)| (d.to_string(), b.to_value(l).unwrap())),
            )
            .unwrap(),
        ),
        (
            "documents",
            Map::try_from_entries(
                f.documents
                    .iter()
                    .map(|(d, j)| (d.to_string(), Value::Bytes(j.as_bytes().to_vec()))),
            )
            .unwrap(),
        ),
    ] {
        value = replace(&value, key, Some(Value::Map(table)));
    }
    value = replace(
        &value,
        "root_scope",
        Some(Value::Text(f.root_scope.to_string())),
    );
    assert_eq!(Doc::new(f, l).unwrap_err(), expected);
    assert_eq!(
        Doc::decode(&htlk_cbor::encode(&value, l).unwrap(), l).unwrap_err(),
        expected
    );
}

#[test]
fn tool_descriptors_preserve_content_and_require_exact_extracted_schemas() {
    let l = Limits::default();
    let f = tool_fixture(&l);
    let doc = Doc::new(f.clone(), &l).unwrap();
    assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
    let binding = f.bindings.values().next().unwrap();
    let descriptor = f.documents[&binding.descriptor()].value();
    let changed = replace(
        descriptor,
        "description",
        Some(Value::Text("Different planning text".into())),
    );
    let mut other = f.clone();
    change_descriptor(&mut other, changed, &l);
    let other = Doc::new(other, &l).unwrap();
    assert_ne!(
        doc.envelope(&l).unwrap().fingerprint(),
        other.envelope(&l).unwrap().fingerprint()
    );
    for key in ["inputSchema", "outputSchema"] {
        let mut bad = f.clone();
        let mismatch =
            JsonDocument::new(br#"{"type":"object","description":"altered"}"#, &l).unwrap();
        change_descriptor(
            &mut bad,
            replace(descriptor, key, Some(mismatch.value().clone())),
            &l,
        );
        expect_integrity_failure(bad, Error::ToolSchemaMismatch(key), &l);
        for replacement in [
            None,
            Some(Value::Null),
            Some(Value::Bool(true)),
            Some(Value::Text("object".into())),
        ] {
            let mut bad = f.clone();
            change_descriptor(&mut bad, replace(descriptor, key, replacement), &l);
            expect_integrity_failure(bad, Error::InvalidDescriptor(key), &l);
        }
    }
}

#[test]
fn every_binding_selection_matches_its_exact_descriptor_field() {
    let l = Limits::default();
    for (kind, key, selected) in [
        (
            McpBindingKind::Resource {
                uri: "file:///Exact".into(),
            },
            "uri",
            "file:///Exact",
        ),
        (
            McpBindingKind::Template {
                uri_template: "https://example.test/{Name}".into(),
            },
            "uriTemplate",
            "https://example.test/{Name}",
        ),
        (
            McpBindingKind::Prompt {
                name: "Prompt/Exact".into(),
            },
            "name",
            "Prompt/Exact",
        ),
        (
            McpBindingKind::Tool {
                name: "Tool/Exact".into(),
                input_schema: d(0),
                output_schema: d(0),
            },
            "name",
            "Tool/Exact",
        ),
    ] {
        let mut f = tool_fixture(&l);
        let old = f.bindings.values().next().unwrap().clone();
        if !matches!(kind, McpBindingKind::Tool { .. }) {
            let value = Value::Map(
                Map::try_from_entries([(key.to_owned(), Value::Text(selected.into()))]).unwrap(),
            );
            let json = JsonDocument::from_value(&value, &l).unwrap();
            let binding = McpBinding::new(old.server().clone(), json.digest(), kind, &l).unwrap();
            f.documents.insert(json.digest(), json);
            select_binding(&mut f, binding, &l);
        }
        if matches!(
            f.bindings.values().next().unwrap().kind(),
            McpBindingKind::Template { .. }
        ) {
            set_prompt_inputs(&mut f, prompt_ports(&[("Name", true)], &l), &l);
        }
        assert!(Doc::new(f.clone(), &l).is_ok());
        let b = f.bindings.values().next().unwrap();
        let value = f.documents[&b.descriptor()].value().clone();
        for replacement in [
            None,
            Some(Value::Integer(1)),
            Some(Value::Text(selected.to_lowercase())),
            Some(Value::Text(format!(" {selected}"))),
        ] {
            let mut bad = f.clone();
            change_descriptor(&mut bad, replace(&value, key, replacement), &l);
            expect_integrity_failure(bad, Error::InvalidDescriptor(key), &l);
        }
        let mut bad = f;
        change_descriptor(&mut bad, Value::Array(vec![]), &l);
        expect_integrity_failure(bad, Error::InvalidDescriptor("object"), &l);
    }
}

#[test]
fn tool_ports_require_exact_schema_identity_and_required_presence() {
    let l = Limits::default();
    let f = tool_fixture(&l);
    let original = f.scopes[&f.root_scope].fields().nodes[0].fields().clone();
    for input in [
        PortTable::default(),
        ports("arguments", &l),
        original.outputs.clone(),
    ] {
        let mut n = original.clone();
        // Keep the locally valid arguments name even when selecting the output schema.
        n.inputs = if input.get("value").is_some() {
            PortTable::new(
                vec![(
                    "arguments".parse().unwrap(),
                    input.get("value").unwrap().clone(),
                )],
                &l,
            )
            .unwrap()
        } else {
            input
        };
        let mut bad = f.clone();
        root(
            &mut bad,
            ScopeFields {
                nodes: vec![Node::new(n, C::Ordinary, &l).unwrap()],
                ..ScopeFields::default()
            },
            &l,
        );
        expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
    }
    let mut n = original;
    n.outputs = ports("value", &l);
    let mut bad = f;
    root(
        &mut bad,
        ScopeFields {
            nodes: vec![Node::new(n, C::Ordinary, &l).unwrap()],
            ..ScopeFields::default()
        },
        &l,
    );
    expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
}

#[test]
fn nested_schema_types_must_name_reached_tool_roots() {
    use htlk_executable::{TypeContext, ValueTypeKind as K};
    let l = Limits::default();
    let mut f = base(&l);
    let arbitrary = JsonDocument::new(br#"{"type":"object"}"#, &l).unwrap();
    let id = arbitrary.digest();
    f.documents.insert(id, arbitrary);
    let schema = ValueType::new(K::Schema(id), TypeContext::Value, &l).unwrap();
    let nested = ValueType::new(K::List(Box::new(schema)), TypeContext::Value, &l).unwrap();
    let inputs = PortTable::new(
        vec![("nested".parse().unwrap(), Port::new(nested.clone(), false))],
        &l,
    )
    .unwrap();
    root(
        &mut f,
        ScopeFields {
            inputs,
            ..ScopeFields::default()
        },
        &l,
    );
    expect_integrity_failure(f, Error::UnreachedSchemaType, &l);
    let mut f = tool_fixture(&l);
    let mut fields = f.scopes[&f.root_scope].fields().clone();
    fields.inputs = PortTable::new(
        vec![("nested".parse().unwrap(), Port::new(nested, false))],
        &l,
    )
    .unwrap();
    root(&mut f, fields, &l);
    assert!(Doc::new(f, &l).is_ok());
}

#[test]
fn schema_root_checks_cover_unused_functions_in_reached_manifests() {
    use htlk_executable::{TypeContext, ValueTypeKind as K};
    let l = Limits::default();
    let mut f = tool_fixture(&l);
    let arbitrary =
        JsonDocument::new(br#"{"type":"object","title":"not a tool root"}"#, &l).unwrap();
    let ty = ValueType::new(K::Schema(arbitrary.digest()), TypeContext::Signature, &l).unwrap();
    let ty = ValueType::new(K::List(Box::new(ty)), TypeContext::Signature, &l).unwrap();
    f.documents.insert(arbitrary.digest(), arbitrary);
    let used = FunctionSignature::new(
        vec![],
        vec![],
        Port::new(ValueType::primitive(PrimitiveType::String), true),
        &l,
    )
    .unwrap();
    let unused = FunctionSignature::new(vec![], vec![], Port::new(ty, true), &l).unwrap();
    let library = Library::new(
        "lib".into(),
        "1".into(),
        d(8),
        vec![
            ("used".parse().unwrap(), used),
            ("unused".parse().unwrap(), unused),
        ],
        &l,
    )
    .unwrap();
    f.libraries.insert(d(8), library);
    let call = Expression::new(
        ExpressionKind::Call {
            function: FunctionId::Library {
                library: d(8),
                name: "used".parse().unwrap(),
            },
            arguments: vec![],
        },
        ExpressionContext::Eval,
        &l,
    )
    .unwrap();
    let mut node = NodeFields::new("worker".parse().unwrap(), Operation::Eval(call));
    node.outputs = ports("value", &l);
    let mut fields = f.scopes[&f.root_scope].fields().clone();
    fields.nodes.push(Node::new(node, C::Ordinary, &l).unwrap());
    root(&mut f, fields, &l);
    expect_integrity_failure(f, Error::UnreachedSchemaType, &l);
}

fn primitive_ports(name: &str, ty: PrimitiveType, l: &Limits) -> PortTable {
    PortTable::new(
        vec![(
            name.parse().unwrap(),
            Port::new(ValueType::primitive(ty), true),
        )],
        l,
    )
    .unwrap()
}
fn prompt_ports(fields: &[(&str, bool)], l: &Limits) -> PortTable {
    let fields = fields
        .iter()
        .map(|(n, r)| {
            (
                n.to_string(),
                Port::new(ValueType::primitive(PrimitiveType::String), *r),
            )
        })
        .collect();
    let ty = ValueType::new(
        htlk_executable::ValueTypeKind::Record(fields),
        htlk_executable::TypeContext::Value,
        l,
    )
    .unwrap();
    PortTable::new(vec![("arguments".parse().unwrap(), Port::new(ty, true))], l).unwrap()
}
fn prompt_fixture(json: &[u8], fields: &[(&str, bool)], l: &Limits) -> DocumentFields {
    let mut f = base(l);
    let descriptor = JsonDocument::new(json, l).unwrap();
    let server = ServerIdentity::new(
        "prod".into(),
        McpTransport::Stdio,
        "server".into(),
        "1".into(),
        l,
    )
    .unwrap();
    let binding = McpBinding::new(
        server,
        descriptor.digest(),
        McpBindingKind::Prompt {
            name: "prompt".into(),
        },
        l,
    )
    .unwrap();
    f.documents.insert(descriptor.digest(), descriptor);
    select_binding(&mut f, binding, l);
    set_prompt_inputs(&mut f, prompt_ports(fields, l), l);
    f
}
fn set_prompt_inputs(f: &mut DocumentFields, inputs: PortTable, l: &Limits) {
    let mut fields = f.scopes[&f.root_scope].fields().clone();
    let mut node = fields.nodes[0].fields().clone();
    node.inputs = inputs;
    fields.nodes[0] = Node::new(node, C::Ordinary, l).unwrap();
    root(f, fields, l);
}

#[test]
fn prompts_match_exact_external_argument_names_and_presence() {
    let l = Limits::default();
    let json = br#"{"name":"prompt","arguments":[{"name":"Z.External","required":true,"description":"Keep exactly"},{"name":"optional"},{"name":"","required":false}]}"#;
    let expected = [("Z.External", true), ("optional", false), ("", false)];
    let f = prompt_fixture(json, &expected, &l);
    let doc = Doc::new(f.clone(), &l).unwrap();
    assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
    for fields in [
        vec![],
        vec![("Z.External", false), ("optional", false), ("", false)],
        vec![("z.external", true), ("optional", false), ("", false)],
        vec![("Z.External", true), ("optional", true), ("", false)],
    ] {
        let mut bad = f.clone();
        set_prompt_inputs(&mut bad, prompt_ports(&fields, &l), &l);
        expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
    }
    let mut bad = f;
    set_prompt_inputs(
        &mut bad,
        primitive_ports("arguments", PrimitiveType::Json, &l),
        &l,
    );
    expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
    for json in [
        br#"{"name":"prompt"}"#.as_slice(),
        br#"{"name":"prompt","arguments":[]}"#.as_slice(),
    ] {
        let f = prompt_fixture(json, &[], &l);
        assert!(Doc::new(f.clone(), &l).is_ok());
        let mut bad = f;
        set_prompt_inputs(&mut bad, PortTable::default(), &l);
        expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
    }
}

#[test]
fn prompt_argument_metadata_rejects_malformed_or_duplicate_declarations() {
    let l = Limits::default();
    for (json, fields, error) in [
        (r#"{"name":"prompt","arguments":null}"#, vec![], "arguments"),
        (
            r#"{"name":"prompt","arguments":[null]}"#,
            vec![("x", false)],
            "prompt argument",
        ),
        (
            r#"{"name":"prompt","arguments":[{}]}"#,
            vec![("x", false)],
            "argument name",
        ),
        (
            r#"{"name":"prompt","arguments":[{"name":"x","required":1}]}"#,
            vec![("x", false)],
            "argument required",
        ),
        (
            r#"{"name":"prompt","arguments":[{"name":"x","description":false}]}"#,
            vec![("x", false)],
            "argument description",
        ),
        (
            r#"{"name":"prompt","arguments":[{"name":"x"},{"name":"x"}]}"#,
            vec![("x", false), ("y", false)],
            "duplicate argument",
        ),
    ] {
        let f = prompt_fixture(json.as_bytes(), &fields, &l);
        expect_integrity_failure(f, Error::InvalidDescriptor(error), &l);
    }
}

#[test]
fn fixed_resource_and_prompt_outputs_have_exact_protocol_types() {
    let l = Limits::default();
    for resource in [false, true] {
        let mut f = prompt_fixture(br#"{"name":"prompt"}"#, &[], &l);
        if resource {
            let old = f.bindings.values().next().unwrap().clone();
            let json = JsonDocument::new(br#"{"uri":"file:///resource"}"#, &l).unwrap();
            let binding = McpBinding::new(
                old.server().clone(),
                json.digest(),
                McpBindingKind::Resource {
                    uri: "file:///resource".into(),
                },
                &l,
            )
            .unwrap();
            f.documents.insert(json.digest(), json);
            select_binding(&mut f, binding, &l);
            assert!(Doc::new(f.clone(), &l).is_ok());
            let mut bad = f.clone();
            set_prompt_inputs(
                &mut bad,
                primitive_ports("arguments", PrimitiveType::Json, &l),
                &l,
            );
            expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
        }
        let mut fields = f.scopes[&f.root_scope].fields().clone();
        let mut node = fields.nodes[0].fields().clone();
        node.outputs = ports("value", &l);
        fields.nodes[0] = Node::new(node, C::Ordinary, &l).unwrap();
        root(&mut f, fields, &l);
        expect_integrity_failure(f, Error::McpInterfaceMismatch, &l);
    }
}

fn template_fixture(template: &str, fields: &[(&str, bool)], l: &Limits) -> DocumentFields {
    let mut f = prompt_fixture(br#"{"name":"prompt"}"#, &[], l);
    let server = f.bindings.values().next().unwrap().server().clone();
    let value = Value::Map(
        Map::try_from_entries([("uriTemplate".into(), Value::Text(template.into()))]).unwrap(),
    );
    let descriptor = JsonDocument::from_value(&value, l).unwrap();
    let binding = McpBinding::new(
        server,
        descriptor.digest(),
        McpBindingKind::Template {
            uri_template: template.into(),
        },
        l,
    )
    .unwrap();
    f.documents.insert(descriptor.digest(), descriptor);
    select_binding(&mut f, binding, l);
    set_prompt_inputs(&mut f, prompt_ports(fields, l), l);
    f
}

#[test]
fn resource_templates_require_exact_distinct_variables_with_level_four_syntax() {
    let l = Limits::default();
    for op in ["", "+", "#", ".", "/", ";", "?", "&"] {
        let template = format!("https://例.test/{{{op}Name:3,other*}}/{{Name}}");
        let f = template_fixture(&template, &[("Name", true), ("other", true)], &l);
        let doc = Doc::new(f, &l).unwrap();
        assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
    }
    let template = "{X.part,%61,a,%6A,%6a}/{X.part:9999}";
    let fields = [
        ("X.part", true),
        ("%61", true),
        ("a", true),
        ("%6A", true),
        ("%6a", true),
    ];
    let f = template_fixture(template, &fields, &l);
    assert!(Doc::new(f.clone(), &l).is_ok());
    for fields in [
        vec![],
        vec![
            ("X.part", false),
            ("%61", true),
            ("a", true),
            ("%6A", true),
            ("%6a", true),
        ],
        vec![("X.part", true), ("a", true), ("j", true)],
    ] {
        let mut bad = f.clone();
        set_prompt_inputs(&mut bad, prompt_ports(&fields, &l), &l);
        expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
    }
    let mut bad = f.clone();
    set_prompt_inputs(
        &mut bad,
        primitive_ports("arguments", PrimitiveType::Json, &l),
        &l,
    );
    expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
    let mut fields = f.scopes[&f.root_scope].fields().clone();
    let mut node = fields.nodes[0].fields().clone();
    node.outputs = ports("value", &l);
    fields.nodes[0] = Node::new(node, C::Ordinary, &l).unwrap();
    let mut bad = f;
    root(&mut bad, fields, &l);
    expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
    let f = template_fixture("relative/path", &[], &l);
    assert!(Doc::new(f.clone(), &l).is_ok());
    let mut bad = f;
    set_prompt_inputs(&mut bad, PortTable::default(), &l);
    expect_integrity_failure(bad, Error::McpInterfaceMismatch, &l);
}

#[test]
fn malformed_resource_templates_fail_canonical_ingress_with_byte_offsets() {
    let l = Limits::default();
    for (template, offset) in [
        ("é/{x:0}", 4),
        ("{!x}", 1),
        ("abc%xx", 3),
        ("{}", 1),
        ("{x..y}", 1),
        ("a{", 1),
    ] {
        let f = template_fixture(template, &[], &l);
        expect_integrity_failure(f, Error::InvalidUriTemplate { offset }, &l);
    }
}

#[test]
fn repeated_template_variables_obey_tighter_per_call_limits() {
    let l = Limits::default();
    let f = template_fixture(&"{x}".repeat(1000), &[("x", true)], &l);
    let doc = Doc::new(f.clone(), &l).unwrap();
    let bytes = doc.encode(&l).unwrap();
    let tight = Limits {
        max_total_values: 500,
        ..l
    };
    // Raw CBOR and each JSON document fit; derived template mentions do not.
    assert!(htlk_cbor::decode(&bytes, &tight).is_ok());
    let expected = Error::LimitExceeded {
        limit: LimitKind::TotalValues,
        maximum: 500,
    };
    assert_eq!(Doc::new(f, &tight).unwrap_err(), expected);
    assert_eq!(doc.encode(&tight).unwrap_err(), expected);
    assert_eq!(Doc::decode(&bytes, &tight).unwrap_err(), expected);
}

fn schema_root_uris(f: &mut DocumentFields, l: &Limits) {
    for binding in f.bindings.values() {
        if let McpBindingKind::Tool {
            input_schema,
            output_schema,
            ..
        } = binding.kind()
        {
            for id in [input_schema, output_schema] {
                let uri = htlk_analyzer::embedded_schema_base(&f.documents[id], l).unwrap();
                f.schema_uris.insert(uri, *id);
            }
        }
    }
}
fn tool_schemas(input: JsonDocument, output: JsonDocument, l: &Limits) -> DocumentFields {
    let mut f = tool_fixture(l);
    let old = f.bindings.values().next().unwrap().clone();
    let value = f.documents[&old.descriptor()].value();
    let descriptor = JsonDocument::from_value(
        &replace(
            &replace(value, "inputSchema", Some(input.value().clone())),
            "outputSchema",
            Some(output.value().clone()),
        ),
        l,
    )
    .unwrap();
    if let McpBindingKind::Tool {
        input_schema,
        output_schema,
        ..
    } = old.kind()
    {
        f.documents.remove(input_schema);
        f.documents.remove(output_schema);
    }
    f.documents.remove(&old.descriptor());
    let binding = McpBinding::new(
        old.server().clone(),
        descriptor.digest(),
        McpBindingKind::Tool {
            name: "Tool/Exact".into(),
            input_schema: input.digest(),
            output_schema: output.digest(),
        },
        l,
    )
    .unwrap();
    for doc in [input, output, descriptor] {
        f.documents.insert(doc.digest(), doc);
    }
    select_binding(&mut f, binding, l);
    schema_root_uris(&mut f, l);
    f
}

#[test]
fn executable_schema_catalog_requires_prescribed_roots_and_rejects_unused_entries() {
    let l = Limits::default();
    assert!(
        Doc::new(base(&l), &l)
            .unwrap()
            .schema_catalog(&l)
            .unwrap()
            .retrieval_uris()
            .next()
            .is_none()
    );
    let mut f = tool_fixture(&l);
    assert_eq!(
        Doc::new(f.clone(), &l)
            .unwrap()
            .schema_catalog(&l)
            .unwrap_err(),
        Error::SchemaRootMismatch
    );
    schema_root_uris(&mut f, &l);
    let doc = Doc::new(f.clone(), &l).unwrap();
    let catalog = doc.schema_catalog(&l).unwrap();
    assert_eq!(catalog.retrieval_uris().count(), 2);
    let decoded = Doc::from_envelope(&doc.envelope(&l).unwrap(), &l).unwrap();
    assert_eq!(decoded.schema_catalog(&l).unwrap(), catalog);
    let mut wrong = f.clone();
    let key = wrong.schema_uris.keys().next().unwrap().clone();
    wrong
        .schema_uris
        .insert(key, wrong.profile.policy_document());
    assert_eq!(
        Doc::new(wrong, &l).unwrap().schema_catalog(&l).unwrap_err(),
        Error::SchemaRootMismatch
    );
    let mut extra = f.clone();
    let id = *extra.schema_uris.values().next().unwrap();
    extra
        .schema_uris
        .insert("https://unused.test/alias".into(), id);
    assert_eq!(
        Doc::new(extra, &l).unwrap().schema_catalog(&l).unwrap_err(),
        Error::UnreachableRecord("schema URI")
    );
    let mut extra = f;
    let json = JsonDocument::new(br#"{"extra":"document"}"#, &l).unwrap();
    extra.documents.insert(json.digest(), json);
    assert_eq!(
        Doc::new(extra, &l).unwrap().schema_catalog(&l).unwrap_err(),
        Error::UnreachableRecord("JSON document")
    );
}

#[test]
fn executable_schema_dependencies_use_declared_bases_not_catalog_aliases() {
    let l = Limits::default();
    let input = JsonDocument::new(
        br#"{"$id":"https://e.test/root","type":"object","$ref":"dep"}"#,
        &l,
    )
    .unwrap();
    let output = JsonDocument::new(br#"{"type":"object","description":"output"}"#, &l).unwrap();
    let mut f = tool_schemas(input, output.clone(), &l);
    assert!(matches!(
        Doc::new(f.clone(), &l).unwrap().schema_catalog(&l),
        Err(Error::Schema(
            htlk_analyzer::SchemaResourceError::MissingResource
        ))
    ));
    let dep = JsonDocument::new(br#"{"type":"object"}"#, &l).unwrap();
    f.schema_uris
        .insert("https://e.test/dep".into(), dep.digest());
    f.documents.insert(dep.digest(), dep.clone());
    let catalog = Doc::new(f, &l).unwrap().schema_catalog(&l).unwrap();
    assert_eq!(catalog.retrieval_uris().count(), 3);
    let input = JsonDocument::new(br#"{"type":"object","$ref":"dep"}"#, &l).unwrap();
    let id = input.digest();
    let mut f = tool_schemas(input, output, &l);
    // Supplying an HTTP alias cannot replace the required synthetic embedded base.
    f.schema_uris.insert("https://e.test/root".into(), id);
    f.schema_uris
        .insert("https://e.test/dep".into(), dep.digest());
    f.documents.insert(dep.digest(), dep);
    assert!(matches!(
        Doc::new(f, &l).unwrap().schema_catalog(&l),
        Err(Error::Schema(
            htlk_analyzer::SchemaResourceError::NonHierarchicalBase
        ))
    ));
}

#[test]
fn executable_schema_snapshot_copies_are_preflighted_before_catalog_construction() {
    let l = Limits::default();
    let input = JsonDocument::new(
        format!(
            r#"{{"type":"object","description":"{}"}}"#,
            "x".repeat(2000)
        )
        .as_bytes(),
        &l,
    )
    .unwrap();
    let id = input.digest();
    let output = JsonDocument::new(br#"{"type":"object"}"#, &l).unwrap();
    let mut f = tool_schemas(input, output, &l);
    for i in 0..20 {
        f.schema_uris.insert(format!("https://copy.test/{i}"), id);
    }
    let doc = Doc::new(f, &l).unwrap();
    let tight = Limits {
        max_total_payload_bytes: 10000,
        ..l
    };
    assert!(doc.to_value(&tight).is_ok());
    assert_eq!(
        doc.schema_catalog(&tight).unwrap_err(),
        Error::LimitExceeded {
            limit: LimitKind::TotalPayloadBytes,
            maximum: 10000
        }
    );
}

#[test]
fn executable_native_schemas_enforce_callable_roots_and_validate_instances() {
    use htlk_analyzer::{NativeSchemaError, NativeSchemaOptions};
    let l = Limits::default();
    let input =
        JsonDocument::new(br#"{"$id":"https://e.test/input","$ref":"object"}"#, &l).unwrap();
    let output = JsonDocument::new(br#"{"type":"object"}"#, &l).unwrap();
    let mut f = tool_schemas(input, output.clone(), &l);
    f.schema_uris
        .insert("https://e.test/object".into(), output.digest());
    let doc = Doc::new(f, &l).unwrap();
    let native = doc
        .native_schemas(NativeSchemaOptions::default(), &l)
        .unwrap();
    assert!(
        native
            .validate(
                "https://e.test/input",
                &JsonDocument::new(b"{}", &l).unwrap(),
                &l
            )
            .unwrap()
    );
    assert!(
        !native
            .validate(
                "https://e.test/input",
                &JsonDocument::new(b"[]", &l).unwrap(),
                &l
            )
            .unwrap()
    );
    let invalid_root = JsonDocument::new(br#"{"type":"array"}"#, &l).unwrap();
    let f = tool_schemas(invalid_root, output, &l);
    assert!(matches!(
        Doc::new(f, &l)
            .unwrap()
            .native_schemas(NativeSchemaOptions::default(), &l),
        Err(Error::NativeSchema(NativeSchemaError::ObjectRootRequired))
    ));
}

#[test]
fn executable_mcp_stage_rejects_conflicting_pinned_selections() {
    let l = Limits::default();
    let mut f = tool_fixture(&l);
    Doc::new(f.clone(), &l)
        .unwrap()
        .validate_mcp_descriptors(&l)
        .unwrap();
    let old = f.bindings.values().next().unwrap().clone();
    let descriptor = JsonDocument::from_value(
        &replace(
            f.documents[&old.descriptor()].value(),
            "description",
            Some(Value::Text("different descriptor".into())),
        ),
        &l,
    )
    .unwrap();
    let binding = McpBinding::new(
        old.server().clone(),
        descriptor.digest(),
        old.kind().clone(),
        &l,
    )
    .unwrap();
    let id = binding.digest(&l).unwrap();
    f.documents.insert(descriptor.digest(), descriptor);
    f.bindings.insert(id, binding);
    let mut fields = f.scopes[&f.root_scope].fields().clone();
    let mut second = fields.nodes[0].fields().clone();
    second.id = "second".parse().unwrap();
    second.operation = Operation::Mcp {
        binding: id,
        retry: RetryPolicy::no_retry(),
    };
    fields
        .nodes
        .push(Node::new(second, C::Ordinary, &l).unwrap());
    root(&mut f, fields, &l);
    assert_eq!(
        Doc::new(f, &l)
            .unwrap()
            .validate_mcp_descriptors(&l)
            .unwrap_err(),
        Error::ConflictingMcpSelection
    );
}
