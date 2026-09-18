//! Independent wire vectors and bounded encoding behavior.

use htlk_executable::cbor::{
    ErrorKind, FiniteFloat, LimitKind, Limits, Map, Value, decode, encode,
};

fn encoded(value: Value) -> Vec<u8> {
    let bytes = encode(&value, &Limits::default()).unwrap();
    let decoded = decode(&bytes, &Limits::default()).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(encode(&decoded, &Limits::default()).unwrap(), bytes);
    bytes
}

fn float(value: f64) -> Value {
    Value::Float(FiniteFloat::new(value).unwrap())
}

#[test]
fn scalar_vectors() {
    // Compatible finite/scalar examples from RFC 8949 Appendix A, plus i64 extrema.
    let vectors: Vec<(Value, &[u8])> = vec![
        (Value::Null, &[0xf6]),
        (Value::Bool(false), &[0xf4]),
        (Value::Bool(true), &[0xf5]),
        (Value::Integer(0), &[0x00]),
        (Value::Integer(23), &[0x17]),
        (Value::Integer(24), &[0x18, 0x18]),
        (Value::Integer(255), &[0x18, 0xff]),
        (Value::Integer(256), &[0x19, 1, 0]),
        (Value::Integer(65535), &[0x19, 0xff, 0xff]),
        (Value::Integer(65536), &[0x1a, 0, 1, 0, 0]),
        (Value::Integer(0xffff_ffff), &[0x1a, 0xff, 0xff, 0xff, 0xff]),
        (
            Value::Integer(0x1_0000_0000),
            &[0x1b, 0, 0, 0, 1, 0, 0, 0, 0],
        ),
        (
            Value::Integer(i64::MAX),
            &[0x1b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ),
        (Value::Integer(-1), &[0x20]),
        (Value::Integer(-24), &[0x37]),
        (Value::Integer(-25), &[0x38, 0x18]),
        (Value::Integer(-256), &[0x38, 0xff]),
        (Value::Integer(-257), &[0x39, 1, 0]),
        (Value::Integer(-65536), &[0x39, 0xff, 0xff]),
        (Value::Integer(-65537), &[0x3a, 0, 1, 0, 0]),
        (
            Value::Integer(-0x1_0000_0000),
            &[0x3a, 0xff, 0xff, 0xff, 0xff],
        ),
        (
            Value::Integer(-0x1_0000_0001),
            &[0x3b, 0, 0, 0, 1, 0, 0, 0, 0],
        ),
        (
            Value::Integer(i64::MIN),
            &[0x3b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ),
        (Value::Text(String::new()), &[0x60]),
        (Value::Text("ü".into()), &[0x62, 0xc3, 0xbc]),
        (Value::Text("e\u{301}".into()), &[0x63, 0x65, 0xcc, 0x81]),
        (Value::Bytes(vec![]), &[0x40]),
        (Value::Bytes(vec![0, 0xff]), &[0x42, 0, 0xff]),
    ];
    for (value, bytes) in vectors {
        assert_eq!(encoded(value), bytes);
    }
}

#[test]
fn float_vectors_are_shortest_and_exact() {
    let vectors: Vec<(f64, &[u8])> = vec![
        (0.0, &[0xf9, 0, 0]),
        (-0.0, &[0xf9, 0, 0]),
        (1.0, &[0xf9, 0x3c, 0]),
        (1.5, &[0xf9, 0x3e, 0]),
        (-4.0, &[0xf9, 0xc4, 0]),
        (65504.0, &[0xf9, 0x7b, 0xff]),
        (2f64.powi(-24), &[0xf9, 0, 1]),
        (2f64.powi(-14), &[0xf9, 4, 0]),
        (2f64.powi(-14) - 2f64.powi(-24), &[0xf9, 3, 0xff]),
        (2f64.powi(-25), &[0xfa, 0x33, 0, 0, 0]),
        (65536.0, &[0xfa, 0x47, 0x80, 0, 0]),
        (100000.0, &[0xfa, 0x47, 0xc3, 0x50, 0]),
        (1.0 + 2f64.powi(-23), &[0xfa, 0x3f, 0x80, 0, 1]),
        (f64::from(f32::from_bits(1)), &[0xfa, 0, 0, 0, 1]),
        (f64::from(f32::MAX), &[0xfa, 0x7f, 0x7f, 0xff, 0xff]),
        (
            1.0 + 2f64.powi(-24),
            &[0xfb, 0x3f, 0xf0, 0, 0, 0x10, 0, 0, 0],
        ),
        (1.1, &[0xfb, 0x3f, 0xf1, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a]),
        (f64::from_bits(1), &[0xfb, 0, 0, 0, 0, 0, 0, 0, 1]),
        (
            f64::MAX,
            &[0xfb, 0x7f, 0xef, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ),
    ];
    for (value, bytes) in vectors {
        assert_eq!(encoded(float(value)), bytes, "{value:?}");
    }
    // Adjacent binary64 values must not round to an apparently shorter width.
    for value in [1.0, 65504.0, 2f64.powi(-24), f64::from(f32::MAX)] {
        for bits in [value.to_bits() - 1, value.to_bits() + 1] {
            assert_eq!(encoded(float(f64::from_bits(bits)))[0], 0xfb);
        }
    }
}

#[test]
fn every_finite_binary16_pattern_has_the_expected_encoding() {
    // Independent binary16-to-f64 calculation; does not use the half helper.
    for bits in 0u16..=u16::MAX {
        let exponent = (bits >> 10) & 31;
        if exponent == 31 {
            continue;
        }
        let fraction = f64::from(bits & 1023);
        let magnitude = if exponent == 0 {
            fraction * 2f64.powi(-24)
        } else {
            (1024.0 + fraction) * 2f64.powi(i32::from(exponent) - 25)
        };
        let value = if bits & 0x8000 == 0 {
            magnitude
        } else {
            -magnitude
        };
        let normalized = if magnitude == 0.0 { 0 } else { bits };
        let [hi, lo] = normalized.to_be_bytes();
        assert_eq!(encoded(float(value)), [0xf9, hi, lo]);
    }
}

#[test]
fn length_headers_and_container_order() {
    for (len, suffix) in [
        (23, vec![23]),
        (24, vec![24, 24]),
        (255, vec![24, 255]),
        (256, vec![25, 1, 0]),
        (65535, vec![25, 255, 255]),
        (65536, vec![26, 0, 1, 0, 0]),
    ] {
        for (major, value, payload) in [
            (2, Value::Bytes(vec![0; len]), 0),
            (3, Value::Text("a".repeat(len)), b'a'),
            (4, Value::Array(vec![Value::Null; len]), 0xf6),
        ] {
            let bytes = encoded(value);
            let mut header = suffix.clone();
            header[0] |= major << 5;
            assert_eq!(&bytes[..header.len()], header);
            assert_eq!(bytes.len(), header.len() + len);
            assert!(bytes[header.len()..].iter().all(|byte| *byte == payload));
        }
    }
    assert_eq!(encoded(Value::Array(vec![])), [0x80]);
    assert_eq!(encoded(Value::Map(Map::new())), [0xa0]);
    let entries = [
        ("aa".into(), Value::Integer(2)),
        (
            "z".into(),
            Value::Array(vec![Value::Integer(1), Value::Null]),
        ),
    ];
    let forward = Value::Map(Map::try_from_entries(entries.clone()).unwrap());
    let reverse = Value::Map(Map::try_from_entries(entries.into_iter().rev()).unwrap());
    let bytes = encoded(forward);
    assert_eq!(
        bytes,
        [0xa2, 0x61, b'z', 0x82, 1, 0xf6, 0x62, b'a', b'a', 2]
    );
    assert_eq!(bytes, encoded(reverse));
}

#[test]
fn map_length_headers_and_utf8_key_length_boundaries() {
    for (count, expected) in [
        (23, vec![0xb7]),
        (24, vec![0xb8, 24]),
        (256, vec![0xb9, 1, 0]),
    ] {
        let map = Map::try_from_entries(
            (0..count)
                .rev()
                .map(|index| (format!("{index:03}"), Value::Null)),
        )
        .unwrap();
        let bytes = encoded(Value::Map(map));
        assert_eq!(&bytes[..expected.len()], expected);
        // Each three-byte key has a one-byte header and a one-byte null value.
        assert_eq!(bytes.len(), expected.len() + count * 5);
    }
    let short = "z".repeat(23);
    let long = "a".repeat(24);
    let map =
        Map::try_from_entries([(long.clone(), Value::Null), (short.clone(), Value::Null)]).unwrap();
    let mut expected = vec![0xa2, 0x77];
    expected.extend_from_slice(short.as_bytes());
    expected.extend_from_slice(&[0xf6, 0x78, 24]);
    expected.extend_from_slice(long.as_bytes());
    expected.push(0xf6);
    assert_eq!(encoded(Value::Map(map)), expected);
}

#[test]
fn unsupported_configuration_is_rejected_at_entry() {
    let error = encode(
        &Value::Null,
        &Limits {
            max_depth: 129,
            max_document_bytes: 0,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), &ErrorKind::InvalidLimits);
}

fn expect_limit(value: &Value, limits: &Limits, limit: LimitKind, maximum: usize) {
    let error = encode(value, limits).unwrap_err();
    assert_eq!(error.kind(), &ErrorKind::LimitExceeded { limit, maximum });
    assert_eq!(error.offset(), None);
}

#[test]
fn every_limit_accepts_the_boundary_and_rejects_one_more() {
    // Root map + two keys + two values = 5. Payload = 1 + 2 + 1 + 2 = 6 bytes.
    let value = Value::Map(
        Map::try_from_entries([
            ("a".into(), Value::Text("é".into())),
            ("b".into(), Value::Bytes(vec![0, 255])),
        ])
        .unwrap(),
    );
    let limits = Limits {
        max_document_bytes: 11,
        max_text_bytes: 2,
        max_byte_string_bytes: 2,
        max_depth: 1,
        max_collection_entries: 2,
        max_total_values: 5,
        max_total_payload_bytes: 6,
    };
    let bytes = encode(&value, &limits).unwrap();
    assert_eq!(bytes.len(), 11);
    assert_eq!(bytes, encode(&value, &Limits::default()).unwrap());
    type LimitCase = (LimitKind, usize, fn(&mut Limits));
    let cases: [LimitCase; 7] = [
        (LimitKind::DocumentBytes, 10, |l| l.max_document_bytes = 10),
        (LimitKind::TextBytes, 1, |l| l.max_text_bytes = 1),
        (LimitKind::ByteStringBytes, 1, |l| {
            l.max_byte_string_bytes = 1
        }),
        (LimitKind::Depth, 0, |l| l.max_depth = 0),
        (LimitKind::CollectionEntries, 1, |l| {
            l.max_collection_entries = 1
        }),
        (LimitKind::TotalValues, 4, |l| l.max_total_values = 4),
        (LimitKind::TotalPayloadBytes, 5, |l| {
            l.max_total_payload_bytes = 5
        }),
    ];
    for (kind, maximum, adjust) in cases {
        let mut tightened = limits.clone();
        adjust(&mut tightened);
        expect_limit(&value, &tightened, kind, maximum);
    }
}

#[test]
fn keys_containers_and_zero_budgets_are_counted() {
    let map = Value::Map(Map::try_from_entries([("é".into(), Value::Null)]).unwrap());
    expect_limit(
        &map,
        &Limits {
            max_text_bytes: 1,
            ..Limits::default()
        },
        LimitKind::TextBytes,
        1,
    );
    expect_limit(
        &map,
        &Limits {
            max_total_payload_bytes: 1,
            ..Limits::default()
        },
        LimitKind::TotalPayloadBytes,
        1,
    );
    for value in [Value::Null, Value::Array(vec![]), Value::Map(Map::new())] {
        expect_limit(
            &value,
            &Limits {
                max_total_values: 0,
                ..Limits::default()
            },
            LimitKind::TotalValues,
            0,
        );
        expect_limit(
            &value,
            &Limits {
                max_document_bytes: 0,
                ..Limits::default()
            },
            LimitKind::DocumentBytes,
            0,
        );
        assert!(
            encode(
                &value,
                &Limits {
                    max_depth: 0,
                    max_collection_entries: 0,
                    ..Limits::default()
                }
            )
            .is_ok()
        );
    }
    let array = Value::Array(vec![Value::Array(vec![])]);
    expect_limit(
        &array,
        &Limits {
            max_depth: 0,
            ..Limits::default()
        },
        LimitKind::Depth,
        0,
    );
    expect_limit(
        &array,
        &Limits {
            max_collection_entries: 0,
            ..Limits::default()
        },
        LimitKind::CollectionEntries,
        0,
    );
    expect_limit(
        &array,
        &Limits {
            max_total_values: 1,
            ..Limits::default()
        },
        LimitKind::TotalValues,
        1,
    );
    for value in [Value::Text(String::new()), Value::Bytes(vec![])] {
        assert!(
            encode(
                &value,
                &Limits {
                    max_text_bytes: 0,
                    max_byte_string_bytes: 0,
                    max_total_payload_bytes: 0,
                    ..Limits::default()
                }
            )
            .is_ok()
        );
    }
}
