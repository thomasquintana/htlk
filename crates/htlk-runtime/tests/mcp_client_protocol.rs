//! Independent client-service message invariants beyond generated JSON Schema.
use htlk_executable::{JsonDocument, cbor::Limits};
use htlk_runtime::mcp::client::*;
use std::{collections::BTreeMap, sync::Arc};

struct NoDispatch;
impl McpClientHandler for NoDispatch {
    fn handle<'a>(
        &'a self,
        _: &'a McpInteractionContext,
        _: &'a JsonDocument,
    ) -> McpServiceFuture<'a> {
        panic!("pure admission cannot invoke host services")
    }
}
fn config(services: &[McpClientService]) -> McpClientConfig {
    let handler: Arc<dyn McpClientHandler> = Arc::new(NoDispatch);
    McpClientConfig::new(
        McpClientLimits::default(),
        services
            .iter()
            .map(|service| (*service, handler.clone()))
            .collect::<BTreeMap<_, _>>(),
        None,
    )
    .unwrap()
}
fn doc(raw: &str) -> JsonDocument {
    JsonDocument::new(raw.as_bytes(), &Limits::default()).unwrap()
}
fn request(method: &str, params: &str) -> JsonDocument {
    doc(&format!(
        r#"{{"jsonrpc":"2.0","id":"server-request","method":"{method}","params":{params}}}"#
    ))
}
const TEXT: &str = r#"{"role":"assistant","model":"actual-model","content":{"type":"text","text":"Paris"},"stopReason":"endTurn"}"#;
const TOOL: &str = r#"{"name":"weather","inputSchema":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}"#;

