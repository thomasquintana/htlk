//! User-facing diagnostics preserve categories, causes, offsets, and redaction.
use htlk_executable::{
    BuiltinType, CoreFunction, Expression, ExpressionError as Error, ExpressionKind as K,
    FunctionId, Identifier, Port, PromptTemplate, ScalarLiteral, TypeContext, TypeError, ValueType,
    cbor::{self, LimitKind, Limits, Map, Value},
    digest::Digest,
};
use std::error::Error as _;

fn text(s: &str) -> Value {
    Value::Text(s.into())
}
fn array(values: Vec<Value>) -> Value {
    Value::Array(values)
}
fn template(parameters: Value, parts: Vec<Value>) -> Value {
    Value::Map(
        Map::try_from_entries([
            ("parameters".into(), parameters),
            ("parts".into(), array(parts)),
        ])
        .unwrap(),
    )
}

#[test]
fn error_categories_explain_the_failed_requirement() {
    for (error, expected) in [
        (
            Error::UnknownConstructor,
            "The expression constructor is not recognized.",
        ),
        (
            Error::UnknownFunction,
            "The function identity form or core function is not recognized.",
        ),
        (
            Error::InvalidIndex,
            "A projection index must be an integer from 0 through 9223372036854775807.",
        ),
        (
            Error::InvalidRegexFlags,
            "Regex flags may contain i, m, and s, each at most once.",
        ),
        (
            Error::DuplicateField,
            "Expression record fields, render arguments, and template parameters must have unique names.",
        ),
        (
            Error::InvalidTemplateParameter,
            "Template parameters must have type string, integer, float, or boolean.",
        ),
        (
            Error::TemplateParameterMismatch,
            "Template parameter declarations must exactly cover the distinct substitution slots.",
        ),
        (
            Error::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                maximum: 1024,
            },
            "The expression or template representation exceeds the configured maximum of 1024 bytes.",
        ),
        (
            Error::LimitExceeded {
                limit: LimitKind::Depth,
                maximum: 17,
            },
            "The expression or template representation exceeds the configured maximum nesting depth of 17.",
        ),
        (
            Error::AllocationFailed,
            "Memory could not be allocated for the expression or template representation.",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
        assert!(error.source().is_none());
    }
}

#[test]
fn schema_descriptions_explain_representation_requirements() {
    for (site, expected) in [
        (
            "identifier",
            "An expression or template identifier must be text.",
        ),
        ("digest", "An expression digest must be text."),
        (
            "constructor arity",
            "The tagged array or record field pair has an incorrect number of elements.",
        ),
        (
            "core function arity",
            "The core function requires exactly one argument.",
        ),
        (
            "nonempty get path",
            "A projection must select at least one field or index.",
        ),
        (
            "scalar literal",
            "A scalar literal must be text, an integer, a float, a boolean, null, or bytes.",
        ),
        ("path", "A projection path must be an array."),
        (
            "path step",
            "A projection path step must be a text field name or an integer index.",
        ),
        (
            "value reference",
            "A value reference must be a nonempty tagged array.",
        ),
        (
            "reference tag",
            "A value reference tag must be one of input, output, scope_output, carried, or next.",
        ),
        ("function id", "A function identity must be an array."),
        ("function tag", "A function identity tag must be text."),
        ("core function", "A core function name must be text."),
        ("expression", "An expression must be an array."),
        (
            "expression tag",
            "An expression must begin with a text constructor tag.",
        ),
        ("regex pattern", "A regex pattern must be text."),
        ("regex flags", "Regex flags must be text."),
        (
            "expression list",
            "Expression elements or arguments must be an array.",
        ),
        (
            "record expression fields",
            "Record expression fields must be an array of name-expression pairs.",
        ),
        (
            "record expression pair",
            "A record expression field must be an array containing its name and expression.",
        ),
        ("record key", "A record expression field name must be text."),
        (
            "call arguments",
            "Function call arguments must be an array.",
        ),
        (
            "render arguments",
            "Template render arguments must be a map of names to expressions.",
        ),
        ("template record", "A template record must be a map."),
        (
            "template fields",
            "A template record must contain exactly the parameters and parts fields.",
        ),
        (
            "template parameters",
            "Template parameters must be a map of names to port declarations.",
        ),
        ("template parts", "Template parts must be an array."),
        (
            "template part",
            "A template part must be text or a two-element slot array containing the slot tag and a parameter name.",
        ),
    ] {
        assert_eq!(Error::InvalidShape(site).to_string(), expected);
    }
}

