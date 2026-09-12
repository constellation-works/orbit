//! Git source-snapshot fixtures for task-pilot prepare/apply [ORB-11236].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};
use tempfile::TempDir;

use orbit_engine::fetch_remote_base;

use super::super::task_pilot::{apply, prepare};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_config, runtime_with_workspace_layout,
};
use crate::application::task::TaskAddParams;

const LANDING: &str = "agent-main";

struct RemoteLandingFixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    seed: PathBuf,
    stale_sha: String,
    current_sha: String,
    task: Task,
}

fn seed_task(runtime: &OrbitRuntime, title: &str) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The fixture outcome is observable.".to_string()],
            plan: "Inspect and update the fixture.".to_string(),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed task")
}

fn selector_assessment(task: &Task, after: Vec<&str>) -> Value {
    json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": after,
        "disposition": "selectors",
        "recommended_crew": "luna",
        "recommended_complexity": "medium",
        "assessment_rationale": "The repair changes a known source boundary.",
        "confidence": "high",
        "evidence_gaps": [],
        "validation_approach": "Run focused source tests.",
        "reassessment_triggers": ["the source revision changes"],
        "blocked_by": [],
        "duplicate_of": null,
        "already_landed": null,
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
    })
}

fn apply_selectors(
    runtime: &OrbitRuntime,
    prepared: &Value,
    task: &Task,
    after: Vec<&str>,
) -> Value {
    apply(
        runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared,
            "results": [{
                "partition_index": 0,
                "task_ids": [task.id.clone()],
                "tasks": [selector_assessment(task, after)],
                "summary": "fixture partition",
            }],
            "workspace_path": prepared["workspace_path"],
        }),
    )
    .expect("apply partition is a durable output")
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo(path: &Path, branch: &str) {
    fs::create_dir_all(path).expect("create repo dir");
    git(path, &["init"]);
    git(path, &["checkout", "-b", branch]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
    git(path, &["config", "commit.gpgsign", "false"]);
}

fn commit_file(repo: &Path, relative: &str, contents: &str) -> String {
    let path = repo.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, contents).expect("write file");
    git(repo, &["add", relative]);
    git(repo, &["commit", "-m", &format!("write {relative}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn remote_landing_fixture_with_workspace_config(
    branch: &str,
    config_toml: Option<&str>,
) -> RemoteLandingFixture {
    let (root, runtime, repo) = match config_toml {
        Some(config_toml) => runtime_with_workspace_config(Some(config_toml)),
        None => runtime_with_workspace_layout(),
    };
    let remote = root.path().join("remote.git");
    let seed = root.path().join("seed");
    git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
    init_repo(&repo, branch);
    fs::create_dir_all(repo.join("src")).expect("src dir");
    fs::write(repo.join(".gitignore"), ".orbit/\n").expect("ignore orbit store");
    fs::write(repo.join("src/existing.rs"), "existing\n").expect("write existing");
    git(&repo, &["add", ".gitignore", "src/existing.rs"]);
    git(&repo, &["commit", "-m", "existing target"]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-u", "origin", branch]);
    let stale_sha = git(&repo, &["rev-parse", "HEAD"]);

    git(
        root.path(),
        &[
            "clone",
            "--branch",
            branch,
            remote.to_str().unwrap(),
            seed.to_str().unwrap(),
        ],
    );
    git(&seed, &["config", "user.name", "Orbit Test"]);
    git(&seed, &["config", "user.email", "orbit-test@example.com"]);
    git(&seed, &["config", "commit.gpgsign", "false"]);
    let current_sha = commit_file(&seed, "src/merged.rs", "newly merged\n");
    git(&seed, &["push", "origin", branch]);

    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), stale_sha);
    assert!(!repo.join("src/merged.rs").exists());

    let task = seed_task(&runtime, "merged target");
    RemoteLandingFixture {
        _root: root,
        runtime,
        repo,
        seed,
        stale_sha,
        current_sha,
        task,
    }
}

fn remote_landing_fixture_for(branch: &str) -> RemoteLandingFixture {
    remote_landing_fixture_with_workspace_config(branch, None)
}

fn remote_landing_fixture() -> RemoteLandingFixture {
    remote_landing_fixture_for(LANDING)
}

fn prepare_landing(fixture: &RemoteLandingFixture) -> Result<Value, String> {
    prepare(
        &fixture.runtime,
        "prepare_task_pilot",
        &json!({
            "task_ids": [fixture.task.id.clone()],
            "workspace_path": fixture.repo,
            "base_branch": LANDING,
        }),
    )
    .map_err(|error| error.to_string())
}

fn prepare_with_base_branch(
    fixture: &RemoteLandingFixture,
    base_branch: Option<&str>,
) -> Result<Value, String> {
    let mut input = json!({
        "task_ids": [fixture.task.id.clone()],
        "workspace_path": fixture.repo,
    });
    if let Some(base_branch) = base_branch {
        input["base_branch"] = json!(base_branch);
    }
    prepare(&fixture.runtime, "prepare_task_pilot", &input).map_err(|error| error.to_string())
}

#[test]
fn zero_input_preparation_uses_the_owning_workspace_main_branch() {
    let fixture = remote_landing_fixture_for("main");

    let prepared = prepare_with_base_branch(&fixture, None)
        .expect("zero-input preparation must use the default workspace branch");

    assert_eq!(prepared["source"]["base_branch"], "main");
    assert_eq!(prepared["source"]["source_revision"], fixture.current_sha);
}

#[test]
fn zero_input_preparation_uses_a_configured_agent_main_branch() {
    let fixture = remote_landing_fixture_with_workspace_config(
        LANDING,
        Some("[workflow]\nbase_branch = \"agent-main\"\n"),
    );

    let prepared = prepare_with_base_branch(&fixture, None)
        .expect("zero-input preparation must use the configured agent-main branch");

    assert_eq!(prepared["source"]["base_branch"], LANDING);
    assert_eq!(prepared["source"]["source_revision"], fixture.current_sha);
}

#[test]
fn explicit_base_branch_overrides_the_owning_workspace_default() {
    let fixture = remote_landing_fixture();

    let prepared = prepare_with_base_branch(&fixture, Some(LANDING))
        .expect("an explicit task-pilot branch must remain authoritative");

    assert_eq!(prepared["source"]["base_branch"], LANDING);
    assert_eq!(prepared["source"]["source_revision"], fixture.current_sha);
}

#[test]
fn explicit_missing_base_branch_fails_before_pilot_dispatch() {
    let fixture = remote_landing_fixture_for("main");

    let error = prepare_with_base_branch(&fixture, Some("missing-branch"))
        .expect_err("an unavailable explicit branch must fail closed");

    assert!(error.contains("could not fetch"), "{error}");
}

#[test]
fn clean_stale_primary_is_preserved_and_apply_admits_newly_merged_file() {
    let fixture = remote_landing_fixture();
    let prepared = prepare_landing(&fixture).expect("clean stale primary prepares");

    assert_eq!(prepared["source"]["base_branch"], LANDING);
    assert_eq!(
        prepared["source"]["source_ref"],
        format!("origin/{LANDING}")
    );
    assert_eq!(prepared["source"]["source_revision"], fixture.current_sha);
    assert_eq!(prepared["source"]["fast_forwarded"], false);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "HEAD"]),
        fixture.stale_sha
    );
    assert!(!fixture.repo.join("src/merged.rs").exists());

    let output = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec!["file:src/merged.rs"],
    );
    assert_eq!(output["status"], "succeeded");
    assert_eq!(output["source"]["source_revision"], fixture.current_sha);
    assert_eq!(
        fixture
            .runtime
            .get_task(&fixture.task.id)
            .unwrap()
            .context_files,
        vec!["file:src/merged.rs"]
    );
}

