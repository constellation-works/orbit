//! Shared child fixture for scenario tables. Each table gets an isolated
//! process; each row gets a fresh HOME/cwd and restored environment.
//! A failed row names both its table and its original scenario identity.

use std::process::Command;

type Scenario = (&'static str, fn());
const ROW: &str = "ORBIT_RUNTIME_SCENARIO_ROW";

pub(super) fn run(module: &str, table: &str, rows: &[Scenario]) {
    let module = module.split_once("::").expect("test module").1;
    let qualified = format!("{module}::{table}");
    if let Ok(selected) = std::env::var(ROW) {
        if selected == "*" {
            for (name, scenario) in rows {
                let _home = RowHome::new(module, name);
                // Report the active row even if its assertion panics.
                use std::io::Write;
                writeln!(std::io::stderr(), "scenario: {module}::{name}").expect("row identity");
                let started = std::time::Instant::now();
                scenario();
                report_elapsed(&format!("{module}::{name}"), "row", started);
                orbit_common::test_env::record_scenario_pass(name);
            }
            return;
        }
        let (_, scenario) = rows
            .iter()
            .find(|(name, _)| *name == selected)
            .expect("the selected row belongs to this table");
        let started = std::time::Instant::now();
        scenario();
        report_elapsed(&format!("{module}::{selected}"), "row", started);
        orbit_common::test_env::record_scenario_pass(&selected);
        return;
    }

    // These families install process-wide worker substitutions. Keep their
    // rows in distinct children so a substitution cannot reach a later row.
    let separate = [
        "distributed_drain::before_landing",
        "distributed_drain::claimed_review",
        "distributed_drain::claimed_owed_evidence",
        "distributed_drain::single_pass",
        "distributed_drain::landing_repair",
        "distributed_drain::desktop_completion",
        "review_evidence_fulfilment",
        "baseline_hold_tick",
        "task_pilot::crew_selection",
        "provider_limit_admission",
        "task_delivery",
    ]
    .contains(&module);
    let selected = if separate {
        rows.iter().map(|(name, _)| *name).collect::<Vec<_>>()
    } else {
        vec!["*"]
    };
    for name in selected {
        let home = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
            .expect("scenario home");
        let logs = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
            .expect("scenario logs");
        let exact_row = format!("{module}::{name}");
        let path = std::env::join_paths(std::iter::once(home.path().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .expect("scenario PATH");
        let started = std::time::Instant::now();
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", &qualified, "--nocapture", "--test-threads=1"])
            .env(ROW, name)
            // Existing fixture constructors enforce child-only use.
            .env("ORBIT_TEST_TASK_FIXTURE_CHILD", &exact_row)
            .env("ORBIT_TEST_PLUGIN_FIXTURE_CHILD", &exact_row)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("PATH", path)
            .current_dir(home.path());
        let output = orbit_common::test_env::run_child_test(&mut command, &exact_row, logs.path());
        orbit_common::test_env::assert_child_test_passed(
            &exact_row,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        // This bypasses libtest capture. Keep child reports owned by their
        // outer case, rather than interleaving them with outer suite results.
        // One stdout lock keeps another case from splitting the frame.
        use std::io::Write;
        {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "qa-child-output-begin: {qualified}").expect("row owner");
            stdout.write_all(&output.stdout).expect("row stdout");
            writeln!(stdout, "\nqa-child-output-end: {qualified}").expect("row end");
        }
        // Preserve host-sandbox deferrals for the affected gate's parser.
        std::io::stderr()
            .write_all(&output.stderr)
            .expect("row stderr");
        // Process startup is the gap between this and the child's row lines.
        report_elapsed(&exact_row, "child", started);
    }
}

/// Print a row's elapsed time so a run's output profiles its fixture cost.
fn report_elapsed(row: &str, scope: &str, started: std::time::Instant) {
    use std::io::Write;
    let elapsed = started.elapsed().as_secs_f64();
    writeln!(
        std::io::stderr(),
        "scenario: {row} {scope} elapsed {elapsed:.3}s"
    )
    .expect("row timing");
}

/// A row owns all runtimes it creates. On return/unwind, restore the child's
/// complete environment and cwd before the next row starts, including variables
/// changed directly by an older fixture rather than through a ScopedEnv guard.
struct RowHome {
    environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    cwd: std::path::PathBuf,
    _home: tempfile::TempDir,
}

impl RowHome {
    fn new(module: &str, name: &str) -> Self {
        let environment = std::env::vars_os().collect();
        let cwd = std::env::current_dir().expect("child cwd");
        let home =
            tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).expect("row home");
        let path = std::env::join_paths(std::iter::once(home.path().join("bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .expect("row PATH");
        // SAFETY: only the selected table runs in this --test-threads=1 child;
        // rows run sequentially and own/drop their fixture work before returning.
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("USERPROFILE", home.path());
            std::env::set_var("PATH", path);
            let exact = format!("{module}::{name}");
            std::env::set_var("ORBIT_TEST_TASK_FIXTURE_CHILD", &exact);
            std::env::set_var("ORBIT_TEST_PLUGIN_FIXTURE_CHILD", &exact);
        }
        std::env::set_current_dir(home.path()).expect("row cwd");
        Self {
            environment,
            cwd,
            _home: home,
        }
    }
}

impl Drop for RowHome {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.cwd).expect("restore child cwd");
        // SAFETY: same single-table, sequential-row child as in new(). The row's
        // local guards and runtimes have dropped before this guard restores state.
        unsafe {
            for (key, _) in std::env::vars_os() {
                std::env::remove_var(key);
            }
            for (key, value) in &self.environment {
                std::env::set_var(key, value);
            }
        }
    }
}

/// Opt in only when a fixture tests persisted/public outcomes rather than
/// polling cadence. The real monotonic deadline and all process work remain.
pub(super) fn with_prompt_pipeline_polls<T>(operation: impl FnOnce() -> T) -> T {
    orbit_core::test_support::with_pipeline_poll_wait(
        |duration| std::thread::sleep(duration.min(std::time::Duration::from_millis(10))),
        operation,
    )
}

/// Skip only the sampling delay for fixtures whose proc readings never change.
/// The public forecast/admission path still reads those files, derives CPU
/// unknown from zero tick deltas, and executes real reservation transitions.
/// Cadence, changing proc data and native proc fixtures must keep the system wait.
pub(super) fn with_immutable_host_load<T>(operation: impl FnOnce() -> T) -> T {
    orbit_core::test_support::with_host_load_sampling_wait(|_| {}, operation)
}