#[test]
fn canonical_ingress_explains_why_it_will_not_repair_data() {
    let limits = Limits::default();
    let reference = array(vec![
        text("ref"),
        array(vec![text("input"), text("secret_name")]),
        array(vec![]),
    ]);
    let path = array(vec![text("secret_field")]);
    let literal = array(vec![text("literal"), text("secret_literal")]);
    let get = array(vec![text("get"), literal.clone(), path.clone()]);
    for (value, rule, expected) in [
        (
            array(vec![text("regex"), text("secret_pattern"), text("mi")]),
            "regex flag order",
            "Regex flags must appear in i, m, s order.",
        ),
        (
            array(vec![text("get"), reference, path.clone()]),
            "get on reference",
            "A projection of a named reference must be folded into the reference's path.",
        ),
        (
            array(vec![text("get"), get, path]),
            "nested get",
            "Nested projections must be combined into a single projection path.",
        ),
        (
            array(vec![
                text("record"),
                array(vec![
                    array(vec![text("z_secret"), literal.clone()]),
                    array(vec![text("a_secret"), literal]),
                ]),
            ]),
            "record field UTF-8 order",
            "Record fields must be sorted by the UTF-8 bytes of their decoded names.",
        ),
    ] {
        let error = Expression::from_value(&value, &limits).unwrap_err();
        assert_eq!(error, Error::NonCanonical(rule));
        assert_eq!(error.to_string(), expected);
        assert!(!format!("{error:?}").contains("secret"));
    }
    for parts in [vec![text("")], vec![text("secret_one"), text("secret_two")]] {
        let error = PromptTemplate::from_value(&template(Value::Map(Map::new()), parts), &limits)
            .unwrap_err();
        assert_eq!(error, Error::NonCanonical("template literal segments"));
        assert_eq!(
            error.to_string(),
            "Template text segments must be nonempty, and adjacent text segments must be joined."
        );
        assert!(!format!("{error:?}").contains("secret"));
    }
}

#[test]
fn public_construction_and_ingress_report_precise_redacted_failures() {
    let limits = Limits::default();
    let error = Expression::new(
        K::Call {
            function: FunctionId::Core(CoreFunction::Length),
            arguments: vec![],
        },
        &limits,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "The core function requires exactly one argument."
    );
    let error = Expression::new(
        K::Regex {
            pattern: "secret_pattern".into(),
            flags: "ii".into(),
        },
        &limits,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Regex flags may contain i, m, and s, each at most once."
    );
    assert!(!format!("{error:?}").contains("secret_pattern"));
    let error = Expression::from_value(
        &array(vec![
            text("call"),
            array(vec![text("core"), text("secret_function")]),
            array(vec![]),
        ]),
        &limits,
    )
    .unwrap_err();
    assert_eq!(error, Error::UnknownFunction);
    assert!(!error.to_string().contains("library"));
    assert!(!format!("{error:?}").contains("secret_function"));
    let error = PromptTemplate::new(
        vec![(
            "secret_parameter".parse().unwrap(),
            Port::new(ValueType::builtin(BuiltinType::Bytes), true),
        )],
        vec![],
        &limits,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Template parameters must have type string, integer, float, or boolean."
    );
    let error = PromptTemplate::from_value(&template(Value::Null, vec![]), &limits).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Template parameters must be a map of names to port declarations."
    );
}

