//! Shared publication repository: lineages are branch-scoped.
//!
//! Regression for 2026-10-25: eleven workspaces were bound to one repo, each
//! on its own `ws/<name>` branch with a distinct publication id. The first
//! publish succeeded; every later one was refused because the repository as
//! a whole was not empty. A lineage must initialize when its bound branch
//! is absent, and must still refuse a bound branch it did not create.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use tempfile::TempDir;

use super::support;
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};
use orbit_store::workflow::task::{
    AttachmentPolicy, AttachmentPolicyKind, PublicationCallerRole, PublicationPublishOutcome,
    PublicationPublishRequest, PublicationPublishStatus, ScannerFailureBehavior,
    publish_task_snapshot,
};

const AUTHORITY: &str = "hm_pub_owner";
const BRANCH_A: &str = "ws/alpha";
const BRANCH_B: &str = "ws/beta";
const BRANCH_FOREIGN: &str = "ws/foreign";

struct Lineage {
    workspace_id: &'static str,
    publication_id: &'static str,
    fingerprint: &'static str,
    branch: &'static str,
}

const LINEAGE_A: Lineage = Lineage {
    workspace_id: "ws_shared_alpha",
    publication_id: "pub_shared_alpha",
    fingerprint: "ssh://source.test/alpha.git",
    branch: BRANCH_A,
};
const LINEAGE_B: Lineage = Lineage {
    workspace_id: "ws_shared_beta",
    publication_id: "pub_shared_beta",
    fingerprint: "ssh://source.test/beta.git",
    branch: BRANCH_B,
};

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
        // Start from an empty bare repo whose default branch name is
        // unrelated to either publication branch.
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--quiet",
                "-b",
                "main",
                bare.to_str().expect("bare path"),
            ],
        );
        let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
        for lineage in [&LINEAGE_A, &LINEAGE_B] {
            let orbit_dir = root
                .path()
                .join("repos")
                .join(lineage.workspace_id)
                .join(".orbit");
            fs::create_dir_all(&orbit_dir).expect("orbit dir");
            let checkout = orbit_dir.parent().expect("checkout").to_path_buf();
            registry
                .bind_workspace(BindWorkspaceParams {
                    partition_id: Some(lineage.workspace_id.to_string()),
                    slug: lineage.workspace_id.to_string(),
                    repo_root: checkout.clone(),
                    workspace_path: checkout,
                    orbit_dir,
                    repo_fingerprint: Some(lineage.fingerprint.to_string()),
                })
                .expect("bind workspace");
        }
        Self {
            registry,
            bare,
            cache,
            _root: root,
        }
    }

    fn request(&self, lineage: &Lineage) -> PublicationPublishRequest {
        PublicationPublishRequest {
            workspace_id: lineage.workspace_id.to_string(),
            task_workspace_id: lineage.workspace_id.to_string(),
            source_repository_fingerprint: lineage.fingerprint.to_string(),
            publication_id: lineage.publication_id.to_string(),
            authority_machine_id: AUTHORITY.to_string(),
            local_machine_id: AUTHORITY.to_string(),
            caller_role: PublicationCallerRole::Owner,
            publication_remote: self.bare.to_str().expect("remote path").to_string(),
            publication_branch: lineage.branch.to_string(),
            cache_dir: self.cache.clone(),
            published_at: Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap(),
            last_success: None,
        }
    }

    fn publish(&self, lineage: &Lineage) -> Result<PublicationPublishOutcome, OrbitError> {
        publish_task_snapshot(&self.registry, self.request(lineage), &policy(), None)
    }

    fn tip(&self, branch: &str) -> String {
        git_output(&self.bare, &["rev-parse", &format!("refs/heads/{branch}")])
    }

    /// Push an ordinary (non-publication) commit to `branch` on the remote.
    fn push_foreign_commit(&self, branch: &str) -> String {
        let work = self
            .bare
            .parent()
            .expect("bare parent")
            .join("foreign-seed");
        git(
            self.bare.parent().expect("bare parent"),
            &[
                "init",
                "--quiet",
                "-b",
                "main",
                work.to_str().expect("work path"),
            ],
        );
        git(&work, &["config", "user.name", "Publication Test"]);
        git(
            &work,
            &["config", "user.email", "publication-test@example.com"],
        );
        git(&work, &["config", "commit.gpgsign", "false"]);
        fs::write(work.join("README.md"), "ordinary source history\n").expect("readme");
        git(&work, &["add", "README.md"]);
        git(&work, &["commit", "--quiet", "-m", "ordinary commit"]);
        git(
            &work,
            &[
                "remote",
                "add",
                "origin",
                self.bare.to_str().expect("bare path"),
            ],
        );
        git(
            &work,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("HEAD:refs/heads/{branch}"),
            ],
        );
        git_output(&work, &["rev-parse", "HEAD"])
    }
}

