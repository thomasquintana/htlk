//! Version-0.1 envelope wire vectors and validation contracts.

use std::error::Error as _;

use htlk_cbor::{ErrorKind, LimitKind, Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::digest::{ParseDigestError, hash_bytes};
use htlk_executable::{EXECUTABLE_FORMAT, EXECUTABLE_VERSION, EnvelopeError, ExecutableEnvelope};

const NULL_FINGERPRINT: &str =
    "sha256:9b039893d4db25c42f0765d6a31877987d90ff86f07ca593af128ca23500120d";

fn entries() -> Vec<(String, Value)> {
    vec![
        ("format".into(), Value::Text("htlk.executable.graph".into())),
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
    // Independently calculated with Python hashlib over the exact prefix
    // b"htlk.executable.graph/0.1\n" followed by raw payload bytes.
    for (payload, encoded_payload, fingerprint) in [
        (
            &[][..],
            &[0x40][..],
            "sha256:ebbad418b6ddd9ead246e32dc337b19276b2c709701d2ad69a9992098f9fa14c",
        ),
        (&[0xf6][..], &[0x41, 0xf6][..], NULL_FINGERPRINT),
        (
            &[0x18, 0][..],
            &[0x42, 0x18, 0][..],
            "sha256:16d5710bfeeff68949a1b7980cb043e2998d21b892165c235a6d7ddda261fbb2",
        ),
    ] {
        // Literal headers also fix canonical map ordering independently of the
        // implementation: format, payload, version, fingerprint.
        let mut expected = b"\xa4\x66format\x75htlk.executable.graph\x67payload".to_vec();
        expected.extend_from_slice(encoded_payload);
        expected.extend_from_slice(b"\x67version\x630.1\x6bfingerprint\x78\x47");
        expected.extend_from_slice(fingerprint.as_bytes());
        let envelope = ExecutableEnvelope::new(payload.to_vec(), &Limits::default()).unwrap();
        assert_eq!(envelope.format(), "htlk.executable.graph");
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
    replace(
        &mut record,
        "format",
        Value::Text("htlk.executable.graph".into()),
    );
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
        // A CBOR array of the domain, version, and payload is not the preimage.
        (
            "sha256:7909b7c4548e6d1c893d61797a7f204434fa14a1b0dd995890e9728f5a535f62".into(),
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
fn alternate_formats_fields_and_hash_formulas_are_rejected() {
    for format in [
        "htlk.executable",
        "htlk_executable_graph",
        "HTLK.executable.graph",
    ] {
        let mut record = entries();
        replace(&mut record, "format", Value::Text(format.into()));
        reject(record, EnvelopeError::UnsupportedFormat);
    }
    let mut record = entries();
    record
        .iter_mut()
        .find(|(field, _)| field == "version")
        .unwrap()
        .0 = "format_version".into();
    reject(record, EnvelopeError::UnknownField);
    let mut record = entries();
    record.push(("format_version".into(), Value::Text("0.1".into())));
    reject(record, EnvelopeError::UnknownField);
    for preimage in [
        &b"htlk.executable/0.1\n\xf6"[..],
        &b"htlk.executable.graph/0.1\\n\xf6"[..],
        &b"htlk.executable.graph/0.1\n\x41\xf6"[..],
        &[0xf6][..],
    ] {
        let mut record = entries();
        replace(
            &mut record,
            "fingerprint",
            Value::Text(hash_bytes(preimage).to_string()),
        );
        reject(record, EnvelopeError::FingerprintMismatch);
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
        max_document_bytes: 137,
        max_depth: 1,
    };
    let envelope = ExecutableEnvelope::new(vec![0xf6], &limits).unwrap();
    let bytes = envelope.encode(&limits).unwrap();
    assert_eq!(bytes.len(), 137);
    assert_eq!(
        ExecutableEnvelope::decode(&bytes, &limits).unwrap(),
        envelope
    );
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 2] = [
        (LimitKind::DocumentBytes, 136, |l| {
            l.max_document_bytes = 136
        }),
        (LimitKind::Depth, 0, |l| l.max_depth = 0),
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
