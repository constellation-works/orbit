//! Publication transport tests.
//!
//! Every remote here is a local temporary bare repository driven with a fixed
//! test identity, so the suite depends on no network service and no ambient
//! credential.

use std::path::{Path, PathBuf};

use orbit_types::task::TASK_EVENTS_FILE_NAME;
use tempfile::TempDir;

use super::super::publish::{clear_before_push_hook, set_before_push_hook};
use super::*;

struct BeforePushGuard;

impl Drop for BeforePushGuard {
    fn drop(&mut self) {
        clear_before_push_hook();
    }
}

const FINGERPRINT: &str = "git@github.com:example/orbit-source.git";
const AUTHORITY: &str = "hm_owner";
const PUBLICATION_ID: &str = "pub_orbit_primary";
const BRANCH: &str = "refs/heads/main";

struct Fixture {
    root: TempDir,
    workspace_id: String,
    remote: PathBuf,
    cache: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn registry(&self) -> TaskRegistryStore {
        open_registry(self.root.path())
    }

    fn remote_str(&self) -> String {
        self.remote.to_string_lossy().into_owned()
    }

    /// Commit ids on the publication branch, newest first. Empty when the
    /// branch does not exist.
    fn remote_history(&self) -> Vec<String> {
        let tip = git(
            &self.remote,
            &["for-each-ref", "--format=%(objectname)", BRANCH],
        );
        let tip = tip.trim();
        if tip.is_empty() {
            return Vec::new();
        }
        git(&self.remote, &["rev-list", tip])
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect()
    }

    fn remote_tip(&self) -> Option<String> {
        self.remote_history().first().cloned()
    }

    fn remote_file(&self, commit: &str, path: &str) -> String {
        git(&self.remote, &["show", &format!("{commit}:{path}")])
    }
}

fn owned_workspace(
    registry: &TaskRegistryStore,
    global: &Path,
    workspace_id: &str,
) -> WorkspaceCheckoutBinding {
    let orbit_dir = global.join("repos").join(workspace_id).join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let repo_root = orbit_dir.parent().unwrap().to_path_buf();
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(workspace_id.to_string()),
            slug: "sample".to_string(),
            repo_root: repo_root.clone(),
            workspace_path: repo_root,
            orbit_dir,
            repo_fingerprint: Some(FINGERPRINT.to_string()),
        })
        .expect("bind workspace")
}

fn init_bare_remote(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "--bare", "-b", "main"]);
}

fn fixture(workspace_id: &str) -> Fixture {
    let root = TempDir::new().unwrap();
    let registry = open_registry(root.path());
    let binding = owned_workspace(&registry, root.path(), workspace_id);
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        workspace_id,
        &make_bundle("ORB-00001", "first task", Vec::new()),
    );

    let remote = root.path().join("publication.git");
    init_bare_remote(&remote);

    // The registered source checkout is a real Git repository with its own
    // remote; publication must never touch it.
    let source = binding.repo_root.clone();
    git(&source, &["init", "-b", "main"]);
    fs::write(source.join("README.md"), "source\n").unwrap();
    git(&source, &["add", "README.md"]);
    git(&source, &["commit", "-m", "source"]);
    git(
        &source,
        &[
            "remote",
            "add",
            "origin",
            "https://example.test/orbit-source.git",
        ],
    );

    Fixture {
        workspace_id: workspace_id.to_string(),
        cache: root.path().join("publication-cache"),
        remote,
        source,
        root,
    }
}

fn seed_task(fixture: &Fixture, task_id: &str) {
    let registry = fixture.registry();
    let binding = registry
        .find_workspace_checkout(&fixture.workspace_id)
        .unwrap()
        .unwrap();
    let store = bundle_store(&registry, &binding);
    seed(
        &store,
        &registry,
        &fixture.workspace_id,
        &make_bundle(task_id, task_id, Vec::new()),
    );
}

fn publish_policy() -> AttachmentPolicy {
    AttachmentPolicy {
        kind: AttachmentPolicyKind::Fail,
        max_file_bytes: 1024,
        max_total_bytes: 4096,
        deny_patterns: Vec::new(),
        scanner_failure_behavior: ScannerFailureBehavior::AllowUnchecked,
    }
}

