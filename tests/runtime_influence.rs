//! Actual-read label/authorization tracing on real compiled expression sites.
mod support;
use htlk::{
    compiler::CompilationSuccess,
    runtime::{
        EvaluationFrame, EvaluationOutcome, EvaluationValue, VerifiedExecutable,
        failure::{FailureCause, FailureCode, RuntimeFailure},
        identity::{InvocationId, RunId, ScopeInstanceId},
        influence::{InfluenceContext, InfluenceEnvironment, ReferenceInfluence},
        records::SecurityLabels,
        verify_executable_owned,
    },
};
use htlk_analyzer::{ExpressionSite, ScopeUse};
use htlk_executable::{
    ValueReference,
    cbor::{Limits, Map, Value},
};
use std::{cell::Cell, collections::BTreeMap, sync::Arc};
use support::{catalogs, native_compiler};

fn compile(source: &str) -> (CompilationSuccess, VerifiedExecutable<'static>) {
    let (compiler, registry) = native_compiler();
    let output = compiler.compile_source(source, &catalogs()).unwrap();
    let verified = verify_executable_owned(
        &output.executable_bytes,
        Arc::new(registry),
        &Limits::default(),
    )
    .unwrap();
    (output, verified)
}
fn labels(value: &str) -> SecurityLabels {
    SecurityLabels::new(vec![value.into()], &Limits::default()).unwrap()
}
fn meta(value: &str) -> ReferenceInfluence {
    ReferenceInfluence {
        labels: labels(value),
        cause: None,
    }
}

#[test]
fn lazy_rhs_is_not_read_authorized_or_tainted_and_denial_never_becomes_false() {
    let (_, verified) = compile(
        r#"ir_version="0.1" graph test.lazy{
      inputs={flag=boolean,secret=string} outputs={value=boolean}
      nodes{work=eval(boolean,inputs.flag or length(inputs.secret)>0){inputs={flag=boolean,secret=string}}}
      edges{edge flag{from=inputs.flag to=work.inputs.flag} edge secret{from=inputs.secret to=work.inputs.secret} edge result{from=work.outputs.value to=outputs.value}}
    }"#,
    );
    let flag = ValueReference::Input("flag".parse().unwrap());
    let secret = ValueReference::Input("secret".parse().unwrap());
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            flag.clone(),
            vec![],
            Ok(EvaluationValue::Present(Value::Bool(true))),
            &Limits::default(),
        )
        .unwrap();
    frame
        .bind(
            secret.clone(),
            vec![],
            Ok(EvaluationValue::Present(Value::Text(
                "protected bytes".into(),
            ))),
            &Limits::default(),
        )
        .unwrap();
    let env = InfluenceEnvironment {
        base: labels("scope"),
        references: BTreeMap::from([
            (flag.clone(), meta("flag-control")),
            (secret, meta("secret")),
        ]),
        outcomes: BTreeMap::new(),
    };
    let reads = Cell::new(0);
    let authorize = |set: &SecurityLabels| {
        reads.set(reads.get() + 1);
        if set.as_slice().iter().any(|s| s == "secret") {
            Err(RuntimeFailure::new(FailureCode::Policy))
        } else {
            Ok(())
        }
    };
    let limits = Limits::default();
    let site = ExpressionSite::Eval("work".parse().unwrap());
    let context = InfluenceContext::new(&frame, &env, &authorize, &limits).unwrap();
    let result = context.evaluate(&verified, &ScopeUse::Root, &site).unwrap();
    assert_eq!(
        result.result.unwrap().value,
        EvaluationValue::Present(Value::Bool(true))
    );
    assert_eq!(result.influence.as_slice(), &["flag-control", "scope"]);
    assert_eq!(reads.get(), 1);
    drop(context);
    frame
        .bind(
            flag,
            vec![],
            Ok(EvaluationValue::Present(Value::Bool(false))),
            &limits,
        )
        .unwrap();
    let context = InfluenceContext::new(&frame, &env, &authorize, &limits).unwrap();
    let result = context.evaluate(&verified, &ScopeUse::Root, &site).unwrap();
    assert_eq!(result.result.err().unwrap().code(), FailureCode::Policy);
    assert_eq!(
        result.influence.as_slice(),
        &["flag-control", "scope", "secret"]
    );
}

