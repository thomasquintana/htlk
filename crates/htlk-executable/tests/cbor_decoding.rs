//! Strict ingress fixtures, resource ceilings, and bounded malformed-input checks.

use htlk_executable::cbor::{ErrorKind, LimitKind, Limits, Map, Value, decode, encode};

fn reject(bytes: &[u8], kind: ErrorKind, offset: usize) {
    let error = decode(bytes, &Limits::default()).unwrap_err();
    assert_eq!(error.kind(), &kind, "input {bytes:x?}");
    assert_eq!(error.offset(), Some(offset), "input {bytes:x?}");
}

#[test]
fn canonicality_prose_explains_decoder_rejections() {
    let cases: &[(&[u8], &str)] = &[
        (
            &[0x18, 0],
            "CBOR integer or length header is wider than necessary at byte 1.",
        ),
        (
            &[0x78, 1, b'a'],
            "CBOR integer or length header is wider than necessary at byte 1.",
        ),
        (
            &[0x5f, 0xff],
            "The CBOR header uses an indefinite-length marker, which HTLK does not allow at byte 1.",
        ),
        (
            &[0x1f],
            "The CBOR header uses an indefinite-length marker, which HTLK does not allow at byte 1.",
        ),
        (
            &[0xfa, 0x3f, 0xc0, 0, 0],
            "CBOR floating-point encoding is wider than necessary at byte 1.",
        ),
        (
            &[0xfb, 0x40, 0xf8, 0x6a, 0, 0, 0, 0, 0],
            "CBOR floating-point encoding is wider than necessary at byte 1.",
        ),
        (
            &[0xf9, 0x80, 0],
            "CBOR encodes negative zero, but canonical encoding requires positive zero at byte 1.",
        ),
        (
            &[0xfb, 0x80, 0, 0, 0, 0, 0, 0, 0],
            "CBOR encodes negative zero, but canonical encoding requires positive zero at byte 1.",
        ),
        (
            &[0xf8, 20],
            "CBOR Boolean or null encoding is wider than necessary at byte 1.",
        ),
        (
            &[0xf8, 21],
            "CBOR Boolean or null encoding is wider than necessary at byte 1.",
        ),
        (
            &[0xf8, 22],
            "CBOR Boolean or null encoding is wider than necessary at byte 1.",
        ),
    ];
    let mut previous = None;
    for (bytes, expected) in cases {
        // The outer array must not overwrite the inner value's offset.
        let nested = [&[0x81], *bytes].concat();
        let error = decode(&nested, &Limits::default()).unwrap_err();
        assert_eq!(error.kind(), &ErrorKind::NonCanonicalEncoding);
        assert_eq!(error.offset(), Some(1));
        assert_eq!(error.to_string(), *expected);
        if let Some(previous) = previous {
            assert_eq!(error, previous);
        }
        previous = Some(error);
    }
    let error = decode(&[0x81, 0xa1, 0x78, 1, b'a', 0xf6], &Limits::default()).unwrap_err();
    assert_eq!(error.kind(), &ErrorKind::NonCanonicalEncoding);
    assert_eq!(error.offset(), Some(2));
    assert_eq!(
        error.to_string(),
        "CBOR integer or length header is wider than necessary at byte 2."
    );
}

#[test]
fn diagnostics_omit_keys_text_and_byte_payloads() {
    let limits = Limits::default();
    let value = Value::Map(
        Map::try_from_entries([
            ("private-key".into(), Value::Text("private-text".into())),
            (
                "payload-key".into(),
                Value::Bytes(b"private-bytes".to_vec()),
            ),
        ])
        .unwrap(),
    );
    let mut bytes = encode(&value, &limits).unwrap();
    bytes.push(0xff);
    let trailing = decode(&bytes, &limits).unwrap_err();
    let duplicate = Map::try_from_entries([
        ("private-key".into(), value.clone()),
        ("private-key".into(), value),
    ])
    .unwrap_err();
    for error in [trailing, duplicate] {
        for diagnostic in [error.to_string(), format!("{error:?}")] {
            for secret in [
                "private-key",
                "private-text",
                "payload-key",
                "private-bytes",
            ] {
                assert!(!diagnostic.contains(secret));
            }
            assert!(!diagnostic.contains(&format!("{:?}", b"private-bytes")));
        }
    }
}