#[test]
fn bare_file_and_directory_selectors_normalize_at_the_pinned_revision() {
    let fixture = remote_landing_fixture();
    let prepared = prepare_landing(&fixture).expect("prepare pinned source");

    let output = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec!["src/merged.rs", "src"],
    );

    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(
        output["tasks"][0]["context_files_after"],
        json!(["file:src/merged.rs", "dir:src"])
    );
    assert_eq!(
        output["tasks"][0]["selector_normalizations"],
        json!([
            {"original": "src/merged.rs", "normalized": "file:src/merged.rs"},
            {"original": "src", "normalized": "dir:src"},
        ])
    );
    assert_eq!(
        fixture
            .runtime
            .get_task(&fixture.task.id)
            .unwrap()
            .context_files,
        vec!["file:src/merged.rs", "dir:src"]
    );
}

#[test]
fn five_task_partition_keeps_valid_siblings_when_bare_target_is_missing() {
    let fixture = remote_landing_fixture();
    let mut tasks = vec![fixture.task.clone()];
    for index in 1..5 {
        tasks.push(seed_task(&fixture.runtime, &format!("sibling-{index}")));
    }
    let task_ids = tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
    let prepared = prepare(
        &fixture.runtime,
        "prepare_task_pilot",
        &json!({
            "task_ids": task_ids,
            "workspace_path": fixture.repo,
            "base_branch": LANDING,
        }),
    )
    .expect("prepare five-task partition");
    let assessments = tasks
        .iter()
        .enumerate()
        .map(|(index, task)| {
            selector_assessment(
                task,
                if index == 0 {
                    vec![".orbit/resources/activities/task_pilot.yaml"]
                } else {
                    vec!["file:src/merged.rs"]
                },
            )
        })
        .collect::<Vec<_>>();

    let output = apply(
        &fixture.runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared,
            "results": [{
                "partition_index": 0,
                "task_ids": task_ids,
                "tasks": assessments,
            }],
            "workspace_path": fixture.repo,
        }),
    )
    .expect("mixed partition returns structured outcomes");

    assert_eq!(output["status"], "failed");
    assert_eq!(output["partition_decisions"][0]["outcome"], "partial");
    assert_eq!(output["applied_count"], 4);
    assert_eq!(output["unresolved_count"], 1);
    assert_eq!(
        output["partition_decisions"][0]["task_outcomes"][0]["outcome"],
        "invalid"
    );
    for task in tasks.iter().skip(1) {
        assert_eq!(
            fixture.runtime.get_task(&task.id).unwrap().context_files,
            vec!["file:src/merged.rs"]
        );
    }
}

