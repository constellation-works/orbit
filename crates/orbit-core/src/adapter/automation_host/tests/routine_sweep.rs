use crate::OrbitRuntime;
use crate::application::job::JobRunListParams;
use crate::application::job::pipeline::worker_command_override;
use chrono::{Duration, TimeZone, Utc};
use orbit_automation::auto_tasks::AutoTaskAddParams;
use orbit_automation::routines::RoutineHostIdentity;
use orbit_automation::routines::clock::{ClockSettings, save_clock_settings};
use orbit_automation::routines::loader::{
    DiscoveredWorkspaces, RoutineLoadError, RoutineWorkspaceProvider,
};
use orbit_automation::routines::sweep::SweepOptions;
use orbit_automation::routines::tick::{
    configured_sweep_options, refresh_discovered_token_scoreboards, run_sweep_at_with_providers,
    run_sweep_at_with_providers_at,
};
use orbit_common::OrbitError;
use orbit_store::InvocationInsertParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct MustNotLoad;

const NOOP_JOB: &str = "schemaVersion: 2\n\
kind: Job\n\
metadata:\n  name: noop\n\
spec:\n  state: enabled\n  kind: workflow\n  max_active_runs: 1\n  \
steps:\n    - id: noop\n      target: activity:worktree_setup\n      \
default_input:\n        task_id: qa\n";

impl RoutineWorkspaceProvider for MustNotLoad {
    type Host = OrbitRuntime;

    fn discover_workspaces(
        &self,
        _global_root: &Path,
    ) -> Result<DiscoveredWorkspaces<OrbitRuntime>, OrbitError> {
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
    result: DiscoveredWorkspaces<OrbitRuntime>,
}

impl RoutineWorkspaceProvider for ScriptedWorkspaces {
    type Host = OrbitRuntime;

