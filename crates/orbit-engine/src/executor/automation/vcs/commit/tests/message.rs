use orbit_types::task::{
    DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature, SeveritySource,
    Task, TaskRelation, TaskRelationType,
};

use super::super::message::{
    RegressionLandedCommit, RegressionLandedCommits, delivery_commit_message,
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

fn message_for(tasks: &[Task], config: &PrConfig, landed: &RegressionLandedCommits) -> String {
    delivery_commit_message(tasks, config, landed, &[])
}

#[test]
fn delivery_markers_survive_a_title_truncated_inside_brackets() {
    let title = format!("Fix [see {}]", "é".repeat(200));
    let task = task_with_file("ORB-5", &title, "README.md", "codex");
    let message = message_for(
        std::slice::from_ref(&task),
        &PrConfig::default(),
        &RegressionLandedCommits::new(),
    );
    let subject = message.lines().next().expect("subject");
    assert!(!subject.contains('…'), "{subject}");
    assert_eq!(delivery_markers(subject), ["[ORB-5]"]);
    assert_eq!(delivery_markers(&message), ["[ORB-5]"]);
}

#[test]
fn regression_trailers_on_task_batch_and_finalize_paths() {
    let mut task = task_with_relations("ORB-9", vec![classified_relation("ORB-20866")]);
    task.planned_by = Some("planner".to_string());
    task.orchestrator = Some("crew".to_string());
    let landed = landed_for("ORB-20866");
    let group = format!(
        "Fixes: 5cac195ba9ab (\"{LANDED_SUBJECT}\")\n\
         Regression-From: ORB-20866\n\
         Regression: defect_in_new; escape=no_test; severity=medium"
    );
    let config = PrConfig::default();
    let other = task_with_file("ORB-10", "Other", "b.rs", "claude");
    for message in [
        message_for(std::slice::from_ref(&task), &config, &landed),
        message_for(&[task.clone(), other], &config, &landed),
    ] {
        assert!(message.contains(&group), "message: {message}");
        assert!(!message.contains("Planned-By:"), "{message}");
        assert!(!message.contains("Implemented-By:"), "{message}");
        assert!(message.contains("Orchestrated-By: crew"), "{message}");
    }
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
    let message = message_for(
        std::slice::from_ref(&task),
        &PrConfig::default(),
        &landed_for("ORB-20866"),
    );
    let first = message
        .find("Regression-From: ORB-20866")
        .expect("first relation present");
    let second = message
        .find("Regression-From: ORB-30000")
        .expect("second relation present");
    assert!(first < second, "relations in order: {message}");
    assert_eq!(message.matches("Fixes:").count(), 1, "message: {message}");
    assert_eq!(
        message.matches("Regression:").count(),
        1,
        "unclassified relation gets no Regression line: {message}"
    );
}

#[test]
fn regression_trailers_disabled_by_setting() {
    let task = task_with_relations("ORB-9", vec![classified_relation("ORB-20866")]);
    let mut config = PrConfig::default();
    config.conventions.regression_trailers = false;
    let message = message_for(
        std::slice::from_ref(&task),
        &config,
        &landed_for("ORB-20866"),
    );
    assert!(!message.contains("Fixes:"), "message: {message}");
    assert!(!message.contains("Regression-From:"), "message: {message}");
    assert!(!message.contains("Regression:"), "message: {message}");
}
