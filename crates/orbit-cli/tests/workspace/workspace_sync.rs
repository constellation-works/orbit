#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Binary-level coverage for workspace managed-artifact convergence through
//! `workspace sync` and `workspace init`.

use std::path::{Path, PathBuf};

use assert_cmd::assert::OutputAssertExt;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

use crate::git_repo;

fn orbit(cwd: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_HOME");
    command.timeout(test_env::FIXTURE_STEP_DEADLINE);
    command
}

/// Run `command` to completion. A command killed at its
/// [`test_env::FIXTURE_STEP_DEADLINE`] guard fails naming `what` and the host
/// load, not as an interrupted failure of the command under test.
fn run(command: &mut assert_cmd::Command, what: &str) -> assert_cmd::assert::Assert {
    let started = std::time::Instant::now();
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run `{what}`: {error}"));
    test_env::assert_step_finished(what, started.elapsed(), &output.status);
    output.assert()
}

fn write_machine_identity(home: &Path) {
    let global = home.join(".orbit");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::write(
        global.join("config.toml"),
        "[machine]\nid = \"hm_workspace_sync\"\nname = \"sync-machine\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("write machine identity");
}

fn read(path: impl Into<PathBuf>) -> Vec<u8> {
    std::fs::read(path.into()).expect("read snapshot path")
}

fn read_optional(path: impl Into<PathBuf>) -> Option<Vec<u8>> {
    std::fs::read(path.into()).ok()
}

/// Upgrade actual previous-release bytes, rather than deriving a legacy fixture
/// from today's templates. Guards managed materialization provenance [DANI-10392].
#[test]
fn workspace_sync_upgrades_previous_release_automation_with_provenance_intact() {
    use orbit_common::protocol::yaml::{parse_auto_task_yaml, parse_routine_yaml};
    use orbit_common::security::release::sha256_hex;

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/automation-v0.24.0");
    for operator_edit in [false, true] {
        let home = tempdir().expect("isolated migration home");
        let repo = home.path().join("upgraded-workspace");
        git_repo::init(&repo);
        write_machine_identity(home.path());
        run(
            orbit(&repo, home.path()).args(["workspace", "init", "--name", "upgraded-workspace"]),
            "orbit workspace init",
        )
        .success();
        let root = repo.join(".orbit");
        for (directory, definition) in [
            ("routines", "task_pilot.yaml"),
            ("auto_tasks", "friction-curation.yaml"),
            ("auto_tasks", "delivery-qa.yaml"),
        ] {
            for name in [definition, ".orbit-managed-assets.json"] {
                std::fs::copy(
                    fixture.join(directory).join(name),
                    root.join(directory).join(name),
                )
                .unwrap();
            }
        }
        let routine = root.join("routines/task_pilot.yaml");
        let auto_task = root.join("auto_tasks/friction-curation.yaml");
        let retired_auto_task = root.join("auto_tasks/delivery-qa.yaml");
        let routine_manifest = root.join("routines/.orbit-managed-assets.json");
        let auto_manifest = root.join("auto_tasks/.orbit-managed-assets.json");
        let old_routine: Value = serde_json::from_slice(&read(&routine_manifest)).unwrap();
        let old_auto: Value = serde_json::from_slice(&read(&auto_manifest)).unwrap();
        assert_eq!(
            old_routine["assets"]["task_pilot"],
            sha256_hex(&read(&routine))
        );
        assert_eq!(
            old_auto["assets"]["friction-curation"],
            sha256_hex(&read(&auto_task))
        );
        if operator_edit {
            // Routine opt-in and an auto-task's handwritten schedule are
            // supported settings; both survive the body upgrade.
            let opted_in = std::fs::read_to_string(&routine)
                .unwrap()
                .replace("enabled: false", "enabled: true");
            std::fs::write(&routine, opted_in).unwrap();
            let edited = std::fs::read_to_string(&auto_task)
                .unwrap()
                .replace("cron: 15 7 * * *", "cron: 45 7 * * *");
            std::fs::write(&auto_task, edited).unwrap();
        }
        let auto_before = read(&auto_task);
        let retired_auto_before = read(&retired_auto_task);
        assert_eq!(
            old_auto["assets"]["delivery-qa"],
            sha256_hex(&retired_auto_before),
            "the historical fixture's delivery-qa digest must match its bytes"
        );
        let output = run(
            orbit(&repo, home.path()).args(["workspace", "sync", "--json"]),
            "orbit workspace sync",
        )
        .success()
        .get_output()
        .stdout
        .clone();
        let report: Value = serde_json::from_slice(&output).unwrap();
        let actions = report["actions"].as_array().unwrap();
        assert!(
            actions.iter().any(|a| a["kind"] == "routine"
                && a["name"] == "task_pilot"
                && (a["outcome"] == "refreshed" || (operator_edit && a["outcome"] == "migrated"))),
            "a previous template must converge, including an operator's opt-in: {report}"
        );
        let definition = parse_routine_yaml(&std::fs::read_to_string(&routine).unwrap()).unwrap();
        assert_eq!(
            definition.name,
            old_routine["routineProvenance"]["task_pilot"]["binding"]["name"]
                .as_str()
                .unwrap()
        );
        assert_eq!(
            definition.enabled, operator_edit,
            "migration preserves the operator's lifecycle choice"
        );
        let state = definition.trigger.state.unwrap();
        let binding = &old_routine["routineProvenance"]["task_pilot"]["binding"];
        assert_eq!(
            state.owner_machine,
            binding["ownerMachine"].as_str().unwrap()
        );
        assert_eq!(state.branch, binding["branch"].as_str().unwrap());
        let migrated: Value = serde_json::from_slice(&read(&routine_manifest)).unwrap();
        let provenance = &migrated["routineProvenance"]["task_pilot"];
        assert_eq!(
            provenance["binding"], *binding,
            "registered workspace renaming must preserve the recorded binding"
        );
        assert_ne!(
            provenance["templateDigest"],
            old_routine["routineProvenance"]["task_pilot"]["templateDigest"],
            "upgrade records the new template provenance"
        );
        assert_eq!(provenance["renderedDigest"], sha256_hex(&read(&routine)));
        assert_eq!(
            migrated["assets"]["task_pilot"],
            provenance["renderedDigest"]
        );

        let migrated_auto: Value = serde_json::from_slice(&read(&auto_manifest)).unwrap();
        let definition =
            parse_auto_task_yaml(&std::fs::read_to_string(&auto_task).unwrap()).unwrap();
        assert_eq!(definition.name, "friction-curation");
        if operator_edit {
            assert_ne!(
                read(&auto_task),
                auto_before,
                "a known shipped body upgrades even with a handwritten schedule"
            );
            assert_eq!(
                migrated_auto["assets"]["friction-curation"],
                sha256_hex(&read(&auto_task)),
                "migration records the current managed body's digest"
            );
            assert!(
                actions.iter().any(|a| a["kind"] == "auto_task"
                    && a["name"] == "friction-curation"
                    && a["outcome"] == "migrated"),
                "{report}"
            );
            let output = run(
                orbit(&repo, home.path()).args([
                    "auto-task",
                    "show",
                    "friction-curation",
                    "--json",
                ]),
                "orbit auto-task show",
            )
            .success()
            .get_output()
            .stdout
            .clone();
            let effective: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(effective["schedule"]["cron"], "45 7 * * *");
            assert_eq!(
                effective["layering"]["settings"]["schedule"]["cron"],
                "45 7 * * *"
            );
            assert_eq!(effective["layering"]["body"], "managed");
        } else {
            assert!(
                actions.iter().any(|a| a["kind"] == "auto_task"
                    && a["name"] == "friction-curation"
                    && a["outcome"] == "refreshed"),
                "{report}"
            );
            assert_ne!(
                read(&auto_task),
                auto_before,
                "an unchanged previous-release definition receives current material"
            );
            assert_eq!(
                migrated_auto["assets"]["friction-curation"],
                sha256_hex(&read(&auto_task))
            );
        }
        assert!(
            !retired_auto_task.exists(),
            "an unmodified previous-release delivery-qa definition is retired on refresh"
        );
        assert!(
            migrated_auto["assets"].get("delivery-qa").is_none(),
            "retirement drops delivery-qa provenance: {migrated_auto}"
        );
        assert!(
            actions.iter().any(|action| {
                action["kind"] == "auto_task"
                    && action["name"] == "delivery-qa"
                    && action["outcome"] == "retired"
            }),
            "{report}"
        );
        let paths = [&routine, &routine_manifest, &auto_task, &auto_manifest];
        let snapshot: Vec<_> = paths.iter().map(|p| read(*p)).collect();
        orbit(&repo, home.path())
            .args(["workspace", "sync", "--json"])
            .assert()
            .success();
        for (path, expected) in paths.iter().zip(snapshot) {
            assert_eq!(
                read(*path),
                expected,
                "repeat materialization is inert at {}",
                path.display()
            );
        }
        assert!(
            !retired_auto_task.exists(),
            "repeat sync must not reseed the retired delivery-qa definition"
        );
    }
}

/// `delivery-qa` is no longer a shipped default. Init leaves it absent, doctor
/// warns while a locally modified seeded copy is still in the catalog, and
/// refresh keeps those bytes outside the active catalog instead of deleting them.
#[test]
fn retired_delivery_qa_is_absent_after_init_and_a_modified_copy_is_kept() {
    use orbit_common::security::release::sha256_hex;

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/automation-v0.24.0");
    let home = tempdir().expect("home tempdir");
    let repo = home.path().join("workspace");
    git_repo::init(&repo);
    write_machine_identity(home.path());
    orbit(&repo, home.path())
        .args(["workspace", "init", "--name", "retired-delivery-qa"])
        .assert()
        .success();

    let auto_tasks = repo.join(".orbit/auto_tasks");
    let definition = auto_tasks.join("delivery-qa.yaml");
    assert!(
        !definition.exists(),
        "orbit workspace init must not seed delivery-qa"
    );
    let manifest_path = auto_tasks.join(".orbit-managed-assets.json");
    let mut manifest: Value =
        serde_json::from_slice(&read(&manifest_path)).expect("parse auto-task manifest");
    assert!(
        manifest["assets"].get("delivery-qa").is_none(),
        "a fresh manifest must not record delivery-qa: {manifest}"
    );

    let original = read(fixture.join("auto_tasks/delivery-qa.yaml"));
    let mut modified = original.clone();
    modified.extend(b"# operator edit\n");
    std::fs::write(&definition, &modified).expect("plant a modified retired default");
    manifest["assets"]["delivery-qa"] = Value::String(sha256_hex(&original));
    std::fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize manifest")
        ),
    )
    .expect("record the seeded digest");

    let doctor = orbit(&repo, home.path())
        .args(["doctor", "--json"])
        .output()
        .expect("run doctor");
    let rows: Value = serde_json::from_slice(&doctor.stdout).unwrap_or_else(|_| {
        panic!(
            "parse doctor JSON (status {:?}): {}",
            doctor.status,
            String::from_utf8_lossy(&doctor.stderr)
        )
    });
    let row = rows
        .as_array()
        .expect("doctor rows")
        .iter()
        .find(|row| row["check"] == "artifacts-auto-tasks")
        .expect("auto-task artifact row");
    assert_eq!(row["status"], "warning", "{row}");
    let message = row["message"].as_str().expect("doctor message");
    assert!(
        message.contains("delivery-qa") && message.contains("no longer ships"),
        "doctor must name the retirement: {row}"
    );
    assert_eq!(
        read(&definition),
        modified,
        "doctor leaves a modified retired copy in place"
    );

    let synced = orbit(&repo, home.path())
        .args(["workspace", "sync", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&synced).expect("parse sync JSON");
    assert!(
        report["actions"]
            .as_array()
            .expect("actions")
            .iter()
            .any(|action| {
                action["kind"] == "auto_task"
                    && action["name"] == "delivery-qa"
                    && action["outcome"] == "preserved"
                    && action["detail"].as_str().is_some_and(|detail| {
                        detail.contains("locally modified") && detail.contains("delivery-qa")
                    })
            }),
        "refresh must report that the modified copy was kept: {report}"
    );
    assert!(
        !definition.exists(),
        "refresh takes the modified copy out of the active catalog"
    );
    let preserved = repo.join(".orbit/.retired-managed/auto_tasks/delivery-qa.yaml");
    assert_eq!(
        read(&preserved),
        modified,
        "refresh keeps the operator's bytes instead of deleting them"
    );
    let migrated: Value = serde_json::from_slice(&read(&manifest_path)).expect("reread manifest");
    assert!(
        migrated["assets"].get("delivery-qa").is_none(),
        "the preserved copy is no longer managed provenance: {migrated}"
    );
}

