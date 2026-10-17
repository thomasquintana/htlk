//! Prose, privacy, and structured causes of graph diagnostics.
use htlk_executable::{
    GraphRecordError as E,
    cbor::{self, LimitKind, Limits, Map, Value},
    digest::Digest,
    *,
};
use std::error::Error as _;

#[test]
fn fixed_descriptions_and_public_fallbacks() {
    type DescriptionCases = (
        fn(&'static str) -> E,
        &'static [(&'static str, &'static str)],
    );
    let groups: &[DescriptionCases] = &[
        (
            E::InvalidShape,
            &[
                ("identifier", "A graph identifier must be text."),
                ("digest", "A graph digest must be text."),
                (
                    "constructor arity",
                    "The tagged graph array has an incorrect number of elements.",
                ),
                (
                    "port table",
                    "Port declarations must be a map of names to ports.",
                ),
                ("source", "An edge source must be a nonempty tagged array."),
                (
                    "destination",
                    "An edge destination must be a nonempty tagged array.",
                ),
                (
                    "source tag",
                    "An edge source tag must be input, output, or carried.",
                ),
                (
                    "destination tag",
                    "An edge destination tag must be output, input, or next.",
                ),
                ("operation", "A node operation must be an array."),
                (
                    "operation tag",
                    "A node operation must begin with a text operation tag.",
                ),
                ("wait topic", "A wait topic must be text."),
                (
                    "loop initializers",
                    "Loop initializers must be a map from carried value names to input names.",
                ),
                ("edge", "An edge record must be a map."),
                ("node", "A node record must be a map."),
                ("scope", "A scope record must be a map."),
                ("nodes", "Node declarations must be an array."),
                ("edges", "Edge declarations must be an array."),
                ("consumer secret", "The graph record has an invalid shape."),
            ],
        ),
        (
            E::UnknownField,
            &[
                (
                    "edge",
                    "The edge record contains a field that is not permitted.",
                ),
                (
                    "node",
                    "The node record contains a field that is not permitted.",
                ),
                (
                    "scope",
                    "The scope record contains a field that is not permitted.",
                ),
                (
                    "consumer secret",
                    "The graph record contains a field that is not permitted.",
                ),
            ],
        ),
        (
            E::InvalidBound,
            &[
                (
                    "timeout_ms",
                    "The wait timeout must be an integer from 1 through 9223372036854775807 milliseconds.",
                ),
                (
                    "max_iterations",
                    "The loop iteration maximum must be an integer from 1 through 9223372036854775807.",
                ),
                (
                    "consumer secret",
                    "The graph bound must be an integer from 1 through 9223372036854775807.",
                ),
            ],
        ),
        (
            E::DuplicateId,
            &[
                ("node", "Node declarations must have unique local IDs."),
                ("edge", "Edge declarations must have unique local IDs."),
                (
                    "consumer secret",
                    "Graph declarations must have unique local IDs within their category.",
                ),
            ],
        ),
        (
            E::NonCanonicalOrder,
            &[
                (
                    "node",
                    "Node declarations must be sorted by their local IDs.",
                ),
                (
                    "edge",
                    "Edge declarations must be sorted by their local IDs.",
                ),
                (
                    "consumer secret",
                    "Graph declarations must be sorted by their local IDs.",
                ),
            ],
        ),
        (
            E::UnknownEndpoint,
            &[
                (
                    "source node",
                    "The edge refers to an undeclared source node.",
                ),
                (
                    "destination node",
                    "The edge refers to an undeclared destination node.",
                ),
                (
                    "source output",
                    "The edge refers to an undeclared output on its source node.",
                ),
                (
                    "destination input",
                    "The edge refers to an undeclared input on its destination node.",
                ),
                (
                    "scope input",
                    "The edge refers to an undeclared scope input.",
                ),
                (
                    "scope output",
                    "The edge refers to an undeclared scope output.",
                ),
                (
                    "carried source",
                    "The edge source refers to an undeclared loop-carried value.",
                ),
                (
                    "next destination",
                    "The edge's next-value destination refers to an undeclared loop-carried value.",
                ),
                (
                    "loop initializer input",
                    "A loop initializer refers to an undeclared input on the loop node.",
                ),
                (
                    "consumer secret",
                    "A graph reference does not identify a declared local endpoint.",
                ),
            ],
        ),
        (
            E::InvalidPorts,
            &[
                (
                    "primitive value output",
                    "An Eval, MCP, or wait node must have exactly one output named value; MCP and wait nodes must declare it as required.",
                ),
                (
                    "wait request input",
                    "A wait node must have exactly one input named request, declared as required JSON.",
                ),
                (
                    "MCP arguments input",
                    "An MCP node must have no inputs or exactly one required input named arguments.",
                ),
                (
                    "consumer secret",
                    "The node's ports do not match its operation's requirements.",
                ),
            ],
        ),
        (
            E::MissingField,
            &[(
                "consumer secret",
                "The graph record is missing a required field.",
            )],
        ),
    ];
    for (constructor, cases) in groups {
        for (description, expected) in *cases {
            let error = constructor(description);
            assert_eq!(error.to_string(), *expected);
            assert!(error.source().is_none());
        }
    }
    for field in [
        "destination",
        "guard",
        "id",
        "source",
        "inputs",
        "limits",
        "operation",
        "outputs",
        "postconditions",
        "preconditions",
        "carried",
        "edges",
        "nodes",
    ] {
        assert_eq!(
            E::MissingField(field).to_string(),
            format!("The graph record is missing the required {field} field.")
        );
    }
    // Generic Display does not erase the consumer's structured payload.
    assert!(format!("{:?}", E::InvalidShape("consumer secret")).contains("consumer secret"));
}

#[test]
fn unit_and_resource_categories() {
    for (error, expected) in [
        (
            E::UnknownOperation,
            "The operation tag is missing or is not a recognized canonical operation tag.",
        ),
        (
            E::ReservedNodeName,
            "A node name must not use a reserved expression root or function name.",
        ),
        (
            E::InvalidScopeRole,
            "The scope's declarations, endpoints, limits, or contracts are incompatible with its use role.",
        ),
        (
            E::AllocationFailed,
            "Memory could not be allocated for the graph record.",
        ),
        (
            E::LimitExceeded {
                limit: LimitKind::DocumentBytes,
                maximum: 1024,
            },
            "The graph record exceeds the configured maximum of 1024 bytes.",
        ),
        (
            E::LimitExceeded {
                limit: LimitKind::Depth,
                maximum: 7,
            },
            "The graph record exceeds the configured maximum nesting depth of 7.",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
        assert!(error.source().is_none());
    }
}

fn private(error: &E) {
    assert!(!format!("{error} {error:?}").contains("secret"));
}

#[test]
fn public_failures_attach_prose_without_submitted_contents() {
    let l = Limits::default();
    let outputs = |required| {
        PortTable::new(
            vec![(
                "value".parse().unwrap(),
                Port::new(ValueType::builtin(BuiltinType::Json), required),
            )],
            &l,
        )
        .unwrap()
    };
    for operation in [
        Operation::Eval(Expression::literal(ScalarLiteral::String(
            "secret_payload".into(),
        ))),
        Operation::Mcp {
            binding: Digest::from_bytes([0; 32]),
            retry: RetryPolicy::default(),
        },
        Operation::Wait {
            topic: "secret_topic".into(),
            timeout_ms: 1,
        },
    ] {
        let mut fields = NodeFields::new("secret_node".parse().unwrap(), operation);
        let error = Node::new(fields.clone(), &l).unwrap_err();
        assert_eq!(error, E::InvalidPorts("primitive value output"));
        assert!(error.to_string().contains("exactly one output named value"));
        private(&error);
        fields.outputs = outputs(false);
        if matches!(fields.operation, Operation::Eval(_)) {
            assert!(Node::new(fields, &l).is_ok());
            continue;
        }
        assert_eq!(
            Node::new(fields.clone(), &l).unwrap_err(),
            E::InvalidPorts("primitive value output")
        );
        fields.outputs = outputs(true);
        fields.inputs = PortTable::new(
            vec![(
                "secret_input".parse().unwrap(),
                Port::new(ValueType::builtin(BuiltinType::Json), true),
            )],
            &l,
        )
        .unwrap();
        let is_wait = matches!(fields.operation, Operation::Wait { .. });
        let error = Node::new(fields, &l).unwrap_err();
        let (category, message) = if is_wait {
            (
                "wait request input",
                "A wait node must have exactly one input named request, declared as required JSON.",
            )
        } else {
            (
                "MCP arguments input",
                "An MCP node must have no inputs or exactly one required input named arguments.",
            )
        };
        assert_eq!(error, E::InvalidPorts(category));
        assert_eq!(error.to_string(), message);
        private(&error);
    }
    let error = Operation::Wait {
        topic: "secret_topic".into(),
        timeout_ms: 0,
    }
    .encode(&l)
    .unwrap_err();
    assert_eq!(error, E::InvalidBound("timeout_ms"));
    assert!(error.to_string().contains("milliseconds"));
    private(&error);
    let error = Node::from_value(
        &Value::Map(
            Map::try_from_entries([("secret_field".into(), Value::Text("secret_payload".into()))])
                .unwrap(),
        ),
        &l,
    )
    .unwrap_err();
    assert_eq!(error, E::UnknownField("node"));
    assert_eq!(
        error.to_string(),
        "The node record contains a field that is not permitted."
    );
    private(&error);
    let error = Operation::from_value(
        &Value::Array(vec![Value::Text("secret_operation".into())]),
        &l,
    )
    .unwrap_err();
    assert_eq!(error, E::UnknownOperation);
    private(&error);
}

#[test]
fn wrapped_categories_preserve_causes_offsets_and_privacy() {
    let l = Limits::default();
    let mut bytes = Operation::Wait {
        topic: "secret_topic".into(),
        timeout_ms: 1,
    }
    .encode(&l)
    .unwrap();
    let offset = bytes.len();
    bytes.push(0);
    let codec = cbor::decode(&bytes, &l).unwrap_err();
    let error = Operation::decode(&bytes, &l).unwrap_err();
    assert_eq!(error, E::from(codec.clone()));
    assert_eq!(
        error.source().unwrap().downcast_ref::<cbor::Error>(),
        Some(&codec)
    );
    assert_eq!(codec.offset(), Some(offset));
    assert!(
        error
            .to_string()
            .starts_with("Could not process the graph record's CBOR representation: ")
    );
    private(&error);

    let error = PortTable::from_value(
        &Value::Map(Map::try_from_entries([("secret_port".into(), Value::Null)]).unwrap()),
        &l,
    )
    .unwrap_err();
    assert_eq!(error, E::Type(TypeError::InvalidShape("port record")));
    assert_eq!(
        error.source().unwrap().downcast_ref::<TypeError>(),
        Some(&TypeError::InvalidShape("port record"))
    );
    private(&error);
    let error = Operation::from_value(
        &Value::Array(vec![
            Value::Text("eval".into()),
            Value::Array(vec![
                Value::Text("regex".into()),
                Value::Text("secret_pattern".into()),
                Value::Text("ii".into()),
            ]),
        ]),
        &l,
    )
    .unwrap_err();
    assert_eq!(error, E::Expression(ExpressionError::InvalidRegexFlags));
    assert_eq!(
        error.source().unwrap().downcast_ref::<ExpressionError>(),
        Some(&ExpressionError::InvalidRegexFlags)
    );
    private(&error);

    let identifier = "SecretName".parse::<Identifier>().unwrap_err();
    let digest = "secret_digest".parse::<Digest>().unwrap_err();
    for (error, prefix) in [
        (
            E::from(identifier),
            "Could not process a graph identifier: ",
        ),
        (E::from(digest), "A graph digest is invalid: "),
        (
            E::from(TypeError::InvalidShape("port record")),
            "Could not process a graph port or type: ",
        ),
        (
            E::from(ExpressionError::InvalidRegexFlags),
            "Could not process a graph expression: ",
        ),
        (
            E::from(ExecutionOptionsError::OutOfRange("timeout_ms")),
            "Could not process the graph's execution limits or retry policy.",
        ),
    ] {
        assert!(error.source().is_some());
        assert!(error.to_string().starts_with(prefix));
        assert!(!error.to_string().contains(".."));
        assert!(!error.to_string().contains("i64"));
        private(&error);
    }
    let nested = E::from(TypeError::from(codec.clone()));
    assert_eq!(
        nested
            .source()
            .unwrap()
            .source()
            .unwrap()
            .downcast_ref::<cbor::Error>(),
        Some(&codec)
    );
    let nested = E::from(ExecutionOptionsError::from(codec.clone()));
    assert_eq!(
        nested
            .source()
            .unwrap()
            .source()
            .unwrap()
            .downcast_ref::<cbor::Error>(),
        Some(&codec)
    );
    for (limit, resource) in [
        (LimitKind::DocumentBytes, "1024 bytes"),
        (LimitKind::Depth, "depth of 1024"),
    ] {
        let error = E::from(ExecutionOptionsError::LimitExceeded {
            limit,
            maximum: 1024,
        });
        assert!(error.to_string().contains(resource));
        assert!(error.source().unwrap().is::<ExecutionOptionsError>());
    }
}

#[test]
fn public_resource_failures_keep_direct_and_codec_categories() {
    let scope = Scope::new(ScopeFields::default(), &Limits::default()).unwrap();
    let bytes = scope.encode(&Limits::default()).unwrap();
    for (limits, kind, maximum, phrase) in [
        (
            Limits {
                max_document_bytes: 97,
                ..Limits::default()
            },
            LimitKind::DocumentBytes,
            97,
            "97 bytes",
        ),
        (
            Limits {
                max_depth: 1,
                ..Limits::default()
            },
            LimitKind::Depth,
            1,
            "depth of 1",
        ),
    ] {
        let direct = scope.to_value(&limits).unwrap_err();
        assert_eq!(
            direct,
            E::LimitExceeded {
                limit: kind,
                maximum
            }
        );
        assert!(direct.to_string().contains(phrase));
        let codec = Scope::decode(&bytes, &limits).unwrap_err();
        assert!(matches!(codec, E::Codec(_)));
        assert!(codec.to_string().contains(phrase));
        assert!(codec.source().unwrap().is::<cbor::Error>());
    }
}