#[test]
fn dirty_primary_preserves_head_index_tracked_and_untracked_bytes() {
    let fixture = remote_landing_fixture();
    fs::write(fixture.repo.join("src/existing.rs"), "staged edit\n").unwrap();
    git(&fixture.repo, &["add", "src/existing.rs"]);
    fs::write(fixture.repo.join("src/existing.rs"), b"dirty edit\0\xff").unwrap();
    fs::write(
        fixture.repo.join("src/merged.rs"),
        b"untracked collision\0\xff",
    )
    .unwrap();
    let index = fs::read(fixture.repo.join(".git/index")).unwrap();
    let prepared = prepare_landing(&fixture).expect("dirty primary prepares");
    let output = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec![
            "file:src/merged.rs",
            "dir:src",
            "symbol:src/existing.rs#existing:function",
        ],
    );
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "HEAD"]),
        fixture.stale_sha
    );
    assert_eq!(fs::read(fixture.repo.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read(fixture.repo.join("src/existing.rs")).unwrap(),
        b"dirty edit\0\xff"
    );
    assert_eq!(
        fs::read(fixture.repo.join("src/merged.rs")).unwrap(),
        b"untracked collision\0\xff"
    );
}

#[cfg(unix)]
#[test]
fn dirty_primary_symlink_does_not_change_pinned_selector_validation() {
    let fixture = remote_landing_fixture();
    fs::remove_file(fixture.repo.join("src/existing.rs")).unwrap();
    std::os::unix::fs::symlink(
        "/missing-outside-target",
        fixture.repo.join("src/existing.rs"),
    )
    .unwrap();
    let prepared = prepare_landing(&fixture).expect("symlink dirty primary prepares");
    let output = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec!["file:src/existing.rs"],
    );
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(
        fs::read_link(fixture.repo.join("src/existing.rs")).unwrap(),
        Path::new("/missing-outside-target")
    );
}

