//! Policy shape and document-level identity/reference boundaries.
use htlk_cbor::{LimitKind, Limits, Map, Value};
use htlk_executable::digest::Digest;
use htlk_executable::{
    CanonicalDocument as Doc, DocumentError as Error, DocumentFields, EngineIdentity,
    EvaluatorLimits, ExecutableEnvelope, ExecutionLimits, ExecutionProfile, Expression,
    ExpressionContext, ExpressionKind, FunctionId, FunctionSignature, JsonDocument, Library,
    McpBinding, McpBindingKind, McpTransport, Node, NodeFields, Operation, PolicyDocument,
    PolicyError, PolicyFields, Port, PortTable, PrimitiveType, PromptTemplate, RetryPolicy,
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
fn policy_is_exact_jcs_with_closed_positive_fields_and_required_defaults() {
    let l = Limits::default();
    let p = PolicyDocument::new(policy_fields(), &l).unwrap();
    let golden = br#"{"cost_unit":"credits","defaults":{"attempt_timeout_ms":100,"max_concurrency":1,"timeout_ms":1000},"evaluator_limits":{"max_collection_visits":1000,"max_expression_depth":64,"max_output_bytes":1024,"max_regex_bytes":1024,"max_regex_compiled_bytes":4096,"max_steps":10000,"max_value_bytes":1024},"maximum_expanded_nodes":1000,"maximum_scope_depth":32}"#;
    assert_eq!(p.document().as_bytes(), golden);
    assert_eq!(PolicyDocument::decode(golden, &l).unwrap(), p);
    assert_eq!(p.fields(), &policy_fields());
    let mut incomplete = policy_fields();
    incomplete.defaults = ExecutionLimits::new();
    assert_eq!(
        PolicyDocument::new(incomplete, &l).unwrap_err(),
        PolicyError::IncompleteDefaults
    );
    for n in [0, (1 << 53), u64::MAX] {
        let mut bad = policy_fields();
        bad.evaluator_limits.max_steps = n;
        assert!(PolicyDocument::new(bad, &l).is_err());
    }
    let value = p.document().value();
    for bad in [
        replace(value, "cost_unit", None),
        replace(value, "extra", Some(Value::Null)),
        replace(value, "maximum_scope_depth", Some(Value::Integer(0))),
        replace(value, "maximum_scope_depth", Some(Value::Text("32".into()))),
        replace(value, "defaults", Some(Value::Map(Map::new()))),
    ] {
        let json = JsonDocument::from_value(&bad, &l).unwrap();
        assert!(PolicyDocument::from_document(json, &l).is_err());
    }
}

#[test]
fn document_and_envelope_round_trip_with_strict_field_shapes() {
    let l = Limits::default();
    let doc = Doc::new(base(&l), &l).unwrap();
    assert_eq!(doc.ir_version(), "0.1");
    let bytes = doc.encode(&l).unwrap();
    assert_eq!(bytes[0], 0xaa);
    assert_eq!(Doc::decode(&bytes, &l).unwrap(), doc);
    let envelope = doc.envelope(&l).unwrap();
    assert_eq!(envelope.payload(), bytes);
    assert_eq!(
        Doc::from_envelope(
            &ExecutableEnvelope::decode(&envelope.encode(&l).unwrap(), &l).unwrap(),
            &l
        )
        .unwrap(),
        doc
    );
    assert!(Doc::from_envelope(&ExecutableEnvelope::new(vec![], &l).unwrap(), &l).is_err());
    let value = doc.to_value(&l).unwrap();
    let Value::Map(m) = &value else { panic!() };
    for (key, _) in m.iter() {
        assert!(matches!(
            Doc::from_value(&replace(&value, key, None), &l),
            Err(Error::MissingField(_))
        ));
    }
    assert_eq!(
        Doc::from_value(&replace(&value, "extra", Some(Value::Null)), &l).unwrap_err(),
        Error::UnknownField
    );
    assert_eq!(
        Doc::from_value(
            &replace(&value, "ir_version", Some(Value::Text("1".into()))),
            &l
        )
        .unwrap_err(),
        Error::UnsupportedVersion
    );
    for name in ["", "Test", "a..b", "a.", "a-b"] {
        let mut f = base(&l);
        f.graph_id = name.into();
        assert_eq!(Doc::new(f, &l).unwrap_err(), Error::InvalidGraphId);
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(Doc::decode(&trailing, &l).is_err());
}

#[test]
fn roots_table_keys_and_policy_bytes_are_assertions() {
    let l = Limits::default();
    let mut f = base(&l);
    f.root_scope = d(9);
    assert_eq!(Doc::new(f, &l).unwrap_err(), Error::MissingRecord("scope"));
    let mut f = base(&l);
    let scope = f.scopes.remove(&f.root_scope).unwrap();
    f.root_scope = d(9);
    f.scopes.insert(d(9), scope);
    assert_eq!(Doc::new(f, &l).unwrap_err(), Error::DigestMismatch("scope"));
    let mut f = base(&l);
    f.documents.clear();
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::MissingRecord("policy document")
    );
    let mut f = base(&l);
    f.documents
        .insert(d(9), JsonDocument::new(b"{}", &l).unwrap());
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::DigestMismatch("JSON document")
    );
    let doc = Doc::new(base(&l), &l).unwrap();
    let value = doc.to_value(&l).unwrap();
    for bytes in [
        b" {} ".to_vec(),
        b"{\"x\":1,\"x\":2}".to_vec(),
        b"null".to_vec(),
    ] {
        let docs = Value::Map(
            Map::try_from_entries([(
                doc.fields().profile.policy_document().to_string(),
                Value::Bytes(bytes),
            )])
            .unwrap(),
        );
        assert!(Doc::from_value(&replace(&value, "documents", Some(docs)), &l).is_err());
    }
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
    // Cyclic definition keys cannot be valid content digests. Detect the cycle
    // before attempting digest equality, with bounded iterative traversal.
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
    assert_eq!(Doc::new(f, &l).unwrap_err(), Error::ScopeCycle);
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
    let json = JsonDocument::new(b"{}", &l).unwrap();
    let jd = json.digest();
    let binding = McpBinding::new(
        server,
        jd,
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
    n.outputs = ports("value", &l);
    let n = Node::new(n, C::Ordinary, &l).unwrap();
    root(
        &mut f,
        ScopeFields {
            nodes: vec![n],
            ..ScopeFields::default()
        },
        &l,
    );
    assert_eq!(
        Doc::new(f.clone(), &l).unwrap_err(),
        Error::MissingRecord("descriptor document")
    );
    f.documents.insert(jd, json);
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
        assert_eq!(Doc::new(bad, &l).unwrap_err(), Error::InvalidSchemaUri);
    }
    f.schema_uris.insert("urn:missing".into(), d(9));
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::MissingRecord("schema URI document")
    );
}

