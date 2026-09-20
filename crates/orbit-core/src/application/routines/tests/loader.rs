//! Origin-aware routine loading tests [ORB-10258]: definitions under
//! `.orbit/routines/` and leftover ones under `.orbit/routines/local/` both
//! load and are evaluated on this host [ORB-12236]; a definition still
//! carrying the retired `hosts:` key loads with a warning that names its file;
//! and a name defined by more than one source fails deterministically naming
//! both sources.
//!
//! These drive `collect_routines` over a real seeded source workspace (the same
//! discovery path the sweep uses), so origin resolution, fail-before-dispatch,
//! and duplicate handling are exercised end-to-end rather than at the parser.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use tempfile::{TempDir, tempdir};

use crate::OrbitRuntime;
use crate::application::job::catalog::{
    DEFAULT_JOB_FILES, reset_v2_job_catalog_loads, v2_job_catalog_loads,
};
use crate::application::routine::RETIRED_ROUTINE_FILES;
use crate::application::routines::loader::{
    LoadedRoutine, RoutineCollection, RoutineOrigin, collect_routines,
};

const NOOP_JOB: &str = "schemaVersion: 2\n\
kind: Job\n\
metadata:\n  name: noop\n\
spec:\n  state: enabled\n  kind: workflow\n  max_active_runs: 1\n  \
steps:\n    - id: noop\n      target: activity:worktree_setup\n      \
default_input:\n        task_id: \"qa\"\n";

/// A seeded global root with one active source workspace `polaris`. Returns the
/// tempdir (kept alive by the caller), the global root, and the workspace's
/// `.orbit` dir so tests can drop routine files under `routines/` and
/// `routines/local/` before collecting.
struct SourceWorkspace {
    _tmp: TempDir,
    workspace: Workspace,
    runtime: OrbitRuntime,
    routines_dir: PathBuf,
    local_dir: PathBuf,
}

fn seed_source_workspace() -> SourceWorkspace {
    let tmp = tempdir().unwrap();
    let global = tmp.path().join("global");
    let ws_root = tmp.path().join("polaris");
    let ws_orbit = ws_root.join(".orbit");
    let routines_dir = ws_orbit.join("routines");
    let local_dir = routines_dir.join("local");
    fs::create_dir_all(global.join("state")).unwrap();
    fs::create_dir_all(&local_dir).unwrap();
    fs::create_dir_all(ws_orbit.join("resources/jobs")).unwrap();

    fs::write(ws_orbit.join("resources/jobs/noop.yaml"), NOOP_JOB).unwrap();

    let workspace = Workspace {
        id: "ws-1".to_string(),
        name: "polaris".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let runtime = OrbitRuntime::from_roots(&global, &ws_orbit).unwrap();

    SourceWorkspace {
        _tmp: tmp,
        workspace,
        runtime,
        routines_dir,
        local_dir,
    }
}

fn seed_shared_source_workspace() -> SourceWorkspace {
    let tmp = tempdir().unwrap();
    let shared_root = tmp.path().join("shared");
    let routines_dir = shared_root.join("routines");
    let local_dir = routines_dir.join("local");
    fs::create_dir_all(shared_root.join("state")).unwrap();
    fs::create_dir_all(&local_dir).unwrap();
    fs::create_dir_all(shared_root.join("resources/jobs")).unwrap();

    let workspace = Workspace {
        id: "ws-shared".to_string(),
        name: "polaris".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let runtime = OrbitRuntime::from_roots(&shared_root, &shared_root).unwrap();

    SourceWorkspace {
        _tmp: tmp,
        workspace,
        runtime,
        routines_dir,
        local_dir,
    }
}

fn write_routine(dir: &Path, file: &str, body: &str) {
    fs::write(dir.join(file), body).unwrap();
}

/// Collect through the same seam the sweep and status projections use.
fn collect(ws: &SourceWorkspace) -> RoutineCollection {
    collect_routines(&[(ws.workspace.clone(), ws.runtime.clone())])
}

fn find<'a>(collection: &'a RoutineCollection, name: &str) -> Option<&'a LoadedRoutine> {
    collection
        .routines
        .iter()
        .find(|r| r.definition.name == name)
}

fn definition(name: &str) -> String {
    format!(
        "schemaVersion: 1\nname: {name}\n\
         trigger: {{ cron: \"* * * * *\" }}\ntarget: job:noop\n"
    )
}

