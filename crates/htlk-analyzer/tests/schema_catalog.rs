//! Cross-document resources, equivalent copies, context, and aggregate ceilings.
use htlk_analyzer::{SchemaCatalog as C, SchemaResourceError as E};
use htlk_cbor::{LimitKind, Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{JsonDocument as J, JsonPointer as P};
fn j(text: &str) -> J {
    J::new(text.as_bytes(), &Limits::default()).unwrap()
}
fn p(text: &str) -> P {
    P::new(text, &Limits::default()).unwrap()
}

#[test]
fn cross_document_targets_keep_complete_document_and_resource_context() {
    let l = Limits::default();
    let source = j(r#"{"$defs":{"start":{"$id":"context/"}}}"#);
    let target = j(
        r#"{"$id":"https://example.test/target","$defs":{"child":{"$id":"child","$defs":{"item":{"$anchor":"Item","$dynamicAnchor":"Dynamic","type":"object"}}}},"examples":[{}]}"#,
    );
    let digest = target.digest();
    let c = C::new(
        vec![
            ("https://example.test/source".into(), source),
            ("https://catalog.test/target".into(), target),
        ],
        &l,
    )
    .unwrap();
    for reference in ["../child#Item", "../child#/$defs/item", "../child#Dynamic"] {
        let resolved = c
            .resolve(
                "https://example.test/source",
                &p("/$defs/start"),
                reference,
                &l,
            )
            .unwrap();
        assert_eq!(resolved.retrieval_uri(), "https://catalog.test/target");
        assert_eq!(resolved.document_digest(), digest);
        assert_eq!(resolved.pointer(), &p("/$defs/child/$defs/item"));
        assert!(matches!(resolved.value(), Value::Map(_)));
        assert!(std::ptr::eq(
            resolved.document(),
            c.document(resolved.retrieval_uri()).unwrap()
        ));
    }
    assert!(c.is_dynamic_anchor("https://example.test/child", "Dynamic"));
    assert_eq!(
        c.resolve(
            "https://example.test/source",
            &p(""),
            "https://catalog.test/target#/examples/0",
            &l
        )
        .unwrap_err(),
        E::NonSchemaTarget
    );
    assert_eq!(
        c.resolve("missing", &p(""), "#", &l).unwrap_err(),
        E::UnknownDocument
    );
    assert_eq!(
        c.resolve("https://example.test/source", &p(""), "absent", &l)
            .unwrap_err(),
        E::MissingResource
    );
}

#[test]
fn catalog_order_and_identical_retrieval_duplicates_do_not_change_results() {
    let l = Limits::default();
    let doc = j(r#"{"$id":"urn:shared","$anchor":"Root"}"#);
    let entries = vec![
        ("https://b.test/doc".into(), doc.clone()),
        ("https://a.test/doc".into(), doc.clone()),
    ];
    let a = C::new(entries.clone(), &l).unwrap();
    let mut reordered = entries;
    reordered.reverse();
    reordered.push(("https://a.test/doc".into(), doc));
    let b = C::new(reordered, &l).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        a.retrieval_uris().collect::<Vec<_>>(),
        ["https://a.test/doc", "https://b.test/doc"]
    );
    assert_eq!(
        a.resolve("https://b.test/doc", &p(""), "#Root", &l)
            .unwrap()
            .retrieval_uri(),
        "https://a.test/doc"
    );
    assert!(a.resource_uris().any(|uri| uri == "urn:shared"));
}

#[test]
fn equivalent_nested_resource_copies_can_share_a_uri() {
    let l = Limits::default();
    let wrapper = j(r#"{"$defs":{"child":{"$id":"urn:shared","$anchor":"Here","type":"object"}}}"#);
    let copy = j(r#"{"$id":"urn:shared","$anchor":"Here","type":"object"}"#);
    let c = C::new(
        vec![
            ("https://a.test/wrapper".into(), wrapper),
            ("https://b.test/copy".into(), copy),
        ],
        &l,
    )
    .unwrap();
    let r = c
        .resolve("https://b.test/copy", &p(""), "#Here", &l)
        .unwrap();
    assert_eq!(r.retrieval_uri(), "https://a.test/wrapper");
    assert_eq!(r.pointer(), &p("/$defs/child"));
    // Resolving the explicit retrieval alias keeps that alias's own root context.
    let r = c
        .resolve(
            "https://a.test/wrapper",
            &p(""),
            "https://b.test/copy#Here",
            &l,
        )
        .unwrap();
    assert_eq!(r.retrieval_uri(), "https://b.test/copy");
    assert_eq!(r.pointer(), &p(""));
}

#[test]
fn conflicting_retrievals_resources_and_base_contexts_fail() {
    let l = Limits::default();
    assert_eq!(
        C::new(
            vec![
                ("urn:one".into(), j("true")),
                ("urn:one".into(), j("false"))
            ],
            &l
        )
        .unwrap_err(),
        E::RetrievalConflict
    );
    let a = j(r#"{"$id":"urn:same","description":"a"}"#);
    let b = j(r#"{"$id":"urn:same","description":"b"}"#);
    assert_eq!(
        C::new(vec![("urn:a".into(), a), ("urn:b".into(), b)], &l).unwrap_err(),
        E::ResourceConflict
    );
    // Identical JSON at a retrieval alias and a nested $id can have different
    // effective bases; matching content alone must not coalesce those claims.
    let a = j(r#"{"$id":"../X"}"#);
    let b = j(r#"{"$defs":{"x":{"$id":"../X"}}}"#);
    assert_eq!(a.value(), p("/$defs/x").resolve(&b).unwrap());
    assert_eq!(
        C::new(
            vec![
                ("https://example.test/base/X".into(), a),
                ("https://example.test/base/deeper/Z".into(), b)
            ],
            &l
        )
        .unwrap_err(),
        E::ResourceConflict
    );
}

#[test]
fn opaque_source_bases_and_unknown_anchors_remain_explicit() {
    let l = Limits::default();
    let c = C::new(
        vec![
            ("urn:source".into(), j("{}")),
            (
                "https://example.test/target".into(),
                j(r#"{"$anchor":"Here"}"#),
            ),
        ],
        &l,
    )
    .unwrap();
    assert_eq!(
        c.resolve("urn:source", &p(""), "target", &l).unwrap_err(),
        E::NonHierarchicalBase
    );
    assert_eq!(
        c.resolve("urn:source", &p(""), "https://example.test/target#Here", &l)
            .unwrap()
            .pointer(),
        &p("")
    );
    assert_eq!(
        c.resolve(
            "urn:source",
            &p(""),
            "https://example.test/target#Missing",
            &l
        )
        .unwrap_err(),
        E::MissingAnchor
    );
}

#[test]
fn aggregate_storage_and_query_limits_are_enforced() {
    let l = Limits::default();
    // One true snapshot and urn:a: input URI + JSON + retained base/resource URI
    // copies + merged URI = 24 text bytes plus five metadata record markers.
    let exact = Limits {
        max_document_bytes: 29,
        ..l.clone()
    };
    let c = C::new(vec![("urn:a".into(), j("true"))], &exact).unwrap();
    let tight = Limits {
        max_document_bytes: 28,
        ..exact.clone()
    };
    assert!(matches!(
        C::new(vec![("urn:a".into(), j("true"))], &tight),
        Err(E::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
    assert!(matches!(
        C::new(
            vec![("urn:a".into(), j("true")), ("urn:b".into(), j("true"))],
            &exact
        ),
        Err(E::LimitExceeded { .. })
    ));
    let tight = Limits {
        max_document_bytes: 4,
        ..l
    };
    assert!(matches!(
        c.resolve("urn:a", &p(""), "#", &tight),
        Err(E::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
    assert!(
        C::new(vec![], &Limits::default())
            .unwrap()
            .retrieval_uris()
            .next()
            .is_none()
    );
}
