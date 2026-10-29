//! Source-to-actor content helpers preserve actual checks and argument protection.

#[allow(dead_code)]
mod support;
use htlk::compiler::Compiler;
use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    execution::{ExecutionConfig, ExecutionEngine},
    failure::{FailureCode, RuntimeFailure},
    link_mcp_content_library,
    records::{InputValue, RunStatus, SecurityLabels},
    store::{SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier, JsonDocument,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        caller: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure> {
        if caller.principal() == "viewer"
            && matches!(action, Authorization::ReadArtifact { metadata } if !metadata.security_labels().as_slice().is_empty())
        {
            return Err(RuntimeFailure::new(FailureCode::Policy));
        }
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::new(vec!["private".into()], &Limits::default()).unwrap())
    }
}

#[test]
fn source_calls_content_helpers_with_checked_narrowing_and_no_protection_downgrade() {
    actix::System::new().block_on(async {
        let limits = Limits::default();
        let (base, mut registry) = support::native_compiler_with_timeouts(10000, 1000);
        let manifest = link_mcp_content_library(&mut registry, &limits).unwrap();
        let mut config = base.config().clone();
        config.libraries.insert(manifest.implementation_digest(), manifest.clone());
        let source = format!(r#"ir_version="0.1"
          predicate_library content=predicates.library("htlk.mcp-content"){{version="1" digest="{}"}}
          graph example.content{{inputs={{response=McpToolResult}}outputs={{title=string,verified=boolean}}
          nodes{{title=eval(string,content.checked(content.json_at(content.data(inputs.response),"/title"))){{inputs={{response=McpToolResult}}}}
          verify=eval(boolean,content.text_contains(content.join_text(content.text_blocks(inputs.response),"\n"),"ALPHA")){{inputs={{response=McpToolResult}}}}}}
          edges{{edge title_input{{from=inputs.response to=title.inputs.response}}edge verify_input{{from=inputs.response to=verify.inputs.response}}
          edge title{{from=title.outputs.value to=outputs.title}}edge verified{{from=verify.outputs.value to=outputs.verified}}}}
          postconditions=outputs.verified}}"#, manifest.implementation_digest());
        let graph = Compiler::new(config).unwrap().compile_source(&source, &support::catalogs()).unwrap();
        let input = JsonDocument::new(br#"{
          "protocol_result":{"content":[{"type":"text","text":"Page marker: ALPHA"}],"structuredContent":{"title":"Store"}},
          "extraction":{"status":"selected","selected_candidate_id":"candidate_0","candidates":[{"id":"candidate_0","value":{"title":"Store"},"validation":"parsed","sources":[{"origin":"structured_content","method":"structured_content"}]}]},
          "data":{"title":"Store"},"validation":{"declared_schema":{"status":"not_configured"},"inferred_schema":{"status":"not_configured"},"application_schema":{"status":"not_configured"}}
        }"#, &limits).unwrap();
        let runtime = Arc::new(Runtime::new(SqliteStore::in_memory(StoreConfig::default()).await.unwrap(), Arc::new(registry), Arc::new(Policy), RuntimeConfig::default()).unwrap());
        let owner = HostContext::new("owner".into(), &limits).unwrap();
        runtime.register_graph(&owner, &graph.executable_bytes, &[]).await.unwrap();
        let engine = ExecutionEngine::launch(runtime.clone(), ExecutionConfig { poll_interval: Duration::from_millis(1), ..Default::default() }).unwrap();
        let started = runtime.start_run(&owner, graph.fingerprint, &BTreeMap::from([("response".parse().unwrap(), InputValue::Inline(input.value().clone()))]), "start", None).await.unwrap();
        let view = tokio::time::timeout(Duration::from_secs(10), async {
            loop { let view = runtime.inspect_run(&owner, &started.run_id).await.unwrap(); if view.header.status.is_terminal() { break view; } actix::clock::sleep(Duration::from_millis(2)).await; }
        }).await.unwrap();
        assert_eq!(view.header.status, RunStatus::Succeeded, "{view:#?}");
        let Value::Map(outputs) = view.outputs.unwrap() else { panic!() };
        for (port, expected) in [("title", Value::Text("Store".into())), ("verified", Value::Bool(true))] {
            let Value::Map(entry) = outputs.get(port).unwrap() else { panic!() };
            let Value::Text(id) = entry.get("artifact_id").unwrap() else { panic!() };
            let id = id.parse().unwrap();
            let artifact = runtime.read_artifact(&owner, id).await.unwrap();
            assert_eq!(artifact.value, expected);
            assert_eq!(artifact.metadata.security_labels().as_slice(), ["private".to_owned()]);
            assert_eq!(runtime.read_artifact(&HostContext::new("viewer".into(), &limits).unwrap(), id).await.unwrap_err().code(), FailureCode::Policy);
        }
        engine.shutdown().await.unwrap();
    });
}
