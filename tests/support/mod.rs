//! Common exact native compiler configuration for facade integration tests.
use htlk::{compiler::*, runtime::NativeRegistry};
use htlk_executable::{cbor::Limits, *};
use std::collections::BTreeMap;

pub fn native_compiler() -> (Compiler, NativeRegistry) {
    let limits = Limits::default();
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
        &limits,
    )
    .unwrap();
    let mut registry = NativeRegistry::new(&limits).unwrap();
    let profile = registry.link_policy(&policy).unwrap();
    let compiler = Compiler::new(CompilerConfig {
        profile,
        policy,
        libraries: BTreeMap::new(),
        limits,
        frontend: FrontendLimits::default(),
        schema_options: analyzer::NativeSchemaOptions::default(),
        capabilities: CompilerCapabilities::default(),
    })
    .unwrap();
    (compiler, registry)
}
pub fn catalogs() -> MCPCatalogInputs {
    MCPCatalogInputs::from_json(
        include_bytes!("../../specs/examples/decision_brief.catalogs.json"),
        &Limits::default(),
    )
    .unwrap()
}
