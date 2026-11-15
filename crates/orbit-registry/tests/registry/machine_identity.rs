use orbit_registry::machine_identity::{
    MachineIdentityOutcome, NewMachineIdentity, ensure_machine_identity, load_machine_identity,
};
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
