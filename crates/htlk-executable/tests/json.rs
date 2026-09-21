//! Independent JCS examples and HTLK's stricter JSON numeric boundary.
use htlk_cbor::{FiniteFloat, LimitKind, Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{JsonDocument as J, JsonError as E};

#[test]
fn canonical_strings_utf16_order_and_raw_identity() {
    let l = Limits::default();
    // RFC 8785 §3.2.3: supplementary characters sort as UTF-16 surrogate pairs.
    let authored = r#"{"\u20ac":"Euro Sign","\r":"Carriage Return","\ufb33":"Hebrew Letter Dalet With Dagesh","1":"One","\ud83d\ude00":"Emoji: Grinning Face","\u0080":"Control","\u00f6":"Latin Small Letter O With Diaeresis"}"#;
    let expected = "{\"\\r\":\"Carriage Return\",\"1\":\"One\",\"\u{80}\":\"Control\",\"ö\":\"Latin Small Letter O With Diaeresis\",\"€\":\"Euro Sign\",\"😀\":\"Emoji: Grinning Face\",\"דּ\":\"Hebrew Letter Dalet With Dagesh\"}";
    let j = J::new(authored.as_bytes(), &l).unwrap();
    assert_eq!(j.as_bytes(), expected.as_bytes());
    assert_eq!(J::decode(j.as_bytes(), &l).unwrap(), j);
    assert_eq!(J::decode(authored.as_bytes(), &l), Err(E::NonCanonical));
    assert_eq!(
        J::new(br#" "\u000f\b\t\n\f\r\/\\\"" "#, &l)
            .unwrap()
            .as_bytes(),
        br#""\u000f\b\t\n\f\r/\\\"""#
    );
    assert_ne!(
        J::new("\"é\"".as_bytes(), &l).unwrap().digest(),
        J::new("\"e\u{301}\"".as_bytes(), &l).unwrap().digest()
    );
    assert_eq!(
        J::new(b"{}", &l).unwrap().digest().to_string(),
        "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
    );
}

#[test]
fn numbers_round_correctly_and_normalize_at_the_explicit_boundary() {
    let l = Limits::default();
    // RFC 8785 §3.2.2 and appendix B vectors that satisfy HTLK's numeric profile.
    for (input, output) in [
        ("333333333.33333329", "333333333.3333333"),
        ("4.50", "4.5"),
        ("2e-3", "0.002"),
        ("0.000000000000000000000000001", "1e-27"),
        ("1e-6", "0.000001"),
        ("1e-7", "1e-7"),
        ("5e-324", "5e-324"),
        ("-5e-324", "-5e-324"),
        ("1e-99999999999999999999999", "0"),
        ("-0.000", "0"),
        ("-0e9999999999999999999999", "0"),
        ("9007199254740991.000", "9007199254740991"),
        ("9007199254740991000e-3", "9007199254740991"),
        ("-9007199254740991", "-9007199254740991"),
        ("1.0", "1"),
    ] {
        assert_eq!(
            J::new(input.as_bytes(), &l).unwrap().as_bytes(),
            output.as_bytes(),
            "{input}"
        );
    }
    assert_eq!(J::new(b"1.0", &l).unwrap().value(), &Value::Integer(1));
    assert!(matches!(
        J::new(b"0.5", &l).unwrap().value(),
        Value::Float(_)
    ));
    assert_eq!(
        J::from_value(&Value::Float(FiniteFloat::new(1.0).unwrap()), &l)
            .unwrap()
            .value(),
        &Value::Integer(1)
    );
    assert_eq!(
        J::from_value(&Value::Integer(i64::MAX), &l),
        Err(E::UnsafeNumber)
    );
    assert_eq!(
        J::from_value(&Value::Bytes(vec![]), &l),
        Err(E::UnsupportedValue)
    );
}

#[test]
fn rejects_unsafe_mathematical_integers_and_unsafe_rounded_fractions() {
    for input in [
        "9007199254740992",
        "-9007199254740992",
        "9007199254740993",
        "9007199254740992.000",
        "9007199254740992000e-3",
        "9.007199254740992e15",
        "9007199254740991.5",
        "-9007199254740991.5",
        "1e309",
        "1e999999999999999999999999",
        "100000000000000000000000.5",
    ] {
        assert_eq!(
            J::new(input.as_bytes(), &Limits::default()),
            Err(E::UnsafeNumber),
            "{input}"
        );
    }
}

#[test]
fn rejects_duplicates_malformed_strings_and_json_syntax() {
    let l = Limits::default();
    for input in [r#"{"a":1,"\u0061":2}"#, r#"{"x":{"a":null,"a":false}}"#] {
        assert!(matches!(
            J::new(input.as_bytes(), &l),
            Err(E::DuplicateKey { .. })
        ));
    }
    for input in [
        "",
        "[1,]",
        "{\"a\":1,}",
        "true false",
        "01",
        "+1",
        "1.",
        ".1",
        "1e",
        "1e+",
        "--1",
        "NaN",
        "Infinity",
        "/*x*/0",
        r#""\ud800""#,
        r#""\udc00""#,
        r#""\ud800\u0000""#,
        r#""\x00""#,
        "\"\n\"",
        "\"unfinished",
    ] {
        assert!(J::new(input.as_bytes(), &l).is_err(), "{input}");
    }
    assert!(matches!(
        J::new(&[b'"', 0xff, b'"'], &l),
        Err(E::InvalidUtf8 { .. })
    ));
}

#[test]
fn every_limit_counts_the_json_representation() {
    let cases = [
        (
            b"null".as_slice(),
            Limits {
                max_document_bytes: 3,
                ..Limits::default()
            },
            LimitKind::DocumentBytes,
        ),
        (
            br#""\ud83d\ude00""#.as_slice(),
            Limits {
                max_document_bytes: 13,
                ..Limits::default()
            },
            LimitKind::DocumentBytes,
        ),
        (
            br#"{"a":"bc"}"#.as_slice(),
            Limits {
                max_document_bytes: 9,
                ..Limits::default()
            },
            LimitKind::DocumentBytes,
        ),
        (
            br#"{"a":1}"#.as_slice(),
            Limits {
                max_document_bytes: 6,
                ..Limits::default()
            },
            LimitKind::DocumentBytes,
        ),
        (
            b"[1,2]".as_slice(),
            Limits {
                max_document_bytes: 4,
                ..Limits::default()
            },
            LimitKind::DocumentBytes,
        ),
        (
            b"[[0]]".as_slice(),
            Limits {
                max_depth: 1,
                ..Limits::default()
            },
            LimitKind::Depth,
        ),
    ];
    for (input, l, expected) in cases {
        assert!(
            matches!(J::new(input, &l), Err(E::LimitExceeded { limit, .. }) if limit == expected)
        );
    }
    let l = Limits {
        max_document_bytes: 2,
        ..Limits::default()
    };
    assert!(matches!(
        J::from_value(&Value::Text("\n".into()), &l),
        Err(E::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
    let l = Limits {
        max_document_bytes: 14,
        ..Limits::default()
    };
    assert!(J::new(br#""\ud83d\ude00""#, &l).is_ok());
    let l = Limits {
        max_document_bytes: 10,
        ..Limits::default()
    };
    assert!(J::new(br#"{"a":"bc"}"#, &l).is_ok());
}

fn depth_exercise() {
    let l = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    for (open, close) in [("[", "]"), ("{\"x\":", "}")] {
        let input = format!("{}0{}", open.repeat(128), close.repeat(128));
        let doc = J::decode(input.as_bytes(), &l).unwrap();
        assert_eq!(doc.clone(), doc);
        assert_eq!(J::from_value(doc.value(), &l).unwrap(), doc);
        let too_deep = format!("{open}{input}{close}");
        assert!(matches!(
            J::new(too_deep.as_bytes(), &l),
            Err(E::LimitExceeded {
                limit: LimitKind::Depth,
                ..
            })
        ));
        let malformed = format!("[{input},");
        assert!(J::new(malformed.as_bytes(), &l).is_err());
    }
}
#[test]
fn json_on_controlled_stacks() {
    const CHILD: &str = "HTLK_JSON_DEPTH_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(depth_exercise)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "json_on_controlled_stacks", "--nocapture"])
            .env(CHILD, size.to_string())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stack {size}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
