#![allow(missing_docs)]

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::activity_job::load_job_asset;
use orbit_agent::loop_engine::audit::AuditSink;
use orbit_types::workflow::activity_job::{JobV2StepBody, V2AuditEventKind};
use tempfile::{TempDir, tempdir};

use crate::template::{self, TemplateContext};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::DispatchError;
use super::super::super::workspace::{WorktreeBoundaryGuard, validate_declared_worktree_pair};
use super::super::run_cli_backend;
use super::test_support::{
    RecordingSink, TestHost, capture_events, test_agent_loop_spec, test_agent_loop_spec_for,
    write_executable,
};

const TASK_LOCAL_PIPELINE_YAML: &str =
    include_str!("../../../../../orbit-core/assets/jobs/task_local_pipeline.yaml");

const TASK_PR_PIPELINE_YAML: &str =
    include_str!("../../../../../orbit-core/assets/jobs/task_pr_pipeline.yaml");

#[test]
fn actual_ship_pipeline_implementers_run_for_each_provider_and_stay_in_the_worktree() {
    let mut rendered_assets = BTreeSet::new();
    for pipeline_yaml in [TASK_LOCAL_PIPELINE_YAML, TASK_PR_PIPELINE_YAML] {
        for provider in ["claude", "codex"] {
            let fixture = linked_worktree_fixture();
            let script = fixture.root().join(provider);
            let assigned = fixture.assigned.display().to_string();
            let task_id = format!("ORB-ASSET-{provider}");
            let (pipeline, input) =
                rendered_implement_input_from_asset(pipeline_yaml, &fixture.assigned, &task_id);
            rendered_assets.insert(pipeline.clone());
            write_executable(
                &script,
                &format!(
                    r#"#!/bin/sh
cat > /dev/null
test "$(pwd -P)" = '{assigned}' || exit 41
test "$(git rev-parse --show-toplevel)" = '{assigned}' || exit 42
printf '%s\n' '{pipeline}:{provider}' > observed-relative-write.txt
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{}},"error":null}}'
"#
                ),
            );
            let mut host = TestHost::with_command(script.display().to_string());
            host.workspace_root = Some(fixture.primary.clone());
            let spec = test_agent_loop_spec_for(provider, Duration::from_secs(5));
            let audit = test_audit(&format!("run-{pipeline}-{provider}"), provider);

            let outcome = run_cli_backend(
                &host,
                &spec,
                "test_activity",
                &format!("run-{pipeline}-{provider}"),
                audit,
                &input,
                None,
            )
            .unwrap_or_else(|error| panic!("{pipeline}/{provider} invocation: {error}"));

            assert!(outcome.success, "{pipeline}/{provider} should succeed");
            assert_eq!(
                fs::read_to_string(fixture.assigned.join("observed-relative-write.txt"))
                    .expect("assigned relative write"),
                format!("{pipeline}:{provider}\n")
            );
            assert!(
                !fixture.primary.join("observed-relative-write.txt").exists(),
                "{pipeline}/{provider} must not write the registered primary checkout"
            );
        }
    }
    assert_eq!(
        rendered_assets,
        BTreeSet::from([
            "task_local_pipeline".to_string(),
            "task_pr_pipeline".to_string(),
        ]),
        "the matrix must load and render both committed shipment assets"
    );
}