#[test]
fn sampling_preserves_multimodal_shapes_and_negotiates_extensions_independently() {
    let limits = Limits::default();
    let basic = config(&[McpClientService::Sampling]);
    let input = request(
        "sampling/createMessage",
        r#"{"messages":[{"role":"user","content":[{"type":"text","text":"Describe"},{"type":"image","mimeType":"image/png","data":"TQ=="},{"type":"audio","mimeType":"audio/wav","data":"TQ=="}]}],"maxTokens":100,"modelPreferences":{"hints":[{"name":"preferred-model"}],"speedPriority":0.8}}"#,
    );
    assert_eq!(
        validate_client_request(&input, &basic, &limits).unwrap(),
        McpIncomingKind::Sampling
    );
    validate_client_result(&input, &doc(TEXT), &basic, &limits).unwrap();
    let invalid_binary = doc(std::str::from_utf8(input.as_bytes())
        .unwrap()
        .replace("TQ==", "!!!")
        .as_str());
    assert_eq!(
        validate_client_request(&invalid_binary, &basic, &limits)
            .unwrap_err()
            .rpc_code(),
        -32602
    );
    assert_eq!(
        validate_client_request(&input, &config(&[]), &limits)
            .unwrap_err()
            .rpc_code(),
        -32601
    );
    for extra in [
        r#""tools":[]"#,
        r#""toolChoice":{"mode":"auto"}"#,
        r#""includeContext":"allServers""#,
    ] {
        let input = request(
            "sampling/createMessage",
            &format!(r#"{{"messages":[],"maxTokens":10,{extra}}}"#),
        );
        assert!(validate_client_request(&input, &basic, &limits).is_err());
    }
    let contextual = config(&[
        McpClientService::Sampling,
        McpClientService::SamplingContext,
    ]);
    validate_client_request(
        &request(
            "sampling/createMessage",
            r#"{"messages":[],"maxTokens":10,"includeContext":"thisServer"}"#,
        ),
        &contextual,
        &limits,
    )
    .unwrap();
    for params in [
        r#"{"messages":[],"maxTokens":-1}"#,
        r#"{"messages":[{"role":"system","content":{"type":"text","text":"x"}}],"maxTokens":10}"#,
    ] {
        assert!(
            validate_client_request(&request("sampling/createMessage", params), &basic, &limits)
                .is_err()
        );
    }
}

#[test]
fn tool_sampling_requires_balanced_history_valid_inputs_and_tool_choice_results() {
    let limits = Limits::default();
    let config = config(&[McpClientService::Sampling, McpClientService::SamplingTools]);
    let history = r#"[{"role":"assistant","content":[{"type":"tool_use","id":"a","name":"weather","input":{"city":"Paris"}},{"type":"tool_use","id":"b","name":"weather","input":{"city":"London"}}]},{"role":"user","content":[{"type":"tool_result","toolUseId":"a","content":[]},{"type":"tool_result","toolUseId":"b","content":[]}]}]"#;
    let input = request(
        "sampling/createMessage",
        &format!(r#"{{"messages":{history},"tools":[{TOOL}],"maxTokens":100}}"#),
    );
    validate_client_request(&input, &config, &limits).unwrap();
    validate_client_result(&input, &doc(TEXT), &config, &limits).unwrap();
    let use_tool = doc(
        r#"{"role":"assistant","model":"actual","content":[{"type":"tool_use","id":"next","name":"weather","input":{"city":"Rome"}}],"stopReason":"toolUse"}"#,
    );
    validate_client_result(&input, &use_tool, &config, &limits).unwrap();
    let provider_reason = doc(&std::str::from_utf8(use_tool.as_bytes()).unwrap().replace(
        r#""stopReason":"toolUse""#,
        r#""stopReason":"provider-specific""#,
    ));
    validate_client_result(&input, &provider_reason, &config, &limits).unwrap();
    for (from, to) in [
        (r#""toolUseId":"b""#, r#""toolUseId":"a""#),
        (
            r#"{"type":"tool_result","toolUseId":"b","content":[]}"#,
            r#"{"type":"text","text":"mixed"}"#,
        ),
        (r#""role":"assistant""#, r#""role":"user""#),
    ] {
        let invalid_history = history.replace(from, to);
        let invalid = request(
            "sampling/createMessage",
            &format!(r#"{{"messages":{invalid_history},"tools":[{TOOL}],"maxTokens":100}}"#),
        );
        assert!(
            validate_client_request(&invalid, &config, &limits).is_err(),
            "{from}"
        );
    }
    for (from, to) in [
        (r#""weather""#, r#""unknown""#),
        (r#""city":"Rome""#, r#""city":42"#),
    ] {
        let invalid = doc(&std::str::from_utf8(use_tool.as_bytes())
            .unwrap()
            .replace(from, to));
        assert!(validate_client_result(&input, &invalid, &config, &limits).is_err());
    }
    let required = request(
        "sampling/createMessage",
        &format!(
            r#"{{"messages":[],"tools":[{TOOL}],"toolChoice":{{"mode":"required"}},"maxTokens":100}}"#
        ),
    );
    assert!(validate_client_result(&required, &doc(TEXT), &config, &limits).is_err());
    validate_client_result(&required, &use_tool, &config, &limits).unwrap();
    let none = doc(&std::str::from_utf8(required.as_bytes())
        .unwrap()
        .replace(r#""mode":"required""#, r#""mode":"none""#));
    assert!(validate_client_result(&none, &use_tool, &config, &limits).is_err());
}

#[test]
fn form_replies_validate_the_requested_schema_including_fractional_numbers_and_multiselects() {
    let limits = Limits::default();
    let config = config(&[McpClientService::ElicitationForm]);
    let input = request(
        "elicitation/create",
        r#"{"message":"Choose","requestedSchema":{"type":"object","properties":{"amount":{"type":"number","minimum":0.5,"maximum":3},"colors":{"type":"array","items":{"type":"string","enum":["red","blue"]},"minItems":1,"maxItems":2}},"required":["amount","colors"]}}"#,
    );
    assert_eq!(
        validate_client_request(&input, &config, &limits).unwrap(),
        McpIncomingKind::Form
    );
    validate_client_result(
        &input,
        &doc(r#"{"action":"accept","content":{"amount":1.5,"colors":["red"]}}"#),
        &config,
        &limits,
    )
    .unwrap();
    for result in [
        r#"{"action":"accept","content":{"amount":4,"colors":["red"]}}"#,
        r#"{"action":"accept","content":{"amount":1,"colors":["green"]}}"#,
        r#"{"action":"accept","content":{"colors":["red"]}}"#,
        r#"{"action":"accept"}"#,
        r#"{"action":"decline","content":{"amount":1}}"#,
        r#"{"action":"cancel","content":{}}"#,
    ] {
        assert!(
            validate_client_result(&input, &doc(result), &config, &limits).is_err(),
            "{result}"
        );
    }
    for action in ["decline", "cancel"] {
        validate_client_result(
            &input,
            &doc(&format!(r#"{{"action":"{action}"}}"#)),
            &config,
            &limits,
        )
        .unwrap();
    }
}

#[test]
fn url_and_roots_validation_has_no_navigation_or_content_acquisition() {
    let limits = Limits::default();
    let config = config(&[McpClientService::ElicitationUrl, McpClientService::Roots]);
    let input = request(
        "elicitation/create",
        r#"{"mode":"url","message":"Authorize","elicitationId":"external","url":"https://example.invalid/authorize"}"#,
    );
    assert_eq!(
        validate_client_request(&input, &config, &limits).unwrap(),
        McpIncomingKind::Url
    );
    for action in ["accept", "decline", "cancel"] {
        validate_client_result(
            &input,
            &doc(&format!(r#"{{"action":"{action}"}}"#)),
            &config,
            &limits,
        )
        .unwrap();
    }
    assert!(
        validate_client_result(
            &input,
            &doc(r#"{"action":"accept","content":{"token":"never"}}"#),
            &config,
            &limits
        )
        .is_err()
    );
    assert!(
        validate_client_request(
            &request(
                "elicitation/create",
                r#"{"message":"Form","requestedSchema":{"type":"object","properties":{}}}"#
            ),
            &config,
            &limits
        )
        .is_err()
    );
    let roots = request("roots/list", "{}");
    validate_client_result(
        &roots,
        &doc(r#"{"roots":[{"uri":"file:///project","name":"Workspace"}]}"#),
        &config,
        &limits,
    )
    .unwrap();
    assert!(
        validate_client_result(
            &roots,
            &doc(r#"{"roots":[{"uri":"https://example.invalid/project"}]}"#),
            &config,
            &limits
        )
        .is_err()
    );
}