fn request(
    fixture: &Fixture,
    second: u32,
    last_success: Option<&PublicationPublishOutcome>,
) -> PublicationPublishRequest {
    PublicationPublishRequest {
        workspace_id: fixture.workspace_id.clone(),
        task_workspace_id: fixture.workspace_id.clone(),
        source_repository_fingerprint: FINGERPRINT.to_string(),
        publication_id: PUBLICATION_ID.to_string(),
        authority_machine_id: AUTHORITY.to_string(),
        local_machine_id: AUTHORITY.to_string(),
        caller_role: PublicationCallerRole::Owner,
        publication_remote: fixture.remote_str(),
        publication_branch: BRANCH.to_string(),
        cache_dir: fixture.cache.clone(),
        published_at: Utc.with_ymd_and_hms(2026, 8, 30, 1, 2, second).unwrap(),
        last_success: last_success.map(|outcome| PublicationLastSuccess {
            generation: outcome.generation,
            commit: outcome.commit_id.clone(),
        }),
    }
}

fn publish(
    fixture: &Fixture,
    request: PublicationPublishRequest,
) -> Result<PublicationPublishOutcome, OrbitError> {
    publish_task_snapshot(&fixture.registry(), request, &publish_policy(), None)
}

/// Publish once and assert the branch was initialized.
fn publish_first(fixture: &Fixture) -> PublicationPublishOutcome {
    let outcome = publish(fixture, request(fixture, 1, None)).expect("first publication");
    assert_eq!(outcome.status, PublicationPublishStatus::Initialized);
    outcome
}

/// Commit `snapshot` into a clone of the publication remote and push it, the
/// way a competing writer or an operator with repository access would.
fn push_external(fixture: &Fixture, label: &str, snapshot: &Path, amend: bool) -> String {
    let work = fixture.root.path().join(format!("external-{label}"));
    git(
        fixture.root.path(),
        &[
            "clone",
            "--quiet",
            &fixture.remote_str(),
            work.to_str().unwrap(),
        ],
    );
    replace_worktree(&work, snapshot);
    git(&work, &["add", "-A"]);
    if amend {
        git(&work, &["commit", "--amend", "--no-edit"]);
        git(
            &work,
            &["push", "--force", "origin", &format!("HEAD:{BRANCH}")],
        );
    } else {
        git(&work, &["commit", "-m", label]);
        git(&work, &["push", "origin", &format!("HEAD:{BRANCH}")]);
    }
    git(&work, &["rev-parse", "HEAD"])
}

/// Build a publication snapshot tree outside the transport, for tests that need
/// a competing writer with a well-formed lineage.
fn external_snapshot(
    fixture: &Fixture,
    label: &str,
    generation: u64,
    previous: Option<&str>,
) -> PathBuf {
    let destination = fixture.root.path().join(format!("snapshot-{label}"));
    build_publication_snapshot(
        &fixture.registry(),
        &destination,
        PublicationSnapshotMetadata {
            publication_id: PUBLICATION_ID.to_string(),
            workspace_id: fixture.workspace_id.clone(),
            source_repository_fingerprint: FINGERPRINT.to_string(),
            authority_machine_id: AUTHORITY.to_string(),
            generation,
            published_at: Utc.with_ymd_and_hms(2026, 8, 30, 9, 0, 0).unwrap(),
            previous_publication: previous.map(ToOwned::to_owned),
        },
        &publish_policy(),
        None,
    )
    .expect("build external snapshot");
    destination
}

#[test]
fn first_publication_initializes_an_empty_repository() {
    let fixture = fixture("ws_publish_init");
    let outcome = publish_first(&fixture);

    assert_eq!(outcome.generation, 1);
    assert_eq!(outcome.previous_publication, None);
    assert_eq!(outcome.observed_tip, None);
    assert_eq!(outcome.branch, BRANCH);
    assert_eq!(
        fixture.remote_tip().as_deref(),
        Some(outcome.commit_id.as_str())
    );
    assert_eq!(fixture.remote_history().len(), 1);

    let envelope = PublicationEnvelope::from_yaml(
        &fixture.remote_file(&outcome.commit_id, PUBLICATION_ENVELOPE_FILE_NAME),
    )
    .expect("published envelope");
    assert_eq!(envelope.generation, 1);
    assert_eq!(envelope.previous_publication, None);
    assert_eq!(envelope.workspace_id, fixture.workspace_id);
    assert_eq!(envelope.task_ids, vec!["ORB-00001".to_string()]);
}

