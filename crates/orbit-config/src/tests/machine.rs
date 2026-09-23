//! The `[machine]` table: global-only, read-only except for `machine.name`,
//! and admitted as one identity rather than three independent settings.

use tempfile::tempdir;

use super::{roots, write_config};
use crate::{
    ConfigScope, ConfigStore, MachineSettings, ResolvedConfig, WorkerContainmentSettings,
    admit_settable_config_key, load_machine_settings,
};

const IDENTITY: &str =
    "[machine]\nid = \"hm_0123456789abcdef\"\nname = \"dk-server-1\"\ntask_prefix = \"DE\"\n";

#[test]
fn a_complete_machine_table_admits_and_projects_through_config_get() {
    let global = tempdir().expect("global");
    write_config(global.path(), IDENTITY);

    let settings = load_machine_settings(global.path()).expect("machine settings");
    assert_eq!(
        settings.clone().complete(),
        Some((
            "hm_0123456789abcdef".to_string(),
            "dk-server-1".to_string(),
            "DE".to_string()
        ))
    );

    // The same values reach `orbit config get`/`show` through the snapshot, so
    // a runtime open and the operator surface cannot disagree.
    let resolved =
        ResolvedConfig::load(&roots(global.path(), global.path())).expect("layered load");
    assert_eq!(resolved.snapshot.machine(), settings);
    assert_eq!(
        resolved.snapshot.value_for("machine.id"),
        Some(serde_json::json!("hm_0123456789abcdef"))
    );
}

#[test]
fn an_absent_machine_table_is_not_an_error() {
    let global = tempdir().expect("global");
    write_config(global.path(), "[workflow]\nbase_branch = \"main\"\n");

    assert_eq!(
        load_machine_settings(global.path()).expect("machine settings"),
        MachineSettings::default()
    );
    assert!(load_machine_settings(tempdir().expect("empty").path()).is_ok());
}

#[test]
fn a_partial_machine_table_fails_closed_naming_the_missing_keys() {
    let global = tempdir().expect("global");
    write_config(global.path(), "[machine]\nname = \"dk-server-1\"\n");

    let error = load_machine_settings(global.path())
        .expect_err("a half-written identity must not resolve")
        .to_string();
    assert!(error.contains("machine.id"), "{error}");
    assert!(error.contains("machine.task_prefix"), "{error}");
    assert!(error.contains("orbit init"), "{error}");
}

