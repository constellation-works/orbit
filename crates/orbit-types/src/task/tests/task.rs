mod external_ref {
    use crate::task::TaskError;
    use crate::task::{ExternalRef, push_external_ref_if_missing};

    #[test]
    fn external_ref_try_new_normalizes_valid_input() {
        let external_ref = ExternalRef::try_new(
            " jira ".to_string(),
            " ENG-1234 ".to_string(),
            Some(" https://example.com/browse/ENG-1234 ".to_string()),
        )
        .expect("valid external ref");

        assert_eq!(external_ref.system, "jira");
        assert_eq!(external_ref.id, "ENG-1234");
        assert_eq!(
            external_ref.url.as_deref(),
            Some("https://example.com/browse/ENG-1234")
        );
    }

    #[test]
    fn external_ref_rejects_invalid_system() {
        let error =
            ExternalRef::try_new("Jira".to_string(), "ENG-1234".to_string(), None).unwrap_err();

        assert!(matches!(error, TaskError::Invalid(_)));
        assert!(error.to_string().contains("must match"));
    }

    #[test]
    fn external_ref_validate_system_normalizes_valid_input() {
        assert!(ExternalRef::is_valid_system(" jira "));
        assert_eq!(
            ExternalRef::validate_system(" github-pr ").expect("valid system"),
            "github-pr"
        );
        assert!(ExternalRef::validate_system("GitHub").is_err());
    }

    #[test]
    fn external_ref_rejects_empty_id() {
        let error = ExternalRef::try_new("jira".to_string(), "   ".to_string(), None).unwrap_err();

        assert!(matches!(error, TaskError::Invalid(_)));
        assert!(error.to_string().contains("id must not be empty"));
    }

    #[test]
    fn external_ref_rejects_invalid_url() {
        let error = ExternalRef::try_new(
            "jira".to_string(),
            "ENG-1234".to_string(),
            Some("not a url".to_string()),
        )
        .unwrap_err();

        assert!(matches!(error, TaskError::Invalid(_)));
        assert!(error.to_string().contains("valid URL"));
    }

    #[test]
    fn external_ref_deserialization_uses_validator() {
        let error = serde_json::from_value::<ExternalRef>(serde_json::json!({
            "system": "jira",
            "id": "ENG-1234",
            "url": "not a url"
        }))
        .unwrap_err();

        assert!(error.to_string().contains("valid URL"));
    }

    #[test]
    fn push_external_ref_if_missing_is_idempotent_by_key() {
        let mut refs = vec![ExternalRef::github_pr("42").expect("github pr ref")];

        push_external_ref_if_missing(
            &mut refs,
            ExternalRef::github_pr("42").expect("duplicate github pr ref"),
        );
        push_external_ref_if_missing(
            &mut refs,
            ExternalRef::parse_key("jira:ENG-1234").expect("jira ref"),
        );

        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].system, "github-pr");
        assert_eq!(refs[0].id, "42");
        assert_eq!(refs[1].system, "jira");
        assert_eq!(refs[1].id, "ENG-1234");
    }
}

mod serialization {
    use crate::task::{Task, TaskStatus, normalize_task_tags};

    #[test]
    fn task_deserializes_missing_collection_fields_as_empty_vecs() {
        let task = serde_yaml::from_str::<Task>(
            r#"id: T20260101-1
title: Legacy task
description: Existing task record.
acceptance_criteria: []
dependencies: []
plan: ""
execution_summary: ""
context_files: []
status: backlog
priority: medium
task_type: chore
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
"#,
        )
        .expect("task without tags deserializes");

        assert_eq!(task.tags, Vec::<String>::new());
        assert_eq!(task.required_tools, Vec::<String>::new());
        assert_eq!(task.crew, None);
    }

    #[test]
    fn task_round_trips_with_crew_set() {
        let task = serde_yaml::from_str::<Task>(
            r#"id: T20260101-1
title: Crew task
description: Existing task record.
acceptance_criteria: []
dependencies: []
plan: ""
execution_summary: ""
context_files: []
status: backlog
priority: medium
task_type: chore
crew: codex
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
"#,
        )
        .expect("task with crew deserializes");

        let serialized = serde_yaml::to_string(&task).expect("serialize task");
        let reparsed = serde_yaml::from_str::<Task>(&serialized).expect("reparse task");

        assert_eq!(reparsed, task);
        assert_eq!(reparsed.crew.as_deref(), Some("codex"));
    }

