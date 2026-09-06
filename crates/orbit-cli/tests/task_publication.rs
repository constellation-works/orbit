#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Network-free operator workflow coverage for task publication and recovery.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

const SOURCE_REMOTE: &str = "ssh://source.test/orbit.git";
const PUBLICATION_REMOTE: &str = "ssh://publication.test/orbit-tasks.git";
const PUBLICATION_ID: &str = "pub_network_free_e2e";
const LOGICAL_WORKSPACE_ID: &str = "ws_orbit";
const RUNTIME_WORKSPACE_ID: &str = "ws_orbit-5c61b3";
const RECOVERY_RUNTIME_WORKSPACE_ID: &str = "ws_orbit-recovery";

#[test]
fn operator_workflow_is_network_free_labelled_and_fail_closed() {
    let temp = tempdir().expect("tempdir");
    let owner_home = temp.path().join("owner-home");
    let recovery_home = temp.path().join("recovery-home");
    let source_repo = temp.path().join("source");
    let recovery_repo = temp.path().join("recovery");
    let publication_bare = temp.path().join("publication.git");
    fs::create_dir_all(&owner_home).expect("owner home");
    fs::create_dir_all(&recovery_home).expect("recovery home");
    init_source_repo(&source_repo);
    init_bare_repo(&publication_bare);
    install_fake_ssh(temp.path());

    orbit(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "publication-owner",
            "--task-prefix",
            "PUB",
        ],
    )
    .success();
    orbit(
        &source_repo,
        &owner_home,
        &publication_bare,
        &["workspace", "init", "--name", "orbit-5c61b3"],
    )
    .success();

    // The registry's logical identity may diverge from config.yaml's task
    // partition. Publication authority follows the former; bundle discovery
    // must keep using the latter.
    let registry_path = owner_home.join(".orbit").join("workspaces.json");
    let registry = fs::read_to_string(&registry_path).expect("workspace registry");
    fs::write(
        &registry_path,
        registry.replace(RUNTIME_WORKSPACE_ID, LOGICAL_WORKSPACE_ID),
    )
    .expect("logical workspace registry");
    let runtime_identity = fs::read_to_string(source_repo.join(".orbit").join("config.yaml"))
        .expect("runtime workspace identity");
    assert!(
        runtime_identity.contains(RUNTIME_WORKSPACE_ID),
        "config.yaml must retain the task partition: {runtime_identity}"
    );

    let first = add_task(
        &source_repo,
        &owner_home,
        &publication_bare,
        "First published task",
        "first durable body",
    );
    let second = add_task(
        &source_repo,
        &owner_home,
        &publication_bare,
        "Second published task",
        "second durable body",
    );
    let attachment = temp.path().join("operator-note.txt");
    fs::write(&attachment, "operator-only attachment\n").expect("attachment");
    orbit(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "task",
            "artifact",
            "put",
            &first,
            attachment.to_str().expect("attachment path"),
            "--path",
            "notes/operator-note.txt",
            "--model",
            "codex",
            "--json",
        ],
    )
    .success();

    assert!(
        branch_tip(&publication_bare).is_none(),
        "task mutations must not publish automatically"
    );
    let source_before = repository_state(&source_repo);

    let credential_failure = orbit(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "workspace",
            "publication",
            "bind",
            "--remote",
            "https://operator:supersecret@publication.test/orbit.git",
            "--publication-id",
            PUBLICATION_ID,
            "--json",
        ],
    )
    .failure();
    assert_output_contains(&credential_failure, "***");
    assert_output_excludes(&credential_failure, "supersecret");

    let source_remote_failure = orbit(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "workspace",
            "publication",
            "bind",
            "--remote",
            SOURCE_REMOTE,
            "--publication-id",
            PUBLICATION_ID,
            "--json",
        ],
    )
    .failure();
    assert_output_contains(
        &source_remote_failure,
        "equivalent to the workspace source remote",
    );

    let binding = orbit_json(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "workspace",
            "publication",
            "bind",
            "--remote",
            PUBLICATION_REMOTE,
            "--publication-id",
            PUBLICATION_ID,
            "--json",
        ],
    );
    assert_eq!(binding["bound"], true);
    assert_eq!(binding["workspace_id"], LOGICAL_WORKSPACE_ID);
    assert_eq!(binding["privacy"], "operator-managed");
    assert_eq!(binding["publication_remote"], PUBLICATION_REMOTE);
    let workspace_id = binding["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let authority = binding["authority_machine_id"]
        .as_str()
        .expect("authority")
        .to_string();

    let published = orbit_json(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "task",
            "publication",
            "publish",
            "--attachments",
            "omit",
            "--json",
        ],
    );
    assert_eq!(published["status"], "initialized");
    assert_eq!(published["workspace_id"], LOGICAL_WORKSPACE_ID);
    assert_eq!(published["generation"], 1);
    assert!(published["omitted_attachment_bytes"].as_u64().unwrap() > 0);
    let published_commit = published["commit_id"]
        .as_str()
        .expect("published commit")
        .to_string();
    assert_eq!(
        branch_tip(&publication_bare).as_deref(),
        Some(published_commit.as_str())
    );

    let status = orbit_json(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "task",
            "publication",
            "status",
            "--json",
        ],
    );
    assert_eq!(status["state"], "current");
    assert_eq!(status["workspace_id"], LOGICAL_WORKSPACE_ID);
    assert_eq!(status["remote_commit"], published_commit);
    assert_eq!(status["incomplete_attachments"], true);

    let published_paths = git_output(
        &publication_bare,
        &["ls-tree", "-r", "--name-only", "refs/heads/main"],
    );
    assert!(published_paths.contains("orbit-task-publication.yaml"));
    assert!(published_paths.contains(&format!("tasks/{first}/task.yaml")));
    assert!(published_paths.contains(&format!("tasks/{second}/task.yaml")));
    for runtime_only in ["orbit.db", "state/", "claims/", "runs/", "workspaces.json"] {
        assert!(
            !published_paths
                .lines()
                .any(|path| path.contains(runtime_only)),
            "runtime-only state leaked through {runtime_only}: {published_paths}"
        );
    }
    assert_eq!(repository_state(&source_repo), source_before);

    init_source_repo(&recovery_repo);
    orbit(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &[
            "init",
            "--non-interactive",
            "--host-name",
            "replacement-host",
            "--task-prefix",
            "PUB",
        ],
    )
    .success();
    fs::copy(
        owner_home.join(".orbit/host.toml"),
        recovery_home.join(".orbit/host.toml"),
    )
    .expect("preserve recovered authority identity");
    orbit(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &["workspace", "init", "--name", "orbit-recovery"],
    )
    .success();
    let recovery_registry_path = recovery_home.join(".orbit").join("workspaces.json");
    let recovery_registry =
        fs::read_to_string(&recovery_registry_path).expect("recovery workspace registry");
    fs::write(
        &recovery_registry_path,
        recovery_registry.replace(RECOVERY_RUNTIME_WORKSPACE_ID, LOGICAL_WORKSPACE_ID),
    )
    .expect("logical recovery workspace registry");
    let recovery_runtime_identity =
        fs::read_to_string(recovery_repo.join(".orbit").join("config.yaml"))
            .expect("recovery runtime workspace identity");
    assert!(
        recovery_runtime_identity.contains(RECOVERY_RUNTIME_WORKSPACE_ID),
        "recovery config.yaml must retain the task partition: {recovery_runtime_identity}"
    );
    let recovery_workspace = orbit_json(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &["workspace", "show", "--format", "json"],
    );
    assert_eq!(recovery_workspace["workspace"]["id"], workspace_id);
    let recovery_before = repository_state(&recovery_repo);

    let inspect_args = consumer_args(&workspace_id, &authority, "inspect");
    let inspection = orbit_json_owned(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &inspect_args,
    );
    assert_eq!(inspection["label"]["render_authority"], "snapshot");
    assert_eq!(inspection["label"]["generation"], 1);
    assert_eq!(inspection["label"]["freshness"], "current");
    assert_eq!(inspection["label"]["incomplete_attachments"], true);
    assert_eq!(inspection["label"]["workspace_id"], workspace_id);
    assert_eq!(inspection["tasks"].as_array().expect("tasks").len(), 2);
    assert_eq!(
        inspection["omitted_attachments"]
            .as_array()
            .expect("omissions")
            .len(),
        1
    );

    let mut mismatch_args = consumer_args(&workspace_id, &authority, "inspect");
    replace_arg_value(
        &mut mismatch_args,
        "--source-remote",
        "ssh://source.test/other.git",
    );
    let mismatch = orbit_owned(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &mismatch_args,
    )
    .failure();
    assert_output_contains(&mismatch, "source repository fingerprint mismatch");
    assert!(task_ids(&recovery_repo, &recovery_home, &publication_bare).is_empty());

    let restore_without_confirmation = consumer_args(&workspace_id, &authority, "restore");
    let confirmation_gate = orbit_owned(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &restore_without_confirmation,
    )
    .failure();
    assert_output_contains(&confirmation_gate, "--confirm");
    assert!(task_ids(&recovery_repo, &recovery_home, &publication_bare).is_empty());

    let restore_args = consumer_args(&workspace_id, &authority, "restore")
        .into_iter()
        .chain(["--confirm".to_string()])
        .collect::<Vec<_>>();
    let restored = orbit_json_owned(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &restore_args,
    );
    assert_eq!(restored["completeness"], "incomplete-attachments");
    assert_eq!(restored["omitted_attachments"].as_array().unwrap().len(), 1);
    assert_eq!(restored["restored_task_ids"].as_array().unwrap().len(), 2);
    assert_eq!(
        task_ids(&recovery_repo, &recovery_home, &publication_bare),
        [first.clone(), second.clone()]
    );
    assert_eq!(
        fs::read_to_string(recovery_repo.join(".orbit").join("config.yaml"))
            .expect("recovery runtime identity after restore"),
        recovery_runtime_identity,
        "restore must not rewrite the destination task partition identity"
    );
    assert_eq!(repository_state(&recovery_repo), recovery_before);

    let recovered_first = task_show(&recovery_repo, &recovery_home, &publication_bare, &first);
    assert_eq!(recovered_first["title"], "First published task");
    assert_eq!(recovered_first["description"], "first durable body");

    orbit(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &[
            "task",
            "update",
            &first,
            "--title",
            "non-identical recovery collision",
            "--model",
            "codex",
        ],
    )
    .success();
    let second_before_collision =
        task_show(&recovery_repo, &recovery_home, &publication_bare, &second);
    let collision_args = consumer_args(&workspace_id, &authority, "restore")
        .into_iter()
        .chain([
            "--allow-identical-retry".to_string(),
            "--confirm".to_string(),
        ])
        .collect::<Vec<_>>();
    let collision = orbit_owned(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &collision_args,
    )
    .failure();
    assert_output_contains(&collision, "collides with non-identical canonical content");
    assert_eq!(
        task_show(&recovery_repo, &recovery_home, &publication_bare, &second,),
        second_before_collision,
        "failed recovery collision must not partially rewrite another task"
    );

    let competing = advance_valid_generation(
        temp.path(),
        &publication_bare,
        &published_commit,
        "competing",
    );
    assert_eq!(
        branch_tip(&publication_bare).as_deref(),
        Some(competing.as_str())
    );
    let conflict = orbit(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "task",
            "publication",
            "publish",
            "--json",
        ],
    )
    .failure();
    assert_output_contains(&conflict, "resolve the publication authority");
    assert_eq!(
        branch_tip(&publication_bare).as_deref(),
        Some(competing.as_str())
    );

    corrupt_current_snapshot(temp.path(), &publication_bare, &first);
    let corrupt = orbit_owned(
        &recovery_repo,
        &recovery_home,
        &publication_bare,
        &inspect_args,
    )
    .failure();
    assert_output_contains(&corrupt, &first);

    assert_eq!(
        repository_state(&source_repo),
        source_before,
        "publication, conflict, and corrupt inspection must not dirty the source repository"
    );

    let rebound = orbit_json(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "workspace",
            "publication",
            "rebind",
            "--remote",
            PUBLICATION_REMOTE,
            "--publication-id",
            "pub_rebound_e2e",
            "--json",
        ],
    );
    assert_eq!(rebound["action"], "rebound");
    assert_eq!(rebound["last_success_commit"], Value::Null);
    let shown = orbit_json(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "workspace",
            "publication",
            "show",
            "--json",
        ],
    );
    assert_eq!(shown["publication_id"], "pub_rebound_e2e");
    let removed = orbit_json(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "workspace",
            "publication",
            "remove",
            "--confirm",
            "--json",
        ],
    );
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["repository_changed"], false);
    let unbound = orbit_json(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "workspace",
            "publication",
            "show",
            "--json",
        ],
    );
    assert_eq!(unbound["bound"], false);
    let logical_workspace = orbit_json(
        &source_repo,
        &owner_home,
        &publication_bare,
        &[
            "--workspace",
            LOGICAL_WORKSPACE_ID,
            "workspace",
            "show",
            "--format",
            "json",
        ],
    );
    assert_eq!(logical_workspace["workspace"]["id"], LOGICAL_WORKSPACE_ID);
    assert_eq!(
        fs::read_to_string(source_repo.join(".orbit").join("config.yaml"))
            .expect("runtime identity after publication"),
        runtime_identity,
        "publication must not rewrite the task partition identity"
    );
    assert_eq!(repository_state(&source_repo), source_before);
}

