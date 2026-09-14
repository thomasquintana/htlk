//! RFC 6901 lookup, fragment decoding, and bounded pointer construction.
use htlk_cbor::{LimitKind, Limits, Value};
use htlk_executable::{JsonDocument, JsonPointer as P, JsonPointerError as E};

#[test]
fn rfc_6901_examples_and_escaped_tokens() {
    let l = Limits::default();
    let doc = JsonDocument::new(br#"{"foo":["bar","baz"],"":0,"a/b":1,"c%d":2,"e^f":3,"g|h":4,"i\\j":5,"k\"l":6," ":7,"m~n":8,"~1":9}"#, &l).unwrap();
    assert!(std::ptr::eq(
        P::new("", &l).unwrap().resolve(&doc).unwrap(),
        doc.value()
    ));
    for (pointer, n) in [
        ("/", 0),
        ("/a~1b", 1),
        ("/c%d", 2),
        ("/e^f", 3),
        ("/g|h", 4),
        ("/i\\j", 5),
        ("/k\"l", 6),
        ("/ ", 7),
        ("/m~0n", 8),
        ("/~01", 9),
    ] {
        let p = P::new(pointer, &l).unwrap();
        assert_eq!(p.resolve(&doc).unwrap(), &Value::Integer(n));
        assert_eq!(p.to_string(), pointer);
    }
    assert_eq!(
        P::new("/foo/0", &l).unwrap().resolve(&doc).unwrap(),
        &Value::Text("bar".into())
    );
    assert_eq!(P::new("/a~1b", &l).unwrap().tokens(), &["a/b"]);
}

#[test]
fn fragment_percent_decoding_precedes_pointer_escaping() {
    let l = Limits::default();
    for (fragment, plain) in [
        ("#", ""),
        ("#/a~1b", "/a~1b"),
        ("#%2Fa%7E1b", "/a~1b"),
        ("#/%C3%A9", "/é"),
        ("#/a+b", "/a+b"),
        ("#/%25", "/%"),
        ("#/%00", "/\0"),
        ("#/%252F", "/%2F"),
        ("#/%7E01", "/~01"),
    ] {
        assert_eq!(
            P::from_fragment(fragment, &l).unwrap(),
            P::new(plain, &l).unwrap()
        );
    }
    assert_ne!(
        P::from_fragment("#/%C3%A9", &l).unwrap(),
        P::from_fragment("#/e%CC%81", &l).unwrap()
    );
    for s in ["", "/a", "#/a b", "#/é", "#/%", "#/%0z", "#/a#b", "#/[a]"] {
        assert!(
            matches!(P::from_fragment(s, &l), Err(E::InvalidFragment { .. })),
            "{s}"
        );
    }
    assert!(matches!(
        P::from_fragment("#/%ff", &l),
        Err(E::InvalidUtf8 { .. })
    ));
    assert!(matches!(
        P::from_fragment("#anchor", &l),
        Err(E::InvalidSyntax { offset: 0 })
    ));
    assert!(matches!(
        P::from_fragment("#/%7e2", &l),
        Err(E::InvalidSyntax { offset: 1 })
    ));
}

#[test]
fn array_indices_are_strict_but_object_keys_are_arbitrary() {
    let l = Limits::default();
    let doc = JsonDocument::new(br#"{"a":[null],"o":{"01":true,"-":false},"n":null}"#, &l).unwrap();
    assert_eq!(
        P::new("/a/0", &l).unwrap().resolve(&doc).unwrap(),
        &Value::Null
    );
    assert_eq!(
        P::new("/o/01", &l).unwrap().resolve(&doc).unwrap(),
        &Value::Bool(true)
    );
    assert_eq!(
        P::new("/o/-", &l).unwrap().resolve(&doc).unwrap(),
        &Value::Bool(false)
    );
    for s in ["/a/01", "/a/-1", "/a/+0", "/a/", "/a/1.0", "/a/٠"] {
        assert_eq!(
            P::new(s, &l).unwrap().resolve(&doc),
            Err(E::InvalidIndex { segment: 1 })
        );
    }
    for s in [
        "/a/1",
        "/a/-",
        "/a/999999999999999999999999999999999999999999",
    ] {
        assert_eq!(
            P::new(s, &l).unwrap().resolve(&doc),
            Err(E::MissingTarget { segment: 1 })
        );
    }
    assert_eq!(
        P::new("/missing", &l).unwrap().resolve(&doc),
        Err(E::MissingTarget { segment: 0 })
    );
    assert_eq!(
        P::new("/n/x", &l).unwrap().resolve(&doc),
        Err(E::ScalarTraversal { segment: 1 })
    );
}

#[test]
fn syntax_errors_have_redacted_precise_offsets() {
    for (s, offset) in [
        ("sensitive_pointer", 0),
        ("/~", 1),
        ("/~2", 1),
        ("/é/~x", 4),
    ] {
        let e = P::new(s, &Limits::default()).unwrap_err();
        assert_eq!(e, E::InvalidSyntax { offset });
        assert!(!e.to_string().contains(s));
    }
}

#[test]
fn pointer_limits_cover_decoded_text_tokens_and_input() {
    let cases = [
        (
            "/ab",
            Limits {
                max_document_bytes: 2,
                ..Limits::default()
            },
            LimitKind::DocumentBytes,
        ),
        (
            "/é",
            Limits {
                max_text_bytes: 1,
                ..Limits::default()
            },
            LimitKind::TextBytes,
        ),
        (
            "/ab/c",
            Limits {
                max_total_payload_bytes: 2,
                ..Limits::default()
            },
            LimitKind::TotalPayloadBytes,
        ),
        (
            "/a/b",
            Limits {
                max_collection_entries: 1,
                ..Limits::default()
            },
            LimitKind::CollectionEntries,
        ),
        (
            "/a/b",
            Limits {
                max_total_values: 1,
                ..Limits::default()
            },
            LimitKind::TotalValues,
        ),
        (
            "/a/b",
            Limits {
                max_depth: 1,
                ..Limits::default()
            },
            LimitKind::Depth,
        ),
    ];
    for (text, l, expected) in cases {
        assert!(
            matches!(P::new(text, &l), Err(E::LimitExceeded { limit, .. }) if limit == expected)
        );
    }
    let l = Limits {
        max_text_bytes: 1,
        max_total_payload_bytes: 2,
        max_depth: 2,
        max_collection_entries: 2,
        max_total_values: 2,
        ..Limits::default()
    };
    assert_eq!(P::new("/~0/~1", &l).unwrap().tokens(), &["~", "/"]);
    assert!(matches!(
        P::from_fragment(
            "#/%61",
            &Limits {
                max_document_bytes: 4,
                ..Limits::default()
            }
        ),
        Err(E::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
    assert!(matches!(
        P::new(
            "",
            &Limits {
                max_depth: 129,
                ..Limits::default()
            }
        ),
        Err(E::Codec(_))
    ));
}

#[test]
fn maximum_depth_lookup_is_iterative_and_borrowed() {
    let l = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    let doc = JsonDocument::new(
        format!("{}null{}", "[".repeat(128), "]".repeat(128)).as_bytes(),
        &l,
    )
    .unwrap();
    let p = P::new(&"/0".repeat(128), &l).unwrap();
    assert_eq!(p.resolve(&doc).unwrap(), &Value::Null);
    assert_eq!(P::new(&p.to_string(), &l).unwrap(), p);
    assert!(matches!(
        P::new(&"/0".repeat(129), &l),
        Err(E::LimitExceeded {
            limit: LimitKind::Depth,
            ..
        })
    ));
}