#[test]
fn terminal_status_and_error_reads_keep_stored_error_protection_and_causes() {
    let (_, verified) = compile(
        r#"ir_version="0.1" graph test.outcomes{
      inputs={} outputs={value=boolean}
      nodes{producer=eval(boolean,true) consumer=eval(boolean,true){when=status(@producer)=="failed" and error(@producer).code=="MCP_TRANSPORT"}}
      edges{edge result{from=consumer.outputs.value to=outputs.value}}
    }"#,
    );
    let node: htlk_executable::Identifier = "producer".parse().unwrap();
    let mut frame = EvaluationFrame::default();
    frame
        .set_outcome(
            node.clone(),
            EvaluationOutcome::Failed {
                code: "MCP_TRANSPORT".into(),
                message: "Previously committed redacted text.".into(),
            },
            &Limits::default(),
        )
        .unwrap();
    let run = RunId::new("run".into(), &Limits::default()).unwrap();
    let root = ScopeInstanceId::root(&run, &Limits::default()).unwrap();
    let cause = FailureCause {
        invocation: InvocationId::new(root, &node, &Limits::default()).unwrap(),
        code: FailureCode::McpTransport,
        location: None,
    };
    let env = InfluenceEnvironment {
        base: labels("scope"),
        references: BTreeMap::new(),
        outcomes: BTreeMap::from([(
            node,
            ReferenceInfluence {
                labels: labels("decision"),
                cause: Some(cause.clone()),
            },
        )]),
    };
    let reads = Cell::new(0);
    let authorize = |_: &SecurityLabels| {
        reads.set(reads.get() + 1);
        Ok(())
    };
    let limits = Limits::default();
    let context = InfluenceContext::new(&frame, &env, &authorize, &limits).unwrap();
    let result = context
        .evaluate(
            &verified,
            &ScopeUse::Root,
            &ExpressionSite::NodeGuard("consumer".parse().unwrap()),
        )
        .unwrap();
    assert_eq!(
        result.result.unwrap().value,
        EvaluationValue::Present(Value::Bool(true))
    );
    assert_eq!(result.influence.as_slice(), &["decision", "scope"]);
    assert_eq!(result.causes, vec![cause]);
    assert_eq!(reads.get(), 1);
}

#[test]
fn projections_preserve_root_labels_and_each_evaluation_rechecks_current_policy() {
    let (_, verified) = compile(
        r#"ir_version="0.1" graph test.projection{
      inputs={data=json} outputs={value=string}
      nodes{work=eval(string,inputs.data.secret){inputs={data=json}}}
      edges{edge input{from=inputs.data to=work.inputs.data} edge output{from=work.outputs.value to=outputs.value}}
    }"#,
    );
    let reference = ValueReference::Input("data".parse().unwrap());
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            reference.clone(),
            vec![],
            Ok(EvaluationValue::Present(Value::Map(
                Map::try_from_entries([("secret".into(), Value::Text("value".into()))]).unwrap(),
            ))),
            &Limits::default(),
        )
        .unwrap();
    let env = InfluenceEnvironment {
        base: SecurityLabels::default(),
        references: BTreeMap::from([(reference, meta("protected"))]),
        outcomes: BTreeMap::new(),
    };
    let allow = Cell::new(false);
    let authorize = |_: &SecurityLabels| {
        if allow.get() {
            Ok(())
        } else {
            Err(RuntimeFailure::new(FailureCode::Policy))
        }
    };
    let limits = Limits::default();
    let context = InfluenceContext::new(&frame, &env, &authorize, &limits).unwrap();
    let site = ExpressionSite::Eval("work".parse().unwrap());
    assert_eq!(
        context
            .evaluate(&verified, &ScopeUse::Root, &site)
            .unwrap()
            .result
            .err()
            .unwrap()
            .code(),
        FailureCode::Policy
    );
    allow.set(true);
    let result = context.evaluate(&verified, &ScopeUse::Root, &site).unwrap();
    assert_eq!(
        result.result.unwrap().value,
        EvaluationValue::Present(Value::Text("value".into()))
    );
    assert_eq!(result.influence.as_slice(), &["protected"]);
}