fn add_task(repo: &Path, home: &Path, bare: &Path, title: &str, description: &str) -> String {
    orbit_json(
        repo,
        home,
        bare,
        &[
            "task",
            "add",
            "--title",
            title,
            "--description",
            description,
            "--complexity",
            "low",
            "--json",
        ],
    )["id"]
        .as_str()
        .expect("task id")
        .to_string()
}

fn consumer_args(workspace_id: &str, authority: &str, action: &str) -> Vec<String> {
    [
        "task",
        "publication",
        action,
        "--workspace-id",
        workspace_id,
        "--source-remote",
        SOURCE_REMOTE,
        "--publication-id",
        PUBLICATION_ID,
        "--authority-machine-id",
        authority,
        "--remote",
        PUBLICATION_REMOTE,
        "--json",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn replace_arg_value(args: &mut [String], flag: &str, replacement: &str) {
    let index = args.iter().position(|arg| arg == flag).expect("flag") + 1;
    args[index] = replacement.to_string();
}

fn task_ids(repo: &Path, home: &Path, bare: &Path) -> Vec<String> {
    let value = orbit_json(repo, home, bare, &["task", "list", "--json"]);
    let tasks = value
        .as_array()
        .or_else(|| value.get("tasks").and_then(Value::as_array))
        .expect("task list");
    let mut ids = tasks
        .iter()
        .filter_map(|task| task["id"].as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn task_show(repo: &Path, home: &Path, bare: &Path, id: &str) -> Value {
    orbit_json(repo, home, bare, &["task", "show", id, "--json"])
}

fn advance_valid_generation(root: &Path, bare: &Path, previous: &str, label: &str) -> String {
    let checkout = root.join(label);
    git(
        root,
        &["clone", bare.to_str().unwrap(), checkout.to_str().unwrap()],
    );
    configure_git(&checkout);
    let envelope = checkout.join("orbit-task-publication.yaml");
    let raw = fs::read_to_string(&envelope).expect("envelope");
    let next = raw.replace("generation: 1", "generation: 2").replace(
        "previous_publication: null",
        &format!("previous_publication: {previous}"),
    );
    assert_ne!(raw, next, "generation envelope must change");
    fs::write(&envelope, next).expect("advance envelope");
    git(&checkout, &["add", "orbit-task-publication.yaml"]);
    git(&checkout, &["commit", "-m", "competing generation"]);
    git(&checkout, &["push", "origin", "HEAD:refs/heads/main"]);
    git_output(&checkout, &["rev-parse", "HEAD"])
}

fn corrupt_current_snapshot(root: &Path, bare: &Path, task_id: &str) {
    let checkout = root.join("corrupt");
    git(
        root,
        &["clone", bare.to_str().unwrap(), checkout.to_str().unwrap()],
    );
    configure_git(&checkout);
    let events = checkout.join("tasks").join(task_id).join("events.jsonl");
    let mut raw = fs::read_to_string(&events).expect("events");
    raw.push('{');
    fs::write(&events, raw).expect("corrupt events");
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "--amend", "--no-edit"]);
    git(
        &checkout,
        &["push", "--force", "origin", "HEAD:refs/heads/main"],
    );
}

#[derive(Debug, PartialEq, Eq)]
struct RepositoryState {
    head: String,
    status: String,
    remotes: String,
}

fn repository_state(repo: &Path) -> RepositoryState {
    RepositoryState {
        head: git_output(repo, &["rev-parse", "HEAD"]),
        status: git_output(repo, &["status", "--porcelain"]),
        remotes: git_output(repo, &["remote", "-v"]),
    }
}

fn init_source_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("repo");
    git(repo, &["init", "-b", "main"]);
    configure_git(repo);
    fs::write(repo.join("README.md"), "source\n").expect("readme");
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-m", "source"]);
    git(repo, &["remote", "add", "origin", SOURCE_REMOTE]);
}

fn init_bare_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("bare repo");
    git(repo, &["init", "--bare", "-b", "main"]);
}

