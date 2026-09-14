//! Canonical type algebra, normalization, contexts, and resource boundaries.

use std::error::Error as _;

use htlk_cbor::{LimitKind, Limits, Map, Value};
use htlk_executable::digest::Digest;
use htlk_executable::{
    Identifier, Port, PrimitiveType as P, TypeContext as C, TypeError, ValueType as T,
    ValueTypeKind as K,
};

fn primitive(p: P) -> T {
    T::primitive(p)
}
fn make(kind: K) -> T {
    T::new(kind, C::Value, &Limits::default()).unwrap()
}
fn text(value: &str) -> Value {
    Value::Text(value.into())
}
fn tagged(tag: &str, value: Value) -> Value {
    Value::Array(vec![text(tag), value])
}
fn port_value(ty: Value, required: bool) -> Value {
    Value::Map(
        Map::try_from_entries([
            ("type".into(), ty),
            ("required".into(), Value::Bool(required)),
        ])
        .unwrap(),
    )
}
fn check_type(ty: &T, context: C) {
    let limits = Limits::default();
    let value = ty.to_value(context, &limits).unwrap();
    let bytes = ty.encode(context, &limits).unwrap();
    assert_eq!(htlk_cbor::encode(&value, &limits).unwrap(), bytes);
    assert_eq!(T::from_value(&value, context, &limits).unwrap(), *ty);
    assert_eq!(T::decode(&bytes, context, &limits).unwrap(), *ty);
}

#[test]
fn primitive_names_and_simple_golden_bytes() {
    for (p, name) in [
        (P::String, "string"),
        (P::Integer, "integer"),
        (P::Float, "float"),
        (P::Boolean, "boolean"),
        (P::Null, "null"),
        (P::Bytes, "bytes"),
        (P::Json, "json"),
        (P::Regex, "regex"),
        (P::ResourceSnapshot, "ResourceSnapshot"),
        (P::McpPromptResult, "McpPromptResult"),
        (P::Error, "Error"),
    ] {
        let ty = primitive(p);
        assert_eq!(p.as_str(), name);
        assert_eq!(p.to_string(), name);
        assert_eq!(
            ty.to_value(C::Value, &Limits::default()).unwrap(),
            text(name)
        );
        check_type(&ty, C::Value);
        check_type(&ty, C::Signature);
    }
    let list = make(K::List(Box::new(primitive(P::String))));
    assert_eq!(
        list.encode(C::Value, &Limits::default()).unwrap(),
        b"\x82\x64list\x66string"
    );
    let map = make(K::Map(Box::new(primitive(P::Integer))));
    assert_eq!(
        map.encode(C::Value, &Limits::default()).unwrap(),
        b"\x82\x63map\x67integer"
    );
    check_type(&list, C::Value);
    check_type(&map, C::Value);
    assert_eq!(
        primitive(P::Null)
            .encode(C::Value, &Limits::default())
            .unwrap(),
        b"\x64null"
    );
    assert!(T::decode(&[0xf6], C::Value, &Limits::default()).is_err());
}

#[test]
fn ports_keep_requiredness_separate_from_nullability() {
    let nullable = make(K::Union(vec![primitive(P::String), primitive(P::Null)]));
    for ty in [primitive(P::String), nullable] {
        for required in [true, false] {
            let port = Port::new(ty.clone(), required);
            assert_eq!(port.value_type(), &ty);
            assert_eq!(port.required(), required);
            let value = port.to_value(C::Value, &Limits::default()).unwrap();
            assert_eq!(
                value,
                port_value(ty.to_value(C::Value, &Limits::default()).unwrap(), required)
            );
            let bytes = port.encode(C::Value, &Limits::default()).unwrap();
            assert_eq!(
                Port::decode(&bytes, C::Value, &Limits::default()).unwrap(),
                port
            );
            assert_eq!(
                Port::from_value(&value, C::Value, &Limits::default()).unwrap(),
                port
            );
        }
    }
    let port = Port::new(primitive(P::String), false);
    assert_eq!(
        port.encode(C::Value, &Limits::default()).unwrap(),
        b"\xa2\x64type\x66string\x68required\xf4"
    );
}

