#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::Command as AssertCommand;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

/// One private home and checkout per scenario; no registry or state is shared between rows.
struct Fixture {
    temp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new(repo_name: &str) -> Self {
        let temp = tempdir().expect("fixture");
        let base = fs::canonicalize(temp.path()).expect("canonical fixture");
        let home = base.join("home");
        let repo = base.join(repo_name);
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&repo).expect("repo");
        Self { temp, home, repo }
    }

    fn seeded(repo_name: &str) -> Self {
        let fixture = Self::new(repo_name);
        Self::seed_repo(&fixture.repo);
        fixture
    }

    fn seed_repo(repo: &Path) {
        crate::git_repo::SeededRepo::new(repo)
            .host_default_branch()
            .readme("# orbit\n")
            .build();
    }

    fn linked_worktree(&self, linked: &Path, branch: Option<&str>) {
        let mut args = vec!["worktree", "add"];
        if let Some(branch) = branch {
            args.extend(["-b", branch]);
        }
        args.push(linked.to_str().expect("worktree path"));
        crate::git_repo::run(&self.repo, &args);
    }

    fn init_worktree(&self, name: &str, branch: Option<&str>) -> PathBuf {
        let linked = self.temp.path().join(name);
        self.linked_worktree(&linked, branch);
        self.init(&self.repo);
        fs::canonicalize(linked).expect("linked worktree")
    }

    fn init(&self, repo: &Path) {
        self.success(repo, &["workspace", "init"]);
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> AssertCommand {
        orbit_command(cwd, &self.home, args, None)
    }

    fn success(&self, cwd: &Path, args: &[&str]) {
        self.command(cwd, args).assert().success();
    }

    fn json(&self, cwd: &Path, args: &[&str]) -> Value {
        self.json_with_root(cwd, args, None)
    }

    fn json_with_root(&self, cwd: &Path, args: &[&str], root: Option<&Path>) -> Value {
        let assert = orbit_command(cwd, &self.home, args, root)
            .assert()
            .success();
        serde_json::from_slice(&assert.get_output().stdout).expect("orbit json output")
    }

    fn output(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        self.output_with_root(cwd, args, None)
    }

    fn output_with_root(
        &self,
        cwd: &Path,
        args: &[&str],
        root: Option<&Path>,
    ) -> std::process::Output {
        orbit_command(cwd, &self.home, args, root)
            .output()
            .expect("run orbit")
    }

    fn assert_workspace_commands_use_named_id(&self, repo: &Path, data_dir: &Path, id: &str) {
        let data_dir_arg = data_dir.to_string_lossy().into_owned();
        let listed = self.json(
            repo,
            &[
                "--root",
                &data_dir_arg,
                "workspace",
                "list",
                "--format",
                "json",
            ],
        );
        let workspaces = listed
            .as_array()
            .unwrap_or_else(|| panic!("workspace list array: {listed}"));
        assert!(
            workspaces
                .iter()
                .any(|workspace| workspace["id"].as_str() == Some(id)),
            "workspace list missing {id}: {listed}"
        );
        self.json(
            repo,
            &[
                "--root",
                &data_dir_arg,
                "auto-task",
                "list",
                "--format",
                "json",
            ],
        );
    }

    fn auto_task(&self, cwd: &Path, name: &str) -> Value {
        self.json(cwd, &["auto-task", "show", name, "--json"])
    }

    fn selected_auto_task(&self, cwd: &Path, selector: &str) -> Value {
        self.json(
            cwd,
            &[
                "--workspace",
                selector,
                "auto-task",
                "show",
                "candidate-probe",
                "--json",
            ],
        )
    }

    fn auto_tasks(&self, cwd: &Path, all: bool) -> Value {
        let mut args = vec!["auto-task", "list", "--format", "json"];
        if all {
            args.push("--all");
        }
        self.json(cwd, &args)
    }
}

fn tool_run_explicit_workspace_resolves_from_home_and_temp_directory() {
    let fixture = Fixture::seeded("repo");
    let home = fixture.home.as_path();
    let repo = fixture.repo.as_path();
    fs::create_dir_all(home.join(".orbit")).expect("global root");

    fs::write(
        home.join(".orbit/config.toml"),
        "[machine]\nid = \"hm_cli_tool\"\nname = \"cli-tool\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("machine identity");
    fixture.init(repo);

    let workspaces = fixture.json(repo, &["workspace", "list", "--format", "json"]);
    let workspace_id = workspaces
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["id"].as_str())
        .expect("registered workspace ID");

    let temp_dir = std::env::temp_dir();
    for cwd in [home, temp_dir.as_path()] {
        let by_id = serde_json::json!({"workspace": workspace_id, "limit": 1, "model": "codex"});
        let listed = fixture.json(
            cwd,
            &[
                "tool",
                "run",
                "orbit.task.list",
                "--input",
                &by_id.to_string(),
            ],
        );
        assert!(
            listed["tasks"].is_array(),
            "list from {}: {listed}",
            cwd.display()
        );

        let qualified = serde_json::json!({
            "workspace": format!("hm_cli_tool/{workspace_id}"),
            "limit": 1,
            "model": "codex"
        });
        let listed = fixture.json(
            cwd,
            &[
                "tool",
                "run",
                "orbit.task.list",
                "--input",
                &qualified.to_string(),
            ],
        );
        assert!(
            listed["tasks"].is_array(),
            "qualified list from {}: {listed}",
            cwd.display()
        );

        let listed = fixture.json(
            cwd,
            &[
                "--workspace",
                workspace_id,
                "tool",
                "run",
                "orbit.task.list",
                "--input",
                r#"{"limit":1,"model":"codex"}"#,
            ],
        );
        assert!(
            listed["tasks"].is_array(),
            "flagged list from {}: {listed}",
            cwd.display()
        );
    }

    let foreign = serde_json::json!({
        "workspace": format!("hm_other/{workspace_id}"),
        "limit": 1,
        "model": "codex"
    });
    let mut command = cargo_bin_cmd!("orbit");
    command
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args([
            "tool",
            "run",
            "orbit.task.list",
            "--input",
            &foreign.to_string(),
        ]);
    clear_inherited_authority_env(&mut command);
    set_orbit_root_env(&mut command, None);
    let failure = command.assert().failure();
    let stderr = String::from_utf8_lossy(&failure.get_output().stderr);
    assert!(
        stderr.contains("hm_other"),
        "foreign host must be named: {stderr}"
    );
}

fn config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides() {
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();
    let linked_worktree = fixture.init_worktree("repo-worktree", Some("orbit-worktree-resolution"));
    let main_orbit = main_repo.join(".orbit");
    let linked_orbit = linked_worktree.join(".orbit");

    let from_main = fixture.json(main_repo, &["config", "show", "--json"]);
    assert_root_fields(&from_main, &main_orbit, &main_orbit);

    let from_worktree = fixture.json(&linked_worktree, &["config", "show", "--json"]);
    assert_root_fields(&from_worktree, &main_orbit, &linked_orbit);
    assert!(
        !linked_orbit.exists(),
        "linked worktree local root should be resolved but not created"
    );

    let explicit_root = main_orbit.to_string_lossy().to_string();
    let from_root_override = fixture.json(
        &linked_worktree,
        &["--root", &explicit_root, "config", "show", "--json"],
    );
    assert_root_fields(&from_root_override, &main_orbit, &main_orbit);

    let from_env = fixture.json_with_root(
        &linked_worktree,
        &["config", "show", "--json"],
        Some(&main_orbit),
    );
    assert_root_fields(&from_env, &main_orbit, &main_orbit);
    assert!(
        !linked_orbit.exists(),
        "resolution should not materialize a linked-worktree .orbit directory"
    );
}

fn submodule_bootstrap_and_runtime_keep_the_submodule_workspace() {
    check_submodule_roots(false);
}

#[cfg(unix)]
fn submodule_roots_remain_independent_when_git_is_unavailable() {
    check_submodule_roots(true);
}

fn check_submodule_roots(without_git: bool) {
    // Cover both legacy discovery before any workspace exists and bootstrap
    // with an initialized, registered superproject above the submodule.
    for initialize_superproject in [false, true] {
        let fixture = Fixture::new("superproject");
        let home = fixture.home.as_path();
        let superproject = fixture.repo.as_path();

        let submodule = init_submodule_fixture(superproject);
        let nested = submodule.join("nested");
        fs::create_dir_all(&nested).expect("nested working directory");

        if initialize_superproject {
            fixture.init(superproject);
        }
        let mut bootstrap = fixture.command(&nested, &["workspace", "init"]);
        if without_git {
            bootstrap.env("PATH", home.join("stub-bin"));
        }
        bootstrap.assert().success();
        let submodule_orbit = submodule.join(".orbit");
        assert!(
            submodule_orbit.join("config.yaml").is_file(),
            "bootstrap must create the submodule workspace at its repository boundary"
        );
        assert_eq!(
            superproject.join(".orbit").exists(),
            initialize_superproject,
            "submodule bootstrap must not create superproject state"
        );
        let mut command = fixture.command(&nested, &["config", "show", "--json"]);
        if without_git {
            command.env("PATH", home.join("stub-bin"));
        }
        let output = command.assert().success();
        let config: Value = serde_json::from_slice(&output.get_output().stdout).expect("config");
        assert_root_fields(&config, &submodule_orbit, &submodule_orbit);
    }
}

fn submodule_runtime_respects_its_registered_data_directory() {
    let fixture = Fixture::new("superproject");
    let home = fixture.home.as_path();
    let superproject = fixture.repo.as_path();

    let submodule = init_submodule_fixture(superproject);
    fixture.init(superproject);
    let data_dir = canonicalize_or_original(fixture.temp.path()).join("submodule-data");
    fixture.init(&submodule);
    fs::rename(submodule.join(".orbit"), &data_dir).expect("relocate submodule state");
    let registry_path = home.join(".orbit/workspaces.json");
    let mut registry: Value = serde_json::from_slice(&fs::read(&registry_path).expect("registry"))
        .expect("registry JSON");
    let checkout = registry["checkouts"]
        .as_array_mut()
        .expect("checkouts")
        .iter_mut()
        .find(|checkout| checkout["repo_root"].as_str() == submodule.to_str())
        .expect("submodule checkout");
    checkout["orbit_dir"] = serde_json::json!(data_dir);
    fs::write(
        &registry_path,
        serde_json::to_vec(&registry).expect("registry JSON"),
    )
    .expect("registered relocated data directory");
    fixture.success(&submodule, &["workspace", "init", "--force"]);
    let config = fixture.json(&submodule, &["config", "show", "--json"]);
    assert_root_fields(&config, &data_dir, &data_dir);
    assert!(
        !submodule.join(".orbit").exists(),
        "the registered data directory must remain authoritative"
    );
}

