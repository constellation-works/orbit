//! Seed identity and host-wide routine name collisions.

use orbit_common::fs::io::write_text_with_parent;
use tempfile::tempdir;

use super::super::materialize::seed_default_routines;
use super::super::seed::{
    DEFAULT_ROUTINE_FILES, RoutineSeedIdentity, default_routine_name_collisions,
};

/// Without a usable workspace suffix every workspace on the host would
/// seed the same bare `task-pilot` name, so seeding refuses the name
/// instead of writing definitions that drop each other at load time.
#[test]
fn seeding_requires_a_workspace_name_with_usable_characters() {
    let root = tempdir().expect("create tempdir");
    let err = seed_default_routines(&root.path().join("routines"), " ***", true)
        .expect_err("unusable workspace name must not seed unsuffixed routines");
    assert!(err.to_string().contains("routine name"), "{err}");
}

/// The seeded suffix is the registered workspace name, so two checkouts
/// whose directories share a basename still seed distinct names, and a
/// name mismatch never leaks the directory into the routine [ORB-12107].
#[test]
fn seeded_names_follow_the_workspace_name_not_the_checkout_directory() {
    let alpha = RoutineSeedIdentity::new("Alpha QA", "hm_test", "main")
        .expect("workspace name renders a routine suffix");
    let beta =
        RoutineSeedIdentity::new("beta", "hm_test", "main").expect("second workspace identity");

    assert_eq!(alpha.routine_name("task_pilot"), "task-pilot-alpha-qa");
    assert_eq!(beta.routine_name("task_pilot"), "task-pilot-beta");
    assert!(
        alpha
            .seeded_routine_names()
            .iter()
            .all(|name| !beta.seeded_routine_names().contains(name)),
        "distinct workspace names must not share a seeded routine name"
    );
}

/// A name another workspace on the host already declares is reported
/// before seeding: routine discovery drops every colliding definition, so
/// writing the duplicate would disable both workspaces' routines.
#[test]
fn collisions_report_names_another_workspace_already_declares() {
    let root = tempdir().expect("create tempdir");
    let other_orbit = root.path().join("other/.orbit");
    seed_default_routines(&other_orbit.join("routines"), "server", false)
        .expect("seed the other workspace");

    let identity = RoutineSeedIdentity::new("server", "hm_test", "main").expect("seed identity");
    let collisions = default_routine_name_collisions(&identity, std::slice::from_ref(&other_orbit));
    assert_eq!(
        collisions.len(),
        DEFAULT_ROUTINE_FILES.len(),
        "every seeded name collides: {collisions:?}"
    );
    assert!(collisions.iter().any(|collision| {
        collision.name == "task-pilot-server"
            && collision.declared_in == other_orbit.join("routines/task_pilot.yaml")
    }));

    let distinct =
        RoutineSeedIdentity::new("other-server", "hm_test", "main").expect("seed identity");
    assert!(
        default_routine_name_collisions(&distinct, &[other_orbit]).is_empty(),
        "a distinct workspace name must not collide"
    );
}

/// Local definitions share the host-wide name space, so a `local/`
/// routine is detected too.
#[test]
fn collisions_cover_local_routine_definitions() {
    let root = tempdir().expect("create tempdir");
    let other_orbit = root.path().join("other/.orbit");
    let local_dir = other_orbit.join("routines/local");
    seed_default_routines(&other_orbit.join("routines"), "alpha", false)
        .expect("seed the other workspace");
    let local = std::fs::read_to_string(other_orbit.join("routines/task_pilot.yaml"))
        .expect("read a seeded routine to adapt")
        .replace("task-pilot-alpha", "task-pilot-beta");
    write_text_with_parent(&local_dir.join("pilot.yaml"), &local).expect("write local routine");

    let identity = RoutineSeedIdentity::new("beta", "hm_test", "main").expect("seed identity");
    let collisions = default_routine_name_collisions(&identity, &[other_orbit]);
    assert_eq!(
        collisions
            .iter()
            .map(|collision| collision.name.as_str())
            .collect::<Vec<_>>(),
        vec!["task-pilot-beta"]
    );
}
