use crate::machine_identity::{
    LEGACY_HOST_TOML_FILE, MachineIdentityOutcome, NewMachineIdentity, ensure_machine_identity,
    inspect_machine_identity, load_machine_identity,
};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::sync::Barrier;
use std::thread;

fn requested(
    name: &str,
    task_prefix: &str,
) -> impl FnOnce() -> Result<NewMachineIdentity, orbit_common::OrbitError> {
    let name = name.to_string();
    let task_prefix = task_prefix.to_string();
    move || Ok(NewMachineIdentity { name, task_prefix })
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