#[test]
fn remote_failure_is_closed_without_git_writes() {
    let fixture = remote_landing_fixture();
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "/no/such/orbit-pilot-remote.git",
        ],
    );
    let before = git(&fixture.repo, &["rev-parse", "HEAD"]);

    let error = prepare_landing(&fixture).expect_err("broken origin must fail closed");
    assert!(
        error.contains("remote failure") || error.contains("could not fetch"),
        "{error}"
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), before);
    assert!(!fixture.repo.join("src/merged.rs").exists());
}

#[test]
fn apply_uses_pinned_revision_after_concurrent_branch_advancement() {
    let fixture = remote_landing_fixture();
    let prepared = prepare_landing(&fixture).expect("prepare pins current landing tip");
    let pinned = prepared["source"]["source_revision"]
        .as_str()
        .expect("pinned sha")
        .to_string();
    assert_eq!(pinned, fixture.current_sha);

    commit_file(&fixture.seed, "src/later.rs", "landed after prepare\n");
    git(&fixture.seed, &["push", "origin", LANDING]);
    git(&fixture.repo, &["fetch", "origin", LANDING]);
    git(
        &fixture.repo,
        &["merge", "--ff-only", &format!("origin/{LANDING}")],
    );
    assert!(fixture.repo.join("src/later.rs").exists());

    let later = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec!["file:src/later.rs"],
    );
    assert_eq!(later["status"], "failed");
    let error = later["partition_decisions"][0]["error"]
        .as_str()
        .expect("failed partition error");
    assert!(error.contains("does not resolve"), "{error}");
    assert!(error.contains(&pinned), "{error}");
    assert!(
        fixture
            .runtime
            .get_task(&fixture.task.id)
            .unwrap()
            .context_files
            .is_empty()
    );

    let merged = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec!["file:src/merged.rs"],
    );
    assert_eq!(merged["status"], "succeeded");
    assert_eq!(
        fixture
            .runtime
            .get_task(&fixture.task.id)
            .unwrap()
            .context_files,
        vec!["file:src/merged.rs"]
    );
}

#[test]
fn untracked_workspace_file_is_not_admitted_against_the_source_snapshot() {
    let fixture = remote_landing_fixture();
    let prepared = prepare_landing(&fixture).expect("pin landing tip");
    fs::write(fixture.repo.join("src/ghost.rs"), "working tree only\n").expect("untracked ghost");

    let output = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec!["file:src/ghost.rs"],
    );
    assert_eq!(output["status"], "failed");
    assert!(
        output["partition_decisions"][0]["error"]
            .as_str()
            .unwrap()
            .contains("does not resolve")
    );
    assert!(
        fixture
            .runtime
            .get_task(&fixture.task.id)
            .unwrap()
            .context_files
            .is_empty()
    );
}

/// Concurrent prepares serialize shared ref fetching and leave primary untouched.
#[test]
fn concurrent_prepare_calls_against_the_same_primary_both_succeed() {
    let fixture = remote_landing_fixture();

    let results: Vec<Result<Value, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let fixture = &fixture;
                scope.spawn(move || prepare_landing(fixture))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("prepare thread joined"))
            .collect()
    });

    for (index, result) in results.iter().enumerate() {
        let prepared = result
            .as_ref()
            .unwrap_or_else(|error| panic!("concurrent prepare {index} failed: {error}"));
        assert_eq!(prepared["source"]["base_branch"], LANDING);
        assert_eq!(
            prepared["source"]["source_revision"], fixture.current_sha,
            "prepare {index} must pin the landing-branch tip"
        );
    }

    assert_eq!(
        git(&fixture.repo, &["rev-parse", "HEAD"]),
        fixture.stale_sha
    );
    assert!(!fixture.repo.join("src/merged.rs").exists());
    assert!(
        results
            .iter()
            .all(|result| result.as_ref().unwrap()["source"]["fast_forwarded"] == false)
    );
}

