//! Runtime identities are independent of graph versions, scheduling and storage.
use htlk_executable::{
    BuiltinType, JsonDocument, ValueType,
    cbor::{self, FiniteFloat, Limits, Value},
};
use htlk_runtime::identity::*;

#[test]
fn all_specification_domains_match_independent_known_answer_vectors() {
    let l = Limits::default();
    let run = RunId::new("example-run".into(), &l).unwrap();
    let root = ScopeInstanceId::root(&run, &l).unwrap();
    let invocation = InvocationId::new(root, &"work".parse().unwrap(), &l).unwrap();
    let content = ValueDigest::new(
        &ValueType::builtin(BuiltinType::String),
        &Value::Text("hello".into()),
        &l,
    )
    .unwrap();
    let vectors =
        JsonDocument::new(include_bytes!("fixtures/runtime-identities.json"), &l).unwrap();
    let Value::Map(vectors) = vectors.value() else {
        panic!()
    };
    for (field, actual) in [
        ("root", root.to_string()),
        ("invocation", invocation.to_string()),
        (
            "child",
            ScopeInstanceId::child(invocation, &l).unwrap().to_string(),
        ),
        (
            "iteration",
            ScopeInstanceId::iteration(invocation, 0, &l)
                .unwrap()
                .to_string(),
        ),
        (
            "attempt",
            AttemptId::new(invocation, 1, &l).unwrap().to_string(),
        ),
        (
            "input_wait",
            WaitId::input(invocation, &l).unwrap().to_string(),
        ),
        (
            "approval",
            WaitId::approval(invocation, 0, &l).unwrap().to_string(),
        ),
        (
            "approval_1",
            WaitId::approval(invocation, 1, &l).unwrap().to_string(),
        ),
        (
            "extended",
            ScopeInstanceId::extended_root(&run, 1, &l)
                .unwrap()
                .to_string(),
        ),
        ("effect", invocation.effect_key(&l).unwrap().to_string()),
        ("content", content.to_string()),
        (
            "artifact",
            ArtifactId::node_output(invocation, &"value".parse().unwrap(), content, &l)
                .unwrap()
                .to_string(),
        ),
        (
            "run_input",
            ArtifactId::run_input(&run, 0, &"seed".parse().unwrap(), content, &l)
                .unwrap()
                .to_string(),
        ),
    ] {
        assert_eq!(vectors.get(field), Some(&Value::Text(actual)), "{field}");
    }
    assert_eq!(root.to_string().parse::<ScopeInstanceId>().unwrap(), root);
    assert_eq!(
        invocation.to_string().parse::<InvocationId>().unwrap(),
        invocation
    );
    assert_eq!(content.to_string().parse::<ValueDigest>().unwrap(), content);
    assert!("SHA256:123".parse::<AttemptId>().is_err());
    assert!(
        format!("{} ", WaitId::input(invocation, &l).unwrap())
            .parse::<WaitId>()
            .is_err()
    );
}

#[test]
fn content_provenance_and_iteration_identities_remain_separate() {
    let l = Limits::default();
    let run = RunId::new("example-run".into(), &l).unwrap();
    let root = ScopeInstanceId::root(&run, &l).unwrap();
    let node = "work".parse().unwrap();
    let invocation = InvocationId::new(root, &node, &l).unwrap();
    let integer = ValueDigest::new(
        &ValueType::builtin(BuiltinType::Integer),
        &Value::Integer(1),
        &l,
    )
    .unwrap();
    let float = ValueDigest::new(
        &ValueType::builtin(BuiltinType::Float),
        &Value::Float(FiniteFloat::new(1.0).unwrap()),
        &l,
    )
    .unwrap();
    assert_ne!(integer.digest(), float.digest());
    let port = "value".parse().unwrap();
    let initial = ArtifactId::run_input(&run, 0, &port, integer, &l).unwrap();
    let extended = ArtifactId::run_input(&run, 1, &port, integer, &l).unwrap();
    let output = ArtifactId::node_output(invocation, &port, integer, &l).unwrap();
    assert_ne!(initial, extended);
    assert_ne!(initial, output);
    let iteration0 = ScopeInstanceId::iteration(invocation, 0, &l).unwrap();
    let iteration1 = ScopeInstanceId::iteration(invocation, 1, &l).unwrap();
    assert_ne!(
        InvocationId::new(iteration0, &node, &l).unwrap(),
        InvocationId::new(iteration1, &node, &l).unwrap()
    );
    assert_ne!(ScopeInstanceId::extended_root(&run, 1, &l).unwrap(), root);
    // Creating an additive root does not change the old scope or invocation.
    assert_eq!(InvocationId::new(root, &node, &l).unwrap(), invocation);
    assert_ne!(
        AttemptId::new(invocation, 1, &l).unwrap(),
        AttemptId::new(invocation, 2, &l).unwrap()
    );
    assert_ne!(
        WaitId::input(invocation, &l).unwrap(),
        WaitId::approval(invocation, 0, &l).unwrap()
    );
}