#[cfg(unix)]
fn linked_worktree_gitfile_fallback_checks_the_common_directory() {
    let fixture = Fixture::seeded("repo");
    let home = fixture.home.as_path();
    let main_repo = fixture.repo.as_path();

    let linked = fixture.init_worktree("linked", None);
    let main_orbit = main_repo.join(".orbit");
    let local_orbit = linked.join(".orbit");
    let nested = linked.join("nested");
    fs::create_dir_all(&nested).expect("nested directory");

    // Git emits an absolute gitfile. Exercise its relative form as well.
    let absolute_gitfile = fs::read_to_string(linked.join(".git")).expect("gitfile");
    for gitfile in [
        absolute_gitfile.as_str(),
        "gitdir: ../repo/.git/worktrees/linked\n",
    ] {
        fs::write(linked.join(".git"), gitfile).expect("gitfile");
        let mut command = fixture.command(&nested, &["config", "show", "--json"]);
        command.env("PATH", home.join("stub-bin"));
        let output = command.assert().success();
        let config: Value = serde_json::from_slice(&output.get_output().stdout).expect("config");
        assert_root_fields(&config, &main_orbit, &local_orbit);
        assert!(
            !local_orbit.exists(),
            "fallback must not create local state"
        );
    }

    // A plausible worktrees/ path alone is insufficient: a mismatched or
    // missing commondir must not redirect resolution to another repository.
    fs::create_dir_all(&local_orbit).expect("local lookup boundary");
    fs::write(local_orbit.join("config.toml"), "").expect("local config");
    let commondir = main_repo.join(".git/worktrees/linked/commondir");
    fs::create_dir_all(main_repo.join("other.git")).expect("unrelated common directory");
    for invalid in [None, Some(""), Some("../../../other.git")] {
        match invalid {
            None => fs::remove_file(&commondir).expect("missing common directory"),
            Some(value) => fs::write(&commondir, value).expect("invalid common directory"),
        }
        let mut command = fixture.command(&nested, &["config", "show", "--json"]);
        command.env("PATH", home.join("stub-bin"));
        let output = command.assert().success();
        let config: Value = serde_json::from_slice(&output.get_output().stdout).expect("config");
        assert_root_fields(&config, &local_orbit, &local_orbit);
    }
}

fn init_submodule_fixture(superproject: &Path) -> PathBuf {
    let source = superproject.parent().expect("fixture root").join("source");
    fs::create_dir_all(&source).expect("submodule source");
    fs::create_dir_all(superproject).expect("superproject");
    Fixture::seed_repo(&source);
    Fixture::seed_repo(superproject);
    crate::git_repo::run(
        superproject,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            source.to_str().expect("source path"),
            "sub",
        ],
    );
    fs::canonicalize(superproject.join("sub")).expect("submodule checkout")
}

/// Restores a path's permissions when dropped, so an assertion that panics
/// while the path is read-only cannot leave a directory the tempdir cannot
/// delete (the leftover would sit inside an agent worktree's `.orbit/tmp`).
#[cfg(unix)]
struct RestorePermissions {
    path: PathBuf,
    original: fs::Permissions,
}

#[cfg(unix)]
impl RestorePermissions {
    fn set_mode(path: &Path, mode: u32) -> Self {
        let original = fs::metadata(path).expect("path to restrict").permissions();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("restrict path");
        Self {
            path: path.to_path_buf(),
            original,
        }
    }
}

#[cfg(unix)]
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, self.original.clone());
    }
}

#[cfg(unix)]
#[test]
fn restore_permissions_guard_restores_the_mode_when_the_test_panics() {
    let temp = tempdir().expect("fixture");
    let dir = temp.path().join("locked");
    fs::create_dir_all(&dir).expect("dir");
    fs::write(dir.join("file"), "x").expect("file");
    let before = fs::metadata(&dir).expect("metadata").permissions().mode();

    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _restore = RestorePermissions::set_mode(&dir, 0o555);
        assert_eq!(
            fs::metadata(&dir).expect("metadata").permissions().mode() & 0o777,
            0o555
        );
        panic!("assertion failed between chmod and restore");
    }));

    assert!(unwound.is_err(), "the fixture must have panicked");
    assert_eq!(
        fs::metadata(&dir).expect("metadata").permissions().mode(),
        before,
        "the guard must restore the directory permissions on unwind"
    );
}

#[cfg(unix)]
fn workspace_list_skips_unchanged_registry_lock_and_refuses_unpersisted_validation() {
    let fixture = Fixture::seeded("repo");
    let home = fixture.home.as_path();
    let repo = fixture.repo.as_path();

    fixture.init(repo);
    let global = home.join(".orbit");
    let lock = global.join(".workspaces.json.lock");
    if lock.exists() {
        fs::remove_file(&lock).expect("remove fixture lock before read-only check");
    }
    let listed = fixture.json(repo, &["workspace", "list", "--format", "json"]);
    assert!(listed.as_array().is_some_and(|rows| !rows.is_empty()));
    assert!(
        !lock.exists(),
        "unchanged validation must not create a lock"
    );

    // The first invocation prepared Orbit's enumerated global SQLite state.
    // A second list must work with the surrounding global directory read-only.
    let _restore_global = RestorePermissions::set_mode(&global, 0o555);
    let listed_read_only = fixture.json(repo, &["workspace", "list", "--format", "json"]);
    assert_eq!(listed_read_only, listed);
    assert!(!lock.exists(), "read-only list must not create a lock");

    let moved_repo = fixture.temp.path().join("moved-repo");
    fs::rename(repo, &moved_repo).expect("make registered checkout missing");
    let unrelated = fixture.temp.path().join("unrelated");
    // Not the registered checkout. Its own lookup boundary keeps a TMPDIR
    // nested in a checkout from loading that checkout's config before the
    // registry validation this test expects to fail closed.
    crate::git_repo::seal_lookup_boundary(&unrelated);
    let before = fs::read(global.join("workspaces.json")).expect("registry before failure");
    let mut command = cargo_bin_cmd!("orbit");
    command
        .current_dir(&unrelated)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["workspace", "list", "--format", "json"]);
    clear_inherited_authority_env(&mut command);
    set_orbit_root_env(&mut command, None);
    let failed = command.assert().failure();
    let stderr = String::from_utf8_lossy(&failed.get_output().stderr);
    assert!(
        stderr.contains("Read-only file system") || stderr.contains("Permission denied"),
        "validation write must fail closed: {stderr}"
    );
    assert_eq!(
        fs::read(global.join("workspaces.json")).expect("registry after failure"),
        before,
        "failed validation must not silently drop or partially persist the edit"
    );
    assert!(
        !lock.exists(),
        "failed validation must not leave a lock file"
    );

    // Selector resolution has a different contract: a failed status save is
    // best-effort, so it still reports the filesystem-validated inactive row.
    // Let lock acquisition succeed and make only the atomic save fail.
    drop(_restore_global);
    fs::write(&lock, b"").expect("prepare existing registry lock");
    let _restore_global = RestorePermissions::set_mode(&global, 0o555);
    let selector = listed[0]["id"].as_str().expect("registered workspace id");
    let failed = fixture
        .command(&unrelated, &[])
        .args(["--workspace", selector, "workspace", "show", "--json"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&failed.get_output().stderr);
    assert!(
        stderr.contains("is invalid on this machine"),
        "selector must use validated status despite the failed save: {stderr}"
    );
    assert_eq!(
        fs::read(global.join("workspaces.json")).expect("registry after best-effort save"),
        before,
        "best-effort selector validation must leave failed status writes unchanged"
    );
}

fn auto_task_show_keeps_alternate_extension_candidates_local() {
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();

    let linked = fixture.init_worktree("candidate", Some("candidate"));
    let shared = fs::canonicalize(main_repo.join(".orbit")).unwrap();
    let local = fs::canonicalize(&linked).unwrap().join(".orbit");
    write_auto_task_fixture(&shared, "Shared enabled body");
    let shared_path = shared.join("auto_tasks/candidate-probe.yaml");
    let shared_bytes = fs::read_to_string(&shared_path)
        .unwrap()
        .replace("enabled: false", "enabled: true");
    fs::write(&shared_path, &shared_bytes).unwrap();
    for extension in ["yml", "YML", "Yml"] {
        write_auto_task_fixture(&local, "Local disabled body");
        let canonical = local.join("auto_tasks/candidate-probe.yaml");
        let alternate = canonical.with_extension(extension);
        fs::rename(&canonical, &alternate).unwrap();
        fs::write(
            local.join("auto_tasks/.orbit-auto-task-settings.json"),
            r#"{"schemaVersion":1,"definitions":{"candidate-probe":{"priority":"high"}}}"#,
        )
        .unwrap();
        let shown = fixture.auto_task(&linked, "candidate-probe");
        assert_eq!(
            shown["description"], "Local disabled body",
            "extension {extension}"
        );
        assert_eq!(shown["enabled"], false);
        assert_definition_source(&shown, &local, &alternate);
        assert_eq!(shown["layering"]["settings"]["priority"], "high");
        let listed = fixture.auto_tasks(&linked, false);
        let rows: Vec<_> = json_rows(&listed)
            .iter()
            .filter(|row| row["name"] == "candidate-probe")
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["description"], shown["description"]);
        assert_eq!(rows[0]["enabled"], false);
        for args in local_write_commands("forbidden", "probe") {
            let mut command = fixture.command(&linked, &args);
            if args[1] == "reset" {
                command.env("ORBIT_OPERATOR", "1");
            }
            let refused = command.output().unwrap();
            assert!(
                !refused.status.success(),
                "alternate read must not grant write authority: {args:?}"
            );
            let message = String::from_utf8_lossy(&refused.stderr);
            assert!(
                message.contains("no such auto-task")
                    || message.contains("must be stored at its canonical .yaml path"),
                "local canonical lookup must refuse {args:?}: {refused:?}"
            );
        }
        let candidate_body = fs::read_to_string(&alternate).unwrap();
        fs::write(
            &alternate,
            format!("# provenance: plugin:absent-probe@1.0.0\n{candidate_body}"),
        )
        .unwrap();
        assert_inactive_plugin(&fixture, &linked, "candidate-probe", &alternate);
        // A second file with the same stem must not choose an arbitrary body.
        write_auto_task_fixture(&local, "Ambiguous canonical body");
        let duplicate =
            fixture.output(&linked, &["auto-task", "show", "candidate-probe", "--json"]);
        assert!(!duplicate.status.success());
        let refusal: Value = serde_json::from_slice(&duplicate.stderr).unwrap();
        assert_eq!(refusal["code"], "invalid_input");
        fs::remove_file(&canonical).unwrap();
        fs::write(&alternate, "name: [malformed").unwrap();
        let malformed =
            fixture.output(&linked, &["auto-task", "show", "candidate-probe", "--json"]);
        assert!(
            !malformed.status.success(),
            "malformed {extension} must shadow shared body"
        );
        let refusal: Value = serde_json::from_slice(&malformed.stderr).unwrap();
        assert_eq!(refusal["code"], "invalid_input");
        let malformed_list = fixture.auto_tasks(&linked, false);
        assert_auto_task_hidden(
            &malformed_list,
            "candidate-probe",
            "list must not reveal the shared body either",
        );
        assert_eq!(fs::read_to_string(&shared_path).unwrap(), shared_bytes);
        assert!(
            !canonical.exists(),
            "refused writes must not create canonical definition"
        );
        fs::remove_file(&alternate).unwrap();
    }
}

