//! Native 2020-12 validation, resource identity, and supported-limit behavior.
use htlk_analyzer::{
    NativeSchemaError as E, NativeSchemaOptions as O, NativeSchemas as N, SchemaCatalog as C,
};
use htlk_cbor::{FiniteFloat, Limits, Value};
use htlk_executable::JsonDocument as J;
use htlk_executable::cbor as htlk_cbor;
fn j(s: &str) -> J {
    J::new(s.as_bytes(), &Limits::default()).unwrap()
}
fn compile(schema: &str) -> N {
    N::compile(
        &C::new(
            vec![("https://e.test/root".into(), j(schema))],
            &Limits::default(),
        )
        .unwrap(),
        O::default(),
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn native_keywords_and_json_numeric_semantics_work_out_of_the_box() {
    let n = compile(
        r#"{"type":"object","properties":{"n":{"type":"integer","minimum":1},"s":{"type":"string","minLength":1}},"required":["n"],"additionalProperties":false}"#,
    );
    let l = Limits::default();
    n.require_object_root("https://e.test/root").unwrap();
    assert!(
        n.validate("https://e.test/root", &j(r#"{"n":1,"s":"é"}"#), &l)
            .unwrap()
    );
    for value in [r#"{"n":0}"#, r#"{"n":1,"extra":true}"#, "[]", "null"] {
        assert!(!n.validate("https://e.test/root", &j(value), &l).unwrap());
    }
    let n = compile(r#"{"type":"integer"}"#);
    let value = Value::Float(FiniteFloat::new(1.0).unwrap());
    assert!(n.validate_value("https://e.test/root", &value, &l).unwrap());
    assert!(matches!(value, Value::Float(_)));
    assert!(
        n.validate_value("https://e.test/root", &Value::Bytes(vec![]), &l)
            .is_err()
    );
}

#[test]
fn conditional_composition_and_unevaluated_properties_are_enforced() {
    let n = compile(
        r#"{"type":"object","properties":{"flag":{"type":"boolean"}},"if":{"properties":{"flag":{"const":true}},"required":["flag"]},"then":{"properties":{"yes":{"type":"string"}},"required":["yes"]},"else":{"properties":{"no":{"type":"number"}},"required":["no"]},"unevaluatedProperties":false}"#,
    );
    let l = Limits::default();
    for v in [r#"{"flag":true,"yes":"ok"}"#, r#"{"flag":false,"no":2}"#] {
        assert!(n.validate("https://e.test/root", &j(v), &l).unwrap());
    }
    for v in [
        r#"{"flag":true,"no":2}"#,
        r#"{"flag":false,"no":2,"extra":1}"#,
    ] {
        assert!(!n.validate("https://e.test/root", &j(v), &l).unwrap());
    }
    let n = compile(
        r#"{"prefixItems":[{"type":"string"}],"items":{"type":"integer"},"contains":{"const":3},"minContains":1,"maxContains":1,"uniqueItems":true}"#,
    );
    assert!(
        n.validate("https://e.test/root", &j(r#"["x",2,3]"#), &l)
            .unwrap()
    );
    assert!(
        !n.validate("https://e.test/root", &j(r#"["x",3,3]"#), &l)
            .unwrap()
    );
}

#[test]
fn offline_references_preserve_percent_encoded_dot_segments_and_root_admission() {
    let l = Limits::default();
    let root = j(r#"{"$ref":"%2e%2e/target"}"#);
    let digest = root.digest();
    let c = C::new(
        vec![
            ("https://e.test/base/root".into(), root),
            (
                "https://e.test/base/%2e%2e/target".into(),
                j(r#"{"type":"object","required":["x"]}"#),
            ),
        ],
        &l,
    )
    .unwrap();
    let n = N::compile(&c, O::default(), &l).unwrap();
    assert_eq!(n.document_digest("https://e.test/base/root"), Some(digest));
    n.require_object_root("https://e.test/base/root").unwrap();
    assert!(
        n.validate("https://e.test/base/root", &j(r#"{"x":1}"#), &l)
            .unwrap()
    );
    assert!(
        !n.validate("https://e.test/base/root", &j("{}"), &l)
            .unwrap()
    );
    assert_eq!(
        compile("{}").require_object_root("https://e.test/root"),
        Err(E::ObjectRootRequired)
    );
    let missing = C::new(
        vec![(
            "urn:root".into(),
            j(r#"{"$ref":"https://missing.test/schema"}"#),
        )],
        &l,
    )
    .unwrap();
    assert!(matches!(
        N::compile(&missing, O::default(), &l),
        Err(E::Resources(_))
    ));
}

#[test]
fn dynamic_reference_rebinding_is_performed_by_the_native_validator() {
    let l = Limits::default();
    let tree = j(
        r##"{"$id":"https://e.test/tree","$dynamicAnchor":"node","type":"object","properties":{"data":true,"children":{"type":"array","items":{"$dynamicRef":"#node"}}}}"##,
    );
    let strict = j(
        r#"{"$id":"https://e.test/strict","$dynamicAnchor":"node","$ref":"tree","unevaluatedProperties":false}"#,
    );
    let n = N::compile(
        &C::new(
            vec![
                ("https://e.test/tree".into(), tree),
                ("https://e.test/strict".into(), strict),
            ],
            &l,
        )
        .unwrap(),
        O::default(),
        &l,
    )
    .unwrap();
    assert!(
        n.validate(
            "https://e.test/strict",
            &j(r#"{"data":1,"children":[{"data":2}]}"#),
            &l
        )
        .unwrap()
    );
    assert!(
        !n.validate(
            "https://e.test/strict",
            &j(r#"{"data":1,"children":[{"extra":2}]}"#),
            &l
        )
        .unwrap()
    );
}

#[test]
fn format_is_annotation_and_unsupported_vocabulary_or_patterns_fail_compilation() {
    let l = Limits::default();
    let n = compile(r#"{"type":"string","format":"email"}"#);
    assert!(
        n.validate("https://e.test/root", &j(r#""not an email""#), &l)
            .unwrap()
    );
    for schema in [
        r#"{"$vocabulary":{"urn:unknown":true}}"#,
        r#"{"$vocabulary":{"https://json-schema.org/draft/2020-12/vocab/format-assertion":true}}"#,
    ] {
        let c = C::new(vec![("urn:root".into(), j(schema))], &l).unwrap();
        assert!(matches!(
            N::compile(&c, O::default(), &l),
            Err(E::UnsupportedVocabulary)
        ));
    }
    let n = compile(
        r#"{"$vocabulary":{"urn:unknown":false},"custom":{"$id":"urn:ignored"},"type":"string","pattern":"^\\d+$"}"#,
    );
    assert!(
        n.validate("https://e.test/root", &j(r#""123""#), &l)
            .unwrap()
    );
    assert!(
        !n.validate("https://e.test/root", &j(r#""١٢٣""#), &l)
            .unwrap()
    );
    let c = C::new(
        vec![("urn:root".into(), j(r#"{"pattern":"^([ab]+)\\1$"}"#))],
        &l,
    )
    .unwrap();
    assert!(matches!(
        N::compile(&c, O::default(), &l),
        Err(E::InvalidSchema)
    ));
    let c = C::new(
        vec![(
            "urn:root".into(),
            j(r#"{"$defs":{"unused":{"pattern":"^([ab]+)\\1$"}}}"#),
        )],
        &l,
    )
    .unwrap();
    assert!(matches!(
        N::compile(&c, O::default(), &l),
        Err(E::InvalidSchema)
    ));
}

#[test]
fn limits_and_invalid_schema_shapes_are_explicit_errors() {
    let l = Limits::default();
    let c = C::new(
        vec![("urn:root".into(), j(r#"{"pattern":"abcdefgh"}"#))],
        &l,
    )
    .unwrap();
    assert!(matches!(
        N::compile(
            &c,
            O {
                max_pattern_bytes: 7,
                ..O::default()
            },
            &l
        ),
        Err(E::PatternLimit)
    ));
    let c = C::new(vec![("urn:root".into(), j(r#"{"minimum":"bad"}"#))], &l).unwrap();
    assert!(matches!(
        N::compile(&c, O::default(), &l),
        Err(E::InvalidSchema)
    ));
    let n = compile("true");
    assert!(matches!(
        n.validate(
            "https://e.test/root",
            &j(r#""too long""#),
            &Limits {
                max_text_bytes: 1,
                ..l
            }
        ),
        Err(E::Json(_))
    ));
    let c = C::new(vec![("urn:root".into(), j("{}"))], &Limits::default()).unwrap();
    assert!(matches!(
        N::compile(
            &c,
            O::default(),
            &Limits {
                max_document_bytes: 64,
                ..Limits::default()
            }
        ),
        Err(E::CompilationInputLimit)
    ));
    let c = C::new(
        (0..5)
            .map(|i| {
                (
                    format!("urn:{i}"),
                    j(&format!(r#"{{"$ref":"urn:{}"}}"#, (i + 1) % 5)),
                )
            })
            .collect(),
        &Limits::default(),
    )
    .unwrap();
    let tight = Limits {
        max_total_values: 20,
        ..Limits::default()
    };
    assert!(
        c.reference_closure(&["urn:0", "urn:1", "urn:2", "urn:3", "urn:4"], &tight)
            .is_ok()
    );
    assert!(matches!(
        N::compile(&c, O::default(), &tight),
        Err(E::AdmissionLimit)
    ));
}
