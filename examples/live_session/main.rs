//! Opt-in private durable live acceptance using ordinary compiled tool graphs.

mod graph;
mod host;
mod model;
mod session;
#[cfg(test)]
mod tests;

use htlk_executable::{
    JsonDocument,
    cbor::{Limits, Value},
};
use model::{Result, field};
use std::{
    io::{BufRead, Read},
    path::Path,
};

async fn run(arguments: &[String]) -> Result<()> {
    if arguments.len() == 3 && arguments[1] == "--inspect" {
        return model::emit(&session::inspect(Path::new(&arguments[2])).await?);
    }
    if arguments.len() != 4 {
        return Err(
            "Usage: mcp_live_session HOST.json CATALOGS.json NEW_OUTPUT_DIR | --inspect OUTPUT_DIR"
                .into(),
        );
    }
    let mut session = session::Session::open(
        Path::new(&arguments[1]),
        Path::new(&arguments[2]),
        Path::new(&arguments[3]),
    )
    .await?;
    model::emit(&model::object(vec![(
        "event",
        Value::Text("ready".into()),
    )])?)?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    std::thread::spawn(move || {
        let input = std::io::stdin();
        let mut input = input.lock();
        loop {
            let mut line = Vec::new();
            let result = (&mut input)
                .take(16 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut line);
            if !matches!(result, Ok(count) if count > 0) {
                break;
            }
            if line.len() > 16 * 1024 * 1024 {
                break;
            }
            if sender.blocking_send(line).is_err() {
                break;
            }
        }
    });
    let result = async {
        while let Some(line) = receiver.recv().await {
            let command = JsonDocument::new(&line, &Limits::default())?;
            if field(command.value(), "stop").ok() == Some(&Value::Bool(true)) {
                return Ok(());
            }
            model::emit(&session.step(command.value()).await?)?;
        }
        Err::<(), Box<dyn std::error::Error>>("live controller disconnected before cleanup".into())
    }
    .await;
    let closed = session.close(result.is_ok()).await;
    result?;
    closed?;
    model::emit(&model::object(vec![(
        "event",
        Value::Text("closed".into()),
    )])?)
}

fn main() -> Result<()> {
    actix::System::new().block_on(run(&std::env::args().collect::<Vec<_>>()))
}
