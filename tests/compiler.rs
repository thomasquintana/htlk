//! Compiler output interoperability with actual native profile/admission.
use htlk::{
    compiler::*,
    runtime::{
        EvaluationFrame, EvaluationValue, ExecutableVerificationError, NativeFunction,
        NativeRegistry, NativeRegistryError, VerifiedExecutable, verify_executable,
        verify_executable_owned,
    },
};
use htlk_executable::cbor::Limits;
use htlk_executable::*;
use std::{collections::BTreeMap, sync::Arc};

fn native_compiler() -> (Compiler, NativeRegistry) {
    let l = Limits::default();
    let policy = PolicyDocument::new(
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
                max_value_bytes: 65536,
                max_collection_visits: 10000,
                max_regex_bytes: 4096,
                max_regex_compiled_bytes: 65536,
                max_output_bytes: 65536,
                max_steps: 100000,
            },
            maximum_scope_depth: 32,
            maximum_expanded_nodes: 1000,
        },
        &l,
    )
    .unwrap();
    let mut registry = NativeRegistry::new(&l).unwrap();
    let profile = registry.link_policy(&policy).unwrap();
    let compiler = Compiler::new(CompilerConfig {
        profile,
        policy,
        libraries: BTreeMap::new(),
        limits: l.clone(),
        frontend: FrontendLimits::default(),
        schema_options: analyzer::NativeSchemaOptions::default(),
        capabilities: CompilerCapabilities::default(),
    })
    .unwrap();
    (compiler, registry)
}

