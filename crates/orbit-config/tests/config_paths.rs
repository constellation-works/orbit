//! Public config-file admission, separate from value/schema tests. The FIFO
//! case owns a guarded child so a blocking-read regression cannot hang the suite.
#![allow(clippy::expect_used, missing_docs)]

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_config::{
    ConfigRoots, ResolvedConfig, load_workspace_plugin_enablement, workspace_config_sets_policy,
};

const GLOBAL: &str = "[execution.codex]\nsandbox = 'danger-full-access'\n\
                      [execution.env]\npass = ['GLOBAL_TOKEN']\n";
const TOGGLES: &str = "[plugin_enablement]\ngraph = false\n";

fn roots(directory: &Path) -> ConfigRoots {
    let global = directory.join("global");
    let workspace = directory.join("workspace");
    fs::create_dir(&global).expect("global directory");
    fs::create_dir(&workspace).expect("workspace directory");
    fs::write(global.join("config.toml"), GLOBAL).expect("global config");
    ConfigRoots::new(global, workspace)
}

fn assert_refused(error: OrbitError, path: &Path, cause: &str) {
    let OrbitError::InvalidInput(message) = error else {
        panic!("expected config path admission refusal, got {error}");
    };
    assert!(message.contains(cause), "wrong admission cause: {message}");
    assert!(
        message.contains(path.to_string_lossy().as_ref()),
        "refusal must name the selected config: {message}"
    );
}

fn assert_loaders_refuse(roots: &ConfigRoots, workspace_leaf: bool, cause: &str) {
    let root = if workspace_leaf {
        roots.workspace()
    } else {
        roots.global()
    };
    let path = root.join("config.toml");
    assert_refused(
        ResolvedConfig::load(roots).expect_err("runtime must refuse an inadmissible leaf"),
        &path,
        cause,
    );
    if workspace_leaf {
        assert_refused(
            load_workspace_plugin_enablement(roots)
                .expect_err("toggle poll must refuse an inadmissible leaf"),
            &path,
            cause,
        );
        assert!(workspace_config_sets_policy(&path));
    }
}

#[test]
fn absent_files_and_parents_inherit_defaults_but_an_empty_file_is_a_layer() {
    let directory = tempfile::tempdir().expect("fixture directory");
    for create_parents in [false, true] {
        let global = directory.path().join(format!("global-{create_parents}"));
        let workspace = directory.path().join(format!("workspace-{create_parents}"));
        if create_parents {
            fs::create_dir(&global).expect("global parent");
            fs::create_dir(&workspace).expect("workspace parent");
        }
        let roots = ConfigRoots::new(&global, &workspace);
        let built_in = ResolvedConfig::load(&ConfigRoots::global_only(&global))
            .expect("absent global config resolves built-in defaults");
        let absent = ResolvedConfig::load(&roots).expect("both absent layers load");
        assert_eq!(
            absent.codex_execution.sandbox(),
            built_in.codex_execution.sandbox()
        );
        assert!(
            load_workspace_plugin_enablement(&roots)
                .expect("absent toggles")
                .is_empty()
        );
        assert!(!workspace_config_sets_policy(
            &workspace.join("config.toml")
        ));

        if !create_parents {
            fs::create_dir(&global).expect("create global parent");
        }
        fs::write(global.join("config.toml"), GLOBAL).expect("global config");
        let inherited = ResolvedConfig::load(&roots).expect("absent workspace inherits global");
        assert_eq!(inherited.codex_execution.sandbox(), "danger-full-access");
        assert_eq!(inherited.snapshot.execution_env_pass, ["GLOBAL_TOKEN"]);

        if !create_parents {
            fs::create_dir(&workspace).expect("create workspace parent");
        }
        fs::write(workspace.join("config.toml"), "").expect("empty workspace config");
        let empty = ResolvedConfig::load(&roots).expect("empty workspace loads");
        assert_eq!(
            empty.codex_execution.sandbox(),
            built_in.codex_execution.sandbox()
        );
        assert_eq!(
            empty.snapshot.execution_env_pass,
            built_in.snapshot.execution_env_pass
        );
        assert!(workspace_config_sets_policy(&workspace.join("config.toml")));
    }
}