fn auto_task_show_reports_and_honors_the_selected_definition_source() {
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();

    let linked_worktree =
        fixture.init_worktree("repo-candidate", Some("orbit-auto-task-candidate"));

    let main_orbit = main_repo.join(".orbit");
    let linked_orbit = linked_worktree.join(".orbit");
    write_auto_task_fixture(&main_orbit, "Primary checkout definition");

    // An empty run-worktree catalog must not hide workspace definitions.
    fs::create_dir_all(linked_orbit.join("auto_tasks")).expect("empty candidate catalog");
    let primary_bytes = fs::read(main_orbit.join("auto_tasks/candidate-probe.yaml"))
        .expect("primary definition before inspection");
    fs::write(
        main_orbit.join("auto_tasks/.orbit-auto-task-settings.json"),
        r#"{"schemaVersion":1,"definitions":{"candidate-probe":{"priority":"high"}}}"#,
    )
    .expect("workspace settings");
    {
        let args = ["auto-task", "show", "candidate-probe", "--json"];
        let shown = fixture.json(&linked_worktree, &args);
        assert_eq!(shown["description"], "Primary checkout definition");
        assert_eq!(shown["template"]["priority"], "high");
        assert_eq!(shown["layering"]["settings"]["priority"], "high");
        assert_eq!(
            shown["enabled"], false,
            "disabled definitions remain inspectable"
        );
        assert_definition_source(
            &shown,
            &main_orbit,
            &main_orbit.join("auto_tasks/candidate-probe.yaml"),
        );
    }
    for args in [
        vec!["auto-task", "list", "--format", "json"],
        vec!["tool", "run", "orbit.auto_task.list", "--input", "{}"],
    ] {
        let listed = fixture.json(&linked_worktree, &args);
        assert!(
            json_rows(&listed).iter().any(|row| {
                row["name"] == "candidate-probe"
                    && row["description"] == "Primary checkout definition"
            }),
            "workspace definition missing from {listed}"
        );
    }
    let missing = fixture.output(
        &linked_worktree,
        &["auto-task", "show", "absent-probe", "--json"],
    );
    assert!(!missing.status.success());
    let refusal: Value = serde_json::from_slice(&missing.stderr).expect("typed JSON refusal");
    assert_eq!(refusal["code"], "invalid_input");
    let error = refusal["error"].as_str().expect("refusal message");
    assert!(
        error.contains(main_orbit.to_str().expect("root")),
        "searched workspace root: {error}"
    );
    assert!(
        error.contains(linked_orbit.to_str().expect("root")),
        "searched candidate root: {error}"
    );
    assert!(
        error.contains("orbit auto-task list"),
        "scope discovery command: {error}"
    );
    for args in local_write_commands("must stay local", "lookup probe") {
        let mut command = fixture.command(&linked_worktree, &args);
        // Exercise the operator-only reset on the disposable fixture, as the
        // lifecycle CLI fixtures do; no ambient authority reaches this child.
        if args[1] == "reset" {
            command.env("ORBIT_OPERATOR", "1");
        }
        let refused = command.output().expect("local write lookup");
        assert!(
            !refused.status.success(),
            "shared inspection must not widen write scope: {args:?}"
        );
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("no such auto-task"),
            "local-only definition lookup must refuse before mutation: {refused:?}"
        );
    }
    assert_eq!(
        fs::read(main_orbit.join("auto_tasks/candidate-probe.yaml")).expect("primary after reads"),
        primary_bytes
    );
    assert!(
        fs::read_dir(linked_orbit.join("auto_tasks"))
            .expect("candidate catalog")
            .next()
            .is_none(),
        "reads and a refused mint must not create local definitions"
    );

    write_auto_task_fixture(&linked_orbit, "Linked candidate definition");
    fs::write(
        linked_orbit.join("auto_tasks/.orbit-auto-task-settings.json"),
        r#"{"schemaVersion":1,"definitions":{"candidate-probe":{"priority":"low"}}}"#,
    )
    .expect("candidate settings");
    fs::write(
        main_orbit.join("auto_tasks/workspace-only.yaml"),
        String::from_utf8(primary_bytes.clone())
            .expect("definition text")
            .replace("candidate-probe", "workspace-only"),
    )
    .expect("shared-only definition for union listing");
    let union = fixture.auto_tasks(&linked_worktree, false);
    let rows = json_rows(&union);
    let candidates: Vec<_> = rows
        .iter()
        .filter(|row| row["name"] == "candidate-probe")
        .collect();
    assert_eq!(candidates.len(), 1, "candidate overrides shared body once");
    assert_eq!(candidates[0]["description"], "Linked candidate definition");
    assert_eq!(candidates[0]["template"]["priority"], "low");
    assert!(
        rows.iter().any(|row| row["name"] == "workspace-only"),
        "a local candidate must not hide other shared names"
    );

    let workspaces = fixture.json(main_repo, &["workspace", "list", "--format", "json"]);
    let workspace_id = workspaces
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["id"].as_str())
        .unwrap_or_else(|| panic!("expected registered workspace in {workspaces}"));

    let linked_selector = linked_worktree.to_string_lossy();
    for (name, cwd, selector, description, root) in [
        (
            "logical selector in worktree",
            linked_worktree.as_path(),
            workspace_id,
            "Linked candidate definition",
            &linked_orbit,
        ),
        (
            "logical selector in primary",
            main_repo,
            workspace_id,
            "Primary checkout definition",
            &main_orbit,
        ),
        (
            "explicit linked selector",
            main_repo,
            linked_selector.as_ref(),
            "Linked candidate definition",
            &linked_orbit,
        ),
    ] {
        let shown = fixture.selected_auto_task(cwd, selector);
        assert_eq!(shown["description"], description, "{name}");
        if name == "logical selector in worktree" {
            assert_eq!(shown["layering"]["settings"]["priority"], "low", "{name}");
        }
        assert_definition_source(&shown, root, &root.join("auto_tasks/candidate-probe.yaml"));
    }
    assert!(
        !linked_orbit.join("tasks").exists(),
        "candidate inspection must not create a worktree-local shadow store"
    );

    fixture.success(
        main_repo,
        &[
            "--workspace",
            &linked_selector,
            "auto-task",
            "update",
            "candidate-probe",
            "--description",
            "Primary definition updated through authoritative routing",
        ],
    );
    let primary_after_update = fixture.selected_auto_task(main_repo, workspace_id);
    assert_eq!(
        primary_after_update["description"],
        "Primary definition updated through authoritative routing"
    );
    let worktree_after_primary_update = fixture.selected_auto_task(&linked_worktree, workspace_id);
    assert_eq!(
        worktree_after_primary_update["description"], "Linked candidate definition",
        "a logical selector from the linked worktree must keep reading worktree YAML"
    );
    let candidate_after_update = fixture.selected_auto_task(main_repo, &linked_selector);
    assert_eq!(
        candidate_after_update["description"], "Linked candidate definition",
        "writable explicit-linked-selector commands must not retarget definition mutations"
    );
    assert!(
        !linked_orbit.join("tasks").exists(),
        "writable routing must not create a worktree-local shadow store"
    );

    fs::write(
        linked_orbit.join("auto_tasks/candidate-probe.yaml"),
        "invalid: [",
    )
    .expect("broken candidate");
    let broken = fixture.output(
        &linked_worktree,
        &["auto-task", "show", "candidate-probe", "--json"],
    );
    assert!(
        !broken.status.success(),
        "an invalid candidate must not fall back to the shared body"
    );
    let listed = fixture.auto_tasks(&linked_worktree, false);
    assert_auto_task_hidden(
        &listed,
        "candidate-probe",
        "a broken local entry must shadow the shared definition",
    );
    // A shared plugin definition must carry its original inactive provenance.
    let plugin_body = String::from_utf8(primary_bytes)
        .expect("definition text")
        .replace("candidate-probe", "plugin-probe");
    fs::write(
        main_orbit.join("auto_tasks/plugin-probe.yaml"),
        format!("# provenance: plugin:absent-probe@1.0.0\n{plugin_body}"),
    )
    .expect("shared plugin definition");
    assert_inactive_plugin(&fixture, &linked_worktree, "plugin-probe", &main_orbit);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            main_orbit.join("auto_tasks/workspace-only.yaml"),
            linked_orbit.join("auto_tasks/workspace-only.yaml"),
        )
        .expect("symlink candidate");
        let confined = fixture.output(
            &linked_worktree,
            &["auto-task", "show", "workspace-only", "--json"],
        );
        assert!(
            !confined.status.success(),
            "a symlinked candidate cannot redirect or fall back"
        );
        let listed = fixture.auto_tasks(&linked_worktree, false);
        assert_auto_task_hidden(
            &listed,
            "workspace-only",
            "a symlinked local entry must not expose the shared definition",
        );
    }

    fs::write(
        main_orbit.join("auto_tasks/shared-fault.yaml"),
        "invalid: [",
    )
    .expect("shared catalog fault");
    let doctor = fixture.output(&linked_worktree, &["doctor", "--json"]);
    let checks: Vec<Value> = serde_json::from_slice(&doctor.stdout).expect("doctor rows");
    let auto_tasks = checks
        .iter()
        .find(|row| row["check"] == "artifacts-auto-tasks")
        .expect("auto-task health");
    assert!(
        auto_tasks["remediation"]
            .as_str()
            .expect("health remediation")
            .contains(
                main_orbit
                    .join("auto_tasks/shared-fault.yaml")
                    .to_str()
                    .expect("fault path")
            ),
        "worktree diagnostics must inspect the shared catalog: {auto_tasks}"
    );

    let outside_checkout = fixture.temp.path().to_string_lossy().into_owned();
    for rejected in ["no-such-workspace", outside_checkout.as_str()] {
        let mut command = fixture.command(
            main_repo,
            &[
                "--workspace",
                rejected,
                "auto-task",
                "show",
                "candidate-probe",
                "--json",
            ],
        );
        let assert = command.assert().failure();
        let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
        assert!(
            stderr.contains("unknown workspace selector"),
            "rejected selector {rejected} reported: {stderr}"
        );
    }
}