    #[test]
    fn normalize_task_tags_trims_lowercases_and_dedupes() {
        let tags = normalize_task_tags(vec![
            "  Perf ".to_string(),
            "BENCH".to_string(),
            "perf".to_string(),
            "   ".to_string(),
        ]);

        assert_eq!(tags, vec!["perf", "bench"]);
    }

    #[test]
    fn task_deserializes_missing_complexity_as_none() {
        let task = serde_yaml::from_str::<Task>(
            r#"id: T20260101-1
title: Legacy unlabeled task
description: Existing task record.
acceptance_criteria: []
dependencies: []
plan: ""
execution_summary: ""
context_files: []
status: backlog
priority: medium
task_type: chore
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
"#,
        )
        .expect("task without complexity deserializes");

        assert_eq!(task.complexity, None);
    }

    #[test]
    fn task_complexity_unassessed_round_trips_and_is_not_assessed() {
        use crate::task::TaskComplexity;
        use std::str::FromStr;

        assert_eq!(
            TaskComplexity::from_str("unassessed").expect("parse"),
            TaskComplexity::Unassessed
        );
        assert_eq!(TaskComplexity::Unassessed.to_string(), "unassessed");
        assert!(!TaskComplexity::Unassessed.is_assessed());
        assert!(TaskComplexity::Low.is_assessed());
        assert!(TaskComplexity::Unassessed.require_assessed().is_err());
        assert_eq!(
            TaskComplexity::Hard.require_assessed().expect("assessed"),
            TaskComplexity::Hard
        );
    }

    #[test]
    fn task_status_deserializes_both_hyphen_and_snake_for_in_progress() {
        let snake: TaskStatus = serde_json::from_str("\"in_progress\"").expect("snake de");
        let hyphen: TaskStatus = serde_json::from_str("\"in-progress\"").expect("hyphen de");
        assert_eq!(snake, TaskStatus::InProgress);
        assert_eq!(hyphen, TaskStatus::InProgress);
        // serialize remains snake_case for persisted history/events compat with prior records
        assert_eq!(
            serde_json::to_string(&TaskStatus::InProgress).expect("ser"),
            "\"in_progress\""
        );
    }

    #[test]
    fn task_status_rejects_removed_friction_variant() {
        assert!(serde_json::from_str::<TaskStatus>("\"friction\"").is_err());
        assert!("friction".parse::<TaskStatus>().is_err());
    }
}

mod dependency_satisfaction {
    use crate::task::{
        DependencyDeadEnd, TASK_REFERENCE_NOT_VERIFIABLE_HERE, Task, TaskReferenceIndex,
        TaskStatus, resolve_task_dependencies, resolve_task_dependencies_with_index,
        resolve_task_relations_with_index, task_dependencies_ready,
        task_dependencies_ready_with_index, unmet_task_dependencies,
        unmet_task_dependencies_with_index, unsatisfiable_task_dependencies,
        unsatisfiable_task_dependencies_with_index,
    };
    use std::collections::BTreeMap;

    /// `Task::dependencies()` reads `blocked_by` *relations*, not the legacy
    /// `dependencies` field, so the fixture must declare them that way.
    fn task_with_dependencies(id: &str, dependencies: &[&str]) -> Task {
        let relations_yaml = dependencies
            .iter()
            .map(|dependency| format!("  - type: blocked_by\n    target: {dependency}\n"))
            .collect::<String>();
        let mut task = serde_yaml::from_str::<Task>(&format!(
            r#"id: {id}
title: Dependent task
description: Fixture.
acceptance_criteria: []
dependencies: []
plan: ""
execution_summary: ""
context_files: []
status: backlog
priority: medium
task_type: chore
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
relations:
{relations_yaml}"#
        ))
        .expect("fixture task deserializes");
        assert_eq!(task.dependencies().len(), dependencies.len());
        task.relations.sort_by(|a, b| a.target.cmp(&b.target));
        task
    }

    fn status_index(entries: &[(&str, TaskStatus)]) -> BTreeMap<String, TaskStatus> {
        entries
            .iter()
            .map(|(id, status)| ((*id).to_string(), *status))
            .collect()
    }

