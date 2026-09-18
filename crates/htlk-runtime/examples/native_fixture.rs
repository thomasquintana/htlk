//! Prints a complete canonical empty-graph fixture for the current native profile.
use htlk_executable::cbor::Limits;
use htlk_executable::{
    CanonicalDocument, DocumentFields, EvaluatorLimits, ExecutionLimits, PolicyDocument,
    PolicyFields, Scope, ScopeContext, ScopeFields,
};
use htlk_runtime::{NativeRegistry, verify_executable};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = Limits::default();
    let policy = PolicyDocument::new(
        PolicyFields {
            cost_unit: "credits".into(),
            defaults: ExecutionLimits::new()
                .with_timeout_ms(1000)?
                .with_attempt_timeout_ms(100)?
                .with_max_concurrency(1)?,
            evaluator_limits: EvaluatorLimits {
                max_expression_depth: 64,
                max_value_bytes: 65536,
                max_collection_visits: 10000,
                max_regex_bytes: 1024,
                max_regex_compiled_bytes: 4096,
                max_output_bytes: 65536,
                max_steps: 100000,
            },
            maximum_scope_depth: 32,
            maximum_expanded_nodes: 1000,
        },
        &limits,
    )?;
    let mut registry = NativeRegistry::new(&limits)?;
    let profile = registry.link_policy(&policy)?;
    let scope = Scope::new(ScopeFields::default(), ScopeContext::Ordinary, &limits)?;
    let root = scope.digest(ScopeContext::Ordinary, &limits)?;
    let mut fields = DocumentFields::new("fixture.empty".into(), profile, root);
    fields.scopes.insert(root, scope);
    fields
        .documents
        .insert(policy.digest(), policy.document().clone());
    let bytes = CanonicalDocument::new(fields, &limits)?
        .envelope(&limits)?
        .encode(&limits)?;
    let verified = verify_executable(&bytes, &registry, &limits)?;
    println!("fingerprint={}", verified.fingerprint());
    println!("root_scope={root}");
    for chunk in bytes.chunks(32) {
        for byte in chunk {
            print!("{byte:02x}");
        }
        println!();
    }
    Ok(())
}
