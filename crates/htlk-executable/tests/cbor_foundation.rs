//! Public API invariants for the shared CBOR foundation.

use htlk_executable::cbor::{ErrorKind, FiniteFloat, Limits, Map, Value};

#[test]
fn floats_reject_nonfinite_values_and_normalize_zero() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            FiniteFloat::new(value).unwrap_err().kind(),
            &ErrorKind::NonFiniteFloat
        );
    }
    assert_eq!(FiniteFloat::new(-0.0).unwrap().get().to_bits(), 0);
    for value in [f64::MIN, f64::MAX, f64::from_bits(1), -1.5] {
        let float = FiniteFloat::try_from(value).unwrap();
        assert_eq!(f64::from(float).to_bits(), value.to_bits());
    }
    assert_ne!(
        Value::Integer(1),
        Value::Float(FiniteFloat::new(1.0).unwrap())
    );
}

#[test]
fn maps_use_encoded_key_order_and_exact_unicode() {
    let keys = ["aa", "z", "é", "e\u{301}", "a", "", "b"];
    let entries = || keys.iter().map(|key| ((*key).into(), Value::Null));
    let map = Map::try_from_entries(entries()).unwrap();
    let reversed = Map::try_from_entries(entries().rev()).unwrap();
    assert_eq!(map, reversed);
    assert_eq!(
        map.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        ["", "a", "b", "z", "aa", "é", "e\u{301}"]
    );
    for key in keys {
        assert_eq!(map.get(key), Some(&Value::Null));
    }
    assert_eq!(map.get("missing"), None);
    assert_eq!(map.len(), keys.len());
    assert!(!map.is_empty());
    assert!(Map::new().is_empty());
}

#[test]
fn duplicate_keys_fail_without_disclosing_key_contents() {
    let error = Map::try_from_entries([
        ("secret-key".into(), Value::Integer(1)),
        ("other".into(), Value::Null),
        ("secret-key".into(), Value::Integer(2)),
    ])
    .unwrap_err();
    assert_eq!(error.kind(), &ErrorKind::DuplicateMapKey);
    assert_eq!(error.offset(), None);
    assert!(!error.to_string().contains("secret-key"));
    assert!(!format!("{error:?}").contains("secret-key"));
}

#[test]
fn limits_allow_tightening_but_reject_unsupported_depth() {
    assert!(Limits::default().validate().is_ok());
    let mut limits = Limits {
        max_depth: 0,
        max_document_bytes: 0,
    };
    assert!(limits.validate().is_ok());
    assert_eq!(Limits::default().max_depth, 64);
    limits.max_depth = 128;
    assert!(limits.validate().is_ok());
    limits.max_depth = 129;
    assert_eq!(
        limits.validate().unwrap_err().kind(),
        &ErrorKind::InvalidLimits
    );
}
