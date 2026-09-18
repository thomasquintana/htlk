//! Expression shape, canonicalization, reference contexts, and limits.

use htlk_cbor::{FiniteFloat, LimitKind, Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::digest::Digest;
use htlk_executable::{
    BinaryOperator as B, CoreFunction as F, Expression as E, ExpressionContext as C,
    ExpressionError as Error, ExpressionKind as K, FunctionId, PathStep as P, ScalarLiteral as S,
    ValueReference as R,
};

fn make(kind: K) -> E {
    E::new(kind, C::LoopUntil, &Limits::default()).unwrap()
}
fn literal(n: i64) -> E {
    E::literal(S::Integer(n))
}
fn text(s: &str) -> Value {
    Value::Text(s.into())
}
fn array(v: Vec<Value>) -> Value {
    Value::Array(v)
}
fn wire(e: &E) -> Value {
    e.to_value(C::LoopUntil, &Limits::default()).unwrap()
}
fn read(v: &Value) -> Result<E, Error> {
    E::from_value(v, C::LoopUntil, &Limits::default())
}
fn round_trip(e: &E, context: C) {
    let limits = Limits::default();
    let bytes = e.encode(context, &limits).unwrap();
    assert_eq!(E::decode(&bytes, context, &limits).unwrap(), *e);
    assert_eq!(
        E::from_value(&e.to_value(context, &limits).unwrap(), context, &limits).unwrap(),
        *e
    );
}

#[test]
fn scalar_literals_have_exact_native_encodings() {
    for (value, expected) in [
        (S::Null, &b"\x82\x67literal\xf6"[..]),
        (S::Boolean(false), &b"\x82\x67literal\xf4"[..]),
        (S::Integer(-1), &b"\x82\x67literal\x20"[..]),
        (S::String("abc".into()), &b"\x82\x67literal\x63abc"[..]),
        (S::Bytes(vec![0, 255]), &b"\x82\x67literal\x42\x00\xff"[..]),
        (
            S::Float(FiniteFloat::new(1.5).unwrap()),
            &b"\x82\x67literal\xf9\x3e\x00"[..],
        ),
    ] {
        let e = E::literal(value);
        assert_eq!(e.encode(C::Eval, &Limits::default()).unwrap(), expected);
        round_trip(&e, C::Eval);
    }
    for value in [i64::MIN, i64::MAX] {
        round_trip(&literal(value), C::Eval);
    }
    for value in [0.0, -0.0, 100000.0, 1.1, f64::from_bits(1), f64::MAX] {
        let e = E::literal(S::Float(FiniteFloat::new(value).unwrap()));
        round_trip(&e, C::Eval);
        let bytes = e.encode(C::Eval, &Limits::default()).unwrap();
        let limits = Limits {
            max_document_bytes: bytes.len(),
            ..Limits::default()
        };
        assert!(e.encode(C::Eval, &limits).is_ok());
        assert!(matches!(
            e.to_value(
                C::Eval,
                &Limits {
                    max_document_bytes: bytes.len() - 1,
                    ..limits
                }
            ),
            Err(Error::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                ..
            })
        ));
    }
    assert!(read(&array(vec![text("literal"), array(vec![])])).is_err());
    assert!(read(&array(vec![text("literal"), Value::Map(Map::new())])).is_err());
}

#[test]
fn references_and_get_paths_normalize_without_evaluation() {
    let reference = make(K::Ref {
        source: R::Input("question".parse().unwrap()),
        path: vec![],
    });
    assert_eq!(
        reference.encode(C::Eval, &Limits::default()).unwrap(),
        b"\x83\x63ref\x82\x65input\x68question\x80"
    );
    let selected = make(K::Get {
        value: Box::new(reference),
        path: vec![P::Field("customerId".into()), P::Index(0)],
    });
    let selected = make(K::Get {
        value: Box::new(selected),
        path: vec![P::Field("".into())],
    });
    let K::Ref { path, .. } = selected.kind() else {
        panic!()
    };
    assert_eq!(
        path,
        &[
            P::Field("customerId".into()),
            P::Index(0),
            P::Field("".into())
        ]
    );
    round_trip(&selected, C::Eval);
    let error = make(K::Error("worker".parse().unwrap()));
    let get = make(K::Get {
        value: Box::new(error),
        path: vec![P::Field("code".into())],
    });
    assert_eq!(
        get.encode(C::LoopUntil, &Limits::default()).unwrap(),
        b"\x83\x63get\x82\x65error\x66worker\x81\x64code"
    );
    let nested = make(K::Get {
        value: Box::new(get.clone()),
        path: vec![P::Index(1)],
    });
    let K::Get { value, path } = nested.kind() else {
        panic!()
    };
    assert!(matches!(value.kind(), K::Error(_)));
    assert_eq!(path.len(), 2);
    let raw_nested = array(vec![
        text("get"),
        wire(&get),
        array(vec![Value::Integer(1)]),
    ]);
    assert!(matches!(read(&raw_nested), Err(Error::NonCanonical(_))));
    assert!(
        E::new(
            K::Get {
                value: Box::new(literal(1)),
                path: vec![]
            },
            C::Eval,
            &Limits::default()
        )
        .is_err()
    );
}