/// Pilot prepare and delivery `fetch_remote_base` share one git-common-dir
/// lock, so concurrent origin fetches pin the same source identity instead
/// of failing ref-CAS.
#[test]
fn concurrent_prepare_and_delivery_fetch_share_the_common_dir_lock() {
    let fixture = remote_landing_fixture();
    fs::write(fixture.repo.join("src/existing.rs"), "user dirty bytes\n").unwrap();
    fs::write(
        fixture.repo.join("src/user-only.rs"),
        "untracked user file\n",
    )
    .unwrap();
    let linked = fixture
        .repo
        .parent()
        .expect("fixture parent")
        .join("linked");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );

    let (prepares, fetches) = std::thread::scope(|scope| {
        let prepare_handles: Vec<_> = (0..3)
            .map(|_| scope.spawn(|| prepare_landing(&fixture)))
            .collect();
        let fetch_paths = [&fixture.repo, &linked];
        let fetch_handles: Vec<_> = fetch_paths
            .into_iter()
            .flat_map(|path| {
                (0..2).map(|_| {
                    let path = path.clone();
                    scope
                        .spawn(move || fetch_remote_base(&path, LANDING).map_err(|e| e.to_string()))
                })
            })
            .collect();
        let prepares: Vec<_> = prepare_handles
            .into_iter()
            .map(|handle| handle.join().expect("prepare thread joined"))
            .collect();
        let fetches: Vec<_> = fetch_handles
            .into_iter()
            .map(|handle| handle.join().expect("fetch thread joined"))
            .collect();
        (prepares, fetches)
    });

    for (index, result) in prepares.iter().enumerate() {
        let prepared = result
            .as_ref()
            .unwrap_or_else(|error| panic!("concurrent prepare {index} failed: {error}"));
        assert_eq!(prepared["source"]["source_revision"], fixture.current_sha);
        assert_eq!(
            prepared["source"]["source_ref"],
            format!("origin/{LANDING}")
        );
    }
    for (index, result) in fetches.iter().enumerate() {
        result
            .as_ref()
            .unwrap_or_else(|error| panic!("concurrent delivery fetch {index} failed: {error}"));
    }

    assert_eq!(
        git(&fixture.repo, &["rev-parse", &format!("origin/{LANDING}")]),
        fixture.current_sha
    );
    assert_eq!(
        git(&linked, &["rev-parse", &format!("origin/{LANDING}")]),
        fixture.current_sha
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "HEAD"]),
        fixture.stale_sha
    );
    assert_eq!(git(&linked, &["rev-parse", "HEAD"]), fixture.stale_sha);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("src/existing.rs")).unwrap(),
        "user dirty bytes\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("src/user-only.rs")).unwrap(),
        "untracked user file\n"
    );
    assert!(!fixture.repo.join("src/merged.rs").exists());
}

#[test]
fn non_git_workspace_still_uses_filesystem_existence() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    fs::create_dir_all(repo_root.join("src")).expect("src");
    fs::write(repo_root.join("src/alpha.rs"), "fn alpha() {}\n").expect("alpha");
    let task = seed_task(&runtime, "filesystem fallback");
    let prepared = prepare(
        &runtime,
        "prepare_task_pilot",
        &json!({
            "task_ids": [task.id.clone()],
            "workspace_path": repo_root,
            "base_branch": LANDING,
        }),
    )
    .expect("non-git prepare");
    assert_eq!(prepared["source"]["source_revision"], Value::Null);

    let output = apply_selectors(&runtime, &prepared, &task, vec!["file:src/alpha.rs"]);
    assert_eq!(output["status"], "succeeded");
}

