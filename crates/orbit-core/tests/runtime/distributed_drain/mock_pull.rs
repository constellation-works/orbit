//! Pull admission freezes a `verify-mock-ui` task's mock digests on the
//! owner, in the same commit as the claim (visual-review §2.4).

use orbit_types::task::HostOs;
use orbit_types::task::{MOCK_DIGESTS_ARTIFACT, MockDigestRecord, TaskArtifact, mock_sha256_hex};

use super::*;

const MANIFEST: &str = "schema_version = 1\n\n[app.web]\nthemes = [\"light\", \"dark\"]\n\n[[view]]\nid = \"checkout\"\napp = \"web\"\n";
const PNG_ONE: &[u8] = b"\x89PNG\r\n\x1a\nmock-one";

fn git_output(repo: &Path, args: &[&str], stdin: Option<&str>) -> String {
    let mut command = crate::git_test_support::command();
    command
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Orbit Test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.com")
        .env("GIT_COMMITTER_NAME", "Orbit Test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.com");
    let output = if let Some(stdin) = stdin {
        use std::io::Write;
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn git");
        child
            .stdin
            .as_mut()
            .expect("git stdin")
            .write_all(stdin.as_bytes())
            .expect("write git stdin");
        child.wait_with_output().expect("git output")
    } else {
        command.output().expect("git output")
    };
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout")
        .trim()
        .to_string()
}

/// Commit `docs/views.toml` into the owner fixture's bare boundary repo so
/// mock admission can read the manifest at the base revision.
fn seed_view_manifest(owner_repo: &Path) {
    let git_dir = owner_repo.join(".git");
    let git_dir = git_dir.to_string_lossy().into_owned();
    std::fs::create_dir_all(owner_repo.join("docs")).unwrap();
    std::fs::write(owner_repo.join("docs/views.toml"), MANIFEST).unwrap();
    let blob = git_output(
        owner_repo,
        &[
            "--git-dir",
            &git_dir,
            "hash-object",
            "-w",
            "docs/views.toml",
        ],
        None,
    );
    let docs_tree = git_output(
        owner_repo,
        &["--git-dir", &git_dir, "mktree"],
        Some(&format!("100644 blob {blob}\tviews.toml\n")),
    );
    let root_tree = git_output(
        owner_repo,
        &["--git-dir", &git_dir, "mktree"],
        Some(&format!("040000 tree {docs_tree}\tdocs\n")),
    );
    let commit = git_output(
        owner_repo,
        &[
            "--git-dir",
            &git_dir,
            "commit-tree",
            &root_tree,
            "-m",
            "seed view manifest",
        ],
        None,
    );
    git_output(
        owner_repo,
        &[
            "--git-dir",
            &git_dir,
            "update-ref",
            "refs/heads/main",
            &commit,
        ],
        None,
    );
    git_output(
        owner_repo,
        &[
            "--git-dir",
            &git_dir,
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
        None,
    );
}

/// A follower with a verified kit claims the owner's `verify-mock-ui`
/// task, and the owner holds the frozen digest baseline for that claim
/// before the leaf implements anything.
fn pull_admission_freezes_mock_digests_on_the_owner() {
    let mut pair = Pair::with_owner_config(
        "[visual_review]\nmanifests = [\"docs/views.toml\"]\n",
        &[None],
    );
    let task = pair.tasks[0].clone();
    seed_view_manifest(&pair.owner_repo);
    pair.wire
        .owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "tags": ["verify-mock-ui"], "model": "codex"}),
        )
        .expect("tag owner task");
    pair.wire
        .owner
        .update_task_as_human(
            &task,
            orbit_core::application::task::TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: "mocks/checkout.png".to_string(),
                    content: PNG_ONE.to_vec(),
                    media_type: "image/png".to_string(),
                    created_by: None,
                }],
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .expect("attach mock as operator");
    assert!(
        pair.wire
            .owner
            .get_task_artifacts(&task)
            .unwrap()
            .iter()
            .all(|artifact| artifact.path != MOCK_DIGESTS_ARTIFACT),
        "no baseline before admission"
    );

    let kit_home = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    super::super::host_ui_routing::write_kit(kit_home.path(), true);
    pair.follower = pair
        .follower
        .clone()
        .with_host_os(Some(HostOs::Linux))
        .with_browser_kit_home(kit_home.path());

    let drain = pair.start_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), task);

    let record: MockDigestRecord = pair
        .wire
        .owner
        .get_task_artifacts(&task)
        .unwrap()
        .into_iter()
        .find(|artifact| artifact.path == MOCK_DIGESTS_ARTIFACT)
        .map(|artifact| serde_json::from_slice(&artifact.content).expect("digest record"))
        .expect("pull admission froze mock digests on the owner");
    assert_eq!(record.mocks.len(), 1);
    assert_eq!(record.mocks[0].mock.artifact, "mocks/checkout.png");
    assert_eq!(record.mocks[0].sha256, mock_sha256_hex(PNG_ONE));
    assert!(
        record
            .admission_id
            .as_deref()
            .is_some_and(|id| id.starts_with("claim-request:")),
        "baseline is keyed to the pull admission: {record:?}"
    );
}

crate::scenario_table!(
    pull_admission_freezes_mock_digests_on_the_owner_scenarios,
    pull_admission_freezes_mock_digests_on_the_owner
);