#[test]
fn wrapped_causes_keep_source_chains_offsets_and_private_contents() {
    let limits = Limits::default();
    let raw = b"\x82\x67literal\x6esecret_literal\x00";
    let error = Expression::decode(raw, &limits).unwrap_err();
    let source = error
        .source()
        .unwrap()
        .downcast_ref::<cbor::Error>()
        .unwrap();
    assert_eq!(source.offset(), Some(raw.len() - 1));
    assert!(
        error
            .to_string()
            .starts_with("Could not process the expression or template CBOR representation: ")
    );
    assert!(!format!("{error} {error:?}").contains("secret_literal"));

    let error = Expression::from_value(
        &array(vec![text("function_ref"), text("secret_digest"), text("f")]),
        &limits,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "An expression digest is invalid: digest must start with sha256:."
    );
    assert_eq!(
        error
            .source()
            .unwrap()
            .downcast_ref::<htlk_executable::digest::ParseDigestError>(),
        Some(&"secret_digest".parse::<Digest>().unwrap_err())
    );
    let error = Expression::from_value(&array(vec![text("status"), text("SecretName")]), &limits)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("Could not process an expression or template identifier: ")
    );
    assert_eq!(
        error
            .source()
            .unwrap()
            .downcast_ref::<htlk_executable::ParseIdentifierError>(),
        Some(&"SecretName".parse::<Identifier>().unwrap_err())
    );
    assert!(!format!("{error} {error:?}").contains("SecretName"));

    let error = PromptTemplate::from_value(
        &template(
            Value::Map(Map::try_from_entries([("secret_parameter".into(), Value::Null)]).unwrap()),
            vec![],
        ),
        &limits,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Could not process a template parameter's port or type: A port must be a map containing type and required fields."
    );
    assert_eq!(
        error.source().unwrap().downcast_ref::<TypeError>(),
        Some(&TypeError::InvalidShape("port record"))
    );
    assert!(!format!("{error} {error:?}").contains("secret_parameter"));
}

#[test]
fn direct_and_wrapped_limits_retain_their_resource_and_maximum() {
    let expression = Expression::literal(ScalarLiteral::String("secret_literal".into()));
    let value = expression.to_value(&Limits::default()).unwrap();
    for (limits, resource) in [
        (
            Limits {
                max_document_bytes: 10,
                ..Limits::default()
            },
            "10 bytes",
        ),
        (
            Limits {
                max_depth: 0,
                ..Limits::default()
            },
            "depth of 0",
        ),
    ] {
        let direct = expression.to_value(&limits).unwrap_err();
        assert!(matches!(direct, Error::LimitExceeded { .. }));
        assert!(direct.to_string().contains(resource));
        let wrapped = Expression::from_value(&value, &limits).unwrap_err();
        assert!(matches!(wrapped, Error::Codec(_)));
        assert!(wrapped.to_string().contains(resource));
        assert!(wrapped.source().is_some());
    }
    // Consumers can also propagate a parameter's nested type/codec limit error.
    let port = Port::new(ValueType::builtin(BuiltinType::String), true);
    let value = port
        .to_value(TypeContext::Value, &Limits::default())
        .unwrap();
    let error = Error::from(
        Port::from_value(
            &value,
            TypeContext::Value,
            &Limits {
                max_document_bytes: 1,
                ..Limits::default()
            },
        )
        .unwrap_err(),
    );
    assert!(matches!(error, Error::Type(TypeError::Codec(_))));
    assert!(
        error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .is::<cbor::Error>()
    );
    assert!(error.to_string().contains("1 bytes"));
}

#[test]
fn consumer_supplied_descriptions_have_safe_prose_fallbacks() {
    for (error, expected) in [
        (
            Error::InvalidShape("consumer description"),
            "The expression or template representation has an invalid shape.",
        ),
        (
            Error::NonCanonical("consumer description"),
            "The expression or template representation is not in canonical form.",
        ),
        (
            Error::ForbiddenReference("consumer description"),
            "This reference category is not permitted at this expression use site.",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
        assert!(error.source().is_none());
    }
    for (kind, category) in [
        ("input", "Input references"),
        ("output", "Node output references"),
        ("scope_output", "Proposed output references"),
        ("carried", "Loop-carried value references"),
        ("next", "Proposed next-value references"),
        ("node outcome", "Node outcome inspection"),
    ] {
        let message = Error::ForbiddenReference(kind).to_string();
        assert!(message.starts_with(category));
        assert!(message.ends_with("not permitted at this expression use site."));
    }
}