fn install_fake_ssh(root: &Path) {
    let path = root.join("publication-test-ssh");
    fs::write(
        &path,
        "#!/bin/sh\ncase \"$*\" in\n  *git-upload-pack*) exec git-upload-pack \"$PUBLICATION_TEST_REPO\" ;;\n  *git-receive-pack*) exec git-receive-pack \"$PUBLICATION_TEST_REPO\" ;;\nesac\nexit 2\n",
    )
    .expect("fake ssh");
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("fake ssh executable");
}

fn configure_git(repo: &Path) {
    git(repo, &["config", "user.name", "Orbit Publication Test"]);
    git(
        repo,
        &["config", "user.email", "publication-test@example.com"],
    );
    git(repo, &["config", "commit.gpgsign", "false"]);
}

fn branch_tip(bare: &Path) -> Option<String> {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(bare)
        .args(["rev-parse", "--verify", "refs/heads/main"])
        .output()
        .expect("branch tip");
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn orbit(
    cwd: &Path,
    home: &Path,
    publication_bare: &Path,
    args: &[&str],
) -> assert_cmd::assert::Assert {
    let mut command = orbit_command(cwd, home, publication_bare);
    command.args(args);
    command.assert()
}

fn orbit_owned(
    cwd: &Path,
    home: &Path,
    publication_bare: &Path,
    args: &[String],
) -> assert_cmd::assert::Assert {
    let mut command = orbit_command(cwd, home, publication_bare);
    command.args(args);
    command.assert()
}

fn orbit_command(cwd: &Path, home: &Path, publication_bare: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    let fake_ssh = publication_bare
        .parent()
        .expect("publication parent")
        .join("publication-test-ssh");
    // ORB-11300: clear inherited authority first; the deliberate
    // `ORBIT_OPERATOR` grant below is this fixture's own, not the host run's.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_OPERATOR", "1")
        .env("GIT_SSH", fake_ssh)
        .env("GIT_SSH_VARIANT", "ssh")
        .env("PUBLICATION_TEST_REPO", publication_bare);
    command
}

fn orbit_json(cwd: &Path, home: &Path, bare: &Path, args: &[&str]) -> Value {
    let assert = orbit(cwd, home, bare, args).success();
    serde_json::from_slice(&assert.get_output().stdout).expect("JSON output")
}

fn orbit_json_owned(cwd: &Path, home: &Path, bare: &Path, args: &[String]) -> Value {
    let assert = orbit_owned(cwd, home, bare, args).success();
    serde_json::from_slice(&assert.get_output().stdout).expect("JSON output")
}

fn assert_output_contains(assert: &assert_cmd::assert::Assert, expected: &str) {
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stdout.contains(expected) || stderr.contains(expected),
        "expected '{expected}' in output\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

fn assert_output_excludes(assert: &assert_cmd::assert::Assert, forbidden: &str) {
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        !stdout.contains(forbidden) && !stderr.contains(forbidden),
        "forbidden value '{forbidden}' leaked\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

fn git(cwd: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git -C {} {} failed\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("git output");
    assert!(
        output.status.success(),
        "git -C {} {} failed: {}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