#[test]
fn declared_repo_root_mismatch_fails_typed_before_provider_spawn() {
    let fixture = linked_worktree_fixture();
    let marker = fixture.root().join("provider-started");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let audit = test_audit("run-repo-root-mismatch", "codex");
    let mut input = worktree_input(&fixture, "ORB-PAIR-MISMATCH");
    input["repo_root"] = serde_json::json!(fixture.primary);

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-repo-root-mismatch",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("mismatched declared pair must fail closed");

    assert_worktree_mismatch(
        &error,
        "ORB-PAIR-MISMATCH",
        "run-repo-root-mismatch",
        "different Git checkouts",
    );
    assert_pre_spawn_failure(&audit, &marker);

    input["repo_root"] = serde_json::Value::Null;
    let null_error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-null-repo-root",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("a declared null repo_root must not erase the worktree pair");
    assert_worktree_mismatch(
        &null_error,
        "ORB-PAIR-MISMATCH",
        "run-null-repo-root",
        "repo_root must be a non-empty string",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn declared_non_git_checkout_fails_typed_before_provider_spawn() {
    let fixture = linked_worktree_fixture();
    let non_git = fixture.root().join("not-a-repository");
    fs::create_dir(&non_git).expect("create non-Git directory");
    // TMPDIR can sit inside a managed worktree. Stop Git from discovering
    // that ancestor so the declared path has no usable repository.
    fs::write(non_git.join(".git"), "gitdir: missing-fixture-gitdir\n")
        .expect("isolate non-Git fixture from ancestor worktree");
    let marker = fixture.root().join("provider-started");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let audit = test_audit("run-non-git-pair", "codex");
    let input = serde_json::json!({
        "task_id": "ORB-NON-GIT",
        "workspace_path": non_git,
        "repo_root": non_git,
    });

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-non-git-pair",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("non-Git declared pair must fail closed");

    assert_worktree_mismatch(
        &error,
        "ORB-NON-GIT",
        "run-non-git-pair",
        "not a Git checkout",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn declared_checkout_from_different_repository_fails_before_provider_spawn() {
    let assigned_fixture = linked_worktree_fixture();
    let registered_fixture = linked_worktree_fixture();
    let marker = assigned_fixture.root().join("provider-started");
    let script = assigned_fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(registered_fixture.primary.clone());
    let audit = test_audit("run-different-common-dir", "codex");

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-different-common-dir",
        audit.clone(),
        &worktree_input(&assigned_fixture, "ORB-DIFFERENT-REPO"),
        None,
    )
    .expect_err("different repositories must fail closed");

    assert_worktree_mismatch(
        &error,
        "ORB-DIFFERENT-REPO",
        "run-different-common-dir",
        "different Git common dirs",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn declared_checkout_cannot_collapse_to_registered_primary() {
    let fixture = linked_worktree_fixture();
    let marker = fixture.root().join("provider-started");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let audit = test_audit("run-primary-collapse", "codex");
    let input = serde_json::json!({
        "task_id": "ORB-PRIMARY-COLLAPSE",
        "workspace_path": fixture.primary,
        "repo_root": fixture.primary,
    });

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-primary-collapse",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("assigned checkout must not collapse to primary");

    assert_worktree_mismatch(
        &error,
        "ORB-PRIMARY-COLLAPSE",
        "run-primary-collapse",
        "collapses to the registered primary",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn unchanged_pre_dirty_primary_does_not_block_valid_worktree_implementation() {
    let fixture = linked_worktree_fixture();
    fs::write(
        fixture.primary.join("README.md"),
        "pre-existing primary dirtiness\n",
    )
    .expect("dirty primary");
    let primary_before = git_bytes(&fixture.primary, &["diff", "--binary", "HEAD", "--"]);
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf 'assigned only\\n' > assigned.txt\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-pre-dirty-primary",
        test_audit("run-pre-dirty-primary", "codex"),
        &worktree_input(&fixture, "ORB-PRE-DIRTY"),
        None,
    )
    .expect("unchanged dirty primary is allowed");

    assert!(outcome.success);
    assert!(fixture.assigned.join("assigned.txt").exists());
    assert_eq!(
        git_bytes(&fixture.primary, &["diff", "--binary", "HEAD", "--"]),
        primary_before,
        "the guard must leave pre-existing primary dirtiness byte-for-byte unchanged"
    );
}

#[test]
fn concurrent_primary_fast_forward_does_not_block_disjoint_worktree_changes() {
    let fixture = linked_worktree_fixture();
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'assigned\\n' > assigned.txt\nprintf 'concurrent\\n' > '{}/concurrent.txt'\ngit -C '{}' add -- concurrent.txt\ngit -C '{}' commit -m concurrent-base\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            fixture.primary.display(),
            fixture.primary.display(),
            fixture.primary.display(),
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-concurrent-fast-forward",
        test_audit("run-concurrent-fast-forward", "codex"),
        &worktree_input(&fixture, "ORB-CONCURRENT-FF"),
        None,
    )
    .expect("a disjoint same-branch primary fast-forward is benign");

    assert!(outcome.success);
    assert!(fixture.assigned.join("assigned.txt").exists());
    assert!(fixture.primary.join("concurrent.txt").exists());
}

#[test]
fn two_in_flight_worktrees_survive_one_primary_merge_advance() {
    let fixture = linked_worktree_fixture();
    let assigned_two = fixture.root().join("assigned-two");
    git_ok(
        &fixture.primary,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-integrity-test-two",
            assigned_two.to_str().expect("utf8 second worktree"),
        ],
    );
    let assigned_two = assigned_two
        .canonicalize()
        .expect("canonical second worktree");
    let input_one = worktree_input(&fixture, "ORB-CONCURRENT-ONE");
    let input_two = serde_json::json!({
        "prompt": "implement",
        "task_id": "ORB-CONCURRENT-TWO",
        "workspace_path": assigned_two,
        "repo_root": assigned_two,
    });
    let pair_one = validate_declared_worktree_pair(
        &input_one,
        None,
        "run-concurrent-one",
        "codex",
        Some(&fixture.primary),
    )
    .expect("validate first pair")
    .expect("first linked pair");
    let pair_two = validate_declared_worktree_pair(
        &input_two,
        None,
        "run-concurrent-two",
        "codex",
        Some(&fixture.primary),
    )
    .expect("validate second pair")
    .expect("second linked pair");
    let guard_one = WorktreeBoundaryGuard::capture(
        &input_one,
        None,
        "run-concurrent-one",
        "codex",
        Some(&fixture.assigned),
        Some(&fixture.primary),
        Some(&pair_one),
    )
    .expect("capture first guard")
    .expect("first guard enabled");
    let guard_two = WorktreeBoundaryGuard::capture(
        &input_two,
        None,
        "run-concurrent-two",
        "codex",
        Some(&assigned_two),
        Some(&fixture.primary),
        Some(&pair_two),
    )
    .expect("capture second guard")
    .expect("second guard enabled");

    fs::write(fixture.assigned.join("candidate-one.txt"), "candidate\n")
        .expect("write first candidate");
    fs::write(assigned_two.join("candidate-two.txt"), "candidate\n")
        .expect("write second candidate");
    fs::write(fixture.primary.join("merged-pr.txt"), "merged\n").expect("write merged PR");
    git_ok(&fixture.primary, &["add", "merged-pr.txt"]);
    git_ok(&fixture.primary, &["commit", "-m", "merge concurrent PR"]);

    guard_one
        .verify()
        .expect("first in-flight pipeline accepts the shared base advance");
    guard_two
        .verify()
        .expect("second in-flight pipeline accepts the shared base advance");

    assert!(fixture.assigned.join("candidate-one.txt").exists());
    assert!(assigned_two.join("candidate-two.txt").exists());
}

#[test]
fn concurrent_auto_task_refresh_stays_in_assigned_worktree_during_boundary_checks() {
    let fixture = linked_worktree_fixture();
    let definition_dir = fixture.assigned.join(".orbit/auto_tasks");
    fs::create_dir_all(&definition_dir).expect("definition dir");
    let definition_path = definition_dir.join("doc-duties.yaml");
    fs::write(
        &definition_path,
        "schemaVersion: 1\nname: doc-duties\nrevision: 0\n",
    )
    .expect("seed definition");
    git_ok(
        &fixture.assigned,
        &["add", ".orbit/auto_tasks/doc-duties.yaml"],
    );
    git_ok(
        &fixture.assigned,
        &["commit", "-m", "seed auto-task definition"],
    );
    git_ok(
        &fixture.primary,
        &["merge", "--ff-only", "orbit-integrity-test"],
    );

    let implement_worktree = fixture.root().join("disjoint-implement");
    git_ok(
        &fixture.primary,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-disjoint-implement",
            implement_worktree
                .to_str()
                .expect("utf8 implement worktree"),
        ],
    );
    let implement_worktree = implement_worktree
        .canonicalize()
        .expect("canonical implement worktree");
    let input_one = worktree_input(&fixture, "ORB-AUTO-TASK-REFRESH");
    let input_two = serde_json::json!({
        "prompt": "implement disjoint work",
        "task_id": "ORB-DISJOINT-IMPLEMENT",
        "workspace_path": implement_worktree,
        "repo_root": implement_worktree,
    });
    let pair_one = validate_declared_worktree_pair(
        &input_one,
        None,
        "run-auto-task-refresh",
        "codex",
        Some(&fixture.primary),
    )
    .expect("validate refresh pair")
    .expect("refresh linked pair");
    let pair_two = validate_declared_worktree_pair(
        &input_two,
        None,
        "run-disjoint-implement",
        "claude",
        Some(&fixture.primary),
    )
    .expect("validate implement pair")
    .expect("implement linked pair");
    let refresh_guard = WorktreeBoundaryGuard::capture(
        &input_one,
        None,
        "run-auto-task-refresh",
        "codex",
        Some(&fixture.assigned),
        Some(&fixture.primary),
        Some(&pair_one),
    )
    .expect("capture refresh guard")
    .expect("refresh guard enabled");
    let implement_guard = WorktreeBoundaryGuard::capture(
        &input_two,
        None,
        "run-disjoint-implement",
        "claude",
        Some(&implement_worktree),
        Some(&fixture.primary),
        Some(&pair_two),
    )
    .expect("capture implement guard")
    .expect("implement guard enabled");
    let primary_before = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);

    let (staged_tx, staged_rx) = std::sync::mpsc::channel();
    let (continue_tx, continue_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        for revision in 1..=32 {
            let staged = definition_dir.join(format!(".doc-duties.{revision}.tmp"));
            fs::write(
                &staged,
                format!("schemaVersion: 1\nname: doc-duties\nrevision: {revision}\n"),
            )
            .expect("stage definition");
            if revision == 1 {
                staged_tx.send(()).expect("signal staged refresh");
                continue_rx.recv().expect("continue refresh");
            }
            fs::rename(&staged, &definition_path).expect("atomically refresh definition");
        }
    });

    staged_rx.recv().expect("refresh reached staging point");
    implement_guard
        .verify()
        .expect("disjoint boundary snapshot must not report primary checkout drift");
    continue_tx.send(()).expect("release refresh");
    writer.join().expect("refresh thread");
    refresh_guard
        .verify()
        .expect("refresh boundary must remain inside its assigned worktree");

    assert_eq!(
        git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
        primary_before,
        "concurrent refresh must leave registered primary byte-identical"
    );
    assert!(
        fixture
            .assigned
            .join(".orbit/auto_tasks/doc-duties.yaml")
            .is_file()
    );
}

#[test]
fn failed_auto_task_refresh_preserves_primary_and_audits_definition_and_run() {
    let fixture = linked_worktree_fixture();
    let primary_before = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nmkdir -p .orbit/auto_tasks\nprintf 'partial\\n' > .orbit/auto_tasks/.doc-duties.tmp\nrm .orbit/auto_tasks/.doc-duties.tmp\nprintf 'auto-task doc-duties refresh failed\\n' >&2\nexit 23\n",
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink.clone();
    let audit = Arc::new(V2AuditWriter::new(
        "run-auto-task-refresh-failed",
        "codex:gpt-5.5",
        sink_for_writer,
    ));

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-auto-task-refresh-failed",
        audit.clone(),
        &worktree_input(&fixture, "ORB-AUTO-TASK-REFRESH"),
        None,
    )
    .expect("failed provider remains an audited dispatch outcome");

    assert!(!outcome.success);
    assert_eq!(
        git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
        primary_before,
        "failed refresh must leave registered primary byte-identical"
    );
    let events = audit.events_snapshot().expect("audit events");
    let failed = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationFinished {
                exit_code,
                stderr_blob_ref,
                ..
            } if *exit_code == Some(23) => Some((
                event.envelope.run_id.as_str(),
                stderr_blob_ref.as_deref().expect("stderr blob"),
            )),
            _ => None,
        })
        .expect("durable failed-refresh event");
    assert_eq!(failed.0, "run-auto-task-refresh-failed");
    assert_eq!(
        sink.blob(failed.1).as_deref(),
        Some(b"auto-task doc-duties refresh failed\n".as_slice()),
        "failure evidence must identify the auto-task"
    );
}

