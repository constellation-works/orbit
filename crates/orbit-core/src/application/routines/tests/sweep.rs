use crate::application::routines::RoutineHostIdentity;
use crate::application::routines::clock::{ClockSettings, save_clock_settings};
use crate::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use crate::application::routines::sweep::{
    SweepOptions, configured_sweep_options, refresh_discovered_token_scoreboards,
    run_sweep_at_with_providers,
};
use chrono::Utc;
use orbit_automation::routines::loader::RoutineLoadError;
use orbit_common::OrbitError;
use orbit_store::InvocationInsertParams;
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
struct MustNotLoad;

impl RoutineWorkspaceProvider for MustNotLoad {
    fn discover_workspaces(&self, _global_root: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        panic!("workspace provider ran before the busy sweep lock returned")
    }
}

#[test]
fn busy_lock_returns_before_workspaces_are_discovered() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let state = global.join("state");
    let _held = orbit_store::try_acquire_routine_sweep_lock(&state)
        .expect("lock")
        .expect("first lock");

    let outcome = run_sweep_at_with_providers(
        &global,
        SweepOptions::default(),
        RoutineHostIdentity {
            machine_id: "hm_local".to_string(),
            host_id: "local".to_string(),
        },
        &MustNotLoad,
    )
    .expect("busy outcome");

    assert!(outcome.lock_busy);
    assert_eq!(outcome.machine_id, "hm_local");
    assert_eq!(outcome.host_id, "local");
}

#[test]
fn production_sweep_options_follow_the_host_clock_cadence() {
    let root = tempfile::tempdir().expect("root");

    let default_options = configured_sweep_options(root.path(), SweepOptions::default())
        .expect("default clock settings");
    assert_eq!(default_options.sweep_cadence_seconds, 60);

    save_clock_settings(
        root.path(),
        ClockSettings {
            cadence_seconds: 300,
        },
    )
    .expect("configured clock settings");
    let configured_options = configured_sweep_options(root.path(), SweepOptions::default())
        .expect("configured clock settings");
    assert_eq!(configured_options.sweep_cadence_seconds, 300);
}

#[test]
fn sweep_refreshes_token_scoreboard_for_each_discovered_workspace() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    let runtime = crate::OrbitRuntime::from_roots(&global, &workspace_root).expect("runtime");

    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "jrun-scoreboard".to_string(),
            activity_id: "implement".to_string(),
            agent: "codex".to_string(),
            model: Some("gpt-test".to_string()),
            task_ids: Vec::new(),
            trace: InvocationTrace {
                usage: TokenUsage {
                    input: 10,
                    ..TokenUsage::default()
                },
                ..InvocationTrace::default()
            },
        })
        .expect("persist invocation");

    refresh_discovered_token_scoreboards(&[(
        Workspace {
            id: "ws-scoreboard".to_string(),
            name: "scoreboard".to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "agent-main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        runtime,
    )]);

    let tokens = std::fs::read_to_string(workspace_root.join("state/scoreboard/tokens.json"))
        .expect("scoreboard refreshed during sweep");
    assert!(tokens.contains("codex"), "{tokens}");
}

struct ScriptedWorkspaces {
    result: DiscoveredWorkspaces,
}

impl RoutineWorkspaceProvider for ScriptedWorkspaces {
    fn discover_workspaces(&self, _global_root: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        Ok(DiscoveredWorkspaces {
            entries: Vec::new(),
            errors: self.result.errors.clone(),
        })
    }
}

fn host() -> RoutineHostIdentity {
    RoutineHostIdentity {
        machine_id: "hm_local".to_string(),
        host_id: "local".to_string(),
    }
}

fn prepared_global_root() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    let _runtime = crate::OrbitRuntime::from_roots(&global, &workspace_root).expect("runtime");
    (root, global)
}