/// Capture WARN-level tracing emitted while `f` runs, so a deprecation the
/// operator must act on is asserted as output rather than as a return value.
fn capture_warnings<F, T>(f: F) -> (T, String)
where
    F: FnOnce() -> T,
{
    use std::io::{self, Write};
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct CaptureMakeWriter(Arc<Mutex<Vec<u8>>>);
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for CaptureMakeWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CaptureWriter(Arc::clone(&self.0))
        }
    }

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureMakeWriter(Arc::clone(&buffer)))
        .with_max_level(LevelFilter::WARN)
        .with_ansi(false)
        .without_time()
        .finish();
    let result = tracing::subscriber::with_default(subscriber, f);
    let logs =
        String::from_utf8(buffer.lock().expect("capture buffer lock").clone()).expect("utf8 logs");
    (result, logs)
}

// ---- workspace origin -----------------------------------------------------

#[test]
fn workspace_routine_loads_with_workspace_origin() {
    let ws = seed_source_workspace();
    write_routine(&ws.routines_dir, "nightly.yaml", &definition("nightly"));

    let collection = collect(&ws);
    let routine = find(&collection, "nightly").expect("workspace routine loads");
    assert_eq!(routine.origin, RoutineOrigin::Workspace);
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
}

#[test]
fn disabled_workspace_routine_still_loads() {
    let ws = seed_source_workspace();
    write_routine(
        &ws.routines_dir,
        "disabled.yaml",
        "schemaVersion: 1\nname: committed-disabled\nenabled: false\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:noop\n",
    );

    let collection = collect(&ws);
    let routine = find(&collection, "committed-disabled").expect("disabled routine loads");
    assert_eq!(routine.origin, RoutineOrigin::Workspace);
    assert!(!routine.definition.enabled);
}

/// [ORB-12236] `hosts:` is retired. A definition that still carries one is
/// loaded and evaluated by this host's clock; the warning names the file so
/// the key can be dropped before the next release rejects it.
#[test]
fn retired_host_pin_loads_with_a_warning_naming_the_file() {
    let ws = seed_source_workspace();
    write_routine(
        &ws.routines_dir,
        "pinned.yaml",
        "schemaVersion: 1\nname: committed-pinned\nhosts: [some-other-host]\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:noop\n",
    );

    let (collection, warnings) = capture_warnings(|| collect(&ws));
    let routine = find(&collection, "committed-pinned").expect("a pinned routine still loads");
    assert!(routine.definition.has_legacy_host_pin());
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
    assert!(warnings.contains("pinned.yaml"), "{warnings}");
    assert!(warnings.contains("hosts:"), "{warnings}");
}

// ---- local origin ---------------------------------------------------------

#[test]
fn local_routine_loads_offline_with_local_origin() {
    let ws = seed_source_workspace();
    // No registry cache, no network — discovery reads only the local registry.
    write_routine(&ws.local_dir, "personal.yaml", &definition("local-only"));

    let collection = collect(&ws);
    let routine = find(&collection, "local-only").expect("local routine loads");
    assert_eq!(routine.origin, RoutineOrigin::Local);
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
}

// ---- cross-origin duplicate names -----------------------------------------

#[test]
fn duplicate_name_across_workspace_and_local_fails_deterministically() {
    let ws = seed_source_workspace();
    write_routine(&ws.routines_dir, "dup.yaml", &definition("dup-name"));
    write_routine(&ws.local_dir, "dup.yaml", &definition("dup-name"));

    let collection = collect(&ws);
    // Neither definition may silently shadow the other: both are dropped.
    assert!(
        find(&collection, "dup-name").is_none(),
        "a cross-origin name collision drops every colliding definition"
    );
    let collision_errors: Vec<&str> = collection
        .errors
        .iter()
        .filter(|e| e.message.contains("dup-name"))
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(
        collision_errors.len(),
        2,
        "one error row per colliding definition: {collision_errors:?}"
    );
    // Both origins are reported in each row so the conflict is diagnosable.
    for message in &collision_errors {
        assert!(
            message.contains("workspace origin") && message.contains("local origin"),
            "collision names both sources: {message}"
        );
    }
}

// ---- retired targets ------------------------------------------------------