#[test]
fn normalization_is_explicit_and_does_not_change_semantic_order() {
    let nested = make(K::Union(vec![primitive(P::Integer), primitive(P::String)]));
    let union = make(K::Union(vec![
        primitive(P::Boolean),
        nested,
        primitive(P::String),
    ]));
    assert_eq!(
        union.to_value(C::Value, &Limits::default()).unwrap(),
        tagged(
            "union",
            Value::Array(vec![text("string"), text("boolean"), text("integer")])
        )
    );
    check_type(&union, C::Value);
    let enumeration = make(K::Enum(vec![
        "z".into(),
        "aa".into(),
        "".into(),
        "é".into(),
        "e\u{301}".into(),
    ]));
    assert_eq!(
        enumeration.kind(),
        &K::Enum(vec![
            "".into(),
            "aa".into(),
            "e\u{301}".into(),
            "z".into(),
            "é".into()
        ])
    );
    check_type(&enumeration, C::Value);
    for kind in [
        K::Union(vec![]),
        K::Union(vec![primitive(P::String)]),
        K::Union(vec![primitive(P::String), primitive(P::String)]),
    ] {
        assert_eq!(
            T::new(kind, C::Value, &Limits::default()).unwrap_err(),
            TypeError::UnionCardinality
        );
    }
    assert_eq!(
        T::new(K::Enum(vec![]), C::Value, &Limits::default()).unwrap_err(),
        TypeError::EmptyEnum
    );
    assert_eq!(
        T::new(
            K::Enum(vec!["x".into(), "x".into()]),
            C::Value,
            &Limits::default()
        )
        .unwrap_err(),
        TypeError::DuplicateEnumMember
    );
}

#[test]
fn canonical_ingress_never_repairs_types() {
    let limits = Limits::default();
    for value in [
        tagged("enum", Value::Array(vec![text("z"), text("aa")])),
        tagged("union", Value::Array(vec![text("boolean"), text("string")])),
        tagged("union", Value::Array(vec![text("string"), text("string")])),
        tagged(
            "union",
            Value::Array(vec![
                text("string"),
                tagged("union", Value::Array(vec![text("null"), text("integer")])),
            ]),
        ),
    ] {
        assert!(matches!(
            T::from_value(&value, C::Value, &limits),
            Err(TypeError::NonCanonical(_))
        ));
        let bytes = htlk_cbor::encode(&value, &limits).unwrap();
        assert!(matches!(
            T::decode(&bytes, C::Value, &limits),
            Err(TypeError::NonCanonical(_))
        ));
    }
    for alias in ["text", "Customer", "STRING", "unknown"] {
        assert_eq!(
            T::from_value(&text(alias), C::Value, &limits).unwrap_err(),
            TypeError::UnknownPrimitive
        );
    }
    let bad = tagged("union", Value::Array(vec![text("string")]));
    assert_eq!(
        T::from_value(&bad, C::Value, &limits).unwrap_err(),
        TypeError::UnionCardinality
    );
}

#[test]
fn record_fields_are_exact_strings_and_normalized_maps() {
    let fields = vec![
        ("customerId".into(), Port::new(primitive(P::String), true)),
        ("".into(), Port::new(primitive(P::Null), false)),
        (
            "Content-Type".into(),
            Port::new(primitive(P::String), false),
        ),
    ];
    let a = make(K::Record(fields.clone()));
    let b = make(K::Record(fields.into_iter().rev().collect()));
    assert_eq!(a, b);
    check_type(&a, C::Value);
    assert!("customerId".parse::<Identifier>().is_err());
    let K::Record(fields) = a.kind() else {
        panic!()
    };
    assert_eq!(
        fields
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["", "customerId", "Content-Type"]
    );
    assert_eq!(
        make(K::Record(vec![]))
            .encode(C::Value, &Limits::default())
            .unwrap(),
        b"\x82\x66record\xa0"
    );
    let duplicate = K::Record(vec![
        ("x".into(), Port::new(primitive(P::Null), false)),
        ("x".into(), Port::new(primitive(P::String), true)),
    ]);
    let TypeError::Codec(error) = T::new(duplicate, C::Value, &Limits::default()).unwrap_err()
    else {
        panic!()
    };
    assert_eq!(error.kind(), &htlk_cbor::ErrorKind::DuplicateMapKey);
}