#[test]
fn aggregate_limits_and_embedded_json_limits_are_rechecked() {
    let l = Limits::default();
    let doc = Doc::new(base(&l), &l).unwrap();
    let bytes = doc.encode(&l).unwrap();
    let exact = Limits {
        max_document_bytes: bytes.len(),
        ..l.clone()
    };
    assert_eq!(doc.encode(&exact).unwrap(), bytes);
    let small = Limits {
        max_document_bytes: bytes.len() - 1,
        ..l.clone()
    };
    assert!(matches!(
        doc.encode(&small),
        Err(Error::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
    assert!(Doc::decode(&bytes, &small).is_err());
    let mut f = base(&l);
    let large = JsonDocument::new(format!("\"{}\"", "x".repeat(100)).as_bytes(), &l).unwrap();
    f.documents.insert(large.digest(), large);
    let doc = Doc::new(f, &l).unwrap();
    let small = Limits {
        max_text_bytes: 99,
        ..l
    };
    assert!(matches!(doc.encode(&small), Err(Error::Json(_))));
    assert!(matches!(
        Doc::decode(&doc.encode(&Limits::default()).unwrap(), &small),
        Err(Error::Json(_))
    ));
}

fn depth_exercise() {
    let l = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    let mut f = base(&l);
    // Document -> scopes -> scope -> preconditions -> not... -> literal scalar.
    let mut bytes = b"\x82\x63not".repeat(124);
    bytes.extend_from_slice(b"\x82\x67literal\xf5");
    let pre = Expression::decode(&bytes, ExpressionContext::Preconditions, &l).unwrap();
    root(
        &mut f,
        ScopeFields {
            preconditions: pre,
            ..ScopeFields::default()
        },
        &l,
    );
    let doc = Doc::new(f, &l).unwrap();
    let bytes = doc.encode(&l).unwrap();
    assert_eq!(Doc::decode(&bytes, &l).unwrap(), doc);
    assert_eq!(doc.clone(), doc);
    let smaller = Limits {
        max_depth: 127,
        ..l
    };
    assert!(doc.encode(&smaller).is_err());
    assert!(Doc::decode(&bytes, &smaller).is_err());
}
#[test]
fn document_on_controlled_stacks() {
    const CHILD: &str = "HTLK_DOCUMENT_DEPTH_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(depth_exercise)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "document_on_controlled_stacks", "--nocapture"])
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