    #[test]
    fn only_archived_and_rejected_are_dead_ends() {
        assert_eq!(
            TaskStatus::Archived.dependency_dead_end(),
            Some(DependencyDeadEnd::Archived)
        );
        assert_eq!(
            TaskStatus::Rejected.dependency_dead_end(),
            Some(DependencyDeadEnd::Rejected)
        );
        for status in [
            TaskStatus::Proposed,
            TaskStatus::Backlog,
            TaskStatus::InProgress,
            TaskStatus::Review,
            TaskStatus::Done,
            TaskStatus::Blocked,
            TaskStatus::Someday,
        ] {
            assert_eq!(
                status.dependency_dead_end(),
                None,
                "{status} must remain a legitimate wait"
            );
        }
    }

    #[test]
    fn done_only_still_satisfies_a_dependency() {
        // Guardrail: this task made dead ends fail loudly; it must not have
        // widened what counts as satisfied.
        for status in [
            TaskStatus::Proposed,
            TaskStatus::Backlog,
            TaskStatus::InProgress,
            TaskStatus::Review,
            TaskStatus::Blocked,
            TaskStatus::Archived,
            TaskStatus::Rejected,
            TaskStatus::Someday,
        ] {
            assert!(
                !status.satisfies_dependency(),
                "{status} must not satisfy a dependency"
            );
        }
        assert!(TaskStatus::Done.satisfies_dependency());
    }

    #[test]
    fn archived_dependency_is_unsatisfiable() {
        let task = task_with_dependencies("ORB-2", &["ORB-1"]);
        let statuses = status_index(&[("ORB-1", TaskStatus::Archived)]);

        let unsatisfiable = unsatisfiable_task_dependencies(&task, &statuses);

        assert_eq!(unsatisfiable.len(), 1);
        assert_eq!(unsatisfiable[0].task_id, "ORB-2");
        assert_eq!(unsatisfiable[0].dependency_id, "ORB-1");
        assert_eq!(unsatisfiable[0].status, "archived");
        assert_eq!(unsatisfiable[0].reason, DependencyDeadEnd::Archived);
        assert!(unsatisfiable[0].label().contains("ORB-2 blocked_by ORB-1"));
    }

    #[test]
    fn rejected_dependency_is_unsatisfiable() {
        let task = task_with_dependencies("ORB-2", &["ORB-1"]);
        let statuses = status_index(&[("ORB-1", TaskStatus::Rejected)]);

        let unsatisfiable = unsatisfiable_task_dependencies(&task, &statuses);

        assert_eq!(unsatisfiable.len(), 1);
        assert_eq!(unsatisfiable[0].reason, DependencyDeadEnd::Rejected);
    }

    #[test]
    fn dangling_dependency_is_unsatisfiable() {
        let task = task_with_dependencies("ORB-2", &["ORB-404"]);
        let statuses = status_index(&[]);

        let unsatisfiable = unsatisfiable_task_dependencies(&task, &statuses);

        assert_eq!(unsatisfiable.len(), 1);
        assert_eq!(unsatisfiable[0].dependency_id, "ORB-404");
        assert_eq!(unsatisfiable[0].status, "missing");
        assert_eq!(unsatisfiable[0].reason, DependencyDeadEnd::Missing);
    }

    #[test]
    fn foreign_dependency_is_marked_ready_and_not_a_dead_end() {
        let task = task_with_dependencies("ORB-2", &["DK-404"]);
        let statuses = status_index(&[("ORB-2", TaskStatus::Backlog)]);

        assert_eq!(
            resolve_task_dependencies(&task, &statuses)[0].status,
            TASK_REFERENCE_NOT_VERIFIABLE_HERE
        );
        assert!(task_dependencies_ready(&task, &statuses));
        assert!(unmet_task_dependencies(&task, &statuses).is_empty());
        assert!(unsatisfiable_task_dependencies(&task, &statuses).is_empty());
    }

