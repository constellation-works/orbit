use super::log::pipeline_worker_file_name;
use super::*;

/// Return a stable path suitable for launching a fresh worker process.
///
/// Linux exposes a process whose executable inode was unlinked as
/// `/installed/path (deleted)`. That pseudo-path cannot be executed, but after
/// an atomic upgrade the original installed path names the replacement binary.
/// Preserve ordinary paths, including real filenames ending in ` (deleted)`.
pub(crate) fn resolve_pipeline_worker_executable(current_exe: PathBuf) -> PathBuf {
    // L-0084: deleted Linux executable paths must resolve through the installed replacement.
    #[cfg(target_os = "linux")]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let current_path_is_missing = matches!(
            std::fs::metadata(&current_exe),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        if current_path_is_missing
            && let Some(installed_path) = current_exe
                .as_os_str()
                .as_bytes()
                .strip_suffix(b" (deleted)")
        {
            return PathBuf::from(OsString::from_vec(installed_path.to_vec()));
        }
    }

    current_exe
}

/// Forward `--root` only when the parent runtime is pinned to one directory
/// (`global_dir == orbit_dir`). That is the `--root` flag's contract: it pins
/// both the workspace and the global store. The default split-root layout
/// (`$HOME/.orbit` vs workspace `.orbit`) must keep this `None` — an explicit
/// `--root` would pin *both* roots and disconnect the worker from the global
/// registry database that contains the persisted run.
pub(crate) fn pipeline_worker_root_override(paths: &WorkspacePaths) -> Option<&Path> {
    (paths.global_dir == paths.orbit_dir).then_some(paths.global_dir.as_path())
}

pub(crate) fn configure_pipeline_worker_command(
    command: &mut Command,
    workspace: &Path,
    run_id: &str,
    root_override: Option<&Path>,
) {
    // [ORB-11998] `resolve_roots` prefers an `ORBIT_ROOT` env value over cwd
    // walk-up, so an inherited value — from the sweep clock's own service
    // environment, an operator's shell, or any other ambient source — would
    // silently redirect this worker to a different registered workspace than
    // the one `current_dir` below pins it to. Every worker gets an explicit
    // workspace identity, either via `--root` (pinned parent) or cwd (default
    // split-root layout), so `ORBIT_ROOT` must never be left to compete with
    // either.
    command.env_remove("ORBIT_ROOT");
    if let Some(root) = root_override {
        // `--root` pins both stores.
        command.arg("--root").arg(root);
    }
    command
        .arg("job")
        .arg("run-pipeline-worker")
        .arg(run_id)
        .current_dir(workspace)
        .stdin(Stdio::null());
}

/// Give a detached worker its own coverage dump path when the parent inherited
/// `LLVM_PROFILE_FILE` (cargo-llvm-cov / instrumented CI).
///
/// The child is the same instrumented `orbit` binary. Sharing the parent's
/// profile file — or following a relative `LLVM_PROFILE_FILE` after cwd is
/// moved to the registered workspace — can stall or abort in CRT init, before
/// `main`, so the run never gets a PID and the worker log stays empty.
pub(crate) fn pipeline_worker_profile_file(
    logs_dir: &Path,
    run_id: &str,
    inherited: Option<&OsStr>,
) -> Result<Option<PathBuf>, OrbitError> {
    let Some(_) = inherited.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    Ok(Some(
        logs_dir.join(pipeline_worker_file_name(run_id, ".%p.profraw")?),
    ))
}

/// Where a submitted run's pinned job definition lives.
pub(crate) fn run_definition_snapshot_path(
    job_runs_dir: &Path,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    Ok(job_runs_dir.join(pipeline_worker_file_name(run_id, ".job.yaml")?))
}

