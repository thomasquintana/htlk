//! Small exercises that use the same public APIs as a Rust host application.
use crate::{Result, host::Host, terminal::Terminal};
use htlk::{
    compiler::CompilationSuccess,
    runtime::{failure::FailureCode, records::RunStatus},
};
use htlk_executable::cbor::Value;
use std::{
    io::{BufRead, Write},
    sync::atomic::Ordering,
};

pub const MESSAGE: &str = include_str!("graphs/message.htlk");
pub const COMPARE: &str = include_str!("graphs/compare.htlk");
pub const TYPE_ERROR: &str = include_str!("graphs/type_error.htlk");
pub const QUESTION: &str = include_str!("graphs/question.htlk");
pub const TOOL: &str = include_str!("graphs/tool.htlk");
pub const TITLES: [&str; 6] = [
    "Your first graph",
    "Run a workflow",
    "Add a calculation",
    "Understand errors",
    "Human input",
    "Tools and approvals",
];

fn source<R: BufRead + Send + 'static, W: Write>(
    terminal: &mut Terminal<R, W>,
    file: &str,
    text: &str,
    diagram: &str,
) -> Result<()> {
    terminal.say(format!(
        "\nSource: examples/interactive_guide/graphs/{file}"
    ))?;
    terminal.say(format!("Flow: {diagram}\n"))?;
    terminal.say(text)
}

fn compiled<R: BufRead + Send + 'static, W: Write>(
    terminal: &mut Terminal<R, W>,
    host: &Host,
    text: &str,
) -> Result<CompilationSuccess> {
    let graph = host.compile(text)?;
    terminal.say(format!(
        "Compilation succeeded: {} executable bytes",
        graph.executable_bytes.len()
    ))?;
    terminal.say(format!("Fingerprint: {}", graph.fingerprint))?;
    Ok(graph)
}

pub async fn run<R: BufRead + Send + 'static, W: Write>(
    lesson: usize,
    terminal: &mut Terminal<R, W>,
    host: &Host,
) -> Result<()> {
    terminal.say(format!(
        "\n=== Lesson {} / 6: {} ===",
        lesson + 1,
        TITLES[lesson]
    ))?;
    match lesson {
        0 => first_graph(terminal, host).await,
        1 => workflow(terminal, host).await,
        2 => calculation(terminal, host).await,
        3 => errors(terminal, host).await,
        4 => question(terminal, host).await,
        5 => tool(terminal, host).await,
        _ => Err("unknown lesson".into()),
    }
}

async fn first_graph<R: BufRead + Send + 'static, W: Write>(
    terminal: &mut Terminal<R, W>,
    host: &Host,
) -> Result<()> {
    terminal.say(
        "A harness is a workflow. In HTLK you describe it as a typed graph.
A port is a named input or output. An edge connects ports; a node does work.
The graph is written in HTLK's .htlk language, not in Rust.
A Rust host application compiles the graph and runs it.",
    )?;
    source(
        terminal,
        "message.htlk",
        MESSAGE,
        "inputs.message --> outputs.message",
    )?;
    terminal.say(
        "ir_version chooses the language version. guide.message names this graph.
Both ports declare string, which means text. nodes {} is empty because
passing a message directly to the output requires no calculation.",
    )?;
    loop {
        let answer = terminal
            .ask(
                "Which connects the ports: nodes or edges? (h for a hint)",
                Some("edges"),
            )
            .await?;
        match answer.to_lowercase().as_str() {
            "edges" => break,
            "h" | "hint" => terminal.say("Hint: look for 'from' and 'to' in the source.")?,
            _ => terminal.say(
                "Nodes do work. Look at the block containing 'from' and 'to', then try again.",
            )?,
        }
    }
    compiled(terminal, host, MESSAGE)?;
    terminal.say(
        "The compiler checked the graph and produced a self-contained executable.
Its fingerprint identifies its executable meaning. Compiling does not run it.
Rust API: host.compiler.compile_source(source, &host.catalogs)
Next we will register this executable with a runtime and give it an input.",
    )
}

async fn workflow<R: BufRead + Send + 'static, W: Write>(
    terminal: &mut Terminal<R, W>,
    host: &Host,
) -> Result<()> {
    source(
        terminal,
        "message.htlk",
        MESSAGE,
        "your message --> graph --> output artifact",
    )?;
    terminal.say(
        "The host supplies typed values and a HostContext (the caller's identity).
This guide uses a local demo identity and an in-memory SQLite store.
Registration independently checks the executable against the host configuration.",
    )?;
    let message = terminal
        .ask("Enter a message", Some("Hello, HTLK!"))
        .await?;
    let graph = compiled(terminal, host, MESSAGE)?;
    let mut session = host.session(&graph).await?;
    let result = async {
        terminal
            .say("register_graph: executable admitted. ExecutionEngine: supervision launched.")?;
        session
            .start(&[("message", Value::Text(message.clone()))])
            .await?;
        terminal.say(format!(
            "start_run: {}",
            session.run.as_ref().ok_or("missing run")?
        ))?;
        let output = session.output("message").await?;
        if output != Value::Text(message) {
            return Err("message output did not match input".into());
        }
        terminal.say("inspect_run: Succeeded")?;
        terminal.say(format!("read_artifact: {}", display(&output)))?;
        terminal.say(
            "Run outputs contain artifact references. read_artifact retrieves the accepted,
typed value after authorization. A successful compile alone does not create a run.
Find the full Rust sequence in examples/interactive_guide/host.rs.",
        )
    }
    .await;
    session.finish(result).await
}