#[test]
fn workspace_sync_creates_missing_defaults_preserves_operator_content_and_is_idempotent() {
    let home = tempdir().expect("home tempdir");
    let repo = home.path().join("workspace");
    git_repo::init(&repo);
    write_machine_identity(home.path());
    orbit(&repo, home.path())
        .args(["workspace", "init"])
        .assert()
        .success();

    // `workspace sync` reports each action's path as the process resolves it,
    // so fixtures compared against those paths have to resolve the same way.
    // A macOS temp dir arrives as `/var/...` and resolves to `/private/var/...`.
    let repo_root = std::fs::canonicalize(&repo).expect("canonical workspace root");
    let workspace_root = repo_root.join(".orbit");
    let auto_tasks = workspace_root.join("auto_tasks");
    let missing = auto_tasks.join("code-review.yaml");
    std::fs::remove_file(&missing).expect("remove a shipped auto-task");

    let locally_modified = auto_tasks.join("security-review.yaml");
    let local_body = format!(
        "{}# operator edit\n",
        std::fs::read_to_string(&locally_modified).expect("read managed auto-task")
    );
    std::fs::write(&locally_modified, &local_body).expect("edit managed auto-task");

    let collision = auto_tasks.join("qa-sweep.yaml");
    let collision_body = "operator-authored definition using a bundled file name\n";
    std::fs::write(&collision, collision_body).expect("write colliding auto-task");
    let manifest_path = auto_tasks.join(".orbit-managed-assets.json");
    let mut manifest: Value =
        serde_json::from_slice(&read(&manifest_path)).expect("parse manifest");
    manifest["assets"]
        .as_object_mut()
        .expect("assets object")
        .remove("qa-sweep");
    std::fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize manifest")
        ),
    )
    .expect("remove collision provenance");

    let registry = home.path().join(".orbit/workspaces.json");
    let identity = workspace_root.join("config.yaml");
    let registry_before = read(&registry);
    let identity_before = read(&identity);
    let gitignore_before = read_optional(repo.join(".gitignore"));

    let check = orbit(&repo, home.path())
        .args(["workspace", "sync", "--check", "--json"])
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();
    let checked: Value = serde_json::from_slice(&check).expect("parse check JSON");
    assert!(checked["check"].as_bool().expect("check flag"));
    assert!(
        checked["actions"]
            .as_array()
            .expect("actions")
            .iter()
            .any(|action| {
                action["outcome"] == "created"
                    && action["kind"] == "auto_task"
                    && action["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with("code-review.yaml"))
            })
    );
    assert!(
        !missing.exists(),
        "--check must not create the missing file"
    );
    assert_eq!(read(&registry), registry_before);
    assert_eq!(read(&identity), identity_before);
    assert_eq!(read_optional(repo.join(".gitignore")), gitignore_before);

    let applied = orbit(&repo, home.path())
        .args(["workspace", "sync", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let applied: Value = serde_json::from_slice(&applied).expect("parse apply JSON");
    let actions = applied["actions"].as_array().expect("actions");
    assert!(actions.iter().any(|action| {
        action["outcome"] == "preserved"
            && action["path"] == locally_modified.to_string_lossy().as_ref()
    }));
    assert!(actions.iter().any(|action| {
        action["outcome"] == "preserved" && action["path"] == collision.to_string_lossy().as_ref()
    }));
    assert!(
        missing.exists(),
        "apply creates the missing shipped definition"
    );
    assert_eq!(
        std::fs::read_to_string(&locally_modified).expect("read local edit"),
        local_body
    );
    assert_eq!(
        std::fs::read_to_string(&collision).expect("read collision"),
        collision_body
    );
    assert_eq!(read(&registry), registry_before);
    assert_eq!(read(&identity), identity_before);
    assert_eq!(read_optional(repo.join(".gitignore")), gitignore_before);

    let managed_after = read(&missing);
    orbit(&repo, home.path())
        .args(["workspace", "sync", "--check", "--json"])
        .assert()
        .success();
    assert_eq!(
        read(&missing),
        managed_after,
        "second run is byte-for-byte inert"
    );
}

#[test]
fn workspace_sync_outside_registered_workspace_fails_before_writing() {
    let parent = tempdir().expect("parent tempdir");
    let home = parent.path().join("home");
    let repo = home.join("outside-workspace");
    let uninitialized_root = home.join("uninitialized-root");
    git_repo::init(parent.path());
    git_repo::init(&repo);
    std::fs::create_dir_all(&uninitialized_root).expect("create uninitialized root");
    write_machine_identity(&home);
    orbit(parent.path(), &home)
        .args(["workspace", "init", "--name", "initialized-parent"])
        .assert()
        .success();
    let parent_state = read(parent.path().join(".orbit/config.yaml"));
    let before: Vec<_> = std::fs::read_dir(&repo).expect("read empty repo").collect();
    orbit(&repo, &home)
        .args([
            "workspace",
            "sync",
            "--root",
            uninitialized_root.to_str().expect("utf8 root"),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("orbit workspace init"));
    let after: Vec<_> = std::fs::read_dir(&repo)
        .expect("reread empty repo")
        .collect();
    assert_eq!(after.len(), before.len());
    assert!(!repo.join(".orbit").exists());
    assert_eq!(read(parent.path().join(".orbit/config.yaml")), parent_state);
}

/// Exact shipped jobs whose manifest cannot be written: both the JSON report
/// and the human summary name the skipped provenance write, and neither
/// claims the catalog converged or its provenance was recorded.
#[cfg(unix)]
#[test]
fn workspace_sync_reports_a_denied_manifest_write_without_claiming_convergence() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempdir().expect("home tempdir");
    let repo = home.path().join("workspace");
    git_repo::init(&repo);
    write_machine_identity(home.path());
    orbit(&repo, home.path())
        .args(["workspace", "init"])
        .assert()
        .success();
    let jobs = home.path().join(".orbit/resources/jobs");
    let manifest = jobs.join(".orbit-managed-assets.json");
    std::fs::remove_file(&manifest).expect("drop the job manifest");

    std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o555))
        .expect("make the job catalog read-only");
    let json = orbit(&repo, home.path())
        .args(["workspace", "sync", "--json"])
        .output()
        .expect("run JSON sync");
    let human = orbit(&repo, home.path())
        .args(["workspace", "sync"])
        .output()
        .expect("run human sync");
    std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o755))
        .expect("restore job catalog permissions");

    assert!(json.status.success(), "{json:?}");
    assert!(!manifest.exists(), "the denied write left no manifest");
    let report: Value = serde_json::from_slice(&json.stdout).expect("parse sync JSON");
    assert!(
        report["warnings"]
            .as_array()
            .expect("warnings array")
            .iter()
            .any(|warning| warning.as_str().is_some_and(|warning| {
                warning.contains("could not write managed job asset manifest")
            })),
        "{report}"
    );
    let migrated: Vec<_> = report["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .filter(|action| action["kind"] == "job" && action["outcome"] == "migrated")
        .collect();
    assert!(!migrated.is_empty(), "{report}");
    for action in migrated {
        assert!(
            action["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("not recorded")),
            "{action}"
        );
    }

    assert!(human.status.success(), "{human:?}");
    let stdout = String::from_utf8(human.stdout).expect("utf8 human output");
    assert!(
        stdout.contains("warning: could not write managed job asset manifest"),
        "{stdout}"
    );
    assert!(stdout.contains("not fully converged"), "{stdout}");
    assert!(!stdout.contains("managed artifacts converged"), "{stdout}");
    assert!(!stdout.contains("already converged"), "{stdout}");
}

/// [ORB-10726, ORB-12718] `workspace init` replaces a legacy bare `.orbit`
/// ignore line, and lines retired from older managed blocks, with one ignore
/// of the whole `.orbit/` directory while keeping the operator's own lines. A
/// forced re-init then leaves every checkout file byte-identical.
#[test]
fn workspace_init_migrates_legacy_gitignore_and_reinit_is_byte_idempotent() {
    let home = tempdir().expect("home tempdir");
    let repo = home.path().join("workspace");
    git_repo::init(&repo);
    write_machine_identity(home.path());
    let gitignore = repo.join(".gitignore");
    std::fs::write(
        &gitignore,
        "target/\n/.orbit/\n.orbit/*\n!.orbit/config.toml\n",
    )
    .expect("write legacy .gitignore");

    orbit(&repo, home.path())
        .args(["workspace", "init", "--name", "gitignore-migration"])
        .assert()
        .success();

    let migrated = std::fs::read_to_string(&gitignore).expect("read migrated .gitignore");
    let lines = migrated.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.first(),
        Some(&"target/"),
        "operator lines keep their place: {migrated}"
    );
    for legacy in ["/.orbit/", ".orbit/*", "!.orbit/config.toml"] {
        assert!(
            !lines.contains(&legacy),
            "legacy line `{legacy}` must be replaced: {migrated}"
        );
    }
    assert_eq!(
        lines.iter().filter(|line| **line == ".orbit/").count(),
        1,
        "exactly one `.orbit/` ignore: {migrated}"
    );
    for path in [".orbit/config.toml", ".orbit/config.yaml"] {
        let ignored = git_repo::command()
            .args(["check-ignore", "--quiet", path])
            .current_dir(&repo)
            .status()
            .expect("run git check-ignore");
        assert!(ignored.success(), "{path} must be ignored: {migrated}");
    }

    let before = checkout_files(&repo);
    assert!(
        before.contains_key(Path::new(".gitignore")) && before.len() > 1,
        "the snapshot covers the checkout's managed files: {:?}",
        before.keys().collect::<Vec<_>>()
    );
    orbit(&repo, home.path())
        .args([
            "workspace",
            "init",
            "--name",
            "gitignore-migration",
            "--force",
        ])
        .assert()
        .success();
    let after = checkout_files(&repo);
    let changed = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        changed.is_empty(),
        "re-init must leave every checkout file byte-identical; changed: {changed:?}"
    );
}

