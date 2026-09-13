//! Version-0.1 envelope wire vectors and validation contracts.

use std::error::Error as _;

use htlk_cbor::{ErrorKind, LimitKind, Limits, Map, Value};
use htlk_executable::digest::ParseDigestError;
use htlk_executable::{EXECUTABLE_FORMAT, EXECUTABLE_VERSION, EnvelopeError, ExecutableEnvelope};

const NULL_FINGERPRINT: &str =
    "sha256:25e4bc7d3cd311fb07bd53b7bfede608ba5a659b124d02106f98e63bee238146";

fn entries() -> Vec<(String, Value)> {
    vec![
        ("format".into(), Value::Text("htlk.executable".into())),
        ("version".into(), Value::Text("0.1".into())),
        ("fingerprint".into(), Value::Text(NULL_FINGERPRINT.into())),
        ("payload".into(), Value::Bytes(vec![0xf6])),
    ]
}

fn encode_record(entries: Vec<(String, Value)>) -> Vec<u8> {
    htlk_cbor::encode(
        &Value::Map(Map::try_from_entries(entries).unwrap()),
        &Limits::default(),
    )
    .unwrap()
}

fn replace(entries: &mut [(String, Value)], field: &str, value: Value) {
    entries.iter_mut().find(|(key, _)| key == field).unwrap().1 = value;
}

fn reject(entries: Vec<(String, Value)>, expected: EnvelopeError) {
    assert_eq!(
        ExecutableEnvelope::decode(&encode_record(entries), &Limits::default()).unwrap_err(),
        expected
    );
}

#[test]
fn independent_version_01_wire_vectors() {
    // Fingerprints independently calculated with Python hashlib over literal
    // CBOR preimages: 83 6f "htlk.executable" 63 "0.1" <byte-string payload>.
    for (payload, encoded_payload, fingerprint) in [
        (
            &[][..],
            &[0x40][..],
            "sha256:1848fa2b7902ab7f9340287c3c8de220515da01c9efd65bd496d209cac398d6a",
        ),
        (&[0xf6][..], &[0x41, 0xf6][..], NULL_FINGERPRINT),
        (
            &[0x18, 0][..],
            &[0x42, 0x18, 0][..],
            "sha256:37bfe69c9c00df40893a2759caec90ae73060bc4177ebd8a27f06806eae6630d",
        ),
    ] {
        // Literal headers also fix canonical map ordering independently of the
        // implementation: format, payload, version, fingerprint.
        let mut expected = b"\xa4\x66format\x6fhtlk.executable\x67payload".to_vec();
        expected.extend_from_slice(encoded_payload);
        expected.extend_from_slice(b"\x67version\x630.1\x6bfingerprint\x78\x47");
        expected.extend_from_slice(fingerprint.as_bytes());
        let envelope = ExecutableEnvelope::new(payload.to_vec(), &Limits::default()).unwrap();
        assert_eq!(envelope.format(), "htlk.executable");
        assert_eq!(envelope.version(), "0.1");
        assert_eq!(EXECUTABLE_FORMAT, envelope.format());
        assert_eq!(EXECUTABLE_VERSION, envelope.version());
        assert_eq!(envelope.fingerprint().to_string(), fingerprint);
        assert_eq!(envelope.payload(), payload);
        assert_eq!(envelope.encode(&Limits::default()).unwrap(), expected);
        let decoded = ExecutableEnvelope::decode(&expected, &Limits::default()).unwrap();
        assert_eq!(decoded, envelope);
        assert_eq!(decoded.encode(&Limits::default()).unwrap(), expected);
    }
}

#[test]
fn required_fields_and_types() {
    for field in ["format", "version", "fingerprint", "payload"] {
        let mut record = entries();
        record.retain(|(name, _)| name != field);
        reject(record, EnvelopeError::MissingField(field));
        for wrong_type in [
            Value::Null,
            Value::Bool(true),
            Value::Integer(1),
            Value::Array(vec![]),
        ] {
            let mut record = entries();
            replace(&mut record, field, wrong_type);
            reject(record, EnvelopeError::InvalidFieldType(field));
        }
    }
    let mut record = entries();
    replace(
        &mut record,
        "payload",
        Value::Text("payload is bytes".into()),
    );
    reject(record, EnvelopeError::InvalidFieldType("payload"));
    let mut record = entries();
    replace(&mut record, "fingerprint", Value::Bytes(vec![0; 32]));
    reject(record, EnvelopeError::InvalidFieldType("fingerprint"));
    for value in [
        Value::Null,
        Value::Array(vec![]),
        Value::Text("record".into()),
    ] {
        let bytes = htlk_cbor::encode(&value, &Limits::default()).unwrap();
        assert_eq!(
            ExecutableEnvelope::decode(&bytes, &Limits::default()).unwrap_err(),
            EnvelopeError::ExpectedRecord
        );
    }
}