    #[test]
    fn shared_reference_index_preserves_reference_classification_across_helpers() {
        let task = task_with_dependencies(
            "ORB-9000",
            &[
                "ORB-1",
                "ORB-404",
                "LEGACY-404",
                "OTHER-404",
                "not-a-task-id",
            ],
        );
        let statuses = status_index(&[
            ("ORB-1", TaskStatus::Done),
            ("LEGACY-1", TaskStatus::Backlog),
        ]);
        let reference_index = TaskReferenceIndex::from_status_index(&statuses);

        assert_eq!(reference_index.indexed_task_count(), statuses.len());
        assert_eq!(
            resolve_task_dependencies_with_index(&task, &statuses, &reference_index)
                .into_iter()
                .map(|dependency| dependency.status)
                .collect::<Vec<_>>(),
            vec![
                "missing".to_string(),
                "done".to_string(),
                "missing".to_string(),
                TASK_REFERENCE_NOT_VERIFIABLE_HERE.to_string(),
                "missing".to_string(),
            ]
        );
        assert_eq!(
            resolve_task_relations_with_index(&task, &statuses, &reference_index)
                .into_iter()
                .filter_map(|relation| relation.verification)
                .collect::<Vec<_>>(),
            vec![TASK_REFERENCE_NOT_VERIFIABLE_HERE.to_string()]
        );
        assert!(!task_dependencies_ready_with_index(
            &task,
            &statuses,
            &reference_index
        ));
        assert_eq!(
            unmet_task_dependencies_with_index(&task, &statuses, &reference_index).len(),
            3
        );
        assert_eq!(
            unsatisfiable_task_dependencies_with_index(&task, &statuses, &reference_index).len(),
            3
        );
    }

    #[test]
    fn shared_reference_index_scans_a_snapshot_once_for_large_mixed_batches() {
        const INDEXED_TASKS: usize = 1_000;
        const UNRESOLVED_REFERENCES: usize = 2_000;

        let statuses = (0..INDEXED_TASKS)
            .map(|number| (format!("ORB-{number}"), TaskStatus::Done))
            .collect::<BTreeMap<_, _>>();
        let dependencies = (0..UNRESOLVED_REFERENCES)
            .map(|number| {
                if number % 10 == 0 {
                    format!("ORB-{}", INDEXED_TASKS + number)
                } else {
                    format!("EXT-{number}")
                }
            })
            .collect::<Vec<_>>();
        let dependency_refs = dependencies.iter().map(String::as_str).collect::<Vec<_>>();
        let task = task_with_dependencies("ORB-9000", &dependency_refs);

        let reference_index = TaskReferenceIndex::from_status_index(&statuses);
        let resolved = resolve_task_dependencies_with_index(&task, &statuses, &reference_index);

        // The old path rebuilt this set for every unresolved valid reference:
        // 2,000,000 key examinations here. This snapshot indexes 1,000 keys
        // once, independent of the 2,000 relation checks below.
        assert_eq!(reference_index.indexed_task_count(), INDEXED_TASKS);
        assert_eq!(resolved.len(), UNRESOLVED_REFERENCES);
        assert_eq!(
            resolved
                .iter()
                .filter(|dependency| dependency.status == "missing")
                .count(),
            UNRESOLVED_REFERENCES / 10
        );
        assert_eq!(
            resolved
                .iter()
                .filter(|dependency| dependency.status == TASK_REFERENCE_NOT_VERIFIABLE_HERE)
                .count(),
            UNRESOLVED_REFERENCES - (UNRESOLVED_REFERENCES / 10)
        );
    }

    #[test]
    fn shared_reference_index_has_bounded_work_for_mostly_resolved_batches() {
        const INDEXED_TASKS: usize = 1_000;
        const RESOLVED_REFERENCES: usize = 900;
        const LOCAL_MISSING_REFERENCES: usize = 50;
        const FOREIGN_REFERENCES: usize = 50;

        let statuses = (0..INDEXED_TASKS)
            .map(|number| (format!("ORB-{number}"), TaskStatus::Done))
            .collect::<BTreeMap<_, _>>();
        let dependencies = (0..RESOLVED_REFERENCES)
            .map(|number| format!("ORB-{number}"))
            .chain(
                (0..LOCAL_MISSING_REFERENCES)
                    .map(|number| format!("ORB-{}", INDEXED_TASKS + number)),
            )
            .chain((0..FOREIGN_REFERENCES).map(|number| format!("EXT-{number}")))
            .collect::<Vec<_>>();
        let dependency_refs = dependencies.iter().map(String::as_str).collect::<Vec<_>>();
        let task = task_with_dependencies("ORB-9000", &dependency_refs);

        let reference_index = TaskReferenceIndex::from_status_index(&statuses);
        let resolved = resolve_task_dependencies_with_index(&task, &statuses, &reference_index);

        // Before: the 100 unresolved valid references would examine 100,000
        // keys. After: this bounded snapshot examines 1,000 keys once; the
        // 900 known targets retain their status-map fast path.
        assert_eq!(reference_index.indexed_task_count(), INDEXED_TASKS);
        assert_eq!(
            resolved
                .iter()
                .filter(|dependency| dependency.status == "done")
                .count(),
            RESOLVED_REFERENCES
        );
        assert_eq!(
            resolved
                .iter()
                .filter(|dependency| dependency.status == "missing")
                .count(),
            LOCAL_MISSING_REFERENCES
        );
        assert_eq!(
            resolved
                .iter()
                .filter(|dependency| dependency.status == TASK_REFERENCE_NOT_VERIFIABLE_HERE)
                .count(),
            FOREIGN_REFERENCES
        );
    }