/// The durable input one workspace drain carries for its whole window.
///
/// Every key here is *omitted* unless the caller asked for it, so a run's
/// persisted input records only the deviations from the job's own defaults —
/// which is what makes an omitted option indistinguishable from the behavior
/// that predated it. Pure, so the durable contract this shape represents can
/// be asserted without submitting a run.
pub(crate) fn workspace_auto_run_input(
    for_seconds: Option<u64>,
    max_active_leaf_runs: Option<u32>,
    completion: crate::application::workflow::CompletionPolicy,
    allowed_crews: &[String],
) -> Result<Value, OrbitError> {
    if max_active_leaf_runs == Some(0) {
        return Err(OrbitError::InvalidInput(
            "concurrency must be at least 1".to_string(),
        ));
    }
    let mut input = serde_json::Map::new();
    input.insert(
        "for_seconds".to_string(),
        json!(for_seconds.unwrap_or_default()),
    );
    // [ORB-11187] Blanket authorization: the drain re-lists the backlog every
    // pass, so this policy governs every task admitted for the whole window,
    // not only the ones visible at submission.
    if completion.completes() {
        input.insert(
            "completion".to_string(),
            Value::String(completion.as_input_value().to_string()),
        );
    }
    if let Some(max_active_leaf_runs) = max_active_leaf_runs {
        input.insert(
            "max_active_leaf_runs".to_string(),
            json!(max_active_leaf_runs),
        );
    }
    // [ORB-11242] Carried by the run itself, so every pipeline it admits
    // inherits the same window without re-deriving it from configuration.
    if !allowed_crews.is_empty() {
        input.insert("allowed_crews".to_string(), json!(allowed_crews));
    }
    Ok(Value::Object(input))
}

/// Test-only substitute for the detached worker program.
///
/// Production re-execs `current_exe` at `job run-pipeline-worker <run_id>`. A
/// test binary must never re-exec itself: libtest reads the worker argv as test
/// filters and recurses through the whole suite. In-crate tests install a small
/// script here instead and assert on the submission path around it.
#[cfg(test)]
pub(crate) mod worker_command_override {
    use std::cell::RefCell;
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// Replaced with the submitted run id in every argv entry.
    pub(crate) const RUN_ID_PLACEHOLDER: &str = "{run_id}";

    thread_local! {
        static ARGV: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    /// Install `argv` as this thread's worker program until [`clear`].
    pub(crate) fn set<I, S>(argv: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let argv = argv.into_iter().map(Into::into).collect::<Vec<_>>();
        ARGV.with(|slot| *slot.borrow_mut() = Some(argv));
    }

    pub(crate) fn clear() {
        ARGV.with(|slot| *slot.borrow_mut() = None);
    }

    pub(crate) fn command(workspace: &Path, run_id: &str) -> Option<Command> {
        let argv = ARGV.with(|slot| slot.borrow().clone())?;
        let mut parts = argv
            .iter()
            .map(|part| part.replace(RUN_ID_PLACEHOLDER, run_id));
        let program = parts.next()?;
        let mut command = Command::new(program);
        command
            .args(parts)
            .current_dir(workspace)
            .stdin(Stdio::null());
        Some(command)
    }
}

/// How this workspace launches a detached worker process.
///
/// Production re-execs this same `orbit` binary at the hidden worker
/// subcommand; workspace context comes from the child's cwd. The one piece of
/// launch policy that cannot be derived at the call site is whether the parent
/// runtime was pinned to a single root, so this carries it.
#[derive(Clone, Debug)]
pub(crate) struct WorkerCommandConfig {
    /// `--root` to forward to the worker, so the child opens the same global
    /// store the parent used to persist the run [ORB-10821]. `None` for the
    /// default split-root layout; see [`pipeline_worker_root_override`].
    root_override: Option<PathBuf>,
}

impl WorkerCommandConfig {
    pub(crate) fn for_paths(paths: &WorkspacePaths) -> Self {
        Self {
            root_override: pipeline_worker_root_override(paths).map(Path::to_path_buf),
        }
    }

    /// The command that runs `run_id`'s worker from `workspace`.
    pub(crate) fn build(&self, workspace: &Path, run_id: &str) -> Result<Command, OrbitError> {
        #[cfg(test)]
        {
            // A test binary must never re-exec itself, so an in-crate test
            // substitutes its own program and there is no `orbit` invocation
            // left to forward the pinned root to.
            let _pinned_root = self.root_override.as_deref();
            worker_command_override::command(workspace, run_id).ok_or_else(|| {
                OrbitError::Execution(
                    "test pipeline worker requires an explicit worker command override".to_string(),
                )
            })
        }

        #[cfg(not(test))]
        {
            let current_exe = std::env::current_exe().map_err(|error| {
                OrbitError::Execution(format!("resolve current orbit executable: {error}"))
            })?;
            let mut command = Command::new(resolve_pipeline_worker_executable(current_exe));
            configure_pipeline_worker_command(
                &mut command,
                workspace,
                run_id,
                self.root_override.as_deref(),
            );
            Ok(command)
        }
    }
}
