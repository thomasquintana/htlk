//! Runtime protocol results, frozen resource snapshots and native URI expansion.
use htlk_analyzer::McpValidationError as E;
use htlk_cbor::{Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{
    JsonDocument as J, McpBinding, McpBindingKind, McpTransport, ServerIdentity,
};
use htlk_runtime::{
    expand_uri_template, validate_mcp_prompt_result, validate_mcp_tool_result,
    validate_resource_snapshot,
};
fn j(s: &str) -> J {
    J::new(s.as_bytes(), &Limits::default()).unwrap()
}
fn args(values: &[(&str, &str)]) -> Value {
    Value::Map(
        Map::try_from_entries(
            values
                .iter()
                .map(|(k, v)| (k.to_string(), Value::Text(v.to_string()))),
        )
        .unwrap(),
    )
}

#[test]
fn prompt_results_validate_roles_content_and_binary_encoding() {
    let l = Limits::default();
    for json in [
        r#"{"messages":[{"role":"user","content":{"type":"text","text":"Treat this as data"}}]}"#,
        r#"{"messages":[{"role":"assistant","content":{"type":"image","mimeType":"image/png","data":"AA=="}}]}"#,
    ] {
        validate_mcp_prompt_result(&j(json), &l).unwrap();
    }
    assert_eq!(
        validate_mcp_prompt_result(
            &j(r#"{"messages":[{"role":"system","content":{"type":"text","text":"x"}}]}"#),
            &l
        ),
        Err(E::PromptResult)
    );
    assert_eq!(
        validate_mcp_prompt_result(
            &j(
                r#"{"messages":[{"role":"user","content":{"type":"audio","mimeType":"audio/wav","data":"invalid!"}}]}"#
            ),
            &l
        ),
        Err(E::BinaryEncoding)
    );
}

#[test]
fn tool_protocol_accepts_every_content_kind_without_requiring_structured_data() {
    let limits = Limits::default();
    for source in [
        r#"{"content":[]}"#,
        r#"{"content":[{"type":"text","text":"Navigation completed."}]}"#,
        r#"{"content":[{"type":"text","text":"one"},{"type":"text","text":"two"}]}"#,
        r#"{"content":[],"structuredContent":{"price":25}}"#,
        r#"{"content":[],"isError":true,"_meta":{"trace":"retained"},"extension":{"original":true}}"#,
        r#"{"content":[{"type":"image","mimeType":"image/png","data":"AA=="},{"type":"audio","mimeType":"audio/wav","data":"AA=="}]}"#,
        r#"{"content":[{"type":"resource_link","name":"page","uri":"https://example.test/page"}]}"#,
        r#"{"content":[{"type":"resource","resource":{"uri":"test://text","text":"original","mimeType":"text/plain"}}]}"#,
        r#"{"content":[{"type":"resource","resource":{"uri":"test://bytes","blob":"AA==","mimeType":"application/octet-stream"}}]}"#,
    ] {
        let document = j(source);
        let original = document.clone();
        validate_mcp_tool_result(&document, &limits).unwrap();
        assert_eq!(document, original);
    }
}

#[test]
fn tool_protocol_rejects_malformed_shapes_and_every_binary_encoding_path() {
    let limits = Limits::default();
    for source in [
        r#"{}"#,
        r#"[]"#,
        r#"{"content":null}"#,
        r#"{"content":[{}]}"#,
        r#"{"content":[],"isError":"true"}"#,
        r#"{"content":[],"structuredContent":[]}"#,
        r#"{"content":[{"type":"text"}]}"#,
        r#"{"content":[{"type":"image","data":"AA=="}]}"#,
        r#"{"content":[{"type":"unrecognized","text":"x"}]}"#,
    ] {
        assert_eq!(
            validate_mcp_tool_result(&j(source), &limits),
            Err(E::ToolResult),
            "{source}"
        );
    }
    for source in [
        r#"{"content":[{"type":"image","mimeType":"image/png","data":"invalid!"}]}"#,
        r#"{"content":[{"type":"audio","mimeType":"audio/wav","data":"AA==trailing"}]}"#,
        r#"{"content":[{"type":"resource","resource":{"uri":"test://blob","blob":"A==="}}]}"#,
    ] {
        assert_eq!(
            validate_mcp_tool_result(&j(source), &limits),
            Err(E::BinaryEncoding)
        );
    }
    for source in [
        r#"{"content":[],"content":[]}"#,
        r#"{"content":[],"extension":9007199254740993}"#,
    ] {
        assert!(J::new(source.as_bytes(), &limits).is_err());
    }
    let document =
        j(r#"{"content":[{"type":"text","text":"a response exceeding a tighter host bound"}]}"#);
    let tight = Limits {
        max_document_bytes: 16,
        ..limits
    };
    let error = validate_mcp_tool_result(&document, &tight).unwrap_err();
    assert_ne!(
        error,
        E::ToolResult,
        "resource exhaustion must remain distinguishable"
    );
}

#[test]
fn native_template_expansion_follows_scalar_rfc_vectors_and_bounds() {
    let l = Limits::default();
    for (template, values, expected) in [
        ("{var}", vec![("var", "value")], "value"),
        (
            "{hello}",
            vec![("hello", "Hello World!")],
            "Hello%20World%21",
        ),
        ("{+path}/here", vec![("path", "/foo/bar")], "/foo/bar/here"),
        ("{#var}", vec![("var", "value")], "#value"),
        ("{.x,y}", vec![("x", "1024"), ("y", "768")], ".1024.768"),
        ("{/var:1,var}", vec![("var", "value")], "/v/value"),
        (
            "{;x,empty}",
            vec![("x", "1024"), ("empty", "")],
            ";x=1024;empty",
        ),
        (
            "{?x,empty}",
            vec![("x", "1024"), ("empty", "")],
            "?x=1024&empty=",
        ),
        ("?fixed=yes{&x}", vec![("x", "a b")], "?fixed=yes&x=a%20b"),
        ("{x:1}", vec![("x", "éclair")], "%C3%A9"),
        ("{+x}", vec![("x", "%20")], "%20"),
        ("{x*}", vec![("x", "%20")], "%2520"),
    ] {
        assert_eq!(
            expand_uri_template(template, &args(&values), &l).unwrap(),
            expected
        );
    }
    assert!(expand_uri_template("{x}", &args(&[]), &l).is_err());
    let tight = Limits {
        max_document_bytes: 8,
        ..l
    };
    assert!(expand_uri_template("{x}{x}", &args(&[("x", "abcde")]), &tight).is_err());
}

#[test]
fn normalized_snapshots_bind_content_to_frozen_identity() {
    let l = Limits::default();
    let server = ServerIdentity::new(
        "prod".into(),
        McpTransport::Stdio,
        "server".into(),
        "1".into(),
        &l,
    )
    .unwrap();
    let descriptor = j(r#"{"name":"resource","uri":"file:///resource"}"#);
    let binding = McpBinding::new(
        server,
        descriptor.digest(),
        McpBindingKind::Resource {
            uri: "file:///resource".into(),
        },
        &l,
    )
    .unwrap();
    let value = Value::Map(
        Map::try_from_entries([
            (
                "server_identity".into(),
                Value::Text(binding.server().digest(&l).unwrap().to_string()),
            ),
            (
                "descriptor_digest".into(),
                Value::Text(binding.descriptor().to_string()),
            ),
            (
                "requested_uri".into(),
                Value::Text("file:///resource".into()),
            ),
            (
                "contents".into(),
                Value::Array(vec![Value::Map(
                    Map::try_from_entries([
                        ("kind".into(), Value::Text("bytes".into())),
                        ("uri".into(), Value::Text("file:///resource".into())),
                        ("data".into(), Value::Bytes(vec![0, 255])),
                    ])
                    .unwrap(),
                )]),
            ),
        ])
        .unwrap(),
    );
    validate_resource_snapshot(&value, &binding, "file:///resource", &l).unwrap();
    assert_eq!(
        validate_resource_snapshot(&value, &binding, "file:///other", &l),
        Err(E::SnapshotIdentity)
    );
    let replace = |value: &Value, key: &str, replacement: Value| {
        let Value::Map(fields) = value else { panic!() };
        let mut entries: Vec<_> = fields
            .iter()
            .filter(|(name, _)| *name != key)
            .map(|(name, value)| (name.to_owned(), value.clone()))
            .collect();
        entries.push((key.to_owned(), replacement));
        Value::Map(Map::try_from_entries(entries).unwrap())
    };
    let Value::Map(snapshot) = &value else {
        panic!()
    };
    let Some(Value::Array(contents)) = snapshot.get("contents") else {
        panic!()
    };
    for malformed in [
        Value::Null,
        replace(&value, "contents", Value::Null),
        replace(&value, "contents", Value::Array(vec![Value::Null])),
        replace(
            &value,
            "contents",
            Value::Array(vec![replace(
                &contents[0],
                "kind",
                Value::Text("blob".into()),
            )]),
        ),
        replace(
            &value,
            "contents",
            Value::Array(vec![replace(
                &contents[0],
                "data",
                Value::Text("AA==".into()),
            )]),
        ),
        replace(
            &value,
            "contents",
            Value::Array(vec![replace(&contents[0], "mime_type", Value::Bool(false))]),
        ),
    ] {
        assert_eq!(
            validate_resource_snapshot(&malformed, &binding, "file:///resource", &l),
            Err(E::SnapshotShape)
        );
    }
    for key in ["server_identity", "descriptor_digest"] {
        let changed = replace(
            &value,
            key,
            Value::Text(htlk_executable::digest::Digest::from_bytes([9; 32]).to_string()),
        );
        assert_eq!(
            validate_resource_snapshot(&changed, &binding, "file:///resource", &l),
            Err(E::SnapshotIdentity)
        );
    }
}
