//! Serde mappings preserve the canonical HTLK value representation.

use htlk_executable::cbor::{self, FiniteFloat, Limits, Map, Value};

#[test]
fn serde_preserves_scalar_types_bytes_and_canonical_map_order() {
    let value = Value::Array(vec![
        Value::Integer(1),
        Value::Float(FiniteFloat::new(1.0).unwrap()),
        Value::Bytes(vec![0, 255]),
        Value::Map(
            Map::try_from_entries([("aa".into(), Value::Null), ("z".into(), Value::Bool(true))])
                .unwrap(),
        ),
    ]);
    let mut buffer = [0; 64];
    let encoded = cbor2::to_slice(&value, &mut buffer).unwrap();
    assert_eq!(
        encoded,
        [
            0x84, 0x01, 0xf9, 0x3c, 0x00, 0x42, 0x00, 0xff, 0xa2, 0x61, b'z', 0xf5, 0x62, b'a',
            b'a', 0xf6,
        ]
    );
    assert_eq!(cbor::decode(encoded, &Limits::default()).unwrap(), value);
    assert_eq!(cbor::encode(&value, &Limits::default()).unwrap(), encoded);
}

#[test]
fn serde_uses_value_shapes_instead_of_rust_enum_tags() {
    let value = Value::Map(
        Map::try_from_entries([
            ("integer".into(), Value::Integer(1)),
            ("float".into(), Value::Float(FiniteFloat::new(1.0).unwrap())),
        ])
        .unwrap(),
    );
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"float":1.0,"integer":1}"#
    );
}
