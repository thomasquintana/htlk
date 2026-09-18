//! Resource-root context, static/initial dynamic anchors, RFC URI behavior and bounds.
use htlk_analyzer::{SchemaResourceError as E, SchemaResources as R};
use htlk_cbor::{LimitKind, Limits};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{JsonDocument as J, JsonPointer as P};
fn p(s: &str) -> P {
    P::new(s, &Limits::default()).unwrap()
}

#[test]
fn nested_ids_and_pointer_fragments_use_the_actual_resource_root() {
    let l = Limits::default();
    let doc = J::new(br#"{"$defs":{"child":{"$id":"child","$defs":{"item":{"type":"object","$anchor":"Item","$dynamicAnchor":"Dynamic"}}},"item":{"type":"string","$anchor":"Item"}},"examples":[{"$id":"fake"}]}"#, &l).unwrap();
    let r = R::new(&doc, "https://example.test/root", &l).unwrap();
    assert_eq!(r.document_digest(), doc.digest());
    assert_eq!(
        r.resources()
            .map(|(u, p)| (u, p.to_string()))
            .collect::<Vec<_>>(),
        [
            ("https://example.test/child", "/$defs/child".into()),
            ("https://example.test/root", "".into())
        ]
    );
    assert_eq!(
        r.base_uri(&p("/$defs/child/$defs/item")),
        Some("https://example.test/child")
    );
    for reference in [
        "child#/$defs/item",
        "child#%2F%24defs%2Fitem",
        "child#Item",
        "child#%49tem",
        "child#Dynamic",
    ] {
        assert_eq!(
            r.resolve(&p(""), reference, &l).unwrap(),
            &p("/$defs/child/$defs/item")
        );
    }
    assert_eq!(r.resolve(&p(""), "#Item", &l).unwrap(), &p("/$defs/item"));
    assert_eq!(
        r.resolve(&p("/$defs/child"), "#Item", &l).unwrap(),
        &p("/$defs/child/$defs/item")
    );
    assert_eq!(r.resolve(&p(""), "child#", &l).unwrap(), &p("/$defs/child"));
    assert!(r.is_dynamic_anchor("https://example.test/child", "Dynamic"));
    assert!(!r.is_dynamic_anchor("https://example.test/root", "Dynamic"));
    assert_eq!(
        r.resolve(&p(""), "#/examples/0", &l),
        Err(E::NonSchemaTarget)
    );
    assert_eq!(
        r.resolve(&p("/examples/0"), "#", &l),
        Err(E::UnknownLocation)
    );
    assert_eq!(r.resolve(&p(""), "fake", &l), Err(E::MissingResource));
}

#[test]
fn retrieval_alias_root_id_and_rfc_dot_segments_are_preserved() {
    let l = Limits::default();
    let doc = J::new(br#"{"$id":"https://ids.test/base/root#","$defs":{"a":{"$id":"./child"},"b":{"$id":"%2e%2e/encoded"},"c":{"$id":"../up"}}}"#, &l).unwrap();
    let r = R::new(&doc, "https://retrieval.test/snapshot", &l).unwrap();
    assert_eq!(r.base_uri(&p("")), Some("https://ids.test/base/root"));
    assert_eq!(
        r.resolve(&p(""), "https://retrieval.test/snapshot#/$defs/a", &l)
            .unwrap(),
        &p("/$defs/a")
    );
    assert_eq!(r.resolve(&p(""), "../up", &l).unwrap(), &p("/$defs/c"));
    assert_eq!(
        r.base_uri(&p("/$defs/b")),
        Some("https://ids.test/base/%2e%2e/encoded")
    );
    assert_eq!(
        r.resolve(&p(""), "%2e%2e/encoded", &l).unwrap(),
        &p("/$defs/b")
    );
}

#[test]
fn opaque_bases_allow_local_fragments_but_not_relative_paths() {
    let l = Limits::default();
    let doc = J::new(br#"{"$defs":{"x":{"$anchor":"Here"}}}"#, &l).unwrap();
    let r = R::new(&doc, "urn:htlk:schema:example", &l).unwrap();
    assert_eq!(r.resolve(&p(""), "#Here", &l).unwrap(), &p("/$defs/x"));
    assert_eq!(r.resolve(&p(""), "", &l).unwrap(), &p(""));
    for reference in ["other.json", "?query", "/root", "//host/path"] {
        assert_eq!(
            r.resolve(&p(""), reference, &l),
            Err(E::NonHierarchicalBase)
        );
    }
    assert_eq!(
        r.resolve(&p(""), "https://external.test/schema", &l),
        Err(E::MissingResource)
    );
    assert_eq!(r.resolve(&p(""), "#Missing", &l), Err(E::MissingAnchor));
}

#[test]
fn resource_and_anchor_conflicts_and_malformed_declarations_fail() {
    let l = Limits::default();
    for (json, expected) in [
        (r#"{"$id":1}"#, E::InvalidKeyword("$id")),
        (
            r#"{"$id":"https://example.test/root#fragment"}"#,
            E::InvalidKeyword("$id"),
        ),
        (r#"{"$anchor":"1bad"}"#, E::InvalidKeyword("$anchor")),
        (
            r#"{"$dynamicAnchor":false}"#,
            E::InvalidKeyword("$dynamicAnchor"),
        ),
        (
            r#"{"$defs":{"a":{"$id":"same"},"b":{"$id":"same"}}}"#,
            E::ResourceConflict,
        ),
        (
            r#"{"$defs":{"a":{"$anchor":"same"},"b":{"$dynamicAnchor":"same"}}}"#,
            E::AnchorConflict,
        ),
    ] {
        assert_eq!(
            R::new(
                &J::new(json.as_bytes(), &l).unwrap(),
                "https://example.test/root",
                &l
            )
            .unwrap_err(),
            expected
        );
    }
    let doc = J::new(br#"{"$anchor":"same","$dynamicAnchor":"same"}"#, &l).unwrap();
    let r = R::new(&doc, "https://example.test/root", &l).unwrap();
    assert!(r.is_dynamic_anchor("https://example.test/root", "same"));
    for uri in [
        "relative",
        "https://example.test/root#",
        "https://example.test/%gg",
        "https://example.test/a b",
    ] {
        assert_eq!(R::new(&doc, uri, &l).unwrap_err(), E::InvalidUri);
    }
}

#[test]
fn metadata_and_resolution_apply_effective_limits() {
    let l = Limits::default();
    let doc = J::new(b"true", &l).unwrap();
    // Retrieval key plus base: two records, two copies of eight-byte urn:test.
    let exact = Limits {
        max_collection_entries: 2,
        max_total_values: 2,
        max_total_payload_bytes: 16,
        ..l.clone()
    };
    let r = R::new(&doc, "urn:test", &exact).unwrap();
    let tight = Limits {
        max_total_payload_bytes: 15,
        ..exact.clone()
    };
    assert!(matches!(
        R::new(&doc, "urn:test", &tight),
        Err(E::LimitExceeded {
            limit: LimitKind::TotalPayloadBytes,
            ..
        })
    ));
    let tight = Limits {
        max_collection_entries: 1,
        ..exact
    };
    assert!(matches!(
        R::new(&doc, "urn:test", &tight),
        Err(E::LimitExceeded {
            limit: LimitKind::CollectionEntries,
            ..
        })
    ));
    let tight = Limits {
        max_text_bytes: 7,
        ..l
    };
    assert!(matches!(
        r.resolve(&p(""), "#", &tight),
        Err(E::LimitExceeded {
            limit: LimitKind::TextBytes,
            ..
        })
    ));
}

fn exercise() {
    let l = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    let doc = J::new(
        format!(
            "{}true{}",
            "{\"$id\":\"child/\",\"not\":".repeat(128),
            "}".repeat(128)
        )
        .as_bytes(),
        &l,
    )
    .unwrap();
    let r = R::new(&doc, "https://example.test/root/", &l).unwrap();
    let leaf = P::new(&"/not".repeat(128), &l).unwrap();
    assert_eq!(r.resolve(&leaf, "#/not", &l).unwrap(), &leaf);
    assert_eq!(r.clone(), r);
}
#[test]
fn schema_resources_on_controlled_stacks() {
    const CHILD: &str = "HTLK_SCHEMA_RESOURCE_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(exercise)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "schema_resources_on_controlled_stacks",
                "--nocapture",
            ])
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

#[test]
fn embedded_bases_use_absolute_root_ids_or_raw_jcs_identity() {
    use htlk_analyzer::embedded_schema_base as base;
    let l = Limits::default();
    let doc = J::new(b"{}", &l).unwrap();
    assert_eq!(
        base(&doc, &l).unwrap(),
        "urn:htlk:schema:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
    );
    for (json, expected) in [
        (r#"{"$id":"https://e.test/root"}"#, "https://e.test/root"),
        (r#"{"$id":"https://e.test/root#"}"#, "https://e.test/root"),
        (r#"{"$id":"urn:Exact"}"#, "urn:Exact"),
    ] {
        let doc = J::new(json.as_bytes(), &l).unwrap();
        let before = doc.clone();
        assert_eq!(base(&doc, &l).unwrap(), expected);
        assert_eq!(doc, before);
    }
    let relative = J::new(br#"{"$id":"relative"}"#, &l).unwrap();
    let synthetic = base(&relative, &l).unwrap();
    assert!(synthetic.starts_with("urn:htlk:schema:"));
    assert_eq!(
        R::new(&relative, &synthetic, &l).unwrap_err(),
        E::NonHierarchicalBase
    );
    for json in [
        r#"{"$id":1}"#,
        r#"{"$id":"bad uri"}"#,
        r#"{"$id":"https://e.test/root#anchor"}"#,
    ] {
        assert_eq!(
            base(&J::new(json.as_bytes(), &l).unwrap(), &l).unwrap_err(),
            E::InvalidKeyword("$id")
        );
    }
    let tight = Limits {
        max_text_bytes: 10,
        ..l
    };
    assert!(matches!(
        base(&doc, &tight),
        Err(E::LimitExceeded {
            limit: LimitKind::TextBytes,
            ..
        })
    ));
}