#[test]
fn missing_actual_read_metadata_fails_closed_without_dropping_base_influence() {
    let (_, verified) = compile(
        r#"ir_version="0.1" graph test.metadata{
      inputs={value=boolean} outputs={value=boolean} nodes{work=eval(boolean,inputs.value){inputs={value=boolean}}}
      edges{edge input{from=inputs.value to=work.inputs.value} edge output{from=work.outputs.value to=outputs.value}}
    }"#,
    );
    let mut frame = EvaluationFrame::default();
    frame
        .bind(
            ValueReference::Input("value".parse().unwrap()),
            vec![],
            Ok(EvaluationValue::Present(Value::Bool(true))),
            &Limits::default(),
        )
        .unwrap();
    let env = InfluenceEnvironment {
        base: labels("scope"),
        ..InfluenceEnvironment::default()
    };
    let authorize = |_: &SecurityLabels| Ok(());
    let limits = Limits::default();
    let context = InfluenceContext::new(&frame, &env, &authorize, &limits).unwrap();
    let result = context
        .evaluate(
            &verified,
            &ScopeUse::Root,
            &ExpressionSite::Eval("work".parse().unwrap()),
        )
        .unwrap();
    assert_eq!(result.result.err().unwrap().code(), FailureCode::Invariant);
    assert_eq!(result.influence.as_slice(), &["scope"]);
}

#[test]
fn composed_contexts_preserve_native_callback_identity_arguments_and_shared_meter() {
    use htlk::runtime::{
        EvaluationArgument, EvaluationContext, EvaluationError, EvaluationMeter, evaluate,
    };
    use htlk_analyzer::{ExpressionCallType, ExpressionCallbackType};
    use htlk_executable::*;
    struct Target(Cell<u64>);
    impl EvaluationContext for Target {
        fn resolve(
            &self,
            _: &ValueReference,
            _: &[PathStep],
            _: &mut EvaluationMeter<'_>,
        ) -> Result<EvaluationValue, EvaluationError> {
            Err(EvaluationError::UnknownReference)
        }
        fn outcome(&self, _: &Identifier) -> Option<&EvaluationOutcome> {
            None
        }
        fn template(&self, _: &digest::Digest) -> Option<&PromptTemplate> {
            None
        }
        fn call(
            &self,
            library: digest::Digest,
            name: &Identifier,
            args: &[EvaluationArgument],
            meter: &mut EvaluationMeter<'_>,
        ) -> Result<EvaluationValue, EvaluationError> {
            assert_eq!(library, digest::Digest::from_bytes([23; 32]));
            assert_eq!(name.as_str(), "callback");
            assert_eq!(
                args,
                [EvaluationArgument::Value(EvaluationValue::Present(
                    Value::Integer(3)
                ))]
            );
            meter.charge(7)?;
            self.0.set(self.0.get() + 1);
            Ok(EvaluationValue::Present(Value::Integer(7)))
        }
    }
    struct Driver(Target);
    impl EvaluationContext for Driver {
        fn resolve(
            &self,
            _: &ValueReference,
            _: &[PathStep],
            _: &mut EvaluationMeter<'_>,
        ) -> Result<EvaluationValue, EvaluationError> {
            Err(EvaluationError::UnknownReference)
        }
        fn outcome(&self, _: &Identifier) -> Option<&EvaluationOutcome> {
            None
        }
        fn template(&self, _: &digest::Digest) -> Option<&PromptTemplate> {
            None
        }
        fn call(
            &self,
            _: digest::Digest,
            _: &Identifier,
            args: &[EvaluationArgument],
            meter: &mut EvaluationMeter<'_>,
        ) -> Result<EvaluationValue, EvaluationError> {
            let limits = Limits::default();
            let environment = InfluenceEnvironment::default();
            let authorize = |_: &SecurityLabels| Ok(());
            let wrapped =
                InfluenceContext::new(&self.0, &environment, &authorize, &limits).unwrap();
            let library = digest::Digest::from_bytes([23; 32]);
            let name: Identifier = "callback".parse().unwrap();
            let port = Port::new(ValueType::builtin(BuiltinType::Integer), true);
            let expression = Expression::literal(ScalarLiteral::Null);
            let boundary = ExpressionCallType {
                expression_path: vec![0],
                library,
                name: name.clone(),
                parameters: vec![port.clone()],
                returns: port.clone(),
                callbacks: vec![],
            };
            let callback = ExpressionCallbackType {
                expression_path: vec![0, 0],
                argument_index: 0,
                library,
                name: name.clone(),
                signature: ValueType::new(
                    ValueTypeKind::Function {
                        parameters: vec![port.clone()],
                        returns: Box::new(port),
                    },
                    TypeContext::Signature,
                    &limits,
                )
                .unwrap(),
            };
            let expected = EvaluationValue::Present(Value::Integer(7));
            assert_eq!(wrapped.call(library, &name, args, meter)?, expected);
            assert_eq!(
                wrapped.call_at(&expression, library, &name, args, meter)?,
                expected
            );
            assert_eq!(
                wrapped.call_typed(&expression, &boundary, args, meter)?,
                expected
            );
            assert_eq!(wrapped.call_callback(&callback, args, meter)?, expected);
            assert_eq!(
                wrapped.call_callback_typed(&callback, &boundary, args, meter)?,
                expected
            );
            Ok(expected)
        }
    }
    let (compiler, _) = native_compiler();
    let policy = &compiler.config().policy.fields().evaluator_limits;
    let expression = Expression::new(
        ExpressionKind::Call {
            function: FunctionId::Library {
                library: digest::Digest::from_bytes([1; 32]),
                name: "driver".parse().unwrap(),
            },
            arguments: vec![Expression::literal(ScalarLiteral::Integer(3))],
        },
        &Limits::default(),
    )
    .unwrap();
    let driver = Driver(Target(Cell::new(0)));
    let result = evaluate(
        &expression,
        ExpressionContext::Eval,
        &driver,
        &Limits::default(),
        policy,
    )
    .unwrap();
    assert_eq!(driver.0.0.get(), 5);
    assert!(result.usage.steps >= 35);
    assert_eq!(result.value, EvaluationValue::Present(Value::Integer(7)));
}