#[test]
fn stationary_primary_delta_is_observed_without_attributing_a_writer() {
    let fixture = linked_worktree_fixture();
    fs::write(
        fixture.primary.join("README.md"),
        "pre-existing primary dirtiness\n",
    )
    .expect("dirty primary before capture");
    let escaped = fixture.primary.join("escaped-after-capture.txt");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'escaped\\n' > '{}'\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            escaped.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-predirty-attribution",
        test_audit("run-predirty-attribution", "codex"),
        &worktree_input(&fixture, "ORB-PREDIRTY-ATTRIBUTION"),
        None,
    )
    .expect("stationary primary dirt has no reliable writer attribution");

    assert!(outcome.success);
    assert_eq!(
        fs::read_to_string(&escaped).expect("read observed primary dirt"),
        "escaped\n",
        "observation must not discard the concurrent primary content"
    );
    assert_eq!(
        fs::read_to_string(fixture.primary.join("README.md")).expect("read pre-dirty path"),
        "pre-existing primary dirtiness\n",
        "observation must not rewrite pre-existing primary dirt"
    );
}

#[test]
fn staged_only_primary_delta_is_preserved_without_blocking_the_candidate() {
    let fixture = linked_worktree_fixture();
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'staged-only\\n' > '{}/README.md'\ngit -C '{}' add -- README.md\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            fixture.primary.display(),
            fixture.primary.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-staged-only-attribution",
        test_audit("run-staged-only-attribution", "codex"),
        &worktree_input(&fixture, "ORB-STAGED-ONLY"),
        None,
    )
    .expect("staged primary dirt is external to the assigned candidate");

    assert!(outcome.success);
    assert_eq!(
        git_bytes(&fixture.primary, &["show", ":README.md"]),
        b"staged-only\n",
        "the external staged change must remain staged verbatim"
    );
}

#[test]
fn primary_index_delta_is_never_moved_into_the_assigned_candidate() {
    let fixture = linked_worktree_fixture();
    let escaped = fixture.primary.join("escaped.txt");
    let script = fixture.root().join("claude");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'escaped\\n' > '{}'\ngit -C '{}' add -- escaped.txt\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            escaped.display(),
            fixture.primary.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let audit = test_audit("run-deliberate-escape", "claude");

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec_for("claude", Duration::from_secs(5)),
        "test_activity",
        "run-deliberate-escape",
        audit.clone(),
        &worktree_input(&fixture, "ORB-ESCAPE"),
        None,
    )
    .expect("primary snapshots alone do not identify the writer");

    assert!(outcome.success);
    assert!(
        escaped.exists(),
        "diagnosis must not clean the primary delta"
    );
    assert!(
        !fixture.assigned.join("escaped.txt").exists(),
        "diagnosis must not copy the primary delta"
    );
    assert_eq!(
        String::from_utf8(git_bytes(
            &fixture.primary,
            &["diff", "--cached", "--name-only", "HEAD", "--"]
        ))
        .expect("utf8 staged paths")
        .trim(),
        "escaped.txt",
        "diagnosis must not reset the provider-mutated primary index"
    );
    assert!(
        audit
            .events_snapshot()
            .expect("audit events")
            .iter()
            .any(|event| matches!(event.kind, V2AuditEventKind::CliInvocationFinished { .. })),
        "terminal provider audit must precede boundary acceptance"
    );
}

