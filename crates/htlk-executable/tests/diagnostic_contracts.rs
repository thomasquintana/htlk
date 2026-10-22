//! Redaction, structured causes and public diagnostic contracts across wire stages.
use htlk_executable::{
    cbor::{self, LimitKind, Limits, Value},
    digest::Digest,
    *,
};
use std::error::Error as _;

fn safe(error: &dyn std::error::Error) {
    let text = error.to_string();
    assert!(!text.is_empty());
    assert!(!text.contains("PRIVATE_SUBMITTED_VALUE"));
    assert!(!format!("{error:?}").contains("PRIVATE_SUBMITTED_VALUE"));
}

#[test]
fn envelope_and_metadata_errors_keep_schema_terms_and_original_typed_causes() {
    let l = Limits::default();
    let codec = cbor::decode(&[], &l).unwrap_err();
    let identifier = "PRIVATE_SUBMITTED_VALUE".parse::<Identifier>().unwrap_err();
    let digest = "PRIVATE_SUBMITTED_VALUE".parse::<Digest>().unwrap_err();
    for error in [
        EnvelopeError::from(codec.clone()),
        EnvelopeError::InvalidFingerprint(digest),
    ] {
        safe(&error);
        assert!(error.source().is_some());
    }
    for error in [
        EnvelopeError::ExpectedRecord,
        EnvelopeError::UnknownField,
        EnvelopeError::MissingField("payload"),
        EnvelopeError::InvalidFieldType("payload"),
        EnvelopeError::UnsupportedFormat,
        EnvelopeError::UnsupportedVersion,
        EnvelopeError::FingerprintMismatch,
    ] {
        safe(&error);
        assert!(error.source().is_none());
    }
    for error in [
        MetadataError::from(codec),
        MetadataError::from(identifier),
        MetadataError::from(digest),
        MetadataError::from(TypeError::UnknownPortField),
    ] {
        safe(&error);
        assert!(error.source().is_some());
    }
    for error in [
        MetadataError::InvalidShape("record"),
        MetadataError::MissingField("version"),
        MetadataError::UnknownField("engine"),
        MetadataError::UnsupportedCoreVersion,
        MetadataError::UnsupportedMcpVersion,
        MetadataError::UnsupportedTransport,
        MetadataError::UnknownBindingKind,
        MetadataError::DuplicateTypeParameter,
        MetadataError::UndeclaredTypeVariable,
        MetadataError::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: 1,
        },
        MetadataError::AllocationFailed,
    ] {
        safe(&error);
        assert!(error.source().is_none());
    }
}

#[test]
fn options_policy_pointer_and_json_errors_preserve_source_chains_and_numeric_details() {
    let l = Limits::default();
    let codec = cbor::decode(&[], &l).unwrap_err();
    let wrapped = ExecutionOptionsError::from(codec.clone());
    safe(&wrapped);
    assert!(wrapped.source().is_some());
    for error in [
        ExecutionOptionsError::ExpectedRecord("retry"),
        ExecutionOptionsError::UnknownField("retry"),
        ExecutionOptionsError::MissingField("on"),
        ExecutionOptionsError::InvalidFieldType("on"),
        ExecutionOptionsError::OutOfRange("timeout_ms"),
        ExecutionOptionsError::ZeroNotAllowed("max_attempts"),
        ExecutionOptionsError::DelayCountMismatch {
            expected: 2,
            actual: 1,
        },
        ExecutionOptionsError::NonCanonicalCodes,
        ExecutionOptionsError::LimitExceeded {
            limit: LimitKind::Depth,
            maximum: 3,
        },
        ExecutionOptionsError::AllocationFailed,
    ] {
        safe(&error);
        assert!(error.source().is_none());
    }
    for error in [
        PolicyError::IncompleteDefaults,
        PolicyError::UnknownField,
        PolicyError::MissingField("defaults"),
    ] {
        safe(&error);
        assert!(error.source().is_none());
    }
    let json = JsonError::from(codec);
    safe(&json);
    assert!(json.source().is_some());
    for error in [
        JsonError::InvalidUtf8 { offset: 2 },
        JsonError::UnsafeNumber,
        JsonError::UnsupportedValue,
        JsonError::NonCanonical,
        JsonError::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: 1,
        },
        JsonError::AllocationFailed,
        JsonError::Serialization,
    ] {
        safe(&error);
        assert!(error.source().is_none());
    }
    let pointer = JsonPointer::new("not/a/pointer", &l).unwrap_err();
    safe(&pointer);
    assert!(pointer.source().is_none());
    let pointer = JsonPointer::new(
        "",
        &Limits {
            max_depth: 129,
            ..l
        },
    )
    .unwrap_err();
    safe(&pointer);
    assert!(pointer.source().is_some());
    safe(
        &format!("sha256:{}", "0".repeat(63))
            .parse::<Digest>()
            .unwrap_err(),
    );
}

#[test]
fn structured_builtin_declarations_are_complete_and_expansion_is_bounded() {
    let l = Limits::default();
    for primitive in [
        BuiltinType::Error,
        BuiltinType::Regex,
        BuiltinType::McpResourceResult,
        BuiltinType::McpPromptResult,
    ] {
        let record = builtin_record_type(primitive, &l).unwrap().unwrap();
        let encoded = record.encode(TypeContext::Value, &l).unwrap();
        assert_eq!(
            ValueType::decode(&encoded, TypeContext::Value, &l).unwrap(),
            record
        );
        for max in [1, 8, 16, 32] {
            assert!(
                builtin_record_type(
                    primitive,
                    &Limits {
                        max_document_bytes: max,
                        ..l.clone()
                    }
                )
                .is_err()
            );
        }
    }
    for primitive in [BuiltinType::String, BuiltinType::Integer, BuiltinType::Null] {
        assert!(builtin_record_type(primitive, &l).unwrap().is_none());
    }
    assert!(JsonDocument::new(&[0xff], &l).is_err());
    assert!(
        JsonDocument::from_value(&Value::Bytes(b"PRIVATE_SUBMITTED_VALUE".to_vec()), &l).is_err()
    );
}
