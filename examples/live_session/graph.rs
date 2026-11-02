//! A bounded host-driven loop whose tool inventory is fixed by actual catalogs.

use super::model::{Result, quote};
use htlk_executable::digest::Digest;

pub(super) fn source(
    alias: &str,
    tools: &[String],
    content: Digest,
    calls: usize,
) -> Result<String> {
    if tools.is_empty() || tools.len() > 128 || !(1..=500).contains(&calls) {
        return Err("invalid live-session inventory or call bound".into());
    }
    let mut nodes = String::from(
        r#"
        ticket = eval(json, inputs.previous) { inputs = { previous = json } }
        command = wait(json) { topic = "live-operation" timeout_ms = 600000 }
        arguments = eval(json, inputs.command.arguments) { inputs = { command = json } }
        done = eval(boolean, inputs.command.done) { inputs = { command = json } }
        stop = eval(json, inputs.previous) {
            inputs = { previous = json, command = json }
            when = command.outputs.value.done
        }
        "#,
    );
    let mut edges = String::from(
        r#"
        edge previous { from = carried.previous to = ticket.inputs.previous }
        edge request { from = ticket.outputs.value to = command.inputs.request }
        edge done_input { from = command.outputs.value to = done.inputs.command }
        edge arguments_input { from = command.outputs.value to = arguments.inputs.command }
        edge done { from = done.outputs.value to = next.done }
        edge stop_previous { from = carried.previous to = stop.inputs.previous }
        edge stop_command { from = command.outputs.value to = stop.inputs.command }
        edge stop_result { from = stop.outputs.value to = outputs.value when = status(@stop) == "succeeded" }
        edge stop_next { from = stop.outputs.value to = next.previous when = status(@stop) == "succeeded" }
        "#,
    );
    for (index, name) in tools.iter().enumerate() {
        let name = quote(name)?;
        let alias = quote(alias)?;
        nodes.push_str(&format!(
            r#"
            call_{index} = call(mcp.tool({alias}, {name})) {{
                when = not command.outputs.value.done and command.outputs.value.tool == {name}
            }}
            accepted_{index} = eval(json, {{tool={name},status="succeeded",result=inputs.result}}) {{
                inputs = {{ result = McpToolResult }}
                when = status(@call_{index}) == "succeeded"
            }}
            failed_{index} = eval(json, {{tool={name},status="failed"}}) {{
                when = status(@call_{index}) == "failed"
            }}
            "#,
        ));
        edges.push_str(&format!(
            r#"
            edge arguments_{index} {{ from=arguments.outputs.value to=call_{index}.inputs.arguments }}
            edge accepted_input_{index} {{ from=call_{index}.outputs.value to=accepted_{index}.inputs.result }}
            edge accepted_output_{index} {{ from=accepted_{index}.outputs.value to=outputs.value when=status(@accepted_{index})=="succeeded" }}
            edge accepted_next_{index} {{ from=accepted_{index}.outputs.value to=next.previous when=status(@accepted_{index})=="succeeded" }}
            edge failed_output_{index} {{ from=failed_{index}.outputs.value to=outputs.value when=status(@failed_{index})=="succeeded" }}
            edge failed_next_{index} {{ from=failed_{index}.outputs.value to=next.previous when=status(@failed_{index})=="succeeded" }}
            "#,
        ));
    }
    Ok(format!(
        r#"
        ir_version = "0.1"
        predicate_library content = predicates.library("htlk.mcp-content") {{
            version = "1" digest = "{content}"
        }}
        graph live.session {{
            inputs = {{}} outputs = {{ value = json }}
            nodes {{
                initial = eval(json, {{}})
                unfinished = eval(boolean, false)
                session = loop {{
                    inputs = {{ seed = json, finished = boolean }} outputs = {{ value = json }}
                    carried = {{ previous = inputs.seed, done = inputs.finished }}
                    body {{ nodes {{ {nodes} }} edges {{ {edges} }} }}
                    until = next.done max_iterations = {}
                }}
            }}
            edges {{
                edge initial {{ from=initial.outputs.value to=session.inputs.seed }}
                edge unfinished {{ from=unfinished.outputs.value to=session.inputs.finished }}
                edge result {{ from=session.outputs.value to=outputs.value }}
            }}
            limits {{ max_mcp_calls = {calls} }}
        }}
        "#,
        calls + 1,
    ))
}