fn auto_task_crud_from_linked_worktree_writes_definition_yaml_to_local_root() {
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();
    let elsewhere = fixture.temp.path().join("elsewhere");

    fs::create_dir_all(&elsewhere).expect("create unrelated cwd");
    let linked_worktree =
        fixture.init_worktree("repo-jrun", Some("orbit-auto-task-worktree-write"));

    let main_orbit = main_repo.join(".orbit");
    let linked_orbit = linked_worktree.join(".orbit");
    let workspaces = fixture.json(main_repo, &["workspace", "list", "--format", "json"]);
    let workspace_id = workspaces
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["id"].as_str())
        .unwrap_or_else(|| panic!("expected registered workspace in {workspaces}"))
        .to_string();
    let workspace_name = workspaces
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["name"].as_str())
        .unwrap_or_else(|| panic!("expected registered workspace name in {workspaces}"))
        .to_string();

    let added = fixture.json(
        &linked_worktree,
        &[
            "--workspace",
            &workspace_id,
            "auto-task",
            "add",
            "--name",
            "worktree-crud",
            "--every-minutes",
            "60",
            "--title",
            "Worktree CRUD",
            "--description",
            "Written from the linked worktree",
            "--json",
        ],
    );
    assert_eq!(added["name"], "worktree-crud");
    let worktree_yaml = linked_orbit.join("auto_tasks/worktree-crud.yaml");
    let primary_yaml = main_orbit.join("auto_tasks/worktree-crud.yaml");
    assert!(
        worktree_yaml.is_file(),
        "add from a linked worktree must write {}",
        worktree_yaml.display()
    );
    assert!(
        !primary_yaml.exists(),
        "add must not write the registered primary checkout"
    );
    assert!(
        !linked_orbit.join("tasks").exists(),
        "must not invent a worktree-local task store"
    );

    fixture.success(
        &linked_worktree,
        &[
            "--workspace",
            &workspace_name,
            "auto-task",
            "update",
            "worktree-crud",
            "--description",
            "Updated in the worktree",
        ],
    );
    fixture.success(
        &linked_worktree,
        &[
            "--workspace",
            &workspace_id,
            "auto-task",
            "toggle",
            "worktree-crud",
            "off",
        ],
    );
    let yaml = fs::read_to_string(&worktree_yaml).expect("read worktree yaml");
    assert!(
        yaml.contains("Updated in the worktree"),
        "update must rewrite worktree YAML: {yaml}"
    );
    assert!(
        yaml.contains("enabled: false"),
        "toggle must rewrite worktree YAML: {yaml}"
    );
    assert!(
        !primary_yaml.exists(),
        "update/toggle must not create primary YAML"
    );

    let from_elsewhere = fixture.json(
        &elsewhere,
        &[
            "--workspace",
            &workspace_id,
            "auto-task",
            "add",
            "--name",
            "primary-from-elsewhere",
            "--every-minutes",
            "60",
            "--title",
            "Primary from elsewhere",
            "--json",
        ],
    );
    assert_eq!(from_elsewhere["name"], "primary-from-elsewhere");
    assert!(
        main_orbit
            .join("auto_tasks/primary-from-elsewhere.yaml")
            .is_file(),
        "a logical selector from a cwd that is not a linked worktree writes the primary"
    );
    assert!(
        !linked_orbit
            .join("auto_tasks/primary-from-elsewhere.yaml")
            .exists()
    );

    let worktree_root = linked_orbit.to_string_lossy().into_owned();
    let mut shadowed = fixture.command(
        &linked_worktree,
        &[
            "--root",
            &worktree_root,
            "auto-task",
            "add",
            "--name",
            "shadow-store",
            "--every-minutes",
            "60",
            "--title",
            "Shadow store",
        ],
    );
    let assert = shadowed.assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("not an Orbit workspace")
            || stderr.contains("unknown workspace selector")
            || stderr.contains("workspaces.json"),
        "--root at the worktree .orbit must be refused as a store shadow: {stderr}"
    );

    let mut managed = fixture.command(
        &linked_worktree,
        &[
            "auto-task",
            "add",
            "--name",
            "managed-envelope",
            "--every-minutes",
            "60",
            "--title",
            "Managed envelope",
            "--json",
        ],
    );
    managed.env("ORBIT_WORKSPACE", &workspace_id);
    managed.env("ORBIT_MANAGED_RUN_CONTEXT", "1");
    managed.env("ORBIT_RUN_ID", "jrun-auto-task-worktree");
    let managed_assert = managed.assert().success();
    serde_json::from_slice::<Value>(&managed_assert.get_output().stdout)
        .expect("managed envelope auto-task add json");
    assert!(
        linked_orbit
            .join("auto_tasks/managed-envelope.yaml")
            .is_file(),
        "ORBIT_WORKSPACE from a linked worktree must write worktree YAML"
    );
    assert!(
        !main_orbit.join("auto_tasks/managed-envelope.yaml").exists(),
        "managed envelope must not write the registered primary checkout"
    );
    assert!(
        !linked_orbit.join("tasks").exists(),
        "managed envelope must not invent a worktree-local task store"
    );
}