#[test]
fn primary_content_delta_does_not_replace_assigned_content() {
    let fixture = linked_worktree_fixture();
    let escaped = fixture.primary.join("ambiguous-primary.txt");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'assigned\\n' > ambiguous-assigned.txt\nprintf 'primary\\n' > '{}'\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            escaped.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-ambiguous-integrity",
        test_audit("run-ambiguous-integrity", "codex"),
        &worktree_input(&fixture, "ORB-AMBIGUOUS"),
        None,
    )
    .expect("primary dirt is external to the assigned candidate");

    assert!(outcome.success);
    assert!(fixture.assigned.join("ambiguous-assigned.txt").exists());
    assert!(escaped.exists());
}

#[test]
fn dirty_integrity_failure_persists_and_restores_tracked_and_untracked_content() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-RECOVER-DIRTY", "run-recover-dirty");

    fs::write(
        fixture.assigned.join("README.md"),
        b"recover tracked bytes\0\n",
    )
    .expect("write binary tracked candidate");
    let untracked = write_worktree_file(
        &fixture.assigned,
        "nested/untracked.bin",
        "recover untracked bytes\0\n",
    );
    write_primary_file(
        &fixture,
        "src/escaped.rs",
        "fn escaped_primary_write() {}\n",
    );
    git_ok(
        &fixture.primary,
        &["checkout", "-b", "primary-recovery-drift"],
    );

    let error = guard
        .verify()
        .expect_err("a primary branch switch must fail after preserving assigned dirt");
    let diagnostic = worktree_integrity_diagnostic(&error);
    let recovery = &diagnostic["recovery"];
    let tracked_patch = PathBuf::from(
        recovery["tracked_patch"]
            .as_str()
            .expect("tracked patch path"),
    );
    let untracked_payload = PathBuf::from(
        recovery["untracked_payload"]
            .as_str()
            .expect("untracked payload path"),
    );
    let manifest = PathBuf::from(recovery["manifest"].as_str().expect("manifest path"));

    assert!(
        tracked_patch.is_file(),
        "tracked patch is durable before cleanup"
    );
    assert_eq!(
        fs::read(untracked_payload.join("nested/untracked.bin")).expect("read payload"),
        b"recover untracked bytes\0\n"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(manifest).expect("read manifest"))
            .expect("parse manifest");
    assert_eq!(manifest["runId"], "run-recover-dirty");
    assert_eq!(manifest["taskId"], "ORB-RECOVER-DIRTY");
    assert_eq!(
        manifest["untrackedFiles"],
        serde_json::json!(["nested/untracked.bin"])
    );

    git_ok(&fixture.assigned, &["reset", "--hard", "HEAD"]);
    fs::remove_file(&untracked).expect("simulate cleanup of untracked candidate");
    git_ok(
        &fixture.assigned,
        &[
            "apply",
            "--binary",
            tracked_patch.to_str().expect("utf8 patch path"),
        ],
    );
    fs::create_dir_all(fixture.assigned.join("nested")).expect("restore nested payload parent");
    fs::copy(
        untracked_payload.join("nested/untracked.bin"),
        fixture.assigned.join("nested/untracked.bin"),
    )
    .expect("restore untracked payload");

    assert_eq!(
        fs::read(fixture.assigned.join("README.md")).expect("read restored tracked file"),
        b"recover tracked bytes\0\n"
    );
    assert_eq!(
        fs::read(fixture.assigned.join("nested/untracked.bin"))
            .expect("read restored untracked file"),
        b"recover untracked bytes\0\n"
    );
}

#[cfg(unix)]
#[test]
fn dirty_integrity_recovery_preserves_untracked_symlinks_without_following_them() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-RECOVER-LINKS", "run-recover-links");
    let secret = fixture.root().join("outside-secret");
    fs::write(&secret, "do not copy\n").expect("write outside secret");
    std::os::unix::fs::symlink(&secret, fixture.assigned.join("secret-link"))
        .expect("link to outside secret");
    std::os::unix::fs::symlink("missing-target", fixture.assigned.join("dangling-link"))
        .expect("dangling link");
    git_ok(&fixture.primary, &["checkout", "-b", "primary-link-drift"]);

    let error = guard
        .verify()
        .expect_err("a primary branch switch must fail after preserving assigned dirt");
    let diagnostic = worktree_integrity_diagnostic(&error);
    let payload = PathBuf::from(
        diagnostic["recovery"]["untracked_payload"]
            .as_str()
            .unwrap_or_else(|| panic!("symlinks must not fail preservation: {diagnostic}")),
    );

    assert_eq!(
        fs::read_link(payload.join("secret-link")).expect("preserved secret link"),
        secret
    );
    assert_eq!(
        fs::read_link(payload.join("dangling-link")).expect("preserved dangling link"),
        PathBuf::from("missing-target")
    );
}

#[test]
fn provider_created_commit_is_a_typed_boundary_failure_without_admission_checks() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-REJECT-COMMIT", "run-reject-commit");

    fs::write(
        fixture.assigned.join("candidate.txt"),
        "provider candidate\n",
    )
    .expect("write provider candidate");
    git_ok(&fixture.assigned, &["add", "--", "candidate.txt"]);
    git_ok(
        &fixture.assigned,
        &[
            "commit",
            "-m",
            "provider commit",
            "-m",
            "Agent-Run: run-reject-commit\nAgent-Task: ORB-REJECT-COMMIT",
        ],
    );

    let error = guard
        .verify()
        .expect_err("provider-created commits are never admissible");
    assert!(matches!(
        error,
        DispatchError::WorktreeIntegrity {
            code: "worktree_content_conflict",
            ..
        }
    ));
    let diagnostic = worktree_integrity_diagnostic(&error);
    let reason = diagnostic["reason"].as_str().expect("typed reason");
    assert!(
        reason.contains("must not create commits or move HEAD"),
        "{reason}"
    );
    assert!(!reason.contains("Agent-Run"), "{reason}");
    assert!(!reason.contains("Agent-Task"), "{reason}");
    assert!(!reason.contains("candidate.txt"), "{reason}");
}

#[test]
fn assigned_history_divergence_is_a_typed_worktree_content_conflict() {
    let fixture = linked_worktree_fixture();
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf 'committed by provider\\n' > assigned-commit.txt\ngit add -- assigned-commit.txt\ngit commit -m assigned-history-change\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-assigned-history-change",
        test_audit("run-assigned-history-change", "codex"),
        &worktree_input(&fixture, "ORB-ASSIGNED-HISTORY"),
        None,
    )
    .expect_err("provider-created worktree history must fail closed");

    assert_worktree_integrity_error(
        &error,
        "worktree_content_conflict",
        (
            "ORB-ASSIGNED-HISTORY",
            "run-assigned-history-change",
            "codex",
        ),
        &fixture,
        "assigned-commit.txt",
    );
    let diagnostic = worktree_integrity_diagnostic(&error);
    assert_eq!(
        diagnostic["changed_paths"],
        serde_json::json!(["assigned-commit.txt"]),
        "worktree conflicts report the run's paths, not primary checkout paths"
    );
}