#[test]
fn records_sort_utf8_but_operands_and_lists_keep_order() {
    let e = make(K::Record(vec![
        ("z".into(), literal(2)),
        ("aa".into(), literal(1)),
    ]));
    let K::Record(fields) = e.kind() else {
        panic!()
    };
    assert_eq!(fields[0].0, "aa");
    assert_eq!(fields[1].0, "z");
    assert_eq!(
        wire(&e),
        array(vec![
            text("record"),
            array(vec![
                array(vec![text("aa"), wire(&literal(1))]),
                array(vec![text("z"), wire(&literal(2))]),
            ])
        ])
    );
    let unsorted = array(vec![
        text("record"),
        array(vec![
            array(vec![text("z"), wire(&literal(2))]),
            array(vec![text("aa"), wire(&literal(1))]),
        ]),
    ]);
    assert!(matches!(read(&unsorted), Err(Error::NonCanonical(_))));
    assert_eq!(
        E::new(
            K::Record(vec![("x".into(), literal(1)), ("x".into(), literal(2))]),
            C::Eval,
            &Limits::default()
        )
        .unwrap_err(),
        Error::DuplicateField
    );
    let list = make(K::List(vec![literal(2), literal(1)]));
    round_trip(&list, C::Eval);
    let K::List(items) = list.kind() else {
        panic!()
    };
    assert_eq!(items[0], literal(2));
    for op in [B::And, B::Or, B::Eq, B::Ne, B::Lt, B::Le, B::Gt, B::Ge] {
        let expression = make(K::Binary {
            operator: op,
            left: Box::new(literal(1)),
            right: Box::new(literal(2)),
        });
        round_trip(&expression, C::Eval);
        assert_eq!(
            wire(&expression),
            array(vec![
                text(op.as_str()),
                wire(&literal(1)),
                wire(&literal(2))
            ])
        );
    }
    let right = make(K::Status("worker".parse().unwrap()));
    let e = make(K::Binary {
        operator: B::And,
        left: Box::new(E::literal(S::Boolean(false))),
        right: Box::new(right.clone()),
    });
    let K::Binary {
        right: retained, ..
    } = e.kind()
    else {
        panic!()
    };
    assert_eq!(**retained, right);
}

#[test]
fn exact_function_and_template_identities_are_preserved() {
    let digest = Digest::from_bytes([1; 32]);
    let function = make(K::FunctionRef {
        library: digest,
        name: "check".parse().unwrap(),
    });
    assert_eq!(
        wire(&function),
        array(vec![
            text("function_ref"),
            text(&digest.to_string()),
            text("check")
        ])
    );
    let call = make(K::Call {
        function: FunctionId::Library {
            library: digest,
            name: "filter".parse().unwrap(),
        },
        arguments: vec![literal(2), function],
    });
    round_trip(&call, C::Eval);
    for function in [F::Length, F::Present] {
        round_trip(
            &make(K::Call {
                function: FunctionId::Core(function),
                arguments: vec![literal(1)],
            }),
            C::Eval,
        );
        assert!(
            E::new(
                K::Call {
                    function: FunctionId::Core(function),
                    arguments: vec![]
                },
                C::Eval,
                &Limits::default()
            )
            .is_err()
        );
    }
    let render = make(K::Render {
        template: digest,
        arguments: vec![
            ("z".parse().unwrap(), literal(2)),
            ("aa".parse().unwrap(), literal(1)),
        ],
    });
    let K::Render {
        arguments,
        template,
    } = render.kind()
    else {
        panic!()
    };
    assert_eq!(*template, digest);
    assert_eq!(arguments[0].0.as_str(), "aa");
    round_trip(&render, C::Eval);
    let invalid = array(vec![
        text("call"),
        array(vec![text("core"), text("status")]),
        array(vec![wire(&literal(1))]),
    ]);
    assert_eq!(read(&invalid).unwrap_err(), Error::UnknownFunction);
}

#[test]
fn regex_flags_normalize_only_for_authored_shapes() {
    let e = make(K::Regex {
        pattern: "a/b".into(),
        flags: "smi".into(),
    });
    assert_eq!(
        e.kind(),
        &K::Regex {
            pattern: "a/b".into(),
            flags: "ims".into()
        }
    );
    round_trip(&e, C::Eval);
    assert!(matches!(
        read(&array(vec![text("regex"), text("x"), text("mi")])),
        Err(Error::NonCanonical(_))
    ));
    for flags in ["ii", "g", "I", "i m"] {
        assert_eq!(
            E::new(
                K::Regex {
                    pattern: "x".into(),
                    flags: flags.into()
                },
                C::Eval,
                &Limits::default()
            )
            .unwrap_err(),
            Error::InvalidRegexFlags
        );
    }
    // A linked regex engine, not this format model, validates pattern syntax.
    assert!(
        E::new(
            K::Regex {
                pattern: "(".into(),
                flags: "".into()
            },
            C::Eval,
            &Limits::default()
        )
        .is_ok()
    );
}

