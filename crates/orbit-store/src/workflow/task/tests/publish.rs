//! Deterministic interleaving and interrupted-save recovery for publication.
//!
//! The pending record is written before the push. When that push never lands,
//! the branch stays at the record's parent. That permits another CAS, but does
//! not permit deleting evidence: the original push could still be in flight.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use tempfile::TempDir;

use super::super::publish::{PendingWriteStage, publish_task_snapshot_inner};
use super::super::{
    AttachmentPolicy, AttachmentPolicyKind, PublicationCallerRole, PublicationLastSuccess,
    PublicationPublishOutcome, PublicationPublishRequest, PublicationPublishStatus,
    ScannerFailureBehavior, publish_task_snapshot,
};
use super::{bundle_store, make_bundle, open_registry, seed};
use crate::driver::sqlite::task_registry::{BindWorkspaceParams, TaskRegistryStore};

const WORKSPACE_ID: &str = "ws_pub_pending";
const PUBLICATION_ID: &str = "pub_pending";
const FINGERPRINT: &str = "ssh://source.test/orbit.git";
const AUTHORITY: &str = "hm_pub_owner";
const BRANCH: &str = "main";

struct Fixture {
    registry: TaskRegistryStore,
    bare: PathBuf,
    cache: PathBuf,
    _root: TempDir,
}

impl Fixture {
    fn open() -> Self {
        let root = TempDir::new().expect("tempdir");
        let bare = root.path().join("publication.git");
        let cache = root.path().join("cache");
        fs::create_dir_all(&cache).expect("cache dir");
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--quiet",
                "-b",
                BRANCH,
                bare.to_str().expect("bare path"),
            ],
        );
        let registry = open_registry(root.path());
        let orbit_dir = root.path().join("repos").join(WORKSPACE_ID).join(".orbit");
        fs::create_dir_all(&orbit_dir).expect("orbit dir");
        let checkout = orbit_dir.parent().expect("checkout").to_path_buf();
        registry
            .bind_workspace(BindWorkspaceParams {
                partition_id: Some(WORKSPACE_ID.to_string()),
                slug: "sample".to_string(),
                repo_root: checkout.clone(),
                workspace_path: checkout,
                orbit_dir,
                repo_fingerprint: Some(FINGERPRINT.to_string()),
            })
            .expect("bind workspace");
        Self {
            registry,
            bare,
            cache,
            _root: root,
        }
    }

    fn publish(&self) -> Result<PublicationPublishOutcome, OrbitError> {
        publish_task_snapshot(&self.registry, self.request(), &policy(), None)
    }

    fn request(&self) -> PublicationPublishRequest {
        let remote = self.bare.to_str().expect("remote path").to_string();
        PublicationPublishRequest {
            workspace_id: WORKSPACE_ID.to_string(),
            task_workspace_id: WORKSPACE_ID.to_string(),
            source_repository_fingerprint: FINGERPRINT.to_string(),
            publication_id: PUBLICATION_ID.to_string(),
            authority_machine_id: AUTHORITY.to_string(),
            local_machine_id: AUTHORITY.to_string(),
            caller_role: PublicationCallerRole::Owner,
            publication_remote: remote,
            publication_branch: BRANCH.to_string(),
            cache_dir: self.cache.clone(),
            published_at: Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
            last_success: None,
        }
    }

    fn pending_path(&self, commit: &str) -> PathBuf {
        self.cache
            .join(PUBLICATION_ID)
            .join("publish")
            .join("pending-publications")
            .join(format!("{commit}.yaml"))
    }

    fn add_task(&self) {
        let binding = self
            .registry
            .find_workspace_checkout(WORKSPACE_ID)
            .expect("checkout lookup")
            .expect("bound checkout");
        seed(
            &bundle_store(&self.registry, &binding),
            &self.registry,
            WORKSPACE_ID,
            &make_bundle(
                "ORB-00001",
                "task added after the parent snapshot",
                Vec::new(),
            ),
        );
    }

    fn tip(&self) -> String {
        git_output(&self.bare, &["rev-parse", &format!("refs/heads/{BRANCH}")])
    }
}

