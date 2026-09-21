use std::sync::Barrier;
use std::thread;

#[cfg(unix)]
use std::os::unix::fs::symlink;

use crate::machine_identity::{
    LEGACY_HOST_TOML_FILE, MachineIdentityOutcome, MachineIdentityState, NewMachineIdentity,
    ensure_machine_identity, inspect_machine_identity, load_machine_identity,
};

fn requested(
    name: &str,
    task_prefix: &str,
) -> impl FnOnce() -> Result<NewMachineIdentity, orbit_common::OrbitError> {
    let name = name.to_string();
    let task_prefix = task_prefix.to_string();
    move || Ok(NewMachineIdentity { name, task_prefix })
}

fn config_text(root: &std::path::Path) -> String {
    std::fs::read_to_string(root.join("config.toml")).expect("read config.toml")
}

#[test]
fn create_writes_the_machine_table_into_the_global_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outcome =
        ensure_machine_identity(dir.path(), requested("dk-server-1", "DE")).expect("create");
    assert!(matches!(outcome, MachineIdentityOutcome::Created(_)));
    let identity = outcome.identity();
    assert_eq!(identity.name, "dk-server-1");
    assert_eq!(identity.task_prefix, "DE");
    assert!(identity.id.starts_with("hm_"));

    let text = config_text(dir.path());
    assert!(text.contains("[machine]"), "{text}");
    assert!(text.contains("task_prefix = \"DE\""), "{text}");
    assert!(
        !dir.path().join(LEGACY_HOST_TOML_FILE).exists(),
        "a fresh init must not create host.toml"
    );

    let loaded = load_machine_identity(dir.path()).expect("load");
    assert_eq!(&loaded, identity);
}

#[test]
fn create_preserves_existing_config_keys_and_comments() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        "# hand-written note\n[workflow]\nbase_branch = \"trunk\"\n",
    )
    .expect("seed config");

    ensure_machine_identity(dir.path(), requested("dk-server-1", "DE")).expect("create");

    let text = config_text(dir.path());
    assert!(text.contains("# hand-written note"), "{text}");
    assert!(text.contains("base_branch = \"trunk\""), "{text}");
    assert!(text.contains("[machine]"), "{text}");
}

#[test]
fn concurrent_ensure_on_absent_root_creates_one_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let workers = 8;
    let barrier = Barrier::new(workers);

    let outcomes: Vec<MachineIdentityOutcome> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    ensure_machine_identity(root, requested("dk-server-1", "DE")).expect("ensure")
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("worker joined"))
            .collect()
    });

    let created = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, MachineIdentityOutcome::Created(_)))
        .count();
    assert_eq!(created, 1, "exactly one thread must create the identity");
    assert!(
        outcomes.iter().all(|outcome| matches!(
            outcome,
            MachineIdentityOutcome::Created(_) | MachineIdentityOutcome::Unchanged(_)
        )),
        "losers must observe Unchanged, not a second create: {outcomes:?}"
    );

    let machine_id = outcomes[0].identity().id.clone();
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.identity().id == machine_id),
        "all threads must observe the same machine id"
    );

    let loaded = load_machine_identity(root).expect("load");
    assert_eq!(loaded.id, machine_id);
    assert_eq!(loaded.name, "dk-server-1");
    assert_eq!(loaded.task_prefix, "DE");
}

#[test]
fn repeated_init_is_unchanged_and_stable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let created =
        ensure_machine_identity(dir.path(), requested("dk-server-1", "DK")).expect("create");
    let first_machine_id = created.identity().id.clone();
    let before = config_text(dir.path());

    // A second init must not prompt (the closure would panic) and must not
    // change the file.
    let again = ensure_machine_identity(dir.path(), || panic!("must not create on repeat"))
        .expect("repeat init");
    assert!(matches!(again, MachineIdentityOutcome::Unchanged(_)));
    assert_eq!(again.identity().id, first_machine_id);
    assert_eq!(config_text(dir.path()), before);
}

#[test]
fn absent_machine_table_classifies_as_absent_and_refuses_a_strict_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        inspect_machine_identity(dir.path()).expect("inspect"),
        MachineIdentityState::Absent
    );
    let error = load_machine_identity(dir.path())
        .expect_err("a strict load must refuse an uninitialized machine")
        .to_string();
    assert!(error.contains("orbit init"), "{error}");
}

#[test]
fn partial_machine_table_fails_closed_naming_the_missing_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        "[machine]\nname = \"dk-server-1\"\n",
    )
    .expect("seed config");

    let error = inspect_machine_identity(dir.path())
        .expect_err("a half-written identity must not resolve")
        .to_string();
    assert!(error.contains("machine.id"), "{error}");
    assert!(error.contains("machine.task_prefix"), "{error}");
}

#[test]
fn invalid_machine_id_fails_closed_without_regenerating_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        "[machine]\nid = \"dk-server-1\"\nname = \"dk-server-1\"\ntask_prefix = \"DE\"\n",
    )
    .expect("seed config");

    let error = inspect_machine_identity(dir.path())
        .expect_err("a hand-edited machine.id must not resolve")
        .to_string();
    assert!(error.contains("machine.id"), "{error}");
    // The refused file is left exactly as the operator wrote it.
    assert!(config_text(dir.path()).contains("id = \"dk-server-1\""));
}

