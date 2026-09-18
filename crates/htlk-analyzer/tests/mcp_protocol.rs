//! Offline descriptor schemas and pinned snapshot provenance.
use htlk_analyzer::{McpDescriptorKind as K, McpValidationError as E, validate_mcp_descriptor};
use htlk_executable::{JsonDocument as J, cbor::Limits};
fn j(s: &str) -> J {
    J::new(s.as_bytes(), &Limits::default()).unwrap()
}

#[test]
fn complete_descriptor_schemas_validate_optional_metadata() {
    let l = Limits::default();
    for (kind, json) in [
        (
            K::Tool,
            r#"{"name":"lookup","inputSchema":{"type":"object"},"outputSchema":{"type":"object"},"annotations":{"readOnlyHint":true},"execution":{"taskSupport":"optional"}}"#,
        ),
        (
            K::Resource,
            r#"{"name":"resource","uri":"file:///resource","annotations":{"priority":0.5,"audience":["user"]}}"#,
        ),
        (
            K::ResourceTemplate,
            r#"{"name":"template","uriTemplate":"https://e.test/{x}","icons":[{"src":"https://e.test/icon","theme":"dark"}]}"#,
        ),
        (
            K::Prompt,
            r#"{"name":"prompt","arguments":[{"name":"x","required":true}],"_meta":{"extension":true}}"#,
        ),
    ] {
        validate_mcp_descriptor(kind, &j(json), &l).unwrap();
    }
    for (kind, json) in [
        (
            K::Tool,
            r#"{"name":"lookup","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":1}}"#,
        ),
        (
            K::Resource,
            r#"{"name":"resource","uri":"file:///resource","annotations":{"priority":2}}"#,
        ),
        (
            K::ResourceTemplate,
            r#"{"name":"template","uriTemplate":"{x}","icons":[{"src":"https://e.test/icon","theme":"other"}]}"#,
        ),
        (K::Prompt, r#"{"name":"prompt","description":null}"#),
    ] {
        assert_eq!(
            validate_mcp_descriptor(kind, &j(json), &l),
            Err(E::Descriptor)
        );
    }
}
#[test]
fn pinned_protocol_snapshot_matches_its_provenance() {
    let provenance: serde_json::Value =
        serde_json::from_slice(include_bytes!("../assets/mcp-source.json")).unwrap();
    assert_eq!(
        provenance["protocol"],
        htlk_executable::MCP_PROTOCOL_VERSION
    );
    let digest =
        htlk_executable::digest::hash_bytes(include_bytes!("../assets/mcp-2025-11-25.schema.json"))
            .to_string();
    assert_eq!(provenance["sha256"], &digest[7..]);
}
