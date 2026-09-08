use super::*;
use orbit_types::task::{TaskComplexity, UNSET_BUCKET};

#[test]
fn completion_by_complexity_includes_unset_and_backfills_null_rows() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);

    let mut hard = create_params("Hard landed", TaskStatus::Done);
    hard.complexity = Some(TaskComplexity::Hard);
    store.create_task(hard).expect("create hard");
    store
        .create_task(create_params("Unlabeled", TaskStatus::Rejected))
        .expect("create unset");

    let rows = store
        .task_completion_by_complexity()
        .expect("aggregate after write");
    assert!(
        rows.iter()
            .any(|row| row.complexity == "hard" && row.total == 1),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.complexity == UNSET_BUCKET && row.total == 1),
        "{rows:?}"
    );

    let conn = rusqlite::Connection::open(task_registry_path(temp.path())).expect("open registry");
    conn.execute("UPDATE task_bundle_index SET complexity = NULL", [])
        .expect("clear indexed complexity");
    drop(conn);

    let backfilled = store
        .task_completion_by_complexity()
        .expect("aggregate after backfill");
    assert!(
        backfilled
            .iter()
            .any(|row| row.complexity == "hard" && row.total == 1),
        "{backfilled:?}"
    );
    assert!(
        backfilled
            .iter()
            .any(|row| row.complexity == UNSET_BUCKET && row.total == 1),
        "{backfilled:?}"
    );
}

#[test]
fn create_get_and_list_task_round_trip_through_v2_bundle() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);

    let mut params = create_params("Build task v2", TaskStatus::Backlog);
    params.orchestrator = Some("orchestration".to_string());
    params.required_tools = vec![
        "github.run.list".to_string(),
        "github.auth.status".to_string(),
        "github.run.list".to_string(),
    ];
    let created = store.create_task(params).expect("create task");

    assert_eq!(created.id, "ORB-00000");
    assert_eq!(
        created.acceptance_criteria,
        vec!["First criterion", "Second criterion"]
    );
    assert_eq!(
        store
            .get_task_comments("ORB-00000")
            .expect("get comments")
            .expect("task exists")
            .len(),
        1
    );
    assert_eq!(
        store
            .get_task_history("ORB-00000")
            .expect("get history")
            .expect("task exists")
            .len(),
        1
    );

    let fetched = store
        .get_task("ORB-00000")
        .expect("get task")
        .expect("task exists");
    assert_eq!(fetched.title, created.title);
    assert_eq!(fetched.orchestrator.as_deref(), Some("orchestration"));
    assert_eq!(
        fetched.required_tools,
        vec!["github.auth.status", "github.run.list"]
    );

    let listed = store.list_tasks().expect("list tasks");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "ORB-00000");
    assert_eq!(listed[0].required_tools, fetched.required_tools);
}

#[test]
fn create_task_allocates_globally_monotonic_orb_ids() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);

    let first = store
        .create_task(create_params("First", TaskStatus::Backlog))
        .expect("create first");
    let second = store
        .create_task(create_params("Second", TaskStatus::Backlog))
        .expect("create second");

    assert_eq!(first.id, "ORB-00000");
    assert_eq!(second.id, "ORB-00001");
}

#[test]
fn unrelated_reads_and_creates_survive_a_corrupt_bundle() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let task_a = store
        .create_task(create_params("Readable A", TaskStatus::Backlog))
        .expect("create task A");
    let task_c = store
        .create_task(create_params("Corrupt C", TaskStatus::Backlog))
        .expect("create task C");
    let corrupt_dir = store
        .bundle_store
        .bundle_path(&task_c.id)
        .expect("corrupt bundle path");

    // Review-thread sidecars were retired in ORB-10332. Their absence is a
    // supported legacy/recovery state and reads must not "repair" it by
    // mutating or quarantining the otherwise-valid bundle.
    let retired_review_dir = corrupt_dir.join("review-threads");
    assert!(!retired_review_dir.exists());
    assert_eq!(
        store
            .get_task(&task_a.id)
            .expect("read unrelated task A")
            .expect("task A exists")
            .id,
        task_a.id
    );
    assert!(
        !retired_review_dir.exists(),
        "reading another task must leave the legacy missing sidecar untouched"
    );

    let missing_path = corrupt_dir.join("description.md");
    fs::remove_file(&missing_path).expect("malform task C");
    let task_b = store
        .create_task(create_params("New B", TaskStatus::Backlog))
        .expect("create task B despite unrelated corrupt task C");
    assert_eq!(task_b.id, "ORB-00002");

    for error in [
        store
            .list_tasks()
            .expect_err("list must surface corruption"),
        store
            .search_tasks("anything")
            .expect_err("search must surface corruption"),
    ] {
        assert!(matches!(
            error,
            OrbitError::TaskBundleCorrupt {
                task_id,
                path,
                reason,
            } if task_id == task_c.id
                && path == corrupt_dir.to_string_lossy()
                && reason.contains("description.md")
        ));
    }
    assert!(
        corrupt_dir.is_dir() && !missing_path.exists(),
        "diagnostics must not delete, move, or silently repair the corrupt bundle"
    );
}