#[test]
fn invalid_task_prefix_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        "[machine]\nid = \"hm_0123456789abcdef\"\nname = \"dk\"\ntask_prefix = \"lower\"\n",
    )
    .expect("seed config");

    let error = inspect_machine_identity(dir.path())
        .expect_err("a malformed namespace must not resolve")
        .to_string();
    assert!(error.contains("machine.task_prefix"), "{error}");
}

#[test]
fn legacy_host_toml_is_folded_into_the_machine_table_and_removed() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(LEGACY_HOST_TOML_FILE),
        "schema_version = 2\nmachine_id = \"hm_9ca6004473492f06\"\nhost_id = \"dk-server-1\"\n\
         task_prefix = \"ORB\"\n",
    )
    .expect("seed host.toml");

    let identity = load_machine_identity(dir.path()).expect("load migrates");
    assert_eq!(identity.id, "hm_9ca6004473492f06");
    assert_eq!(identity.name, "dk-server-1");
    assert_eq!(identity.task_prefix, "ORB");
    assert!(
        !dir.path().join(LEGACY_HOST_TOML_FILE).exists(),
        "the migrated file must be removed"
    );
    assert!(config_text(dir.path()).contains("hm_9ca6004473492f06"));

    // Idempotent: a second load reads the table and writes nothing new.
    let before = config_text(dir.path());
    assert_eq!(load_machine_identity(dir.path()).expect("reload"), identity);
    assert_eq!(config_text(dir.path()), before);
}

#[test]
fn legacy_host_toml_without_a_machine_id_or_prefix_keeps_its_name_and_orb_namespace() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(LEGACY_HOST_TOML_FILE),
        "host_id = \"dk-server-1\"\n",
    )
    .expect("seed host.toml");

    let identity = load_machine_identity(dir.path()).expect("load migrates");
    assert_eq!(identity.name, "dk-server-1");
    assert_eq!(identity.task_prefix, "ORB");
    assert!(identity.id.starts_with("hm_"));
}

#[test]
fn disagreeing_host_toml_and_machine_table_fail_closed_naming_both_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        "[machine]\nid = \"hm_0123456789abcdef\"\nname = \"dk-server-1\"\ntask_prefix = \"DE\"\n",
    )
    .expect("seed config");
    std::fs::write(
        dir.path().join(LEGACY_HOST_TOML_FILE),
        "schema_version = 2\nmachine_id = \"hm_ffffffffffffffff\"\nhost_id = \"other\"\n\
         task_prefix = \"OT\"\n",
    )
    .expect("seed host.toml");

    let error = inspect_machine_identity(dir.path())
        .expect_err("two different identities must not silently resolve to one")
        .to_string();
    assert!(error.contains("host.toml"), "{error}");
    assert!(error.contains("config.toml"), "{error}");
    assert!(
        dir.path().join(LEGACY_HOST_TOML_FILE).exists(),
        "a refused migration must leave both files for the operator"
    );
}

#[test]
fn an_agreeing_host_toml_is_removed_without_rewriting_the_machine_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        "[machine]\nid = \"hm_0123456789abcdef\"\nname = \"dk-server-1\"\ntask_prefix = \"DE\"\n",
    )
    .expect("seed config");
    let before = config_text(dir.path());
    std::fs::write(
        dir.path().join(LEGACY_HOST_TOML_FILE),
        "schema_version = 2\nmachine_id = \"hm_0123456789abcdef\"\nhost_id = \"dk-server-1\"\n\
         task_prefix = \"DE\"\n",
    )
    .expect("seed host.toml");

    load_machine_identity(dir.path()).expect("load");
    assert!(!dir.path().join(LEGACY_HOST_TOML_FILE).exists());
    assert_eq!(config_text(dir.path()), before);
}

#[cfg(unix)]
#[test]
fn a_symlinked_host_toml_is_refused_rather_than_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = dir.path().join("elsewhere.toml");
    std::fs::write(&outside, "host_id = \"stolen\"\n").expect("write target");
    symlink(&outside, dir.path().join(LEGACY_HOST_TOML_FILE)).expect("symlink");

    let error = inspect_machine_identity(dir.path())
        .expect_err("a symlinked identity file must be refused")
        .to_string();
    assert!(error.contains("regular"), "{error}");
}

/// Resolving who this machine is is a read. A root Orbit can read but not
/// write still resolves a legacy `host.toml` from memory, keeps the file for a
/// later writable open, and never fails the caller. [ORB-12725]
#[cfg(unix)]
#[test]
fn a_read_only_root_still_resolves_a_legacy_identity_without_migrating_it() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(LEGACY_HOST_TOML_FILE),
        "schema_version = 2\nmachine_id = \"hm_readonly\"\nhost_id = \"frozen\"\n\
         task_prefix = \"RO\"\n",
    )
    .expect("seed host.toml");
    let mut permissions = std::fs::metadata(dir.path())
        .expect("root metadata")
        .permissions();
    permissions.set_mode(0o500);
    std::fs::set_permissions(dir.path(), permissions).expect("freeze root");

    let resolved = load_machine_identity(dir.path());

    // Restore write access before the assertions so the tempdir can be reaped
    // whichever way they go.
    let mut permissions = std::fs::metadata(dir.path())
        .expect("root metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(dir.path(), permissions).expect("thaw root");

    let identity = resolved.expect("a read-only root still resolves its identity");
    assert_eq!(identity.id, "hm_readonly");
    assert_eq!(identity.name, "frozen");
    assert_eq!(identity.task_prefix, "RO");
    assert!(
        dir.path().join(LEGACY_HOST_TOML_FILE).exists(),
        "an unpersisted migration must keep the file for a later writable open"
    );
}
