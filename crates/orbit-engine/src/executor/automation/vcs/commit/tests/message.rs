use orbit_types::task::{
    DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature, SeveritySource,
    Task, TaskRelation, TaskRelationType,
};

use super::super::message::{
    RegressionLandedCommit, RegressionLandedCommits, batch_commit_message, finalize_commit_message,
    task_commit_message,
};
use super::test_support::task_with_file;
use crate::context::PrConfig;
use crate::executor::automation::vcs::delivery_marker::delivery_markers;

const LANDED_SHA: &str = "5cac195ba9abcdef0123456789abcdef01234567";
const LANDED_SUBJECT: &str =
    "Persist declared script output files as durable bounded run artifacts";

fn classified_relation(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::RegressionFrom,
        target: target.to_string(),
        regression: Some(RegressionClassification {
            nature: RegressionNature::DefectInNew,
            escape: RegressionEscape::NoTest,
            severity: DefectSeverity::Medium,
            severity_source: SeveritySource::Filer,
            sites: Vec::new(),
        }),
    }
}

fn task_with_relations(id: &str, relations: Vec<TaskRelation>) -> Task {
    let mut task = task_with_file(id, "Fix the regression", "README.md", "codex");
    task.relations = relations;
    task
}

fn landed_for(target: &str) -> RegressionLandedCommits {
    RegressionLandedCommits::from([(
        target.to_string(),
        RegressionLandedCommit {
            sha: LANDED_SHA.to_string(),
            subject: LANDED_SUBJECT.to_string(),
        },
    )])
}

fn expected_group() -> String {
    format!(
        "Fixes: 5cac195ba9ab (\"{LANDED_SUBJECT}\")\n\
         Regression-From: ORB-20866\n\
         Regression: defect_in_new; escape=no_test; severity=medium"
    )
}

// Exercise the producer/parser seam for the truncated-bracket regression.
#[test]
fn delivery_markers_survive_a_title_truncated_inside_brackets() {
    let title = format!("Fix [see {}]", "é".repeat(200));
    let task = task_with_file("ORB-5", &title, "README.md", "codex");
    let message = batch_commit_message(
        &task,
        &crate::context::PrConfig::default(),
        &RegressionLandedCommits::new(),
    );
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

#[test]
fn regression_trailers_on_task_batch_and_finalize_paths() {
    let task = task_with_relations("ORB-9", vec![classified_relation("ORB-20866")]);
    let landed = landed_for("ORB-20866");
    let group = expected_group();
    let config = PrConfig::default();

    let task_message = task_commit_message(&task, &config, &landed);
    assert!(
        task_message.contains(&group),
        "task message: {task_message}"
    );

    let batch_message = batch_commit_message(&task, &config, &landed);
    assert!(
        batch_message.contains(&group),
        "batch message: {batch_message}"
    );
    // Next to the existing attribution trailers, after Implemented-By.
    let implemented = batch_message
        .find("Implemented-By: codex")
        .expect("batch message carries Implemented-By");
    let fixes = batch_message.find("Fixes:").expect("batch carries Fixes");
    assert!(implemented < fixes, "batch message: {batch_message}");

    let finalize_message = finalize_commit_message(std::slice::from_ref(&task), &config, &landed);
    assert!(
        finalize_message.contains(&group),
        "finalize message: {finalize_message}"
    );
}

#[test]
fn regression_trailers_two_relations_in_order_and_missing_evidence() {
    let unclassified = TaskRelation {
        relation_type: TaskRelationType::RegressionFrom,
        target: "ORB-30000".to_string(),
        regression: None,
    };
    let task = task_with_relations(
        "ORB-9",
        vec![classified_relation("ORB-20866"), unclassified],
    );
    // Only the first target has a resolvable landed commit.
    let message = batch_commit_message(&task, &PrConfig::default(), &landed_for("ORB-20866"));
    let first = message
        .find("Regression-From: ORB-20866")
        .expect("first group present");
    let second = message
        .find("Regression-From: ORB-30000")
        .expect("second group present");
    assert!(first < second, "groups in relation order: {message}");
    assert_eq!(message.matches("Fixes:").count(), 1, "message: {message}");
    assert_eq!(
        message.matches("Regression:").count(),
        1,
        "unclassified relation gets no Regression line: {message}"
    );
    assert!(
        message.contains("Regression-From: ORB-30000"),
        "unlanded target keeps Regression-From: {message}"
    );
}

#[test]
fn regression_trailers_disabled_by_setting() {
    let task = task_with_relations("ORB-9", vec![classified_relation("ORB-20866")]);
    let mut config = PrConfig::default();
    config.conventions.regression_trailers = false;
    let landed = landed_for("ORB-20866");
    for message in [
        task_commit_message(&task, &config, &landed),
        batch_commit_message(&task, &config, &landed),
        finalize_commit_message(std::slice::from_ref(&task), &config, &landed),
    ] {
        assert!(!message.contains("Fixes:"), "message: {message}");
        assert!(!message.contains("Regression-From:"), "message: {message}");
        assert!(!message.contains("Regression:"), "message: {message}");
    }
}

#[test]
fn no_regression_relations_leave_message_unchanged() {
    let task = task_with_file("ORB-5", "Fix the widget", "README.md", "codex");
    let message =
        batch_commit_message(&task, &PrConfig::default(), &RegressionLandedCommits::new());
    assert!(!message.contains("Fixes:"), "message: {message}");
    assert!(!message.contains("Regression-From:"), "message: {message}");
    assert!(
        message.contains("Implemented-By: codex"),
        "message: {message}"
    );
}
