//! Origin-aware routine loading tests [ORB-10258]: committed definitions under
//! `.orbit/routines/` and machine-local ones under `.orbit/routines/local/`
//! both load and are evaluated on this host [ORB-12236]; a definition still
//! carrying the retired `hosts:` key loads with a warning that names its file;
//! and a name defined by more than one origin fails deterministically naming
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

// ---- committed origin -----------------------------------------------------

#[test]
fn committed_routine_loads_with_committed_origin() {
    let ws = seed_source_workspace();
    write_routine(&ws.routines_dir, "nightly.yaml", &definition("committed"));

    let collection = collect(&ws);
    let routine = find(&collection, "committed").expect("committed routine loads");
    assert_eq!(routine.origin, RoutineOrigin::Committed);
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
}

#[test]
fn disabled_committed_routine_still_loads() {
    let ws = seed_source_workspace();
    write_routine(
        &ws.routines_dir,
        "disabled.yaml",
        "schemaVersion: 1\nname: committed-disabled\nenabled: false\n\
         trigger: { cron: \"* * * * *\" }\ntarget: job:noop\n",
    );

    let collection = collect(&ws);
    let routine = find(&collection, "committed-disabled").expect("disabled routine loads");
    assert_eq!(routine.origin, RoutineOrigin::Committed);
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
fn duplicate_name_across_committed_and_local_fails_deterministically() {
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
            message.contains("committed origin") && message.contains("local origin"),
            "collision names both sources: {message}"
        );
    }
}