async fn calculation<R: BufRead + Send + 'static, W: Write>(
    terminal: &mut Terminal<R, W>,
    host: &Host,
) -> Result<()> {
    source(
        terminal,
        "compare.htlk",
        COMPARE,
        "left, right --> compare (eval) --> greater",
    )?;
    terminal.say(
        "eval(boolean, expression) is a pure calculation with a true/false result.
Inside the node, inputs.left means the node's own input, not an implicit read
of the graph input. The two input edges explicitly supply those values.
eval publishes its result on outputs.value; the last edge exports it as greater.
HTLK supports comparisons directly; arithmetic uses explicitly linked library
functions rather than infix operators such as +.",
    )?;
    let left = terminal.integer("Left integer", "7").await?;
    let right = terminal.integer("Right integer", "5").await?;
    let prediction = terminal
        .choice(
            "Predict: is left greater than right? true / false",
            &["true", "false"],
            "true",
        )
        .await?;
    let graph = compiled(terminal, host, COMPARE)?;
    let mut session = host.session(&graph).await?;
    let result = async {
        session
            .start(&[
                ("left", Value::Integer(left)),
                ("right", Value::Integer(right)),
            ])
            .await?;
        let output = session.output("greater").await?;
        if output != Value::Bool(left > right) {
            return Err("calculation returned an unexpected result".into());
        }
        terminal.say(format!("Actual result: {}", display(&output)))?;
        terminal.say(if (prediction == "true") == (left > right) {
            "Your prediction matched! The runtime evaluated the typed expression."
        } else {
            "Compare the result with your prediction. The node tests whether left > right."
        })?;
        terminal.say("Try again with different inputs using r at the next prompt.")
    }
    .await;
    session.finish(result).await
}

async fn errors<R: BufRead + Send + 'static, W: Write>(
    terminal: &mut Terminal<R, W>,
    host: &Host,
) -> Result<()> {
    source(
        terminal,
        "type_error.htlk",
        TYPE_ERROR,
        "string input --X--> integer output",
    )?;
    terminal.say("This deliberately broken graph connects incompatible types.")?;
    let failure = match host.compile(TYPE_ERROR) {
        Err(failure) => failure,
        Ok(_) => return Err("type_error.htlk no longer contains the intended error; restore its integer output to follow this lesson".into()),
    };
    if !failure
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "C_TYPE")
    {
        return Err(failure.into());
    }
    terminal.say(format!("Actual compiler diagnostic:\n{failure}"))?;
    for diagnostic in &failure.diagnostics {
        terminal.say(format!("  Location: {:?}", diagnostic.location))?;
    }
    terminal
        .say("No executable was emitted. To preserve the message, the output must accept text.")?;
    loop {
        let answer = terminal
            .choice(
                "Change the output type to integer or string?",
                &["integer", "string"],
                "string",
            )
            .await?;
        if answer == "string" {
            break;
        }
        terminal.say("Hint: the input is string. This graph forwards it without converting it.")?;
    }
    let fixed = TYPE_ERROR.replace("message = integer", "message = string");
    let graph = compiled(terminal, host, &fixed)?;
    terminal.say(
        "Now test the other boundary: a valid graph can still receive an invalid host input.",
    )?;
    terminal
        .ask(
            "Press Enter to try sending integer 42 to its string input",
            None,
        )
        .await?;
    let mut session = host.session(&graph).await?;
    let result = async {
        let error = match session.start(&[("message", Value::Integer(42))]).await {
            Err(error) => error,
            Ok(()) => return Err("the edited graph now accepts integer inputs; restore its string input to follow this lesson".into()),
        };
        let runtime_error = error.downcast_ref::<htlk::runtime::RuntimeError>().ok_or("expected runtime validation failure")?;
        if runtime_error.code() != FailureCode::Input { return Err(error); }
        terminal.say(format!("Actual runtime rejection: {error}"))?;
        terminal.say("The input was rejected before a run was admitted. Let's supply the right type.")?;
        let message = terminal.ask("Replacement text", Some("Types now match!")).await?;
        session.start(&[("message", Value::Text(message))]).await?;
        terminal.say(format!("Repaired run: Succeeded; output: {}", display(&session.output("message").await?)))?;
        terminal.say("Compile-time checks validate graph connections. Runtime checks validate actual values.
The repair above was in memory. You can also edit the .htlk files in your editor
and rerun the cargo command to rebuild the guide with your changes.")
    }.await;
    session.finish(result).await
}