/// Delivery creates its worktrees under `<checkout>/.orbit/state/worktrees/`
/// even when `--root` keeps Orbit's data directory outside the checkout. If
/// the checkout does not ignore `.orbit/`, each worktree is an untracked
/// nested checkout that the primary-checkout Git snapshot cannot hash, and the
/// first local ship fails in its implement step. `workspace init` must ignore
/// it, and `workspace sync` must restore the ignore for an existing checkout.
#[test]
fn relocated_root_checkout_ignores_its_delivery_worktrees() {
    let home = tempdir().expect("home tempdir");
    let repo = home.path().join("workspace");
    git_repo::init(&repo);
    git(
        &repo,
        &["commit", "--quiet", "--allow-empty", "-m", "initial"],
    );
    let root = home.path().join("relocated-orbit-root");
    std::fs::create_dir_all(&root).expect("create relocated root");
    std::fs::write(
        root.join("config.toml"),
        "[machine]\nid = \"hm_relocated_root\"\nname = \"relocated\"\ntask_prefix = \"TST\"\n",
    )
    .expect("write machine identity");
    let root_arg = root.to_str().expect("utf-8 root");

    orbit(&repo, home.path())
        .args([
            "--root",
            root_arg,
            "workspace",
            "init",
            "--ship-mode",
            "local",
        ])
        .assert()
        .success();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            ".orbit/state/worktrees/orbit-delivery",
        ],
    );
    assert_snapshot_omits_orbit_dir(&repo, "after workspace init");

    std::fs::remove_file(repo.join(".gitignore")).expect("drop the managed ignore");
    orbit(&repo, home.path())
        .args(["--root", root_arg, "workspace", "sync"])
        .assert()
        .success();
    assert_snapshot_omits_orbit_dir(&repo, "after workspace sync");
}