#[test]
fn second_lineage_initializes_on_its_own_branch_in_a_shared_repo() {
    if !isolated("second_lineage_initializes_on_its_own_branch_in_a_shared_repo") {
        return;
    }
    let fixture = Fixture::open();

    let first = fixture
        .publish(&LINEAGE_A)
        .expect("first lineage initializes");
    assert_eq!(first.status, PublicationPublishStatus::Initialized);
    assert_eq!(first.generation, 1);
    assert_eq!(first.branch, format!("refs/heads/{BRANCH_A}"));
    let tip_a = fixture.tip(BRANCH_A);
    assert_eq!(tip_a, first.commit_id);

    // Before the fix this failed with "publication repository is not empty
    // and does not carry branch 'refs/heads/ws/beta'".
    let second = fixture
        .publish(&LINEAGE_B)
        .expect("absent bound branch initializes even though another lineage branch exists");
    assert_eq!(second.status, PublicationPublishStatus::Initialized);
    assert_eq!(second.generation, 1);
    assert_eq!(second.branch, format!("refs/heads/{BRANCH_B}"));
    assert_eq!(second.observed_tip, None);
    assert_ne!(second.commit_id, first.commit_id);
    assert_eq!(fixture.tip(BRANCH_B), second.commit_id);
    assert_eq!(
        fixture.tip(BRANCH_A),
        tip_a,
        "initializing a second lineage must not move the first lineage's branch"
    );
}

#[test]
fn shared_repo_refuses_a_bound_branch_that_is_not_this_lineages_envelope() {
    if !isolated("shared_repo_refuses_a_bound_branch_that_is_not_this_lineages_envelope") {
        return;
    }
    let fixture = Fixture::open();
    let first = fixture
        .publish(&LINEAGE_A)
        .expect("first lineage initializes");
    let tip_a = fixture.tip(BRANCH_A);
    assert_eq!(tip_a, first.commit_id);

    // Foreign ordinary content on the bound branch: refuse, no remote change.
    let foreign_tip = fixture.push_foreign_commit(BRANCH_FOREIGN);
    let foreign_lineage = Lineage {
        workspace_id: LINEAGE_B.workspace_id,
        publication_id: LINEAGE_B.publication_id,
        fingerprint: LINEAGE_B.fingerprint,
        branch: BRANCH_FOREIGN,
    };
    let error = fixture
        .publish(&foreign_lineage)
        .expect_err("a bound branch carrying foreign content must be refused");
    assert!(
        error.to_string().contains("refusing to adopt or overwrite"),
        "expected an authority-conflict refusal, got {error}"
    );
    assert_eq!(fixture.tip(BRANCH_FOREIGN), foreign_tip);
    assert_eq!(fixture.tip(BRANCH_A), tip_a);

    // Another lineage's valid envelope on the bound branch (wrong
    // publication id for this branch): refuse, no remote change.
    let mismatched = Lineage {
        publication_id: "pub_shared_wrong",
        ..LINEAGE_A
    };
    let request = fixture.request(&mismatched);
    let error = publish_task_snapshot(&fixture.registry, request, &policy(), None)
        .expect_err("another publication id on the bound branch must be refused");
    assert!(
        error.to_string().contains("publication id mismatch"),
        "expected an authority-conflict refusal, got {error}"
    );
    assert_eq!(fixture.tip(BRANCH_A), tip_a);
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
    support::isolated(&format!("shared_repo::{function}"))
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