/// The real detached worker must bootstrap, claim, and finish its run while an
/// unrelated WAL writer holds the already-current task registry. The parent
/// keeps supervising the child; no fixture claims the run on its behalf.
fn detached_worker_bootstraps_and_claims_while_registry_writer_is_held() {
    let fixture = Fixture::seeded("repo");
    let repo = fixture.repo.as_path();
    let custom_root = fixture.temp.path().join("custom-orbit");

    fixture.success(
        repo,
        &[
            "--root",
            custom_root.to_str().expect("utf8 custom root"),
            "workspace",
            "init",
        ],
    );
    pin_default_crew_for_isolated_root(&custom_root);

    // Match the task store's task_registry_path and the checkout-binding fixtures.
    let registry_path = custom_root.join("tasks").join("index.sqlite");
    assert!(
        registry_path.is_file(),
        "workspace init must create the task registry before contention: {}",
        registry_path.display()
    );
    let registry_writer = rusqlite::Connection::open_with_flags(
        &registry_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .expect("open existing task registry writer without creating it");
    registry_writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold registry WAL writer");

    let job_path = repo.join("root-override-smoke.yaml");
    fs::write(
        &job_path,
        r#"schemaVersion: 2
kind: Job
metadata:
  name: root_override_smoke
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap
      default_input:
        seconds: 0
      spec:
        type: deterministic
        action: sleep
        config: {}
"#,
    )
    .expect("write smoke job");

    let custom_root_arg = custom_root.to_string_lossy().into_owned();
    let job_path_arg = job_path.to_string_lossy().into_owned();
    let submitted = fixture.json(
        repo,
        &[
            "--root",
            &custom_root_arg,
            "run",
            "job",
            &job_path_arg,
            "--json",
        ],
    );
    let run_id = submitted["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("expected run_id in {submitted}"))
        .to_string();
    assert_eq!(submitted["state"].as_str(), Some("submitted"));
    // Readiness, not total CLI startup time, proves independence from the
    // registry writer (ORB-14396). Keep the writer held through the real
    // worker's claim AND completion; a busy timeout must still fail the run.
    let shown = wait_for_run_terminal_state(&custom_root, &run_id);
    let state = shown["run"]["state"].as_str().unwrap_or("missing");
    assert_eq!(
        state,
        "success",
        "{}",
        detached_worker_diagnostic(&custom_root, &run_id, &shown)
    );
    assert!(
        shown["run"]["pid"].as_u64().is_some(),
        "the real worker must claim its persisted run: {shown}"
    );
    registry_writer
        .execute_batch("ROLLBACK")
        .expect("release registry writer");
}

fn doctor_graph_cleanup_uses_split_roots_and_keeps_json_stdout_clean() {
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();

    let linked_worktree = fixture.init_worktree("repo-doctor", Some("orbit-worktree-doctor"));

    let local_marker = linked_worktree.join(".orbit/graph/local.db");
    let shared_marker = main_repo.join(".orbit/knowledge/graph/shared.db");
    for marker in [&local_marker, &shared_marker] {
        fs::create_dir_all(marker.parent().expect("graph parent")).expect("create graph parent");
        fs::write(marker, b"retired").expect("write graph marker");
    }

    let ordinary = fixture.json(&linked_worktree, &["doctor", "--json"]);
    assert!(local_marker.exists());
    assert!(shared_marker.exists());
    assert!(
        json_rows(&ordinary)
            .iter()
            .all(|row| row["check"] != "graph-index")
    );

    let cleaned = fixture.json(&linked_worktree, &["doctor", "--remove-graph", "--json"]);
    assert!(!local_marker.exists());
    assert!(!shared_marker.exists());
    assert!(
        json_rows(&cleaned)
            .iter()
            .all(|row| row["check"] != "graph-index")
    );

    // Parsing succeeds again with no cleanup prose mixed into stdout, and
    // absence remains a successful no-op.
    fixture.json(&linked_worktree, &["doctor", "--remove-graph", "--json"]);
}

/// [ORB-12131, ORB-12143] `--fix-orphan-task-stores` deletes task data. It
/// refuses without `--confirm`. Once confirmed it removes only an empty
/// partition and a populated one whose checkout is confirmed absent. The
/// partitions of a live checkout, of a checkout that fails to stat for any
/// reason other than absence, and with no binding at all keep their bundles.
/// [ORB-14516] Empty and removed mount points are ambiguous absence and must
/// keep their partitions too; a dangling parent symlink forces read_dir to fail.
#[test]
fn doctor_orphan_task_store_repair_deletes_only_confirmed_absent_checkouts() {
    const TEST: &str = "worktree_resolution::doctor_orphan_task_store_repair_deletes_only_confirmed_absent_checkouts";
    const CHILD: &str = "ORBIT_TEST_ORPHAN_TASK_STORE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempdir().expect("isolated child home");
        let mut command = StdCommand::new(std::env::current_exe().expect("test binary"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path())
            .output()
            .expect("isolated task-store repair fixture");
        test_env::assert_child_test_passed(TEST, output.status, &output.stdout, &output.stderr);
        return;
    }

    let fixture = Fixture::new("live");
    let home = fixture.home.as_path();
    write_machine_identity(home);
    // The pre-written global config seeds no crews, so routing falls back to
    // the built-in claude-backed `opus` and `system` crews. Without their CLI
    // every `doctor` run fails its provider checks, and the refusal asserted
    // below would hold for the wrong reason.
    plant_agent_cli_stub(home, "claude");
    let partitions = home.join(".orbit/tasks/workspaces");
    let deleted_volume = fixture.temp.path().join("deleted-volume");
    let file_volume = fixture.temp.path().join("file-volume");
    let empty_mount = fixture.temp.path().join("empty-mount");
    let removed_mount = fixture.temp.path().join("removed-mount");
    #[cfg(unix)]
    let dangling_parent = fixture.temp.path().join("dangling-parent");

    let mut partition_of = std::collections::BTreeMap::new();
    let checkouts = vec![
        ("live", fixture.temp.path().join("live")),
        ("gone", deleted_volume.join("gone")),
        ("unreachable", file_volume.join("unreachable")),
        ("empty-mount", empty_mount.join("proj")),
        ("removed-mount", removed_mount.join("proj")),
    ];
    #[cfg(unix)]
    let checkouts = {
        let mut checkouts = checkouts;
        checkouts.push(("dangling-parent", dangling_parent.join("proj")));
        checkouts
    };
    for (name, repo) in checkouts {
        Fixture::seed_repo(&repo);
        let before = task_store_partitions(&partitions);
        fixture.success(&repo, &["workspace", "init", "--name", name]);
        fixture.success(
            &repo,
            &[
                "task",
                "add",
                "--title",
                &format!("{name} task"),
                "--description",
                "seed a task bundle",
                "--complexity",
                "low",
            ],
        );
        let created = task_store_partitions(&partitions)
            .into_keys()
            .filter(|partition| !before.contains_key(partition))
            .collect::<Vec<_>>();
        let [partition] = created.as_slice() else {
            panic!("{name} must create exactly one partition: {created:?}");
        };
        partition_of.insert(name, partition.clone());
    }
    fs::create_dir_all(partitions.join("ws_residue")).expect("create empty partition");
    fs::create_dir_all(partitions.join("ws_unowned/ORB-00077")).expect("create unowned bundle");

    // Confirmed absent: the immediate parent is readable and still populated.
    fs::remove_dir_all(deleted_volume.join("gone")).expect("delete checkout");
    fs::write(deleted_volume.join("sibling"), b"still mounted").expect("populate parent");
    // Not absence: resolving the checkout fails with ENOTDIR.
    fs::remove_dir_all(&file_volume).expect("remove volume");
    fs::write(&file_volume, b"not a directory").expect("replace volume with a file");
    // Unmounting exposes either an empty mount point or no mount point at all.
    fs::remove_dir_all(empty_mount.join("proj")).expect("empty mount point");
    fs::remove_dir_all(&removed_mount).expect("remove mount point");
    #[cfg(unix)]
    {
        fs::remove_dir_all(&dangling_parent).expect("remove symlink parent checkout");
        std::os::unix::fs::symlink(fixture.temp.path().join("offline-volume"), &dangling_parent)
            .expect("create dangling parent symlink");
        assert!(fs::symlink_metadata(&dangling_parent).is_ok());
        assert_eq!(
            fs::metadata(dangling_parent.join("proj"))
                .expect_err("checkout stat must enter confirm_absence")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(
            fs::read_dir(&dangling_parent).is_err(),
            "ORB-14516: existing parent symlink must exercise the read_dir failure arm"
        );
    }

    let global_root = home.join(".orbit");
    let classified = orbit_cmd::task_store::inspect_task_store_partitions(&global_root)
        .expect("inspect partition classifications")
        .expect("partitions exist");
    assert_eq!(
        classified
            .stale
            .iter()
            .map(|p| p.path.clone())
            .collect::<Vec<_>>(),
        vec![partitions.join(&partition_of["gone"])],
        "ORB-14516: only absence beneath a populated immediate parent is stale"
    );
    let unreachable_names = partition_of
        .keys()
        .copied()
        .filter(|name| !matches!(*name, "live" | "gone"))
        .collect::<Vec<_>>();
    for name in &unreachable_names {
        assert!(
            classified
                .unreachable
                .iter()
                .any(|p| p.partition.path == partitions.join(&partition_of[name])),
            "ORB-14516: {name} must be unreachable: {classified:?}"
        );
    }

    let before = task_store_partitions(&partitions);
    for name in partition_of.keys() {
        assert_eq!(
            before[&partition_of[name]].len(),
            1,
            "{name} partition holds its bundle: {before:?}"
        );
    }

    let refused = fixture.output(
        &fixture.temp.path().join("live"),
        &["doctor", "--fix-orphan-task-stores", "--json"],
    );
    assert!(
        !refused.status.success(),
        "an unconfirmed repair must refuse: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert_eq!(
        task_store_partitions(&partitions),
        before,
        "an unconfirmed repair must not delete anything"
    );

    let repaired = fixture.json(
        &fixture.temp.path().join("live"),
        &["doctor", "--fix-orphan-task-stores", "--confirm", "--json"],
    );
    assert!(
        json_rows(&repaired)
            .iter()
            .any(|row| row["check"] == "fix-orphan-task-stores" && row["status"] == "ok"),
        "the confirmed repair reports its outcome: {repaired}"
    );
    let mut expected = before;
    expected.remove(&partition_of["gone"]);
    expected.remove("ws_residue");
    assert_eq!(
        task_store_partitions(&partitions),
        expected,
        "only the empty partition and the confirmed-absent checkout's partition may go"
    );
    for name in unreachable_names {
        assert!(
            orbit_cmd::task_store::partition_is_bound(&global_root, &partition_of[name])
                .expect("check retained binding"),
            "ORB-14516: the repair must retain {name}'s binding for remount recovery"
        );
    }
    for mount in [&empty_mount, &removed_mount] {
        fs::create_dir_all(mount.join("proj")).expect("restore checkout after remount");
    }
    let restored = orbit_cmd::task_store::inspect_task_store_partitions(&global_root)
        .expect("inspect restored checkouts")
        .expect("partitions exist");
    for name in ["empty-mount", "removed-mount"] {
        assert!(
            restored
                .unreachable
                .iter()
                .all(|p| p.partition.path != partitions.join(&partition_of[name])),
            "ORB-14516: remounted {name} must become a live claim"
        );
    }
}

/// `--fix-stale-locks` clears a dead holder record while preserving its file.
/// It keeps a live holder record, and a dead record in a file a live process
/// holds through the OS lock.
#[cfg(unix)]
fn doctor_stale_lock_repair_keeps_locks_a_live_process_holds() {
    let fixture = Fixture::seeded("repo");
    let repo = fixture.repo.as_path();

    fixture.init(repo);

    let state_dir = repo.join(".orbit/state");
    let dead_pid = reaped_child_pid();
    let dead_lock = state_dir.join(".crashed-op.lock");
    let live_lock = state_dir.join(".live-op.lock");
    let held_lock = state_dir.join(".held-op.lock");
    write_lock_holder(&dead_lock, dead_pid, "crashed op");
    write_lock_holder(&live_lock, std::process::id(), "live op");
    write_lock_holder(&held_lock, dead_pid, "stale metadata");
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&held_lock)
        .expect("open held lock");
    held.lock().expect("hold the lock");

    let rows = fixture.json(repo, &["doctor", "--fix-stale-locks", "--json"]);
    held.unlock().expect("release the lock");

    assert!(
        json_rows(&rows)
            .iter()
            .any(|row| row["check"] == "fix-stale-locks" && row["status"] == "ok"),
        "the repair reports its outcome: {rows}"
    );
    assert_eq!(
        fs::read(&dead_lock).expect("dead holder's lock file remains"),
        b"",
        "a dead holder's record is cleared"
    );
    assert!(
        orbit_common::fs::file_lock::read_file_lock_holder(&live_lock)
            .is_some_and(|holder| holder.pid == std::process::id()),
        "a live holder's record must remain"
    );
    assert!(
        orbit_common::fs::file_lock::read_file_lock_holder(&held_lock)
            .is_some_and(|holder| holder.pid == dead_pid),
        "a held lock's record must remain whatever its metadata says"
    );
}

/// `--remove-graph` refuses to walk through a symlinked `knowledge` ancestor.
/// The public runtime cleanup matrix separately proves graph-leaf unlinking;
/// this real CLI refusal keeps the ancestor link and outside sentinels.
#[cfg(unix)]
fn doctor_graph_cleanup_never_deletes_through_symlinks() {
    let fixture = Fixture::seeded("repo");
    let repo = fixture.repo.as_path();

    fixture.init(repo);
    let orbit_dir = repo.join(".orbit");

    let outside = fixture.temp.path().join("outside");
    fs::create_dir_all(&outside).expect("create outside");
    let outside_marker = outside.join("keep.db");
    fs::write(&outside_marker, b"keep").expect("write outside marker");
    let external_graph = outside.join("graph");
    fs::create_dir_all(&external_graph).expect("create external graph");
    let external_graph_marker = external_graph.join("keep.db");
    fs::write(&external_graph_marker, b"keep").expect("write external graph marker");
    let knowledge_link = orbit_dir.join("knowledge");
    if knowledge_link.exists() {
        fs::remove_dir_all(&knowledge_link).expect("clear knowledge directory");
    }
    std::os::unix::fs::symlink(&outside, &knowledge_link).expect("link knowledge outside");

    let refused = fixture.output(repo, &["doctor", "--remove-graph", "--json"]);
    assert!(
        !refused.status.success(),
        "a symlinked ancestor is refused: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        external_graph_marker.is_file(),
        "cleanup must not delete through a symlinked ancestor"
    );
    assert!(outside_marker.is_file());
    assert!(
        fs::symlink_metadata(&knowledge_link)
            .expect("knowledge link")
            .file_type()
            .is_symlink(),
        "the ancestor link itself stays"
    );
}

/// Root-resolution precedence observed through `config show --json`: an
/// explicit `--root` beats `ORBIT_ROOT`, which beats linked-worktree
/// discovery, which beats walking up from a subdirectory. `--workspace`
/// selects a registered checkout from outside any repository. An explicit
/// root that is not an initialized workspace is refused, not adopted.
fn root_resolution_precedence() {
    struct Case<'a> {
        name: &'a str,
        cwd: &'a Path,
        root_flag: Option<&'a Path>,
        env_root: Option<&'a Path>,
        workspace: Option<&'a str>,
        /// `(shared_root, local_root)`, or `None` when resolution must fail.
        expected: Option<(&'a Path, &'a Path)>,
    }

    let fixture = Fixture::seeded("main");
    let base = fs::canonicalize(fixture.temp.path()).expect("canonical tempdir");
    let home = fixture.home.as_path();
    let main_repo = fixture.repo.as_path();
    let other_repo = base.join("other");
    let linked = base.join("main-linked");
    let outside = base.join("outside");
    let uninitialized = base.join("uninitialized");
    for dir in [home, main_repo, &other_repo, &outside, &uninitialized] {
        fs::create_dir_all(dir).expect("create fixture directory");
    }
    Fixture::seed_repo(&other_repo);
    fixture.linked_worktree(&linked, Some("root-precedence"));
    fixture.success(main_repo, &["workspace", "init", "--name", "main"]);
    fixture.success(&other_repo, &["workspace", "init", "--name", "other"]);
    let subdirectory = main_repo.join("src/nested");
    fs::create_dir_all(&subdirectory).expect("create subdirectory");

    let main_orbit = main_repo.join(".orbit");
    let other_orbit = other_repo.join(".orbit");
    let linked_orbit = linked.join(".orbit");
    let cases = [
        Case {
            name: "--root beats ORBIT_ROOT and the linked worktree",
            cwd: &linked,
            root_flag: Some(&main_orbit),
            env_root: Some(&other_orbit),
            workspace: None,
            expected: Some((&main_orbit, &main_orbit)),
        },
        Case {
            name: "ORBIT_ROOT beats the linked worktree",
            cwd: &linked,
            root_flag: None,
            env_root: Some(&other_orbit),
            workspace: None,
            expected: Some((&other_orbit, &other_orbit)),
        },
        Case {
            name: "a linked worktree shares the main root and keeps its own local root",
            cwd: &linked,
            root_flag: None,
            env_root: None,
            workspace: None,
            expected: Some((&main_orbit, &linked_orbit)),
        },
        Case {
            name: "a subdirectory walks up to its checkout",
            cwd: &subdirectory,
            root_flag: None,
            env_root: None,
            workspace: None,
            expected: Some((&main_orbit, &main_orbit)),
        },
        Case {
            name: "--workspace selects a registered checkout from outside any repository",
            cwd: &outside,
            root_flag: None,
            env_root: None,
            workspace: Some("other"),
            expected: Some((&other_orbit, &other_orbit)),
        },
        Case {
            name: "--root must name an initialized workspace",
            cwd: main_repo,
            root_flag: Some(&uninitialized),
            env_root: None,
            workspace: None,
            expected: None,
        },
        Case {
            name: "ORBIT_ROOT must name an initialized workspace",
            cwd: main_repo,
            root_flag: None,
            env_root: Some(&uninitialized),
            workspace: None,
            expected: None,
        },
    ];

    for case in cases {
        let mut args: Vec<String> = Vec::new();
        if let Some(root) = case.root_flag {
            args.extend(["--root".to_string(), root.to_string_lossy().into_owned()]);
        }
        if let Some(workspace) = case.workspace {
            args.extend(["--workspace".to_string(), workspace.to_string()]);
        }
        args.extend(["config", "show", "--json"].map(String::from));
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = fixture.output_with_root(case.cwd, &args, case.env_root);
        let stdout = String::from_utf8_lossy(&output.stdout);
        match case.expected {
            Some((shared_root, local_root)) => {
                assert!(
                    output.status.success(),
                    "{}: {stdout}\n{}",
                    case.name,
                    String::from_utf8_lossy(&output.stderr)
                );
                let value: Value = serde_json::from_slice(&output.stdout).expect("config json");
                assert_eq!(
                    (
                        string_field(&value, "shared_root"),
                        string_field(&value, "local_root")
                    ),
                    (
                        shared_root.to_string_lossy().as_ref(),
                        local_root.to_string_lossy().as_ref()
                    ),
                    "{}",
                    case.name
                );
            }
            None => assert!(!output.status.success(), "{}: {stdout}", case.name),
        }
    }
    assert!(
        !linked_orbit.exists(),
        "resolution must not materialize a linked-worktree .orbit directory"
    );
}

/// Verify config reports the shared store and checkout-local roots without
/// the retired root aliases.
fn assert_root_fields(value: &Value, shared_root: &Path, local_root: &Path) {
    let shared = shared_root.to_string_lossy();
    let local = local_root.to_string_lossy();
    assert_eq!(string_field(value, "shared_root"), shared.as_ref());
    assert_eq!(string_field(value, "local_root"), local.as_ref());
    assert!(
        value.get("workspace_root").is_none(),
        "legacy `workspace_root` alias must be removed from `config show --json` output (use `shared_root`)"
    );
    assert!(
        value.get("root").is_none(),
        "legacy `root` alias must be removed from `config show --json` output (use `shared_root`)"
    );
    assert!(
        value.get("selected_root").is_none(),
        "legacy `selected_root` alias must be removed from `config show --json` output (use `shared_root`)"
    );
}

fn wait_for_run_terminal_state(custom_root: &Path, run_id: &str) -> Value {
    // Observe the worker's persisted state without starting another Orbit
    // participant during bootstrap. Repeated `run show` clients can lose the
    // 2s generation-admission race under suite load, independently of the
    // registry writer this test holds (ORB-14396).
    let observer = rusqlite::Connection::open_with_flags(
        custom_root.join("orbit.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open run observer");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let last = observer
            .query_row(
                "SELECT state, pid, started_at FROM job_runs WHERE run_id = ?1",
                [run_id],
                |row| {
                    Ok(serde_json::json!({"run": {
                        "state": row.get::<_, String>(0)?,
                        "pid": row.get::<_, Option<u32>>(1)?,
                        "started_at": row.get::<_, Option<String>>(2)?,
                    }}))
                },
            )
            .expect("observe the submitted run");
        if last["run"]["state"]
            .as_str()
            .is_some_and(|state| state != "pending" && state != "running")
        {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            detached_worker_diagnostic(custom_root, run_id, &last)
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn detached_worker_diagnostic(custom_root: &Path, run_id: &str, last: &Value) -> String {
    let worker_log = custom_root
        .join("state/logs")
        .join(format!("{run_id}.worker.log"));
    let worker_log_text = fs::read_to_string(&worker_log)
        .unwrap_or_else(|error| format!("<missing worker log {}: {error}>", worker_log.display()));
    let config = fs::read_to_string(custom_root.join("config.toml")).unwrap_or_default();
    let state = last["run"]["state"].as_str().unwrap_or("missing");
    let pid = last["run"]["pid"].clone();
    let started_at = last["run"]["started_at"].clone();
    format!(
        "run {run_id} stayed non-terminal under --root {}; \
         state={state} pid={pid} started_at={started_at}; last show={last}; \
         config.toml:\n{config}\nworker log:\n{worker_log_text}",
        custom_root.display()
    )
}

/// `workspace init` only seeds `[crews]` / `default_crew` when it sees an agent
/// CLI on PATH. The stub planted by [`plant_agent_cli_stub`] covers the common
/// case; write the same contract into the isolated root so a detached worker
/// cannot lose crew resolution if detection did not stick.
fn pin_default_crew_for_isolated_root(custom_root: &Path) {
    let config_path = custom_root.join("config.toml");
    let existing = fs::read_to_string(&config_path).unwrap_or_default();
    if existing.contains("default_crew") && existing.contains("[crews.") {
        return;
    }
    fs::write(
        &config_path,
        r#"[workflow]
default_crew = "codex"

[crews.codex]
provider = "codex"
model = "gpt-5.4"
backend = "cli"
"#,
    )
    .expect("pin isolated-root default crew");
}

fn string_field<'a>(value: &'a Value, field: &str) -> &'a str {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected string field `{field}` in {value}"))
}

/// Upper bound on one `orbit` child, so a wedged command fails the test
/// instead of hanging it.
const ORBIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

fn orbit_command(
    cwd: &Path,
    home: &Path,
    args: &[&str],
    orbit_root: Option<&Path>,
) -> AssertCommand {
    let mut command = cargo_bin_cmd!("orbit");
    command
        .current_dir(cwd)
        .env(
            "PATH",
            stub_first_path(&plant_agent_cli_stub(home, "codex")),
        )
        .env("HOME", home)
        .env("USERPROFILE", home)
        .timeout(ORBIT_COMMAND_TIMEOUT)
        .args(args);
    clear_inherited_authority_env(&mut command);
    set_orbit_root_env(&mut command, orbit_root);
    command
}

fn write_machine_identity(home: &Path) {
    fs::create_dir_all(home.join(".orbit")).expect("create global root");
    fs::write(
        home.join(".orbit/config.toml"),
        "[machine]\nid = \"hm_worktree_resolution\"\nname = \"worktree-resolution\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write machine identity");
}

/// Every task-store partition under `partitions`, mapped to the task bundles
/// (subdirectories) it holds.
fn task_store_partitions(
    partitions: &Path,
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let directory_names = |dir: &Path| {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>()
    };
    directory_names(partitions)
        .into_iter()
        .map(|partition| {
            let bundles = directory_names(&partitions.join(&partition));
            (partition, bundles)
        })
        .collect()
}

/// PID of a child that has already exited and been reaped: a holder that is
/// certainly dead.
#[cfg(unix)]
fn reaped_child_pid() -> u32 {
    let mut child = StdCommand::new("true").spawn().expect("spawn child");
    let pid = child.id();
    child.wait().expect("reap child");
    pid
}

/// Write the holder record an Orbit file lock carries.
#[cfg(unix)]
fn write_lock_holder(path: &Path, pid: u32, label: &str) {
    fs::create_dir_all(path.parent().expect("lock parent")).expect("create lock parent");
    let holder = serde_json::json!({
        "pid": pid,
        "acquired_at": "2026-10-03T00:00:00Z",
        "label": label,
    });
    fs::write(path, holder.to_string()).expect("write lock holder");
}

/// Prevent ambient managed-run authority from changing child command
/// behavior — or, worse, routing its writes (ORB-11300). `orbit_command`
/// applies this before `set_orbit_root_env`, so an explicit fixture root still
/// wins.
fn clear_inherited_authority_env(command: &mut AssertCommand) {
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    // Not Orbit authority: a coverage build hands every child the same profile
    // path, which the child would then clobber.
    command.env_remove("LLVM_PROFILE_FILE");
}

fn set_orbit_root_env(command: &mut AssertCommand, orbit_root: Option<&Path>) {
    match orbit_root {
        Some(path) => {
            command.env("ORBIT_ROOT", path);
        }
        None => {
            command.env_remove("ORBIT_ROOT");
        }
    }
}

/// Write an executable no-op named `name` into `<home>/stub-bin`, standing in
/// for an agent CLI during detection, and return that directory.
///
/// `orbit workspace init` freezes crew seeding to the agent CLIs it finds on
/// `PATH`, so a host with none installed — every CI runner — seeds an empty
/// `[crews]` table and leaves `[workflow].default_crew` unset. Any run that has
/// to resolve a crew then dies with `no crew selected`. Planting on every
/// invocation keeps the stub in place before `init` runs, whatever order the
/// tests call the helpers in; nothing here dispatches an agent, so the stub only
/// has to exist and be executable.
fn plant_agent_cli_stub(home: &Path, name: &str) -> PathBuf {
    let bin = home.join("stub-bin");
    fs::create_dir_all(&bin).expect("create stub CLI directory");
    let stub = bin.join(name);
    fs::write(&stub, "#!/bin/sh\nexit 0\n").expect("write stub agent CLI");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))
            .expect("mark the stub agent CLI executable");
    }
    bin
}