#[test]
fn schema_precedence_is_explicit() {
    reject(
        vec![("unexpected".into(), Value::Null)],
        EnvelopeError::UnknownField,
    );
    reject(vec![], EnvelopeError::MissingField("fingerprint"));
    let mut record = entries();
    record.retain(|(key, _)| key != "version");
    replace(&mut record, "fingerprint", Value::Null);
    reject(record, EnvelopeError::MissingField("version"));
    let mut record = entries();
    for (_, value) in &mut record {
        *value = Value::Null;
    }
    reject(record, EnvelopeError::InvalidFieldType("fingerprint"));
}

#[test]
fn format_and_version_precede_fingerprint_validation() {
    let mut record = entries();
    replace(&mut record, "format", Value::Text("other".into()));
    replace(&mut record, "version", Value::Text("0.3".into()));
    replace(&mut record, "fingerprint", Value::Text("invalid".into()));
    reject(record.clone(), EnvelopeError::UnsupportedFormat);
    replace(&mut record, "format", Value::Text("htlk.executable".into()));
    reject(record.clone(), EnvelopeError::UnsupportedVersion);
    replace(&mut record, "version", Value::Text("0.1".into()));
    reject(
        record,
        EnvelopeError::InvalidFingerprint(ParseDigestError::InvalidPrefix),
    );
    for version in ["", "0.01", "0.1 ", "0.3", "1"] {
        let mut record = entries();
        replace(&mut record, "version", Value::Text(version.into()));
        reject(record, EnvelopeError::UnsupportedVersion);
    }
}

#[test]
fn malformed_or_mismatched_fingerprints_and_tampered_payloads() {
    for (fingerprint, expected) in [
        (
            "sha256:00".to_owned(),
            EnvelopeError::InvalidFingerprint(ParseDigestError::InvalidLength),
        ),
        (
            format!("sha256:{}", "A".repeat(64)),
            EnvelopeError::InvalidFingerprint(ParseDigestError::InvalidHex),
        ),
        (
            format!("sha256:{}", "0".repeat(64)),
            EnvelopeError::FingerprintMismatch,
        ),
        // Same payload hashed with old 0.3 version label cannot authorize 0.1.
        (
            "sha256:9efe570f1d2ce87ec8dbd7493193c030004f398d52308f62f619ba175ac8b4f2".into(),
            EnvelopeError::FingerprintMismatch,
        ),
    ] {
        let mut record = entries();
        replace(&mut record, "fingerprint", Value::Text(fingerprint));
        reject(record, expected);
    }
    let mut record = entries();
    replace(&mut record, "payload", Value::Bytes(vec![0xf5]));
    reject(record, EnvelopeError::FingerprintMismatch);
}

#[test]
fn payload_is_opaque_and_preserved() {
    for bytes in [vec![], vec![0x18, 0], vec![0xff, 0, 0x80, b'a']] {
        assert!(htlk_cbor::decode(&bytes, &Limits::default()).is_err());
        let envelope = ExecutableEnvelope::new(bytes.clone(), &Limits::default()).unwrap();
        let encoded = envelope.encode(&Limits::default()).unwrap();
        let decoded = ExecutableEnvelope::decode(&encoded, &Limits::default()).unwrap();
        assert_eq!(decoded.payload(), bytes);
    }
}

