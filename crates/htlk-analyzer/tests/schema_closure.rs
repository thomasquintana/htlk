//! Conservative reference closure with explicit retrieval contexts and no I/O.
use htlk_analyzer::{SchemaCatalog as C, SchemaReferenceKind as K, SchemaResourceError as E};
use htlk_cbor::{LimitKind, Limits};
use htlk_executable::JsonDocument as J;
use htlk_executable::cbor as htlk_cbor;
fn j(s: &str) -> J {
    J::new(s.as_bytes(), &Limits::default()).unwrap()
}

#[test]
fn transitive_cycles_and_dynamic_initial_targets_are_recorded_once() {
    let l = Limits::default();
    let c = C::new(
        vec![
            ("https://e.test/a".into(), j(r#"{"$ref":"b"}"#)),
            (
                "https://e.test/b".into(),
                j(r#"{"$ref":"a","$defs":{"inner":{"$dynamicRef":"c#Here"}}}"#),
            ),
            ("https://e.test/c".into(), j(r#"{"$dynamicAnchor":"Here"}"#)),
        ],
        &l,
    )
    .unwrap();
    let closure = c.reference_closure(&["https://e.test/a"], &l).unwrap();
    assert_eq!(
        closure.retrieval_uris(),
        &["https://e.test/a", "https://e.test/b", "https://e.test/c"]
    );
    assert_eq!(closure.references().len(), 3);
    let dynamic = closure
        .references()
        .iter()
        .find(|r| r.kind() == K::DynamicRef)
        .unwrap();
    assert_eq!(dynamic.source_retrieval_uri(), "https://e.test/b");
    assert_eq!(dynamic.source_pointer().to_string(), "/$defs/inner");
    assert_eq!(dynamic.reference(), "c#Here");
    assert_eq!(dynamic.target().retrieval_uri(), "https://e.test/c");
    assert_eq!(
        c.reference_closure(&["https://e.test/b", "https://e.test/a"], &l)
            .unwrap(),
        closure
    );
}

#[test]
fn unrelated_aliases_and_annotation_references_do_not_enter_closure() {
    let l = Limits::default();
    let doc = j(r##"{"$id":"urn:shared","$ref":"#","examples":[{"$ref":"urn:missing"}]}"##);
    let c = C::new(
        vec![
            ("https://a.test/unused-alias".into(), doc.clone()),
            ("https://z.test/root".into(), doc),
            (
                "https://unused.test/bad-reference".into(),
                j(r#"{"$ref":null}"#),
            ),
        ],
        &l,
    )
    .unwrap();
    let closure = c.reference_closure(&["https://z.test/root"], &l).unwrap();
    assert_eq!(closure.retrieval_uris(), &["https://z.test/root"]);
    assert_eq!(closure.references().len(), 1);
    assert_eq!(
        closure.references()[0].target().retrieval_uri(),
        "https://z.test/root"
    );
}

#[test]
fn nested_resources_can_be_discovered_through_later_reference_paths() {
    let l = Limits::default();
    let root = j(r#"{"$ref":"urn:nested","$defs":{"load":{"$ref":"b"}}}"#);
    let b = j(r#"{"$defs":{"nested":{"$id":"urn:nested"}}}"#);
    let c = C::new(
        vec![
            ("https://e.test/a".into(), root),
            ("https://e.test/b".into(), b.clone()),
        ],
        &l,
    )
    .unwrap();
    let closure = c.reference_closure(&["https://e.test/a"], &l).unwrap();
    assert_eq!(
        closure.retrieval_uris(),
        &["https://e.test/a", "https://e.test/b"]
    );
    assert_eq!(
        closure.references()[0].target().pointer().to_string(),
        "/$defs/nested"
    );
    // The complete reached document is scanned, including its $defs. Without
    // the reference to b, the catalog does not invent a retrieval for urn:nested.
    let c = C::new(
        vec![
            ("https://e.test/a".into(), j(r#"{"$ref":"urn:nested"}"#)),
            ("https://e.test/b".into(), b),
        ],
        &l,
    )
    .unwrap();
    assert_eq!(
        c.reference_closure(&["https://e.test/a"], &l).unwrap_err(),
        E::MissingResource
    );
    assert!(
        c.reference_closure(&["https://e.test/a", "https://e.test/b"], &l)
            .is_ok()
    );
}

#[test]
fn malformed_missing_and_non_schema_references_fail() {
    let l = Limits::default();
    for (json, expected) in [
        (r#"{"$ref":false}"#, E::InvalidReference("$ref")),
        (r#"{"$dynamicRef":1}"#, E::InvalidReference("$dynamicRef")),
        (r#"{"$ref":"urn:missing"}"#, E::MissingResource),
        (
            r##"{"$ref":"#/examples/0","examples":[{}]}"##,
            E::NonSchemaTarget,
        ),
    ] {
        let c = C::new(vec![("urn:root".into(), j(json))], &l).unwrap();
        assert_eq!(
            c.reference_closure(&["urn:root"], &l).unwrap_err(),
            expected
        );
    }
    let c = C::new(vec![], &l).unwrap();
    assert_eq!(
        c.reference_closure(&["urn:missing"], &l).unwrap_err(),
        E::UnknownDocument
    );
    assert!(
        c.reference_closure(&[], &l)
            .unwrap()
            .references()
            .is_empty()
    );
}

#[test]
fn closure_has_fresh_work_snapshot_and_context_limits() {
    let c = C::new(vec![("urn:a".into(), j("{}"))], &Limits::default()).unwrap();
    // Context URI + snapshot + known resource URI: 5 + 2 + 5 = 12 bytes.
    // Context, resource, and schema-location visit: three one-byte markers.
    let exact = Limits {
        max_document_bytes: 15,
        ..Limits::default()
    };
    assert!(c.reference_closure(&["urn:a"], &exact).is_ok());
    let tight = Limits {
        max_document_bytes: 14,
        ..exact.clone()
    };
    assert_eq!(
        c.reference_closure(&["urn:a"], &tight).unwrap_err(),
        E::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: 14
        }
    );
    let tight = Limits {
        max_document_bytes: 4,
        ..exact
    };
    assert!(matches!(
        c.reference_closure(&["urn:a"], &tight),
        Err(E::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
}

#[test]
fn long_reference_cycles_do_not_consume_recursive_depth() {
    let l = Limits::default();
    let docs = (0..300)
        .map(|i| {
            (
                format!("urn:{i}"),
                j(&format!(r#"{{"$ref":"urn:{}"}}"#, (i + 1) % 300)),
            )
        })
        .collect();
    let c = C::new(docs, &l).unwrap();
    let tight = Limits { max_depth: 2, ..l };
    let closure = c.reference_closure(&["urn:0"], &tight).unwrap();
    assert_eq!(closure.retrieval_uris().len(), 300);
    assert_eq!(closure.references().len(), 300);
}

#[test]
fn short_references_cannot_amplify_large_resource_pointer_work() {
    let l = Limits::default();
    let fields = (0..20)
        .map(|i| format!(r#""x{i}":{{"$ref":"urn:deep#/not"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let a = j(&format!(r#"{{"$defs":{{{fields}}}}}"#));
    let b = j(&format!(
        r#"{{"$defs":{{"{}":{{"$id":"urn:deep","not":true}}}}}}"#,
        "k".repeat(1000)
    ));
    let c = C::new(vec![("urn:a".into(), a), ("urn:b".into(), b)], &l).unwrap();
    assert_eq!(
        c.reference_closure(&["urn:a", "urn:b"], &l)
            .unwrap()
            .references()
            .len(),
        20
    );
    let tight = Limits {
        max_document_bytes: 6000,
        ..l
    };
    assert_eq!(
        c.reference_closure(&["urn:a", "urn:b"], &tight)
            .unwrap_err(),
        E::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: 6000
        }
    );
}