/// `PATH` with the fixture's stub directory first, so agent detection sees the
/// stubs regardless of what the host has installed.
fn stub_first_path(bin: &Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bin.to_path_buf()];
    entries.extend(std::env::split_paths(&inherited));
    std::env::join_paths(entries).expect("join PATH entries")
}

fn assert_inactive_plugin(fixture: &Fixture, cwd: &Path, name: &str, source: &Path) {
    let plugin = fixture.auto_task(cwd, name);
    assert_eq!(plugin["inactive_plugin"]["namespace"], "absent-probe");
    assert!(
        plugin["skipped_reason"]
            .as_str()
            .expect("inactive reason")
            .contains(source.to_str().expect("source path")),
        "inactive reason must name the supplying catalog: {plugin}"
    );
    let visible = fixture.auto_tasks(cwd, false);
    assert_auto_task_hidden(&visible, name, "inactive plugin definitions stay hidden");
    let all = fixture.auto_tasks(cwd, true);
    assert!(json_rows(&all).iter().any(|row| row["name"] == name
        && row["inactive_plugin"]["namespace"] == "absent-probe"));
}

fn json_rows(value: &Value) -> &[Value] {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected JSON rows: {value}"))
}

fn assert_auto_task_hidden(listed: &Value, name: &str, reason: &str) {
    assert!(
        !json_rows(listed).iter().any(|row| row["name"] == name),
        "{reason}: {name}: {listed}"
    );
}