/// A routine targeting a job a prior release shipped and this one dropped is
/// dead weight until `orbit workspace sync` retires it — not a broken
/// definition. It loads as retired so the clock tick stops logging the same
/// load error on every pass [DANI-10392].
#[test]
fn routine_targeting_a_retired_job_is_skipped_not_failed() {
    let ws = seed_source_workspace();
    write_routine(
        &ws.routines_dir,
        "auto_task_scheduler.yaml",
        "schemaVersion: 1\nname: auto-task-scheduler-polaris\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:auto_task_scheduler_pipeline\n",
    );

    let collection = collect(&ws);
    assert!(
        collection.errors.is_empty(),
        "a retired target must not be a load error: {:?}",
        collection
            .errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
    );
    assert!(find(&collection, "auto-task-scheduler-polaris").is_none());
    let retired = collection
        .retired
        .iter()
        .find(|routine| routine.name == "auto-task-scheduler-polaris")
        .expect("the definition is reported as retired");
    assert_eq!(retired.job, "auto_task_scheduler_pipeline");
    assert_eq!(retired.origin, RoutineOrigin::Workspace);
    assert_eq!(retired.source_workspace, "polaris");
    assert_eq!(
        retired.path,
        ws.routines_dir.join("auto_task_scheduler.yaml")
    );
    // This definition is the operator's own, so synchronization preserves it:
    // advising the sync would send them back to a command that reports
    // `unchanged` forever [DANI-10502].
    assert!(
        !retired.reason.contains("orbit workspace sync"),
        "sync does not retire a definition Orbit did not write: {}",
        retired.reason
    );
    assert!(
        retired.reason.contains("delete")
            && retired.reason.contains(
                &ws.routines_dir
                    .join("auto_task_scheduler.yaml")
                    .display()
                    .to_string()
            ),
        "the reason names a step that changes something, and the file it applies to: {}",
        retired.reason
    );
}

/// The other half of the same rule: a copy of the template the release that
/// retired it shipped *is* Orbit's file, and `orbit workspace sync` retires it
/// by content even with no manifest entry — so naming the sync is true, and
/// the operator is told the one command that clears it [DANI-10502].
#[test]
fn orbit_seeded_retired_default_keeps_the_sync_advice() {
    let ws = seed_source_workspace();
    write_routine(
        &ws.routines_dir,
        "auto_task_scheduler.yaml",
        &retired_scheduler_template("auto-task-scheduler-polaris"),
    );

    let collection = collect(&ws);
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
    let retired = collection
        .retired
        .iter()
        .find(|routine| routine.name == "auto-task-scheduler-polaris")
        .expect("the definition is reported as retired");
    assert!(
        retired.reason.contains("orbit workspace sync"),
        "the reason names the command that retires the file: {}",
        retired.reason
    );
}

/// The retired scheduler template as the release that last shipped it wrote
/// it for `name`.
fn retired_scheduler_template(name: &str) -> String {
    RETIRED_ROUTINE_FILES
        .iter()
        .find(|(stem, _)| *stem == "auto_task_scheduler")
        .map(|(_, template)| template.replace("__ORBIT_ROUTINE_NAME__", name))
        .expect("the retired scheduler ships as a provenance shape")
}

/// The catalog still wins: a workspace that defines a job of the retired
/// name itself keeps an ordinary, evaluable routine.
#[test]
fn workspace_defined_job_of_a_retired_name_still_loads_normally() {
    let ws = seed_source_workspace();
    fs::write(
        ws.routines_dir
            .parent()
            .expect("routines dir has a parent")
            .join("resources/jobs/auto_task_scheduler_pipeline.yaml"),
        NOOP_JOB.replace("name: noop", "name: auto_task_scheduler_pipeline"),
    )
    .unwrap();
    write_routine(
        &ws.routines_dir,
        "auto_task_scheduler.yaml",
        "schemaVersion: 1\nname: auto-task-scheduler-polaris\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:auto_task_scheduler_pipeline\n",
    );

    let collection = collect(&ws);
    assert!(collection.errors.is_empty());
    assert!(
        collection.retired.is_empty(),
        "a job the workspace still defines is not retired"
    );
    assert!(find(&collection, "auto-task-scheduler-polaris").is_some());
}

/// An unresolvable target that is *not* a known retired job is still a
/// fail-closed load error (ADR-0206).
#[test]
fn unknown_target_is_still_a_load_error() {
    let ws = seed_source_workspace();
    write_routine(
        &ws.routines_dir,
        "typo.yaml",
        "schemaVersion: 1\nname: typo-polaris\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:no_such_pipeline\n",
    );

    let collection = collect(&ws);
    assert!(collection.retired.is_empty());
    assert!(
        collection.errors.iter().any(|error| error
            .message
            .contains("does not resolve in workspace 'polaris'")),
        "{:?}",
        collection
            .errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
    );
}

// ---- catalog parse cost ---------------------------------------------------