fn catalogs() -> MCPCatalogInputs {
    MCPCatalogInputs::from_json(
        include_bytes!("../specs/examples/decision_brief.catalogs.json"),
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn source_output_is_admitted_under_exact_native_configuration() {
    let (compiler, registry) = native_compiler();
    let l = Limits::default();
    let catalogs = MCPCatalogInputs::from_json(
        include_bytes!("../specs/examples/decision_brief.catalogs.json"),
        &l,
    )
    .unwrap();
    let output = compiler
        .compile_ir(
            include_str!("../specs/examples/decision_brief.htlk"),
            &catalogs,
        )
        .unwrap();
    let verified = verify_executable(&output.executable_bytes, &registry, &l).unwrap();
    assert_eq!(verified.fingerprint(), output.fingerprint);
    assert_eq!(output.analysis().structural_summary().expanded_nodes(), 34);
    assert_eq!(output.analysis().structural_summary().scope_depth(), 3);
    let bundle = SourceBundle::from_json(
        include_bytes!("../specs/examples/modules/source_bundle.json"),
        &l,
    )
    .unwrap();
    for (pin, package) in &bundle.packages {
        assert_eq!(*pin, package.digest(&l).unwrap());
    }
    let modular = compiler.compile_ir(&bundle, &catalogs).unwrap();
    assert_eq!(output.canonical_document, modular.canonical_document);
    assert_eq!(output.executable_bytes, modular.executable_bytes);
    assert!(
        modular
            .source_map
            .iter()
            .all(|e| matches!(e.span.source, SourceLocation::Module { .. }))
    );
    let module = ModuleRef {
        package_digest: bundle.entry.package_digest,
        module_path: "types".into(),
    };
    let report = compiler
        .inspect_module(&bundle, &module, &["BriefOutcome".into()], &catalogs)
        .unwrap();
    assert_eq!(report.available_exports.len(), 1);
    assert!(matches!(&report.exports[0],ExportSummary::Type{name,..} if name=="BriefOutcome"));
}

#[test]
fn owned_admission_retains_registry_and_checked_execution_across_threads() {
    fn shareable<T: Send + Sync + 'static>() {}
    shareable::<VerifiedExecutable<'static>>();
    let (compiler, registry) = native_compiler();
    let registry = Arc::new(registry);
    let weak = Arc::downgrade(&registry);
    let output = compiler
        .compile_source(
            r#"ir_version="0.1"
      graph test.constant { inputs={} outputs={value=integer}
        nodes { compute=eval(integer,42) }
        edges { edge result { from=compute.outputs.value to=outputs.value } }
      }"#,
            &catalogs(),
        )
        .unwrap();
    let verified = verify_executable_owned(
        &output.executable_bytes,
        Arc::clone(&registry),
        &Limits::default(),
    )
    .unwrap();
    drop(registry);
    assert!(weak.upgrade().is_some());
    assert_eq!(verified.document(), output.analysis().document());
    assert_eq!(verified.fingerprint(), output.fingerprint);
    let verified = std::thread::spawn(move || {
        let result = verified
            .evaluate(
                &analyzer::ScopeUse::Root,
                &analyzer::ExpressionSite::Eval("compute".parse().unwrap()),
                &EvaluationFrame::default(),
            )
            .unwrap();
        assert_eq!(
            result.value,
            EvaluationValue::Present(cbor::Value::Integer(42))
        );
        verified
    })
    .join()
    .unwrap();
    drop(verified);
    assert!(weak.upgrade().is_none());
}

#[test]
fn compiler_reports_cannot_replace_admission_of_authoritative_bytes() {
    let (compiler, registry) = native_compiler();
    let mut output = compiler
        .compile_source(
            r#"ir_version="0.1"
      graph test.empty { inputs={} outputs={} nodes{} edges{} }"#,
            &catalogs(),
        )
        .unwrap();
    let original = output.fingerprint;
    output.executable_bytes = ExecutableEnvelope::new(vec![0xa0], &Limits::default())
        .unwrap()
        .encode(&Limits::default())
        .unwrap();
    assert_eq!(output.analysis().fingerprint(), original);
    assert!(matches!(
        verify_executable_owned(
            &output.executable_bytes,
            Arc::new(registry),
            &Limits::default()
        ),
        Err(ExecutableVerificationError::Document(_))
    ));
}

#[test]
fn compiler_selected_policy_requires_explicit_runtime_linking() {
    let (compiler, _) = native_compiler();
    let output = compiler
        .compile_source(
            r#"ir_version="0.1"
      graph test.empty { inputs={} outputs={} nodes{} edges{} }"#,
            &catalogs(),
        )
        .unwrap();
    let registry = NativeRegistry::new(&Limits::default()).unwrap();
    assert!(matches!(
        verify_executable(&output.executable_bytes, &registry, &Limits::default()),
        Err(ExecutableVerificationError::Registry(
            NativeRegistryError::PolicyNotLinked
        ))
    ));

    let mut config = compiler.config().clone();
    let profile = &config.profile;
    config.profile = ExecutionProfile::new(
        digest::Digest::from_bytes([0; 32]),
        profile.regex_engine().clone(),
        profile.schema_validator().clone(),
        profile.uri_template_engine().clone(),
        profile.policy_document(),
        &config.limits,
    )
    .unwrap();
    let unlinked = Compiler::new(config)
        .unwrap()
        .compile_source(
            r#"ir_version="0.1"
      graph test.empty { inputs={} outputs={} nodes{} edges{} }"#,
            &catalogs(),
        )
        .unwrap();
    let (_, registry) = native_compiler();
    assert!(matches!(
        verify_executable_owned(
            &unlinked.executable_bytes,
            Arc::new(registry),
            &Limits::default()
        ),
        Err(ExecutableVerificationError::Registry(
            NativeRegistryError::ProfileMismatch("core")
        ))
    ));
}

#[test]
fn complete_compiler_library_manifests_must_match_runtime_implementations() {
    let (base, _) = native_compiler();
    let limits = Limits::default();
    let returns = Port::new(ValueType::builtin(BuiltinType::Integer), true);
    let manifest = Library::new(
        "test.library".into(),
        "0.1".into(),
        digest::Digest::from_bytes([17; 32]),
        vec![(
            "constant".parse().unwrap(),
            FunctionSignature::new(vec![], vec![], returns, &limits).unwrap(),
        )],
        &limits,
    )
    .unwrap();
    let mut config = base.config().clone();
    config
        .libraries
        .insert(manifest.implementation_digest(), manifest.clone());
    let compiler = Compiler::new(config).unwrap();
    let source = format!(
        r#"ir_version="0.1"
      predicate_library lib=predicates.library("test.library") {{version="0.1" digest="{}"}}
      graph test.library {{inputs={{}} outputs={{value=integer}}
        nodes{{compute=eval(integer,lib.constant())}}
        edges{{edge result{{from=compute.outputs.value to=outputs.value}}}}
      }}"#,
        manifest.implementation_digest()
    );
    let output = compiler.compile_source(&source, &catalogs()).unwrap();
    let (_, mut registry) = native_compiler();
    assert!(matches!(
        verify_executable(&output.executable_bytes, &registry, &limits),
        Err(ExecutableVerificationError::Registry(
            NativeRegistryError::UnknownLibrary
        ))
    ));
    let function = NativeFunction::new(
        |_, _, _| Ok(EvaluationValue::Present(cbor::Value::Integer(7))),
        1,
    )
    .unwrap();
    registry
        .register(
            manifest.clone(),
            BTreeMap::from([("constant".parse().unwrap(), function)]),
        )
        .unwrap();
    let admitted =
        verify_executable_owned(&output.executable_bytes, Arc::new(registry), &limits).unwrap();
    assert_eq!(
        admitted
            .evaluate(
                &analyzer::ScopeUse::Root,
                &analyzer::ExpressionSite::Eval("compute".parse().unwrap()),
                &EvaluationFrame::default()
            )
            .unwrap()
            .value,
        EvaluationValue::Present(cbor::Value::Integer(7))
    );

    let (_, mut different) = native_compiler();
    let changed = Library::new(
        "test.library".into(),
        "0.2".into(),
        manifest.implementation_digest(),
        manifest.functions().to_vec(),
        &limits,
    )
    .unwrap();
    different
        .register(
            changed,
            BTreeMap::from([("constant".parse().unwrap(), function)]),
        )
        .unwrap();
    assert!(matches!(
        verify_executable(&output.executable_bytes, &different, &limits),
        Err(ExecutableVerificationError::Registry(
            NativeRegistryError::ManifestMismatch
        ))
    ));
}
