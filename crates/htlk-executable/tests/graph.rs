//! Canonical graph-record shapes, local invariants, and digest domains.

use htlk_cbor::{LimitKind, Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::digest::{Digest, RecordKind, record_digest};
use htlk_executable::{
    Edge, EdgeDestination as D, EdgeSource as S, ExecutionLimits, Expression as E,
    ExpressionContext as EC, ExpressionKind as EK, GraphRecordError as Error, Node, NodeFields,
    Operation as O, Port, PortTable, PrimitiveType as P, RetryPolicy, ScalarLiteral as L, Scope,
    ScopeContext as C, ScopeFields, ValueReference, ValueType,
};
use std::error::Error as _;

fn table(names: &[&str], ty: P) -> PortTable {
    PortTable::new(
        names
            .iter()
            .map(|n| {
                (
                    n.parse().unwrap(),
                    Port::new(ValueType::primitive(ty), true),
                )
            })
            .collect(),
        &Limits::default(),
    )
    .unwrap()
}
fn literal() -> E {
    E::literal(L::Boolean(true))
}
fn node(id: &str) -> Node {
    let mut f = NodeFields::new(
        id.parse().unwrap(),
        O::Eval(E::literal(L::String("x".into()))),
    );
    f.outputs = table(&["value"], P::String);
    Node::new(f, C::Ordinary, &Limits::default()).unwrap()
}
fn replace(value: &Value, key: &str, replacement: Option<Value>) -> Value {
    let Value::Map(map) = value else { panic!() };
    let mut entries: Vec<_> = map
        .iter()
        .filter(|(k, _)| *k != key)
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect();
    if let Some(value) = replacement {
        entries.push((key.into(), value));
    }
    Value::Map(Map::try_from_entries(entries).unwrap())
}

#[test]
fn empty_scope_has_golden_bytes_and_context_independent_identity() {
    let l = Limits::default();
    let scope = Scope::new(ScopeFields::default(), C::Ordinary, &l).unwrap();
    let bytes = b"\xa8\x65edges\x80\x65nodes\x80\x66inputs\xa0\x66limits\xa0\x67carried\xa0\x67outputs\xa0\x6dpreconditions\x82\x67literal\xf5\x6epostconditions\x82\x67literal\xf5";
    assert_eq!(scope.encode(C::Ordinary, &l).unwrap(), bytes);
    assert_eq!(Scope::decode(bytes, C::Ordinary, &l).unwrap(), scope);
    assert_eq!(Scope::decode(bytes, C::LoopBody, &l).unwrap(), scope);
    assert_eq!(
        scope.digest(C::Ordinary, &l).unwrap(),
        scope.digest(C::LoopBody, &l).unwrap()
    );
    // Independent Python hashlib vector over the literal bytes above.
    assert_eq!(
        scope.digest(C::Ordinary, &l).unwrap().to_string(),
        "sha256:a0a913309d437ba674343e89eaeda42314773a4bcb14debced7148df89b0f221"
    );
}

#[test]
fn node_edge_arrays_normalize_by_ascii_id_not_encoded_key_order() {
    let l = Limits::default();
    let edges = ["z", "aa"].map(|id| {
        Edge::new(
            id.parse().unwrap(),
            S::Output {
                node: id.parse().unwrap(),
                port: "value".parse().unwrap(),
            },
            D::Output("result".parse().unwrap()),
        )
    });
    let fields = ScopeFields {
        nodes: vec![node("z"), node("aa")],
        edges: edges.to_vec(),
        outputs: table(&["result"], P::String),
        ..ScopeFields::default()
    };
    let scope = Scope::new(fields, C::Ordinary, &l).unwrap();
    assert_eq!(scope.fields().nodes[0].id().as_str(), "aa");
    assert_eq!(scope.fields().edges[0].id().as_str(), "aa");
    let value = scope.to_value(C::Ordinary, &l).unwrap();
    for key in ["nodes", "edges"] {
        let Value::Map(map) = &value else { panic!() };
        let Some(Value::Array(items)) = map.get(key) else {
            panic!()
        };
        let mut items = items.clone();
        items.reverse();
        let bad = replace(&value, key, Some(Value::Array(items)));
        assert!(matches!(
            Scope::from_value(&bad, C::Ordinary, &l),
            Err(Error::NonCanonicalOrder(_))
        ));
    }
    assert_eq!(
        Scope::decode(&scope.encode(C::Ordinary, &l).unwrap(), C::Ordinary, &l).unwrap(),
        scope
    );
    // Node and edge IDs are separate namespaces, so the matching names above are valid.
    let bad = ScopeFields {
        nodes: vec![node("x"), node("x")],
        ..ScopeFields::default()
    };
    assert_eq!(
        Scope::new(bad, C::Ordinary, &l).unwrap_err(),
        Error::DuplicateId("node")
    );
    let e = Edge::new(
        "x".parse().unwrap(),
        S::Input("q".parse().unwrap()),
        D::Output("q".parse().unwrap()),
    );
    let bad = ScopeFields {
        inputs: table(&["q"], P::String),
        outputs: table(&["q"], P::String),
        edges: vec![e.clone(), e],
        ..ScopeFields::default()
    };
    assert_eq!(
        Scope::new(bad, C::Ordinary, &l).unwrap_err(),
        Error::DuplicateId("edge")
    );
}

#[test]
fn port_tables_require_identifiers_and_value_types() {
    let l = Limits::default();
    let ports = table(&["z", "aa"], P::String);
    assert_eq!(
        ports.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["aa", "z"]
    );
    assert!(ports.get("missing").is_none());
    assert_eq!(ports.len(), 2);
    assert_eq!(
        PortTable::decode(&ports.encode(&l).unwrap(), &l).unwrap(),
        ports
    );
    let p = Port::new(ValueType::primitive(P::String), false);
    let value = Value::Map(
        Map::try_from_entries([(
            "customerId".into(),
            p.to_value(htlk_executable::TypeContext::Value, &l).unwrap(),
        )])
        .unwrap(),
    );
    assert!(matches!(
        PortTable::from_value(&value, &l),
        Err(Error::Identifier(_))
    ));
    assert!(
        PortTable::new(
            vec![("x".parse().unwrap(), p.clone()), ("x".parse().unwrap(), p)],
            &l
        )
        .is_err()
    );
    assert_eq!(PortTable::default().encode(&l).unwrap(), [0xa0]);
}

#[test]
fn operation_shapes_and_positive_bounds_are_exact() {
    let l = Limits::default();
    let d = Digest::from_bytes([0; 32]);
    let operations = [
        O::Eval(literal()),
        O::Mcp {
            binding: d,
            retry: RetryPolicy::default(),
        },
        O::Scope(d),
        O::Loop {
            body: d,
            initializers: vec![("x".parse().unwrap(), "seed".parse().unwrap())],
            until: literal(),
            max_iterations: 3,
        },
        O::Wait {
            topic: "review".into(),
            timeout_ms: 1000,
        },
    ];
    for operation in operations {
        let value = operation.to_value(&l).unwrap();
        assert_eq!(O::from_value(&value, &l).unwrap(), operation);
        assert_eq!(
            O::decode(&operation.encode(&l).unwrap(), &l).unwrap(),
            operation
        );
    }
    assert_eq!(
        O::Eval(literal()).encode(&l).unwrap(),
        b"\x82\x64eval\x82\x67literal\xf5"
    );
    assert_eq!(
        O::Wait {
            topic: "review".into(),
            timeout_ms: 1000
        }
        .encode(&l)
        .unwrap(),
        b"\x83\x64wait\x66review\x19\x03\xe8"
    );
    for n in [0, u64::MAX] {
        assert_eq!(
            O::Wait {
                topic: "".into(),
                timeout_ms: n
            }
            .encode(&l)
            .unwrap_err(),
            Error::InvalidBound("timeout_ms")
        );
        assert_eq!(
            O::Loop {
                body: d,
                initializers: vec![],
                until: literal(),
                max_iterations: n
            }
            .encode(&l)
            .unwrap_err(),
            Error::InvalidBound("max_iterations")
        );
    }
    for n in [-1, 0] {
        let v = Value::Array(vec![
            Value::Text("wait".into()),
            Value::Text("review".into()),
            Value::Integer(n),
        ]);
        assert_eq!(
            O::from_value(&v, &l).unwrap_err(),
            Error::InvalidBound("timeout_ms")
        );
    }
    let v = Value::Array(vec![Value::Text("call".into()), Value::Text(d.to_string())]);
    assert_eq!(O::from_value(&v, &l).unwrap_err(), Error::UnknownOperation);
    let duplicated = O::Loop {
        body: d,
        initializers: vec![
            ("x".parse().unwrap(), "a".parse().unwrap()),
            ("x".parse().unwrap(), "b".parse().unwrap()),
        ],
        until: literal(),
        max_iterations: 1,
    };
    assert!(duplicated.encode(&l).is_err());
}

#[test]
fn primitive_layouts_reserved_names_and_context_free_representation() {
    let l = Limits::default();
    let base = node("worker");
    for name in [
        "inputs",
        "outputs",
        "carried",
        "next",
        "length",
        "present",
        "status",
        "error",
        "render",
        "mcp",
        "predicates",
    ] {
        let mut fields = base.fields().clone();
        fields.id = name.parse().unwrap();
        assert_eq!(
            Node::new(fields, C::Ordinary, &l).unwrap_err(),
            Error::ReservedNodeName
        );
    }
    assert_eq!(node("graph").id().as_str(), "graph");
    let mut f = base.fields().clone();
    f.outputs = PortTable::default();
    assert!(matches!(
        Node::new(f, C::Ordinary, &l),
        Err(Error::InvalidPorts(_))
    ));
    let mut f = base.fields().clone();
    f.outputs = PortTable::new(
        vec![(
            "value".parse().unwrap(),
            Port::new(ValueType::primitive(P::String), false),
        )],
        &l,
    )
    .unwrap();
    assert!(Node::new(f.clone(), C::Ordinary, &l).is_ok()); // eval may return absence
    f.operation = O::Mcp {
        binding: Digest::from_bytes([0; 32]),
        retry: RetryPolicy::default(),
    };
    assert!(matches!(
        Node::new(f, C::Ordinary, &l),
        Err(Error::InvalidPorts(_))
    ));
    let mut f = base.fields().clone();
    f.operation = O::Wait {
        topic: "".into(),
        timeout_ms: 1,
    };
    assert!(matches!(
        Node::new(f.clone(), C::Ordinary, &l),
        Err(Error::InvalidPorts(_))
    ));
    f.inputs = table(&["request"], P::Json);
    assert!(Node::new(f, C::Ordinary, &l).is_ok());
    let status = E::new(
        EK::Status("worker".parse().unwrap()),
        EC::ScopePostconditions,
        &l,
    )
    .unwrap();
    let mut f = base.fields().clone();
    f.preconditions = status.clone();
    assert!(Node::new(f, C::Ordinary, &l).is_ok());
    let mut f = base.fields().clone();
    f.operation = O::Scope(Digest::from_bytes([0; 32]));
    f.postconditions = status;
    assert!(Node::new(f, C::Ordinary, &l).is_ok());
}

#[test]
fn loop_contexts_do_not_become_serialized_role_labels() {
    let l = Limits::default();
    let e = Edge::new(
        "advance".parse().unwrap(),
        S::Carried("x".parse().unwrap()),
        D::Next("x".parse().unwrap()),
    );
    assert_eq!(
        e.encode(C::Ordinary, &l).unwrap(),
        e.encode(C::LoopBody, &l).unwrap()
    );
    assert_eq!(
        Edge::decode(&e.encode(C::LoopBody, &l).unwrap(), C::LoopBody, &l).unwrap(),
        e
    );
    let fields = ScopeFields {
        carried: table(&["x"], P::String),
        edges: vec![e],
        ..ScopeFields::default()
    };
    let body = Scope::new(fields.clone(), C::LoopBody, &l).unwrap();
    assert!(Scope::new(fields.clone(), C::Ordinary, &l).is_ok());
    assert_eq!(
        Scope::decode(&body.encode(C::LoopBody, &l).unwrap(), C::LoopBody, &l).unwrap(),
        body
    );
    let mut bad = fields.clone();
    bad.limits = ExecutionLimits::new().with_timeout_ms(1).unwrap();
    assert!(Scope::new(bad, C::LoopBody, &l).is_ok());
    let mut bad = fields;
    bad.postconditions = E::new(
        EK::Not(Box::new(E::literal(L::Boolean(false)))),
        EC::ScopePostconditions,
        &l,
    )
    .unwrap();
    assert!(Scope::new(bad, C::LoopBody, &l).is_ok());
    let mut f = node("worker").fields().clone();
    f.guard = E::new(
        EK::Ref {
            source: ValueReference::Carried("x".parse().unwrap()),
            path: vec![],
        },
        EC::Guard { loop_body: true },
        &l,
    )
    .unwrap();
    let n = Node::new(f, C::LoopBody, &l).unwrap();
    assert_eq!(
        n.encode(C::Ordinary, &l).unwrap(),
        n.encode(C::LoopBody, &l).unwrap()
    );
}

#[test]
fn initializer_names_are_preserved_for_later_semantic_linkage() {
    let l = Limits::default();
    let mut f = NodeFields::new(
        "loop_node".parse().unwrap(),
        O::Loop {
            body: Digest::from_bytes([0; 32]),
            initializers: vec![("x".parse().unwrap(), "seed".parse().unwrap())],
            until: literal(),
            max_iterations: 1,
        },
    );
    assert!(Node::new(f.clone(), C::Ordinary, &l).is_ok());
    f.inputs = table(&["seed"], P::String);
    let n = Node::new(f, C::Ordinary, &l).unwrap();
    assert_eq!(
        Node::decode(&n.encode(C::Ordinary, &l).unwrap(), C::Ordinary, &l).unwrap(),
        n
    );
    // The referenced digest does not need to exist until document linkage.
}

#[test]
fn closed_records_never_infer_missing_canonical_fields() {
    let l = Limits::default();
    let n = node("worker");
    let value = n.to_value(C::Ordinary, &l).unwrap();
    for field in [
        "id",
        "inputs",
        "outputs",
        "guard",
        "preconditions",
        "postconditions",
        "limits",
        "operation",
    ] {
        assert_eq!(
            Node::from_value(&replace(&value, field, None), C::Ordinary, &l).unwrap_err(),
            Error::MissingField(field)
        );
    }
    assert_eq!(
        Node::from_value(
            &replace(&value, "unknown", Some(Value::Null)),
            C::Ordinary,
            &l
        )
        .unwrap_err(),
        Error::UnknownField("node")
    );
    let s = Scope::new(ScopeFields::default(), C::Ordinary, &l)
        .unwrap()
        .to_value(C::Ordinary, &l)
        .unwrap();
    for field in [
        "inputs",
        "outputs",
        "carried",
        "nodes",
        "edges",
        "preconditions",
        "postconditions",
        "limits",
    ] {
        assert_eq!(
            Scope::from_value(&replace(&s, field, None), C::Ordinary, &l).unwrap_err(),
            Error::MissingField(field)
        );
    }
    let e = Edge::new(
        "pass".parse().unwrap(),
        S::Input("x".parse().unwrap()),
        D::Output("x".parse().unwrap()),
    )
    .to_value(C::Ordinary, &l)
    .unwrap();
    assert_eq!(
        Edge::from_value(&replace(&e, "guard", None), C::Ordinary, &l).unwrap_err(),
        Error::MissingField("guard")
    );
    let mut trailing = n.encode(C::Ordinary, &l).unwrap();
    trailing.push(0);
    let error = Node::decode(&trailing, C::Ordinary, &l).unwrap_err();
    assert!(matches!(error, Error::Codec(_)));
    assert!(error.source().is_some());
}

#[test]
fn all_record_domains_use_exact_prefixes_and_node_content_matters() {
    let l = Limits::default();
    let empty = Value::Map(Map::new());
    for (kind, expected) in [
        (
            RecordKind::Scope,
            "8c034d63b0e24cd119367d71ee8d7e570ad371f9c791e20ddde8cf064bdcbcfe",
        ),
        (
            RecordKind::Node,
            "6e79c1e3ece454237e83840d0f59ccc8155e972ce6cacad819be603f9bd6daa4",
        ),
        (
            RecordKind::Template,
            "402338d394acb6b501380204859cc2c920b7a5e3bbe98eab5e00b2d7ab11c32c",
        ),
        (
            RecordKind::Binding,
            "1e5449016c7a23b3be83f62461da52a9876214bbeffa4b45aee57fe65a496e54",
        ),
        (
            RecordKind::Server,
            "8ae36d873d9342b1539f3af7390647c8ad8a7f2d7b140e0ecf02e6b900abb52d",
        ),
    ] {
        assert_eq!(
            record_digest(kind, &empty, &l).unwrap().to_string(),
            format!("sha256:{expected}")
        );
    }
    let a = node("worker");
    let mut f = a.fields().clone();
    f.limits = ExecutionLimits::new().with_max_mcp_calls(0).unwrap();
    let b = Node::new(f, C::Ordinary, &l).unwrap();
    assert_ne!(
        a.digest(C::Ordinary, &l).unwrap(),
        b.digest(C::Ordinary, &l).unwrap()
    );
}

#[test]
fn whole_scope_conversion_and_codec_limits_are_enforced() {
    let l = Limits {
        max_document_bytes: 98,
        max_depth: 2,
    };
    let s = Scope::new(ScopeFields::default(), C::Ordinary, &l).unwrap();
    let bytes = s.encode(C::Ordinary, &l).unwrap();
    assert_eq!(bytes.len(), 98);
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 2] = [
        (LimitKind::DocumentBytes, 97, |l| l.max_document_bytes = 97),
        (LimitKind::Depth, 1, |l| l.max_depth = 1),
    ];
    for (limit, maximum, adjust) in cases {
        let mut tight = l.clone();
        adjust(&mut tight);
        assert_eq!(
            s.to_value(C::Ordinary, &tight).unwrap_err(),
            Error::LimitExceeded { limit, maximum }
        );
        assert!(matches!(
            Scope::decode(&bytes, C::Ordinary, &tight),
            Err(Error::Codec(_))
        ));
    }
    for end in 0..bytes.len() {
        let Error::Codec(error) = Scope::decode(&bytes[..end], C::Ordinary, &l).unwrap_err() else {
            panic!()
        };
        assert_eq!(error.offset(), Some(end));
    }
}
