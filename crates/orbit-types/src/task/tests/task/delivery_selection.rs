use crate::task::{TaskError, delivery_job_selection};

fn tags(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

#[test]
fn a_delivery_tag_selects_its_job_and_other_tags_select_nothing() {
    assert_eq!(
        delivery_job_selection(&tags(&["orbit-research", "bug"])),
        Ok(None)
    );
    assert_eq!(
        delivery_job_selection(&tags(&[
            "orbit-research",
            "delivery:research_investigation"
        ])),
        Ok(Some("research_investigation"))
    );
}

#[test]
fn a_repeated_selection_is_one_selection() {
    assert_eq!(
        delivery_job_selection(&tags(&[
            "delivery:research_investigation",
            "delivery:research_investigation",
        ])),
        Ok(Some("research_investigation"))
    );
}

#[test]
fn conflicting_or_empty_selections_are_refused_rather_than_ordered() {
    let conflict = delivery_job_selection(&tags(&["delivery:first_job", "delivery:second_job"]))
        .expect_err("two jobs are ambiguous");
    assert!(matches!(&conflict, TaskError::Invalid(message)
        if message.contains("first_job") && message.contains("second_job")));

    let empty = delivery_job_selection(&tags(&["delivery:"])).expect_err("no job named");
    assert!(matches!(empty, TaskError::Invalid(_)));
}