#[test]
fn non_fast_forward_primary_move_remains_a_typed_drift_failure() {
    let fixture = linked_worktree_fixture();
    fs::write(fixture.primary.join("before-reset.txt"), "before reset\n")
        .expect("write primary commit");
    git_ok(&fixture.primary, &["add", "before-reset.txt"]);
    git_ok(&fixture.primary, &["commit", "-m", "primary before reset"]);
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'assigned survives\\n' > assigned-survives.txt\ngit -C '{}' reset --hard HEAD^\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            fixture.primary.display(),
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-primary-reset",
        test_audit("run-primary-reset", "codex"),
        &worktree_input(&fixture, "ORB-PRIMARY-RESET"),
        None,
    )
    .expect_err("a primary reset must remain fail closed");

    assert_worktree_integrity_error(
        &error,
        "primary_checkout_drift",
        ("ORB-PRIMARY-RESET", "run-primary-reset", "codex"),
        &fixture,
        "before-reset.txt",
    );
    assert!(
        fixture.assigned.join("assigned-survives.txt").exists(),
        "the guard diagnoses without discarding the run candidate"
    );
}

#[test]
fn benign_primary_fast_forward_ignores_primary_dirt_disjoint_from_the_run() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-BENIGN-FF-DIRT", "run-benign-ff-dirt");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    advance_primary(&fixture, "merged-pr.txt");
    let unrelated = fixture.primary.join(".orbit/routines/worktree_gc.yaml");
    fs::create_dir_all(unrelated.parent().expect("routines parent")).expect("create routines dir");
    fs::write(&unrelated, "schemaVersion: 1\n").expect("write unrelated primary dirt");

    let (result, events) = capture_events(|| guard.verify());
    result.expect("an unrelated untracked primary path must not defeat a benign fast-forward");

    assert!(
        events.iter().any(|event| event
            .field("ignored_primary_paths")
            .is_some_and(|paths| paths.contains(".orbit/routines/worktree_gc.yaml"))),
        "the accepted fast-forward must report the ignored primary path: {events:?}"
    );
    assert!(
        unrelated.exists(),
        "the guard must not clean the primary dirt it ignored"
    );
}

#[test]
fn dirty_to_clean_primary_fast_forward_is_accepted() {
    // The F2026-07-172 shape: the primary already carries untracked
    // record-store dirt when the guard captures its "before" state, then a
    // fast-forward commit lands exactly that pre-existing dirty content, so
    // `dirty_paths` moves from non-empty to empty even though no interference
    // occurred (HEAD proven to have advanced, `conflicting_paths` empty).
    let fixture = linked_worktree_fixture();
    let dirty_record = ".orbit/auto_tasks/nightly.yaml";
    write_primary_file(&fixture, dirty_record, "name: nightly\n");

    let guard = boundary_guard(&fixture, "ORB-DIRTY-TO-CLEAN-FF", "run-dirty-to-clean-ff");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    let head_before = git_bytes(&fixture.primary, &["rev-parse", "HEAD"]);
    git_ok(&fixture.primary, &["add", "--", dirty_record]);
    git_ok(
        &fixture.primary,
        &[
            "commit",
            "-m",
            "commit exactly the pre-existing dirty content",
        ],
    );

    let (result, events) = capture_events(|| guard.verify());
    result.expect(
        "a fast-forward that lands exactly the primary's pre-existing dirty content must be accepted",
    );

    assert_ne!(
        git_bytes(&fixture.primary, &["rev-parse", "HEAD"]),
        head_before,
        "the accepted case is defined by a proven fast-forward advance"
    );
    assert!(
        events.iter().any(|event| event
            .field("ignored_primary_paths")
            .is_some_and(|paths| paths.contains(dirty_record))),
        "the accepted fast-forward must report the dirty-to-clean path: {events:?}"
    );
}

#[test]
fn primary_dirt_intersecting_the_run_does_not_defeat_a_fast_forward() {
    for (kind, shared) in [("untracked", "shared-new.txt"), ("tracked", "README.md")] {
        let fixture = linked_worktree_fixture();
        let run_id = format!("run-{kind}-interference");
        let task_id = format!("ORB-{}-INTERFERENCE", kind.to_ascii_uppercase());
        let guard = boundary_guard(&fixture, &task_id, &run_id);

        fs::write(fixture.assigned.join(shared), "run candidate\n").expect("write run candidate");
        advance_primary(&fixture, "merged-pr.txt");
        fs::write(fixture.primary.join(shared), "primary escape\n").expect("write primary escape");

        let primary_before = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);
        guard.verify().unwrap_or_else(|error| {
            panic!("{kind} primary dirt cannot stand in for a remote merge conflict: {error}")
        });
        assert_eq!(
            git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
            primary_before,
            "boundary acceptance must preserve {kind} primary dirt"
        );
    }
}

#[test]
fn stationary_primary_record_store_dirt_disjoint_from_the_run_is_accepted() {
    let fixture = linked_worktree_fixture();
    let tracked_record = ".orbit/auto_tasks/nightly.yaml";
    write_primary_file(&fixture, tracked_record, "name: nightly\n");
    git_ok(&fixture.primary, &["add", "--", tracked_record]);
    git_ok(&fixture.primary, &["commit", "-m", "track an auto-task"]);

    let guard = boundary_guard(&fixture, "ORB-STATIONARY-DIRT", "run-stationary-dirt");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    // The F2026-07-166 shape: an out-of-run curation pass re-serializes an
    // already-tracked record and drops an untracked sibling, all disjoint from
    // the run, while the primary HEAD and branch never move.
    let head_before = git_bytes(&fixture.primary, &["rev-parse", "HEAD"]);
    write_primary_file(&fixture, tracked_record, "name: nightly\nenabled: true\n");
    let untracked_record = write_primary_file(
        &fixture,
        ".orbit/frictions/F-0002/friction.yaml",
        "id: F-0002\n",
    );

    let (result, events) = capture_events(|| guard.verify());
    result.expect("stationary record-store dirt disjoint from the run must not raise drift");

    assert_eq!(
        git_bytes(&fixture.primary, &["rev-parse", "HEAD"]),
        head_before,
        "the accepted case is defined by an unmoved primary HEAD"
    );
    assert!(
        events.iter().any(|event| event
            .field("ignored_primary_paths")
            .is_some_and(|paths| paths.contains(tracked_record)
                && paths.contains(".orbit/frictions/F-0002/friction.yaml"))),
        "the accepted stationary delta must report both ignored primary paths: {events:?}"
    );
    assert!(
        untracked_record.exists(),
        "the guard must not clean the primary dirt it ignored"
    );
}

