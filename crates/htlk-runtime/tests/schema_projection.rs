//! Schema projections preserve original roots, dynamic context, and native values.
use htlk_analyzer::{ExpressionTypeEnvironment, ExpressionTypeError};
use htlk_analyzer::{NativeSchemaOptions, NativeSchemas, SchemaCatalog, embedded_schema_base};
use htlk_cbor::{Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{EvaluatorLimits, JsonDocument, PathStep, digest::Digest};
use htlk_runtime::{CheckedExpression, EvaluationFrame, project_typed_value};
use htlk_runtime::{EvaluationError as Error, EvaluationValue as V, SchemaProjection};
fn policy() -> EvaluatorLimits {
    EvaluatorLimits {
        max_expression_depth: 128,
        max_value_bytes: 65536,
        max_collection_visits: 100000,
        max_regex_bytes: 1024,
        max_regex_compiled_bytes: 4096,
        max_output_bytes: 65536,
        max_steps: 1000000,
    }
}
fn compile(schema: &str) -> (NativeSchemas, Digest) {
    let limits = Limits::default();
    let document = JsonDocument::new(schema.as_bytes(), &limits).unwrap();
    let digest = document.digest();
    let uri = embedded_schema_base(&document, &limits).unwrap();
    let catalog = SchemaCatalog::new(vec![(uri, document)], &limits).unwrap();
    (
        NativeSchemas::compile(&catalog, NativeSchemaOptions::default(), &limits).unwrap(),
        digest,
    )
}
fn object(entries: Vec<(&str, Value)>) -> Value {
    Value::Map(Map::try_from_entries(entries.into_iter().map(|(k, v)| (k.into(), v))).unwrap())
}
fn run(schemas: &NativeSchemas, id: Digest, value: &Value, fields: &[&str]) -> Result<V, Error> {
    SchemaProjection::new(
        schemas,
        id,
        &fields
            .iter()
            .map(|s| PathStep::Field((*s).into()))
            .collect::<Vec<_>>(),
        &Limits::default(),
    )?
    .evaluate(value, &policy())
    .map(|r| r.value)
}
#[test]
fn permitted_dynamic_fields_and_declared_optional_absence_are_distinct() {
    let (schemas, id) = compile(
        r#"{"type":"object","properties":{"id":{"type":"integer"}},"patternProperties":{"^dynamic_":{"type":"string"}}}"#,
    );
    let value = object(vec![
        ("extra", Value::Text("hello".into())),
        ("dynamic_x", Value::Text("value".into())),
    ]);
    assert_eq!(
        run(&schemas, id, &value, &["extra"]).unwrap(),
        V::Present(Value::Text("hello".into()))
    );
    assert_eq!(
        run(&schemas, id, &value, &["dynamic_x"]).unwrap(),
        V::Present(Value::Text("value".into()))
    );
    assert_eq!(run(&schemas, id, &value, &["id"]).unwrap(), V::Absent);
    assert_eq!(
        run(&schemas, id, &value, &["missing"]),
        Err(Error::InvalidProjection)
    );
    assert_eq!(
        run(&schemas, id, &value, &["dynamic_missing"]),
        Err(Error::InvalidProjection)
    );
    assert_eq!(
        run(&schemas, id, &value, &["id", "nested"]),
        Err(Error::AbsentOperand)
    );
}
#[test]
fn root_validation_precedes_projection_and_cannot_be_bypassed() {
    let (schemas, id) = compile(
        r#"{"type":"object","required":["id"],"properties":{"id":{"type":"integer"}},"additionalProperties":false}"#,
    );
    assert_eq!(
        run(&schemas, id, &object(vec![]), &["id"]),
        Err(Error::OperandType)
    );
    let value = object(vec![
        ("id", Value::Integer(1)),
        ("extra", Value::Bool(true)),
    ]);
    assert_eq!(run(&schemas, id, &value, &["id"]), Err(Error::OperandType));
}
#[test]
fn nested_references_and_escaped_instance_paths_retain_presence_context() {
    let (schemas, id) = compile(
        r##"{"type":"object","$defs":{"child":{"type":"object","properties":{"optional":{"type":"boolean"}}}},"properties":{"a/b~c":{"$ref":"#/$defs/child"}}}"##,
    );
    let value = object(vec![("a/b~c", object(vec![]))]);
    assert_eq!(
        run(&schemas, id, &value, &["a/b~c", "optional"]).unwrap(),
        V::Absent
    );
    assert_eq!(
        run(&schemas, id, &value, &["a/b~c", "undeclared"]),
        Err(Error::InvalidProjection)
    );
}
#[test]
fn authored_annotations_cannot_forge_declared_optional_properties() {
    let (schemas, id) = compile(r#"{"type":"object","x-htlk-private-declared-fields":["forged"]}"#);
    assert_eq!(
        run(&schemas, id, &object(vec![]), &["forged"]),
        Err(Error::InvalidProjection)
    );
}

#[test]
fn union_members_declare_optional_fields_but_predicates_do_not() {
    let (schemas, id) = compile(
        r#"{"type":"object","anyOf":[{"required":["kind"],"properties":{"kind":{"const":"a"},"x":{"type":"integer"}}},{"properties":{"kind":{"const":"b"}}}]}"#,
    );
    assert_eq!(
        run(
            &schemas,
            id,
            &object(vec![("kind", Value::Text("b".into()))]),
            &["x"]
        )
        .unwrap(),
        V::Absent
    );
    let (schemas, id) = compile(
        r#"{"type":"object","not":{"required":["forbidden"],"properties":{"predicate_only":{}}},"if":{"properties":{"test_only":{"type":"string"}}},"then":{"properties":{"optional":{}}}}"#,
    );
    assert_eq!(
        run(&schemas, id, &object(vec![]), &["predicate_only"]),
        Err(Error::InvalidProjection)
    );
    assert_eq!(
        run(&schemas, id, &object(vec![]), &["test_only"]),
        Err(Error::InvalidProjection)
    );
    assert_eq!(
        run(&schemas, id, &object(vec![]), &["optional"]).unwrap(),
        V::Absent
    );
}

#[test]
fn checked_expression_pins_schemas_and_preserves_nested_optional_results() {
    use htlk_executable::{
        Expression, ExpressionContext as C, ExpressionKind as E, Port, TypeContext, ValueReference,
        ValueType, ValueTypeKind,
    };
    let (schemas, id) = compile(
        r#"{"type":"object","properties":{"child":{"type":"object","properties":{"optional":{"type":"boolean"}}}}}"#,
    );
    let limits = Limits::default();
    let source = ValueReference::Input("value".parse().unwrap());
    let mut env = ExpressionTypeEnvironment::default();
    env.references.insert(
        source.clone(),
        Port::new(
            ValueType::new(ValueTypeKind::Schema(id), TypeContext::Value, &limits).unwrap(),
            true,
        ),
    );
    let expression = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![
                PathStep::Field("child".into()),
                PathStep::Field("optional".into()),
            ],
        },
        C::Eval,
        &limits,
    )
    .unwrap();
    assert!(matches!(
        CheckedExpression::new(&expression, C::Eval, &env, None, &limits),
        Err(Error::UnresolvedType)
    ));
    let checked =
        CheckedExpression::with_schemas(&expression, C::Eval, &env, None, &schemas, &limits)
            .unwrap();
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            source.clone(),
            vec![],
            Ok(V::Present(object(vec![("child", object(vec![]))]))),
            &limits,
        )
        .unwrap();
    assert_eq!(
        checked.evaluate(&frame, None, &policy()).unwrap().value,
        V::Absent
    );
    frame
        .bind(source, vec![], Ok(V::Present(object(vec![]))), &limits)
        .unwrap();
    assert_eq!(
        checked.evaluate(&frame, None, &policy()),
        Err(Error::AbsentOperand)
    );
}