#[test]
fn material_criteria_edit_invalidates_real_pilot_apply() {
    let fixture = remote_landing_fixture();
    let prepared = prepare_landing(&fixture).unwrap();
    assert!(prepared["tasks"][0]["material_fingerprint"].is_string());
    fixture
        .runtime
        .update_task(
            &fixture.task.id,
            crate::application::task::TaskUpdateParams {
                acceptance_criteria: Some(vec!["A different material requirement".into()]),
                ..Default::default()
            },
        )
        .unwrap();
    let applied = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec!["file:src/merged.rs"],
    );
    assert_eq!(applied["status"], "failed");
    assert_eq!(
        applied["skipped_stale_partitions"][0]["stale_tasks"][0]["reason"],
        "material_changed"
    );
    assert!(
        fixture
            .runtime
            .get_task(&fixture.task.id)
            .unwrap()
            .context_files
            .is_empty()
    );
}

#[test]
fn comment_and_summary_leave_material_fingerprint_unchanged() {
    let fixture = remote_landing_fixture();
    let before = prepare_landing(&fixture).unwrap();
    fixture
        .runtime
        .update_task(
            &fixture.task.id,
            crate::application::task::TaskUpdateParams {
                comment: Some("Progress only".into()),
                execution_summary: Some("Instrumentation only".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let after = prepare_landing(&fixture).unwrap();
    assert_eq!(
        before["tasks"][0]["material_fingerprint"],
        after["tasks"][0]["material_fingerprint"]
    );
}

#[test]
fn state_member_apply_preserves_resulting_provenance_without_promotion() {
    use orbit_engine::RuntimeHost;
    use orbit_types::workflow::automation::{members::*, *};
    let mut fixture = remote_landing_fixture();
    fixture.runtime = fixture
        .runtime
        .with_automation_machine_identity(Some("fixture".into()));
    // State consumers pin the configured local integration ref.
    let source = SourceRevision {
        commit: fixture.stale_sha.clone(),
        tree: git(&fixture.repo, &["rev-parse", "HEAD^{tree}"]),
    };
    let fingerprint = crate::application::automation::preparation::fingerprint(
        &fixture.runtime,
        &fixture.task,
        &source.commit,
    )
    .unwrap();
    let now = chrono::Utc::now();
    let member = StateMember {
        key: fixture.task.id.clone(),
        task_ids: vec![fixture.task.id.clone()],
        fingerprint,
        source: source.clone(),
        evidence: json!({}),
        first_seen: now,
        changed_at: now,
    };
    let consumer =
        crate::application::automation::consumer_key(&fixture.runtime, "routine", "pilot").unwrap();
    let trigger = StateTrigger {
        kind: StateTriggerKind::PreparationEligible,
        owner_machine: "fixture".into(),
        branch: LANDING.into(),
        debounce_minutes: 2,
        max_wait_minutes: 10,
        max_items: 50,
        retries: 1,
        deadline_minutes: 30,
    };
    let definition: orbit_types::workflow::RoutineDefinition = serde_json::from_value(json!({
        "schemaVersion":1,"name":"pilot","enabled":true,"hosts":["fixture"],"target":"job:task_pilot_pipeline",
        "trigger":{"state":trigger},"policy":{"overlap":"forbid","retries":{"max":1,"backoff_minutes":5},"timeout_minutes":30}
    })).unwrap();
    let epoch = orbit_automation::delivery::definition_epoch(&(
        &trigger.kind,
        &trigger.owner_machine,
        &trigger.branch,
        &definition.target,
    ))
    .unwrap();
    let store = fixture.runtime.automation_store().unwrap();
    let state = AutomationState {
        members: Some(MemberState::default()),
        consumer: consumer.clone(),
        epoch,
        trigger: None,
        repository: "fixture".into(),
        branch: LANDING.into(),
        generation: 0,
        baseline: source.clone(),
        observed: source.clone(),
        covered: source,
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        active: None,
    };
    assert!(store.automation_initialize(&state).unwrap());
    let claim = MemberAttempt {
        consumer: consumer.clone(),
        kind: StateTriggerKind::PreparationEligible,
        id: "fixture-attempt".into(),
        member: member.clone(),
        attempt: 1,
        max_attempts: 2,
        deadline: now + chrono::Duration::minutes(30),
        retry_after: now,
        action_key: "fixture-key".into(),
        action_id: None,
        exhausted: false,
    };
    let mut claimed = state.clone();
    claimed.generation += 1;
    claimed
        .members
        .as_mut()
        .unwrap()
        .pending
        .insert(member.key.clone(), member);
    claimed.members.as_mut().unwrap().active = Some(claim.clone());
    assert!(store.automation_commit(&state, &claimed, None).unwrap());
    let run = fixture
        .runtime
        .stores()
        .jobs()
        .insert_automation_job_run(
            "task_pilot_pipeline",
            json!({"state_automation":claim}),
            "fixture-key",
        )
        .unwrap();
    let mut admitted = claimed.clone();
    admitted.generation += 1;
    admitted
        .members
        .as_mut()
        .unwrap()
        .active
        .as_mut()
        .unwrap()
        .action_id = Some(run.run_id.clone());
    assert!(store.automation_commit(&claimed, &admitted, None).unwrap());
    let input = json!({"state_automation":claim,"task_ids":[fixture.task.id],"workspace_path":fixture.repo,"base_branch":LANDING,"source_revision":fixture.stale_sha});
    assert!(
        fixture
            .runtime
            .run_deterministic(
                "prepare_task_pilot",
                &json!({}),
                &input,
                fixture
                    .runtime
                    .tool_context_for_activity(Some("wrong-run"), None, None, None)
            )
            .is_err()
    );
    let prepared = fixture
        .runtime
        .run_deterministic(
            "prepare_task_pilot",
            &json!({}),
            &input,
            fixture
                .runtime
                .tool_context_for_activity(Some(&run.run_id), None, None, None),
        )
        .unwrap();
    let result = apply_selectors(
        &fixture.runtime,
        &prepared,
        &fixture.task,
        vec!["file:src/existing.rs"],
    );
    assert_eq!(result["status"], "succeeded");
    let evidence: MemberEvidence =
        serde_json::from_value(result["member_evidence"].clone()).unwrap();
    assert_eq!(evidence.input_fingerprint, claim.member.fingerprint);
    assert_ne!(evidence.input_fingerprint, evidence.resulting_fingerprint);
    let task = fixture.runtime.get_task(&fixture.task.id).unwrap();
    assert_eq!(task.status, TaskStatus::Backlog);
    assert_eq!(
        evidence.resulting_fingerprint,
        crate::application::automation::preparation::fingerprint(
            &fixture.runtime,
            &task,
            &fixture.stale_sha
        )
        .unwrap()
    );
    assert_eq!(evidence.action_id, run.run_id);
    let mut pipeline = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        json!({"state_automation":claim}),
    );
    pipeline.record_step(
        2,
        orbit_types::workflow::JobRunState::Success,
        Some(result),
        None,
    );
    fixture
        .runtime
        .write_run_state(&run.run_id, &pipeline)
        .unwrap();
    fixture
        .runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, now, std::process::id())
        .unwrap();
    fixture
        .runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            orbit_types::workflow::JobRunState::Failed,
            chrono::Utc::now(),
            Some(1),
            orbit_store::TaskReservationReleaseReason::RunTerminal,
        )
        .unwrap();
    let diagnostic = crate::application::automation::evaluate_routine(
        &fixture.runtime,
        &definition,
        false,
        now + chrono::Duration::minutes(1),
    )
    .unwrap();
    assert_eq!(diagnostic.receipts.len(), 1);
    assert!(diagnostic.state.unwrap().members.unwrap().active.is_none());
    let receipt = store
        .automation_receipts(&consumer, 20)
        .unwrap()
        .pop()
        .unwrap();
    let accepted: MemberEvidence = serde_json::from_slice(&receipt.evidence).unwrap();
    assert_eq!(accepted, evidence);
}