/// Membership checks must not re-parse every job YAML per routine definition.
#[test]
fn collect_routines_parses_each_workspace_catalog_once() {
    let ws = seed_source_workspace();
    for name in ["alpha", "beta", "gamma"] {
        write_routine(&ws.routines_dir, &format!("{name}.yaml"), &definition(name));
    }

    reset_v2_job_catalog_loads();
    let collection = collect(&ws);
    assert_eq!(
        v2_job_catalog_loads(),
        1,
        "one catalog parse per workspace, not per routine"
    );
    assert_eq!(collection.routines.len(), 3);
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
}

#[test]
fn collect_routines_parses_one_catalog_per_workspace() {
    let first = seed_source_workspace();
    let second = seed_source_workspace();
    write_routine(&first.routines_dir, "one.yaml", &definition("one"));
    write_routine(&second.routines_dir, "two.yaml", &definition("two"));
    write_routine(&second.routines_dir, "three.yaml", &definition("three"));

    reset_v2_job_catalog_loads();
    let collection = collect_routines(&[
        (first.workspace.clone(), first.runtime.clone()),
        (second.workspace.clone(), second.runtime.clone()),
    ]);
    assert_eq!(
        v2_job_catalog_loads(),
        2,
        "each workspace catalog is parsed once even when a workspace has several routines"
    );
    assert_eq!(collection.routines.len(), 3);
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
}

#[test]
fn default_routine_survives_a_distinct_workspace_job_catalog_error() {
    let ws = seed_source_workspace();
    let default_yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_auto_pipeline").then_some(*yaml))
        .expect("task auto pipeline default exists");
    let global_jobs_dir = ws.runtime.global_root().join("resources/jobs");
    fs::create_dir_all(&global_jobs_dir).expect("create global jobs directory");
    fs::write(
        global_jobs_dir.join("task_auto_pipeline.yaml"),
        default_yaml,
    )
    .expect("seed global default job");

    let malformed = ws
        .runtime
        .shared_root()
        .join("resources/jobs/malformed.yaml");
    fs::write(&malformed, "schemaVersion: 2\nkind: Job\nspec: [")
        .expect("write malformed workspace job");
    write_routine(
        &ws.routines_dir,
        "default-target.yaml",
        "schemaVersion: 1\nname: default-target-polaris\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:task_auto_pipeline\n",
    );

    let names = ws
        .runtime
        .load_v2_job_execution_names()
        .expect("execution names remain available");
    assert!(names.contains("task_auto_pipeline"), "{names:?}");
    assert!(
        ws.runtime
            .load_v2_job_asset_by_name("task_auto_pipeline")
            .is_ok(),
        "the default still resolves through named execution"
    );

    let collection = collect(&ws);
    assert!(
        find(&collection, "default-target-polaris").is_some(),
        "a routine targeting the healthy global default should load"
    );
    let messages = collection
        .errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        messages.len(),
        1,
        "one malformed catalog file yields one diagnostic"
    );
    assert!(
        messages.iter().any(|message| {
            message.contains("failed to load job catalog") && message.contains("malformed.yaml")
        }),
        "catalog failure should name the bad file: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.contains("no such job in its catalog")),
        "a catalog failure must not be reported as missing default job: {messages:?}"
    );
}

#[test]
fn routine_targeting_healthy_job_survives_malformed_job_in_shared_root() {
    let ws = seed_shared_source_workspace();
    let default_yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_auto_pipeline").then_some(*yaml))
        .expect("task auto pipeline default exists");
    let jobs_dir = ws.runtime.shared_root().join("resources/jobs");
    fs::write(jobs_dir.join("task_auto_pipeline.yaml"), default_yaml)
        .expect("seed shared default job");
    fs::write(
        jobs_dir.join("malformed.yaml"),
        "schemaVersion: 2\nkind: Job\nspec: [",
    )
    .expect("write malformed shared job");
    write_routine(
        &ws.routines_dir,
        "default-target.yaml",
        "schemaVersion: 1\nname: default-target-polaris\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:task_auto_pipeline\n",
    );

    let collection = collect(&ws);
    assert!(
        find(&collection, "default-target-polaris").is_some(),
        "healthy shared job target should keep its routine: {:?}",
        collection
            .routines
            .iter()
            .map(|routine| &routine.definition.name)
            .collect::<Vec<_>>()
    );
    let messages = collection
        .errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        messages.len(),
        1,
        "one malformed catalog file yields one diagnostic"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("malformed.yaml")),
        "shared catalog error should name the bad file: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.contains("no such job in its catalog")),
        "healthy target must not be reported missing: {messages:?}"
    );
}