fn load_error(name: &str, message: &str) -> RoutineLoadError {
    RoutineLoadError {
        source_workspace: name.to_string(),
        path: Some(PathBuf::from(format!("/tmp/{name}/.orbit"))),
        message: message.to_string(),
    }
}

fn capture_errors<F, T>(f: F) -> (T, String)
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
        .with_max_level(LevelFilter::ERROR)
        .with_ansi(false)
        .without_time()
        .finish();
    let result = tracing::subscriber::with_default(subscriber, f);
    let logs =
        String::from_utf8(buffer.lock().expect("capture buffer lock").clone()).expect("utf8 logs");
    (result, logs)
}

#[test]
fn every_workspace_load_error_sets_a_single_no_workspace_loaded_row() {
    let (_root, global) = prepared_global_root();
    let provider = ScriptedWorkspaces {
        result: DiscoveredWorkspaces {
            entries: Vec::new(),
            errors: vec![
                load_error(
                    "nebula",
                    "failed to open workspace runtime: schema migration failed",
                ),
                load_error(
                    "polaris",
                    "failed to open workspace runtime: schema migration failed",
                ),
            ],
        },
    };

    let (outcome, logs) = capture_errors(|| {
        run_sweep_at_with_providers(&global, SweepOptions::default(), host(), &provider)
            .expect("sweep outcome")
    });

    let row = outcome
        .no_workspace_loaded
        .as_deref()
        .expect("no_workspace_loaded row");
    assert!(row.contains("sweep.no_workspace_loaded"), "{row}");
    assert!(row.contains(env!("CARGO_PKG_VERSION")), "{row}");
    assert!(row.contains("nebula"), "{row}");
    assert!(row.contains("schema migration failed"), "{row}");
    assert_eq!(outcome.load_errors.len(), 2);
    assert!(
        logs.contains("sweep.no_workspace_loaded"),
        "expected a single tracing row, got: {logs}"
    );
    assert_eq!(
        logs.matches("sweep.no_workspace_loaded").count(),
        1,
        "{logs}"
    );
}

#[test]
fn partial_workspace_load_errors_do_not_fail_the_pass() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    let runtime = crate::OrbitRuntime::from_roots(&global, &workspace_root).expect("runtime");

    struct Partial {
        workspace: Workspace,
        runtime: crate::OrbitRuntime,
        errors: Vec<RoutineLoadError>,
    }
    impl RoutineWorkspaceProvider for Partial {
        fn discover_workspaces(
            &self,
            _global_root: &Path,
        ) -> Result<DiscoveredWorkspaces, OrbitError> {
            Ok(DiscoveredWorkspaces {
                entries: vec![(self.workspace.clone(), self.runtime.clone())],
                errors: self.errors.clone(),
            })
        }
    }

    let provider = Partial {
        workspace: Workspace {
            id: "ws-ok".to_string(),
            name: "ok".to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "agent-main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        runtime,
        errors: vec![load_error("broken", "failed to open workspace runtime")],
    };

    let (outcome, logs) = capture_errors(|| {
        run_sweep_at_with_providers(&global, SweepOptions::default(), host(), &provider)
            .expect("sweep outcome")
    });

    assert!(
        outcome.no_workspace_loaded.is_none(),
        "{:?}",
        outcome.no_workspace_loaded
    );
    assert_eq!(outcome.load_errors.len(), 1);
    assert!(
        !logs.contains("sweep.no_workspace_loaded"),
        "partial load errors must not emit the fail-loud row: {logs}"
    );
}

#[test]
fn unconfigured_host_does_not_report_no_workspace_loaded() {
    let (_root, global) = prepared_global_root();
    let provider = ScriptedWorkspaces {
        result: DiscoveredWorkspaces::default(),
    };

    let outcome = run_sweep_at_with_providers(&global, SweepOptions::default(), host(), &provider)
        .expect("sweep outcome");
    assert!(outcome.no_workspace_loaded.is_none());
    assert!(outcome.load_errors.is_empty());
}