#[test]
fn malformed_noncanonical_and_unsupported_inputs() {
    let fixtures: Vec<(&[u8], ErrorKind, usize)> = vec![
        (&[], ErrorKind::UnexpectedEnd, 0),
        (&[0x18], ErrorKind::UnexpectedEnd, 1),
        (&[0x19, 1], ErrorKind::UnexpectedEnd, 2),
        (&[0x63, b'a'], ErrorKind::UnexpectedEnd, 2),
        (&[0x82, 1], ErrorKind::UnexpectedEnd, 2),
        (&[0xf6, 0xf6], ErrorKind::TrailingData, 1),
        (&[0x18, 0], ErrorKind::NonCanonicalEncoding, 0),
        (&[0x38, 23], ErrorKind::NonCanonicalEncoding, 0),
        (&[0x19, 0, 24], ErrorKind::NonCanonicalEncoding, 0),
        (&[0x58, 0], ErrorKind::NonCanonicalEncoding, 0),
        (&[0x78, 1, b'a'], ErrorKind::NonCanonicalEncoding, 0),
        (&[0x98, 0], ErrorKind::NonCanonicalEncoding, 0),
        (&[0xb8, 0], ErrorKind::NonCanonicalEncoding, 0),
        (&[0x5f, 0xff], ErrorKind::NonCanonicalEncoding, 0),
        (&[0x7f, 0xff], ErrorKind::NonCanonicalEncoding, 0),
        (&[0x9f, 0xff], ErrorKind::NonCanonicalEncoding, 0),
        (&[0xbf, 0xff], ErrorKind::NonCanonicalEncoding, 0),
        (&[0xc0, 0xf6], ErrorKind::UnsupportedType, 0),
        (&[0xf7], ErrorKind::UnsupportedType, 0),
        (&[0xff], ErrorKind::UnsupportedType, 0),
        (&[0xf8, 20], ErrorKind::NonCanonicalEncoding, 0),
        (&[0xf8, 0], ErrorKind::UnsupportedType, 0),
        (&[0x1c], ErrorKind::UnsupportedType, 0),
        (&[0x1d], ErrorKind::UnsupportedType, 0),
        (&[0x1e], ErrorKind::UnsupportedType, 0),
        (&[0x61, 0xff], ErrorKind::InvalidUtf8, 1),
        (&[0x63, b'a', 0xc0, 0x80], ErrorKind::InvalidUtf8, 2),
        (&[0x63, 0xed, 0xa0, 0x80], ErrorKind::InvalidUtf8, 1),
        (&[0xa1, 0x01, 0xf6], ErrorKind::UnsupportedType, 1),
        (&[0xa1, 0x61, 0xff, 0xf6], ErrorKind::InvalidUtf8, 2),
        (
            &[0xa1, 0x78, 1, b'a', 0xf6],
            ErrorKind::NonCanonicalEncoding,
            1,
        ),
        (
            &[0xa2, 0x61, b'a', 0, 0x61, b'a', 1],
            ErrorKind::DuplicateMapKey,
            4,
        ),
        (
            &[0xa2, 0x62, b'a', b'a', 0, 0x61, b'z', 1],
            ErrorKind::MapKeyOutOfOrder,
            5,
        ),
        (
            &[0xa2, 0x61, b'b', 0, 0x61, b'a', 1],
            ErrorKind::MapKeyOutOfOrder,
            4,
        ),
        (&[0x81, 0x18, 0], ErrorKind::NonCanonicalEncoding, 1),
        (&[0xa1, 0x61, b'a', 0x18], ErrorKind::UnexpectedEnd, 4),
    ];
    for (bytes, kind, offset) in fixtures {
        reject(bytes, kind, offset);
    }
    for major in [0x1b, 0x3b] {
        reject(
            &[major, 0x80, 0, 0, 0, 0, 0, 0, 0],
            ErrorKind::IntegerOutOfRange,
            0,
        );
        reject(
            &[major, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            ErrorKind::IntegerOutOfRange,
            0,
        );
    }
}

#[test]
fn floats_reject_nonfinite_negative_zero_and_unnecessary_width() {
    for bytes in [
        vec![0xf9, 0x7c, 0],
        vec![0xf9, 0xfc, 0],
        vec![0xf9, 0x7e, 0],
        vec![0xfa, 0x7f, 0x80, 0, 0],
        vec![0xfa, 0xff, 0x80, 0, 0],
        vec![0xfa, 0x7f, 0x80, 0, 1],
        vec![0xfb, 0x7f, 0xf0, 0, 0, 0, 0, 0, 0],
        vec![0xfb, 0xff, 0xf0, 0, 0, 0, 0, 0, 0],
        vec![0xfb, 0x7f, 0xf0, 0, 0, 0, 0, 0, 1],
    ] {
        reject(&bytes, ErrorKind::NonFiniteFloat, 0);
    }
    for bytes in [
        vec![0xf9, 0x80, 0],
        vec![0xfa, 0x80, 0, 0, 0],
        vec![0xfb, 0x80, 0, 0, 0, 0, 0, 0, 0],
        vec![0xfa, 0x3f, 0xc0, 0, 0], // 1.5 fits binary16
        vec![0xfb, 0x3f, 0xf8, 0, 0, 0, 0, 0, 0],
        vec![0xfb, 0x40, 0xf8, 0x6a, 0, 0, 0, 0, 0], // 100000 fits binary32
        vec![0xfa, 0, 0, 0, 0],
    ] {
        reject(&bytes, ErrorKind::NonCanonicalEncoding, 0);
    }
}

#[test]
fn every_binary16_input_pattern_is_classified() {
    for bits in 0u16..=u16::MAX {
        let [hi, lo] = bits.to_be_bytes();
        let bytes = [0xf9, hi, lo];
        if bits & 0x7c00 == 0x7c00 {
            reject(&bytes, ErrorKind::NonFiniteFloat, 0);
        } else if bits == 0x8000 {
            reject(&bytes, ErrorKind::NonCanonicalEncoding, 0);
        } else {
            let value = decode(&bytes, &Limits::default()).unwrap();
            assert_eq!(encode(&value, &Limits::default()).unwrap(), bytes);
        }
    }
}

#[test]
fn every_truncated_prefix_fails_at_the_first_missing_byte() {
    let documents = [
        vec![0x1b, 0, 0, 0, 1, 0, 0, 0, 0],
        vec![0x65, b'h', b'e', b'l', b'l', b'o'],
        vec![0x44, 0, 1, 2, 3],
        vec![0xa1, 0x61, b'a', 0x82, 1, 2],
        vec![0xfb, 0x3f, 0xf1, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a],
    ];
    for bytes in documents {
        assert!(decode(&bytes, &Limits::default()).is_ok());
        for end in 0..bytes.len() {
            reject(&bytes[..end], ErrorKind::UnexpectedEnd, end);
        }
    }
}

#[test]
fn all_limits_include_keys_and_validate_before_work() {
    let bytes = [0xa2, 0x61, b'a', 0x62, 0xc3, 0xa9, 0x61, b'b', 0x42, 0, 255];
    let limits = Limits {
        max_document_bytes: 11,
        max_depth: 1,
    };
    let value = decode(&bytes, &limits).unwrap();
    assert_eq!(encode(&value, &limits).unwrap(), bytes);
    type LimitCase = (LimitKind, usize, usize, fn(&mut Limits));
    let cases: [LimitCase; 2] = [
        (LimitKind::DocumentBytes, 10, 0, |l| {
            l.max_document_bytes = 10
        }),
        (LimitKind::Depth, 0, 0, |l| l.max_depth = 0),
    ];
    for (limit, maximum, offset, adjust) in cases {
        let mut tightened = limits.clone();
        adjust(&mut tightened);
        let error = decode(&bytes, &tightened).unwrap_err();
        assert_eq!(error.kind(), &ErrorKind::LimitExceeded { limit, maximum });
        assert_eq!(error.offset(), Some(offset));
    }
    let key = [0xa1, 0x62, 0xc3, 0xa9, 0xf6];
    let error = decode(
        &key,
        &Limits {
            max_document_bytes: 4,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        error.kind(),
        &ErrorKind::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: 4
        }
    );
    assert_eq!(error.offset(), Some(0));
    let error = decode(
        &[],
        &Limits {
            max_depth: 129,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind(), &ErrorKind::InvalidLimits);
    assert_eq!(error.offset(), None);
    for bytes in [&[0xf6][..], &[0x80], &[0xa0], &[0x60], &[0x40]] {
        let zero = Limits {
            max_depth: 0,
            max_document_bytes: 1,
        };
        assert!(decode(bytes, &zero).is_ok());
        let error = decode(
            bytes,
            &Limits {
                max_document_bytes: 0,
                ..zero
            },
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            &ErrorKind::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                maximum: 0
            }
        );
    }
}

#[test]
fn hostile_lengths_are_rejected_before_allocation() {
    let huge = Limits {
        max_document_bytes: usize::MAX,
        max_depth: 128,
    };
    for major in [0x5b, 0x7b, 0x9b, 0xbb] {
        let bytes = [major, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        for limits in [&Limits::default(), &huge] {
            let error = decode(&bytes, limits).unwrap_err();
            assert!(matches!(
                error.kind(),
                ErrorKind::LimitExceeded { .. } | ErrorKind::UnexpectedEnd
            ));
        }
    }
    // A declared length within configured ceilings is still not proof that
    // enough data exists; the decoder must not reserve 100,000 slots here.
    let bytes = [0x9a, 0, 1, 0x86, 0xa0];
    reject(&bytes, ErrorKind::UnexpectedEnd, bytes.len());
}

#[test]
fn nested_payloads_are_opaque_and_errors_do_not_disclose_values() {
    let bytes = [0x42, 0x18, 0x00];
    let Value::Bytes(payload) = decode(&bytes, &Limits::default()).unwrap() else {
        panic!()
    };
    reject(&payload, ErrorKind::NonCanonicalEncoding, 0);
    let mut bytes = encode(
        &Value::Map(Map::try_from_entries([("secret-key".into(), Value::Null)]).unwrap()),
        &Limits::default(),
    )
    .unwrap();
    *bytes.last_mut().unwrap() = 0xff;
    let error = decode(&bytes, &Limits::default()).unwrap_err();
    assert!(!error.to_string().contains("secret-key"));
    assert!(!format!("{error:?}").contains("secret-key"));
}

#[test]
fn bounded_arbitrary_bytes_never_panic_and_accepted_bytes_round_trip() {
    let limits = Limits {
        max_document_bytes: 128,
        max_depth: 8,
    };
    let verify = |bytes: &[u8]| match decode(bytes, &limits) {
        Ok(value) => assert_eq!(encode(&value, &limits).unwrap(), bytes),
        Err(error) => assert!(error.offset().is_some_and(|offset| offset <= bytes.len())),
    };
    for first in 0..=u8::MAX {
        verify(&[first]);
        for second in 0..=u8::MAX {
            verify(&[first, second]);
        }
    }
    let mut state = 0x4854_4c4bu32;
    for size in 0..=128 {
        for _ in 0..32 {
            let mut bytes = vec![0; size];
            for byte in &mut bytes {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                *byte = (state >> 24) as u8;
            }
            verify(&bytes);
        }
    }
}