#[test]
fn delete_task_removes_v2_bundle_and_index_rows() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Delete me", TaskStatus::Rejected))
        .expect("create task");

    assert!(store.delete_task("ORB-00000").expect("delete task"));
    assert_eq!(store.get_task("ORB-00000").expect("get deleted"), None);
    assert!(store.list_tasks().expect("list tasks").is_empty());
    assert_eq!(
        store
            .registry
            .indexed_task_count_for_workspace(&store.workspace_id)
            .expect("index count"),
        0
    );
    assert!(!store.delete_task("ORB-00000").expect("delete missing"));
}

#[cfg(unix)]
#[test]
fn create_task_on_readonly_parent_dir_names_lock_path_and_hints_sandbox() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("First", TaskStatus::Backlog))
        .expect("create first");
    let next_bundle = store
        .bundle_store
        .bundle_path("ORB-00001")
        .expect("next bundle path");
    let parent = next_bundle.parent().expect("tasks dir").to_path_buf();
    let lock_path = parent.join(".ORB-00001.bundle.lock");
    let _restore = make_readonly(&parent);

    let err = store
        .create_task(create_params("Second", TaskStatus::Backlog))
        .expect_err("create must fail when the tasks dir is read-only");
    assert_sandbox_write_io(&err, &lock_path.display().to_string());
}

#[cfg(unix)]
#[test]
fn delete_task_on_readonly_bundle_dir_names_path_and_hints_sandbox() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Delete me", TaskStatus::Rejected))
        .expect("create task");
    let bundle_dir = store
        .bundle_store
        .bundle_path("ORB-00000")
        .expect("bundle path");
    let _restore = make_readonly(&bundle_dir);

    let err = store
        .delete_task("ORB-00000")
        .expect_err("delete must fail on a read-only bundle dir");
    assert_sandbox_write_io(&err, &bundle_dir.display().to_string());
}

#[test]
fn create_task_persists_lineage_relations() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let parent = store
        .create_task(create_params("Parent", TaskStatus::Backlog))
        .expect("create parent");
    let dependency = store
        .create_task(create_params("Dependency", TaskStatus::Backlog))
        .expect("create dependency");
    let source = store
        .create_task(create_params("Source", TaskStatus::Done))
        .expect("create source");
    let mut params = create_params("Child", TaskStatus::Backlog);
    params.parent_id = Some(parent.id.clone());
    params.dependencies = vec![dependency.id.clone()];
    params.source_task_id = Some(source.id.clone());

    let child = store.create_task(params).expect("create related task");
    assert_eq!(child.parent_id(), Some(parent.id.as_str()));
    assert_eq!(child.dependencies(), vec![dependency.id.clone()]);
    assert_eq!(child.source_task_id(), Some(source.id.as_str()));

    let envelope = store
        .bundle_store
        .read_bundle(&child.id)
        .expect("read related bundle")
        .envelope;
    assert!(envelope.relations.iter().any(|relation| {
        relation.relation_type == TaskRelationType::ChildOf && relation.target == parent.id
    }));
    assert!(envelope.relations.iter().any(|relation| {
        relation.relation_type == TaskRelationType::BlockedBy && relation.target == dependency.id
    }));
    assert!(envelope.relations.iter().any(|relation| {
        relation.relation_type == TaskRelationType::RegressionFrom && relation.target == source.id
    }));
}

#[test]
fn filters_and_searches_v2_tasks() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Backlog task", TaskStatus::Backlog))
        .expect("create backlog");
    let mut review = create_params("Review task", TaskStatus::Review);
    review.tags = vec!["review".to_string()];
    review.description = "Contains a rare search phrase".to_string();
    store.create_task(review).expect("create review");

    assert_eq!(
        store
            .list_tasks_filtered(Some(TaskStatus::Review), None, None, None, None, None,)
            .expect("filtered")
            .into_iter()
            .map(|task| task.title)
            .collect::<Vec<_>>(),
        vec!["Review task"]
    );
    assert_eq!(
        store
            .list_tasks_by_tags(&["task-artifacts".to_string()])
            .expect("tagged")
            .len(),
        1
    );
    assert_eq!(
        store
            .search_tasks("rare search")
            .expect("search")
            .into_iter()
            .map(|task| task.title)
            .collect::<Vec<_>>(),
        vec!["Review task"]
    );
    assert_eq!(
        store
            .search_tasks_filtered("task", &["review".to_string()])
            .expect("search filtered")
            .into_iter()
            .map(|task| task.title)
            .collect::<Vec<_>>(),
        vec!["Review task"]
    );
}

