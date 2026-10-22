//! Bounded structural mutation corpus for every public canonical wire-record family.
use htlk_executable::{
    cbor::{self, Limits, Map, Value},
    digest::Digest,
    *,
};

#[derive(Clone, Copy, Debug)]
enum Kind {
    Type,
    Port,
    Expression,
    Template,
    Limits,
    Retry,
    Engine,
    Profile,
    Server,
    Binding,
    Signature,
    Library,
    Node,
    Edge,
    Scope,
    Document,
}
fn parse(kind: Kind, value: &Value, l: &Limits) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(match kind {
        Kind::Type => ValueType::from_value(value, TypeContext::Signature, l)?
            .to_value(TypeContext::Signature, l)?,
        Kind::Port => Port::from_value(value, TypeContext::Signature, l)?
            .to_value(TypeContext::Signature, l)?,
        Kind::Expression => Expression::from_value(value, l)?.to_value(l)?,
        Kind::Template => PromptTemplate::from_value(value, l)?.to_value(l)?,
        Kind::Limits => ExecutionLimits::from_value(value, l)?.to_value(l)?,
        Kind::Retry => RetryPolicy::from_value(value, l)?.to_value(l)?,
        Kind::Engine => EngineIdentity::from_value(value, l)?.to_value(l)?,
        Kind::Profile => ExecutionProfile::from_value(value, l)?.to_value(l)?,
        Kind::Server => ServerIdentity::from_value(value, l)?.to_value(l)?,
        Kind::Binding => McpBinding::from_value(value, l)?.to_value(l)?,
        Kind::Signature => FunctionSignature::from_value(value, l)?.to_value(l)?,
        Kind::Library => Library::from_value(value, l)?.to_value(l)?,
        Kind::Node => Node::from_value(value, l)?.to_value(l)?,
        Kind::Edge => Edge::from_value(value, l)?.to_value(l)?,
        Kind::Scope => Scope::from_value(value, l)?.to_value(l)?,
        Kind::Document => CanonicalDocument::from_value(value, l)?.to_value(l)?,
    })
}
fn mutations(value: &Value, depth: usize) -> Vec<Value> {
    let mut out = vec![
        Value::Null,
        Value::Bool(false),
        Value::Integer(-1),
        Value::Text("invalid".into()),
        Value::Bytes(vec![0]),
        Value::Array(vec![]),
        Value::Map(Map::new()),
    ];
    if depth == 0 {
        return out;
    }
    match value {
        Value::Array(items) => {
            let mut reversed = items.clone();
            reversed.reverse();
            out.push(Value::Array(reversed));
            for (index, child) in items.iter().enumerate() {
                let mut missing = items.clone();
                missing.remove(index);
                out.push(Value::Array(missing));
                for replacement in mutations(child, depth - 1) {
                    let mut changed = items.clone();
                    changed[index] = replacement;
                    out.push(Value::Array(changed));
                }
            }
        }
        Value::Map(fields) => {
            let mut extra = fields
                .iter()
                .map(|(k, v)| (k.into(), v.clone()))
                .collect::<Vec<_>>();
            extra.push(("unexpected_key".into(), Value::Null));
            out.push(Value::Map(Map::try_from_entries(extra).unwrap()));
            for (key, child) in fields.iter() {
                out.push(Value::Map(
                    Map::try_from_entries(
                        fields
                            .iter()
                            .filter(|(k, _)| *k != key)
                            .map(|(k, v)| (k.into(), v.clone())),
                    )
                    .unwrap(),
                ));
                for replacement in mutations(child, depth - 1) {
                    out.push(Value::Map(
                        Map::try_from_entries(fields.iter().map(|(k, v)| {
                            (
                                k.into(),
                                if k == key {
                                    replacement.clone()
                                } else {
                                    v.clone()
                                },
                            )
                        }))
                        .unwrap(),
                    ));
                }
            }
        }
        _ => {}
    }
    out
}