async fn question<R: BufRead + Send + 'static, W: Write>(
    terminal: &mut Terminal<R, W>,
    host: &Host,
) -> Result<()> {
    source(
        terminal,
        "question.htlk",
        QUESTION,
        "request --> wait(string) --> your answer",
    )?;
    terminal.say(
        "wait(string) pauses this node until the host submits a text reply.
Its topic routes the question; the topic itself grants no permission to reply.
The runtime checks the host identity, reply type, and addressed wait ID.",
    )?;
    let graph = compiled(terminal, host, QUESTION)?;
    let mut session = host.session(&graph).await?;
    let result = async {
        session
            .start(&[(
                "request",
                Value::Text("What would you like to build with HTLK?".into()),
            )])
            .await?;
        let wait = session.wait().await?;
        terminal.say(format!(
            "Open wait: {} (topic: {})",
            wait.wait_id, wait.topic
        ))?;
        if let Some(id) = wait.request_artifact_id {
            let request = session.runtime.read_artifact(&session.context, id).await?;
            terminal.say(format!("Request artifact: {}", display(&request.value)))?;
        }
        terminal.say(
            "The workflow is waiting for you. Actor supervision continues while you type.
This lesson has a 30-minute graph deadline; use r to start fresh if it expires.",
        )?;
        let answer = terminal
            .ask("Your answer", Some("A research workflow"))
            .await?;
        session
            .runtime
            .submit_reply(
                &session.context,
                wait.wait_id,
                Value::Text(answer.clone()),
                "guide-reply",
            )
            .await?;
        let output = session.output("answer").await?;
        if output != Value::Text(answer) {
            return Err("reply output did not match".into());
        }
        terminal.say(format!(
            "submit_reply: accepted; run: Succeeded; answer: {}",
            display(&output)
        ))?;
        terminal.say(
            "Accepting a reply and successfully finishing a node are separate steps.
Here the accepted string passes through to the graph's output.",
        )
    }
    .await;
    session.finish(result).await
}

async fn tool<R: BufRead + Send + 'static, W: Write>(
    terminal: &mut Terminal<R, W>,
    host: &Host,
) -> Result<()> {
    source(
        terminal,
        "tool.htlk",
        TOOL,
        "query --> arguments --> MCP lookup --> result",
    )?;
    terminal.say(
        "MCP (Model Context Protocol) describes tools a host can connect to.
The compiler resolves lookup from catalogs.json; it does not discover tools online.
This lesson uses an in-process scripted adapter with a fixed response.
No server process, network connection, or provider credentials are needed.
The host's policy requires a separate reviewer to approve this exact action.",
    )?;
    let query = loop {
        let query = terminal.ask("Lookup query", Some("What is HTLK?")).await?;
        if !query.is_empty() {
            break query;
        }
        terminal.say("The tool schema requires a nonempty query.")?;
    };
    let graph = compiled(terminal, host, TOOL)?;
    let mut session = host.session(&graph).await?;
    let result = async {
        session.start(&[("query", Value::Text(query))]).await?;
        let wait = session.wait().await?;
        let digest = wait
            .request_digest
            .ok_or("expected exact-action approval")?;
        terminal.say(format!(
            "Approval wait: {}\nExact request digest: {digest}",
            wait.wait_id
        ))?;
        if let Some(request) = &wait.approval_request {
            terminal.say(format!("Actual approval request: {}", display(request)))?;
        }
        terminal.say(
            "The local guide now acts as guide-reviewer. In an application, your host
must authenticate that reviewer. Typing an identity into graph data grants no authority.",
        )?;
        let decision = terminal
            .choice(
                "Approve this action? allow / deny",
                &["allow", "deny"],
                "allow",
            )
            .await?;
        session.approve(&wait, decision == "allow").await?;
        let view = session.terminal().await?;
        if decision == "allow" {
            let output = session.output("result").await?;
            if session.tool.calls.load(Ordering::SeqCst) != 1 {
                return Err("approved tool was not invoked exactly once".into());
            }
            terminal.say(format!(
                "Approved run: Succeeded; result: {}",
                display(&output)
            ))?;
        } else {
            if view.header.status != RunStatus::Failed
                || session.tool.calls.load(Ordering::SeqCst) != 0
            {
                return Err("denied action unexpectedly ran".into());
            }
            terminal.say(format!("Denied run: Failed; failure: {:?}", view.failure))?;
            terminal.say(
                "The tool was never invoked. This is the expected result of denying approval.",
            )?;
        }
        terminal.say(
            "Typed wait replies provide data. Exact-action approvals authorize a specific action.
Try r to explore the other decision. For graph joins and live extension, run:
  cargo run --example runtime_workflow --locked
Next reading: docs/interactive-guide.md#where-to-go-next",
        )
    }
    .await;
    session.finish(result).await
}

fn display(value: &Value) -> String {
    match htlk_executable::JsonDocument::from_value(
        value,
        &htlk_executable::cbor::Limits::default(),
    ) {
        Ok(json) => String::from_utf8_lossy(json.as_bytes()).into_owned(),
        Err(_) => format!("{value:?}"),
    }
}
