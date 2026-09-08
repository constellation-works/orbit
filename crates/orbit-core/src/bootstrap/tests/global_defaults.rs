//! When a runtime open must reconcile the global managed defaults, and what it
//! must leave untouched when it does not.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::{TempDir, tempdir};

use crate::OrbitRuntime;
use crate::bootstrap::global_defaults::{global_defaults_are_current, stamp_path};
use crate::bootstrap::init::ensure_orbit_root_initialized;
use crate::runtime::OrbitRuntimeRoots;

/// A stamp whose digest belongs to no build of this binary — the shape a root
/// last reconciled by a different Orbit release has.
const FOREIGN_RELEASE_STAMP: &str = r#"{
  "schemaVersion": 1,
  "defaultsDigest": "0000000000000000000000000000000000000000000000000000000000000000"
}
"#;

struct Roots {
    _temp: TempDir,
    global: PathBuf,
    workspace: PathBuf,
}

fn roots() -> Roots {
    let temp = tempdir().expect("tempdir");
    let global = temp.path().join("global");
    let workspace = temp.path().join("repo/.orbit");
    Roots {
        _temp: temp,
        global,
        workspace,
    }
}

fn activities_dir(global_root: &Path) -> PathBuf {
    global_root.join("resources/activities")
}

fn managed_manifest_path(dir: &Path) -> PathBuf {
    dir.join(crate::application::MANAGED_ASSET_MANIFEST_FILE)
}

#[test]
fn first_open_seeds_global_defaults_and_stamps_the_root() {
    let roots = roots();
    assert!(
        !global_defaults_are_current(&roots.global),
        "an unseeded root cannot be current"
    );

    ensure_orbit_root_initialized(&roots.global, &roots.workspace).expect("first runtime open");

    assert!(
        roots
            .global
            .join("skills/orbit/SKILL.md")
            .try_exists()
            .expect("probe seeded skill router"),
        "first open must seed the global skill catalog"
    );
    assert!(
        activities_dir(&roots.global)
            .join("agent_implement.yaml")
            .try_exists()
            .expect("probe seeded activity"),
        "first open must seed the global activity catalog"
    );
    assert!(
        roots
            .global
            .join("config.toml")
            .try_exists()
            .expect("probe seeded config"),
        "first open must seed the global config"
    );
    assert!(
        global_defaults_are_current(&roots.global),
        "a completed first open must stamp the root"
    );
}

/// The stamp exists to make the second open cheap: nothing under the managed
/// catalogs may be read or hashed again. A manifest no reconciliation could
/// parse stands in as the sentinel — reconciliation loads it before deciding
/// anything, so an open that still succeeds provably never ran one.
#[test]
fn warm_open_does_not_reconcile_managed_asset_digests() {
    let roots = roots();
    ensure_orbit_root_initialized(&roots.global, &roots.workspace).expect("first runtime open");

    let activities = activities_dir(&roots.global);
    let activity_path = activities.join("agent_implement.yaml");
    let seeded_activity = fs::read_to_string(&activity_path).expect("read seeded activity");
    fs::write(managed_manifest_path(&activities), "{ not json").expect("corrupt managed manifest");

    ensure_orbit_root_initialized(&roots.global, &roots.workspace)
        .expect("warm open must not read the managed activity manifest");

    assert_eq!(
        fs::read_to_string(&activity_path).expect("read activity after warm open"),
        seeded_activity,
        "a warm open must leave the managed catalog exactly as it found it"
    );

    // Control: the corrupt manifest really is load-bearing, so the assertion
    // above measures the skip rather than a tolerant reconciliation.
    fs::remove_file(stamp_path(&roots.global)).expect("drop the stamp");
    let unstamped = ensure_orbit_root_initialized(&roots.global, &roots.workspace);
    assert!(
        unstamped.is_err(),
        "without a stamp the open reconciles and must surface the corrupt manifest"
    );
}

