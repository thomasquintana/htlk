//! Canonical document representation, identities, limits and controlled-stack depth.
use CanonicalDocument as Doc;
use DocumentError as Error;
use ScopeContext as C;
use htlk_executable::{
    cbor::{LimitKind, Limits, Map, Value},
    digest::Digest,
    *,
};
fn d(n: u8) -> Digest {
    Digest::from_bytes([n; 32])
}
fn policy_fields() -> PolicyFields {
    PolicyFields {
        cost_unit: "credits".into(),
        defaults: ExecutionLimits::new()
            .with_timeout_ms(1000)
            .unwrap()
            .with_attempt_timeout_ms(100)
            .unwrap()
            .with_max_concurrency(1)
            .unwrap(),
        evaluator_limits: EvaluatorLimits {
            max_expression_depth: 64,
            max_value_bytes: 1024,
            max_collection_visits: 1000,
            max_regex_bytes: 1024,
            max_regex_compiled_bytes: 4096,
            max_output_bytes: 1024,
            max_steps: 10000,
        },
        maximum_scope_depth: 32,
        maximum_expanded_nodes: 1000,
    }
}
fn base(l: &Limits) -> DocumentFields {
    let policy = PolicyDocument::new(policy_fields(), l).unwrap();
    let engine = EngineIdentity::new("engine".into(), "1".into(), "1".into(), d(0), l).unwrap();
    let profile = ExecutionProfile::new(
        d(1),
        engine.clone(),
        engine.clone(),
        engine,
        policy.digest(),
        l,
    )
    .unwrap();
    let scope = Scope::new(ScopeFields::default(), C::Ordinary, l).unwrap();
    let mut f = DocumentFields::new(
        "test.entry".into(),
        profile,
        scope.digest(C::Ordinary, l).unwrap(),
    );
    f.scopes.insert(f.root_scope, scope);
    f.documents
        .insert(policy.digest(), policy.document().clone());
    f
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
fn policy_is_exact_jcs_with_closed_positive_fields_and_required_defaults() {
    let l = Limits::default();
    let p = PolicyDocument::new(policy_fields(), &l).unwrap();
    let golden=br#"{"cost_unit":"credits","defaults":{"attempt_timeout_ms":100,"max_concurrency":1,"timeout_ms":1000},"evaluator_limits":{"max_collection_visits":1000,"max_expression_depth":64,"max_output_bytes":1024,"max_regex_bytes":1024,"max_regex_compiled_bytes":4096,"max_steps":10000,"max_value_bytes":1024},"maximum_expanded_nodes":1000,"maximum_scope_depth":32}"#;
    assert_eq!(p.document().as_bytes(), golden);
    assert_eq!(PolicyDocument::decode(golden, &l).unwrap(), p);
    assert_eq!(p.fields(), &policy_fields());
    let mut incomplete = policy_fields();
    incomplete.defaults = ExecutionLimits::new();
    assert_eq!(
        PolicyDocument::new(incomplete, &l).unwrap_err(),
        PolicyError::IncompleteDefaults
    );
    for n in [0, 1 << 53, u64::MAX] {
        let mut bad = policy_fields();
        bad.evaluator_limits.max_steps = n;
        assert!(PolicyDocument::new(bad, &l).is_err());
    }
    let value = p.document().value();
    for bad in [
        replace(value, "cost_unit", None),
        replace(value, "extra", Some(Value::Null)),
        replace(value, "maximum_scope_depth", Some(Value::Integer(0))),
        replace(value, "maximum_scope_depth", Some(Value::Text("32".into()))),
        replace(value, "defaults", Some(Value::Map(Map::new()))),
    ] {
        assert!(
            PolicyDocument::from_document(JsonDocument::from_value(&bad, &l).unwrap(), &l).is_err()
        );
    }
}
#[test]
fn document_and_envelope_round_trip_with_strict_field_shapes() {
    let l = Limits::default();
    let doc = Doc::new(base(&l), &l).unwrap();
    assert_eq!(doc.ir_version(), "0.1");
    let bytes = doc.encode(&l).unwrap();
    assert_eq!(bytes[0], 0xaa);
    assert_eq!(Doc::decode(&bytes, &l).unwrap(), doc);
    let envelope = doc.envelope(&l).unwrap();
    assert_eq!(envelope.payload(), bytes);
    assert_eq!(
        Doc::from_envelope(
            &ExecutableEnvelope::decode(&envelope.encode(&l).unwrap(), &l).unwrap(),
            &l
        )
        .unwrap(),
        doc
    );
    assert!(Doc::from_envelope(&ExecutableEnvelope::new(vec![], &l).unwrap(), &l).is_err());
    let value = doc.to_value(&l).unwrap();
    let Value::Map(m) = &value else { panic!() };
    for (key, _) in m.iter() {
        assert!(matches!(
            Doc::from_value(&replace(&value, key, None), &l),
            Err(Error::MissingField(_))
        ));
    }
    assert_eq!(
        Doc::from_value(&replace(&value, "extra", Some(Value::Null)), &l).unwrap_err(),
        Error::UnknownField
    );
    assert_eq!(
        Doc::from_value(
            &replace(&value, "ir_version", Some(Value::Text("1".into()))),
            &l
        )
        .unwrap_err(),
        Error::UnsupportedVersion
    );
    for name in ["", "Test", "a..b", "a.", "a-b"] {
        let mut f = base(&l);
        f.graph_id = name.into();
        assert_eq!(Doc::new(f, &l).unwrap_err(), Error::InvalidGraphId);
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(Doc::decode(&trailing, &l).is_err());
}
#[test]
fn record_keys_and_canonical_json_bytes_are_assertions() {
    let l = Limits::default();
    let mut f = base(&l);
    let scope = f.scopes.remove(&f.root_scope).unwrap();
    f.root_scope = d(9);
    f.scopes.insert(d(9), scope);
    assert_eq!(Doc::new(f, &l).unwrap_err(), Error::DigestMismatch("scope"));
    let mut f = base(&l);
    f.documents
        .insert(d(9), JsonDocument::new(b"{}", &l).unwrap());
    assert_eq!(
        Doc::new(f, &l).unwrap_err(),
        Error::DigestMismatch("JSON document")
    );
    let doc = Doc::new(base(&l), &l).unwrap();
    let value = doc.to_value(&l).unwrap();
    for bytes in [
        b" {} ".to_vec(),
        b"{\"x\":1,\"x\":2}".to_vec(),
        b"null".to_vec(),
    ] {
        let docs = Value::Map(
            Map::try_from_entries([(
                doc.fields().profile.policy_document().to_string(),
                Value::Bytes(bytes),
            )])
            .unwrap(),
        );
        assert!(Doc::from_value(&replace(&value, "documents", Some(docs)), &l).is_err());
    }
}
#[test]
fn unresolved_semantic_references_remain_representable() {
    let l = Limits::default();
    let mut f = base(&l);
    f.root_scope = d(9);
    f.documents.clear();
    let doc = Doc::new(f, &l).unwrap();
    assert_eq!(Doc::decode(&doc.encode(&l).unwrap(), &l).unwrap(), doc);
}
#[test]
fn aggregate_limits_and_embedded_json_limits_are_rechecked() {
    let l = Limits::default();
    let doc = Doc::new(base(&l), &l).unwrap();
    let bytes = doc.encode(&l).unwrap();
    let exact = Limits {
        max_document_bytes: bytes.len(),
        ..l.clone()
    };
    assert_eq!(doc.encode(&exact).unwrap(), bytes);
    let small = Limits {
        max_document_bytes: bytes.len() - 1,
        ..l.clone()
    };
    assert!(matches!(
        doc.encode(&small),
        Err(Error::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            ..
        })
    ));
    assert!(Doc::decode(&bytes, &small).is_err());
    let mut f = base(&l);
    let large = JsonDocument::new(
        format!("{}0{}", "[".repeat(20), "]".repeat(20)).as_bytes(),
        &l,
    )
    .unwrap();
    f.documents.insert(large.digest(), large);
    let doc = Doc::new(f, &l).unwrap();
    let small = Limits { max_depth: 19, ..l };
    assert!(matches!(doc.encode(&small), Err(Error::Json(_))));
    assert!(matches!(
        Doc::decode(&doc.encode(&Limits::default()).unwrap(), &small),
        Err(Error::Json(_))
    ));
}
fn depth_exercise() {
    let l = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    let mut f = base(&l);
    let mut bytes = b"\x82\x63not".repeat(124);
    bytes.extend_from_slice(b"\x82\x67literal\xf5");
    let pre = Expression::decode(&bytes, ExpressionContext::Preconditions, &l).unwrap();
    f.scopes.remove(&f.root_scope);
    let scope = Scope::new(
        ScopeFields {
            preconditions: pre,
            ..ScopeFields::default()
        },
        C::Ordinary,
        &l,
    )
    .unwrap();
    f.root_scope = scope.digest(C::Ordinary, &l).unwrap();
    f.scopes.insert(f.root_scope, scope);
    let doc = Doc::new(f, &l).unwrap();
    let bytes = doc.encode(&l).unwrap();
    assert_eq!(Doc::decode(&bytes, &l).unwrap(), doc);
    assert_eq!(doc.clone(), doc);
    let smaller = Limits {
        max_depth: 127,
        ..l
    };
    assert!(doc.encode(&smaller).is_err());
    assert!(Doc::decode(&bytes, &smaller).is_err());
}
#[test]
fn document_on_controlled_stacks() {
    const CHILD: &str = "HTLK_DOCUMENT_DEPTH_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(depth_exercise)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "document_on_controlled_stacks", "--nocapture"])
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

#[test]
fn serde_preserves_complete_document_and_envelope_wire_shapes() {
    let limits = Limits::default();
    let document = Doc::new(base(&limits), &limits).unwrap();
    let expected = document.encode(&limits).unwrap();
    let mut buffer = vec![0u8; expected.len()];
    assert_eq!(cbor2::to_slice(&document, &mut buffer).unwrap(), expected);
    let envelope = document.envelope(&limits).unwrap();
    let expected = envelope.encode(&limits).unwrap();
    let mut buffer = vec![0u8; expected.len()];
    assert_eq!(cbor2::to_slice(&envelope, &mut buffer).unwrap(), expected);
    assert_eq!(
        Doc::from_envelope(
            &ExecutableEnvelope::decode(&buffer, &limits).unwrap(),
            &limits
        )
        .unwrap(),
        document
    );
}