#[test]
fn stationary_primary_source_edit_is_external_to_the_candidate() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-STATIONARY-SOURCE", "run-stationary-source");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    // Disjoint from the run, but outside Orbit's record store: this is the
    // ORB-10134 escape shape, and no path-disjointness argument may excuse it.
    write_primary_file(&fixture, ".orbit/routines/nightly.yaml", "name: nightly\n");
    write_primary_file(&fixture, "src/escaped.rs", "fn escaped() {}\n");

    let primary_before = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);
    guard
        .verify()
        .expect("source path class does not identify who changed the primary");
    assert_eq!(
        git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
        primary_before,
        "the guard must preserve all primary source dirt"
    );
    assert!(!fixture.assigned.join("src/escaped.rs").exists());
}

#[test]
fn stationary_primary_dirt_intersecting_the_run_is_not_a_remote_conflict() {
    for (kind, shared) in [
        ("untracked", ".orbit/frictions/F-0009/friction.yaml"),
        ("tracked", ".orbit/routines/worktree_gc.yaml"),
    ] {
        let fixture = linked_worktree_fixture();
        if kind == "tracked" {
            write_primary_file(&fixture, shared, "schemaVersion: 1\n");
            git_ok(&fixture.primary, &["add", "--", shared]);
            git_ok(&fixture.primary, &["commit", "-m", "track a routine"]);
        }
        let run_id = format!("run-stationary-{kind}-interference");
        let task_id = format!("ORB-STATIONARY-{}", kind.to_ascii_uppercase());
        let guard = boundary_guard(&fixture, &task_id, &run_id);

        write_worktree_file(&fixture.assigned, shared, "run candidate\n");
        write_primary_file(&fixture, shared, "primary escape\n");

        let primary_before = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);
        guard.verify().unwrap_or_else(|error| {
            panic!("stationary {kind} pathname overlap is not remote integration: {error}")
        });
        assert_eq!(
            git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
            primary_before,
            "the guard must preserve stationary {kind} primary dirt"
        );
    }
}

#[test]
fn stationary_primary_source_dirt_survives_clean_fetched_target_integration() {
    let fixture = linked_worktree_fixture();
    write_primary_file(&fixture, "src/deleted.rs", "pub fn retained() {}\n");
    git_ok(&fixture.primary, &["add", "--", "src/deleted.rs"]);
    git_ok(&fixture.primary, &["commit", "-m", "seed tracked source"]);
    let seeded_head = git_head_text(&fixture.primary);
    git_ok(&fixture.assigned, &["merge", "--ff-only", &seeded_head]);
    let (remote, target_branch) = initialize_remote(&fixture);
    let guard = boundary_guard(&fixture, "ORB-CLEAN-INTEGRATION", "run-clean-integration");

    // Candidate and remote both edit README.md, but at distinct locations.
    // The primary simultaneously accumulates overlapping staged dirt, an
    // unstaged deletion, and an untracked source file while HEAD stays put.
    write_worktree_file(&fixture.assigned, "README.md", "base\ncandidate\n");
    write_primary_file(&fixture, "README.md", "primary-local\nbase\n");
    git_ok(&fixture.primary, &["add", "--", "README.md"]);
    fs::remove_file(fixture.primary.join("src/deleted.rs")).expect("delete tracked primary source");
    write_primary_file(&fixture, "src/untracked.rs", "pub fn external() {}\n");
    advance_remote_file(
        &fixture,
        &remote,
        &target_branch,
        "README.md",
        "remote\nbase\n",
    );

    let primary_status = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);
    let primary_index = git_bytes(&fixture.primary, &["ls-files", "--stage", "-z", "--"]);
    let primary_patch = git_bytes(&fixture.primary, &["diff", "--binary", "HEAD", "--"]);
    guard
        .verify()
        .expect("local primary dirt must defer to fetched-target integration");

    git_ok(&fixture.assigned, &["add", "--", "README.md"]);
    git_ok(&fixture.assigned, &["commit", "-m", "candidate change"]);
    git_ok(&fixture.assigned, &["fetch", "origin", &target_branch]);
    let remote_ref = format!("origin/{target_branch}");
    git_ok(&fixture.assigned, &["rebase", &remote_ref]);

    assert_eq!(
        git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
        primary_status,
        "fetch and candidate integration must preserve primary working contents"
    );
    assert_eq!(
        git_bytes(&fixture.primary, &["ls-files", "--stage", "-z", "--"]),
        primary_index,
        "fetch and candidate integration must preserve the primary index"
    );
    assert_eq!(
        git_bytes(&fixture.primary, &["diff", "--binary", "HEAD", "--"]),
        primary_patch,
        "tracked primary modifications and deletion must remain byte-identical"
    );
    assert_eq!(
        git_bytes(&fixture.assigned, &["show", "HEAD:README.md"]),
        b"remote\nbase\ncandidate\n",
        "same-path nonconflicting candidate and remote edits must integrate"
    );
    assert_eq!(
        git_bytes(&fixture.assigned, &["show", "HEAD:src/deleted.rs"]),
        b"pub fn retained() {}\n",
        "the primary deletion must not enter the candidate"
    );
    assert!(
        !fixture.assigned.join("src/untracked.rs").exists(),
        "untracked primary source must not enter the candidate"
    );
}

#[test]
fn true_candidate_remote_conflict_still_stops_fetched_target_integration() {
    let fixture = linked_worktree_fixture();
    let (remote, target_branch) = initialize_remote(&fixture);
    let guard = boundary_guard(
        &fixture,
        "ORB-CONFLICT-INTEGRATION",
        "run-conflict-integration",
    );

    write_worktree_file(&fixture.assigned, "README.md", "candidate\n");
    write_primary_file(&fixture, "README.md", "primary-local\n");
    git_ok(&fixture.primary, &["add", "--", "README.md"]);
    advance_remote_file(&fixture, &remote, &target_branch, "README.md", "remote\n");
    let primary_status = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);
    let primary_index = git_bytes(&fixture.primary, &["ls-files", "--stage", "-z", "--"]);

    guard
        .verify()
        .expect("local pathname overlap must not be mislabeled as a remote conflict");
    git_ok(&fixture.assigned, &["add", "--", "README.md"]);
    git_ok(
        &fixture.assigned,
        &["commit", "-m", "conflicting candidate"],
    );
    git_ok(&fixture.assigned, &["fetch", "origin", &target_branch]);
    let remote_ref = format!("origin/{target_branch}");
    let rebase = Command::new("git")
        .arg("-C")
        .arg(&fixture.assigned)
        .args(["rebase", &remote_ref])
        .output()
        .expect("run conflicting fetched-target rebase");

    assert!(
        !rebase.status.success(),
        "a true candidate-versus-remote content conflict cannot falsely succeed"
    );
    assert!(
        !git_bytes(&fixture.assigned, &["ls-files", "-u"]).is_empty(),
        "the unmerged index entries are the existing conflict-recovery trigger"
    );
    assert_eq!(
        git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
        primary_status,
        "failed candidate integration must preserve primary contents"
    );
    assert_eq!(
        git_bytes(&fixture.primary, &["ls-files", "--stage", "-z", "--"]),
        primary_index,
        "failed candidate integration must preserve the primary index"
    );
}

