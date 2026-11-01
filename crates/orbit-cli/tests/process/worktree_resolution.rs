#![allow(missing_docs)]

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

macro_rules! isolated {
    ($name:literal) => {
        #[cfg(unix)]
        if crate::isolated_test::run_in_child($name) {
            return;
        }
    };
}

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
        mkdir(&home);
        mkdir(&repo);
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
        let data_dir_arg = arg(data_dir);
        let root: &[&str] = &["--root", &data_dir_arg];
        let listed = self.json(
            repo,
            &[root, &["workspace", "list", "--format", "json"]].concat(),
        );
        assert!(
            json_rows(&listed)
                .iter()
                .any(|workspace| workspace["id"].as_str() == Some(id)),
            "workspace list missing {id}: {listed}"
        );
        self.json(
            repo,
            &[root, &["auto-task", "list", "--format", "json"]].concat(),
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
    fn config_show(&self, cwd: &Path) -> Value {
        self.json(cwd, &["config", "show", "--json"])
    }
    fn workspace_list(&self, cwd: &Path) -> Value {
        self.json(cwd, &["workspace", "list", "--format", "json"])
    }
    fn first_workspace_name(&self, cwd: &Path) -> String {
        let listed = self.workspace_list(cwd);
        listed
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["name"].as_str())
            .unwrap_or_else(|| panic!("registered workspace name in {listed}"))
            .to_string()
    }
    fn auto_task_add(
        &self,
        cwd: &Path,
        selector: &str,
        name: &str,
        title: &str,
        description: Option<&str>,
    ) -> Value {
        let mut args = vec![
            "--workspace",
            selector,
            "auto-task",
            "add",
            "--name",
            name,
            "--every-minutes",
            "60",
            "--title",
            title,
        ];
        if let Some(description) = description {
            args.extend(["--description", description]);
        }
        args.push("--json");
        self.json(cwd, &args)
    }
    fn first_workspace_id(&self, cwd: &Path) -> String {
        let listed = self.workspace_list(cwd);
        listed
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["id"].as_str())
            .unwrap_or_else(|| panic!("registered workspace ID in {listed}"))
            .to_string()
    }
    fn set_base_branch(&self, cwd: &Path, value: &str, fresh: bool) {
        let mut args = vec!["config", "set"];
        if fresh {
            args.push("--fresh");
        }
        args.extend(["workflow.base_branch", value]);
        self.success(cwd, &args);
    }
    fn set_base_branch_selected(&self, cwd: &Path, id: &str, value: &str) {
        self.success(
            cwd,
            &[
                "--workspace",
                id,
                "config",
                "set",
                "workflow.base_branch",
                value,
            ],
        );
    }
    /// Every local-write verb must refuse with one of `needles` in stderr.
    fn assert_local_writes_refused(
        &self,
        cwd: &Path,
        needles: &[&str],
        case: &str,
        description: &str,
        reason: &str,
    ) {
        for args in local_write_commands(description, reason) {
            let mut command = self.command(cwd, &args);
            if args[1] == "reset" {
                command.env("ORBIT_OPERATOR", "1");
            }
            let refused = command.output().expect("local write lookup");
            assert!(
                !refused.status.success(),
                "{case}: write must refuse: {args:?}"
            );
            let message = stderr_of(&refused);
            assert!(
                needles.iter().any(|needle| message.contains(needle)),
                "{case}: local lookup must refuse {args:?} with {needles:?}: {message}"
            );
        }
    }
}

/// Seeded main checkout plus an initialized linked worktree:
/// `(fixture, main_repo, linked, main_orbit, linked_orbit)`.
fn linked_scenario(name: &str, branch: &str) -> (Fixture, PathBuf, PathBuf, PathBuf, PathBuf) {
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.clone();
    let linked = fixture.init_worktree(name, Some(branch));
    let main_orbit = main_repo.join(".orbit");
    let linked_orbit = linked.join(".orbit");
    (fixture, main_repo, linked, main_orbit, linked_orbit)
}

fn arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn mkdir(path: &Path) {
    fs::create_dir_all(path).expect("create directory");
}

fn write(path: &Path, contents: impl AsRef<[u8]>) {
    fs::write(path, contents).expect("write file");
}

fn read_str(path: &Path) -> String {
    fs::read_to_string(path).expect("read file")
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn contains(haystack: &str, needle: &str, case: &str) {
    assert!(haystack.contains(needle), "{case} ({needle}): {haystack}");
}

fn not_exists(path: &Path, case: &str) {
    assert!(!path.exists(), "{case}: {}", path.display());
}

fn write_settings(orbit: &Path, priority: &str) {
    write(
        &orbit.join("auto_tasks/.orbit-auto-task-settings.json"),
        format!(
            "{{\"schemaVersion\":1,\"definitions\":{{\"candidate-probe\":{{\"priority\":\"{priority}\"}}}}}}"
        ),
    );
}

fn assert_config(fixture: &Fixture, cwd: &Path, shared: &Path, local: &Path) {
    assert_root_fields(&fixture.config_show(cwd), shared, local);
}

/// A local entry must shadow any shared definition: show refuses, list hides it.
fn assert_shadowed(fixture: &Fixture, cwd: &Path, name: &str, case: &str) {
    let shown = fixture.output(cwd, &["auto-task", "show", name, "--json"]);
    assert!(!shown.status.success(), "{case} must refuse");
    assert_auto_task_hidden(&fixture.auto_tasks(cwd, false), name, case);
}

fn assert_selected(
    fixture: &Fixture,
    case: &str,
    cwd: &Path,
    selector: &str,
    expected: &str,
    root: Option<&Path>,
) {
    let shown = fixture.selected_auto_task(cwd, selector);
    assert_eq!(shown["description"], expected, "{case}");
    if let Some(root) = root {
        assert_definition_source(&shown, root, &root.join("auto_tasks/candidate-probe.yaml"));
    }
    if case == "logical in worktree" {
        assert_eq!(shown["layering"]["settings"]["priority"], "low", "{case}");
    }
}

fn refusal_code(output: &std::process::Output, case: &str) -> Value {
    assert!(!output.status.success(), "{case} must refuse");
    let refusal: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|_| panic!("{case}: typed JSON refusal: {}", stderr_of(output)));
    assert_eq!(refusal["code"], "invalid_input", "{case}");
    refusal
}

