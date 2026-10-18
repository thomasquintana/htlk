//! Exercise real lesson flows, including navigation while a run is waiting.
use super::*;

fn transcript(script: &'static str) -> String {
    actix::System::new().block_on(async {
        let mut terminal = Terminal::new(io::Cursor::new(script), Vec::new());
        guide(&mut terminal).await.unwrap();
        String::from_utf8(terminal.output).unwrap()
    })
}

#[test]
fn all_lessons_run_with_real_compilation_execution_and_both_approval_decisions() {
    let output = transcript(CHECK_SCRIPT);
    verify_walkthrough(&output).unwrap();
    assert!(output.contains("Choose 1 / 2 / 3 / 4 / 5 / 6 / h"));
    assert!(output.contains("Hint: look for 'from' and 'to'"));
    assert!(output.contains("Enter a whole number"));
    assert!(output.contains("Choose allow / deny"));
    assert!(output.contains("The tool was never invoked"));
}

#[test]
fn menu_and_eof_close_active_input_and_approval_waits() {
    // m cancels the input-wait run; EOF closes a fresh pending approval run.
    let output = transcript("5\nm\n6\nA lookup\n");
    assert!(output.contains("Open wait:"));
    assert!(output.contains("Approval wait:"));
    assert!(!output.contains("Lesson could not complete:"));
    assert!(output.ends_with("--locked\n"));
}

#[test]
fn empty_input_exits_and_help_returns_to_menu() {
    assert!(transcript("").contains("Goodbye!"));
    let output = transcript("h\nm\nq\n");
    assert!(output.contains("Cargo compiles this Rust example"));
    assert!(output.contains("Goodbye!"));
}

#[test]
fn different_calculation_inputs_and_wrong_predictions_can_be_retried() {
    let output = transcript("3\n-8\n-8\ntrue\nr\n2\n-9\nfalse\nq\n");
    assert!(output.contains("Actual result: false"));
    assert!(output.contains("Actual result: true"));
    assert!(output.contains("Compare the result with your prediction"));
    assert!(!output.contains("Lesson could not complete:"));
}

#[test]
fn quitting_during_a_wait_cancels_the_run_before_shutdown() {
    actix::System::new().block_on(async {
        let host = Host::new().unwrap();
        let graph = host.compile(lessons::QUESTION).unwrap();
        let mut session = host.session(&graph).await.unwrap();
        session
            .start(&[(
                "request",
                htlk_executable::cbor::Value::Text("Question".into()),
            )])
            .await
            .unwrap();
        session.wait().await.unwrap();
        let runtime = session.runtime.clone();
        let context = session.context.clone();
        let run = session.run.clone().unwrap();
        let result: Result<()> = session.finish(Err(Navigation::Quit.into())).await;
        assert_eq!(
            result.unwrap_err().downcast_ref::<Navigation>(),
            Some(&Navigation::Quit)
        );
        let view = runtime.inspect_run(&context, &run).await.unwrap();
        assert_eq!(
            view.header.status,
            htlk::runtime::records::RunStatus::Cancelled
        );
        assert!(view.waits.is_empty());
    });
}