#[test]
fn signature_restrictions_apply_recursively_and_parameters_keep_order() {
    let limits = Limits::default();
    let variable = T::new(K::Var("t".parse().unwrap()), C::Signature, &limits).unwrap();
    assert_eq!(
        variable.encode(C::Signature, &limits).unwrap(),
        b"\x82\x63var\x61t"
    );
    let function = T::new(
        K::Function {
            parameters: vec![
                Port::new(variable.clone(), false),
                Port::new(primitive(P::String), true),
            ],
            returns: Box::new(Port::new(variable.clone(), false)),
        },
        C::Signature,
        &limits,
    )
    .unwrap();
    check_type(&function, C::Signature);
    let K::Function {
        parameters,
        returns,
    } = function.kind()
    else {
        panic!()
    };
    assert_eq!(parameters[0].value_type(), &variable);
    assert!(!parameters[0].required());
    assert!(parameters[1].required());
    assert!(!returns.required());
    let zero = T::new(
        K::Function {
            parameters: vec![],
            returns: Box::new(Port::new(primitive(P::Null), true)),
        },
        C::Signature,
        &limits,
    )
    .unwrap();
    check_type(&zero, C::Signature);
    for (ty, tag) in [(variable.clone(), "var"), (function, "function")] {
        assert_eq!(
            ty.encode(C::Value, &limits).unwrap_err(),
            TypeError::SignatureOnly(tag)
        );
        let wire = ty.encode(C::Signature, &limits).unwrap();
        assert_eq!(
            T::decode(&wire, C::Value, &limits).unwrap_err(),
            TypeError::SignatureOnly(tag)
        );
        assert_eq!(
            Port::new(ty.clone(), true)
                .encode(C::Value, &limits)
                .unwrap_err(),
            TypeError::SignatureOnly(tag)
        );
        assert_eq!(
            T::new(K::List(Box::new(ty.clone())), C::Value, &limits).unwrap_err(),
            TypeError::SignatureOnly(tag)
        );
        let record = T::new(
            K::Record(vec![("x".into(), Port::new(ty, true))]),
            C::Signature,
            &limits,
        )
        .unwrap();
        let wire = record.encode(C::Signature, &limits).unwrap();
        assert_eq!(
            T::decode(&wire, C::Value, &limits).unwrap_err(),
            TypeError::SignatureOnly(tag)
        );
    }
}