#[test]
fn primary_branch_switch_remains_a_typed_drift_failure() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-PRIMARY-BRANCH", "run-primary-branch");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    git_ok(
        &fixture.primary,
        &["checkout", "-b", "orbit-primary-switch"],
    );

    let error = guard
        .verify()
        .expect_err("a primary branch switch must remain fail closed");

    assert_worktree_integrity_error(
        &error,
        "primary_checkout_drift",
        ("ORB-PRIMARY-BRANCH", "run-primary-branch", "codex"),
        &fixture,
        "<branch-ref>",
    );
}

#[test]
fn primary_dirt_does_not_override_nonzero_exit_or_timeout() {
    for (terminal, trailer, timeout) in [
        ("nonzero", "exit 23", Duration::from_secs(5)),
        // The timeout arm's budget has to cover a fork/exec plus one write on a
        // host running the rest of the suite alongside it; the deadline starts
        // before the spawn, not when the child reaches its first statement.
        ("timeout", "sleep 10", Duration::from_secs(4)),
    ] {
        let fixture = linked_worktree_fixture();
        let escaped_name = format!("{terminal}-primary.txt");
        let escaped = fixture.primary.join(&escaped_name);
        let script = fixture.root().join("codex");
        // The escape write comes before the stdin drain deliberately. `cat`
        // blocks until the supervisor's detached writer thread runs and drops
        // the pipe, so draining first makes the child's side effect wait on a
        // parent thread the scheduler owes nothing to. On the timeout arm that
        // stall is charged to the deadline: under full-suite load the child was
        // killed having written nothing, leaving no drift to detect and failing
        // the assertion for a reason the boundary guard had no part in.
        write_executable(
            &script,
            &format!(
                "#!/bin/sh\nprintf '{terminal}\\n' > '{}'\ncat > /dev/null\n{trailer}\n",
                escaped.display()
            ),
        );
        let mut host = TestHost::with_command(script.display().to_string());
        host.workspace_root = Some(fixture.primary.clone());
        let run_id = format!("run-{terminal}-escape");
        let task_id = format!("ORB-{}", terminal.to_ascii_uppercase());

        let outcome = run_cli_backend(
            &host,
            &test_agent_loop_spec(timeout),
            "test_activity",
            &run_id,
            test_audit(&run_id, "codex"),
            &worktree_input(&fixture, &task_id),
            None,
        )
        .expect("primary dirt must not replace the provider's own terminal outcome");

        assert!(
            !outcome.success,
            "{terminal} remains the invocation outcome"
        );
        assert!(
            escaped.exists(),
            "{terminal} primary dirt must remain untouched"
        );
    }
}

pub(super) struct LinkedWorktreeFixture {
    temp: TempDir,
    pub(super) primary: PathBuf,
    pub(super) assigned: PathBuf,
}

impl LinkedWorktreeFixture {
    pub(super) fn root(&self) -> &Path {
        self.temp.path()
    }
}

pub(super) fn linked_worktree_fixture() -> LinkedWorktreeFixture {
    let temp = tempdir().expect("fixture tempdir");
    let primary = temp.path().join("primary");
    let assigned = temp.path().join("assigned");
    fs::create_dir_all(&primary).expect("create primary");
    git_ok(&primary, &["init"]);
    git_ok(&primary, &["config", "user.name", "Orbit Test"]);
    git_ok(
        &primary,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    fs::write(primary.join("README.md"), "base\n").expect("write initial file");
    git_ok(&primary, &["add", "README.md"]);
    git_ok(&primary, &["commit", "-m", "initial"]);
    git_ok(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-integrity-test",
            assigned.to_str().expect("utf8 assigned path"),
        ],
    );

    LinkedWorktreeFixture {
        primary: primary.canonicalize().expect("canonical primary"),
        assigned: assigned.canonicalize().expect("canonical assigned"),
        temp,
    }
}

/// Capture a boundary guard over the fixture's validated linked-worktree pair.
fn boundary_guard(
    fixture: &LinkedWorktreeFixture,
    task_id: &str,
    run_id: &str,
) -> WorktreeBoundaryGuard {
    let input = worktree_input(fixture, task_id);
    let pair =
        validate_declared_worktree_pair(&input, None, run_id, "codex", Some(&fixture.primary))
            .expect("validate declared pair")
            .expect("linked worktree pair");
    WorktreeBoundaryGuard::capture(
        &input,
        None,
        run_id,
        "codex",
        Some(&fixture.assigned),
        Some(&fixture.primary),
        Some(&pair),
    )
    .expect("capture boundary guard")
    .expect("boundary guard enabled")
}

/// Fast-forward the registered primary the way a merged sibling PR does.
fn advance_primary(fixture: &LinkedWorktreeFixture, path: &str) {
    fs::write(fixture.primary.join(path), "merged\n").expect("write merged PR file");
    git_ok(&fixture.primary, &["add", "--", path]);
    git_ok(&fixture.primary, &["commit", "-m", "merge sibling PR"]);
}

fn initialize_remote(fixture: &LinkedWorktreeFixture) -> (PathBuf, String) {
    let remote = fixture.root().join("remote.git");
    let remote_arg = remote.to_str().expect("utf8 remote path");
    git_ok(fixture.root(), &["init", "--bare", remote_arg]);
    git_ok(&fixture.primary, &["remote", "add", "origin", remote_arg]);
    let target_branch =
        String::from_utf8(git_bytes(&fixture.primary, &["branch", "--show-current"]))
            .expect("utf8 target branch")
            .trim()
            .to_string();
    let refspec = format!("HEAD:refs/heads/{target_branch}");
    git_ok(&fixture.primary, &["push", "origin", &refspec]);
    (remote, target_branch)
}

fn advance_remote_file(
    fixture: &LinkedWorktreeFixture,
    remote: &Path,
    target_branch: &str,
    path: &str,
    contents: &str,
) {
    let publisher = fixture.root().join("publisher");
    fs::create_dir(&publisher).expect("create publisher checkout");
    git_ok(&publisher, &["init"]);
    git_ok(&publisher, &["config", "user.name", "Orbit Publisher"]);
    git_ok(
        &publisher,
        &["config", "user.email", "publisher@example.invalid"],
    );
    git_ok(
        &publisher,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("utf8 remote path"),
        ],
    );
    git_ok(&publisher, &["fetch", "origin", target_branch]);
    git_ok(&publisher, &["checkout", "-b", "publisher", "FETCH_HEAD"]);
    write_worktree_file(&publisher, path, contents);
    git_ok(&publisher, &["add", "--", path]);
    git_ok(&publisher, &["commit", "-m", "advance remote target"]);
    let refspec = format!("HEAD:refs/heads/{target_branch}");
    git_ok(&publisher, &["push", "origin", &refspec]);
}

