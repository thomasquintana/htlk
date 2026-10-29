//! Stable stage classifications and preserved typed cause chains at public boundaries.
use htlk_analyzer::*;
use htlk_executable::{
    cbor::{self, LimitKind, Limits},
    digest::Digest,
    *,
};
use std::error::Error as _;

#[test]
fn shared_regex_admission_reports_redacted_configuration_and_resource_failures() {
    for (pattern, options, expected) in [
        (
            "(?=private-pattern)",
            RegexOptions::default(),
            RegexError::Syntax,
        ),
        (
            "private-pattern",
            RegexOptions {
                max_pattern_bytes: 1,
                ..Default::default()
            },
            RegexError::PatternLimit,
        ),
        (
            "[a-z]+private-pattern",
            RegexOptions {
                max_compiled_bytes: 1,
                ..Default::default()
            },
            RegexError::CompiledLimit,
        ),
        (
            "private-pattern",
            RegexOptions {
                max_compiled_bytes: 0,
                ..Default::default()
            },
            RegexError::InvalidOptions,
        ),
    ] {
        let error = compile_regex(pattern, "", options).unwrap_err();
        assert_eq!(error, expected);
        assert!(!error.to_string().is_empty());
        assert!(!error.to_string().contains("private-pattern"));
        assert!(error.source().is_none());
    }
}

#[test]
fn linkage_conversions_preserve_document_categories_and_causal_sources() {
    let l = Limits::default();
    let cases = [
        (
            DocumentError::Codec(cbor::decode(&[], &l).unwrap_err()),
            "C_SERIALIZATION",
        ),
        (
            DocumentError::Graph(GraphRecordError::InvalidScopeRole),
            "C_LOOP",
        ),
        (
            DocumentError::Metadata(MetadataError::UnknownField("metadata")),
            "C_LIBRARY",
        ),
        (
            DocumentError::Expression(ExpressionError::UnknownConstructor),
            "C_SCOPE",
        ),
        (DocumentError::Type(TypeError::UnknownPortField), "C_TYPE"),
        (
            DocumentError::Digest("bad".parse::<Digest>().unwrap_err()),
            "C_SERIALIZATION",
        ),
        (
            DocumentError::Json(JsonDocument::new(b"{", &l).unwrap_err()),
            "C_SERIALIZATION",
        ),
    ];
    for (original, code) in cases {
        let error = LinkageError::from(original);
        assert_eq!(error.diagnostic_code(), code);
        assert!(error.source().is_some());
        assert!(!error.to_string().is_empty());
        let outer = DocumentAnalysisError::from(error.clone());
        assert_eq!(outer.source().unwrap().to_string(), error.to_string());
        assert!(!outer.to_string().is_empty());
    }
    for original in [
        DocumentError::InvalidUriTemplate { offset: 17 },
        DocumentError::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: 3,
        },
        DocumentError::AllocationFailed,
        DocumentError::UnknownField,
    ] {
        let error = LinkageError::from(original);
        assert!(!error.to_string().is_empty());
        assert_eq!(
            error.source().is_some(),
            matches!(error, LinkageError::Representation(_))
        );
    }
}

#[test]
fn every_linkage_family_has_a_stable_compiler_code_and_redacted_stage_chain() {
    let cases = [
        (LinkageError::MissingRecord("library"), "C_LIBRARY"),
        (LinkageError::MissingRecord("function"), "C_LIBRARY"),
        (LinkageError::MissingRecord("template"), "C_PROMPT"),
        (LinkageError::MissingRecord("binding"), "C_MCP"),
        (LinkageError::MissingRecord("descriptor document"), "C_MCP"),
        (LinkageError::MissingRecord("scope"), "C_NAME"),
        (LinkageError::UnreachableRecord("scope"), "C_UNUSED_NODE"),
        (LinkageError::ScopeCycle, "C_DEPENDENCY_CYCLE"),
        (LinkageError::ScopeInterfaceMismatch, "C_SCOPE"),
        (LinkageError::FunctionArity, "C_LIBRARY"),
        (LinkageError::TemplateArguments, "C_PROMPT"),
        (LinkageError::UnreachedSchemaType, "C_TYPE"),
        (
            LinkageError::Policy(PolicyError::IncompleteDefaults),
            "C_POLICY",
        ),
        (
            LinkageError::StructuralLimitExceeded {
                limit: StructuralLimit::ScopeDepth,
                maximum: 2,
            },
            "C_POLICY",
        ),
        (LinkageError::SchemaRootMismatch, "C_SCHEMA"),
        (LinkageError::ConflictingMcpSelection, "C_MCP"),
        (LinkageError::InvalidDescriptor("selection"), "C_MCP"),
        (LinkageError::InvalidUriTemplate { offset: 2 }, "C_MCP"),
        (LinkageError::ToolSchemaMismatch("input"), "C_MCP"),
        (LinkageError::McpInterfaceMismatch, "C_MCP"),
        (LinkageError::AllocationFailed, "C_SERIALIZATION"),
    ];
    for (error, code) in cases {
        assert_eq!(error.diagnostic_code(), code);
        assert!(!error.to_string().is_empty());
    }
    let schema = NativeSchemaDiagnostic {
        location: None,
        error: NativeSchemaError::InvalidSchema,
    };
    assert!(schema.source().is_some());
    assert!(!schema.to_string().is_empty());
    let errors = [
        LinkageError::SchemaDiagnostic(Box::new(schema)),
        NativeSchemaError::InvalidOptions.into(),
        SchemaResourceError::InvalidUri.into(),
        McpValidationError::Descriptor.into(),
    ];
    for error in errors {
        assert!(error.source().is_some());
        assert!(matches!(error.diagnostic_code(), "C_SCHEMA" | "C_MCP"));
    }
}

#[test]
fn backend_and_protocol_errors_preserve_each_nested_source_without_instance_data() {
    let l = Limits::default();
    for error in [
        NativeSchemaError::from(TypeError::UnknownPortField),
        NativeSchemaError::from(JsonDocument::new(b"{", &l).unwrap_err()),
        NativeSchemaError::from(SchemaResourceError::InvalidUri),
    ] {
        assert!(error.source().is_some());
        assert!(!error.to_string().is_empty());
    }
    for error in [
        NativeSchemaError::InvalidOptions,
        NativeSchemaError::UnknownRoot,
        NativeSchemaError::EngineFailure,
    ] {
        assert!(error.source().is_none());
        assert!(!error.to_string().is_empty());
    }
    for error in [
        McpValidationError::from(cbor::decode(&[], &l).unwrap_err()),
        McpValidationError::from(JsonDocument::new(b"{", &l).unwrap_err()),
        McpValidationError::from(NativeSchemaError::InvalidSchema),
        McpValidationError::from(MetadataError::UnknownField("metadata")),
    ] {
        assert!(error.source().is_some());
        assert!(!error.to_string().is_empty());
    }
    for error in [
        McpValidationError::BinaryEncoding,
        McpValidationError::BinaryLimit,
        McpValidationError::Descriptor,
        McpValidationError::PromptResult,
        McpValidationError::SnapshotShape,
        McpValidationError::SnapshotIdentity,
    ] {
        assert!(error.source().is_none());
        assert!(!error.to_string().is_empty());
    }
}