#[test]
fn every_accepted_mutation_preserves_exact_canonical_native_representation() {
    let l = Limits::default();
    let id = Digest::from_bytes([17; 32]);
    let string = ValueType::builtin(BuiltinType::String);
    let port = Port::new(string.clone(), false);
    let generic = ValueType::new(
        ValueTypeKind::Var("t".parse().unwrap()),
        TypeContext::Signature,
        &l,
    )
    .unwrap();
    let signature = FunctionSignature::new(
        vec!["t".parse().unwrap()],
        vec![Port::new(generic.clone(), true)],
        Port::new(generic, true),
        &l,
    )
    .unwrap();
    let library = Library::new(
        "fixture.library".into(),
        "1".into(),
        id,
        vec![("identity".parse().unwrap(), signature.clone())],
        &l,
    )
    .unwrap();
    let engine = EngineIdentity::new("engine".into(), "1".into(), "data-1".into(), id, &l).unwrap();
    let profile =
        ExecutionProfile::new(id, engine.clone(), engine.clone(), engine.clone(), id, &l).unwrap();
    let server = ServerIdentity::new(
        "fixture".into(),
        McpTransport::Stdio,
        "fixture".into(),
        "1".into(),
        &l,
    )
    .unwrap();
    let template = PromptTemplate::new(
        vec![("name".parse().unwrap(), port.clone())],
        vec![
            TemplatePart::Text("Hi ".into()),
            TemplatePart::Slot("name".parse().unwrap()),
        ],
        &l,
    )
    .unwrap();
    let expr = Expression::new(
        ExpressionKind::Record(vec![(
            "array".into(),
            Expression::new(
                ExpressionKind::List(vec![
                    Expression::literal(ScalarLiteral::Integer(7)),
                    Expression::literal(ScalarLiteral::Null),
                ]),
                &l,
            )
            .unwrap(),
        )]),
        &l,
    )
    .unwrap();
    let mut node = NodeFields::new(
        "n".parse().unwrap(),
        Operation::Eval(Expression::literal(ScalarLiteral::Integer(7))),
    );
    node.outputs = PortTable::new(
        vec![(
            "value".parse().unwrap(),
            Port::new(ValueType::builtin(BuiltinType::Integer), true),
        )],
        &l,
    )
    .unwrap();
    let node = Node::new(node, &l).unwrap();
    let edge = Edge::new(
        "e".parse().unwrap(),
        EdgeSource::Input("value".parse().unwrap()),
        EdgeDestination::Output("value".parse().unwrap()),
    );
    let scope = Scope::new(ScopeFields::default(), &l).unwrap();
    let mut fields = DocumentFields::new(
        "test.mutations".into(),
        profile.clone(),
        scope.digest(&l).unwrap(),
    );
    fields.scopes.insert(fields.root_scope, scope.clone());
    fields
        .templates
        .insert(template.digest(&l).unwrap(), template.clone());
    fields.libraries.insert(id, library.clone());
    let document = CanonicalDocument::new(fields, &l).unwrap();
    let mut corpus = vec![
        (
            Kind::Type,
            ValueType::new(
                ValueTypeKind::Record(vec![("value".into(), port.clone())]),
                TypeContext::Value,
                &l,
            )
            .unwrap()
            .to_value(TypeContext::Value, &l)
            .unwrap(),
        ),
        (Kind::Port, port.to_value(TypeContext::Value, &l).unwrap()),
        (Kind::Expression, expr.to_value(&l).unwrap()),
        (Kind::Template, template.to_value(&l).unwrap()),
        (
            Kind::Limits,
            ExecutionLimits::new()
                .with_timeout_ms(100)
                .unwrap()
                .with_max_mcp_calls(0)
                .unwrap()
                .to_value(&l)
                .unwrap(),
        ),
        (
            Kind::Retry,
            RetryPolicy::new(2, vec!["MCP_TIMEOUT".into()], vec![1], &l)
                .unwrap()
                .to_value(&l)
                .unwrap(),
        ),
        (Kind::Engine, engine.to_value(&l).unwrap()),
        (Kind::Profile, profile.to_value(&l).unwrap()),
        (Kind::Server, server.to_value(&l).unwrap()),
        (Kind::Signature, signature.to_value(&l).unwrap()),
        (Kind::Library, library.to_value(&l).unwrap()),
        (Kind::Node, node.to_value(&l).unwrap()),
        (Kind::Edge, edge.to_value(&l).unwrap()),
        (Kind::Scope, scope.to_value(&l).unwrap()),
        (Kind::Document, document.to_value(&l).unwrap()),
    ];
    for kind in [
        McpBindingKind::Tool {
            name: "echo".into(),
            input_schema: id,
            output_schema: id,
        },
        McpBindingKind::Resource {
            uri: "context://fixed".into(),
        },
        McpBindingKind::Template {
            uri_template: "context://{name}".into(),
        },
        McpBindingKind::Prompt {
            name: "review".into(),
        },
    ] {
        corpus.push((
            Kind::Binding,
            McpBinding::new(server.clone(), id, kind, &l)
                .unwrap()
                .to_value(&l)
                .unwrap(),
        ));
    }
    for kind in [
        ExpressionKind::Get {
            value: Box::new(Expression::literal(ScalarLiteral::String("x".into()))),
            path: vec![PathStep::Index(0)],
        },
        ExpressionKind::Ref {
            source: ValueReference::Output {
                node: "n".parse().unwrap(),
                port: "value".parse().unwrap(),
            },
            path: vec![PathStep::Field("x".into())],
        },
        ExpressionKind::Call {
            function: FunctionId::Core(CoreFunction::Length),
            arguments: vec![Expression::literal(ScalarLiteral::String("x".into()))],
        },
        ExpressionKind::FunctionRef {
            library: id,
            name: "identity".parse().unwrap(),
        },
        ExpressionKind::Render {
            template: id,
            arguments: vec![(
                "name".parse().unwrap(),
                Expression::literal(ScalarLiteral::String("x".into())),
            )],
        },
    ] {
        corpus.push((
            Kind::Expression,
            Expression::new(kind, &l).unwrap().to_value(&l).unwrap(),
        ));
    }
    let mut rejected = 0;
    let mut accepted = 0;
    for (kind, value) in corpus {
        assert_eq!(parse(kind, &value, &l).unwrap(), value);
        for changed in mutations(&value, 4) {
            match parse(kind, &changed, &l) {
                Ok(roundtrip) => {
                    accepted += 1;
                    assert_eq!(roundtrip, changed, "{kind:?}");
                    assert_eq!(
                        cbor::encode(&roundtrip, &l).unwrap(),
                        cbor::encode(&changed, &l).unwrap()
                    );
                }
                Err(error) => {
                    rejected += 1;
                    assert!(!error.to_string().is_empty());
                }
            }
        }
    }
    assert!(rejected > 1000 && accepted > 10);
}
