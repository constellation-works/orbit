use std::io::Cursor;

use super::super::command::{minted_prefix_conflict, prompt_task_prefix_from};

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

#[test]
fn a_prefix_that_contradicts_minted_ids_is_refused_with_the_way_forward() {
    let message = minted_prefix_conflict(Some(("ORB".to_string(), 3)), Some("QA"))
        .expect("QA cannot replace the ORB ids already minted");
    assert!(message.contains("'QA'"), "{message}");
    assert!(message.contains("3 task id(s)"), "{message}");
    assert!(message.contains("Nothing was written"), "{message}");
    assert!(
        message.contains("cannot be chosen for a new identity"),
        "ORB can never be adopted, so no flag is suggested: {message}"
    );

    let adoptable = minted_prefix_conflict(Some(("DE".to_string(), 7)), None)
        .expect("an unnamed prefix cannot be assumed");
    assert!(adoptable.contains("--task-prefix DE"), "{adoptable}");
}

#[test]
fn a_pristine_store_or_the_prefix_already_in_use_is_not_a_conflict() {
    assert_eq!(minted_prefix_conflict(None, Some("QA")), None);
    assert_eq!(
        minted_prefix_conflict(Some(("DE".to_string(), 7)), Some("DE")),
        None
    );
}
