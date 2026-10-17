//! Compiler output interoperability with actual native profile/admission.
use htlk::{
    compiler::*,
    runtime::{NativeRegistry, verify_executable},
};
use htlk_executable::cbor::Limits;
use htlk_executable::*;
use std::collections::BTreeMap;

#[test]
fn source_output_is_admitted_under_exact_native_configuration() {
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