#[test]
fn codec_failures_preserve_error_and_offset() {
    let valid = encode_record(entries());
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut nonminimal = vec![0xb8, 4];
    nonminimal.extend_from_slice(&valid[1..]);
    let mut invalid_utf8 = valid.clone();
    invalid_utf8[9] = 0xff;
    let mut duplicate = valid.clone();
    duplicate[0] = 0xa5;
    duplicate.extend_from_slice(b"\x6bfingerprint\x78\x47");
    duplicate.extend_from_slice(NULL_FINGERPRINT.as_bytes());
    for bytes in [vec![], trailing, nonminimal, invalid_utf8, duplicate] {
        let expected = htlk_cbor::decode(&bytes, &Limits::default()).unwrap_err();
        let error = ExecutableEnvelope::decode(&bytes, &Limits::default()).unwrap_err();
        assert_eq!(error, EnvelopeError::Codec(expected.clone()));
        assert_eq!(
            error.source().unwrap().downcast_ref::<htlk_cbor::Error>(),
            Some(&expected)
        );
    }
    for end in 0..valid.len() {
        let error = ExecutableEnvelope::decode(&valid[..end], &Limits::default()).unwrap_err();
        let EnvelopeError::Codec(error) = error else {
            panic!("expected codec error");
        };
        assert_eq!(error.kind(), &ErrorKind::UnexpectedEnd);
        assert_eq!(error.offset(), Some(end));
    }
}

#[test]
fn all_codec_limits_bound_construction_encoding_and_decoding() {
    let limits = Limits {
        max_document_bytes: 131,
        max_text_bytes: 71,
        max_byte_string_bytes: 1,
        max_depth: 1,
        max_collection_entries: 4,
        max_total_values: 9,
        max_total_payload_bytes: 121,
    };
    let envelope = ExecutableEnvelope::new(vec![0xf6], &limits).unwrap();
    let bytes = envelope.encode(&limits).unwrap();
    assert_eq!(bytes.len(), 131);
    assert_eq!(
        ExecutableEnvelope::decode(&bytes, &limits).unwrap(),
        envelope
    );
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 7] = [
        (LimitKind::DocumentBytes, 130, |l| {
            l.max_document_bytes = 130
        }),
        (LimitKind::TextBytes, 70, |l| l.max_text_bytes = 70),
        (LimitKind::ByteStringBytes, 0, |l| {
            l.max_byte_string_bytes = 0
        }),
        (LimitKind::Depth, 0, |l| l.max_depth = 0),
        (LimitKind::CollectionEntries, 3, |l| {
            l.max_collection_entries = 3
        }),
        (LimitKind::TotalValues, 8, |l| l.max_total_values = 8),
        (LimitKind::TotalPayloadBytes, 120, |l| {
            l.max_total_payload_bytes = 120
        }),
    ];
    for (limit, maximum, adjust) in cases {
        let mut tight = limits.clone();
        adjust(&mut tight);
        for result in [
            ExecutableEnvelope::new(vec![0xf6], &tight).map(|_| ()),
            envelope.encode(&tight).map(|_| ()),
            ExecutableEnvelope::decode(&bytes, &tight).map(|_| ()),
        ] {
            let EnvelopeError::Codec(error) = result.unwrap_err() else {
                panic!("expected codec error");
            };
            assert_eq!(error.kind(), &ErrorKind::LimitExceeded { limit, maximum });
        }
    }
    let bad = Limits {
        max_depth: 129,
        ..limits.clone()
    };
    for result in [
        ExecutableEnvelope::new(vec![], &bad).map(|_| ()),
        envelope.encode(&bad).map(|_| ()),
        ExecutableEnvelope::decode(&[], &bad).map(|_| ()),
    ] {
        let EnvelopeError::Codec(error) = result.unwrap_err() else {
            panic!("expected codec error");
        };
        assert_eq!(error.kind(), &ErrorKind::InvalidLimits);
    }
    // Failed encoding leaves the immutable result available for a later call.
    assert_eq!(envelope.encode(&limits).unwrap(), bytes);
}

#[test]
fn diagnostics_do_not_retain_untrusted_fields_or_payload() {
    let marker = "private-data";
    let mut record = entries();
    record.push((marker.into(), Value::Text(marker.into())));
    let unknown =
        ExecutableEnvelope::decode(&encode_record(record), &Limits::default()).unwrap_err();
    let mut record = entries();
    replace(&mut record, "format", Value::Text(marker.into()));
    let format =
        ExecutableEnvelope::decode(&encode_record(record), &Limits::default()).unwrap_err();
    let invalid = EnvelopeError::InvalidFingerprint(ParseDigestError::InvalidPrefix);
    assert!(invalid.source().unwrap().is::<ParseDigestError>());
    for error in [unknown, format, invalid] {
        assert!(!error.to_string().contains(marker));
        assert!(!format!("{error:?}").contains(marker));
    }
    let envelope = ExecutableEnvelope::new(marker.as_bytes().to_vec(), &Limits::default()).unwrap();
    assert!(!format!("{envelope:?}").contains(marker));
}
