//! Canonical execution-option records and strict validation boundaries.

use std::error::Error as _;

use htlk_cbor::{ErrorKind, FiniteFloat, LimitKind, Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{ExecutionLimits as E, ExecutionOptionsError as Error, RetryPolicy as R};

fn record(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(
        Map::try_from_entries(fields.into_iter().map(|(key, value)| (key.into(), value))).unwrap(),
    )
}
fn retry(attempts: Value, codes: Value, delays: Value) -> Value {
    record(vec![
        ("max_attempts", attempts),
        ("on", codes),
        ("backoff_ms", delays),
    ])
}
fn texts(codes: &[&str]) -> Value {
    Value::Array(
        codes
            .iter()
            .map(|code| Value::Text((*code).into()))
            .collect(),
    )
}
fn integers(values: &[i64]) -> Value {
    Value::Array(values.iter().map(|n| Value::Integer(*n)).collect())
}

#[test]
fn empty_limits_inherit_but_explicit_zero_is_preserved() {
    let limits = Limits::default();
    let empty = E::new();
    assert_eq!(empty, E::default());
    assert_eq!(empty.encode(&limits).unwrap(), [0xa0]);
    assert_eq!(E::decode(&[0xa0], &limits).unwrap(), empty);
    assert_eq!(empty.timeout_ms(), None);
    assert_eq!(empty.attempt_timeout_ms(), None);
    assert_eq!(empty.max_mcp_calls(), None);
    assert_eq!(empty.max_tokens(), None);
    assert_eq!(empty.max_cost_units(), None);
    assert_eq!(empty.max_concurrency(), None);
    let zero = E::new().with_max_mcp_calls(0).unwrap();
    assert_ne!(zero, empty);
    assert_eq!(zero.max_mcp_calls(), Some(0));
    assert_eq!(zero.encode(&limits).unwrap(), b"\xa1\x6dmax_mcp_calls\x00");
    assert_eq!(
        E::decode(&zero.encode(&limits).unwrap(), &limits).unwrap(),
        zero
    );
    let zero = zero
        .with_max_tokens(0)
        .unwrap()
        .with_max_cost_units(0)
        .unwrap();
    let value = zero.to_value(&limits).unwrap();
    assert_eq!(E::from_value(&value, &limits).unwrap(), zero);
    assert!(format!("{zero:?}").contains("max_mcp_calls: 0"));
}

#[test]
fn all_limit_fields_round_trip_and_getters_match() {
    let limits = Limits::default();
    let options = E::new()
        .with_timeout_ms(60000)
        .unwrap()
        .with_attempt_timeout_ms(30000)
        .unwrap()
        .with_max_mcp_calls(3)
        .unwrap()
        .with_max_tokens(4000)
        .unwrap()
        .with_max_cost_units(500)
        .unwrap()
        .with_max_concurrency(2)
        .unwrap();
    assert_eq!(options.timeout_ms(), Some(60000));
    assert_eq!(options.attempt_timeout_ms(), Some(30000));
    assert_eq!(options.max_mcp_calls(), Some(3));
    assert_eq!(options.max_tokens(), Some(4000));
    assert_eq!(options.max_cost_units(), Some(500));
    assert_eq!(options.max_concurrency(), Some(2));
    let expected = record(vec![
        ("timeout_ms", Value::Integer(60000)),
        ("attempt_timeout_ms", Value::Integer(30000)),
        ("max_mcp_calls", Value::Integer(3)),
        ("max_tokens", Value::Integer(4000)),
        ("max_cost_units", Value::Integer(500)),
        ("max_concurrency", Value::Integer(2)),
    ]);
    assert_eq!(options.to_value(&limits).unwrap(), expected);
    assert_eq!(E::from_value(&expected, &limits).unwrap(), options);
    assert_eq!(
        E::decode(&options.encode(&limits).unwrap(), &limits).unwrap(),
        options
    );
    let changed = options.clone().with_timeout_ms(1).unwrap();
    assert_eq!(changed.timeout_ms(), Some(1));
    assert_eq!(options.timeout_ms(), Some(60000));
    assert_eq!(
        E::new()
            .with_timeout_ms(1000)
            .unwrap()
            .encode(&limits)
            .unwrap(),
        b"\xa1\x6atimeout_ms\x19\x03\xe8"
    );
}

#[test]
fn numeric_ranges_apply_without_coercion() {
    type Setter = fn(E, u64) -> Result<E, Error>;
    let fields: [(&str, bool, Setter); 6] = [
        ("timeout_ms", true, E::with_timeout_ms),
        ("attempt_timeout_ms", true, E::with_attempt_timeout_ms),
        ("max_concurrency", true, E::with_max_concurrency),
        ("max_mcp_calls", false, E::with_max_mcp_calls),
        ("max_tokens", false, E::with_max_tokens),
        ("max_cost_units", false, E::with_max_cost_units),
    ];
    let limits = Limits::default();
    for (field, positive, set) in fields {
        for value in [1, i64::MAX as u64] {
            let options = set(E::new(), value).unwrap();
            assert_eq!(
                E::decode(&options.encode(&limits).unwrap(), &limits).unwrap(),
                options
            );
        }
        for value in [i64::MAX as u64 + 1, u64::MAX] {
            assert_eq!(set(E::new(), value).unwrap_err(), Error::OutOfRange(field));
        }
        if positive {
            assert_eq!(set(E::new(), 0).unwrap_err(), Error::ZeroNotAllowed(field));
            assert_eq!(
                E::from_value(&record(vec![(field, Value::Integer(0))]), &limits).unwrap_err(),
                Error::ZeroNotAllowed(field)
            );
        } else {
            assert!(set(E::new(), 0).is_ok());
        }
        assert_eq!(
            E::from_value(&record(vec![(field, Value::Integer(-1))]), &limits).unwrap_err(),
            Error::OutOfRange(field)
        );
        for value in [
            Value::Null,
            Value::Bool(true),
            Value::Text("1".into()),
            Value::Float(FiniteFloat::new(1.0).unwrap()),
            Value::Array(vec![]),
        ] {
            assert_eq!(
                E::from_value(&record(vec![(field, value)]), &limits).unwrap_err(),
                Error::InvalidFieldType(field)
            );
        }
    }
}

#[test]
fn limits_reject_unknown_fields_with_stable_precedence() {
    let limits = Limits::default();
    assert_eq!(
        E::from_value(&Value::Null, &limits).unwrap_err(),
        Error::ExpectedRecord("execution limits")
    );
    let value = record(vec![
        ("unknown", Value::Null),
        ("attempt_timeout_ms", Value::Null),
    ]);
    assert_eq!(
        E::from_value(&value, &limits).unwrap_err(),
        Error::UnknownField("execution limits")
    );
    let value = record(vec![
        ("timeout_ms", Value::Integer(-1)),
        ("attempt_timeout_ms", Value::Null),
    ]);
    assert_eq!(
        E::from_value(&value, &limits).unwrap_err(),
        Error::InvalidFieldType("attempt_timeout_ms")
    );
}

#[test]
fn retry_golden_bytes_and_no_retry_default() {
    let limits = Limits::default();
    let none = R::no_retry();
    assert_eq!(none, R::default());
    assert_eq!(none.max_attempts(), 1);
    assert!(none.on().is_empty());
    assert!(none.backoff_ms().is_empty());
    assert_eq!(
        none.encode(&limits).unwrap(),
        b"\xa3\x62on\x80\x6abackoff_ms\x80\x6cmax_attempts\x01"
    );
    let policy = R::new(
        3,
        vec![
            "MCP_TRANSPORT".into(),
            "MCP_TIMEOUT".into(),
            "MCP_TIMEOUT".into(),
        ],
        vec![1000, 5000],
        &limits,
    )
    .unwrap();
    assert_eq!(policy.on(), ["MCP_TIMEOUT", "MCP_TRANSPORT"]);
    assert_eq!(policy.backoff_ms(), [1000, 5000]);
    let expected = b"\xa3\x62on\x82\x6bMCP_TIMEOUT\x6dMCP_TRANSPORT\x6abackoff_ms\x82\x19\x03\xe8\x19\x13\x88\x6cmax_attempts\x03";
    assert_eq!(policy.encode(&limits).unwrap(), expected);
    assert_eq!(R::decode(expected, &limits).unwrap(), policy);
    assert_eq!(
        R::from_value(&policy.to_value(&limits).unwrap(), &limits).unwrap(),
        policy
    );
}

#[test]
fn retry_attempts_and_delay_cardinality_are_exact() {
    let limits = Limits::default();
    assert_eq!(
        R::new(0, vec![], vec![], &limits).unwrap_err(),
        Error::ZeroNotAllowed("max_attempts")
    );
    assert_eq!(
        R::new(u64::MAX, vec![], vec![], &limits).unwrap_err(),
        Error::OutOfRange("max_attempts")
    );
    for (attempts, delays, expected) in [(1, vec![0], 0), (3, vec![], 2), (2, vec![0, 1], 1)] {
        assert_eq!(
            R::new(attempts, vec![], delays.clone(), &limits).unwrap_err(),
            Error::DelayCountMismatch {
                expected,
                actual: delays.len()
            }
        );
    }
    // A huge claimed count with no matching vector fails without allocating from it.
    assert_eq!(
        R::new(i64::MAX as u64, vec![], vec![], &limits).unwrap_err(),
        Error::DelayCountMismatch {
            expected: i64::MAX as u64 - 1,
            actual: 0
        }
    );
    assert_eq!(
        R::new(2, vec![], vec![u64::MAX], &limits).unwrap_err(),
        Error::OutOfRange("backoff_ms item")
    );
    let policy = R::new(3, vec![], vec![i64::MAX as u64, 0], &limits).unwrap();
    assert_eq!(policy.backoff_ms(), [i64::MAX as u64, 0]);
    assert_eq!(
        R::decode(&policy.encode(&limits).unwrap(), &limits).unwrap(),
        policy
    );
    let invalid = retry(Value::Integer(-1), texts(&[]), integers(&[]));
    assert_eq!(
        R::from_value(&invalid, &limits).unwrap_err(),
        Error::OutOfRange("max_attempts")
    );
    let invalid = retry(Value::Integer(2), texts(&[]), integers(&[-1]));
    assert_eq!(
        R::from_value(&invalid, &limits).unwrap_err(),
        Error::OutOfRange("backoff_ms item")
    );
}

#[test]
fn retry_code_set_normalizes_only_during_authored_construction() {
    let limits = Limits::default();
    let policy = R::new(
        1,
        vec![
            "z".into(),
            "aa".into(),
            "".into(),
            "é".into(),
            "e\u{301}".into(),
            "z".into(),
        ],
        vec![],
        &limits,
    )
    .unwrap();
    assert_eq!(policy.on(), ["", "aa", "e\u{301}", "z", "é"]);
    for codes in [texts(&["z", "aa"]), texts(&["a", "a"])] {
        let value = retry(Value::Integer(1), codes, integers(&[]));
        let bytes = htlk_cbor::encode(&value, &limits).unwrap();
        assert_eq!(
            R::from_value(&value, &limits).unwrap_err(),
            Error::NonCanonicalCodes
        );
        assert_eq!(
            R::decode(&bytes, &limits).unwrap_err(),
            Error::NonCanonicalCodes
        );
    }
}

#[test]
fn retry_fields_are_required_and_typed() {
    let limits = Limits::default();
    assert_eq!(
        R::from_value(&Value::Null, &limits).unwrap_err(),
        Error::ExpectedRecord("retry policy")
    );
    assert_eq!(
        R::from_value(&record(vec![]), &limits).unwrap_err(),
        Error::MissingField("backoff_ms")
    );
    assert_eq!(
        R::from_value(&record(vec![("idempotent", Value::Bool(true))]), &limits).unwrap_err(),
        Error::UnknownField("retry policy")
    );
    let full = [
        ("max_attempts", Value::Integer(1)),
        ("on", texts(&[])),
        ("backoff_ms", integers(&[])),
    ];
    for field in ["max_attempts", "on", "backoff_ms"] {
        let missing = record(
            full.iter()
                .filter(|(key, _)| *key != field)
                .cloned()
                .collect(),
        );
        assert_eq!(
            R::from_value(&missing, &limits).unwrap_err(),
            Error::MissingField(field)
        );
        let wrong = record(
            full.iter()
                .map(|(key, value)| {
                    (
                        *key,
                        if *key == field {
                            Value::Null
                        } else {
                            value.clone()
                        },
                    )
                })
                .collect(),
        );
        assert_eq!(
            R::from_value(&wrong, &limits).unwrap_err(),
            Error::InvalidFieldType(field)
        );
    }
    for value in [
        Value::Bool(true),
        Value::Text("1".into()),
        Value::Float(FiniteFloat::new(1.0).unwrap()),
    ] {
        assert_eq!(
            R::from_value(&retry(value, texts(&[]), integers(&[])), &limits).unwrap_err(),
            Error::InvalidFieldType("max_attempts")
        );
    }
    assert_eq!(
        R::from_value(
            &retry(Value::Integer(1), integers(&[1]), integers(&[])),
            &limits
        )
        .unwrap_err(),
        Error::InvalidFieldType("on item")
    );
    assert_eq!(
        R::from_value(
            &retry(Value::Integer(2), texts(&[]), texts(&["1"])),
            &limits
        )
        .unwrap_err(),
        Error::InvalidFieldType("backoff_ms item")
    );
}

#[test]
fn wire_limits_bound_both_records_and_retry_normalization() {
    let base = Limits {
        max_document_bytes: 63,
        max_depth: 2,
    };
    let policy = R::new(
        3,
        vec!["MCP_TIMEOUT".into(), "MCP_TRANSPORT".into()],
        vec![1000, 5000],
        &base,
    )
    .unwrap();
    let bytes = policy.encode(&base).unwrap();
    assert_eq!(bytes.len(), 63);
    assert_eq!(R::decode(&bytes, &base).unwrap(), policy);
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 2] = [
        (LimitKind::DocumentBytes, 62, |l| l.max_document_bytes = 62),
        (LimitKind::Depth, 1, |l| l.max_depth = 1),
    ];
    for (limit, maximum, adjust) in cases {
        let mut tight = base.clone();
        adjust(&mut tight);
        assert_eq!(
            policy.to_value(&tight).unwrap_err(),
            Error::LimitExceeded { limit, maximum }
        );
        assert_eq!(
            R::new(
                3,
                policy.on().to_vec(),
                policy.backoff_ms().to_vec(),
                &tight
            )
            .unwrap_err(),
            Error::LimitExceeded { limit, maximum }
        );
        let Error::Codec(error) = R::decode(&bytes, &tight).unwrap_err() else {
            panic!()
        };
        assert_eq!(error.kind(), &ErrorKind::LimitExceeded { limit, maximum });
    }
    assert!(matches!(
        R::new(
            3,
            vec![
                "MCP_TIMEOUT".into(),
                "MCP_TIMEOUT".into(),
                "MCP_TRANSPORT".into()
            ],
            vec![1000, 5000],
            &base
        ),
        Err(Error::LimitExceeded { .. })
    ));
    let empty_limits = Limits {
        max_document_bytes: 1,
        max_depth: 0,
    };
    assert_eq!(E::new().encode(&empty_limits).unwrap(), [0xa0]);
    assert!(matches!(
        E::new().with_max_tokens(0).unwrap().to_value(&empty_limits),
        Err(Error::LimitExceeded { .. })
    ));
    let one = E::new().with_max_mcp_calls(0).unwrap();
    let exact = Limits {
        max_document_bytes: 16,
        max_depth: 1,
    };
    assert_eq!(one.encode(&exact).unwrap().len(), 16);
    assert_eq!(
        E::decode(&one.encode(&exact).unwrap(), &exact).unwrap(),
        one
    );
    assert_eq!(
        one.to_value(&Limits {
            max_document_bytes: 15,
            ..exact
        })
        .unwrap_err(),
        Error::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: 15
        }
    );
}

