use std::io::Cursor;

use super::super::command::prompt_task_prefix_from;

#[test]
fn task_prefix_prompt_rejects_closed_stdin_without_retrying() {
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();

    let error = prompt_task_prefix_from(&mut input, &mut output).expect_err("closed stdin fails");

    assert!(!error.to_string().is_empty(), "{error}");
    assert!(error.to_string().contains("stdin closed"), "{error}");
    assert_eq!(
        String::from_utf8(output)
            .expect("prompt output is UTF-8")
            .matches("Task prefix")
            .count(),
        1,
        "EOF must not retry the prompt",
    );
}

#[test]
fn task_prefix_prompt_accepts_a_fourth_answer_after_three_invalid_prefixes() {
    let mut input = Cursor::new(b"A\nTOOLONG\nab\nDE\n".to_vec());
    let mut output = Vec::new();

    let prefix = prompt_task_prefix_from(&mut input, &mut output).expect("fourth prefix succeeds");

    assert_eq!(prefix, "DE");
}
