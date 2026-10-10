//! Canonical template records, coverage, normalization, and record hashes.

use htlk_cbor::{LimitKind, Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{
    BuiltinType as P, ExpressionError as Error, Port, PromptTemplate as T, TemplatePart as Part,
    ValueType,
};

fn parameter(p: P) -> Port {
    Port::new(ValueType::builtin(p), true)
}

#[test]
fn float_parameters_round_trip_with_presence_and_normalization() {
    let limits = Limits::default();
    for required in [false, true] {
        let template = T::new(
            vec![(
                "x".parse().unwrap(),
                Port::new(ValueType::builtin(P::Float), required),
            )],
            vec![
                Part::Text("".into()),
                Part::Text("x".into()),
                Part::Text("=".into()),
                Part::Slot("x".parse().unwrap()),
            ],
            &limits,
        )
        .unwrap();
        assert_eq!(template.parameters()[0].1.required(), required);
        assert_eq!(template.parts()[0], Part::Text("x=".into()));
        let value = template.to_value(&limits).unwrap();
        let bytes = template.encode(&limits).unwrap();
        assert_eq!(T::from_value(&value, &limits).unwrap(), template);
        let decoded = T::decode(&bytes, &limits).unwrap();
        assert_eq!(decoded, template);
        assert_eq!(
            decoded.digest(&limits).unwrap(),
            template.digest(&limits).unwrap()
        );
    }
}
fn value(parameters: Value, parts: Vec<Value>) -> Value {
    Value::Map(
        Map::try_from_entries([
            ("parameters".into(), parameters),
            ("parts".into(), Value::Array(parts)),
        ])
        .unwrap(),
    )
}

#[test]
fn template_golden_bytes_and_digest() {
    let template = T::new(
        vec![("name".parse().unwrap(), parameter(P::String))],
        vec![
            Part::Text("Hi ".into()),
            Part::Slot("name".parse().unwrap()),
        ],
        &Limits::default(),
    )
    .unwrap();
    let bytes = b"\xa2\x65parts\x82\x63Hi \x82\x64slot\x64name\x6aparameters\xa1\x64name\xa2\x64type\x66string\x68required\xf5";
    assert_eq!(template.encode(&Limits::default()).unwrap(), bytes);
    // Independently calculated from the literal CBOR bytes with Python hashlib.
    assert_eq!(
        template.digest(&Limits::default()).unwrap().to_string(),
        "sha256:b2a86fc68a03809076995b595fe6a6367de25626bab6b82b808857fd4de52313"
    );
    assert_eq!(T::decode(bytes, &Limits::default()).unwrap(), template);
    assert_eq!(
        T::from_value(
            &template.to_value(&Limits::default()).unwrap(),
            &Limits::default()
        )
        .unwrap(),
        template
    );
}

#[test]
fn authored_parts_normalize_and_repeated_slots_keep_order() {
    let template = T::new(
        vec![
            ("z".parse().unwrap(), parameter(P::Boolean)),
            ("aa".parse().unwrap(), parameter(P::Integer)),
        ],
        vec![
            Part::Text("".into()),
            Part::Text("Hi".into()),
            Part::Text(" ".into()),
            Part::Slot("z".parse().unwrap()),
            Part::Text("".into()),
            Part::Slot("z".parse().unwrap()),
            Part::Slot("aa".parse().unwrap()),
        ],
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(template.parameters()[0].0.as_str(), "aa");
    assert_eq!(
        template.parts(),
        [
            Part::Text("Hi ".into()),
            Part::Slot("z".parse().unwrap()),
            Part::Slot("z".parse().unwrap()),
            Part::Slot("aa".parse().unwrap())
        ]
    );
    let bytes = template.encode(&Limits::default()).unwrap();
    assert_eq!(T::decode(&bytes, &Limits::default()).unwrap(), template);
    let empty = T::new(vec![], vec![Part::Text("".into())], &Limits::default()).unwrap();
    assert!(empty.parts().is_empty());
    assert_eq!(
        empty.encode(&Limits::default()).unwrap(),
        b"\xa2\x65parts\x80\x6aparameters\xa0"
    );
    for parts in [
        vec![Value::Text("".into())],
        vec![Value::Text("a".into()), Value::Text("b".into())],
    ] {
        assert!(matches!(
            T::from_value(&value(Value::Map(Map::new()), parts), &Limits::default()),
            Err(Error::NonCanonical(_))
        ));
    }
}

#[test]
fn parameters_keep_supported_types_and_unresolved_names() {
    assert!(
        T::new(
            vec![],
            vec![Part::Slot("missing".parse().unwrap())],
            &Limits::default()
        )
        .is_ok()
    );
    assert!(
        T::new(
            vec![("unused".parse().unwrap(), parameter(P::String))],
            vec![],
            &Limits::default()
        )
        .is_ok()
    );
    for p in [
        P::Bytes,
        P::Json,
        P::Regex,
        P::Null,
        P::Error,
        P::McpResourceResult,
        P::McpPromptResult,
    ] {
        assert_eq!(
            T::new(
                vec![("x".parse().unwrap(), parameter(p))],
                vec![Part::Slot("x".parse().unwrap())],
                &Limits::default()
            )
            .unwrap_err(),
            Error::InvalidTemplateParameter
        );
        let parameters = Value::Map(
            Map::try_from_entries([(
                "x".into(),
                parameter(p)
                    .to_value(TypeContext::Value, &Limits::default())
                    .unwrap(),
            )])
            .unwrap(),
        );
        assert_eq!(
            T::from_value(&value(parameters, vec![]), &Limits::default()).unwrap_err(),
            Error::InvalidTemplateParameter
        );
    }
    use htlk_executable::{TypeContext, ValueTypeKind};
    for kind in [
        ValueTypeKind::Union(vec![
            ValueType::builtin(P::Float),
            ValueType::builtin(P::String),
        ]),
        ValueTypeKind::Record(vec![]),
        ValueTypeKind::List(Box::new(ValueType::builtin(P::Float))),
    ] {
        let port = Port::new(
            ValueType::new(kind, TypeContext::Value, &Limits::default()).unwrap(),
            true,
        );
        assert_eq!(
            T::new(
                vec![("x".parse().unwrap(), port.clone())],
                vec![],
                &Limits::default()
            )
            .unwrap_err(),
            Error::InvalidTemplateParameter
        );
        let parameters = Value::Map(
            Map::try_from_entries([(
                "x".into(),
                port.to_value(TypeContext::Value, &Limits::default())
                    .unwrap(),
            )])
            .unwrap(),
        );
        assert_eq!(
            T::from_value(&value(parameters, vec![]), &Limits::default()).unwrap_err(),
            Error::InvalidTemplateParameter
        );
    }
    assert!(
        T::new(
            vec![
                ("x".parse().unwrap(), parameter(P::String)),
                ("x".parse().unwrap(), parameter(P::String))
            ],
            vec![],
            &Limits::default()
        )
        .is_err()
    );
    assert!(T::from_value(&Value::Null, &Limits::default()).is_err());
    let bad_part = value(
        Value::Map(Map::new()),
        vec![Value::Array(vec![Value::Text("unknown".into())])],
    );
    assert!(matches!(
        T::from_value(&bad_part, &Limits::default()),
        Err(Error::InvalidShape(_))
    ));
}

#[test]
fn template_limits_include_normalized_text_and_metadata() {
    let t = T::new(
        vec![("name".parse().unwrap(), parameter(P::String))],
        vec![
            Part::Text("Hi ".into()),
            Part::Slot("name".parse().unwrap()),
        ],
        &Limits::default(),
    )
    .unwrap();
    let bytes = t.encode(&Limits::default()).unwrap();
    let exact = Limits {
        max_document_bytes: bytes.len(),
        ..Limits::default()
    };
    assert_eq!(t.encode(&exact).unwrap(), bytes);
    assert!(matches!(
        t.to_value(&Limits {
            max_document_bytes: bytes.len() - 1,
            ..exact
        }),
        Err(Error::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
    // Each input text fits 10 bytes, but joining the pair would exceed it.
    let limits = Limits {
        max_document_bytes: 10,
        ..Limits::default()
    };
    assert_eq!(
        T::new(
            vec![],
            vec![Part::Text("123456".into()), Part::Text("123456".into())],
            &limits
        )
        .unwrap_err(),
        Error::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: 10
        }
    );
    for end in 0..bytes.len() {
        let Error::Codec(error) = T::decode(&bytes[..end], &Limits::default()).unwrap_err() else {
            panic!()
        };
        assert_eq!(error.offset(), Some(end));
    }
}

#[test]
fn template_conversion_and_decode_enforce_each_wire_ceiling() {
    let t = T::new(
        vec![("name".parse().unwrap(), parameter(P::String))],
        vec![
            Part::Text("Hi ".into()),
            Part::Slot("name".parse().unwrap()),
        ],
        &Limits::default(),
    )
    .unwrap();
    let exact = Limits {
        max_document_bytes: 63,
        max_depth: 3,
    };
    let bytes = t.encode(&exact).unwrap();
    assert_eq!(T::decode(&bytes, &exact).unwrap(), t);
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 2] = [
        (LimitKind::DocumentBytes, 62, |l| l.max_document_bytes = 62),
        (LimitKind::Depth, 2, |l| l.max_depth = 2),
    ];
    for (limit, maximum, adjust) in cases {
        let mut l = exact.clone();
        adjust(&mut l);
        assert_eq!(
            t.to_value(&l).unwrap_err(),
            Error::LimitExceeded { limit, maximum }
        );
        assert!(matches!(T::decode(&bytes, &l), Err(Error::Codec(_))));
    }
}

#[test]
fn template_ingress_checks_parameter_records_and_names() {
    let slot = Value::Array(vec![Value::Text("slot".into()), Value::Text("x".into())]);
    let parameters = Value::Map(
        Map::try_from_entries([(
            "x".into(),
            parameter(P::Json)
                .to_value(htlk_executable::TypeContext::Value, &Limits::default())
                .unwrap(),
        )])
        .unwrap(),
    );
    assert_eq!(
        T::from_value(&value(parameters, vec![slot]), &Limits::default()).unwrap_err(),
        Error::InvalidTemplateParameter
    );
    let parameters = Value::Map(
        Map::try_from_entries([(
            "BadName".into(),
            parameter(P::String)
                .to_value(htlk_executable::TypeContext::Value, &Limits::default())
                .unwrap(),
        )])
        .unwrap(),
    );
    assert!(matches!(
        T::from_value(&value(parameters, vec![]), &Limits::default()),
        Err(Error::Identifier(_))
    ));
    let extra = Value::Map(
        Map::try_from_entries([
            ("parameters".into(), Value::Map(Map::new())),
            ("parts".into(), Value::Array(vec![])),
            ("extra".into(), Value::Null),
        ])
        .unwrap(),
    );
    assert!(matches!(
        T::from_value(&extra, &Limits::default()),
        Err(Error::InvalidShape(_))
    ));
}