#[test]
fn hand_edited_identity_values_are_validated_rather_than_regenerated() {
    for (body, expected) in [
        (
            "[machine]\nid = \"dk\"\nname = \"dk\"\ntask_prefix = \"DE\"\n",
            "machine.id",
        ),
        (
            "[machine]\nid = \"hm_a\"\nname = \"dk\"\ntask_prefix = \"lower\"\n",
            "machine.task_prefix",
        ),
        (
            "[machine]\nid = \"hm_a\"\nname = \"a/b\"\ntask_prefix = \"DE\"\n",
            "machine.name",
        ),
    ] {
        let global = tempdir().expect("global");
        write_config(global.path(), body);
        let error = load_machine_settings(global.path())
            .expect_err("an invalid identity must not resolve")
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn identity_reads_do_not_inherit_the_rest_of_the_document_s_failure_domain() {
    // A config an operator has to repair (`[crews.*]` with no default crew)
    // must still say who this machine is: `orbit init --force` is the repair
    // tool, and it resolves the identity before it rewrites anything.
    let global = tempdir().expect("global");
    write_config(
        global.path(),
        &format!("{IDENTITY}\n[crews.qa]\nprovider = \"codex\"\nmodel = \"gpt-5.6-terra\"\n"),
    );

    assert!(ResolvedConfig::load(&roots(global.path(), global.path())).is_err());
    assert_eq!(
        load_machine_settings(global.path())
            .expect("identity still resolves")
            .name
            .as_deref(),
        Some("dk-server-1")
    );
}

#[test]
fn a_workspace_machine_table_is_refused_at_load_naming_the_file() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    write_config(global.path(), IDENTITY);
    write_config(
        workspace.path(),
        "[machine]\nid = \"hm_forged\"\nname = \"forged\"\ntask_prefix = \"FG\"\n",
    );

    let error = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect_err("a checkout must not be able to re-identify its machine")
        .to_string();
    assert!(
        error.contains("[machine] is not a workspace setting"),
        "{error}"
    );
    assert!(error.contains("config.toml"), "{error}");
    assert!(error.contains("--global"), "{error}");
}

#[test]
fn config_set_refuses_the_two_read_only_identity_keys() {
    for key in ["machine.id", "machine.task_prefix"] {
        let error = admit_settable_config_key(key)
            .expect_err("an identity an operator cannot change must not be settable")
            .to_string();
        assert!(error.contains("read-only"), "{error}");
        assert!(error.contains(key), "{error}");
    }
    admit_settable_config_key("machine.name").expect("the display name is the one settable field");
}

#[test]
fn a_workspace_store_refuses_a_machine_key_before_touching_the_document() {
    let workspace = tempdir().expect("workspace");
    let path = workspace.path().join("config.toml");
    std::fs::write(&path, "[workflow]\nbase_branch = \"main\"\n").expect("seed workspace config");

    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open");
    let error = store
        .set_value("machine.name", "forged")
        .expect_err("machine identity is global-only")
        .to_string();
    assert!(error.contains("--global"), "{error}");
    assert!(
        !store.is_key_set("machine.name"),
        "the refusal must not stage a write"
    );
}

#[test]
fn set_machine_identity_writes_the_table_without_disturbing_the_rest_of_the_file() {
    let global = tempdir().expect("global");
    let path = global.path().join("config.toml");
    std::fs::write(
        &path,
        "# operator note\n[workflow]\nbase_branch = \"trunk\"\n",
    )
    .expect("seed global config");

    let mut store = ConfigStore::open(ConfigScope::Global, &path).expect("open");
    store
        .set_machine_identity("hm_0123456789abcdef", "dk-server-1", "DE")
        .expect("record identity");
    store.save().expect("save");

    let written = std::fs::read_to_string(&path).expect("read back");
    assert!(written.contains("# operator note"), "{written}");
    assert!(written.contains("base_branch = \"trunk\""), "{written}");
    assert_eq!(
        load_machine_settings(global.path())
            .expect("identity")
            .complete(),
        Some((
            "hm_0123456789abcdef".to_string(),
            "dk-server-1".to_string(),
            "DE".to_string()
        ))
    );
}

#[cfg(unix)]
#[test]
fn load_machine_settings_refuses_a_symlinked_config_file() {
    let global = tempdir().expect("global");
    let real = global.path().join("real.toml");
    std::fs::write(&real, IDENTITY).expect("write real config");
    std::os::unix::fs::symlink(&real, global.path().join("config.toml")).expect("symlink leaf");

    let error = load_machine_settings(global.path())
        .expect_err("a symlinked identity file must be refused")
        .to_string();
    assert!(error.contains("must not be a symlink"), "{error}");
}

#[cfg(unix)]
#[test]
fn load_machine_settings_follows_a_symlinked_parent_directory() {
    let dir = tempdir().expect("parent");
    let real_dir = dir.path().join("real");
    std::fs::create_dir(&real_dir).expect("create real parent");
    write_config(&real_dir, IDENTITY);
    let link_dir = dir.path().join("link");
    std::os::unix::fs::symlink(&real_dir, &link_dir).expect("symlink parent");

    let settings = load_machine_settings(&link_dir).expect("symlinked parent is allowed");
    assert_eq!(
        settings.complete(),
        Some((
            "hm_0123456789abcdef".to_string(),
            "dk-server-1".to_string(),
            "DE".to_string()
        ))
    );
}

#[test]
fn a_workspace_store_never_writes_a_machine_identity() {
    let workspace = tempdir().expect("workspace");
    let path = workspace.path().join("config.toml");
    let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open");
    assert!(
        store
            .set_machine_identity("hm_0123456789abcdef", "forged", "FG")
            .is_err()
    );
}

#[test]
fn an_identity_key_cannot_be_unset_into_a_partial_table() {
    let global = tempdir().expect("global");
    let path = global.path().join("config.toml");
    std::fs::write(&path, IDENTITY).expect("seed global config");

    let mut store = ConfigStore::open(ConfigScope::Global, &path).expect("open");
    let error = store
        .unset_value("machine.name")
        .expect_err("clearing one identity key would leave a table that does not load")
        .to_string();
    assert!(error.contains("cannot be unset"), "{error}");
    assert!(
        store.is_key_set("machine.name"),
        "the refusal must not stage a removal"
    );
}

/// [ORB-12903] Worker limits live in the global `[machine]` table beside the
/// identity, default on, and reach consumers as admitted systemd values.
#[test]
fn worker_limits_default_on_and_admit_systemd_sizes() {
    let global = tempdir().expect("global");
    write_config(global.path(), IDENTITY);
    let defaults = ResolvedConfig::load(&roots(global.path(), global.path()))
        .expect("layered load")
        .snapshot
        .worker_containment();
    assert!(defaults.enabled);
    assert!(defaults.memory_max.ends_with('%'), "derived from host RAM");
    assert!(defaults.tasks_max > 0);

    write_config(
        global.path(),
        &format!(
            "{IDENTITY}worker_containment = false\nworker_memory_high = \"6G\"\n\
             worker_memory_max = \"infinity\"\nworker_tasks_max = 512\n"
        ),
    );
    let configured = ResolvedConfig::load(&roots(global.path(), global.path()))
        .expect("layered load")
        .snapshot
        .worker_containment();
    assert_eq!(
        configured,
        WorkerContainmentSettings {
            enabled: false,
            memory_high: "6G".to_string(),
            memory_max: "infinity".to_string(),
            tasks_max: 512,
        }
    );
}

/// Each value is later one `systemd-run --property=` argument; anything the
/// manager would reject must fail at load, not at every worker launch.
#[test]
fn malformed_worker_limits_fail_closed_naming_the_key() {
    for (line, key) in [
        ("worker_memory_max = \"8 G\"", "machine.worker_memory_max"),
        ("worker_memory_max = \"150%\"", "machine.worker_memory_max"),
        (
            "worker_memory_high = \"8G TasksMax=1\"",
            "machine.worker_memory_high",
        ),
        ("worker_memory_high = \"0\"", "machine.worker_memory_high"),
        ("worker_tasks_max = 0", "machine.worker_tasks_max"),
    ] {
        let global = tempdir().expect("global");
        write_config(global.path(), &format!("{IDENTITY}{line}\n"));
        let error = ResolvedConfig::load(&roots(global.path(), global.path()))
            .expect_err("malformed worker limit must not load")
            .to_string();
        assert!(error.contains(key), "{line}: {error}");
    }
}

/// Unlike the identity, a worker limit has a built-in default to fall back to.
#[test]
fn a_worker_limit_unsets_to_its_default() {
    let global = tempdir().expect("global");
    let path = global.path().join("config.toml");
    std::fs::write(&path, format!("{IDENTITY}worker_tasks_max = 512\n")).expect("seed");

    let mut store = ConfigStore::open(ConfigScope::Global, &path).expect("open");
    assert!(
        store
            .unset_value("machine.worker_tasks_max")
            .expect("unset worker limit")
    );
    assert!(store.is_key_set("machine.id"));
}
