//! A failed publication push must not be mistaken for an authority conflict.
//!
//! The pending record is written before the push. When that push never lands,
//! the branch stays at the record's parent. With no last-success (a rebind
//! clears it), that parent is proof the push did not land.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use tempfile::TempDir;

use super::super::{
    AttachmentPolicy, AttachmentPolicyKind, PublicationCallerRole, PublicationPublishOutcome,
    PublicationPublishRequest, PublicationPublishStatus, ScannerFailureBehavior,
    publish_task_snapshot,
};
use super::{bundle_store, make_bundle, open_registry, seed};
use crate::driver::sqlite::task_registry::{BindWorkspaceParams, TaskRegistryStore};

const WORKSPACE_ID: &str = "ws_pub_pending";
const PUBLICATION_ID: &str = "pub_pending";
const FINGERPRINT: &str = "ssh://source.test/orbit.git";
const AUTHORITY: &str = "hm_pub_owner";
const BRANCH: &str = "main";
/// Valid commit id that is never the branch tip in these fixtures.
const UNLANDED_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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
        let remote = self.bare.to_str().expect("remote path").to_string();
        publish_task_snapshot(
            &self.registry,
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
            },
            &AttachmentPolicy {
                kind: AttachmentPolicyKind::Omit,
                max_file_bytes: 1024,
                max_total_bytes: 1024,
                deny_patterns: Vec::new(),
                scanner_failure_behavior: ScannerFailureBehavior::AllowUnchecked,
            },
            None,
        )
    }

    fn tip(&self) -> String {
        git_output(&self.bare, &["rev-parse", &format!("refs/heads/{BRANCH}")])
    }

    /// Replace the transport's pending record with a push that names `previous`
    /// as its parent and never appears on the branch.
    fn write_unlanded_pending(&self, previous: &str) {
        let path = self
            .cache
            .join(PUBLICATION_ID)
            .join("publish")
            .join("pending-publication.yaml");
        let document = format!(
            "format_version: 1\n\
             publication_id: {PUBLICATION_ID}\n\
             workspace_id: {WORKSPACE_ID}\n\
             publication_branch: refs/heads/{BRANCH}\n\
             generation: 2\n\
             commit: {UNLANDED_COMMIT}\n\
             previous_publication: {previous}\n"
        );
        fs::write(&path, document).expect("write pending publication");
    }
}

#[test]
fn failed_push_whose_parent_is_still_the_tip_publishes_from_that_tip() {
    let fixture = Fixture::open();
    let initialized = fixture
        .publish()
        .expect("empty publication repository initializes");
    assert_eq!(initialized.status, PublicationPublishStatus::Initialized);
    let tip = fixture.tip();
    assert_eq!(tip, initialized.commit_id);
    assert_ne!(tip, UNLANDED_COMMIT);

    let binding = fixture
        .registry
        .find_workspace_checkout(WORKSPACE_ID)
        .expect("checkout lookup")
        .expect("bound checkout");
    let store = bundle_store(&fixture.registry, &binding);
    seed(
        &store,
        &fixture.registry,
        WORKSPACE_ID,
        &make_bundle("ORB-00001", "task added after the failed push", Vec::new()),
    );
    fixture.write_unlanded_pending(&tip);

    let advanced = fixture.publish().expect(
        "ORB-14125: pending {commit C, previous T} with the branch still at T must publish from T",
    );
    assert_eq!(advanced.status, PublicationPublishStatus::Advanced);
    assert_eq!(advanced.generation, 2);
    assert_eq!(advanced.previous_publication.as_deref(), Some(tip.as_str()));
    assert_eq!(advanced.observed_tip.as_deref(), Some(tip.as_str()));
    assert_ne!(advanced.commit_id, tip);
    assert_ne!(advanced.commit_id, UNLANDED_COMMIT);
    assert_eq!(fixture.tip(), advanced.commit_id);
    assert_eq!(
        git_output(
            &fixture.bare,
            &["rev-parse", &format!("{}^", advanced.commit_id)]
        ),
        tip,
        "the new publication commit must be a child of the pre-push tip"
    );
}

#[test]
fn pending_record_refuses_a_tip_that_is_neither_its_commit_nor_its_parent() {
    let fixture = Fixture::open();
    let initialized = fixture
        .publish()
        .expect("empty publication repository initializes");
    let parent = initialized.commit_id;
    let competing = push_descendant(&fixture.bare, &parent);
    assert_ne!(competing, parent);
    assert_ne!(competing, UNLANDED_COMMIT);
    fixture.write_unlanded_pending(&parent);

    let error = fixture.publish().expect_err(
        "ORB-14125: a tip that is neither the pending commit nor its parent stays an authority conflict",
    );
    let message = error.to_string();
    assert!(
        message.contains("is not the commit this owner pushed"),
        "expected an authority conflict, got {message}"
    );
    assert!(
        message.contains(&competing),
        "the refusal must name the observed tip {competing}, got {message}"
    );
    assert_eq!(
        fixture.tip(),
        competing,
        "a refusal must not move the branch"
    );
    let pending = fs::read_to_string(
        fixture
            .cache
            .join(PUBLICATION_ID)
            .join("publish")
            .join("pending-publication.yaml"),
    )
    .expect("authority conflict keeps the pending record");
    assert!(
        pending.contains(UNLANDED_COMMIT),
        "a real conflict must not discard the pending record: {pending}"
    );
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

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
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
    let output = Command::new("git")
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