/// The untracked set the primary-checkout snapshot reads must not reach into
/// `.orbit/`, where Orbit keeps its own nested delivery worktrees.
fn assert_snapshot_omits_orbit_dir(repo: &Path, when: &str) {
    let output = git_repo::command()
        .args(["status", "--porcelain=v2", "--untracked-files=all"])
        .current_dir(repo)
        .output()
        .expect("run git status");
    assert!(output.status.success(), "git status failed {when}");
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(
        !status.lines().any(|line| line.contains(".orbit/")),
        "the checkout's .orbit/ must be ignored {when}:\n{status}"
    );
}

fn git(repo: &Path, args: &[&str]) {
    let output = git_repo::command()
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.com",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// [ORB-12107] Without `--force`, `workspace init` refuses a second name for
/// an initialized checkout and an existing name for a second checkout. Neither
/// refusal writes the registry or a checkout identity.
#[test]
fn workspace_init_refuses_checkout_and_name_collisions_without_writing() {
    let home = tempdir().expect("home tempdir");
    let first = home.path().join("first");
    let second = home.path().join("second");
    for repo in [&first, &second] {
        git_repo::init(repo);
    }
    write_machine_identity(home.path());
    orbit(&first, home.path())
        .args(["workspace", "init", "--name", "shared-name"])
        .assert()
        .success();

    let registry = home.path().join(".orbit/workspaces.json");
    let identity = first.join(".orbit/config.yaml");
    let registry_before = read(&registry);
    let identity_before = read(&identity);

    orbit(&first, home.path())
        .args(["workspace", "init", "--name", "another-name"])
        .assert()
        .failure();
    assert_eq!(
        read(&registry),
        registry_before,
        "a renamed checkout collision must preserve the registry"
    );
    assert_eq!(
        read(&identity),
        identity_before,
        "a renamed checkout collision must preserve its identity"
    );

    orbit(&second, home.path())
        .args(["workspace", "init", "--name", "shared-name"])
        .assert()
        .failure();
    assert_eq!(
        read(&registry),
        registry_before,
        "a durable-name collision must preserve the registry"
    );
    assert!(
        !second.join(".orbit").exists(),
        "a durable-name collision must not initialize the second checkout"
    );
}

/// Every file in a checkout, outside `.git` and the runtime's own
/// `.orbit/state`, mapped to its bytes.
fn checkout_files(repo: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![repo.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read checkout directory") {
            let path = entry.expect("checkout entry").path();
            let relative = path
                .strip_prefix(repo)
                .expect("inside checkout")
                .to_path_buf();
            if relative == Path::new(".git") || relative == Path::new(".orbit/state") {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(relative, read(&path));
            }
        }
    }
    files
}
