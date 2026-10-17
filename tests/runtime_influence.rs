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