#[test]
fn schema_references_and_malformed_records() {
    let limits = Limits::default();
    let digest = Digest::from_bytes([0; 32]);
    let schema = make(K::Schema(digest));
    assert_eq!(
        schema.to_value(C::Value, &limits).unwrap(),
        tagged("schema", text(&digest.to_string()))
    );
    check_type(&schema, C::Value);
    for value in [
        Value::Null,
        Value::Bool(true),
        Value::Integer(1),
        Value::Array(vec![]),
        Value::Array(vec![Value::Null]),
        Value::Array(vec![text("list")]),
        Value::Array(vec![text("list"), text("string"), text("string")]),
        tagged("record", Value::Array(vec![])),
        tagged("enum", Value::Array(vec![Value::Null])),
        tagged("schema", Value::Bytes(vec![0; 32])),
        tagged("map", Value::Null),
        Value::Array(vec![
            text("function"),
            Value::Null,
            port_value(text("null"), true),
        ]),
    ] {
        assert!(
            matches!(
                T::from_value(&value, C::Signature, &limits),
                Err(TypeError::InvalidShape(_))
            ),
            "{value:?}"
        );
    }
    assert_eq!(
        T::from_value(&tagged("private-input", text("string")), C::Value, &limits).unwrap_err(),
        TypeError::UnknownConstructor
    );
    let error = T::from_value(&tagged("var", text("BadName")), C::Signature, &limits).unwrap_err();
    assert!(matches!(error, TypeError::Identifier(_)));
    assert!(error.source().is_some());
    let error =
        T::from_value(&tagged("schema", text("private-input")), C::Value, &limits).unwrap_err();
    assert!(matches!(error, TypeError::Digest(_)));
    assert!(error.source().is_some());
    assert!(!error.to_string().contains("private-input"));
    for (fields, expected) in [
        (vec![], TypeError::MissingPortField("required")),
        (
            vec![("required".into(), Value::Bool(false))],
            TypeError::MissingPortField("type"),
        ),
        (
            vec![("other".into(), Value::Null)],
            TypeError::UnknownPortField,
        ),
        (
            vec![
                ("type".into(), text("string")),
                ("required".into(), Value::Null),
            ],
            TypeError::InvalidShape("required boolean"),
        ),
    ] {
        let record = Value::Map(Map::try_from_entries(fields).unwrap());
        assert_eq!(
            Port::from_value(&record, C::Value, &limits).unwrap_err(),
            expected
        );
    }
    assert!(matches!(
        Port::from_value(&Value::Null, C::Value, &limits),
        Err(TypeError::InvalidShape(_))
    ));
}

