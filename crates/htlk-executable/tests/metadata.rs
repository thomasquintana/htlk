//! Profile, signature, library, and MCP metadata conformance.

use htlk_cbor::{LimitKind, Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::digest::{Digest, RecordKind, record_digest};
use htlk_executable::{
    CORE_VERSION, EngineIdentity as Engine, ExecutionProfile as Profile,
    FunctionSignature as Signature, Library, MCP_PROTOCOL_VERSION, McpBinding as Binding,
    McpBindingKind as Kind, McpTransport as Transport, MetadataError as Error, Port,
    PrimitiveType as P, TypeContext as C, ValueType as T, ValueTypeKind as K,
};
use std::error::Error as _;

fn d(n: u8) -> Digest {
    Digest::from_bytes([n; 32])
}
fn port(p: P) -> Port {
    Port::new(T::primitive(p), true)
}
fn engine() -> Engine {
    Engine::new("x".into(), "v".into(), "d".into(), d(0), &Limits::default()).unwrap()
}
fn server() -> ServerIdentity {
    ServerIdentity::new(
        "Prod/EU".into(),
        Transport::Stdio,
        "ToolServer".into(),
        "2.0+build".into(),
        &Limits::default(),
    )
    .unwrap()
}
use htlk_executable::ServerIdentity;
fn profile() -> Profile {
    Profile::new(d(1), engine(), engine(), engine(), d(2), &Limits::default()).unwrap()
}
fn signature() -> Signature {
    Signature::new(
        vec![],
        vec![port(P::String)],
        port(P::Boolean),
        &Limits::default(),
    )
    .unwrap()
}
fn replace(v: &Value, key: &str, replacement: Option<Value>) -> Value {
    let Value::Map(m) = v else { panic!() };
    let mut fields: Vec<_> = m
        .iter()
        .filter(|(k, _)| *k != key)
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect();
    if let Some(v) = replacement {
        fields.push((key.into(), v));
    }
    Value::Map(Map::try_from_entries(fields).unwrap())
}

#[test]
fn engine_golden_bytes_and_exact_external_strings() {
    let l = Limits::default();
    let e = engine();
    let mut expected = b"\xa4\x64name\x61x\x67version\x61v\x6cdata_version\x61d\x75implementation_digest\x78\x47sha256:".to_vec();
    expected.extend_from_slice("0".repeat(64).as_bytes());
    assert_eq!(e.encode(&l).unwrap(), expected);
    assert_eq!(Engine::decode(&expected, &l).unwrap(), e);
    assert_eq!(e.name(), "x");
    assert_eq!(e.version(), "v");
    assert_eq!(e.data_version(), "d");
    assert_eq!(e.implementation_digest(), d(0));
    let external = Engine::new(
        "Regex Engine".into(),
        "2.9+custom".into(),
        "Unicode-15.1".into(),
        d(3),
        &l,
    )
    .unwrap();
    assert_eq!(
        Engine::decode(&external.encode(&l).unwrap(), &l).unwrap(),
        external
    );
    // CDDL tstr permits empty text; interpretation/availability is a profile check.
    assert!(Engine::new("".into(), "".into(), "".into(), d(0), &l).is_ok());
}

#[test]
fn profile_versions_are_fixed_but_engine_versions_are_not_rewritten() {
    let l = Limits::default();
    let p = profile();
    assert_eq!(p.core_version(), CORE_VERSION);
    assert_eq!(CORE_VERSION, "0.1");
    assert_eq!(p.mcp_protocol_version(), MCP_PROTOCOL_VERSION);
    assert_eq!(MCP_PROTOCOL_VERSION, "2025-11-25");
    assert_eq!(p.core_digest(), d(1));
    assert_eq!(p.policy_document(), d(2));
    assert_eq!(p.regex_engine(), &engine());
    assert_eq!(p.schema_validator(), &engine());
    assert_eq!(p.uri_template_engine(), &engine());
    assert_eq!(Profile::decode(&p.encode(&l).unwrap(), &l).unwrap(), p);
    let value = p.to_value(&l).unwrap();
    assert_eq!(
        Profile::from_value(
            &replace(&value, "core_version", Some(Value::Text("0.3".into()))),
            &l
        )
        .unwrap_err(),
        Error::UnsupportedCoreVersion
    );
    assert_eq!(
        Profile::from_value(
            &replace(
                &value,
                "mcp_protocol_version",
                Some(Value::Text("latest".into()))
            ),
            &l
        )
        .unwrap_err(),
        Error::UnsupportedMcpVersion
    );
}

#[test]
fn generic_signatures_preserve_order_presence_and_binding_scope() {
    let l = Limits::default();
    let t = T::new(K::Var("t".parse().unwrap()), C::Signature, &l).unwrap();
    let list = T::new(K::List(Box::new(t.clone())), C::Signature, &l).unwrap();
    let predicate = T::new(
        K::Function {
            parameters: vec![Port::new(t.clone(), false)],
            returns: Box::new(port(P::Boolean)),
        },
        C::Signature,
        &l,
    )
    .unwrap();
    let sig = Signature::new(
        vec!["u".parse().unwrap(), "t".parse().unwrap()],
        vec![
            Port::new(list.clone(), true),
            Port::new(predicate.clone(), true),
        ],
        Port::new(list, false),
        &l,
    )
    .unwrap();
    assert_eq!(
        sig.type_parameters()
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>(),
        ["u", "t"]
    );
    assert_eq!(sig.parameters()[1].value_type(), &predicate);
    assert!(!sig.returns().required());
    assert_eq!(
        Signature::decode(&sig.encode(&l).unwrap(), &l).unwrap(),
        sig
    );
    assert_eq!(
        Signature::new(
            vec!["t".parse().unwrap(), "t".parse().unwrap()],
            vec![],
            port(P::Null),
            &l
        )
        .unwrap_err(),
        Error::DuplicateTypeParameter
    );
    assert!(Signature::new(vec![], vec![Port::new(t.clone(), true)], port(P::Null), &l).is_ok());
    assert!(Signature::new(vec![], vec![], Port::new(t, true), &l).is_ok());
    let bad = replace(
        &sig.to_value(&l).unwrap(),
        "type_parameters",
        Some(Value::Array(vec![Value::Text("u".into())])),
    );
    assert!(Signature::from_value(&bad, &l).is_ok());
    assert!(Signature::new(vec![], vec![], port(P::Null), &l).is_ok());
}

#[test]
fn library_maps_keep_all_functions_and_the_supplied_implementation_identity() {
    let l = Limits::default();
    let sig = signature();
    let functions = vec![
        ("z".parse().unwrap(), sig.clone()),
        ("aa".parse().unwrap(), sig.clone()),
    ];
    let a = Library::new("helpers".into(), "0.1".into(), d(9), functions.clone(), &l).unwrap();
    let b = Library::new(
        "helpers".into(),
        "0.1".into(),
        d(9),
        functions.into_iter().rev().collect(),
        &l,
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.library_id(), "helpers");
    assert_eq!(a.version(), "0.1");
    assert_eq!(a.implementation_digest(), d(9));
    assert_eq!(
        a.functions()
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        ["aa", "z"]
    );
    assert_eq!(a.function("z"), Some(&sig));
    assert!(a.function("missing").is_none());
    assert_eq!(Library::decode(&a.encode(&l).unwrap(), &l).unwrap(), a);
    assert!(
        Library::new(
            "helpers".into(),
            "0.1".into(),
            d(9),
            vec![
                ("x".parse().unwrap(), sig.clone()),
                ("x".parse().unwrap(), sig)
            ],
            &l
        )
        .is_err()
    );
    // Manifest completeness/identity is checked against the registry later.
    let empty = Library::new("helpers".into(), "0.1".into(), d(9), vec![], &l).unwrap();
    assert_eq!(empty.implementation_digest(), a.implementation_digest());
    assert_ne!(empty, a);
}

#[test]
fn server_identity_is_exact_closed_and_content_addressed() {
    let l = Limits::default();
    let s = server();
    assert_eq!(s.deployment_id(), "Prod/EU");
    assert_eq!(s.transport(), Transport::Stdio);
    assert_eq!(s.implementation_name(), "ToolServer");
    assert_eq!(s.implementation_version(), "2.0+build");
    assert_eq!(
        ServerIdentity::decode(&s.encode(&l).unwrap(), &l).unwrap(),
        s
    );
    assert_eq!(
        s.digest(&l).unwrap(),
        record_digest(RecordKind::Server, &s.to_value(&l).unwrap(), &l).unwrap()
    );
    let http = ServerIdentity::new(
        "Prod/EU".into(),
        Transport::StreamableHttp,
        "ToolServer".into(),
        "2.0+build".into(),
        &l,
    )
    .unwrap();
    assert_eq!(http.transport().as_str(), "streamable_http");
    assert_ne!(s.digest(&l).unwrap(), http.digest(&l).unwrap());
    let value = s.to_value(&l).unwrap();
    for alias in ["http", "streamable-http", "STDIO"] {
        assert_eq!(
            ServerIdentity::from_value(
                &replace(&value, "transport", Some(Value::Text(alias.into()))),
                &l
            )
            .unwrap_err(),
            Error::UnsupportedTransport
        );
    }
    for forbidden in ["endpoint", "credentials", "alias", "observed_at"] {
        assert_eq!(
            ServerIdentity::from_value(
                &replace(&value, forbidden, Some(Value::Text("private-input".into()))),
                &l
            )
            .unwrap_err(),
            Error::UnknownField("server identity")
        );
    }
}

#[test]
fn all_mcp_binding_variants_preserve_kind_specific_fields() {
    let l = Limits::default();
    let kinds = [
        Kind::Tool {
            name: "SearchWeb".into(),
            input_schema: d(1),
            output_schema: d(2),
        },
        Kind::Resource {
            uri: "Docs://Host/Path%2fA".into(),
        },
        Kind::Template {
            uri_template: "Docs://Host/{DocumentId}".into(),
        },
        Kind::Prompt {
            name: "Draft.Answer".into(),
        },
    ];
    for kind in kinds {
        let b = Binding::new(server(), d(3), kind.clone(), &l).unwrap();
        assert_eq!(b.kind(), &kind);
        assert_eq!(b.server(), &server());
        assert_eq!(b.descriptor(), d(3));
        let value = b.to_value(&l).unwrap();
        assert_eq!(Binding::decode(&b.encode(&l).unwrap(), &l).unwrap(), b);
        assert_eq!(
            b.digest(&l).unwrap(),
            record_digest(RecordKind::Binding, &value, &l).unwrap()
        );
        assert_eq!(
            Binding::from_value(&replace(&value, "extra", Some(Value::Null)), &l).unwrap_err(),
            Error::UnknownField("MCP binding")
        );
        let mut other = value.clone();
        other = replace(&other, "kind", Some(Value::Text("call".into())));
        assert_eq!(
            Binding::from_value(&other, &l).unwrap_err(),
            Error::UnknownBindingKind
        );
    }
    let resource = Binding::new(server(), d(3), Kind::Resource { uri: "x".into() }, &l)
        .unwrap()
        .to_value(&l)
        .unwrap();
    assert_eq!(
        Binding::from_value(
            &replace(&resource, "name", Some(Value::Text("x".into()))),
            &l
        )
        .unwrap_err(),
        Error::UnknownField("MCP binding")
    );
}

#[test]
fn metadata_records_are_closed_and_never_default_missing_fields() {
    let l = Limits::default();
    let e = engine().to_value(&l).unwrap();
    for key in ["name", "version", "data_version", "implementation_digest"] {
        assert_eq!(
            Engine::from_value(&replace(&e, key, None), &l).unwrap_err(),
            Error::MissingField(key)
        );
        assert!(Engine::from_value(&replace(&e, key, Some(Value::Null)), &l).is_err());
    }
    let p = profile().to_value(&l).unwrap();
    for key in [
        "core_version",
        "core_digest",
        "mcp_protocol_version",
        "regex_engine",
        "schema_validator",
        "uri_template_engine",
        "policy_document",
    ] {
        assert_eq!(
            Profile::from_value(&replace(&p, key, None), &l).unwrap_err(),
            Error::MissingField(key)
        );
    }
    let sig = signature().to_value(&l).unwrap();
    for key in ["parameters", "returns", "type_parameters"] {
        assert_eq!(
            Signature::from_value(&replace(&sig, key, None), &l).unwrap_err(),
            Error::MissingField(key)
        );
    }
    let lib = Library::new("helpers".into(), "0.1".into(), d(0), vec![], &l)
        .unwrap()
        .to_value(&l)
        .unwrap();
    for key in [
        "library_id",
        "version",
        "implementation_digest",
        "functions",
    ] {
        assert_eq!(
            Library::from_value(&replace(&lib, key, None), &l).unwrap_err(),
            Error::MissingField(key)
        );
    }
    let invalid_functions =
        Value::Map(Map::try_from_entries([("BadName".into(), sig.clone())]).unwrap());
    assert!(matches!(
        Library::from_value(&replace(&lib, "functions", Some(invalid_functions)), &l),
        Err(Error::Identifier(_))
    ));
    let s = server().to_value(&l).unwrap();
    for key in [
        "deployment_id",
        "transport",
        "implementation_name",
        "implementation_version",
    ] {
        assert_eq!(
            ServerIdentity::from_value(&replace(&s, key, None), &l).unwrap_err(),
            Error::MissingField(key)
        );
    }
    let b = Binding::new(
        server(),
        d(0),
        Kind::Tool {
            name: "x".into(),
            input_schema: d(1),
            output_schema: d(2),
        },
        &l,
    )
    .unwrap()
    .to_value(&l)
    .unwrap();
    for key in [
        "kind",
        "descriptor",
        "server",
        "name",
        "input_schema",
        "output_schema",
    ] {
        assert_eq!(
            Binding::from_value(&replace(&b, key, None), &l).unwrap_err(),
            Error::MissingField(key)
        );
    }
    assert!(Engine::from_value(&Value::Array(vec![]), &l).is_err());
    let error = Engine::from_value(
        &replace(
            &e,
            "implementation_digest",
            Some(Value::Text("private-input".into())),
        ),
        &l,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Digest(_)));
    assert!(error.source().is_some());
    assert!(!error.to_string().contains("private-input"));
    assert!(!format!("{error:?}").contains("private-input"));
}