#[test]
fn a_non_empty_repository_is_never_initialized_or_overwritten() {
    let fixture = fixture("ws_publish_foreign");
    let work = fixture.root.path().join("foreign");
    git(
        fixture.root.path(),
        &[
            "clone",
            "--quiet",
            &fixture.remote_str(),
            work.to_str().unwrap(),
        ],
    );
    fs::write(work.join("README.md"), "not a publication\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-m", "foreign"]);
    git(&work, &["push", "origin", "HEAD:refs/heads/other"]);

    // Refs exist, but not the configured branch.
    let error = publish(&fixture, request(&fixture, 1, None))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("refusing to initialize, adopt, or overwrite"),
        "{error}"
    );

    git(&work, &["push", "origin", &format!("HEAD:{BRANCH}")]);
    let foreign_tip = git(&work, &["rev-parse", "HEAD"]);
    let error = publish(&fixture, request(&fixture, 1, None))
        .unwrap_err()
        .to_string();
    assert!(error.contains(PUBLICATION_ENVELOPE_FILE_NAME), "{error}");
    assert_eq!(fixture.remote_tip(), Some(foreign_tip));
    assert_eq!(fixture.remote_history().len(), 1);
}

#[test]
fn repeat_publication_without_changes_creates_no_commit() {
    let fixture = fixture("ws_publish_noop");
    let first = publish_first(&fixture);

    let repeat = publish(&fixture, request(&fixture, 30, Some(&first))).expect("repeat");
    assert_eq!(repeat.status, PublicationPublishStatus::Unchanged);
    assert_eq!(repeat.commit_id, first.commit_id);
    assert_eq!(repeat.generation, 1);
    assert_eq!(fixture.remote_history(), vec![first.commit_id]);
}

#[test]
fn changed_tasks_advance_the_branch_as_a_linear_fast_forward() {
    let fixture = fixture("ws_publish_advance");
    let first = publish_first(&fixture);
    seed_task(&fixture, "ORB-00002");

    let second = publish(&fixture, request(&fixture, 2, Some(&first))).expect("second publication");
    assert_eq!(second.status, PublicationPublishStatus::Advanced);
    assert_eq!(second.generation, 2);
    assert_eq!(
        second.previous_publication.as_deref(),
        Some(first.commit_id.as_str())
    );
    assert_eq!(
        second.observed_tip.as_deref(),
        Some(first.commit_id.as_str())
    );

    let history = fixture.remote_history();
    assert_eq!(
        history,
        vec![second.commit_id.clone(), first.commit_id.clone()]
    );

    let envelope = PublicationEnvelope::from_yaml(
        &fixture.remote_file(&second.commit_id, PUBLICATION_ENVELOPE_FILE_NAME),
    )
    .expect("published envelope");
    assert_eq!(envelope.generation, 2);
    // The envelope's previous_publication is the Git parent, not a timestamp.
    assert_eq!(
        envelope.previous_publication.as_deref(),
        Some(first.commit_id.as_str())
    );
    assert_eq!(
        envelope.task_ids,
        vec!["ORB-00001".to_string(), "ORB-00002".to_string()]
    );
}

#[test]
fn a_competing_writer_stops_publication_at_the_branch_boundary() {
    let fixture = fixture("ws_publish_competing");
    let first = publish_first(&fixture);

    // Another machine publishing as the same authority moves the branch.
    let snapshot = external_snapshot(&fixture, "gen2", 2, Some(&first.commit_id));
    let competing = push_external(&fixture, "gen2", &snapshot, false);
    assert_eq!(fixture.remote_tip().as_deref(), Some(competing.as_str()));

    seed_task(&fixture, "ORB-00002");
    let error = publish(&fixture, request(&fixture, 2, Some(&first)))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("resolve the publication authority"),
        "{error}"
    );
    assert!(error.contains(&first.commit_id), "{error}");
    assert!(error.contains(&competing), "{error}");

    // The remote keeps its last good commit and gains nothing.
    assert_eq!(fixture.remote_tip(), Some(competing));
    assert_eq!(fixture.remote_history().len(), 2);
}