/// Admitted unit test: force the push/write/save race through the same workflow
/// as production; the public boundary cannot pause between evidence and CAS.
#[test]
fn concurrent_losing_push_preserves_landed_evidence_across_failed_saves() {
    if !isolated("concurrent_losing_push_preserves_landed_evidence_across_failed_saves") {
        return;
    }
    for initialize in [true, false] {
        let fixture = Fixture::open();
        let mut request = fixture.request();
        if !initialize {
            let parent = fixture.publish().expect("initialize recorded parent");
            request.last_success = Some(last_success(&parent));
            fixture.add_task();
        }
        let (ready_tx, ready_rx) = sync_channel(1);
        let (release_tx, release_rx) = sync_channel(1);
        let (landed_tx, landed_rx) = sync_channel(1);
        let registry = fixture.registry.clone();
        let winner_request = request.clone();
        let (winner, losing_commit) = std::thread::scope(|scope| {
            let winner = scope.spawn(move || {
                let outcome = publish_task_snapshot_inner(
                    &registry,
                    winner_request,
                    &policy(),
                    None,
                    Some(&|stage, commit| {
                        if stage == PendingWriteStage::After {
                            ready_tx
                                .send(commit.to_string())
                                .expect("signal durable write");
                            release_rx
                                .recv_timeout(Duration::from_secs(90))
                                .expect("release winning push");
                        }
                    }),
                )
                .expect("winning push lands");
                landed_tx
                    .send(outcome.clone())
                    .expect("signal landed push without saving last_success");
                outcome
            });
            let landed_commit = ready_rx
                .recv_timeout(Duration::from_secs(90))
                .expect("winning evidence");
            let evidence =
                fs::read(fixture.pending_path(&landed_commit)).expect("durable winning evidence");
            let mut losing_request = request.clone();
            losing_request.published_at += chrono::Duration::seconds(1);
            let losing_commit = RefCell::new(String::new());
            let error = publish_task_snapshot_inner(
                &fixture.registry, losing_request, &policy(), None,
                Some(&|stage, commit| {
                    if stage == PendingWriteStage::Before {
                        assert_ne!(commit, landed_commit, "attempts must stage distinct commits from the same parent");
                        *losing_commit.borrow_mut() = commit.to_string();
                        assert_eq!(fs::read(fixture.pending_path(&landed_commit)).expect("in-flight evidence"), evidence,
                            "observing the parent or an empty remote must not delete an in-flight push's evidence");
                        release_tx.send(()).expect("allow winning push");
                        let landed = landed_rx.recv_timeout(Duration::from_secs(90)).expect("winning push before losing write");
                        assert_eq!(landed.commit_id, landed_commit);
                    }
                }),
            ).expect_err("losing CAS against the old tip must fail");
            assert!(
                error.to_string().contains("moved during publication"),
                "{error}"
            );
            assert_eq!(
                fs::read(fixture.pending_path(&landed_commit)).expect("landed evidence"),
                evidence,
                "a losing push must neither replace nor delete a landed push's recovery evidence"
            );
            (
                winner.join().expect("winning thread"),
                losing_commit.into_inner(),
            )
        });
        assert_eq!(fixture.tip(), winner.commit_id);
        let losing: serde_yaml::Value = serde_yaml::from_slice(
            &fs::read(fixture.pending_path(&losing_commit)).expect("losing evidence"),
        )
        .expect("parse losing evidence");
        assert_eq!(losing["generation"].as_u64(), Some(winner.generation));
        assert_eq!(
            losing["previous_publication"].as_str(),
            winner.previous_publication.as_deref()
        );
        if let Some(parent) = &winner.previous_publication {
            let git_dir = fixture
                .cache
                .join(PUBLICATION_ID)
                .join("publish/origin.git");
            assert_eq!(
                git_output(&git_dir, &["rev-parse", &format!("{losing_commit}^")]),
                *parent
            );
        }
        // Model repeated interrupted/failed saves by keeping last_success stale.
        for _ in 0..3 {
            let retry = publish_task_snapshot_inner(
                &fixture.registry,
                request.clone(),
                &policy(),
                None,
                Some(&|_, _| panic!("reconciliation must not stage another push")),
            )
            .expect("retry reconciles landed commit despite losing evidence");
            assert_eq!(retry.status, PublicationPublishStatus::Reconciled);
            assert_eq!(retry.commit_id, winner.commit_id);
            assert_eq!(retry.generation, winner.generation);
            assert_eq!(fixture.tip(), winner.commit_id);
            assert!(
                fixture.pending_path(&winner.commit_id).is_file(),
                "lost save must remain recoverable"
            );
        }
        // A matching envelope alone is not local proof of a foreign push.
        let foreign = push_descendant(&fixture.bare, &winner.commit_id);
        for last in [request.last_success.clone(), None] {
            let mut foreign_request = request.clone();
            foreign_request.last_success = last;
            let error = publish_task_snapshot(&fixture.registry, foreign_request, &policy(), None)
                .expect_err("unrelated remote history remains an authority conflict");
            assert!(
                error
                    .to_string()
                    .contains("resolve the publication authority"),
                "{error}"
            );
            assert_eq!(fixture.tip(), foreign);
            assert!(fixture.pending_path(&winner.commit_id).is_file());
            assert!(fixture.pending_path(&losing_commit).is_file());
        }
    }
}

