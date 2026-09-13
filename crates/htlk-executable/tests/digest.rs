//! Digest representation, SHA-256 known answers, and CBOR composition contracts.

use std::collections::HashSet;

use htlk_cbor::{FiniteFloat, Limits, Map, Value};
use htlk_executable::digest::{Digest, ParseDigestError, hash_bytes, hash_cbor};

#[test]
fn sha256_known_answers() {
    // Published SHA-256 vectors: empty input, "abc", the 56-byte test message,
    // and one million ASCII 'a' bytes exercise padding and multiple blocks.
    for (input, expected) in [
        (
            &b""[..],
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            &b"abc"[..],
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            &b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"[..],
            "sha256:248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        assert_eq!(hash_bytes(input).to_string(), expected);
        assert_eq!(hash_bytes(input), expected.parse::<Digest>().unwrap());
    }
    assert_eq!(
        hash_bytes(&vec![b'a'; 1_000_000]).to_string(),
        "sha256:cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn canonical_text_preserves_bytes_including_leading_zeros() {
    const ZERO: Digest = Digest::from_bytes([0; 32]);
    assert_eq!(ZERO.to_string(), format!("sha256:{}", "0".repeat(64)));
    let bytes = std::array::from_fn(|i| i as u8);
    let digest = Digest::from_bytes(bytes);
    let text = "sha256:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    assert_eq!(digest.as_bytes(), &bytes);
    assert_eq!(digest.to_string(), text);
    assert_eq!(format!("{digest:?}"), text);
    assert_eq!(text.parse::<Digest>().unwrap(), digest);
    for byte in 0..=u8::MAX {
        let digest = Digest::from_bytes([byte; 32]);
        let text = digest.to_string();
        assert_eq!(text.len(), 71);
        assert_eq!(text.parse::<Digest>().unwrap().as_bytes(), &[byte; 32]);
    }
    let smaller = Digest::from_bytes([1; 32]);
    let larger = Digest::from_bytes([2; 32]);
    assert!(smaller < larger);
    assert_eq!(HashSet::from([smaller, larger, smaller]).len(), 2);
}

#[test]
fn parsing_is_strict_and_has_stable_error_precedence() {
    for text in [
        "".to_owned(),
        "SHA256:".to_owned(),
        "sha512:".to_owned(),
        " sha256:".to_owned(),
        "sha256".to_owned(),
        "sha256：".to_owned(),
    ] {
        assert_eq!(text.parse::<Digest>(), Err(ParseDigestError::InvalidPrefix));
    }
    for suffix in [
        String::new(),
        "0".repeat(63),
        "g".repeat(65),
        format!("{}\n", "0".repeat(64)),
        "０".repeat(64),
    ] {
        assert_eq!(
            format!("sha256:{suffix}").parse::<Digest>(),
            Err(ParseDigestError::InvalidLength)
        );
    }
    for invalid in [b'A', b'F', b'G', b'g', b'/', b':', b' ', b'\n', 0] {
        // Verify every nibble position, not just the first pair.
        for index in 0..64 {
            let mut suffix = vec![b'0'; 64];
            suffix[index] = invalid;
            let text = format!("sha256:{}", String::from_utf8(suffix).unwrap());
            assert_eq!(text.parse::<Digest>(), Err(ParseDigestError::InvalidHex));
        }
    }
    assert_eq!(
        format!("sha256:{}", "é".repeat(32)).parse::<Digest>(),
        Err(ParseDigestError::InvalidHex)
    );
    let input = format!("sha256:private-input{}", "0".repeat(51));
    let error = input.parse::<Digest>().unwrap_err();
    assert!(!error.to_string().contains("private-input"));
    assert!(!format!("{error:?}").contains("private-input"));
}

#[test]
fn cbor_hashes_match_independent_known_bytes() {
    // Expected digests independently checked with Python hashlib over these
    // hand-authored CBOR bytes, not derived through the codec under test.
    let vectors: Vec<(Value, &[u8], &str)> = vec![
        (
            Value::Text("abc".into()),
            b"\x63abc",
            "sha256:a6d89baf01ac02637da09835b28485b2db68576834d01869fc15e36b124c617c",
        ),
        (
            Value::Integer(1),
            &[0x01],
            "sha256:4bf5122f344554c53bde2ebb8cd2b7e3d1600ad631c385a5d7cce23c7785459a",
        ),
        (
            Value::Float(FiniteFloat::new(1.0).unwrap()),
            &[0xf9, 0x3c, 0],
            "sha256:d823dcdc2d6aaab28aef32ce978988f5108ff31130f79d7f77117826d2cebc15",
        ),
    ];
    for (value, bytes, expected) in vectors {
        let digest = hash_cbor(&value, &Limits::default()).unwrap();
        assert_eq!(digest.to_string(), expected);
        assert_eq!(digest, hash_bytes(bytes));
        assert_eq!(
            digest,
            hash_bytes(&htlk_cbor::encode(&value, &Limits::default()).unwrap())
        );
    }
    assert_ne!(
        hash_bytes(b"abc"),
        hash_cbor(&Value::Text("abc".into()), &Limits::default()).unwrap()
    );
}

#[test]
fn map_order_and_valid_limits_do_not_change_the_digest() {
    let entries = [
        ("aa".into(), Value::Integer(2)),
        ("z".into(), Value::Integer(1)),
    ];
    let forward = Value::Map(Map::try_from_entries(entries.clone()).unwrap());
    let reverse = Value::Map(Map::try_from_entries(entries.into_iter().rev()).unwrap());
    let tight = Limits {
        max_document_bytes: 8,
        max_text_bytes: 2,
        max_byte_string_bytes: 0,
        max_collection_entries: 2,
        max_depth: 1,
        max_total_values: 5,
        max_total_payload_bytes: 3,
    };
    let digest = hash_cbor(&forward, &tight).unwrap();
    assert_eq!(
        digest.to_string(),
        "sha256:ade2165710f494d77ff74f543cb07dec2423ece94abcae8a2ba49b6e1400c9bc"
    );
    assert_eq!(digest, hash_cbor(&reverse, &Limits::default()).unwrap());
    assert_eq!(
        digest,
        hash_bytes(&[0xa2, 0x61, b'z', 1, 0x62, b'a', b'a', 2])
    );
}

#[test]
fn caller_supplied_domain_labels_are_hashed_exactly() {
    for (domain, expected) in [
        (
            "htlk.root_scope",
            "sha256:f4968d5e2c868088fb9102528bb62b80c55fa92b6944988b0c337fa7b8659b33",
        ),
        (
            "htlk.child_scope",
            "sha256:705fb04851d208b58c90ac0f6e2635fce01fbc5f4c1b95671aaf915c2ebddf39",
        ),
    ] {
        let preimage = Value::Array(vec![
            Value::Text(domain.into()),
            Value::Text("0.3".into()),
            Value::Text("example-run".into()),
        ]);
        assert_eq!(
            hash_cbor(&preimage, &Limits::default())
                .unwrap()
                .to_string(),
            expected
        );
    }
}

#[test]
fn encoding_errors_propagate_unchanged() {
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
    let changes: [fn(&mut Limits); 8] = [
        |l| l.max_document_bytes = 10,
        |l| l.max_text_bytes = 1,
        |l| l.max_byte_string_bytes = 1,
        |l| l.max_depth = 0,
        |l| l.max_collection_entries = 1,
        |l| l.max_total_values = 4,
        |l| l.max_total_payload_bytes = 5,
        |l| l.max_depth = 129,
    ];
    for change in changes {
        let mut limits = limits.clone();
        change(&mut limits);
        let expected = htlk_cbor::encode(&value, &limits).unwrap_err();
        assert_eq!(hash_cbor(&value, &limits).unwrap_err(), expected);
    }
    // Failed calls do not affect the fresh accounting of a later valid call.
    assert!(hash_cbor(&value, &limits).is_ok());
}