#[test]
fn raw_context_composition_keeps_templates_projection_and_base_metadata_bounds() {
    use htlk::runtime::{EvaluationError, evaluate};
    use htlk_executable::*;
    use std::error::Error as _;
    let (compiler, _) = native_compiler();
    let limits = Limits::default();
    let policy = &compiler.config().policy.fields().evaluator_limits;
    let mut frame = EvaluationFrame::default();
    let template =
        PromptTemplate::new(vec![], vec![TemplatePart::Text("rendered".into())], &limits).unwrap();
    let digest = frame.insert_template(template, &limits).unwrap();
    let env = InfluenceEnvironment {
        base: labels("scope"),
        references: [
            ValueReference::ScopeOutput("value".parse().unwrap()),
            ValueReference::Carried("value".parse().unwrap()),
            ValueReference::Next("value".parse().unwrap()),
            ValueReference::Output {
                node: "n".parse().unwrap(),
                port: "value".parse().unwrap(),
            },
        ]
        .into_iter()
        .map(|r| (r, meta("value")))
        .collect(),
        outcomes: BTreeMap::new(),
    };
    let authorize = |_: &SecurityLabels| Ok(());
    let context = InfluenceContext::new(&frame, &env, &authorize, &limits).unwrap();
    let render = Expression::new(
        ExpressionKind::Render {
            template: digest,
            arguments: vec![],
        },
        &limits,
    )
    .unwrap();
    assert_eq!(
        evaluate(&render, ExpressionContext::Eval, &context, &limits, policy)
            .unwrap()
            .value,
        EvaluationValue::Present(Value::Text("rendered".into()))
    );
    let get = Expression::new(
        ExpressionKind::Get {
            value: Box::new(
                Expression::new(
                    ExpressionKind::List(vec![Expression::literal(ScalarLiteral::Integer(7))]),
                    &limits,
                )
                .unwrap(),
            ),
            path: vec![PathStep::Index(0)],
        },
        &limits,
    )
    .unwrap();
    assert_eq!(
        evaluate(&get, ExpressionContext::Eval, &context, &limits, policy)
            .unwrap()
            .value,
        EvaluationValue::Present(Value::Integer(7))
    );
    assert!(
        InfluenceContext::new(
            &frame,
            &env,
            &authorize,
            &Limits {
                max_document_bytes: 1,
                ..limits.clone()
            }
        )
        .is_err()
    );
    for error in [
        htlk::runtime::influence::InfluencedError::Evaluation(EvaluationError::OperandType),
        htlk::runtime::influence::InfluencedError::Failure(RuntimeFailure::new(
            FailureCode::Policy,
        )),
    ] {
        assert!(error.source().is_some());
        assert!(!error.to_string().is_empty());
    }
}
