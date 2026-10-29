//! Actual compiler/runtime/session checks against an independent local wire peer.

use super::{
    model::{self, field, object, text},
    session,
};
use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Value},
};
use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

#[test]
fn durable_controller_keeps_one_provider_session_and_offline_inspection_calls_nothing() {
    actix::System::new().block_on(run_controller());
}

async fn run_controller() {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = std::env::temp_dir().join(format!(
        "htlk-live-controller-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir(&directory).unwrap();
    let log = directory.join("requests.jsonl");
    std::fs::write(&log, "").unwrap();
    let host = object(vec![
        ("alias", Value::Text("fixture".into())),
        ("deployment_id", Value::Text("test".into())),
        ("transport", Value::Text("stdio".into())),
        ("program", Value::Text("node".into())),
        (
            "arguments",
            Value::Array(vec![
                Value::Text(
                    root.join("tests/support/stateful_mcp_peer.mjs")
                        .to_str()
                        .unwrap()
                        .into(),
                ),
                Value::Text("stdio".into()),
                Value::Text("stateful".into()),
                Value::Text(log.to_str().unwrap().into()),
            ]),
        ),
        ("max_calls", Value::Integer(8)),
    ])
    .unwrap();
    model::write(&directory.join("host.json"), &host).unwrap();
    let tools = model::read(&root.join("examples/mcp_capability_spike/tools.json")).unwrap();
    let Value::Array(tools) = tools.value() else {
        panic!()
    };
    let common = r#""catalog_version":"0.1","mcp_protocol_version":"2025-11-25","servers":{"fixture":{"deployment_id":"test","transport":"stdio","implementation_name":"fixture","implementation_version":"1"}},"schema_documents":{}"#;
    let tools = tools
        .iter()
        .map(|descriptor| {
            format!(
                r#"{{"server":"fixture","descriptor":{}}}"#,
                std::str::from_utf8(
                    JsonDocument::from_value(descriptor, &Limits::default())
                        .unwrap()
                        .as_bytes()
                )
                .unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let catalog = format!(
        r#"{{"tools":{{{common},"tools":[{tools}]}},"resources":{{{common},"resources":[],"templates":[]}},"prompts":{{{common},"prompts":[]}}}}"#
    );
    std::fs::write(directory.join("catalogs.json"), catalog).unwrap();
    let output = directory.join("run");
    let mut session = session::Session::open(
        &directory.join("host.json"),
        &directory.join("catalogs.json"),
        &output,
    )
    .await
    .unwrap();
    let command = |tool: &str, arguments| {
        object(vec![
            ("tool", Value::Text(tool.into())),
            ("arguments", arguments),
        ])
        .unwrap()
    };
    session
        .step(&command("sandbox_create", object(vec![]).unwrap()))
        .await
        .unwrap();
    let created = model::read(&output.join("response-001.json")).unwrap();
    let sandbox = field(
        field(field(created.value(), "result").unwrap(), "data").unwrap(),
        "sandbox",
    )
    .unwrap()
    .clone();
    session
        .step(&command(
            "sandbox_execute",
            object(vec![
                ("sandbox", sandbox.clone()),
                ("marker", Value::Text("retained-marker".into())),
            ])
            .unwrap(),
        ))
        .await
        .unwrap();
    session
        .step(&command(
            "sandbox_inspect",
            object(vec![("sandbox", sandbox.clone())]).unwrap(),
        ))
        .await
        .unwrap();
    let inspected = model::read(&output.join("response-003.json")).unwrap();
    assert_eq!(
        text(
            field(
                field(field(inspected.value(), "result").unwrap(), "data").unwrap(),
                "marker"
            )
            .unwrap()
        )
        .unwrap(),
        "retained-marker"
    );
    session
        .step(&command(
            "sandbox_destroy",
            object(vec![("sandbox", sandbox)]).unwrap(),
        ))
        .await
        .unwrap();
    session
        .step(&command(
            "sandbox_inspect",
            object(vec![("sandbox", Value::Text("missing".into()))]).unwrap(),
        ))
        .await
        .unwrap();
    let failure = model::read(&output.join("step-005.json")).unwrap();
    assert_eq!(
        field(failure.value(), "failure_code").unwrap(),
        &Value::Text("MCP_TOOL_ERROR".into())
    );
    session.close(true).await.unwrap();
    let offline = session::inspect(&output).await.unwrap();
    assert_eq!(
        field(&offline, "verified_steps").unwrap(),
        &Value::Integer(5)
    );
    assert_eq!(
        field(&offline, "provider_calls").unwrap(),
        &Value::Integer(0)
    );
    let response_path = output.join("response-001.json");
    let original = std::fs::read(&response_path).unwrap();
    std::fs::write(&response_path, "{}").unwrap();
    assert!(session::inspect(&output).await.is_err());
    std::fs::write(&response_path, original).unwrap();
    let raw_path = output.join("raw-001.json");
    let original = std::fs::read(&raw_path).unwrap();
    std::fs::write(&raw_path, "{}").unwrap();
    assert!(session::inspect(&output).await.is_err());
    assert_eq!(std::fs::read(&raw_path).unwrap(), b"{}");
    std::fs::write(&raw_path, original).unwrap();
    let requests: Vec<_> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| JsonDocument::new(line.as_bytes(), &Limits::default()).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .filter(|request| field(request.value(), "method").ok()
                == Some(&Value::Text("initialize".into())))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| field(request.value(), "method").ok()
                == Some(&Value::Text("tools/call".into())))
            .count(),
        5
    );
    std::fs::remove_dir_all(directory).unwrap();
}
