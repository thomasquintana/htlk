//! Beginner-friendly interactive lessons backed by the real compiler and runtime.
mod host;
mod lessons;
mod terminal;

use host::Host;
use std::{
    error::Error,
    io::{self, BufRead, Write},
};
use terminal::{Navigation, Terminal};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

// Exercise every lesson, retry paths, both approval decisions and navigation.
const CHECK_SCRIPT: &str = "invalid\n1\nh\nnodes\nedges\nn\nHello from the guide\nn\nnot-a-number\n7\n5\ntrue\nn\ninteger\nstring\n\nTypes now match!\nn\nA research workflow\nn\nWhat is HTLK?\nmaybe\nallow\nr\nAnother lookup\ndeny\nn\nq\n";

async fn guide<R: BufRead + Send + 'static, W: Write>(terminal: &mut Terminal<R, W>) -> Result<()> {
    terminal.say(
        "Harness Toolkit — Interactive Guide

HTLK describes workflows as typed graphs. This Rust program is their host.
Each lesson uses the actual compiler or runtime and explains its results.

Controls: Enter accepts [the default]. m = menu. q = quit (also Ctrl-D / EOF).
Your lesson data lives in memory and is discarded when you leave the lesson.
Rust/setup help: docs/interactive-guide.md

Setting up the native compiler and local tutorial policy...",
    )?;
    let host = Host::new()?;
    let mut selected = None;
    let mut completed = [false; 6];
    loop {
        let result: Result<()> = async {
            let lesson = if let Some(lesson) = selected.take() {
                lesson
            } else {
                terminal.say("\nChoose a lesson (you can start anywhere):")?;
                for (index, title) in lessons::TITLES.iter().enumerate() {
                    terminal.say(format!("  {}. {title}{}", index + 1, if completed[index] { " [done]" } else { "" }))?;
                }
                let choice = terminal.choice("Lesson 1-6, h for help, q to quit", &["1", "2", "3", "4", "5", "6", "h"], "1").await?;
                if choice == "h" {
                    terminal.say("Start with 1 and use n to continue through all six lessons.
At the end of a lesson: n = next, r = retry, m = menu, q = quit.
Graphs: examples/interactive_guide/graphs/
Rust host: examples/interactive_guide/host.rs
Cargo compiles this Rust example and then runs it. The guide compiles HTLK graphs.")?;
                    return Ok(());
                }
                choice.parse::<usize>()? - 1
            };
            match lessons::run(lesson, terminal, &host).await {
                Ok(()) => {
                    completed[lesson] = true;
                    terminal.say(format!("\nProgress: {} / 6 lessons completed this session.", completed.iter().filter(|done| **done).count()))?;
                }
                Err(error) => {
                    if error.is::<Navigation>() { return Err(error); }
                    terminal.say(format!("\nLesson could not complete: {error}"))?;
                    terminal.say("Check any graph edits. Use r to retry or m to choose another lesson.")?;
                }
            }
            let choice = terminal.choice("n = next lesson, r = retry, m = menu, q = quit", &["n", "r"], "n").await?;
            if choice == "r" {
                selected = Some(lesson);
            } else if lesson + 1 < lessons::TITLES.len() {
                selected = Some(lesson + 1);
            } else {
                if completed.iter().all(|done| *done) {
                    terminal.say("\nYou've completed all six lessons!
You have compiled graphs, run workflows, inspected artifacts, handled errors,
answered a wait, and made an approval decision.
Continue with docs/interactive-guide.md#where-to-go-next or revisit a lesson.")?;
                } else {
                    terminal.say("\nYou've reached the end of the lesson list. Choose any unfinished lessons below.")?;
                }
            }
            Ok(())
        }.await;
        match result {
            Ok(()) => {}
            Err(error) => match error.downcast_ref::<Navigation>() {
                Some(Navigation::Menu) => selected = None,
                Some(Navigation::Quit) => {
                    terminal.say("\nGoodbye! Restart anytime with cargo run --example interactive_guide --locked")?;
                    return Ok(());
                }
                None => return Err(error),
            },
        }
    }
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => actix::System::new().block_on(guide(&mut Terminal::new(io::BufReader::new(io::stdin()), io::stdout()))),
        [flag] if flag == "--check" => actix::System::new().block_on(async {
            let mut terminal = Terminal::new(io::Cursor::new(CHECK_SCRIPT), Vec::new());
            guide(&mut terminal).await?;
            verify_walkthrough(&String::from_utf8(terminal.output)?)?;
            println!("All six lessons passed, including compiler/runtime rejection, a typed reply, and allow/deny approval.");
            Ok(())
        }),
        [flag] if flag == "--help" || flag == "-h" => {
            println!("Usage: cargo run --example interactive_guide --locked -- [--check | --help]\n\nWithout arguments: interactive lessons.\n--check: run and verify a credential-free scripted walkthrough.\nSetup and lesson notes: docs/interactive-guide.md");
            Ok(())
        },
        _ => Err("unknown arguments; use --help for usage".into()),
    }
}

fn verify_walkthrough(transcript: &str) -> Result<()> {
    if transcript.contains("Lesson could not complete:") {
        return Err(format!("scripted lesson failed:\n{transcript}").into());
    }
    for expected in [
        "Compilation succeeded:",
        "read_artifact: \"Hello from the guide\"",
        "Actual result: true",
        "Actual compiler diagnostic:",
        "Actual runtime rejection:",
        "Repaired run: Succeeded",
        "submit_reply: accepted; run: Succeeded",
        "Approved run: Succeeded",
        "Denied run: Failed",
        "You've completed all six lessons!",
        "Goodbye!",
    ] {
        if !transcript.contains(expected) {
            return Err(format!("walkthrough did not reach '{expected}':\n{transcript}").into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