    fn discover_workspaces(
        &self,
        _global_root: &Path,
    ) -> Result<DiscoveredWorkspaces<OrbitRuntime>, OrbitError> {
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

fn workspace(id: &str, name: &str) -> Workspace {
    Workspace {
        id: id.to_string(),
        name: name.to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn auto_task(name: &str) -> AutoTaskAddParams {
    AutoTaskAddParams {
        name: name.to_string(),
        description: format!("Auto-task {name}"),
        schedule: AutoTaskSchedule::Interval { every_minutes: 1 },
        template: AutoTaskTemplate {
            title: format!("Chore for {name}"),
            description: "Recurring chore body.".to_string(),
            acceptance_criteria: vec!["Chore is observable.".to_string()],
            task_type: TaskType::Chore,
            tags: Vec::new(),
            required_tools: Vec::new(),
            priority: TaskPriority::Medium,
            crew: None,
            status: TaskStatus::Backlog,
        },
        dedupe: DedupePolicy::Always,
    }
}

struct FixedWorkspaces {
    entries: Vec<(Workspace, crate::OrbitRuntime)>,
}

impl RoutineWorkspaceProvider for FixedWorkspaces {
    type Host = OrbitRuntime;

    fn discover_workspaces(
        &self,
        _global_root: &Path,
    ) -> Result<DiscoveredWorkspaces<OrbitRuntime>, OrbitError> {
        Ok(DiscoveredWorkspaces {
            entries: self.entries.clone(),
            errors: Vec::new(),
        })
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
        type Host = OrbitRuntime;

        fn discover_workspaces(
            &self,
            _global_root: &Path,
        ) -> Result<DiscoveredWorkspaces<OrbitRuntime>, OrbitError> {
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

#[test]
fn tick_mints_due_auto_task_without_creating_a_job_run_and_dry_run_is_inert() {
    let _tz = orbit_common::test_env::unset(["TZ"]);
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let orbit_dir = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&orbit_dir).expect("workspace root");
    let runtime = crate::OrbitRuntime::from_roots(&global, &orbit_dir).expect("runtime");
    runtime
        .auto_task_add(auto_task("chore"))
        .expect("auto-task");
    let provider = FixedWorkspaces {
        entries: vec![(workspace("ws-one", "one"), runtime.clone())],
    };
    let first = Utc
        .with_ymd_and_hms(2026, 9, 12, 7, 0, 0)
        .single()
        .expect("time");

    let dry_baseline = run_sweep_at_with_providers_at(
        &global,
        SweepOptions {
            dry_run: true,
            ..SweepOptions::default()
        },
        host(),
        &provider,
        first,
    )
    .expect("dry tick");
    assert_eq!(dry_baseline.auto_task_reports[0].action, "would_baseline");
    assert!(!orbit_automation::auto_tasks::cursor_state_path(&runtime.paths().state_dir).exists());

    run_sweep_at_with_providers_at(&global, SweepOptions::default(), host(), &provider, first)
        .expect("baseline tick");
    let cursor_before = std::fs::read(orbit_automation::auto_tasks::cursor_state_path(
        &runtime.paths().state_dir,
    ))
    .expect("baseline cursor");
    let dry_due = run_sweep_at_with_providers_at(
        &global,
        SweepOptions {
            dry_run: true,
            ..SweepOptions::default()
        },
        host(),
        &provider,
        first + Duration::minutes(2),
    )
    .expect("dry due tick");
    assert_eq!(dry_due.auto_task_reports[0].action, "would_fire");
    assert!(runtime.list_tasks().expect("tasks").is_empty());
    assert_eq!(
        std::fs::read(orbit_automation::auto_tasks::cursor_state_path(
            &runtime.paths().state_dir,
        ))
        .expect("cursor after dry-run"),
        cursor_before
    );

    let due = run_sweep_at_with_providers_at(
        &global,
        SweepOptions::default(),
        host(),
        &provider,
        first + Duration::minutes(2),
    )
    .expect("due tick");
    assert_eq!(due.auto_task_reports[0].action, "minted");
    assert!(due.auto_task_reports[0].task_id.is_some());
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 1);
    assert!(
        runtime
            .list_job_runs(JobRunListParams::default())
            .expect("job runs")
            .is_empty(),
        "auto-task evaluation must not create a jrun"
    );
    assert_ne!(
        std::fs::read(orbit_automation::auto_tasks::cursor_state_path(
            &runtime.paths().state_dir,
        ))
        .expect("advanced cursor"),
        cursor_before
    );
}

#[test]
fn one_tick_fires_a_routine_and_auto_task_and_isolates_another_workspace_error() {
    let _tz = orbit_common::test_env::unset(["TZ"]);
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let healthy_dir = root.path().join("healthy/.orbit");
    let broken_dir = root.path().join("broken/.orbit");
    for orbit_dir in [&healthy_dir, &broken_dir] {
        std::fs::create_dir_all(orbit_dir.join("routines")).expect("routines dir");
        std::fs::create_dir_all(orbit_dir.join("auto_tasks")).expect("auto-tasks dir");
    }
    std::fs::create_dir_all(global.join("resources/jobs")).expect("global jobs dir");
    std::fs::write(global.join("resources/jobs/noop.yaml"), NOOP_JOB).expect("job");
    std::fs::write(
        healthy_dir.join("routines/minutely.yaml"),
        "schemaVersion: 1\nname: minutely\nenabled: true\ntrigger:\n  cron: '* * * * *'\ntarget: job:noop\n",
    )
    .expect("routine");
    let healthy = crate::OrbitRuntime::from_roots(&global, &healthy_dir).expect("healthy runtime");
    let broken = crate::OrbitRuntime::from_roots(&global, &broken_dir).expect("broken runtime");
    healthy
        .auto_task_add(auto_task("healthy-chore"))
        .expect("auto-task");
    broken
        .auto_task_add(auto_task("broken-chore"))
        .expect("auto-task baseline");
    let provider = FixedWorkspaces {
        entries: vec![
            (workspace("ws-broken", "broken"), broken.clone()),
            (workspace("ws-healthy", "healthy"), healthy.clone()),
        ],
    };
    let first = Utc
        .with_ymd_and_hms(2026, 9, 12, 7, 0, 0)
        .single()
        .expect("time");
    run_sweep_at_with_providers_at(&global, SweepOptions::default(), host(), &provider, first)
        .expect("baseline tick");
    std::fs::write(
        broken_dir.join("auto_tasks/broken-chore.yaml"),
        "not: [valid",
    )
    .expect("break one workspace definition");

    worker_command_override::set(["sh", "-c", "true"]);
    let outcome = run_sweep_at_with_providers_at(
        &global,
        SweepOptions::default(),
        host(),
        &provider,
        first + Duration::minutes(2),
    )
    .expect("combined tick");
    worker_command_override::clear();

    assert!(
        outcome.reports.iter().any(|row| row.action == "fired"),
        "{:?}",
        outcome.reports
    );
    assert!(
        outcome
            .auto_task_reports
            .iter()
            .any(|row| { row.source == "broken" && row.action == "error" })
    );
    assert!(
        outcome.auto_task_reports.iter().any(|row| {
            row.source == "healthy" && row.action == "minted" && row.task_id.is_some()
        })
    );
    assert_eq!(healthy.list_tasks().expect("healthy tasks").len(), 1);
    assert!(broken.list_tasks().expect("broken tasks").is_empty());
    assert_eq!(
        healthy
            .list_job_runs(JobRunListParams::default())
            .expect("routine run")
            .len(),
        1,
        "the only jrun belongs to the due routine"
    );
}