#[cfg(unix)]
#[test]
fn symlink_leaves_cannot_supply_external_config_or_toggles() {
    use std::os::unix::fs::symlink;

    for workspace_leaf in [false, true] {
        let directory = tempfile::tempdir().expect("fixture directory");
        let roots = roots(directory.path());
        let external = directory.path().join("external.toml");
        fs::write(&external, if workspace_leaf { TOGGLES } else { GLOBAL })
            .expect("external config");
        let path = if workspace_leaf {
            roots.workspace()
        } else {
            roots.global()
        }
        .join("config.toml");
        if !workspace_leaf {
            fs::remove_file(&path).expect("replace fixture global file");
        }
        symlink(&external, &path).expect("config symlink");
        assert_loaders_refuse(&roots, workspace_leaf, "must not be a symlink");
    }
}

#[cfg(unix)]
#[test]
fn dangling_symlink_leaves_are_refused_instead_of_treated_as_absent() {
    use std::os::unix::fs::symlink;

    for workspace_leaf in [false, true] {
        let directory = tempfile::tempdir().expect("fixture directory");
        let roots = roots(directory.path());
        let path = if workspace_leaf {
            roots.workspace()
        } else {
            roots.global()
        }
        .join("config.toml");
        if !workspace_leaf {
            fs::remove_file(&path).expect("replace fixture global file");
        }
        symlink(directory.path().join("absent.toml"), &path).expect("dangling config symlink");
        assert!(!path.exists(), "fixture is a dangling link");
        assert_loaders_refuse(&roots, workspace_leaf, "must not be a symlink");
    }
}

#[test]
fn directory_leaves_are_refused_as_non_regular_files() {
    for workspace_leaf in [false, true] {
        let directory = tempfile::tempdir().expect("fixture directory");
        let roots = roots(directory.path());
        let path = if workspace_leaf {
            roots.workspace()
        } else {
            roots.global()
        }
        .join("config.toml");
        if !workspace_leaf {
            fs::remove_file(&path).expect("replace fixture global file");
        }
        fs::create_dir(&path).expect("config directory leaf");
        assert_loaders_refuse(&roots, workspace_leaf, "must be a regular file");
    }
}

#[cfg(unix)]
#[test]
fn fifo_leaves_are_refused_without_waiting_for_a_writer() {
    use orbit_common::test_env::{assert_child_test_passed, run_child_test};
    use std::process::Command;

    const CHILD: &str = "ORBIT_CONFIG_FIFO_CHILD";
    const TEST: &str = "fifo_leaves_are_refused_without_waiting_for_a_writer";
    if std::env::var_os(CHILD).is_none() {
        let output = tempfile::tempdir().expect("child output directory");
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1");
        let result = run_child_test(&mut command, TEST, output.path());
        assert_child_test_passed(TEST, result.status, &result.stdout, &result.stderr);
        return;
    }
    for workspace_leaf in [false, true] {
        let directory = tempfile::tempdir().expect("fixture directory");
        let roots = roots(directory.path());
        let path = if workspace_leaf {
            roots.workspace()
        } else {
            roots.global()
        }
        .join("config.toml");
        if !workspace_leaf {
            fs::remove_file(&path).expect("replace fixture global file");
        }
        assert!(
            Command::new("mkfifo")
                .arg(&path)
                .status()
                .expect("create FIFO")
                .success()
        );
        assert_loaders_refuse(&roots, workspace_leaf, "must be a regular file");
    }
}

#[cfg(unix)]
#[test]
fn symlinked_parent_directories_still_load_regular_config_files() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("fixture directory");
    let real = roots(directory.path());
    fs::write(real.workspace().join("config.toml"), TOGGLES).expect("workspace toggles");
    let global_alias = directory.path().join("global-alias");
    let workspace_alias = directory.path().join("workspace-alias");
    symlink(real.global(), &global_alias).expect("global parent symlink");
    symlink(real.workspace(), &workspace_alias).expect("workspace parent symlink");
    let aliased = ConfigRoots::new(global_alias, workspace_alias);
    let config = ResolvedConfig::load(&aliased).expect("regular config through aliased parents");
    assert_eq!(config.codex_execution.sandbox(), "danger-full-access");
    assert_eq!(config.plugin_enablement.get("graph"), Some(&false));
    assert_eq!(
        load_workspace_plugin_enablement(&aliased).expect("aliased toggles"),
        config.plugin_enablement
    );
}