fn last_success(outcome: &PublicationPublishOutcome) -> PublicationLastSuccess {
    PublicationLastSuccess {
        generation: outcome.generation,
        commit: outcome.commit_id.clone(),
    }
}

fn policy() -> AttachmentPolicy {
    AttachmentPolicy {
        kind: AttachmentPolicyKind::Omit,
        max_file_bytes: 1024,
        max_total_bytes: 1024,
        deny_patterns: Vec::new(),
        scanner_failure_behavior: ScannerFailureBehavior::AllowUnchecked,
    }
}

fn isolated(function: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_PUBLICATION_CHILD";
    let name = format!(
        "{}::{function}",
        module_path!().split_once("::").expect("module path").1
    );
    if std::env::var(MARKER).as_deref() == Ok(&name) {
        return true;
    }
    let home = TempDir::new().expect("isolated home");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env(MARKER, &name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output = orbit_common::process::run_bounded(&mut command, Duration::from_secs(300))
        .expect("isolated publication test completes");
    orbit_common::test_env::assert_child_test_passed(
        &name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    false
}

fn git(cwd: &Path, args: &[&str]) {
    let output = crate::git_test_support::command()
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is utf-8")
        .trim()
        .to_ascii_lowercase()
}

/// Child commit whose envelope stays a valid publication of the same binding,
/// so observation succeeds and the authority decision is what refuses it.
fn push_descendant(bare: &Path, parent: &str) -> String {
    let checkout = bare
        .parent()
        .expect("bare parent")
        .join("competing-publication");
    git(
        bare.parent().expect("bare parent"),
        &[
            "clone",
            "--quiet",
            bare.to_str().expect("bare path"),
            checkout.to_str().expect("checkout path"),
        ],
    );
    git(&checkout, &["config", "user.name", "Publication Test"]);
    git(
        &checkout,
        &["config", "user.email", "publication-test@example.com"],
    );
    git(&checkout, &["config", "commit.gpgsign", "false"]);
    let envelope_path = checkout.join("orbit-task-publication.yaml");
    let raw = fs::read_to_string(&envelope_path).expect("publication envelope");
    let mut value: serde_yaml::Value = serde_yaml::from_str(&raw).expect("parse envelope");
    let generation = value
        .get("generation")
        .and_then(serde_yaml::Value::as_u64)
        .expect("generation");
    value["generation"] = serde_yaml::to_value(generation + 1).expect("next generation");
    value["previous_publication"] = serde_yaml::to_value(parent).expect("parent commit");
    fs::write(
        &envelope_path,
        serde_yaml::to_string(&value).expect("encode envelope"),
    )
    .expect("write envelope");
    git(&checkout, &["add", "orbit-task-publication.yaml"]);
    git(
        &checkout,
        &["commit", "--quiet", "-m", "competing publication"],
    );
    git(
        &checkout,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("HEAD:refs/heads/{BRANCH}"),
        ],
    );
    git_output(&checkout, &["rev-parse", "HEAD"])
}