#[test]
fn conversion_and_ingress_honor_wire_limits() {
    let ty = make(K::Enum(vec!["aa".into(), "z".into()]));
    let limits = Limits {
        max_document_bytes: 12,
        max_text_bytes: 4,
        max_byte_string_bytes: 0,
        max_depth: 2,
        max_collection_entries: 2,
        max_total_values: 5,
        max_total_payload_bytes: 7,
    };
    let bytes = ty.encode(C::Value, &limits).unwrap();
    assert_eq!(bytes, b"\x82\x64enum\x82\x62aa\x61z");
    assert_eq!(T::decode(&bytes, C::Value, &limits).unwrap(), ty);
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 6] = [
        (LimitKind::DocumentBytes, 11, |l| l.max_document_bytes = 11),
        (LimitKind::TextBytes, 3, |l| l.max_text_bytes = 3),
        (LimitKind::Depth, 1, |l| l.max_depth = 1),
        (LimitKind::CollectionEntries, 1, |l| {
            l.max_collection_entries = 1
        }),
        (LimitKind::TotalValues, 4, |l| l.max_total_values = 4),
        (LimitKind::TotalPayloadBytes, 6, |l| {
            l.max_total_payload_bytes = 6
        }),
    ];
    for (limit, maximum, adjust) in cases {
        let mut tight = limits.clone();
        adjust(&mut tight);
        assert_eq!(
            ty.to_value(C::Value, &tight).unwrap_err(),
            TypeError::LimitExceeded { limit, maximum }
        );
        assert_eq!(
            T::new(ty.kind().clone(), C::Value, &tight).unwrap_err(),
            TypeError::LimitExceeded { limit, maximum }
        );
        let TypeError::Codec(error) = T::decode(&bytes, C::Value, &tight).unwrap_err() else {
            panic!()
        };
        assert_eq!(
            error.kind(),
            &htlk_cbor::ErrorKind::LimitExceeded { limit, maximum }
        );
    }
    // A type named bytes is text metadata, not a byte-string payload.
    assert!(
        primitive(P::Bytes)
            .encode(
                C::Value,
                &Limits {
                    max_byte_string_bytes: 0,
                    ..Limits::default()
                }
            )
            .is_ok()
    );
    let port = Port::new(primitive(P::String), false);
    let tight = Limits {
        max_document_bytes: 23,
        max_text_bytes: 8,
        max_byte_string_bytes: 0,
        max_depth: 1,
        max_collection_entries: 2,
        max_total_values: 5,
        max_total_payload_bytes: 18,
    };
    let bytes = port.encode(C::Value, &tight).unwrap();
    assert_eq!(bytes.len(), 23);
    assert_eq!(Port::decode(&bytes, C::Value, &tight).unwrap(), port);
    assert!(matches!(
        port.to_value(
            C::Value,
            &Limits {
                max_document_bytes: 22,
                ..tight
            }
        ),
        Err(TypeError::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
    let bad = Limits {
        max_depth: 129,
        ..Limits::default()
    };
    for error in [
        ty.encode(C::Value, &bad).unwrap_err(),
        T::decode(&[], C::Value, &bad).unwrap_err(),
        port.encode(C::Value, &bad).unwrap_err(),
        Port::decode(&[], C::Value, &bad).unwrap_err(),
    ] {
        let TypeError::Codec(error) = error else {
            panic!()
        };
        assert_eq!(error.kind(), &htlk_cbor::ErrorKind::InvalidLimits);
    }
}

#[test]
fn flattened_union_must_fit_its_final_collection_limit() {
    let a = make(K::Union(vec![primitive(P::String), primitive(P::Boolean)]));
    let b = make(K::Union(vec![primitive(P::Null), primitive(P::Integer)]));
    let tight = Limits {
        max_collection_entries: 2,
        ..Limits::default()
    };
    assert!(matches!(
        T::new(K::Union(vec![a, b]), C::Value, &tight),
        Err(TypeError::LimitExceeded {
            limit: LimitKind::CollectionEntries,
            maximum: 2
        })
    ));
}

#[test]
fn conversion_size_checks_match_cbor_header_boundaries() {
    for length in [23, 24, 255, 256, 65535, 65536] {
        let ty = make(K::Enum(vec!["a".repeat(length)]));
        let bytes = ty.encode(C::Value, &Limits::default()).unwrap();
        let exact = Limits {
            max_document_bytes: bytes.len(),
            ..Limits::default()
        };
        assert_eq!(ty.encode(C::Value, &exact).unwrap(), bytes);
        assert_eq!(T::decode(&bytes, C::Value, &exact).unwrap(), ty);
        assert!(matches!(
            ty.to_value(
                C::Value,
                &Limits {
                    max_document_bytes: bytes.len() - 1,
                    ..exact
                }
            ),
            Err(TypeError::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                ..
            })
        ));
    }
    for count in [23, 24, 255, 256] {
        let ty = make(K::Enum((0..count).map(|i| format!("{i:05}")).collect()));
        let bytes = ty.encode(C::Value, &Limits::default()).unwrap();
        let exact = Limits {
            max_document_bytes: bytes.len(),
            ..Limits::default()
        };
        assert_eq!(ty.encode(C::Value, &exact).unwrap(), bytes);
        assert!(
            ty.to_value(
                C::Value,
                &Limits {
                    max_document_bytes: bytes.len() - 1,
                    ..exact
                }
            )
            .is_err()
        );
    }
}

#[test]
fn codec_errors_retain_offsets_and_partial_types_are_not_published() {
    let valid = b"\x82\x64list\x66string";
    for len in 0..valid.len() {
        let error = T::decode(&valid[..len], C::Value, &Limits::default()).unwrap_err();
        let TypeError::Codec(ref codec) = error else {
            panic!()
        };
        assert_eq!(codec.offset(), Some(len));
        assert!(error.source().is_some());
    }
    let mut trailing = valid.to_vec();
    trailing.push(0);
    let TypeError::Codec(error) = T::decode(&trailing, C::Value, &Limits::default()).unwrap_err()
    else {
        panic!()
    };
    assert_eq!(error.kind(), &htlk_cbor::ErrorKind::TrailingData);
    let value = port_value(text("string"), true);
    let mut bytes = htlk_cbor::encode(&value, &Limits::default()).unwrap();
    bytes.push(0);
    assert!(matches!(
        Port::decode(&bytes, C::Value, &Limits::default()),
        Err(TypeError::Codec(_))
    ));
    assert!(T::decode(valid, C::Value, &Limits::default()).is_ok());
}