/// A root stamped by an earlier release is reconciled again: shipped defaults
/// this binary needs are restored, and anything the operator edited is kept.
#[test]
fn upgraded_root_restores_required_defaults_and_preserves_user_edits() {
    let roots = roots();
    ensure_orbit_root_initialized(&roots.global, &roots.workspace).expect("first runtime open");

    let activities = activities_dir(&roots.global);
    let removed = activities.join("agent_implement.yaml");
    let shipped_activity = fs::read_to_string(&removed).expect("read seeded activity");
    fs::remove_file(&removed).expect("remove a required default");

    let edited = activities.join("sleep.yaml");
    let operator_content = format!(
        "{}# operator edit\n",
        fs::read_to_string(&edited).expect("read seeded activity")
    );
    fs::write(&edited, &operator_content).expect("hand edit a managed default");

    fs::write(stamp_path(&roots.global), FOREIGN_RELEASE_STAMP).expect("stamp an older release");
    assert!(!global_defaults_are_current(&roots.global));

    ensure_orbit_root_initialized(&roots.global, &roots.workspace).expect("upgraded runtime open");

    assert_eq!(
        fs::read_to_string(&removed).expect("read restored activity"),
        shipped_activity,
        "an upgraded root must restore the defaults this binary requires"
    );
    assert_eq!(
        fs::read_to_string(&edited).expect("read preserved activity"),
        operator_content,
        "reconciliation must preserve a locally modified managed asset"
    );
    assert!(
        global_defaults_are_current(&roots.global),
        "the reconciled root must carry this binary's stamp"
    );
}

/// An immutable global root cannot record the stamp, which is bookkeeping and
/// never a precondition for reading the catalog it describes.
#[cfg(unix)]
#[test]
fn denied_stamp_write_keeps_the_open_and_its_reads_working() {
    use std::os::unix::fs::PermissionsExt;

    let roots = roots();
    ensure_orbit_root_initialized(&roots.global, &roots.workspace).expect("first runtime open");
    fs::remove_file(stamp_path(&roots.global)).expect("drop the stamp");

    let resources_dir = roots.global.join("resources");
    fs::set_permissions(&resources_dir, fs::Permissions::from_mode(0o555))
        .expect("make the resources directory read-only");
    // A privileged test process ignores the mode bits, so there is no denial
    // left to observe; the readable-catalog assertions below still hold.
    let denial_is_observable = fs::write(resources_dir.join(".probe"), "").is_err();

    let opened = ensure_orbit_root_initialized(&roots.global, &roots.workspace);
    fs::set_permissions(&resources_dir, fs::Permissions::from_mode(0o755))
        .expect("restore the resources directory");

    opened.expect("a denied stamp write must not fail the open");
    assert!(
        fs::read_to_string(activities_dir(&roots.global).join("agent_implement.yaml")).is_ok(),
        "the managed catalog stays readable after a denied stamp write"
    );
    if denial_is_observable {
        assert!(
            !global_defaults_are_current(&roots.global),
            "an unrecorded stamp must leave the next open reconciling"
        );
    }
}

/// Bootstrap and composition read config at different scopes on purpose: the
/// scoreboard seed is a global-root decision, while everything the runtime
/// resolves layers the workspace over the global file. Skipping reconciliation
/// must not blur the two.
#[test]
fn global_bootstrap_scope_survives_a_workspace_config_override() {
    let roots = roots();
    fs::create_dir_all(&roots.global).expect("create global root");
    fs::create_dir_all(&roots.workspace).expect("create workspace root");
    fs::write(
        roots.global.join("config.toml"),
        "[scoring]\nenabled = false\n",
    )
    .expect("write global config");
    fs::write(
        roots.workspace.join("config.toml"),
        "[scoring]\nenabled = true\n",
    )
    .expect("write workspace config");

    let _env = orbit_common::test_env::scoped(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .copied()
            .map(|name| (name, None)),
    );

    for open in ["first", "warm"] {
        let runtime = OrbitRuntime::initialize_from_resolved_roots(
            OrbitRuntimeRoots {
                global_root: roots.global.clone(),
                shared_root: roots.workspace.clone(),
                local_root: roots.workspace.clone(),
            },
            None,
        )
        .unwrap_or_else(|error| panic!("{open} runtime open: {error}"));

        assert!(
            runtime.scoring_enabled(),
            "{open} open must resolve the workspace override over the global file"
        );
        drop(runtime);

        assert!(
            !roots
                .workspace
                .join("state/scoreboard/pr.json")
                .try_exists()
                .expect("probe scoreboard template"),
            "{open} open must decide scoreboard seeding from the global config alone"
        );
    }

    assert!(
        global_defaults_are_current(&roots.global),
        "the first open stamps the root the warm open then skips"
    );
}
