#![allow(missing_docs)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use orbit_types::workflow::activity_job::V2AuditEventKind;

use super::super::run_cli_backend;
use super::orchestrator::{
    LinkedWorktreeFixture, git_bytes, git_ok, linked_worktree_fixture, test_audit, worktree_input,
};
use super::test_support::{TestHost, test_agent_loop_spec, write_executable};

#[test]
fn conflict_recovery_host_completes_only_its_checkpointed_rebase() {
    for wrong_checkpoint in [false, true] {
        let recovery = stopped_rebase_fixture(false);
        let script = recovery.fixture.root().join("codex");
        write_executable(
            &script,
            r##"#!/bin/sh
set -eu
cat > /dev/null
printf 'candidate and target\n' > README.md
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{},"error":null}'
"##,
        );
        let mut host = TestHost::with_command(script.display().to_string());
        host.workspace_root = Some(recovery.fixture.primary.clone());
        let mut input = conflict_recovery_input(&recovery);
        if wrong_checkpoint {
            input["target_base_sha"] = serde_json::json!(recovery.original);
        }
        let audit = test_audit("run-rebase-recovery", "codex");
        let outcome = run_cli_backend(
            &host,
            &test_agent_loop_spec(Duration::from_secs(30)),
            "pr_conflict_recovery",
            "run-rebase-recovery",
            audit.clone(),
            &input,
            None,
        );
        if wrong_checkpoint {
            let error = outcome.unwrap_err().to_string();
            assert!(error.contains("existing rebase matching"), "{error}");
            assert!(
                !audit
                    .events_snapshot()
                    .unwrap()
                    .iter()
                    .any(|event| matches!(
                        event.kind,
                        V2AuditEventKind::CliInvocationStarted { .. }
                    ))
            );
            assert!(
                String::from_utf8_lossy(&git_bytes(
                    &recovery.fixture.assigned,
                    &["ls-files", "-u"]
                ))
                .contains("README.md")
            );
        } else {
            assert!(outcome.unwrap().success);
            assert_eq!(
                fs::read_to_string(recovery.fixture.assigned.join("README.md")).unwrap(),
                "candidate and target\n"
            );
            assert_ne!(git_head(&recovery.fixture.assigned), recovery.original);
            git_ok(
                &recovery.fixture.assigned,
                &["merge-base", "--is-ancestor", &recovery.target, "HEAD"],
            );
            assert_eq!(git_head(&recovery.fixture.primary), recovery.target);
            assert_eq!(
                fs::read_to_string(recovery.fixture.assigned.join("candidate.txt")).unwrap(),
                "nonconflicting candidate\n"
            );
            let persisted: serde_json::Value =
                serde_json::from_slice(&fs::read(script.with_extension("sync_base.json")).unwrap())
                    .unwrap();
            assert_eq!(persisted["head_sha"], git_head(&recovery.fixture.assigned));
            assert_eq!(persisted["head_sha_before"], recovery.original);
            assert_eq!(persisted["original_base_sha"], recovery.original_base);
            assert_eq!(persisted["base_sha"], recovery.target);
            assert_eq!(persisted["task_ids"], serde_json::json!(["T-recovery"]));
            assert_eq!(persisted["run_id"], "run-rebase-recovery");
            assert_eq!(persisted["rewritten"], true);
        }
    }
}

#[test]
fn conflict_recovery_refuses_incomplete_or_out_of_scope_agent_edits() {
    for (name, body, expected) in [
        (
            "incomplete",
            "printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"recovered\":true},\"error\":null}'\n",
            "were not repaired",
        ),
        (
            "out-of-scope",
            "printf 'candidate and target\\n' > README.md\nprintf 'unexpected\\n' > EXTRA.md\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
            "outside the authorized conflict set",
        ),
    ] {
        let recovery = stopped_rebase_fixture(false);
        let script = recovery.fixture.root().join("codex");
        write_executable(
            &script,
            &format!("#!/bin/sh\nset -eu\ncat > /dev/null\n{body}"),
        );
        let mut host = TestHost::with_command(script.display().to_string());
        host.workspace_root = Some(recovery.fixture.primary.clone());

        let error = run_cli_backend(
            &host,
            &test_agent_loop_spec(Duration::from_secs(30)),
            "pr_conflict_recovery",
            "run-rebase-recovery",
            test_audit(&format!("run-rebase-recovery-{name}"), "codex"),
            &conflict_recovery_input(&recovery),
            None,
        )
        .expect_err("host must reject incomplete or unrelated edits");

        assert!(error.to_string().contains(expected), "{error}");
        assert_eq!(
            git_head(&recovery.fixture.assigned),
            git_head(&recovery.fixture.primary),
            "stopped rebase HEAD remains at its target until host continuation"
        );
        assert!(!git_bytes(&recovery.fixture.assigned, &["ls-files", "-u"]).is_empty());
    }
}