fn tool_run_explicit_workspace_resolves_from_home_and_temp_directory() {
    let fixture = Fixture::seeded("repo");
    let home = fixture.home.as_path();
    let repo = fixture.repo.as_path();
    mkdir(&home.join(".orbit"));
    write(
        &home.join(".orbit/config.toml"),
        "[machine]\nid = \"hm_cli_tool\"\nname = \"cli-tool\"\ntask_prefix = \"ORB\"\n",
    );
    fixture.init(repo);
    let workspace_id = fixture.first_workspace_id(repo);
    let temp_dir = std::env::temp_dir();
    for cwd in [home, temp_dir.as_path()] {
        for (case, input, flagged) in [
            (
                "list",
                serde_json::json!({"workspace": workspace_id, "limit": 1, "model": "codex"}),
                false,
            ),
            (
                "qualified list",
                serde_json::json!({"workspace": format!("hm_cli_tool/{workspace_id}"), "limit": 1, "model": "codex"}),
                false,
            ),
            (
                "flagged list",
                serde_json::json!({"limit":1,"model":"codex"}),
                true,
            ),
        ] {
            let input = input.to_string();
            let mut args = vec!["tool", "run", "orbit.task.list", "--input", &input];
            if flagged {
                args = vec![
                    "--workspace",
                    &workspace_id,
                    "tool",
                    "run",
                    "orbit.task.list",
                    "--input",
                    &input,
                ];
            }
            let listed = fixture.json(cwd, &args);
            assert!(
                listed["tasks"].is_array(),
                "{case} from {}: {listed}",
                cwd.display()
            );
        }
    }
    let foreign = serde_json::json!({"workspace": format!("hm_other/{workspace_id}"), "limit": 1, "model": "codex"});
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
    contains(
        &stderr_of(failure.get_output()),
        "hm_other",
        "foreign host must be named",
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
    for initialize_superproject in [false, true] {
        let fixture = Fixture::new("superproject");
        let home = fixture.home.as_path();
        let superproject = fixture.repo.as_path();
        let submodule = init_submodule_fixture(superproject);
        let nested = submodule.join("nested");
        mkdir(&nested);
        if initialize_superproject {
            fixture.init(superproject);
        }
        let run = |args: &[&str]| {
            let mut command = fixture.command(&nested, args);
            if without_git {
                command.env("PATH", home.join("stub-bin"));
            }
            command
        };
        run(&["workspace", "init"]).assert().success();
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
        let output = run(&["config", "show", "--json"]).assert().success();
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
    fs::rename(submodule.join(".orbit"), &data_dir).expect("relocate state");
    let registry_path = home.join(".orbit/workspaces.json");
    let mut registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("registry")).expect("registry");
    let checkout = registry["checkouts"]
        .as_array_mut()
        .expect("checkouts")
        .iter_mut()
        .find(|checkout| checkout["repo_root"].as_str() == submodule.to_str())
        .expect("submodule checkout");
    checkout["orbit_dir"] = serde_json::json!(data_dir);
    write(
        &registry_path,
        serde_json::to_vec(&registry).expect("registry JSON"),
    );
    fixture.success(&submodule, &["workspace", "init", "--force"]);
    assert_root_fields(&fixture.config_show(&submodule), &data_dir, &data_dir);
    not_exists(
        &submodule.join(".orbit"),
        "registered data directory must remain authoritative",
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
    mkdir(&nested);
    let show_stubbed = || {
        let mut command = fixture.command(&nested, &["config", "show", "--json"]);
        command.env("PATH", home.join("stub-bin"));
        let output = command.assert().success();
        serde_json::from_slice(&output.get_output().stdout).expect("config")
    };
    let absolute_gitfile = read_str(&linked.join(".git"));
    for gitfile in [
        absolute_gitfile.as_str(),
        "gitdir: ../repo/.git/worktrees/linked\n",
    ] {
        write(&linked.join(".git"), gitfile);
        assert_root_fields(&show_stubbed(), &main_orbit, &local_orbit);
        not_exists(&local_orbit, "fallback must not create local state");
    }
    mkdir(&local_orbit);
    write(&local_orbit.join("config.toml"), "");
    let commondir = main_repo.join(".git/worktrees/linked/commondir");
    mkdir(&main_repo.join("other.git"));
    for invalid in [None, Some(""), Some("../../../other.git")] {
        match invalid {
            None => fs::remove_file(&commondir).expect("missing commondir"),
            Some(value) => write(&commondir, value),
        }
        assert_root_fields(&show_stubbed(), &local_orbit, &local_orbit);
    }
}

fn init_submodule_fixture(superproject: &Path) -> PathBuf {
    let source = superproject.parent().expect("fixture root").join("source");
    mkdir(&source);
    mkdir(superproject);
    Fixture::seed_repo(&source);
    Fixture::seed_repo(superproject);
    let src = arg(&source);
    crate::git_repo::run(
        superproject,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &src,
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
        fs::remove_file(&lock).expect("remove fixture lock");
    }
    let listed = fixture.workspace_list(repo);
    assert!(listed.as_array().is_some_and(|rows| !rows.is_empty()));
    not_exists(&lock, "unchanged validation must not create a lock");
    let _restore_global = RestorePermissions::set_mode(&global, 0o555);
    assert_eq!(fixture.workspace_list(repo), listed);
    not_exists(&lock, "read-only list must not create a lock");
    let moved_repo = fixture.temp.path().join("moved-repo");
    fs::rename(repo, &moved_repo).expect("make registered checkout missing");
    let unrelated = fixture.temp.path().join("unrelated");
    crate::git_repo::seal_lookup_boundary(&unrelated);
    let registry = global.join("workspaces.json");
    let before = fs::read(&registry).expect("registry before failure");
    let mut command = cargo_bin_cmd!("orbit");
    command
        .current_dir(&unrelated)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["workspace", "list", "--format", "json"]);
    clear_inherited_authority_env(&mut command);
    set_orbit_root_env(&mut command, None);
    let failed = command.assert().failure();
    let stderr = stderr_of(failed.get_output());
    assert!(
        stderr.contains("Read-only file system") || stderr.contains("Permission denied"),
        "validation write must fail closed: {stderr}"
    );
    assert_eq!(
        fs::read(&registry).expect("registry after failure"),
        before,
        "failed validation must not drop or partially persist the edit"
    );
    not_exists(&lock, "failed validation must not leave a lock file");
    drop(_restore_global);
    write(&lock, b"");
    let _restore_global = RestorePermissions::set_mode(&global, 0o555);
    let selector = listed[0]["id"].as_str().expect("registered workspace id");
    let failed = fixture
        .command(&unrelated, &[])
        .args(["--workspace", selector, "workspace", "show", "--json"])
        .assert()
        .failure();
    contains(
        &stderr_of(failed.get_output()),
        "is invalid on this machine",
        "selector must use validated status despite failed save",
    );
    assert_eq!(
        fs::read(&registry).expect("registry after best-effort save"),
        before,
        "best-effort selector validation must leave failed status writes unchanged"
    );
}

fn auto_task_show_keeps_alternate_extension_candidates_local() {
    let (fixture, _main_repo, linked, main_orbit, _linked_orbit) =
        linked_scenario("candidate", "candidate");
    let shared = fs::canonicalize(main_orbit).unwrap();
    let local = fs::canonicalize(&linked).unwrap().join(".orbit");
    write_auto_task_fixture(&shared, "Shared enabled body");
    let shared_path = shared.join("auto_tasks/candidate-probe.yaml");
    let shared_bytes = read_str(&shared_path).replace("enabled: false", "enabled: true");
    write(&shared_path, &shared_bytes);
    for extension in ["yml", "YML", "Yml"] {
        let case = format!("extension {extension}");
        write_auto_task_fixture(&local, "Local disabled body");
        let canonical = local.join("auto_tasks/candidate-probe.yaml");
        let alternate = canonical.with_extension(extension);
        fs::rename(&canonical, &alternate).unwrap();
        write_settings(&local, "high");
        let shown = fixture.auto_task(&linked, "candidate-probe");
        assert_eq!(shown["description"], "Local disabled body", "{case}");
        assert_eq!(shown["enabled"], false, "{case}");
        assert_definition_source(&shown, &local, &alternate);
        assert_eq!(shown["layering"]["settings"]["priority"], "high", "{case}");
        let listed = fixture.auto_tasks(&linked, false);
        let rows: Vec<_> = json_rows(&listed)
            .iter()
            .filter(|row| row["name"] == "candidate-probe")
            .collect();
        assert_eq!(rows.len(), 1, "{case}");
        assert_eq!(rows[0]["description"], shown["description"], "{case}");
        assert_eq!(rows[0]["enabled"], false, "{case}");
        fixture.assert_local_writes_refused(
            &linked,
            &[
                "no such auto-task",
                "must be stored at its canonical .yaml path",
            ],
            &case,
            "forbidden",
            "probe",
        );
        let body = read_str(&alternate);
        write(
            &alternate,
            format!("# provenance: plugin:absent-probe@1.0.0\n{body}"),
        );
        assert_inactive_plugin(&fixture, &linked, "candidate-probe", &alternate);
        write_auto_task_fixture(&local, "Ambiguous canonical body");
        let duplicate =
            fixture.output(&linked, &["auto-task", "show", "candidate-probe", "--json"]);
        refusal_code(&duplicate, &format!("{case} duplicate"));
        fs::remove_file(&canonical).unwrap();
        write(&alternate, "name: [malformed");
        let malformed =
            fixture.output(&linked, &["auto-task", "show", "candidate-probe", "--json"]);
        assert!(!malformed.status.success(), "{case} malformed must shadow");
        refusal_code(&malformed, &format!("{case} malformed"));
        assert_auto_task_hidden(
            &fixture.auto_tasks(&linked, false),
            "candidate-probe",
            "list must not reveal the shared body either",
        );
        assert_eq!(read_str(&shared_path), shared_bytes, "{case}");
        not_exists(&canonical, "refused writes must not create canonical");
        fs::remove_file(&alternate).unwrap();
    }
}

fn auto_task_show_reports_and_honors_the_selected_definition_source() {
    let f = Fixture::seeded("repo");
    let repo = f.repo.as_path();
    let linked = f.init_worktree("repo-candidate", Some("orbit-auto-task-candidate"));
    let mo = repo.join(".orbit");
    let lo = linked.join(".orbit");
    write_auto_task_fixture(&mo, "Primary checkout definition");
    mkdir(&lo.join("auto_tasks"));
    let primary_path = mo.join("auto_tasks/candidate-probe.yaml");
    let primary_bytes = fs::read(&primary_path).expect("primary definition");
    write_settings(&mo, "high");
    let shown = f.auto_task(&linked, "candidate-probe");
    assert_eq!(shown["description"], "Primary checkout definition");
    assert_eq!(shown["template"]["priority"], "high");
    assert_eq!(shown["layering"]["settings"]["priority"], "high");
    assert_eq!(shown["enabled"], false, "disabled remains inspectable");
    assert_definition_source(&shown, &mo, &primary_path);
    for args in [
        vec!["auto-task", "list", "--format", "json"],
        vec!["tool", "run", "orbit.auto_task.list", "--input", "{}"],
    ] {
        let listed = f.json(&linked, &args);
        assert!(
            json_rows(&listed).iter().any(|row| {
                row["name"] == "candidate-probe"
                    && row["description"] == "Primary checkout definition"
            }),
            "workspace definition missing from {listed}"
        );
    }
    let missing = f.output(&linked, &["auto-task", "show", "absent-probe", "--json"]);
    let refusal = refusal_code(&missing, "missing auto-task");
    let error = refusal["error"].as_str().expect("refusal message");
    contains(error, &arg(&mo), "searched workspace root");
    contains(error, &arg(&lo), "searched candidate root");
    contains(error, "orbit auto-task list", "scope discovery command");
    f.assert_local_writes_refused(
        &linked,
        &["no such auto-task"],
        "shared inspection must not widen write scope",
        "must stay local",
        "lookup probe",
    );
    assert_eq!(fs::read(&primary_path).expect("primary"), primary_bytes);
    assert!(
        fs::read_dir(lo.join("auto_tasks"))
            .expect("candidate catalog")
            .next()
            .is_none(),
        "reads and refused mint must not create local definitions"
    );
    write_auto_task_fixture(&lo, "Linked candidate definition");
    write_settings(&lo, "low");
    write(
        &mo.join("auto_tasks/workspace-only.yaml"),
        String::from_utf8(primary_bytes.clone())
            .expect("definition text")
            .replace("candidate-probe", "workspace-only"),
    );
    let union = f.auto_tasks(&linked, false);
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
    let workspace_id = f.first_workspace_id(repo);
    let wid = workspace_id.as_str();
    let linked_selector = arg(&linked);
    assert_selected(
        &f,
        "logical in worktree",
        &linked,
        wid,
        "Linked candidate definition",
        Some(&lo),
    );
    assert_selected(
        &f,
        "logical in primary",
        repo,
        wid,
        "Primary checkout definition",
        Some(&mo),
    );
    assert_selected(
        &f,
        "explicit linked",
        repo,
        linked_selector.as_str(),
        "Linked candidate definition",
        Some(&lo),
    );
    not_exists(
        &lo.join("tasks"),
        "candidate inspection must not create store",
    );
    f.success(
        repo,
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
    assert_selected(
        &f,
        "primary after update",
        repo,
        wid,
        "Primary definition updated through authoritative routing",
        None,
    );
    assert_selected(
        &f,
        "logical from linked keeps YAML",
        &linked,
        wid,
        "Linked candidate definition",
        None,
    );
    assert_selected(
        &f,
        "explicit linked not retargeted",
        repo,
        linked_selector.as_str(),
        "Linked candidate definition",
        None,
    );
    not_exists(&lo.join("tasks"), "writable routing must not create store");
    write(&lo.join("auto_tasks/candidate-probe.yaml"), "invalid: [");
    assert_shadowed(
        &f,
        &linked,
        "candidate-probe",
        "broken local must shadow shared",
    );
    let plugin_body = String::from_utf8(primary_bytes)
        .expect("definition text")
        .replace("candidate-probe", "plugin-probe");
    write(
        &mo.join("auto_tasks/plugin-probe.yaml"),
        format!("# provenance: plugin:absent-probe@1.0.0\n{plugin_body}"),
    );
    assert_inactive_plugin(&f, &linked, "plugin-probe", &mo);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            mo.join("auto_tasks/workspace-only.yaml"),
            lo.join("auto_tasks/workspace-only.yaml"),
        )
        .expect("symlink candidate");
        assert_shadowed(
            &f,
            &linked,
            "workspace-only",
            "symlinked local must not expose shared",
        );
    }
    write(&mo.join("auto_tasks/shared-fault.yaml"), "invalid: [");
    let doctor = f.output(&linked, &["doctor", "--json"]);
    let checks: Vec<Value> = serde_json::from_slice(&doctor.stdout).expect("doctor rows");
    let auto_tasks = checks
        .iter()
        .find(|row| row["check"] == "artifacts-auto-tasks")
        .expect("auto-task health");
    contains(
        auto_tasks["remediation"].as_str().expect("remediation"),
        &arg(&mo.join("auto_tasks/shared-fault.yaml")),
        "worktree diagnostics must inspect shared catalog",
    );
    let outside_checkout = arg(f.temp.path());
    for rejected in ["no-such-workspace", outside_checkout.as_str()] {
        let mut command = f.command(
            repo,
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
        contains(
            &stderr_of(assert.get_output()),
            "unknown workspace selector",
            &format!("rejected selector {rejected}"),
        );
    }
}

fn auto_task_crud_from_linked_worktree_writes_definition_yaml_to_local_root() {
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();
    let elsewhere = fixture.temp.path().join("elsewhere");
    mkdir(&elsewhere);
    let linked_worktree =
        fixture.init_worktree("repo-jrun", Some("orbit-auto-task-worktree-write"));
    let main_orbit = main_repo.join(".orbit");
    let linked_orbit = linked_worktree.join(".orbit");
    let workspace_id = fixture.first_workspace_id(main_repo);
    let workspace_name = fixture.first_workspace_name(main_repo);
    let added = fixture.auto_task_add(
        &linked_worktree,
        &workspace_id,
        "worktree-crud",
        "Worktree CRUD",
        Some("Written from the linked worktree"),
    );
    assert_eq!(added["name"], "worktree-crud");
    let worktree_yaml = linked_orbit.join("auto_tasks/worktree-crud.yaml");
    let primary_yaml = main_orbit.join("auto_tasks/worktree-crud.yaml");
    assert!(worktree_yaml.is_file(), "add must write worktree YAML");
    not_exists(&primary_yaml, "add must not write primary checkout");
    not_exists(&linked_orbit.join("tasks"), "must not invent task store");
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
    let yaml = read_str(&worktree_yaml);
    contains(&yaml, "Updated in the worktree", "update rewrites YAML");
    contains(&yaml, "enabled: false", "toggle rewrites YAML");
    not_exists(&primary_yaml, "update/toggle must not create primary YAML");
    let from_elsewhere = fixture.auto_task_add(
        &elsewhere,
        &workspace_id,
        "primary-from-elsewhere",
        "Primary from elsewhere",
        None,
    );
    assert_eq!(from_elsewhere["name"], "primary-from-elsewhere");
    assert!(
        main_orbit
            .join("auto_tasks/primary-from-elsewhere.yaml")
            .is_file(),
        "logical selector from non-linked cwd writes the primary"
    );
    not_exists(
        &linked_orbit.join("auto_tasks/primary-from-elsewhere.yaml"),
        "elsewhere add must not write linked YAML",
    );
    let worktree_root = arg(&linked_orbit);
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
    let stderr = stderr_of(assert.get_output());
    assert!(
        stderr.contains("not an Orbit workspace")
            || stderr.contains("unknown workspace selector")
            || stderr.contains("workspaces.json"),
        "--root at worktree .orbit must be refused as shadow: {stderr}"
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
    serde_json::from_slice::<Value>(&managed_assert.get_output().stdout).expect("managed add");
    assert!(
        linked_orbit
            .join("auto_tasks/managed-envelope.yaml")
            .is_file(),
        "ORBIT_WORKSPACE from linked worktree must write worktree YAML"
    );
    not_exists(
        &main_orbit.join("auto_tasks/managed-envelope.yaml"),
        "managed envelope must not write primary",
    );
    not_exists(
        &linked_orbit.join("tasks"),
        "managed envelope must not invent task store",
    );
}

/// The real detached worker must bootstrap, claim, and finish its run while an
/// unrelated WAL writer holds the already-current task registry. The parent
/// keeps supervising the child; no fixture claims the run on its behalf.
fn detached_worker_bootstraps_and_claims_while_registry_writer_is_held() {
    let fixture = Fixture::seeded("repo");
    let repo = fixture.repo.as_path();
    let custom_root = fixture.temp.path().join("custom-orbit");
    let root_arg = arg(&custom_root);
    fixture.success(repo, &["--root", &root_arg, "workspace", "init"]);
    pin_default_crew_for_isolated_root(&custom_root);
    let registry_path = custom_root.join("tasks").join("index.sqlite");
    assert!(registry_path.is_file(), "init must create task registry");
    let registry_writer = rusqlite::Connection::open_with_flags(
        &registry_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .expect("open task registry writer");
    registry_writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold registry WAL writer");
    let job_path = repo.join("root-override-smoke.yaml");
    write(
        &job_path,
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: root_override_smoke\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 0\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
    );
    let job_arg = arg(&job_path);
    let submitted = fixture.json(
        repo,
        &["--root", &root_arg, "run", "job", &job_arg, "--json"],
    );
    let run_id = submitted["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("expected run_id in {submitted}"))
        .to_string();
    assert_eq!(submitted["state"].as_str(), Some("submitted"));
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
        mkdir(marker.parent().expect("graph parent"));
        write(marker, b"retired");
    }
    for (case, args, exists) in [
        ("ordinary", vec!["doctor", "--json"], true),
        ("cleaned", vec!["doctor", "--remove-graph", "--json"], false),
    ] {
        let shown = fixture.json(&linked_worktree, &args);
        assert_eq!(local_marker.exists(), exists, "{case} local marker");
        assert_eq!(shared_marker.exists(), exists, "{case} shared marker");
        assert!(
            json_rows(&shown)
                .iter()
                .all(|row| row["check"] != "graph-index"),
            "{case}: no graph-index row"
        );
    }
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
        let title = format!("{name} task");
        fixture.success(
            &repo,
            &[
                "task",
                "add",
                "--title",
                &title,
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
    mkdir(&partitions.join("ws_residue"));
    mkdir(&partitions.join("ws_unowned/ORB-00077"));
    fs::remove_dir_all(deleted_volume.join("gone")).expect("delete checkout");
    write(&deleted_volume.join("sibling"), b"still mounted");
    fs::remove_dir_all(&file_volume).expect("remove volume");
    write(&file_volume, b"not a directory");
    fs::remove_dir_all(empty_mount.join("proj")).expect("empty mount");
    fs::remove_dir_all(&removed_mount).expect("remove mount");
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
    let live = fixture.temp.path().join("live");
    let refused = fixture.output(&live, &["doctor", "--fix-orphan-task-stores", "--json"]);
    assert!(
        !refused.status.success(),
        "unconfirmed repair must refuse: {}",
        stdout_of(&refused)
    );
    assert_eq!(
        task_store_partitions(&partitions),
        before,
        "unconfirmed repair must not delete anything"
    );
    let repaired = fixture.json(
        &live,
        &["doctor", "--fix-orphan-task-stores", "--confirm", "--json"],
    );
    assert!(
        json_rows(&repaired)
            .iter()
            .any(|row| row["check"] == "fix-orphan-task-stores" && row["status"] == "ok"),
        "confirmed repair reports outcome: {repaired}"
    );
    let mut expected = before;
    expected.remove(&partition_of["gone"]);
    expected.remove("ws_residue");
    assert_eq!(
        task_store_partitions(&partitions),
        expected,
        "only empty and confirmed-absent partitions may go"
    );
    for name in unreachable_names {
        assert!(
            orbit_cmd::task_store::partition_is_bound(&global_root, &partition_of[name])
                .expect("check retained binding"),
            "ORB-14516: repair must retain {name}'s binding"
        );
    }
    for mount in [&empty_mount, &removed_mount] {
        mkdir(&mount.join("proj"));
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
    mkdir(&outside);
    let outside_marker = outside.join("keep.db");
    write(&outside_marker, b"keep");
    let external_graph = outside.join("graph");
    mkdir(&external_graph);
    let external_graph_marker = external_graph.join("keep.db");
    write(&external_graph_marker, b"keep");
    let knowledge_link = orbit_dir.join("knowledge");
    if knowledge_link.exists() {
        fs::remove_dir_all(&knowledge_link).expect("clear knowledge");
    }
    std::os::unix::fs::symlink(&outside, &knowledge_link).expect("link knowledge");
    let refused = fixture.output(repo, &["doctor", "--remove-graph", "--json"]);
    assert!(
        !refused.status.success(),
        "symlinked ancestor refused: {}",
        stdout_of(&refused)
    );
    assert!(
        external_graph_marker.is_file(),
        "cleanup must not delete through symlinked ancestor"
    );
    assert!(outside_marker.is_file(), "outside marker must remain");
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
    let fixture = Fixture::seeded("main");
    let base = fs::canonicalize(fixture.temp.path()).expect("canonical tempdir");
    let home = fixture.home.as_path();
    let main_repo = fixture.repo.as_path();
    let other_repo = base.join("other");
    let linked = base.join("main-linked");
    let outside = base.join("outside");
    let uninitialized = base.join("uninitialized");
    for dir in [home, main_repo, &other_repo, &outside, &uninitialized] {
        mkdir(dir);
    }
    Fixture::seed_repo(&other_repo);
    fixture.linked_worktree(&linked, Some("root-precedence"));
    fixture.success(main_repo, &["workspace", "init", "--name", "main"]);
    fixture.success(&other_repo, &["workspace", "init", "--name", "other"]);
    let subdirectory = main_repo.join("src/nested");
    mkdir(&subdirectory);
    let main_orbit = main_repo.join(".orbit");
    let other_orbit = other_repo.join(".orbit");
    let linked_orbit = linked.join(".orbit");
    let (m, o) = (&main_orbit, &other_orbit);
    let case =
        |name, cwd, root, env, ws, exp| assert_root_case(&fixture, name, cwd, root, env, ws, exp);
    case("from main", main_repo, None, None, None, Some((m, m)));
    case(
        "--root beats env+linked",
        &linked,
        Some(m),
        Some(o),
        None,
        Some((m, m)),
    );
    case(
        "ORBIT_ROOT beats linked",
        &linked,
        None,
        Some(o),
        None,
        Some((o, o)),
    );
    case(
        "linked shares main",
        &linked,
        None,
        None,
        None,
        Some((m, &linked_orbit)),
    );
    case(
        "subdirectory walks up",
        &subdirectory,
        None,
        None,
        None,
        Some((m, m)),
    );
    case(
        "--workspace from outside",
        &outside,
        None,
        None,
        Some("other"),
        Some((o, o)),
    );
    case(
        "--root must be initialized",
        main_repo,
        Some(&uninitialized),
        None,
        None,
        None,
    );
    case(
        "ORBIT_ROOT must be init",
        main_repo,
        None,
        Some(&uninitialized),
        None,
        None,
    );
    not_exists(
        &linked_orbit,
        "resolution must not materialize linked .orbit",
    );
}

/// Verify config reports the shared store and checkout-local roots without
/// the retired root aliases.
fn assert_root_case(
    fixture: &Fixture,
    name: &str,
    cwd: &Path,
    root_flag: Option<&Path>,
    env_root: Option<&Path>,
    workspace: Option<&str>,
    expected: Option<(&Path, &Path)>,
) {
    let mut args: Vec<String> = Vec::new();
    if let Some(root) = root_flag {
        args.extend(["--root".to_string(), arg(root)]);
    }
    if let Some(workspace) = workspace {
        args.extend(["--workspace".to_string(), workspace.to_string()]);
    }
    args.extend(["config", "show", "--json"].map(String::from));
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = fixture.output_with_root(cwd, &args, env_root);
    match expected {
        Some((shared, local)) => {
            assert!(output.status.success(), "{name}: {}", stderr_of(&output));
            let value: Value = serde_json::from_slice(&output.stdout).expect("config");
            assert_root_fields(&value, shared, local);
            assert_eq!(
                (
                    string_field(&value, "shared_root"),
                    string_field(&value, "local_root")
                ),
                (arg(shared).as_str(), arg(local).as_str()),
                "{name}"
            );
        }
        None => assert!(!output.status.success(), "{name}: {}", stdout_of(&output)),
    }
}

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
    write(
        &config_path,
        "[workflow]\ndefault_crew = \"codex\"\n\n[crews.codex]\nprovider = \"codex\"\nmodel = \"gpt-5.4\"\nbackend = \"cli\"\n",
    );
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
    mkdir(&home.join(".orbit"));
    write(
        &home.join(".orbit/config.toml"),
        "[machine]\nid = \"hm_worktree_resolution\"\nname = \"worktree-resolution\"\ntask_prefix = \"ORB\"\n",
    );
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
    mkdir(path.parent().expect("lock parent"));
    let holder =
        serde_json::json!({"pid": pid, "acquired_at": "2026-10-03T00:00:00Z", "label": label});
    write(path, holder.to_string());
}

/// Prevent ambient managed-run authority from changing child command
/// behavior — or, worse, routing its writes (ORB-11300). `orbit_command`
/// applies this before `set_orbit_root_env`, so an explicit fixture root still
/// wins.
fn clear_inherited_authority_env(command: &mut AssertCommand) {
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
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

/// Executable no-op in `<home>/stub-bin` so `workspace init` crew seeding
/// finds an agent CLI on every host (see `orbit_command`).
fn plant_agent_cli_stub(home: &Path, name: &str) -> PathBuf {
    let bin = home.join("stub-bin");
    mkdir(&bin);
    let stub = bin.join(name);
    write(&stub, "#!/bin/sh\nexit 0\n");
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
    contains(
        plugin["skipped_reason"].as_str().expect("inactive reason"),
        &arg(source),
        "inactive reason must name supplying catalog",
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
    assert_eq!(shown["definition_source"]["root"], arg(root).as_str());
    assert_eq!(shown["definition_source"]["path"], arg(path).as_str());
}

fn write_auto_task_fixture(orbit_dir: &Path, description: &str) {
    let directory = orbit_dir.join("auto_tasks");
    mkdir(&directory);
    write(
        &directory.join("candidate-probe.yaml"),
        format!(
            "schemaVersion: 1\nname: candidate-probe\ndescription: {description}\nenabled: false\nschedule:\n  cron: 0 * * * *\ntemplate:\n  title: Inspect candidate definition\n  description: Exercise definition-source selection.\n  acceptance_criteria:\n  - The selected definition is reported.\n  task_type: chore\n  tags:\n  - candidate-probe\n  priority: low\n  complexity: low\n  status: backlog\ndedupe: skip_if_open\ncreated_by: system\ncreated_at: 2026-09-14T00:00:00Z\nupdated_by: system\nupdated_at: 2026-09-14T00:00:00Z\n"
        ),
    );
}

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
    mkdir(&data_dir);
    let data_dir_arg = arg(&data_dir);
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
    write(
        &data_dir.join("config.yaml"),
        "schema_version: 1\nworkspace_id: tmp-5b7149\n",
    );
}

fn canonicalize_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn registered_workspace_id(fixture: &Fixture, repo: &Path) -> String {
    let listed = fixture.workspace_list(repo);
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
    let stderr = stderr_of(output);
    assert!(
        !output.status.success(),
        "indirect workspace use must be refused: {stderr}"
    );
    let caller = cwd.join(".orbit");
    for (label, needle) in [
        ("resolved workspace", arg(live_root)),
        ("caller directory", arg(cwd)),
        ("caller .orbit", arg(&caller)),
        ("--root escape hatch", "--root".to_string()),
    ] {
        contains(&stderr, &needle, &format!("refusal must name {label}"));
    }
}

/// An empty `.orbit/` under a linked worktree must not bootstrap or rewrite
/// the main checkout. A nested directory that already has `config.toml`
/// resolves to itself, and ordinary reads keep the shared main root.
#[test]
fn empty_nested_orbit_in_linked_worktree_does_not_rewrite_main_workspace() {
    isolated!(
        "worktree_resolution::empty_nested_orbit_in_linked_worktree_does_not_rewrite_main_workspace"
    );
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();
    let linked = fixture.init_worktree("repo-worktree", Some("orbit-empty-nested-orbit"));
    let main_orbit = main_repo.join(".orbit");
    let main_config = main_orbit.join("config.toml");
    fixture.set_base_branch(main_repo, "agent-main", true);
    let stamp = config_stamp(&main_config);
    let unchanged = |label: &str| assert_config_unchanged(&main_config, &stamp, label);
    let work = linked.join("scratch").join("work");
    mkdir(&work.join(".orbit"));
    let init_force = fixture.output(&work, &["workspace", "init", "--force"]);
    assert_indirect_workspace_refusal(&init_force, &main_orbit, &work);
    unchanged("workspace init --force");
    not_exists(
        &work.join(".orbit/config.toml"),
        "refused init must not initialize empty fixture",
    );
    let config_set = fixture.output(&work, &["config", "set", "workflow.base_branch", "main"]);
    assert_indirect_workspace_refusal(&config_set, &main_orbit, &work);
    unchanged("config set");
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
    assert!(
        orbit_init.status.success(),
        "orbit init targets global root: {}",
        stderr_of(&orbit_init)
    );
    unchanged("orbit init");
    not_exists(
        &work.join(".orbit/config.toml"),
        "orbit init must not write empty fixture config",
    );
    assert_config(&fixture, &work, &main_orbit, &linked.join(".orbit"));
    unchanged("config show from empty fixture");
    let with_config = linked.join("scratch").join("with-config");
    mkdir(&with_config);
    let with_orbit = with_config.join(".orbit");
    let with_orbit_arg = arg(&with_orbit);
    fixture.success(
        &with_config,
        &["--root", &with_orbit_arg, "workspace", "init"],
    );
    unchanged("explicit --root workspace init");
    assert_root_fields(&fixture.config_show(&with_config), &with_orbit, &with_orbit);
    unchanged("config show from initialized nested fixture");
    let from_env = fixture.json_with_root(&work, &["config", "show", "--json"], Some(&main_orbit));
    assert_root_fields(&from_env, &main_orbit, &main_orbit);
    unchanged("ORBIT_ROOT config show");
    let bare = fixture.temp.path().join("bare-orbit");
    mkdir(&bare);
    let uninitialized = fixture.output_with_root(&work, &["config", "show", "--json"], Some(&bare));
    let uninitialized_stderr = stderr_of(&uninitialized);
    assert!(
        !uninitialized.status.success(),
        "uninitialized explicit root must fail: {uninitialized_stderr}"
    );
    contains(
        &uninitialized_stderr,
        "not an Orbit workspace",
        "uninitialized explicit root refusal",
    );
    unchanged("uninitialized explicit root");
    let fresh = fixture.temp.path().join("fresh");
    Fixture::seed_repo(&fresh);
    fixture.success(&fresh, &["workspace", "init"]);
    assert!(
        fresh.join(".orbit/config.yaml").is_file(),
        "fresh project init must bootstrap its .orbit"
    );
    unchanged("fresh project workspace init");
    assert_root_fields(
        &fixture.config_show(&linked),
        &main_orbit,
        &linked.join(".orbit"),
    );
    unchanged("config show from linked worktree");
    mkdir(&fixture.home.join(".git"));
    let from_home = fixture.output(&fixture.home, &["workspace", "init"]);
    let home_stderr = stderr_of(&from_home);
    assert!(
        !from_home.status.success(),
        "global home must be refused: {home_stderr}"
    );
    contains(&home_stderr, "global Orbit root", "global home refusal");
    unchanged("global home workspace init");
}

/// An explicit `--workspace` selector names its target directly: a nested
/// empty `.orbit/` (or one holding a malformed config) in the caller's
/// working directory must not refuse the write, while an implicitly
/// selected write from the same directory stays refused.
#[test]
fn config_set_explicit_workspace_selector_bypasses_indirect_refusal() {
    isolated!(
        "worktree_resolution::config_set_explicit_workspace_selector_bypasses_indirect_refusal"
    );
    let fixture = Fixture::seeded("repo");
    let main_repo = fixture.repo.as_path();
    fixture.init(main_repo);
    fixture.set_base_branch(main_repo, "agent-main", true);
    let repo_b = canonicalize_or_original(&fixture.temp.path().join("repo-b"));
    Fixture::seed_repo(&repo_b);
    fixture.init(&repo_b);
    fixture.set_base_branch(&repo_b, "b-before", true);
    let id_a = registered_workspace_id(&fixture, main_repo);
    let id_b = registered_workspace_id(&fixture, &repo_b);
    let config_a = main_repo.join(".orbit/config.toml");
    let config_b = repo_b.join(".orbit/config.toml");
    let nested = main_repo.join("nested");
    mkdir(&nested.join(".orbit"));
    let stamp_a = config_stamp(&config_a);
    let stamp_b = config_stamp(&config_b);
    let implicit_refused = |label: &str| {
        let implicit = fixture.output(
            &nested,
            &["config", "set", "workflow.base_branch", "implicit"],
        );
        assert!(
            !implicit.status.success(),
            "{label}: implicit set must be refused: {}",
            stderr_of(&implicit)
        );
    };
    implicit_refused("empty nested .orbit");
    assert_config_unchanged(&config_a, &stamp_a, "implicit config set");
    assert_config_unchanged(&config_b, &stamp_b, "implicit config set");
    fixture.set_base_branch_selected(&nested, &id_a, "explicit-a");
    contains(
        &read_str(&config_a),
        "explicit-a",
        "explicit selector must update workspace A",
    );
    assert_config_unchanged(&config_b, &stamp_b, "explicit A config set");
    write(
        &nested.join(".orbit/config.toml"),
        "[workflow\nbase_branch = [",
    );
    let stamp_a = config_stamp(&config_a);
    fixture.set_base_branch_selected(&nested, &id_b, "explicit-b");
    contains(
        &read_str(&config_b),
        "explicit-b",
        "explicit selector must update workspace B",
    );
    assert_config_unchanged(&config_a, &stamp_a, "explicit B config set");
    implicit_refused("after explicit writes");
    assert!(
        !read_str(&config_b).contains("implicit"),
        "refused implicit write must not change workspace B"
    );
}

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
