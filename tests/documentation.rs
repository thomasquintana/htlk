//! Documentation examples are checked through independent observable results.
mod support;

use htlk::runtime::{
    Authorization, HostContext, Runtime, RuntimeConfig, RuntimePolicy,
    execution::{ExecutionConfig, ExecutionEngine, ExecutionScope},
    failure::{FailureCode, RuntimeFailure},
    records::{InputValue, RunStatus, SecurityLabels},
    store::{SqliteStore, StoreConfig},
};
use htlk_executable::{
    Identifier,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn complete_programs(markdown: &str) -> Vec<&str> {
    markdown
        .split("```htlk\n")
        .skip(1)
        .filter_map(|block| {
            let source = block.split("```").next().unwrap().trim();
            source.starts_with("ir_version").then_some(source)
        })
        .collect()
}

struct Policy;
impl RuntimePolicy for Policy {
    fn authorize(&self, _: &HostContext, _: Authorization<'_>) -> Result<(), RuntimeFailure> {
        Ok(())
    }
    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure> {
        Ok(SecurityLabels::new(vec![], &Limits::default()).unwrap())
    }
}

async fn execute(source: &str, inputs: &[(&str, Value)], output: &str) -> Value {
    let (compiler, registry) = support::native_compiler_with_timeouts(30_000, 100);
    let compiled = compiler
        .compile_source(source, &support::catalogs())
        .unwrap();
    let store = SqliteStore::in_memory(StoreConfig::default())
        .await
        .unwrap();
    let runtime = Arc::new(
        Runtime::new(
            store,
            Arc::new(registry),
            Arc::new(Policy),
            RuntimeConfig::default(),
        )
        .unwrap(),
    );
    let caller = HostContext::new("documentation-reader".into(), &Limits::default()).unwrap();
    runtime
        .register_graph(&caller, &compiled.executable_bytes, &[])
        .await
        .unwrap();
    let engine = ExecutionEngine::launch(
        runtime.clone(),
        ExecutionConfig {
            scope: ExecutionScope::Attached,
            ..Default::default()
        },
    )
    .unwrap();
    let inputs = inputs
        .iter()
        .map(|(name, value)| (name.parse().unwrap(), InputValue::Inline(value.clone())))
        .collect::<BTreeMap<_, _>>();
    let started = runtime
        .start_run(&caller, compiled.fingerprint, &inputs, "example", None)
        .await
        .unwrap();
    engine.attach_run(&started.run_id).await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let view = loop {
        let view = runtime.inspect_run(&caller, &started.run_id).await.unwrap();
        if view.header.status.is_terminal() {
            break view;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "guide example stalled"
        );
        actix::clock::sleep(Duration::from_millis(5)).await;
    };
    engine.shutdown().await.unwrap();
    assert_eq!(
        view.header.status,
        RunStatus::Succeeded,
        "{:?}",
        view.failure
    );
    let Some(Value::Map(outputs)) = view.outputs else {
        panic!("no output manifest")
    };
    let Some(Value::Map(entry)) = outputs.get(output) else {
        panic!("missing documented output")
    };
    let Some(Value::Text(id)) = entry.get("artifact_id") else {
        panic!("missing accepted artifact")
    };
    runtime
        .read_artifact(&caller, id.parse().unwrap())
        .await
        .unwrap()
        .value
}

#[test]
fn complete_user_guide_programs_produce_the_explained_values() {
    let programs = complete_programs(include_str!("../docs/user-guide.md"));
    assert_eq!(
        programs.len(),
        2,
        "new complete examples need observable cases"
    );
    let hello = programs
        .iter()
        .find(|p| p.contains("graph hello {"))
        .unwrap();
    let compare = programs
        .iter()
        .find(|p| p.contains("graph compare {"))
        .unwrap();
    actix::System::new().block_on(async {
        assert_eq!(
            execute(
                hello,
                &[("message", Value::Text("A real workflow".into()))],
                "message"
            )
            .await,
            Value::Text("A real workflow".into())
        );
        for (left, right, expected) in [(7, 5, true), (5, 5, false), (-1, 2, false)] {
            assert_eq!(
                execute(
                    compare,
                    &[
                        ("left", Value::Integer(left)),
                        ("right", Value::Integer(right))
                    ],
                    "greater"
                )
                .await,
                Value::Bool(expected)
            );
        }
    });
}

#[test]
fn loop_body_order_is_nonsemantic_but_inventory_is_exact() {
    let (compiler, _) = support::native_compiler();
    let source = |body: &str| {
        format!(
            r#"ir_version="0.1"
        graph ordering {{ inputs={{}} outputs={{}}
          nodes {{ work=loop {{ inputs={{}} outputs={{}} carried={{}}
            body {{ {body} }} until=true max_iterations=1 }} }}
          edges {{}} postconditions=status(@work)=="succeeded"
        }}"#
        )
    };
    let nodes_first = compiler
        .compile_source(&source("nodes {} edges {}"), &support::catalogs())
        .unwrap();
    let edges_first = compiler
        .compile_source(&source("edges {} nodes {}"), &support::catalogs())
        .unwrap();
    assert_eq!(nodes_first.executable_bytes, edges_first.executable_bytes);
    for body in [
        "",
        "nodes {}",
        "edges {}",
        "nodes {} nodes {} edges {}",
        "edges {} nodes {} edges {}",
    ] {
        assert!(
            compiler
                .compile_source(&source(body), &support::catalogs())
                .is_err(),
            "{body}"
        );
    }
}

#[test]
fn documented_failure_spellings_are_public_runtime_codes() {
    let specification = include_str!("../specs/runtime-spec.md")
        .split("## 16. Stable failures")
        .nth(1)
        .unwrap()
        .split("## 17.")
        .next()
        .unwrap();
    let mut count = 0;
    for code in specification
        .lines()
        .filter(|line| line.starts_with('|'))
        .flat_map(|line| line.split('`').skip(1).step_by(2))
    {
        if code.starts_with("E_") || code.starts_with("MCP_") {
            assert!(
                code.parse::<FailureCode>().is_ok(),
                "unknown documented code: {code}"
            );
            count += 1;
        }
    }
    assert!(count > 0);
}