#[test]
fn every_limit_conversion_ceiling_is_checked_before_building_the_map() {
    let options = E::new().with_max_mcp_calls(0).unwrap();
    let exact = Limits {
        max_document_bytes: 16,
        max_depth: 1,
    };
    let bytes = options.encode(&exact).unwrap();
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 2] = [
        (LimitKind::DocumentBytes, 15, |l| l.max_document_bytes = 15),
        (LimitKind::Depth, 0, |l| l.max_depth = 0),
    ];
    for (limit, maximum, adjust) in cases {
        let mut tight = exact.clone();
        adjust(&mut tight);
        assert_eq!(
            options.to_value(&tight).unwrap_err(),
            Error::LimitExceeded { limit, maximum }
        );
        let Error::Codec(error) = E::decode(&bytes, &tight).unwrap_err() else {
            panic!()
        };
        assert_eq!(error.kind(), &ErrorKind::LimitExceeded { limit, maximum });
    }
    assert_eq!(options.encode(&exact).unwrap(), bytes);
}

#[test]
fn codec_failures_preserve_offsets_and_errors_do_not_leak_data() {
    let limits = Limits::default();
    let policy = R::no_retry().encode(&limits).unwrap();
    for end in 0..policy.len() {
        let error = R::decode(&policy[..end], &limits).unwrap_err();
        let Error::Codec(ref codec) = error else {
            panic!()
        };
        assert_eq!(codec.kind(), &ErrorKind::UnexpectedEnd);
        assert_eq!(codec.offset(), Some(end));
        assert!(error.source().is_some());
    }
    let bad = Limits {
        max_depth: 129,
        ..limits.clone()
    };
    for error in [
        E::new().encode(&bad).unwrap_err(),
        E::decode(&[], &bad).unwrap_err(),
        R::default().encode(&bad).unwrap_err(),
        R::decode(&[], &bad).unwrap_err(),
        R::new(1, vec![], vec![], &bad).unwrap_err(),
    ] {
        let Error::Codec(error) = error else { panic!() };
        assert_eq!(error.kind(), &ErrorKind::InvalidLimits);
    }
    for bytes in [
        vec![0xa0, 0],
        vec![0xb8, 0],
        vec![0xa2, 0x61, b'x', 0, 0x61, b'x', 0],
    ] {
        let expected = htlk_cbor::decode(&bytes, &limits).unwrap_err();
        assert_eq!(
            E::decode(&bytes, &limits).unwrap_err(),
            Error::Codec(expected.clone())
        );
        assert_eq!(
            R::decode(&bytes, &limits).unwrap_err(),
            Error::Codec(expected)
        );
    }
    let error = E::from_value(&record(vec![("private-input", Value::Null)]), &limits).unwrap_err();
    assert!(!error.to_string().contains("private-input"));
    assert!(!format!("{error:?}").contains("private-input"));
}

