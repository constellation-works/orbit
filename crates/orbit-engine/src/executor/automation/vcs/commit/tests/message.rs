use super::super::message::batch_commit_message;
use super::test_support::task_with_file;
use crate::executor::automation::vcs::delivery_marker::delivery_markers;

// Exercise the producer/parser seam for the truncated-bracket regression.
#[test]
fn delivery_markers_survive_a_title_truncated_inside_brackets() {
    let title = format!("Fix [see {}]", "é".repeat(200));
    let task = task_with_file("ORB-5", &title, "README.md", "codex");
    let message = batch_commit_message(&task);
    let (subject, body) = message
        .split_once("\n\n")
        .expect("a truncated title is preserved in the body");
    assert_eq!(subject.matches('[').count(), 2);
    assert_eq!(subject.matches(']').count(), 1);
    assert!(delivery_markers(body).is_empty());
    for fragment in [subject, message.as_str()] {
        assert_eq!(delivery_markers(fragment), ["[ORB-5]"]);
    }
}