#[test]
fn manual_tampering_and_mismatched_envelopes_are_refused() {
    let fixture = fixture("ws_publish_tamper");
    let first = publish_first(&fixture);

    let snapshot = external_snapshot(&fixture, "tamper", 1, None);
    let envelope_path = snapshot.join(PUBLICATION_ENVELOPE_FILE_NAME);
    let tampered = fs::read_to_string(&envelope_path)
        .unwrap()
        .replace(PUBLICATION_ID, "pub_other_lineage");
    fs::write(&envelope_path, tampered).unwrap();
    let tampered_tip = push_external(&fixture, "tamper", &snapshot, true);

    let error = publish(&fixture, request(&fixture, 2, Some(&first)))
        .unwrap_err()
        .to_string();
    assert!(error.contains("publication id mismatch"), "{error}");
    assert_eq!(fixture.remote_tip(), Some(tampered_tip));
    assert_eq!(fixture.remote_history().len(), 1);
}

#[test]
fn an_invalid_snapshot_publishes_nothing() {
    let fixture = fixture("ws_publish_invalid");
    let first = publish_first(&fixture);

    let canonical = fixture
        .registry()
        .canonical_task_bundle_path(&fixture.workspace_id, "ORB-00001")
        .unwrap();
    let events = canonical.join(TASK_EVENTS_FILE_NAME);
    let raw = fs::read_to_string(&events).unwrap();
    fs::write(&events, format!("{raw}{{")).unwrap();

    let error = publish(&fixture, request(&fixture, 2, Some(&first)))
        .unwrap_err()
        .to_string();
    assert!(error.contains("ORB-00001"), "{error}");
    assert_eq!(fixture.remote_history(), vec![first.commit_id]);
    assert!(
        !fixture
            .cache
            .join(PUBLICATION_ID)
            .join("publish")
            .join("pending-publication.yaml")
            .exists()
    );
}

#[test]
fn replica_callers_and_stale_owners_may_not_publish() {
    let fixture = fixture("ws_publish_role");

    let mut replica = request(&fixture, 1, None);
    replica.caller_role = PublicationCallerRole::Replica;
    let error = publish(&fixture, replica).unwrap_err().to_string();
    assert!(
        error.contains("only the declared owner may publish"),
        "{error}"
    );

    let mut stale_owner = request(&fixture, 1, None);
    stale_owner.local_machine_id = "hm_other".to_string();
    let error = publish(&fixture, stale_owner).unwrap_err().to_string();
    assert!(error.contains("not local machine 'hm_other'"), "{error}");

    let mut wrong_fingerprint = request(&fixture, 1, None);
    wrong_fingerprint.source_repository_fingerprint =
        "git@github.com:example/other-source.git".to_string();
    let error = publish(&fixture, wrong_fingerprint)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("does not match its registered source remote"),
        "{error}"
    );

    let mut unregistered = request(&fixture, 1, None);
    unregistered.task_workspace_id = "ws_unregistered".to_string();
    let error = publish(&fixture, unregistered).unwrap_err().to_string();
    assert!(error.contains("not registered"), "{error}");

    let mut inside_source = request(&fixture, 1, None);
    inside_source.workspace_id = "ws_logical_publication".to_string();
    inside_source.cache_dir = fixture.source.join(".orbit-publication-cache");
    let error = publish(&fixture, inside_source).unwrap_err().to_string();
    assert!(error.contains("outside the source repository"), "{error}");

    // Nothing reached the remote.
    assert!(fixture.remote_tip().is_none());
}

#[test]
fn an_unreachable_remote_leaves_the_publication_untouched() {
    let fixture = fixture("ws_publish_unreachable");
    let first = publish_first(&fixture);

    let mut unreachable = request(&fixture, 2, Some(&first));
    unreachable.publication_remote = fixture
        .root
        .path()
        .join("missing-publication.git")
        .to_string_lossy()
        .into_owned();
    unreachable.cache_dir = fixture.cache.join("unreachable");
    assert!(publish(&fixture, unreachable).is_err());
    assert_eq!(fixture.remote_history(), vec![first.commit_id]);
}