#[test]
fn integer_and_array_header_boundaries_match_codec_sizes() {
    for value in [
        0,
        23,
        24,
        255,
        256,
        65535,
        65536,
        0xffff_ffff,
        0x1_0000_0000,
        i64::MAX as u64,
    ] {
        let options = E::new().with_max_tokens(value).unwrap();
        let bytes = options.encode(&Limits::default()).unwrap();
        let exact = Limits {
            max_document_bytes: bytes.len(),
            ..Limits::default()
        };
        assert_eq!(options.encode(&exact).unwrap(), bytes);
        assert!(matches!(
            options.encode(&Limits {
                max_document_bytes: bytes.len() - 1,
                ..exact
            }),
            Err(Error::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                ..
            })
        ));
    }
    for count in [23, 24, 255, 256] {
        let policy = R::new(
            count + 1,
            (0..count).map(|i| format!("code_{i:03}")).collect(),
            vec![0; count as usize],
            &Limits::default(),
        )
        .unwrap();
        let bytes = policy.encode(&Limits::default()).unwrap();
        let exact = Limits {
            max_document_bytes: bytes.len(),
            ..Limits::default()
        };
        assert_eq!(policy.encode(&exact).unwrap(), bytes);
        assert_eq!(R::decode(&bytes, &exact).unwrap(), policy);
        assert!(matches!(
            policy.encode(&Limits {
                max_document_bytes: bytes.len() - 1,
                ..exact
            }),
            Err(Error::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                ..
            })
        ));
    }
}
