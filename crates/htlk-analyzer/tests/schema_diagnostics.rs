//! Schema diagnostic locations retain original document identity without content.
use htlk_analyzer::{NativeSchemaOptions, NativeSchemas, SchemaCatalog, embedded_schema_base};
use htlk_executable::{JsonDocument, cbor::Limits};

#[test]
fn schema_diagnostics_keep_original_document_and_pointer_without_pattern_text() {
    let limits = Limits::default();
    let document=JsonDocument::new(br#"{"type":"object","properties":{"value":{"type":"string","pattern":"(?<=TOP_SECRET_PATTERN)a"}}}"#,&limits).unwrap();
    let digest = document.digest();
    let uri = embedded_schema_base(&document, &limits).unwrap();
    let catalog = SchemaCatalog::new(vec![(uri, document)], &limits).unwrap();
    let error =
        NativeSchemas::compile_diagnostic(&catalog, NativeSchemaOptions::default(), &limits)
            .err()
            .unwrap();
    let location = error.location.as_ref().unwrap();
    assert_eq!(location.document, digest);
    assert_eq!(location.pointer.to_string(), "/properties/value");
    assert!(!format!("{error:?}").contains("TOP_SECRET_PATTERN"));
}
