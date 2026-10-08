//! Combinatorial readiness rules: stage × no-diff tag × context files ×
//! complexity, with severities, plus the statuses readiness does not describe.

use crate::task::{
    NO_DIFF_EXPECTED_TAG, ReadinessGapCode, ReadinessSeverity, ReadinessStage, Task,
    TaskComplexity, TaskStatus, readiness_gaps, task_readiness, task_readiness_json,
};

use ReadinessGapCode::{MissingContextFiles, UnassessedComplexity};
use ReadinessSeverity::{Advisory, Blocking};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

#[test]
fn gap_matrix_matches_the_approval_and_admission_rules() {
    let complexities = [
        None,
        Some(TaskComplexity::Unassessed),
        Some(TaskComplexity::Medium),
    ];
    for stage in [ReadinessStage::Proposed, ReadinessStage::Backlog] {
        for no_diff in [false, true] {
            let tags = if no_diff {
                strings(&["bug", NO_DIFF_EXPECTED_TAG])
            } else {
                strings(&["bug"])
            };
            for context in [strings(&[]), strings(&["file:src/lib.rs"])] {
                for complexity in complexities {
                    let assessed = complexity.is_some_and(TaskComplexity::is_assessed);
                    let mut expected = Vec::new();
                    if !no_diff && context.is_empty() {
                        let severity = match stage {
                            ReadinessStage::Proposed => Blocking,
                            ReadinessStage::Backlog => Advisory,
                        };
                        expected.push((MissingContextFiles, severity));
                    }
                    if !no_diff && !assessed {
                        expected.push((UnassessedComplexity, Blocking));
                    }
                    let actual: Vec<_> = readiness_gaps(stage, &tags, &context, complexity)
                        .into_iter()
                        .map(|gap| {
                            assert!(!gap.message.is_empty() && !gap.fix.is_empty());
                            (gap.code, gap.severity)
                        })
                        .collect();
                    assert_eq!(
                        actual, expected,
                        "{stage:?} no_diff={no_diff} context={context:?} complexity={complexity:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn readiness_is_reported_only_for_proposed_and_backlog_and_ready_ignores_advisory_gaps() {
    let mut task = serde_yaml::from_str::<Task>(
        "id: ORB-1\ntitle: Readiness\ndescription: Fixture.\nacceptance_criteria: []\n\
         dependencies: []\nplan: \"\"\nexecution_summary: \"\"\ncontext_files: []\nstatus: backlog\n\
         priority: medium\ncomplexity: low\ntask_type: chore\n\
         created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n",
    )
    .expect("fixture task deserializes");
    assert!(task.context_files.is_empty());
    for status in [
        TaskStatus::Someday,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Blocked,
        TaskStatus::Archived,
        TaskStatus::Rejected,
    ] {
        task.status = status;
        assert!(task_readiness(&task).is_none(), "{status}");
        assert!(task_readiness_json(&task).is_none(), "{status}");
    }

    task.status = TaskStatus::Backlog;
    let backlog = task_readiness(&task).expect("backlog readiness");
    assert!(
        backlog.ready(),
        "an advisory gap alone leaves the task ready"
    );
    assert_eq!(backlog.gaps.len(), 1);

    task.status = TaskStatus::Proposed;
    let json = task_readiness_json(&task).expect("proposed readiness");
    assert_eq!(json["ready"], false);
    assert_eq!(json["gaps"][0]["code"], "missing_context_files");
    assert_eq!(json["gaps"][0]["severity"], "blocking");
    assert!(json["gaps"][0]["message"].is_string());
    assert!(json["gaps"][0]["fix"].is_string());
}
