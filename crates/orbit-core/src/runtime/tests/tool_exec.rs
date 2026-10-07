//! Sibling tests for `tool_exec.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::OrbitRuntime;
use crate::runtime::tool_exec::populate_filesystem_policy_context;
use orbit_tools::ToolContext;
use tempfile::TempDir;

/// Security invariant: nested persisted identities must not compare scrubbed
/// values, while scrubbed diagnostic prose remains valid output. Crafted
/// nested replies exercise fields unavailable from the read-only probe.
#[test]
fn corrupted_drain_identities_are_detected_in_nested_replies() {
    use crate::runtime::tool_exec::corrupted_drain_identity;
    use serde_json::json;

    for field in [
        "protocol_fingerprint",
        "caller_fingerprint",
        "commit",
        "tree",
        "sha256",
        "reviewed_head_sha",
        "reviewed_base_sha",
        "reviewer_commit",
        "covering_commit",
        "final_candidate_tree",
        "head_sha",
        "task_spec_digest",
    ] {
        let reply = json!({"receipt": [{"candidate": {field: "ab[REDACTED_ENV]cd"}}]});
        assert_eq!(
            corrupted_drain_identity(&reply),
            Some(field),
            "protect {field}"
        );
    }
    let reply = json!({"receipt": {"examined_commits": ["intact", "ab[REDACTED_ENV]cd"]}});
    assert_eq!(corrupted_drain_identity(&reply), Some("examined_commits"));
    assert_eq!(
        corrupted_drain_identity(&json!({"diagnostics": ["[REDACTED_ENV]"],
        "candidate": {"commit": "a".repeat(40), "tree": "b".repeat(40)}})),
        None
    );
}

/// A source-inspection slot is a standalone repository, so it never shares the
/// runtime's common Git directory. [ORB-13800] It is still this repository's
/// pinned checkout, so registered tools run there; a repository merely planted
/// in or beside the pool still falls back to the registered root.
#[test]
fn root_resolution_uses_owned_inspection_slot_and_rejects_planted_checkouts() {
    let fixture = GitRuntimeFixture::new();
    let repo_root = canonical(&fixture.repo_root);
    let revision = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    let pool = repo_root.join(".orbit/state/source-inspections-v1");

    let slot = inspection_slot(&pool.join("0"), &repo_root.join(".git"), &revision);
    let subdirectory = slot.join("subdirectory");
    std::fs::create_dir(&subdirectory).expect("create slot subdirectory");
    for cwd in [&slot, &subdirectory] {
        assert_eq!(
            resolved_root(&fixture.runtime, cwd),
            Some(slot.clone()),
            "an owned inspection slot must be the process root"
        );
    }

    let foreign = inspection_slot(
        &pool.join("1"),
        &canonical(&fixture.unrelated).join(".git"),
        &git_stdout(&fixture.unrelated, &["rev-parse", "HEAD"]),
    );
    let unowned = inspection_slot(&pool.join("2"), &repo_root.join(".git"), &revision);
    std::fs::remove_file(pool.join("2/owner")).expect("drop owner marker");
    let outside_pool = inspection_slot(
        &repo_root.join(".orbit/state/elsewhere/0"),
        &repo_root.join(".git"),
        &revision,
    );
    for (cwd, case) in [
        (&foreign, "a slot holding another repository's history"),
        (&unowned, "a slot without its owner marker"),
        (&outside_pool, "a checkout outside the inspection pool"),
    ] {
        assert_eq!(
            resolved_root(&fixture.runtime, cwd),
            Some(repo_root.clone()),
            "{case} must not become the process root"
        );
    }
}

fn resolved_root(runtime: &OrbitRuntime, cwd: &Path) -> Option<PathBuf> {
    let mut context = cwd_context(cwd, None, None);
    populate_filesystem_policy_context(runtime, &mut context).expect("populate context");
    context.workspace_root
}

/// The slot layout the CLI runner materializes: a standalone repository
/// fetched at `revision` and detached, beside its owner marker.
fn inspection_slot(slot: &Path, source_git_dir: &Path, revision: &str) -> PathBuf {
    let checkout = slot.join("checkout");
    std::fs::create_dir_all(&checkout).expect("create slot checkout");
    std::fs::write(slot.join("owner"), "orbit-source-inspection-v1\n").expect("owner marker");
    git(&checkout, &["init", "--quiet", "--template="]);
    git(
        &checkout,
        &[
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            source_git_dir.to_str().expect("utf8 git dir"),
            revision,
        ],
    );
    git(&checkout, &["checkout", "--quiet", "--detach", revision]);
    canonical(&checkout)
}

struct GitRuntimeFixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo_root: PathBuf,
    unrelated: PathBuf,
}

impl GitRuntimeFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let global_root = root.path().join("global");
        let repo_root = root.path().join("repo");
        let unrelated = root.path().join("unrelated");
        std::fs::create_dir_all(&global_root).expect("global");
        std::fs::create_dir_all(repo_root.join(".orbit")).expect("workspace");
        std::fs::create_dir_all(&unrelated).expect("unrelated");

        git(&repo_root, &["init", "-b", "agent-main"]);
        git(&repo_root, &["config", "user.name", "Orbit Test"]);
        git(
            &repo_root,
            &["config", "user.email", "orbit-test@example.com"],
        );
        git(&repo_root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo_root.join("README.md"), "init\n").expect("write");
        git(&repo_root, &["add", "README.md"]);
        git(&repo_root, &["commit", "-m", "init"]);

        git(&unrelated, &["init", "-b", "other"]);
        git(&unrelated, &["config", "user.name", "Orbit Test"]);
        git(
            &unrelated,
            &["config", "user.email", "orbit-test@example.com"],
        );
        git(&unrelated, &["config", "commit.gpgsign", "false"]);
        std::fs::write(unrelated.join("other.txt"), "other\n").expect("write unrelated");
        git(&unrelated, &["add", "other.txt"]);
        git(&unrelated, &["commit", "-m", "unrelated"]);

        let runtime = OrbitRuntime::from_roots(&global_root, &repo_root.join(".orbit"))
            .expect("build runtime");
        Self {
            _root: root,
            runtime,
            repo_root,
            unrelated,
        }
    }
}

fn cwd_context(
    cwd: &Path,
    orbit_host: Option<std::sync::Arc<dyn orbit_tools::OrbitToolHost>>,
    workspace_root: Option<PathBuf>,
) -> ToolContext {
    ToolContext {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        orbit_host,
        workspace_root,
        ..Default::default()
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn git(current_dir: &Path, args: &[&str]) {
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
}

fn git_stdout(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