#[test]
fn preimage_limits_cover_headers_and_embedded_depth_without_changing_identity() {
    let l = Limits::default();
    let run = RunId::new("run".into(), &l).unwrap();
    let preimage = Value::Array(vec![
        Value::Text("htlk.root_scope".into()),
        Value::Text("0.1".into()),
        Value::Text("run".into()),
    ]);
    let encoded = cbor::encode(&preimage, &l).unwrap();
    let mut exact = Limits {
        max_document_bytes: encoded.len(),
        max_depth: 1,
    };
    let expected = ScopeInstanceId::root(&run, &l).unwrap();
    assert_eq!(ScopeInstanceId::root(&run, &exact).unwrap(), expected);
    exact.max_document_bytes -= 1;
    assert!(matches!(
        ScopeInstanceId::root(&run, &exact),
        Err(IdentityError::Encoding(_))
    ));
    exact.max_document_bytes += 1;
    exact.max_depth = 0;
    assert!(matches!(
        ScopeInstanceId::root(&run, &exact),
        Err(IdentityError::Encoding(_))
    ));
    let nested = Value::Array(vec![Value::Text("hello".into())]);
    let ty = ValueType::builtin(BuiltinType::Json);
    assert!(
        ValueDigest::new(
            &ty,
            &nested,
            &Limits {
                max_depth: 1,
                ..l.clone()
            }
        )
        .is_err()
    );
    assert!(ValueDigest::new(&ty, &nested, &Limits { max_depth: 2, ..l }).is_ok());
    assert!(
        RunId::new(
            "secret".into(),
            &Limits {
                max_document_bytes: 6,
                max_depth: 0
            }
        )
        .is_err()
    );
}

#[test]
fn counter_boundaries_and_opaque_names_are_exact() {
    let l = Limits::default();
    let run = RunId::new("é".into(), &l).unwrap();
    let decomposed = RunId::new("e\u{301}".into(), &l).unwrap();
    assert_ne!(
        ScopeInstanceId::root(&run, &l).unwrap(),
        ScopeInstanceId::root(&decomposed, &l).unwrap()
    );
    let root = ScopeInstanceId::root(&run, &l).unwrap();
    let invocation = InvocationId::new(root, &"work".parse().unwrap(), &l).unwrap();
    assert_eq!(
        AttemptId::new(invocation, 0, &l),
        Err(IdentityError::CounterRange)
    );
    assert_eq!(
        ScopeInstanceId::extended_root(&run, 0, &l),
        Err(IdentityError::CounterRange)
    );
    for invalid in [i64::MAX as u64 + 1, u64::MAX] {
        assert_eq!(
            AttemptId::new(invocation, invalid, &l),
            Err(IdentityError::CounterRange)
        );
        assert_eq!(
            ScopeInstanceId::iteration(invocation, invalid, &l),
            Err(IdentityError::CounterRange)
        );
        assert_eq!(
            WaitId::approval(invocation, invalid, &l),
            Err(IdentityError::CounterRange)
        );
    }
    assert!(AttemptId::new(invocation, i64::MAX as u64, &l).is_ok());
    assert!(WaitId::approval(invocation, i64::MAX as u64, &l).is_ok());
}