#[test]
fn conversion_limits_and_codec_failures_remain_bounded() {
    let e = engine();
    let exact = Limits {
        max_document_bytes: 128,
        max_depth: 1,
    };
    let bytes = e.encode(&exact).unwrap();
    assert_eq!(bytes.len(), 128);
    assert_eq!(Engine::decode(&bytes, &exact).unwrap(), e);
    type Case = (LimitKind, usize, fn(&mut Limits));
    let cases: [Case; 2] = [
        (LimitKind::DocumentBytes, 127, |l| {
            l.max_document_bytes = 127
        }),
        (LimitKind::Depth, 0, |l| l.max_depth = 0),
    ];
    for (limit, maximum, adjust) in cases {
        let mut l = exact.clone();
        adjust(&mut l);
        assert_eq!(
            e.to_value(&l).unwrap_err(),
            Error::LimitExceeded { limit, maximum }
        );
        assert!(matches!(Engine::decode(&bytes, &l), Err(Error::Codec(_))));
    }
    for end in 0..bytes.len() {
        let error = Engine::decode(&bytes[..end], &exact).unwrap_err();
        let Error::Codec(ref cbor) = error else {
            panic!()
        };
        assert_eq!(cbor.offset(), Some(end));
        assert!(error.source().is_some());
    }
    let invalid = Limits {
        max_depth: 129,
        ..Limits::default()
    };
    assert!(matches!(e.to_value(&invalid), Err(Error::Codec(_))));
    let p = profile();
    let wire = p.encode(&Limits::default()).unwrap();
    let exact = Limits {
        max_document_bytes: wire.len(),
        ..Limits::default()
    };
    assert_eq!(p.encode(&exact).unwrap(), wire);
    assert!(matches!(
        p.to_value(&Limits {
            max_document_bytes: wire.len() - 1,
            ..exact
        }),
        Err(Error::LimitExceeded { .. })
    ));
    assert!(matches!(
        p.to_value(&Limits {
            max_depth: 1,
            ..Limits::default()
        }),
        Err(Error::LimitExceeded {
            limit: LimitKind::Depth,
            ..
        })
    ));
}