#[test]
fn conflict_recovery_rechecks_live_ownership_before_git_mutation() {
    for (key, reason) in [
        ("recovery_mutation_denied", "run was cancelled"),
        ("recovery_authorization_denied", "grant_revoked"),
    ] {
        let recovery = stopped_rebase_fixture(false);
        let script = recovery.fixture.root().join("codex");
        write_executable(
            &script,
            "#!/bin/sh\nset -eu\ncat > /dev/null\nprintf 'candidate and target\\n' > README.md\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
        );
        let mut host = TestHost::with_command(script.display().to_string());
        host.workspace_root = Some(recovery.fixture.primary.clone());
        let mut task_context = serde_json::json!({ "id": "T-recovery" });
        task_context[key] = serde_json::json!(reason);
        host.task_context = Some(task_context);

        let error = run_cli_backend(
            &host,
            &test_agent_loop_spec(Duration::from_secs(30)),
            "pr_conflict_recovery",
            "run-rebase-recovery",
            test_audit("run-rebase-recovery-refused", "codex"),
            &conflict_recovery_input(&recovery),
            None,
        )
        .expect_err("stale ownership or revoked authorization must refuse host continuation");

        assert!(error.to_string().contains(reason), "{error}");
        assert!(!git_bytes(&recovery.fixture.assigned, &["ls-files", "-u"]).is_empty());
    }
}

#[test]
fn completion_conflict_recovery_requires_preserved_candidate_and_pr_identity() {
    for stale_candidate in [false, true] {
        let recovery = stopped_rebase_fixture(false);
        let script = recovery.fixture.root().join("codex");
        write_executable(
            &script,
            "#!/bin/sh\nset -eu\ncat > /dev/null\nprintf 'candidate and target\\n' > README.md\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
        );
        let mut host = TestHost::with_command(script.display().to_string());
        host.workspace_root = Some(recovery.fixture.primary.clone());
        let mut input = conflict_recovery_input(&recovery);
        input["failed_step_id"] = serde_json::json!("complete_pr");
        input["activity_name"] = serde_json::json!("pr_complete");
        input["failed_step_input"] = serde_json::json!({
            "head": "orbit-integrity-test",
            "published_head_sha": if stale_candidate {
                &recovery.original_base
            } else {
                &recovery.original
            },
            "completion": "done",
            "pr_number": "1482",
            "base": recovery.base_ref,
            "base_sync": "local",
        });

        let outcome = run_cli_backend(
            &host,
            &test_agent_loop_spec(Duration::from_secs(30)),
            "pr_conflict_recovery",
            "run-rebase-recovery",
            test_audit("run-completion-conflict-recovery", "codex"),
            &input,
            None,
        );
        if stale_candidate {
            let error = outcome.expect_err("changed published candidate must fail before spawn");
            assert!(
                error.to_string().contains("existing rebase matching"),
                "{error}"
            );
            assert!(!git_bytes(&recovery.fixture.assigned, &["ls-files", "-u"]).is_empty());
        } else {
            assert!(outcome.expect("completion recovery").success);
            git_ok(
                &recovery.fixture.assigned,
                &["merge-base", "--is-ancestor", &recovery.target, "HEAD"],
            );
        }
    }
}

#[test]
fn conflict_recovery_reports_additional_conflicts_without_a_second_agent_attempt() {
    let recovery = stopped_rebase_fixture(true);
    let script = recovery.fixture.root().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\nset -eu\ncat > /dev/null\nprintf 'candidate one and target\\n' > README.md\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(recovery.fixture.primary.clone());

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(30)),
        "pr_conflict_recovery",
        "run-rebase-recovery",
        test_audit("run-rebase-recovery-additional", "codex"),
        &conflict_recovery_input(&recovery),
        None,
    )
    .expect_err("a later commit conflict must remain diagnosable");

    let message = error.to_string();
    assert!(
        message.contains("additional_conflicting_paths"),
        "{message}"
    );
    assert!(message.contains("README.md"), "{message}");
    assert!(!git_bytes(&recovery.fixture.assigned, &["ls-files", "-u"]).is_empty());
}

#[test]
fn recovery_cannot_report_success_when_completion_checkpoint_cannot_be_persisted() {
    let recovery = stopped_rebase_fixture(false);
    let script = recovery.fixture.root().join("codex");
    write_executable(
        &script,
        r##"#!/bin/sh
set -eu
cat > /dev/null
printf 'candidate and target\n' > README.md
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{},"error":null}'
"##,
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(recovery.fixture.primary.clone());
    host.task_context = Some(serde_json::json!({"checkpoint_denied": true}));
    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(30)),
        "pr_conflict_recovery",
        "run-rebase-recovery",
        test_audit("run-checkpoint-denied", "codex"),
        &conflict_recovery_input(&recovery),
        None,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("checkpoint storage unavailable"),
        "{error}"
    );
    assert!(!script.with_extension("sync_base.json").exists());
}