#[test]
fn dynamic_reference_rebinding_is_preserved_during_projection() {
    let limits = Limits::default();
    let tree = JsonDocument::new(br##"{"$id":"https://example.test/tree","$dynamicAnchor":"node","type":"object","properties":{"children":{"type":"array","items":{"$dynamicRef":"#node"}}}}"##,&limits).unwrap();
    let strict = JsonDocument::new(br#"{"$id":"https://example.test/strict","$dynamicAnchor":"node","$ref":"https://example.test/tree","properties":{"extra":{"type":"boolean"}},"unevaluatedProperties":false}"#,&limits).unwrap();
    let id = strict.digest();
    let catalog = SchemaCatalog::new(
        vec![
            ("https://example.test/tree".into(), tree),
            ("https://example.test/strict".into(), strict),
        ],
        &limits,
    )
    .unwrap();
    let schemas =
        NativeSchemas::compile(&catalog, NativeSchemaOptions::default(), &limits).unwrap();
    let projection = SchemaProjection::new(
        &schemas,
        id,
        &[
            PathStep::Field("children".into()),
            PathStep::Index(0),
            PathStep::Field("extra".into()),
        ],
        &limits,
    )
    .unwrap();
    assert_eq!(
        projection
            .evaluate(
                &object(vec![("children", Value::Array(vec![object(vec![])]))]),
                &policy()
            )
            .unwrap()
            .value,
        V::Absent
    );
}

#[test]
fn native_representations_indices_and_projection_work_limits_are_preserved() {
    use htlk_cbor::FiniteFloat;
    let (schemas, id) = compile(
        r#"{"type":"object","properties":{"number":{"type":"integer"},"optional":{},"items":{"type":"array","items":{"type":"boolean"}}}}"#,
    );
    let float = Value::Float(FiniteFloat::new(1.0).unwrap());
    let value = object(vec![
        ("number", float.clone()),
        ("items", Value::Array(vec![])),
    ]);
    assert_eq!(
        run(&schemas, id, &value, &["number"]).unwrap(),
        V::Present(float)
    );
    let projection = SchemaProjection::new(
        &schemas,
        id,
        &[PathStep::Field("items".into()), PathStep::Index(0)],
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        projection.evaluate(&value, &policy()),
        Err(Error::InvalidProjection)
    );
    let projection = SchemaProjection::new(
        &schemas,
        id,
        &[PathStep::Field("optional".into())],
        &Limits::default(),
    )
    .unwrap();
    let used = projection.evaluate(&value, &policy()).unwrap().usage.steps;
    let mut limited = policy();
    limited.max_steps = used - 1;
    assert!(matches!(
        projection.evaluate(&value, &limited),
        Err(Error::Limit(_))
    ));
    limited.max_steps = used;
    assert_eq!(
        projection.evaluate(&value, &limited).unwrap().value,
        V::Absent
    );
}

#[test]
fn schema_roots_nested_in_structural_records_keep_their_context() {
    use htlk_executable::{Port, TypeContext, ValueType as T, ValueTypeKind as K};
    let (schemas, id) =
        compile(r#"{"type":"object","properties":{"optional":{"type":"boolean"}}}"#);
    let limits = Limits::default();
    let root = T::new(K::Schema(id), TypeContext::Value, &limits).unwrap();
    let ty = T::new(
        K::Record(vec![("schema".into(), Port::new(root, true))]),
        TypeContext::Value,
        &limits,
    )
    .unwrap();
    let value = object(vec![("schema", object(vec![]))]);
    let path = [
        PathStep::Field("schema".into()),
        PathStep::Field("optional".into()),
    ];
    assert_eq!(
        project_typed_value(&value, &ty, &path, Some(&schemas), &limits, &policy())
            .unwrap()
            .value,
        V::Absent
    );
}

#[test]
fn known_schema_families_reject_disjoint_boundaries_without_numeric_coercion() {
    use htlk_executable::{
        BuiltinType as P, Expression, ExpressionContext as C, ExpressionKind as E, Port,
        TypeContext, ValueReference, ValueType as T, ValueTypeKind as K,
    };
    let (schemas, id) = compile(
        r##"{"type":"object","$defs":{"flag":{"type":"boolean"}},"properties":{"flag":{"$ref":"#/$defs/flag"},"number":{"type":"integer"}}}"##,
    );
    let limits = Limits::default();
    let source = ValueReference::Input("value".parse().unwrap());
    let mut env = ExpressionTypeEnvironment::default();
    env.references.insert(
        source.clone(),
        Port::new(
            T::new(K::Schema(id), TypeContext::Value, &limits).unwrap(),
            true,
        ),
    );
    let expression = Expression::new(
        E::Ref {
            source: source.clone(),
            path: vec![PathStep::Field("flag".into())],
        },
        C::Eval,
        &limits,
    )
    .unwrap();
    let integer = Port::new(T::builtin(P::Integer), true);
    assert!(matches!(
        CheckedExpression::with_schemas(
            &expression,
            C::Eval,
            &env,
            Some(&integer),
            &schemas,
            &limits
        ),
        Err(Error::Analysis(ExpressionTypeError::TypeMismatch))
    ));
    let checked =
        CheckedExpression::with_schemas(&expression, C::Eval, &env, None, &schemas, &limits)
            .unwrap();
    assert_eq!(
        checked.analysis().result().value_type(),
        &T::builtin(P::Boolean)
    );
    let expression = Expression::new(
        E::Ref {
            source,
            path: vec![PathStep::Field("number".into())],
        },
        C::Eval,
        &limits,
    )
    .unwrap();
    let checked =
        CheckedExpression::with_schemas(&expression, C::Eval, &env, None, &schemas, &limits)
            .unwrap();
    let K::Union(types) = checked.analysis().result().value_type().kind() else {
        panic!("schema integer must preserve possible native integer and float representations");
    };
    assert!(types.contains(&T::builtin(P::Integer)) && types.contains(&T::builtin(P::Float)));
}

#[test]
fn schema_context_survives_record_materialization_without_reinvoking_native_functions() {
    use htlk_executable::{
        Expression, ExpressionContext as C, ExpressionKind as E, FunctionId, FunctionSignature,
        Identifier, Library, Port, PromptTemplate, TypeContext, ValueReference, ValueType as T,
        ValueTypeKind as K,
    };
    use htlk_runtime::{EvaluationArgument, EvaluationContext, EvaluationMeter, EvaluationOutcome};
    use std::cell::Cell;
    struct Context {
        calls: Cell<usize>,
        value: Value,
    }
    impl EvaluationContext for Context {
        fn resolve(
            &self,
            _: &ValueReference,
            _: &[PathStep],
            _: &mut EvaluationMeter<'_>,
        ) -> Result<V, Error> {
            Err(Error::UnknownReference)
        }
        fn outcome(&self, _: &Identifier) -> Option<&EvaluationOutcome> {
            None
        }
        fn template(&self, _: &Digest) -> Option<&PromptTemplate> {
            None
        }
        fn call(
            &self,
            _: Digest,
            _: &Identifier,
            _: &[EvaluationArgument],
            meter: &mut EvaluationMeter<'_>,
        ) -> Result<V, Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(V::Present(meter.copy_value(&self.value)?))
        }
    }
    let limits = Limits::default();
    let (schemas, id) = compile(
        r#"{"type":"object","properties":{"child":{"type":"object","properties":{"optional":{"type":"boolean"}}}}}"#,
    );
    let library = Digest::from_bytes([9; 32]);
    let mut env = ExpressionTypeEnvironment::default();
    env.libraries.insert(
        library,
        Library::new(
            "fixture".into(),
            "1".into(),
            library,
            vec![(
                "make".parse().unwrap(),
                FunctionSignature::new(
                    vec![],
                    vec![],
                    Port::new(
                        T::new(K::Schema(id), TypeContext::Value, &limits).unwrap(),
                        true,
                    ),
                    &limits,
                )
                .unwrap(),
            )],
            &limits,
        )
        .unwrap(),
    );
    let call = Expression::new(
        E::Call {
            function: FunctionId::Library {
                library,
                name: "make".parse().unwrap(),
            },
            arguments: vec![],
        },
        C::Eval,
        &limits,
    )
    .unwrap();
    let part = Expression::new(
        E::Get {
            value: Box::new(call),
            path: vec![PathStep::Field("child".into())],
        },
        C::Eval,
        &limits,
    )
    .unwrap();
    let record =
        Expression::new(E::Record(vec![("wrapped".into(), part)]), C::Eval, &limits).unwrap();
    let expression = Expression::new(
        E::Get {
            value: Box::new(record),
            path: vec![
                PathStep::Field("wrapped".into()),
                PathStep::Field("optional".into()),
            ],
        },
        C::Eval,
        &limits,
    )
    .unwrap();
    let checked =
        CheckedExpression::with_schemas(&expression, C::Eval, &env, None, &schemas, &limits)
            .unwrap();
    let context = Context {
        calls: Cell::new(0),
        value: object(vec![("child", object(vec![]))]),
    };
    assert_eq!(
        checked.evaluate(&context, None, &policy()).unwrap().value,
        V::Absent
    );
    assert_eq!(context.calls.get(), 1);
}