#[test]
fn search_tasks_matches_text_artifacts() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Searchable", TaskStatus::Backlog))
        .expect("create task");
    store
        .upsert_task_artifacts(
            "ORB-00000",
            &TaskArtifactUpdateParams {
                owner_run_id: None,
                actor: "codex:gpt-5.5".to_string(),
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "reports/search.md",
                    "needle-artifact-body\n",
                )],
            },
        )
        .expect("upsert artifact");

    for query in ["needle-artifact-body", "reports/search.md"] {
        assert_eq!(
            store
                .search_tasks(query)
                .expect("search")
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            vec!["ORB-00000"],
            "query {query} should match v2 non-envelope content"
        );
    }
    assert!(
        store
            .search_tasks("definitely-missing-query")
            .expect("search no match")
            .is_empty()
    );
}

#[test]
fn search_tasks_skips_binary_artifacts_without_poisoning_results() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Binary artifact task", TaskStatus::Backlog))
        .expect("create binary task");
    store
        .create_task(create_params("Text artifact task", TaskStatus::Backlog))
        .expect("create text task");

    let at = Utc.with_ymd_and_hms(2026, 5, 11, 15, 0, 0).unwrap();
    let binary = vec![0xff, 0xfe, 0xfd, 0x00];
    let binary_dir = store
        .bundle_store
        .bundle_path("ORB-00000")
        .expect("binary bundle path")
        .join(TASK_ARTIFACTS_DIR_NAME)
        .join(TASK_ARTIFACT_FILES_DIR_NAME);
    std::fs::create_dir_all(&binary_dir).expect("create binary artifact dir");
    std::fs::write(binary_dir.join("payload.bin"), &binary).expect("write binary artifact");
    store
        .bundle_store
        .rewrite_artifact_manifest(
            "ORB-00000",
            &ArtifactManifestV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                files: vec![ArtifactManifestFileV2 {
                    path: "payload.bin".to_string(),
                    blob: "files/payload.bin".to_string(),
                    sha256: format!("{:x}", Sha256::digest(&binary)),
                    media_type: "application/octet-stream".to_string(),
                    size_bytes: binary.len() as u64,
                    created_by: "codex:gpt-5.5".to_string(),
                    created_at: at,
                }],
            },
        )
        .expect("write binary manifest");

    store
        .upsert_task_artifacts(
            "ORB-00001",
            &TaskArtifactUpdateParams {
                owner_run_id: None,
                actor: "codex:gpt-5.5".to_string(),
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "reports/text.txt",
                    "needle-safe-text\n",
                )],
            },
        )
        .expect("upsert text artifact");

    assert_eq!(
        store
            .search_tasks("needle-safe-text")
            .expect("search skips binary")
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>(),
        vec!["ORB-00001"]
    );
}

#[test]
fn list_tasks_filtered_supports_lineage_and_job_run_filters() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let parent = store
        .create_task(create_params("Parent", TaskStatus::Backlog))
        .expect("create parent");
    let mut child = create_params("Child", TaskStatus::Backlog);
    child.parent_id = Some(parent.id.clone());
    let child = store.create_task(child).expect("create child");
    store
        .update_task_document(
            &child.id,
            &TaskDocumentUpdateParams {
                actor: "codex:gpt-5.5".to_string(),
                job_run_id: Some(Some("jrun-store".to_string())),
                ..Default::default()
            },
        )
        .expect("assign job run");

    assert_eq!(
        store
            .list_tasks_filtered(None, None, Some(&parent.id), None, None, None)
            .expect("filter by parent")
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>(),
        vec![child.id.clone()]
    );
    assert_eq!(
        store
            .list_tasks_filtered(None, None, None, Some("jrun-store"), None, None)
            .expect("filter by job run")
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>(),
        vec![child.id]
    );
}

#[test]
fn stale_generated_index_is_rebuilt_before_filtered_reads() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    store
        .create_task(create_params("Indexed", TaskStatus::Backlog))
        .expect("create task");
    let stale_envelope = store
        .bundle_store
        .read_bundle("ORB-00000")
        .expect("read bundle")
        .envelope;

    store
        .update_task_history(
            "ORB-00000",
            &TaskHistoryUpdateParams {
                actor: "codex:gpt-5.5".to_string(),
                status: Some(TaskStatus::InProgress),
                status_note: Some("Starting".to_string()),
                ..Default::default()
            },
        )
        .expect("update status");
    store
        .registry
        .replace_task_index(&store.workspace_id, &stale_envelope)
        .expect("force stale index row");

    let matches = store
        .list_tasks_filtered(Some(TaskStatus::InProgress), None, None, None, None, None)
        .expect("filtered tasks");
    assert_eq!(
        matches.into_iter().map(|task| task.id).collect::<Vec<_>>(),
        vec!["ORB-00000"]
    );
    let current_updated_at = store
        .bundle_store
        .read_bundle("ORB-00000")
        .expect("read current bundle")
        .envelope
        .updated_at
        .to_rfc3339();
    assert_eq!(
        store
            .registry
            .indexed_task_versions_for_workspace(&store.workspace_id)
            .expect("index versions")
            .get("ORB-00000"),
        Some(&current_updated_at)
    );
}