fn git_head_text(repo: &Path) -> String {
    String::from_utf8(git_bytes(repo, &["rev-parse", "HEAD"]))
        .expect("utf8 HEAD")
        .trim()
        .to_string()
}

/// Write a (possibly nested) path inside the registered primary checkout.
fn write_primary_file(fixture: &LinkedWorktreeFixture, path: &str, contents: &str) -> PathBuf {
    write_worktree_file(&fixture.primary, path, contents)
}

fn write_worktree_file(root: &Path, path: &str, contents: &str) -> PathBuf {
    let target = root.join(path);
    fs::create_dir_all(target.parent().expect("nested path has a parent"))
        .expect("create parent dirs");
    fs::write(&target, contents).expect("write checkout file");
    target
}

pub(super) fn git_ok(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} in {} failed: {}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn git_bytes(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} in {} failed: {}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub(super) fn test_audit(run_id: &str, provider: &str) -> Arc<V2AuditWriter> {
    let sink: Arc<dyn AuditSink> = Arc::new(RecordingSink::default());
    Arc::new(V2AuditWriter::new(
        run_id,
        format!("{provider}:test-model"),
        sink,
    ))
}

fn rendered_implement_input_from_asset(
    yaml: &str,
    assigned_root: &Path,
    task_id: &str,
) -> (String, serde_json::Value) {
    let asset = load_job_asset(yaml).expect("committed shipment asset parses");
    assert!(
        matches!(
            asset.name.as_str(),
            "task_local_pipeline" | "task_pr_pipeline"
        ),
        "unexpected shipment asset {}",
        asset.name
    );
    let implement_bundle = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "implement_bundle")
        .expect("shipment asset has implement_bundle");
    let JobV2StepBody::Loop { loop_ } = &implement_bundle.body else {
        panic!("{} implement_bundle must be a loop", asset.name);
    };
    let implement_one = loop_
        .steps
        .iter()
        .find(|step| step.id == "implement_one")
        .expect("shipment asset has implement_one");
    let JobV2StepBody::TargetRef(implement) = &implement_one.body else {
        panic!("{} implement_one must reference an activity", asset.name);
    };
    assert_eq!(implement.target, "activity:agent_implement");
    let template_input = implement
        .default_input
        .as_ref()
        .expect("agent_implement default input");
    for field in ["workspace_path", "repo_root"] {
        assert_eq!(
            template_input[field], "{{ steps.worktree.output.workspace_path }}",
            "{} must source {field} from the committed worktree output",
            asset.name
        );
    }

    let assigned = assigned_root.display().to_string();
    let mut steps = HashMap::new();
    steps.insert(
        "worktree".to_string(),
        serde_json::json!({ "output": { "workspace_path": assigned } }),
    );
    let context = TemplateContext {
        item: Some(serde_json::json!(task_id)),
        steps: std::sync::Arc::new(steps),
        ..TemplateContext::default()
    };
    let rendered = render_asset_value(template_input, &context);
    assert_eq!(rendered["task_id"], task_id);
    assert_eq!(rendered["workspace_path"], assigned);
    assert_eq!(rendered["repo_root"], assigned);
    (asset.name, rendered)
}

fn render_asset_value(value: &serde_json::Value, context: &TemplateContext) -> serde_json::Value {
    match value {
        serde_json::Value::String(template_value) if template_value.contains("{{") => {
            serde_json::Value::String(
                template::render(template_value, context).expect("render committed asset input"),
            )
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| render_asset_value(value, context))
                .collect(),
        ),
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), render_asset_value(value, context)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn worktree_input(fixture: &LinkedWorktreeFixture, task_id: &str) -> serde_json::Value {
    serde_json::json!({
        "prompt": "implement",
        "task_id": task_id,
        "workspace_path": fixture.assigned,
        "repo_root": fixture.assigned,
    })
}

fn worktree_integrity_diagnostic(error: &DispatchError) -> serde_json::Value {
    let DispatchError::WorktreeIntegrity { diagnostic, .. } = error else {
        panic!("expected WorktreeIntegrity, got {error:?}");
    };
    serde_json::from_str(diagnostic).expect("worktree integrity diagnostic is JSON")
}

fn assert_worktree_mismatch(
    error: &DispatchError,
    task_id: &str,
    run_id: &str,
    reason_fragment: &str,
) {
    assert!(
        matches!(
            error,
            DispatchError::WorktreeIntegrity {
                code: "worktree_mismatch",
                ..
            }
        ),
        "unexpected mismatch error: {error:?}"
    );
    assert!(error.is_non_retryable());
    let diagnostic = worktree_integrity_diagnostic(error);
    assert_eq!(diagnostic["code"], "worktree_mismatch");
    assert_eq!(diagnostic["task_id"], task_id);
    assert_eq!(diagnostic["run_id"], run_id);
    assert_eq!(diagnostic["provider"], "codex");
    assert!(
        diagnostic["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains(reason_fragment)),
        "unexpected mismatch reason: {}",
        diagnostic["reason"]
    );
}

fn assert_pre_spawn_failure(audit: &V2AuditWriter, marker: &Path) {
    assert!(!marker.exists(), "provider process must not have started");
    assert!(
        !audit
            .events_snapshot()
            .expect("audit events")
            .iter()
            .any(|event| matches!(event.kind, V2AuditEventKind::CliInvocationStarted { .. })),
        "mismatch must be rejected before cli.invocation.started"
    );
}

fn assert_worktree_integrity_error(
    error: &DispatchError,
    expected_code: &str,
    identity: (&str, &str, &str),
    fixture: &LinkedWorktreeFixture,
    changed_path: &str,
) {
    let (task_id, run_id, provider) = identity;
    assert!(
        matches!(
            error,
            DispatchError::WorktreeIntegrity { code, .. } if *code == expected_code
        ),
        "unexpected integrity error: {error:?}"
    );
    let rendered = error.to_string();
    for expected in [
        expected_code,
        task_id,
        run_id,
        provider,
        &fixture.assigned.display().to_string(),
        &fixture.primary.display().to_string(),
        changed_path,
        "assigned_before",
        "assigned_after",
        "primary_before",
        "primary_after",
    ] {
        assert!(
            rendered.contains(expected),
            "integrity diagnostic missing {expected:?}: {rendered}"
        );
    }
}