    #[test]
    fn in_flight_dependency_is_unmet_but_not_unsatisfiable() {
        let task = task_with_dependencies("ORB-2", &["ORB-1"]);
        for status in [
            TaskStatus::Backlog,
            TaskStatus::Proposed,
            TaskStatus::InProgress,
            TaskStatus::Review,
        ] {
            let statuses = status_index(&[("ORB-1", status)]);

            assert!(
                unsatisfiable_task_dependencies(&task, &statuses).is_empty(),
                "{status} must not fail dispatch fast"
            );
            assert_eq!(
                unmet_task_dependencies(&task, &statuses).len(),
                1,
                "{status} must still be reported as an unmet wait"
            );
        }
    }

    #[test]
    fn satisfied_dependency_is_neither_unmet_nor_unsatisfiable() {
        let task = task_with_dependencies("ORB-2", &["ORB-1"]);
        let statuses = status_index(&[("ORB-1", TaskStatus::Done)]);

        assert!(unsatisfiable_task_dependencies(&task, &statuses).is_empty());
        assert!(unmet_task_dependencies(&task, &statuses).is_empty());
    }

    #[test]
    fn mixed_edges_report_only_the_dead_ends_as_unsatisfiable() {
        let task = task_with_dependencies("ORB-4", &["ORB-1", "ORB-2", "ORB-3"]);
        let statuses = status_index(&[
            ("ORB-1", TaskStatus::Backlog),
            ("ORB-2", TaskStatus::Archived),
            ("ORB-3", TaskStatus::Done),
        ]);

        let unsatisfiable = unsatisfiable_task_dependencies(&task, &statuses);

        assert_eq!(unsatisfiable.len(), 1);
        assert_eq!(unsatisfiable[0].dependency_id, "ORB-2");
        // The plain unmet rollup is unchanged: it still reports both.
        let unmet: Vec<String> = unmet_task_dependencies(&task, &statuses)
            .into_iter()
            .map(|dependency| dependency.id)
            .collect();
        assert_eq!(unmet, vec!["ORB-1".to_string(), "ORB-2".to_string()]);
    }
}

mod unlabeled_bucket {
    use crate::task::{UNSET_BUCKET, complexity_bucket, complexity_bucket_ord, labeled_or_unset};

    #[test]
    fn empty_and_missing_labels_become_unset() {
        assert_eq!(labeled_or_unset(None), UNSET_BUCKET);
        assert_eq!(labeled_or_unset(Some("")), UNSET_BUCKET);
        assert_eq!(labeled_or_unset(Some("   ")), UNSET_BUCKET);
        assert_eq!(labeled_or_unset(Some("hard")), "hard");
    }

    #[test]
    fn unassessed_and_missing_complexity_share_one_bucket() {
        assert_eq!(complexity_bucket(None), UNSET_BUCKET);
        assert_eq!(complexity_bucket(Some("")), UNSET_BUCKET);
        assert_eq!(complexity_bucket(Some("unassessed")), UNSET_BUCKET);
        assert_eq!(complexity_bucket(Some("hard")), "hard");
    }

    #[test]
    fn unexpected_labels_are_not_folded_into_a_known_band() {
        assert_eq!(labeled_or_unset(Some("extreme")), "extreme");
        assert_eq!(complexity_bucket(Some("extreme")), "extreme");
        assert!(complexity_bucket_ord("unset") < complexity_bucket_ord("low"));
        assert!(complexity_bucket_ord("hard") < complexity_bucket_ord("extreme"));
    }
}