#[test]
fn malformed_shapes_paths_and_identifiers_fail() {
    for value in [
        Value::Null,
        text("literal"),
        array(vec![]),
        array(vec![Value::Null]),
        array(vec![text("literal")]),
        array(vec![text("not"), wire(&literal(1)), wire(&literal(2))]),
        array(vec![text("ref"), array(vec![text("input"), text("x")])]),
        array(vec![text("record"), array(vec![array(vec![text("x")])])]),
    ] {
        assert!(matches!(read(&value), Err(Error::InvalidShape(_))));
    }
    assert_eq!(
        read(&array(vec![
            text("add"),
            wire(&literal(1)),
            wire(&literal(2))
        ]))
        .unwrap_err(),
        Error::UnknownConstructor
    );
    let reference = array(vec![text("input"), text("x")]);
    assert_eq!(
        read(&array(vec![
            text("ref"),
            reference.clone(),
            array(vec![Value::Integer(-1)])
        ]))
        .unwrap_err(),
        Error::InvalidIndex
    );
    assert!(
        E::new(
            K::Ref {
                source: R::Input("x".parse().unwrap()),
                path: vec![P::Index(u64::MAX)]
            },
            C::Eval,
            &Limits::default()
        )
        .is_err()
    );
    assert!(matches!(
        read(&array(vec![
            text("ref"),
            array(vec![text("input"), text("Bad")]),
            array(vec![])
        ])),
        Err(Error::Identifier(_))
    ));
    assert!(matches!(
        read(&array(vec![text("function_ref"), text("bad"), text("f")])),
        Err(Error::Digest(_))
    ));
    let e = make(K::Ref {
        source: R::Input("x".parse().unwrap()),
        path: vec![P::Index(i64::MAX as u64)],
    });
    round_trip(&e, C::Eval);
}

#[test]
fn conversion_and_ingress_respect_all_applicable_limits() {
    let e = E::literal(S::Bytes(vec![0, 255]));
    let exact = Limits {
        max_document_bytes: 12,
        max_text_bytes: 7,
        max_byte_string_bytes: 2,
        max_depth: 1,
        max_collection_entries: 2,
        max_total_values: 3,
        max_total_payload_bytes: 9,
    };
    let bytes = e.encode(C::Eval, &exact).unwrap();
    assert_eq!(bytes.len(), 12);
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 7] = [
        (LimitKind::DocumentBytes, 11, |l| l.max_document_bytes = 11),
        (LimitKind::TextBytes, 6, |l| l.max_text_bytes = 6),
        (LimitKind::ByteStringBytes, 1, |l| {
            l.max_byte_string_bytes = 1
        }),
        (LimitKind::Depth, 0, |l| l.max_depth = 0),
        (LimitKind::CollectionEntries, 1, |l| {
            l.max_collection_entries = 1
        }),
        (LimitKind::TotalValues, 2, |l| l.max_total_values = 2),
        (LimitKind::TotalPayloadBytes, 8, |l| {
            l.max_total_payload_bytes = 8
        }),
    ];
    for (limit, maximum, adjust) in cases {
        let mut limits = exact.clone();
        adjust(&mut limits);
        assert_eq!(
            e.to_value(C::Eval, &limits).unwrap_err(),
            Error::LimitExceeded { limit, maximum }
        );
        assert!(matches!(
            E::decode(&bytes, C::Eval, &limits),
            Err(Error::Codec(_))
        ));
    }
    for end in 0..bytes.len() {
        let Error::Codec(error) = E::decode(&bytes[..end], C::Eval, &exact).unwrap_err() else {
            panic!()
        };
        assert_eq!(error.offset(), Some(end));
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(matches!(
        E::decode(&trailing, C::Eval, &Limits::default()),
        Err(Error::Codec(_))
    ));
    let invalid = Limits {
        max_depth: 129,
        ..Limits::default()
    };
    assert!(matches!(
        e.to_value(C::Eval, &invalid),
        Err(Error::Codec(_))
    ));
}

#[test]
fn normalized_paths_must_fit_the_final_collection_limit() {
    let r = make(K::Ref {
        source: R::Input("x".parse().unwrap()),
        path: vec![P::Index(0), P::Index(1), P::Index(2)],
    });
    let limits = Limits {
        max_collection_entries: 3,
        ..Limits::default()
    };
    assert_eq!(
        E::new(
            K::Get {
                value: Box::new(r),
                path: vec![P::Index(3)]
            },
            C::Eval,
            &limits
        )
        .unwrap_err(),
        Error::LimitExceeded {
            limit: LimitKind::CollectionEntries,
            maximum: 3
        }
    );
}