fn local_write_commands<'a>(description: &'a str, reason: &'a str) -> [Vec<&'a str>; 5] {
    [
        vec!["auto-task", "mint", "candidate-probe"],
        vec![
            "auto-task",
            "update",
            "candidate-probe",
            "--description",
            description,
        ],
        vec!["auto-task", "toggle", "candidate-probe", "on"],
        vec![
            "auto-task",
            "recover",
            "candidate-probe",
            "--adopt-settings",
            "--reason",
            reason,
        ],
        vec!["auto-task", "reset", "candidate-probe", "--reason", reason],
    ]
}

fn assert_definition_source(shown: &Value, root: &Path, path: &Path) {
    assert_eq!(
        shown["definition_source"]["root"],
        root.to_string_lossy().as_ref()
    );
    assert_eq!(
        shown["definition_source"]["path"],
        path.to_string_lossy().as_ref()
    );
}

fn write_auto_task_fixture(orbit_dir: &Path, description: &str) {
    let directory = orbit_dir.join("auto_tasks");
    fs::create_dir_all(&directory).expect("create auto-task fixture directory");
    fs::write(
        directory.join("candidate-probe.yaml"),
        format!(
            r#"schemaVersion: 1
name: candidate-probe
description: {description}
enabled: false
schedule:
  cron: 0 * * * *
template:
  title: Inspect candidate definition
  description: Exercise definition-source selection.
  acceptance_criteria:
  - The selected definition is reported.
  task_type: chore
  tags:
  - candidate-probe
  priority: low
  complexity: low
  status: backlog
dedupe: skip_if_open
created_by: system
created_at: 2026-09-14T00:00:00Z
updated_by: system
updated_at: 2026-09-14T00:00:00Z
"#
        ),
    )
    .expect("write auto-task fixture");
}

// The three historical setup paths share the same init -> bind -> list sequence.
#[derive(Clone, Copy)]
enum DataDirSetup {
    Clean,
    ProvidersFromHome,
    SyntheticCheckout,
}

fn doctor_providers_with_explicit_root_does_not_bind_parent_of_data_dir() {
    check_data_dir_checkout(DataDirSetup::ProvidersFromHome);
}

fn clean_root_workspace_init_then_auto_task_list_succeeds() {
    check_data_dir_checkout(DataDirSetup::Clean);
}

fn workspace_init_force_rebinds_synthetic_data_dir_checkout() {
    check_data_dir_checkout(DataDirSetup::SyntheticCheckout);
}

fn check_data_dir_checkout(setup: DataDirSetup) {
    let fixture = Fixture::seeded("qa-ws");
    let home = fixture.home.as_path();
    let repo = fixture.repo.as_path();
    let data_dir = fixture.temp.path().join("qa-root");
    fs::create_dir_all(&data_dir).expect("create data dir");
    let data_dir_arg = data_dir.to_string_lossy().into_owned();
    let from_home = !matches!(setup, DataDirSetup::Clean);
    fixture.success(
        if from_home { home } else { repo },
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "qa",
            "--task-prefix",
            "QAZ",
            "--root",
            &data_dir_arg,
        ],
    );
    if from_home {
        fixture.success(
            home,
            &[
                "--root",
                &data_dir_arg,
                "doctor",
                "providers",
                "--format",
                "json",
            ],
        );
    }
    match setup {
        DataDirSetup::ProvidersFromHome => {
            let parent = canonicalize_or_original(data_dir.parent().expect("data dir parent"));
            for (workspace_id, repo_root, orbit_dir) in checkout_bindings(&data_dir) {
                assert_ne!(
                    canonicalize_or_original(Path::new(&repo_root)),
                    parent,
                    "doctor providers bound parent(data-dir) as {workspace_id} repo_root={repo_root} orbit_dir={orbit_dir}"
                );
            }
        }
        DataDirSetup::SyntheticCheckout => seed_synthetic_data_dir_checkout(&data_dir),
        DataDirSetup::Clean => {}
    }
    let mut bind = vec!["--root", &data_dir_arg, "workspace", "init", "--name", "qa"];
    if from_home {
        bind.push("--force");
    }
    bind.extend(["--format", "json"]);
    fixture.success(repo, &bind);
    if matches!(setup, DataDirSetup::SyntheticCheckout) {
        let repo_canon = canonicalize_or_original(repo);
        let data_canon = canonicalize_or_original(&data_dir);
        let bindings = checkout_bindings(&data_dir);
        assert!(
            bindings.iter().any(|(workspace_id, repo_root, orbit_dir)| {
                workspace_id == "ws_qa"
                    && canonicalize_or_original(Path::new(repo_root)) == repo_canon
                    && canonicalize_or_original(Path::new(orbit_dir)) == data_canon
            }),
            "expected ws_qa checkout for the git repo; got {bindings:?}"
        );
        assert!(
            bindings
                .iter()
                .all(|(workspace_id, _, orbit_dir)| workspace_id != "tmp-5b7149"
                    || canonicalize_or_original(Path::new(orbit_dir)) != data_canon),
            "synthetic tmp-* row must not keep the data dir: {bindings:?}"
        );
    }
    fixture.assert_workspace_commands_use_named_id(repo, &data_dir, "ws_qa");
}

fn checkout_bindings(data_dir: &Path) -> Vec<(String, String, String)> {
    let db = data_dir.join("tasks").join("index.sqlite");
    if !db.is_file() {
        return Vec::new();
    }
    let conn = rusqlite::Connection::open(&db).expect("open task registry");
    let mut stmt = conn
        .prepare("SELECT workspace_id, repo_root, orbit_dir FROM workspace_checkout_bindings")
        .expect("prepare checkout query");
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .expect("query checkout bindings");
    rows.collect::<Result<Vec<_>, _>>()
        .expect("collect checkout bindings")
}

fn seed_synthetic_data_dir_checkout(data_dir: &Path) {
    let db = data_dir.join("tasks").join("index.sqlite");
    let now = "2026-08-22T00:00:00+00:00";
    let parent = canonicalize_or_original(data_dir.parent().expect("data dir parent"));
    let data_canon = canonicalize_or_original(data_dir);
    let conn = rusqlite::Connection::open(&db).expect("open task registry");
    conn.execute(
        "INSERT INTO workspace_bindings (
            workspace_id, slug, repo_fingerprint, created_at, updated_at
        ) VALUES (?1, ?2, NULL, ?3, ?3)",
        rusqlite::params!["tmp-5b7149", "tmp", now],
    )
    .expect("insert synthetic workspace");
    conn.execute(
        "INSERT INTO workspace_checkout_bindings (
            workspace_id, repo_root, workspace_path, orbit_dir, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        rusqlite::params![
            "tmp-5b7149",
            parent.to_string_lossy().as_ref(),
            parent.to_string_lossy().as_ref(),
            data_canon.to_string_lossy().as_ref(),
            now,
        ],
    )
    .expect("insert synthetic checkout");
    fs::write(
        data_dir.join("config.yaml"),
        "schema_version: 1\nworkspace_id: tmp-5b7149\n",
    )
    .expect("write synthetic identity");
}

fn canonicalize_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn registered_workspace_id(fixture: &Fixture, repo: &Path) -> String {
    let listed = fixture.json(repo, &["workspace", "list", "--format", "json"]);
    let repo = canonicalize_or_original(repo);
    listed
        .as_array()
        .expect("workspace list rows")
        .iter()
        .find(|row| {
            row["repo_root"]
                .as_str()
                .is_some_and(|root| canonicalize_or_original(Path::new(root)) == repo)
        })
        .and_then(|row| row["id"].as_str())
        .unwrap_or_else(|| panic!("registered workspace for {}: {listed}", repo.display()))
        .to_string()
}

struct ConfigStamp {
    bytes: Vec<u8>,
    modified: std::time::SystemTime,
}

fn config_stamp(path: &Path) -> ConfigStamp {
    let meta =
        fs::metadata(path).unwrap_or_else(|error| panic!("metadata {}: {error}", path.display()));
    ConfigStamp {
        bytes: fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
        modified: meta
            .modified()
            .unwrap_or_else(|error| panic!("mtime {}: {error}", path.display())),
    }
}

fn assert_config_unchanged(path: &Path, stamp: &ConfigStamp, label: &str) {
    let now = config_stamp(path);
    assert_eq!(
        now.bytes, stamp.bytes,
        "{label}: live workspace config.toml content changed"
    );
    assert_eq!(
        now.modified, stamp.modified,
        "{label}: live workspace config.toml mtime changed"
    );
}

fn assert_indirect_workspace_refusal(output: &std::process::Output, live_root: &Path, cwd: &Path) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "indirect workspace use must be refused, stderr: {stderr}"
    );
    let caller = cwd.join(".orbit");
    for (label, needle) in [
        ("resolved workspace", live_root.to_str().expect("live root")),
        ("caller directory", cwd.to_str().expect("cwd")),
        ("caller .orbit", caller.to_str().expect("caller orbit")),
        ("--root escape hatch", "--root"),
    ] {
        assert!(
            stderr.contains(needle),
            "refusal must name {label} ({needle}): {stderr}"
        );
    }
}