#[test]
fn an_unrecorded_push_reconciles_by_commit_id_instead_of_republishing() {
    let fixture = fixture("ws_publish_reconcile");
    let first = publish_first(&fixture);

    // The push landed but the owner never persisted its last-success record.
    let reconciled = publish(&fixture, request(&fixture, 2, None)).expect("reconcile");
    assert_eq!(reconciled.status, PublicationPublishStatus::Reconciled);
    assert_eq!(reconciled.commit_id, first.commit_id);
    assert_eq!(reconciled.generation, 1);
    assert_eq!(fixture.remote_history(), vec![first.commit_id.clone()]);

    // Once recorded, publication resumes as an ordinary fast-forward.
    seed_task(&fixture, "ORB-00002");
    let next = publish(&fixture, request(&fixture, 3, Some(&reconciled))).expect("next generation");
    assert_eq!(next.status, PublicationPublishStatus::Advanced);
    assert_eq!(next.generation, 2);
    assert_eq!(
        next.previous_publication.as_deref(),
        Some(first.commit_id.as_str())
    );
    assert_eq!(
        fixture.remote_history(),
        vec![next.commit_id, first.commit_id]
    );
}

#[test]
fn publication_never_touches_the_source_repository_or_canonical_state() {
    let fixture = fixture("ws_publish_readonly");
    let registry = fixture.registry();
    let before_tasks = registry
        .tasks_for_workspace(&fixture.workspace_id)
        .unwrap()
        .len();
    let before_allocator = registry.allocator_next_number().unwrap();
    let canonical = registry
        .canonical_task_bundle_path(&fixture.workspace_id, "ORB-00001")
        .unwrap();
    let before_bundle = tree_bytes(&canonical);
    let before_source = tree_bytes(&fixture.source);
    let before_head = git(&fixture.source, &["rev-parse", "HEAD"]);
    let before_refs = git(&fixture.source, &["show-ref"]);
    let before_remotes = git(&fixture.source, &["remote", "-v"]);
    let before_status = git(&fixture.source, &["status", "--porcelain"]);
    let before_index = git(&fixture.source, &["diff", "--cached", "--name-status"]);

    publish_first(&fixture);
    let mut failing = request(&fixture, 2, None);
    failing.publication_branch = "refs/heads/absent".to_string();
    assert!(publish(&fixture, failing).is_err());

    assert_eq!(
        registry
            .tasks_for_workspace(&fixture.workspace_id)
            .unwrap()
            .len(),
        before_tasks
    );
    assert_eq!(registry.allocator_next_number().unwrap(), before_allocator);
    assert_eq!(tree_bytes(&canonical), before_bundle);
    assert_eq!(tree_bytes(&fixture.source), before_source);
    assert_eq!(git(&fixture.source, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(git(&fixture.source, &["show-ref"]), before_refs);
    assert_eq!(git(&fixture.source, &["remote", "-v"]), before_remotes);
    assert_eq!(
        git(&fixture.source, &["status", "--porcelain"]),
        before_status
    );
    assert_eq!(
        git(&fixture.source, &["diff", "--cached", "--name-status"]),
        before_index
    );
}

fn assert_authority_conflict(error: &str, observed: &str) {
    assert!(
        error.contains("resolve the publication authority"),
        "{error}"
    );
    assert!(error.contains(observed), "{error}");
}

#[test]
fn deleting_the_branch_after_observation_is_an_authority_conflict() {
    let fixture = fixture("ws_publish_delete_race");
    let first = publish_first(&fixture);
    seed_task(&fixture, "ORB-00002");

    let _guard = BeforePushGuard;
    let remote = fixture.remote.clone();
    set_before_push_hook(move || {
        git(&remote, &["update-ref", "-d", BRANCH]);
    });

    let error = publish(&fixture, request(&fixture, 2, Some(&first)))
        .unwrap_err()
        .to_string();
    assert_authority_conflict(&error, &first.commit_id);
    assert!(error.contains("moved during publication"), "{error}");
    assert!(fixture.remote_tip().is_none(), "{}", error);
    assert!(fixture.remote_history().is_empty());
}

#[test]
fn rewinding_the_branch_after_observation_is_an_authority_conflict() {
    let fixture = fixture("ws_publish_rewind_race");
    let first = publish_first(&fixture);
    seed_task(&fixture, "ORB-00002");
    let second = publish(&fixture, request(&fixture, 2, Some(&first))).expect("second");
    seed_task(&fixture, "ORB-00003");

    let _guard = BeforePushGuard;
    let remote = fixture.remote.clone();
    let rewind_to = first.commit_id.clone();
    set_before_push_hook(move || {
        git(&remote, &["update-ref", BRANCH, &rewind_to]);
    });

    let error = publish(&fixture, request(&fixture, 3, Some(&second)))
        .unwrap_err()
        .to_string();
    assert_authority_conflict(&error, &second.commit_id);
    assert!(error.contains("moved during publication"), "{error}");
    assert_eq!(
        fixture.remote_tip().as_deref(),
        Some(first.commit_id.as_str())
    );
    assert_eq!(fixture.remote_history(), vec![first.commit_id.clone()]);
}

#[test]
fn a_concurrent_fast_forward_after_observation_is_an_authority_conflict() {
    let fixture = fixture("ws_publish_ff_race");
    let first = publish_first(&fixture);
    seed_task(&fixture, "ORB-00002");

    let snapshot = external_snapshot(&fixture, "race-ff", 2, Some(&first.commit_id));
    let _guard = BeforePushGuard;
    let root = fixture.root.path().to_path_buf();
    let remote = fixture.remote_str();
    set_before_push_hook(move || {
        let work = root.join("external-race-ff");
        git(
            root.as_path(),
            &["clone", "--quiet", &remote, work.to_str().unwrap()],
        );
        replace_worktree(&work, &snapshot);
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-m", "race-ff"]);
        git(&work, &["push", "origin", &format!("HEAD:{BRANCH}")]);
    });

    let error = publish(&fixture, request(&fixture, 2, Some(&first)))
        .unwrap_err()
        .to_string();
    assert_authority_conflict(&error, &first.commit_id);
    let competing = fixture.remote_tip().expect("competing tip");
    assert_ne!(competing, first.commit_id);
    assert_eq!(fixture.remote_history().len(), 2);
    let envelope = fixture.remote_file(&competing, PUBLICATION_ENVELOPE_FILE_NAME);
    assert!(envelope.contains("generation: 2"), "{envelope}");
}

#[test]
fn a_concurrent_divergent_update_after_observation_is_an_authority_conflict() {
    let fixture = fixture("ws_publish_div_race");
    let first = publish_first(&fixture);
    seed_task(&fixture, "ORB-00002");

    let snapshot = external_snapshot(&fixture, "race-div", 1, None);
    let _guard = BeforePushGuard;
    let root = fixture.root.path().to_path_buf();
    let remote = fixture.remote_str();
    set_before_push_hook(move || {
        let work = root.join("external-race-div");
        git(
            root.as_path(),
            &["clone", "--quiet", &remote, work.to_str().unwrap()],
        );
        replace_worktree(&work, &snapshot);
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--amend", "--no-edit"]);
        git(
            &work,
            &["push", "--force", "origin", &format!("HEAD:{BRANCH}")],
        );
    });

    let error = publish(&fixture, request(&fixture, 2, Some(&first)))
        .unwrap_err()
        .to_string();
    assert_authority_conflict(&error, &first.commit_id);
    let competing = fixture.remote_tip().expect("divergent tip");
    assert_ne!(competing, first.commit_id);
    assert_eq!(fixture.remote_history().len(), 1);
}

#[test]
fn an_unchanged_expected_tip_still_advances_without_rewriting_history() {
    let fixture = fixture("ws_publish_cas_advance");
    let first = publish_first(&fixture);
    seed_task(&fixture, "ORB-00002");

    let _guard = BeforePushGuard;
    set_before_push_hook(|| {});

    let second = publish(&fixture, request(&fixture, 2, Some(&first))).expect("cas advance");
    assert_eq!(second.status, PublicationPublishStatus::Advanced);
    assert_eq!(
        second.observed_tip.as_deref(),
        Some(first.commit_id.as_str())
    );
    assert_eq!(
        fixture.remote_history(),
        vec![second.commit_id.clone(), first.commit_id.clone()]
    );
}

#[test]
fn initializing_requires_the_branch_to_stay_absent() {
    let fixture = fixture("ws_publish_init_cas");
    let _guard = BeforePushGuard;
    let root = fixture.root.path().to_path_buf();
    let remote = fixture.remote_str();
    set_before_push_hook(move || {
        let work = root.join("external-init-cas");
        git(
            root.as_path(),
            &["clone", "--quiet", &remote, work.to_str().unwrap()],
        );
        fs::write(work.join("README.md"), "foreign\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-m", "foreign"]);
        git(&work, &["push", "origin", &format!("HEAD:{BRANCH}")]);
    });

    let error = publish(&fixture, request(&fixture, 1, None))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("resolve the publication authority"),
        "{error}"
    );
    assert!(error.contains("moved during publication"), "{error}");
    let foreign = fixture.remote_tip().expect("foreign tip");
    assert!(!fixture.remote_file(&foreign, "README.md").is_empty());
    assert_eq!(fixture.remote_history().len(), 1);
}