// A byte-for-byte Git copy can satisfy the old HEAD/index/ref checks. The
// host must reject its identity before staging, and must reject edits to the
// sequencer instructions even when the three checkpoint fields still match.
#[test]
fn conflict_recovery_rejects_metadata_copy_redirection_and_poisoned_todo() {
    for attack in ["redirect", "replace", "pointer", "todo"] {
        let recovery = stopped_rebase_fixture(false);
        let script = recovery.fixture.root().join("codex");
        let scratch = recovery.fixture.root().join("scratch-git");
        let body = match attack {
            "redirect" => format!(
                "cp -a \"$gitdir\" '{scratch}'\nprintf '%s\\n' \"$common\" > '{scratch}/commondir'\nprintf 'gitdir: %s\\n' '{scratch}' > .git",
                scratch = scratch.display(),
            ),
            "replace" => format!(
                "cp -a \"$gitdir\" '{scratch}'\nmv \"$gitdir\" \"$gitdir.original\"\nmv '{scratch}' \"$gitdir\"",
                scratch = scratch.display(),
            ),
            "pointer" => format!("cp .git '{scratch}'\nmv '{scratch}' .git", scratch = scratch.display()),
            "todo" => "printf 'exec touch poisoned-by-host\\n' >> \"$gitdir/rebase-merge/git-rebase-todo\"".to_string(),
            _ => unreachable!(),
        };
        write_executable(
            &script,
            &format!(
                r##"#!/bin/sh
set -eu
cat > /dev/null
gitdir=$(git rev-parse --absolute-git-dir)
common=$(git rev-parse --path-format=absolute --git-common-dir)
{body}
printf 'candidate and target\n' > README.md
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{}},"error":null}}'
"##
            ),
        );
        let mut host = TestHost::with_command(script.display().to_string());
        host.workspace_root = Some(recovery.fixture.primary.clone());
        let error = run_cli_backend(
            &host,
            &test_agent_loop_spec(Duration::from_secs(30)),
            "pr_conflict_recovery",
            "run-rebase-recovery",
            test_audit("run-metadata-poison", "codex"),
            &conflict_recovery_input(&recovery),
            None,
        )
        .expect_err("host must not consume copied or poisoned metadata");
        assert!(
            error.to_string().contains("changed Git metadata"),
            "{attack}: {error}"
        );
        assert!(!script.with_extension("sync_base.json").exists());
        assert!(!recovery.fixture.assigned.join("poisoned-by-host").exists());
        assert!(!git_bytes(&recovery.fixture.assigned, &["ls-files", "-u"]).is_empty());
    }
}

struct StoppedRebaseFixture {
    fixture: LinkedWorktreeFixture,
    original: String,
    original_base: String,
    base_ref: String,
    target: String,
}

fn stopped_rebase_fixture(additional_candidate_commit: bool) -> StoppedRebaseFixture {
    let fixture = linked_worktree_fixture();
    let original_base = git_head(&fixture.assigned);
    let base_ref = String::from_utf8(git_bytes(&fixture.primary, &["branch", "--show-current"]))
        .unwrap()
        .trim()
        .to_string();
    fs::write(fixture.assigned.join("README.md"), "candidate one\n").unwrap();
    fs::write(
        fixture.assigned.join("candidate.txt"),
        "nonconflicting candidate\n",
    )
    .unwrap();
    git_ok(&fixture.assigned, &["add", "README.md", "candidate.txt"]);
    git_ok(&fixture.assigned, &["commit", "-m", "candidate one"]);
    if additional_candidate_commit {
        fs::write(fixture.assigned.join("README.md"), "candidate two\n").unwrap();
        git_ok(&fixture.assigned, &["add", "README.md"]);
        git_ok(&fixture.assigned, &["commit", "-m", "candidate two"]);
    }
    let original = git_head(&fixture.assigned);
    fs::write(fixture.primary.join("README.md"), "target\n").unwrap();
    git_ok(&fixture.primary, &["add", "README.md"]);
    git_ok(&fixture.primary, &["commit", "-m", "target change"]);
    let target = git_head(&fixture.primary);
    let stopped = Command::new("git")
        .arg("-C")
        .arg(&fixture.assigned)
        .args(["rebase", &target])
        .output()
        .unwrap();
    assert!(!stopped.status.success());
    StoppedRebaseFixture {
        fixture,
        original,
        original_base,
        base_ref,
        target,
    }
}

fn conflict_recovery_input(recovery: &StoppedRebaseFixture) -> serde_json::Value {
    let mut input = worktree_input(&recovery.fixture, "T-recovery");
    input["run_id"] = serde_json::json!("run-rebase-recovery");
    input["failed_step_id"] = serde_json::json!("sync_base");
    input["activity_name"] = serde_json::json!("git_rebase");
    input["recovery_kind"] = serde_json::json!("vcs_conflict");
    input["operation"] = serde_json::json!("git_rebase");
    input["original_base_sha"] = serde_json::json!(recovery.original_base);
    input["target_base_sha"] = serde_json::json!(recovery.target);
    input["conflicting_paths"] = serde_json::json!(["README.md"]);
    input["failed_step_input"] = serde_json::json!({
        "head": "orbit-integrity-test",
        "head_sha": recovery.original,
        "base_ref": recovery.base_ref,
        "base_sha": recovery.target,
    });
    input
}

fn git_head(repo: &Path) -> String {
    String::from_utf8(git_bytes(repo, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_string()
}