/// An empty `.orbit/` under a linked worktree must not bootstrap or rewrite
/// the main checkout. A nested directory that already has `config.toml`
/// resolves to itself, and ordinary reads keep the shared main root.
#[test]
fn empty_nested_orbit_in_linked_worktree_does_not_rewrite_main_workspace() {
    #[cfg(unix)]
    if crate::isolated_test::run_in_child(
        "worktree_resolution::empty_nested_orbit_in_linked_worktree_does_not_rewrite_main_workspace",
    ) {
        return;
    }

    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();
    let linked = fixture.init_worktree("repo-worktree", Some("orbit-empty-nested-orbit"));
    let main_orbit = main_repo.join(".orbit");
    let main_config = main_orbit.join("config.toml");
    // Workspace init records identity in config.yaml. The settings file the
    // incident rewrote is config.toml, so create it with a known value first.
    fixture.success(
        main_repo,
        &[
            "config",
            "set",
            "--fresh",
            "workflow.base_branch",
            "agent-main",
        ],
    );
    let stamp = config_stamp(&main_config);
    let work = linked.join("scratch").join("work");
    fs::create_dir_all(work.join(".orbit")).expect("empty caller .orbit");

    let init_force = fixture.output(&work, &["workspace", "init", "--force"]);
    assert_indirect_workspace_refusal(&init_force, &main_orbit, &work);
    assert_config_unchanged(&main_config, &stamp, "workspace init --force");
    assert!(
        !work.join(".orbit/config.toml").exists(),
        "refused workspace init must not initialize the empty fixture"
    );

    let config_set = fixture.output(&work, &["config", "set", "workflow.base_branch", "main"]);
    assert_indirect_workspace_refusal(&config_set, &main_orbit, &work);
    assert_config_unchanged(&main_config, &stamp, "config set");

    let orbit_init = fixture.output(
        &work,
        &[
            "init",
            "--non-interactive",
            "--skip-host-prerequisites",
            "--machine-name",
            "empty-nested",
            "--task-prefix",
            "ENX",
        ],
    );
    let init_stderr = String::from_utf8_lossy(&orbit_init.stderr);
    assert!(
        orbit_init.status.success(),
        "orbit init targets the global root and must still succeed: {init_stderr}"
    );
    assert_config_unchanged(&main_config, &stamp, "orbit init");
    assert!(
        !work.join(".orbit/config.toml").exists(),
        "orbit init must not write the empty fixture config"
    );

    let from_empty = fixture.json(&work, &["config", "show", "--json"]);
    assert_root_fields(&from_empty, &main_orbit, &linked.join(".orbit"));
    assert_config_unchanged(&main_config, &stamp, "config show from empty fixture");

    let with_config = linked.join("scratch").join("with-config");
    fs::create_dir_all(&with_config).expect("nested initialized fixture");
    let with_orbit = with_config.join(".orbit");
    let with_orbit_arg = with_orbit.to_string_lossy().into_owned();
    fixture.success(
        &with_config,
        &["--root", &with_orbit_arg, "workspace", "init"],
    );
    assert_config_unchanged(&main_config, &stamp, "explicit --root workspace init");
    let from_initialized = fixture.json(&with_config, &["config", "show", "--json"]);
    assert_root_fields(&from_initialized, &with_orbit, &with_orbit);
    assert_config_unchanged(
        &main_config,
        &stamp,
        "config show from initialized nested fixture",
    );

    let from_env = fixture.json_with_root(&work, &["config", "show", "--json"], Some(&main_orbit));
    assert_root_fields(&from_env, &main_orbit, &main_orbit);
    assert_config_unchanged(&main_config, &stamp, "ORBIT_ROOT config show");

    let bare = fixture.temp.path().join("bare-orbit");
    fs::create_dir_all(&bare).expect("uninitialized explicit root");
    let uninitialized = fixture.output_with_root(&work, &["config", "show", "--json"], Some(&bare));
    let uninitialized_stderr = String::from_utf8_lossy(&uninitialized.stderr);
    assert!(
        !uninitialized.status.success(),
        "uninitialized explicit root must still fail: {uninitialized_stderr}"
    );
    assert!(
        uninitialized_stderr.contains("not an Orbit workspace"),
        "uninitialized explicit root keeps its existing refusal: {uninitialized_stderr}"
    );
    assert_config_unchanged(&main_config, &stamp, "uninitialized explicit root");

    let fresh = fixture.temp.path().join("fresh");
    Fixture::seed_repo(&fresh);
    fixture.success(&fresh, &["workspace", "init"]);
    assert!(
        fresh.join(".orbit/config.yaml").is_file(),
        "workspace init in a fresh project must bootstrap that project's .orbit"
    );
    assert_config_unchanged(&main_config, &stamp, "fresh project workspace init");

    let from_linked = fixture.json(&linked, &["config", "show", "--json"]);
    assert_root_fields(&from_linked, &main_orbit, &linked.join(".orbit"));
    assert_config_unchanged(&main_config, &stamp, "config show from linked worktree");

    fs::create_dir_all(fixture.home.join(".git"))
        .expect("stop home from walking into another repo");
    let from_home = fixture.output(&fixture.home, &["workspace", "init"]);
    let home_stderr = String::from_utf8_lossy(&from_home.stderr);
    assert!(
        !from_home.status.success(),
        "global home must still be refused: {home_stderr}"
    );
    assert!(
        home_stderr.contains("global Orbit root"),
        "global home keeps its existing refusal: {home_stderr}"
    );
    assert_config_unchanged(&main_config, &stamp, "global home workspace init");
}

/// An explicit `--workspace` selector names its target directly: a nested
/// empty `.orbit/` (or one holding a malformed config) in the caller's
/// working directory must not refuse the write, while an implicitly
/// selected write from the same directory stays refused.
#[test]
fn config_set_explicit_workspace_selector_bypasses_indirect_refusal() {
    #[cfg(unix)]
    if crate::isolated_test::run_in_child(
        "worktree_resolution::config_set_explicit_workspace_selector_bypasses_indirect_refusal",
    ) {
        return;
    }

    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();
    fixture.init(main_repo);
    fixture.success(
        main_repo,
        &[
            "config",
            "set",
            "--fresh",
            "workflow.base_branch",
            "agent-main",
        ],
    );
    let repo_b = canonicalize_or_original(&fixture.temp.path().join("repo-b"));
    Fixture::seed_repo(&repo_b);
    fixture.init(&repo_b);
    fixture.success(
        &repo_b,
        &[
            "config",
            "set",
            "--fresh",
            "workflow.base_branch",
            "b-before",
        ],
    );
    let id_a = registered_workspace_id(&fixture, main_repo);
    let id_b = registered_workspace_id(&fixture, &repo_b);
    let config_a = main_repo.join(".orbit/config.toml");
    let config_b = repo_b.join(".orbit/config.toml");

    let nested = main_repo.join("nested");
    fs::create_dir_all(nested.join(".orbit")).expect("empty caller .orbit");

    let stamp_a = config_stamp(&config_a);
    let stamp_b = config_stamp(&config_b);
    let implicit = fixture.output(
        &nested,
        &["config", "set", "workflow.base_branch", "implicit"],
    );
    assert!(
        !implicit.status.success(),
        "implicit config set from an empty nested .orbit must be refused: {}",
        String::from_utf8_lossy(&implicit.stderr)
    );
    assert_config_unchanged(&config_a, &stamp_a, "implicit config set");
    assert_config_unchanged(&config_b, &stamp_b, "implicit config set");

    fixture.success(
        &nested,
        &[
            "--workspace",
            &id_a,
            "config",
            "set",
            "workflow.base_branch",
            "explicit-a",
        ],
    );
    assert!(
        fs::read_to_string(&config_a)
            .expect("workspace A config")
            .contains("explicit-a"),
        "explicit selector must update the named workspace"
    );
    assert_config_unchanged(&config_b, &stamp_b, "explicit A config set");

    // A malformed config in the caller's own .orbit is unrelated to the
    // explicitly selected workspace and must not block its write either.
    fs::write(
        nested.join(".orbit/config.toml"),
        "[workflow\nbase_branch = [",
    )
    .expect("malformed caller config");
    let stamp_a = config_stamp(&config_a);
    fixture.success(
        &nested,
        &[
            "--workspace",
            &id_b,
            "config",
            "set",
            "workflow.base_branch",
            "explicit-b",
        ],
    );
    let written_b = fs::read_to_string(&config_b).expect("workspace B config");
    assert!(
        written_b.contains("explicit-b"),
        "explicit selector must update workspace B despite the caller config: {written_b}"
    );
    assert_config_unchanged(&config_a, &stamp_a, "explicit B config set");

    let implicit = fixture.output(
        &nested,
        &["config", "set", "workflow.base_branch", "implicit"],
    );
    assert!(
        !implicit.status.success(),
        "implicit config set must stay refused: {}",
        String::from_utf8_lossy(&implicit.stderr)
    );
    assert!(
        !fs::read_to_string(&config_b)
            .expect("workspace B config")
            .contains("implicit"),
        "a refused implicit write must not change workspace B"
    );
}

// Keep failure output tied to the historical scenario name, including assertions in shared helpers.
macro_rules! contract_rows {
    ($( $(#[$meta:meta])* $check:ident ),+ $(,)?) => {
        for (name, check) in [$( $(#[$meta])* (stringify!($check), $check as fn()),)+] {
            tracing::info!(case = name, "CLI contract row");
            assert!(std::panic::catch_unwind(check).is_ok(), "CLI contract row {name} failed");
        }
    };
}

#[test]
fn checkout_routing_contracts_0() {
    contract_rows! {
        tool_run_explicit_workspace_resolves_from_home_and_temp_directory,
        config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides,
        submodule_bootstrap_and_runtime_keep_the_submodule_workspace,
        #[cfg(unix)]
        submodule_roots_remain_independent_when_git_is_unavailable,
    }
}

#[test]
fn checkout_routing_contracts_1() {
    contract_rows! {
        submodule_runtime_respects_its_registered_data_directory,
        #[cfg(unix)]
        linked_worktree_gitfile_fallback_checks_the_common_directory,
        #[cfg(unix)]
        workspace_list_skips_unchanged_registry_lock_and_refuses_unpersisted_validation,
        auto_task_show_keeps_alternate_extension_candidates_local,
        auto_task_show_reports_and_honors_the_selected_definition_source,
    }
}

#[test]
fn checkout_routing_contracts_2() {
    contract_rows! {
        auto_task_crud_from_linked_worktree_writes_definition_yaml_to_local_root,
        detached_worker_bootstraps_and_claims_while_registry_writer_is_held,
        doctor_graph_cleanup_uses_split_roots_and_keeps_json_stdout_clean,
        #[cfg(unix)]
        doctor_stale_lock_repair_keeps_locks_a_live_process_holds,
    }
}

#[test]
fn checkout_routing_contracts_3() {
    contract_rows! {
        #[cfg(unix)]
        doctor_graph_cleanup_never_deletes_through_symlinks,
        root_resolution_precedence,
        doctor_providers_with_explicit_root_does_not_bind_parent_of_data_dir,
        clean_root_workspace_init_then_auto_task_list_succeeds,
    }
}

#[test]
fn checkout_routing_contracts_4() {
    contract_rows! {
        workspace_init_force_rebinds_synthetic_data_dir_checkout,
    }
}