#[test]
fn publication_preserves_crlf_bytes_and_skips_configured_filters() {
    let fixture = fixture("ws_publish_crlf_attr");
    let registry = fixture.registry();
    let binding = registry
        .find_workspace_checkout(&fixture.workspace_id)
        .unwrap()
        .unwrap();
    let store = bundle_store(&registry, &binding);
    let manifest = seed_crlf_gitattributes_attachments(&store, "ORB-00001");
    let expected_sha = manifest
        .files
        .iter()
        .find(|file| file.path == "payload.txt")
        .expect("payload manifest entry")
        .sha256
        .clone();

    let (env, sentinel) = poison_publication_git_filters(fixture.root.path());
    let outcome = publish_task_snapshot(
        &registry,
        request(&fixture, 1, None),
        &include_attachment_policy(),
        None,
    )
    .expect("publish crlf attachments");
    let inspection = inspect_publication(PublicationInspectRequest {
        workspace_id: fixture.workspace_id.clone(),
        source_repository_fingerprint: FINGERPRINT.to_string(),
        publication_id: PUBLICATION_ID.to_string(),
        authority_machine_id: AUTHORITY.to_string(),
        publication_remote: fixture.remote_str(),
        publication_branch: BRANCH.to_string(),
        cache_dir: fixture.root.path().join("inspect-cache"),
        commit: None,
    })
    .expect("inspect published crlf snapshot");
    drop(env);

    assert_eq!(outcome.status, PublicationPublishStatus::Initialized);
    assert_eq!(inspection.label.commit_id, outcome.commit_id);
    assert!(
        !sentinel.exists(),
        "publication or inspection executed an external Git filter"
    );

    let payload_spec = format!(
        "{}:tasks/ORB-00001/artifacts/files/payload.txt",
        outcome.commit_id
    );
    assert_eq!(
        git_binary(&fixture.remote, &["cat-file", "blob", &payload_spec]),
        CRLF_PAYLOAD
    );
    let attrs_spec = format!(
        "{}:tasks/ORB-00001/artifacts/files/.gitattributes",
        outcome.commit_id
    );
    assert_eq!(
        git_binary(&fixture.remote, &["cat-file", "blob", &attrs_spec]),
        GITATTRIBUTES_CRLF
    );
    let published_manifest = fixture.remote_file(
        &outcome.commit_id,
        "tasks/ORB-00001/artifacts/manifest.yaml",
    );
    assert!(
        published_manifest.contains(&expected_sha),
        "{published_manifest}"
    );
    assert_eq!(
        expected_sha,
        format!("{:x}", sha2::Sha256::digest(CRLF_PAYLOAD))
    );
    let inspected = fixture
        .root
        .path()
        .join("inspect-cache")
        .join(PUBLICATION_ID)
        .join("tree/tasks/ORB-00001/artifacts/files/payload.txt");
    assert_eq!(fs::read(inspected).unwrap(), CRLF_PAYLOAD);
}
