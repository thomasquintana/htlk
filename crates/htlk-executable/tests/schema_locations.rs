//! Standard 2020-12 location discovery, data exclusion, and derived-index limits.
use htlk_cbor::{LimitKind, Limits};
use htlk_executable::{
    JSON_SCHEMA_DIALECT, JsonDocument as J, JsonPointer as P, SchemaLocationError as E,
    SchemaLocations as S,
};

#[test]
fn keyword_families_discover_exact_schema_locations() {
    let l = Limits::default();
    for keyword in [
        "additionalProperties",
        "contains",
        "contentSchema",
        "else",
        "if",
        "items",
        "not",
        "propertyNames",
        "then",
        "unevaluatedItems",
        "unevaluatedProperties",
    ] {
        let doc = J::new(format!(r#"{{"{keyword}":false}}"#).as_bytes(), &l).unwrap();
        let index = S::new(&doc, &l).unwrap();
        assert_eq!(index.pointers().len(), 2);
        assert!(index.contains(&P::new(&format!("/{keyword}"), &l).unwrap()));
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        let doc = J::new(format!(r#"{{"{keyword}":[true,{{}}]}}"#).as_bytes(), &l).unwrap();
        let index = S::new(&doc, &l).unwrap();
        assert_eq!(index.pointers().len(), 3);
        assert!(!index.contains(&P::new(&format!("/{keyword}"), &l).unwrap()));
        assert!(index.contains(&P::new(&format!("/{keyword}/1"), &l).unwrap()));
    }
    for keyword in [
        "$defs",
        "dependentSchemas",
        "patternProperties",
        "properties",
    ] {
        let doc = J::new(
            format!(r#"{{"{keyword}":{{"a/b~c":true,"":false}}}}"#).as_bytes(),
            &l,
        )
        .unwrap();
        let index = S::new(&doc, &l).unwrap();
        assert_eq!(index.document_digest(), doc.digest());
        assert_eq!(index.pointers().len(), 3);
        assert!(index.contains(&P::new(&format!("/{keyword}/a~1b~0c"), &l).unwrap()));
        assert!(index.contains(&P::new(&format!("/{keyword}/"), &l).unwrap()));
        for p in index.pointers() {
            assert!(p.resolve(&doc).is_ok());
        }
    }
}

#[test]
fn instance_and_annotation_objects_do_not_declare_schema_resources() {
    let l = Limits::default();
    let doc = J::new(br#"{"examples":[{"$id":"urn:example","not":null}],"default":{"$id":"urn:default"},"const":{"$schema":"wrong"},"enum":[{"properties":7}],"custom":{"$id":"urn:custom"},"properties":{"real":{"$id":"urn:real","examples":[{"$id":"urn:fake"}]}}}"#, &l).unwrap();
    let index = S::new(&doc, &l).unwrap();
    assert_eq!(
        index
            .pointers()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["", "/properties/real"]
    );
    assert!(!index.contains(&P::new("/examples/0", &l).unwrap()));
    assert!(P::new("/examples/0", &l).unwrap().resolve(&doc).is_ok());
}

#[test]
fn boolean_roots_and_dialect_selection_are_explicit() {
    let l = Limits::default();
    for value in ["true", "false", "{}"] {
        assert_eq!(
            S::new(&J::new(value.as_bytes(), &l).unwrap(), &l)
                .unwrap()
                .pointers()
                .len(),
            1
        );
    }
    for dialect in [
        JSON_SCHEMA_DIALECT.to_owned(),
        format!("{JSON_SCHEMA_DIALECT}#"),
    ] {
        assert!(
            S::new(
                &J::new(format!(r#"{{"$schema":"{dialect}"}}"#).as_bytes(), &l).unwrap(),
                &l
            )
            .is_ok()
        );
    }
    for json in [
        r#"{"$schema":"http://json-schema.org/draft-07/schema#"}"#,
        r#"{"$defs":{"child":{"$schema":"urn:other"}}}"#,
    ] {
        assert_eq!(
            S::new(&J::new(json.as_bytes(), &l).unwrap(), &l).unwrap_err(),
            E::UnsupportedDialect
        );
    }
    assert_eq!(
        S::new(&J::new(br#"{"$schema":null}"#, &l).unwrap(), &l).unwrap_err(),
        E::InvalidKeyword("$schema")
    );
}

#[test]
fn schema_positions_and_applicator_containers_require_their_defined_shapes() {
    let l = Limits::default();
    for json in [
        "null",
        "[]",
        "1",
        r#"{"not":null}"#,
        r#"{"properties":{"x":"not a schema"}}"#,
        r#"{"allOf":[3]}"#,
    ] {
        assert_eq!(
            S::new(&J::new(json.as_bytes(), &l).unwrap(), &l).unwrap_err(),
            E::InvalidSchema
        );
    }
    for (json, keyword) in [
        (r#"{"allOf":[]}"#, "allOf"),
        (r#"{"prefixItems":true}"#, "prefixItems"),
        (r#"{"properties":[]}"#, "properties"),
        (r#"{"$defs":null}"#, "$defs"),
    ] {
        assert_eq!(
            S::new(&J::new(json.as_bytes(), &l).unwrap(), &l).unwrap_err(),
            E::InvalidKeyword(keyword)
        );
    }
}

#[test]
fn derived_index_accounting_can_reject_an_individually_valid_json_document() {
    let l = Limits::default();
    let doc = J::new(
        format!("{}true{}", "{\"not\":".repeat(30), "}".repeat(30)).as_bytes(),
        &l,
    )
    .unwrap();
    for (tight, kind) in [
        (
            Limits {
                max_collection_entries: 30,
                ..l.clone()
            },
            LimitKind::CollectionEntries,
        ),
        (
            Limits {
                max_total_values: 200,
                ..l.clone()
            },
            LimitKind::TotalValues,
        ),
        (
            Limits {
                max_total_payload_bytes: 200,
                ..l.clone()
            },
            LimitKind::TotalPayloadBytes,
        ),
    ] {
        assert!(J::decode(doc.as_bytes(), &tight).is_ok());
        assert!(
            matches!(S::new(&doc, &tight), Err(E::LimitExceeded { limit, .. }) if limit == kind)
        );
    }
    // 31 locations, 0+1+...+30 tokens, four encoded bytes per path token.
    let exact = Limits {
        max_collection_entries: 31,
        max_total_values: 496,
        max_total_payload_bytes: 1860,
        ..l
    };
    assert_eq!(S::new(&doc, &exact).unwrap().pointers().len(), 31);
}

fn exercise() {
    let l = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    let doc = J::new(
        format!("{}true{}", "{\"not\":".repeat(128), "}".repeat(128)).as_bytes(),
        &l,
    )
    .unwrap();
    let index = S::new(&doc, &l).unwrap();
    assert_eq!(index.pointers().len(), 129);
    let last = P::new(&"/not".repeat(128), &l).unwrap();
    assert!(index.contains(&last));
    assert_eq!(index.clone(), index);
    assert!(last.resolve(&doc).is_ok());
}
#[test]
fn schema_locations_on_controlled_stacks() {
    const CHILD: &str = "HTLK_SCHEMA_LOCATION_STACK";
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
                "schema_locations_on_controlled_stacks",
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
