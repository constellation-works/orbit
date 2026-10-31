//! One attempt of a `target: script:<name>` step on this host (script steps
//! §2–§5): resolve the cwd, snapshot the script, check its grant, probe the
//! script profile, then run the snapshot under it.
//!
//! Every check that can refuse runs before anything is spawned, and each
//! refusal names its `script_refusal` code. The job executor decides what
//! the answer means for the step and writes the `script_execution` entry.

use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::security::redaction::{argv_redactor, redact_all_and_home};
use orbit_engine::activity_job::{ScriptSnapshot, load_script_snapshot, render_script_args};
use orbit_engine::{ScriptStepEnd, ScriptStepRan, ScriptStepRequest, ScriptStepRun};
use orbit_exec::{
    SCRIPT_SANDBOX_MOUNT, SCRIPT_SCRATCH_DIRS, ScriptRunEnd, ScriptSandboxRequest,
    ScriptSandboxSpec,
};
use orbit_types::workflow::activity_job::{
    ScriptExecutionRecord, ScriptGrant, ScriptHostRole, ScriptNetwork, script_refusal,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::pipeline::{ScriptScopeLauncher, script_scope_launcher};

/// The timeout a step gets when neither the step nor the manifest sets one.
const DEFAULT_SCRIPT_TIMEOUT_SECONDS: u64 = 600;

struct Refusal {
    code: &'static str,
    message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
    }
}

/// Run one attempt, or refuse it before spawning anything.
pub(crate) fn run_script_step(
    runtime: &OrbitRuntime,
    request: &ScriptStepRequest,
) -> ScriptStepRun {
    let mut record = ScriptExecutionRecord {
        script: request.script.clone(),
        ..ScriptExecutionRecord::default()
    };
    match prepare_and_run(runtime, request, &mut record) {
        Ok(ran) => ScriptStepRun::Ran(Box::new(ran)),
        Err(Refusal { code, message }) => ScriptStepRun::Refused {
            code,
            message,
            record: Box::new(record),
        },
    }
}

fn prepare_and_run(
    runtime: &OrbitRuntime,
    request: &ScriptStepRequest,
    record: &mut ScriptExecutionRecord,
) -> Result<ScriptStepRan, Refusal> {
    record.platform = Some(std::env::consts::OS.to_string());
    let (cwd, cwd_writable) = script_cwd(runtime, request.workspace_path.as_deref())?;
    record.cwd = Some(cwd.clone());
    record.cwd_mode = Some(
        if cwd_writable {
            "worktree"
        } else {
            "read_only_root"
        }
        .to_string(),
    );

    let manifest_path = cwd
        .join(".orbit")
        .join("resources")
        .join("scripts")
        .join(format!("{}.yaml", request.script));
    let snapshot = load_script_snapshot(&manifest_path, &cwd)
        .map_err(|error| refuse(script_refusal::MANIFEST_INVALID, error.to_string()))?;
    let asset = &snapshot.asset;
    record.entry = asset.spec.entry.clone();
    record.digest = asset.digest.clone();
    record.network = asset.spec.requests.network;

    let grant = admitted_grant(runtime, &request.script, record)?;
    if grant.digest != asset.digest {
        return Err(refuse(
            script_refusal::GRANT_STALE,
            format!(
                "script `{}` changed since it was granted (granted digest {}, current digest {}); \
                 review it and run `orbit script grant {}` again",
                request.script, grant.digest, asset.digest, request.script
            ),
        ));
    }
    let interpreter = admitted_interpreter(&snapshot, &grant)?;
    record.interpreter_paths = grant.interpreter_paths.clone();
    check_requests_within_grant(&snapshot, &grant)?;
    record.read_roots = grant.read_roots.clone();
    record.write_roots = grant.write_roots.clone();

    let input_text = admitted_input(&snapshot, &request.input)?;

    // After the grant checks, so a refusal on a host without the profile
    // still records which script and grant it refused.
    let profile = orbit_exec::probe_script_sandbox().map_err(|reason| {
        refuse(
            script_refusal::SANDBOX_UNAVAILABLE,
            format!("the script profile cannot run on this host: {reason}"),
        )
    })?;
    record.profile = Some(profile.to_string());

    let args = render_script_args(&asset.spec.args, &request.input)
        .map_err(|error| refuse(script_refusal::INPUT_INVALID, error.to_string()))?;
    for root in &grant.read_roots {
        if !root.exists() {
            return Err(refuse(
                script_refusal::ROOT_UNAVAILABLE,
                format!("granted read root {} does not exist", root.display()),
            ));
        }
    }
    for root in &grant.write_roots {
        #[allow(
            clippy::disallowed_methods,
            reason = "an operator-granted write root is an external directory, not Orbit state; \
                      it keeps the operator's umask like a plugin write root"
        )]
        std::fs::create_dir_all(root).map_err(|error| {
            refuse(
                script_refusal::ROOT_UNAVAILABLE,
                format!("create granted write root {}: {error}", root.display()),
            )
        })?;
    }

    let scope = script_scope_launcher(
        &request.run_id,
        &request.step_id,
        runtime.context.settings().worker_containment(),
    )
    .map_err(|reason| {
        refuse(
            script_refusal::SANDBOX_UNAVAILABLE,
            format!(
                "machine.worker_containment is strict and the script's own scope is \
                 unavailable: {reason}"
            ),
        )
    })?;
    let launcher = match scope {
        ScriptScopeLauncher::Scoped { unit, argv } => {
            record.scope_unit = Some(unit);
            argv
        }
        ScriptScopeLauncher::Uncontained { reason } => {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id = %request.run_id,
                step_id = %request.step_id,
                reason,
                "script step runs without its own systemd scope, in the worker's cgroup",
            );
            Vec::new()
        }
    };

    let scratch = ScriptScratch::create(runtime, request)?;
    let result = run_in_scratch(RunPlan {
        runtime,
        request,
        snapshot: &snapshot,
        grant: &grant,
        interpreter: &interpreter,
        args: &args,
        cwd: &cwd,
        cwd_writable,
        scratch: &scratch.path,
        launcher: &launcher,
        stdin: input_text.into_bytes(),
        record,
    });
    scratch.remove();
    result
}

/// The cwd tree and whether it is writable: the run's own worktree, or the
/// workspace root read-only when the job has none.
fn script_cwd(
    runtime: &OrbitRuntime,
    workspace_path: Option<&str>,
) -> Result<(PathBuf, bool), Refusal> {
    let Some(path) = workspace_path else {
        let root = runtime.paths().repo_root.canonicalize().map_err(|error| {
            refuse(
                script_refusal::ROOT_UNAVAILABLE,
                format!(
                    "workspace root {} is unavailable: {error}",
                    runtime.paths().repo_root.display()
                ),
            )
        })?;
        return Ok((root, false));
    };
    let unavailable = |detail: String| {
        refuse(
            script_refusal::ROOT_UNAVAILABLE,
            format!("run worktree {path} {detail}"),
        )
    };
    let worktree = Path::new(path)
        .canonicalize()
        .map_err(|error| unavailable(format!("is unavailable: {error}")))?;
    let worktrees = runtime
        .paths()
        .worktrees_dir
        .canonicalize()
        .map_err(|error| unavailable(format!("has no worktree directory: {error}")))?;
    // Only a run worktree itself, never the pool or a directory inside one,
    // is a writable cwd.
    if worktree.parent() != Some(worktrees.as_path()) {
        return Err(unavailable(format!(
            "is not a run worktree of this workspace (under {})",
            worktrees.display()
        )));
    }
    Ok((worktree, true))
}

/// The verified grant for `script`, admitted for this host's role.
fn admitted_grant(
    runtime: &OrbitRuntime,
    script: &str,
    record: &mut ScriptExecutionRecord,
) -> Result<ScriptGrant, Refusal> {
    let grant = runtime
        .verified_script_grant(script)
        .map_err(|error| {
            refuse(
                script_refusal::GRANT_MISSING,
                format!("script `{script}` has no usable grant: {error}"),
            )
        })?
        .ok_or_else(|| {
            refuse(
                script_refusal::GRANT_MISSING,
                format!(
                    "script `{script}` is not granted on this host; review it and run \
                     `orbit script grant {script}`"
                ),
            )
        })?;
    record.granted_digest = Some(grant.digest.clone());
    record.grant_id = Some(grant.grant_id.clone());
    record.grantor = Some(grant.grantor.clone());
    record.grant_provenance = Some(grant.provenance.clone());
    if let Some(expires_at) = &grant.expires_at {
        let expired = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map(|at| at <= chrono::Utc::now())
            .unwrap_or(true);
        if expired {
            return Err(refuse(
                script_refusal::GRANT_MISSING,
                format!(
                    "script `{script}` grant {} expired at {expires_at}",
                    grant.grant_id
                ),
            ));
        }
    }
    if grant.host_role == ScriptHostRole::Owner && runs_as_follower(runtime) {
        return Err(refuse(
            script_refusal::FOLLOWER_REFUSED,
            format!(
                "this host runs the step for another owner and script `{script}` was granted \
                 for owners only; grant it here with `orbit script grant {script} --follower`"
            ),
        ));
    }
    Ok(grant)
}

/// A replica checkout, or a worker bound to a claim another machine owns.
fn runs_as_follower(runtime: &OrbitRuntime) -> bool {
    runtime.replica_owner_machine().is_some()
        || crate::runtime::RuntimeKernel::worker_invocation(runtime).is_some_and(|invocation| {
            invocation.execution.machine_id != invocation.owner_machine_id
        })
}

/// The launcher argv the grant recorded, as long as it still names the same
/// program. The caller's `PATH` never decides what runs.
fn admitted_interpreter(
    snapshot: &ScriptSnapshot,
    grant: &ScriptGrant,
) -> Result<Vec<String>, Refusal> {
    let asset = &snapshot.asset;
    let changed = |detail: String| {
        refuse(
            script_refusal::INTERPRETER_CHANGED,
            format!(
                "script `{}` interpreter {detail}; run `orbit script grant {}` again",
                asset.name, asset.name
            ),
        )
    };
    let Some(program) = grant.interpreter_paths.first() else {
        if asset.interpreter.is_empty() {
            return Ok(Vec::new());
        }
        return Err(changed("was declared after the grant".to_string()));
    };
    if asset.interpreter.is_empty() || asset.interpreter.len() != grant.interpreter_paths.len() {
        return Err(changed("no longer matches the grant".to_string()));
    }
    match program.canonicalize() {
        Ok(current) if current == *program && current.is_file() => {}
        Ok(current) => {
            return Err(changed(format!(
                "{} now resolves to {}",
                program.display(),
                current.display()
            )));
        }
        Err(error) => {
            return Err(changed(format!("{} is gone: {error}", program.display())));
        }
    }
    Ok(grant
        .interpreter_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect())
}

/// A manifest request beyond the grant, or one this phase cannot honour,
/// refuses the step.
fn check_requests_within_grant(
    snapshot: &ScriptSnapshot,
    grant: &ScriptGrant,
) -> Result<(), Refusal> {
    let asset = &snapshot.asset;
    let requests = &asset.spec.requests;
    let beyond = |what: String| {
        refuse(
            script_refusal::GRANT_INSUFFICIENT,
            format!(
                "script `{}` requests {what} beyond its grant {}; re-grant it to widen the grant",
                asset.name, grant.grant_id
            ),
        )
    };
    if network_rank(requests.network) > network_rank(grant.network) {
        return Err(beyond(format!("network `{}`", requests.network)));
    }
    if let Some(root) = requests
        .read_roots
        .iter()
        .find(|root| !grant.read_roots.contains(root))
    {
        return Err(beyond(format!("read root {}", root.display())));
    }
    if let Some(root) = requests
        .write_roots
        .iter()
        .find(|root| !grant.write_roots.contains(root))
    {
        return Err(beyond(format!("write root {}", root.display())));
    }
    if let Some(name) = requests
        .env_pass
        .iter()
        .find(|name| !grant.env_pass.contains(name))
    {
        return Err(beyond(format!("environment variable `{name}`")));
    }
    if let Some(seconds) = requests.timeout_seconds
        && seconds > grant.limits.timeout_max_seconds
    {
        return Err(beyond(format!("a {seconds}s timeout")));
    }
    let unsupported = |what: &str| {
        refuse(
            script_refusal::SANDBOX_UNAVAILABLE,
            format!(
                "script `{}` uses {what}, which this build's script profile does not run yet",
                asset.name
            ),
        )
    };
    if requests.network != ScriptNetwork::None || grant.network != ScriptNetwork::None {
        return Err(unsupported("network access"));
    }
    if !requests.env_pass.is_empty() || !grant.env_pass.is_empty() {
        return Err(unsupported("`env_pass`"));
    }
    if !asset.spec.files.inputs.is_empty() {
        return Err(unsupported("`files.inputs`"));
    }
    Ok(())
}

fn network_rank(network: ScriptNetwork) -> u8 {
    match network {
        ScriptNetwork::None => 0,
        ScriptNetwork::Https => 1,
        ScriptNetwork::Any => 2,
    }
}

/// The step input validated against `input_schema_json`, as stdin text.
fn admitted_input(snapshot: &ScriptSnapshot, input: &Value) -> Result<String, Refusal> {
    let invalid = |detail: String| {
        refuse(
            script_refusal::INPUT_INVALID,
            format!("script `{}` input {detail}", snapshot.asset.name),
        )
    };
    let schema = &snapshot.asset.spec.input_schema_json;
    if !schema.is_null() {
        let validator = orbit_tools::plugin::CompiledSchema::compile(schema.clone())
            .map_err(|error| invalid(format!("schema is invalid: {error}")))?;
        if let Some(violations) = validator.violations(input) {
            return Err(invalid(format!("fails input_schema_json: {violations}")));
        }
    }
    serde_json::to_string(input).map_err(|error| invalid(format!("is not JSON: {error}")))
}

/// The step's scratch directory under the workspace state, removed after
/// the attempt.
struct ScriptScratch {
    path: PathBuf,
}

impl ScriptScratch {
    fn create(runtime: &OrbitRuntime, request: &ScriptStepRequest) -> Result<Self, Refusal> {
        let nonce = getrandom::u32().unwrap_or_else(|_| std::process::id());
        let path = runtime
            .paths()
            .state_dir
            .join("scripts")
            .join("runs")
            .join(path_segment(&request.run_id))
            .join(format!("{}-{nonce:08x}", path_segment(&request.step_id)));
        let unavailable = |error: std::io::Error| {
            refuse(
                script_refusal::ROOT_UNAVAILABLE,
                format!("create script scratch {}: {error}", path.display()),
            )
        };
        for directory in SCRIPT_SCRATCH_DIRS {
            orbit_common::fs::io::create_private_dir_all(&path.join(directory))
                .map_err(unavailable)?;
        }
        let path = path.canonicalize().map_err(unavailable)?;
        Ok(Self { path })
    }

    fn remove(self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            tracing::warn!(
                target: "orbit.core.job_run",
                path = %self.path.display(),
                error = %error,
                "script scratch directory was not removed",
            );
        }
    }
}

fn path_segment(value: &str) -> String {
    value
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

struct RunPlan<'a> {
    runtime: &'a OrbitRuntime,
    request: &'a ScriptStepRequest,
    snapshot: &'a ScriptSnapshot,
    grant: &'a ScriptGrant,
    interpreter: &'a [String],
    args: &'a [String],
    cwd: &'a Path,
    cwd_writable: bool,
    scratch: &'a Path,
    launcher: &'a [String],
    stdin: Vec<u8>,
    record: &'a mut ScriptExecutionRecord,
}

fn run_in_scratch(plan: RunPlan<'_>) -> Result<ScriptStepRan, Refusal> {
    let RunPlan {
        runtime,
        request,
        snapshot,
        grant,
        interpreter,
        args,
        cwd,
        cwd_writable,
        scratch,
        launcher,
        stdin,
        record,
    } = plan;
    let asset = &snapshot.asset;
    let entry_name = asset
        .entry_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "entry".to_string());
    let snapshot_dir = scratch.join("snapshot");
    let unwritable = |error: std::io::Error| {
        refuse(
            script_refusal::ROOT_UNAVAILABLE,
            format!(
                "write the script snapshot under {}: {error}",
                snapshot_dir.display()
            ),
        )
    };
    orbit_common::fs::io::atomic_write_bytes(
        &snapshot_dir.join(format!("{}.yaml", asset.name)),
        &snapshot.manifest_bytes,
    )
    .map_err(unwritable)?;
    let entry = snapshot_dir.join(&entry_name);
    orbit_common::fs::io::atomic_write_bytes(&entry, &snapshot.entry_bytes).map_err(unwritable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o500))
            .map_err(unwritable)?;
    }

    let mount = Path::new(SCRIPT_SANDBOX_MOUNT);
    let mut argv: Vec<String> = interpreter.to_vec();
    argv.push(
        mount
            .join("snapshot")
            .join(&entry_name)
            .display()
            .to_string(),
    );
    argv.extend(args.iter().cloned());
    record.argv = argv
        .iter()
        .map(|argument| argv_redactor().apply_str(argument))
        .collect();

    let env = script_env(request, asset.digest.as_str(), grant);
    let launcher_env: Vec<(String, String)> =
        orbit_common::security::child_env::allowlisted_child_env(&[], &[]);
    let mut readable: Vec<PathBuf> = grant
        .interpreter_paths
        .first()
        .into_iter()
        .cloned()
        .collect();
    readable.extend(grant.read_roots.iter().cloned());
    let mut masked = vec![cwd.join(".orbit"), runtime.global_root()];
    if let Ok(orbit_dir) = runtime.paths().orbit_dir.canonicalize() {
        masked.push(orbit_dir);
    }
    let home = std::env::var_os("HOME");
    let timeout_seconds = request
        .timeout_seconds
        .or(asset.spec.requests.timeout_seconds)
        .unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECONDS)
        .min(grant.limits.timeout_max_seconds)
        .max(1);

    let run = orbit_exec::run_script_sandboxed(ScriptSandboxRequest {
        sandbox: ScriptSandboxSpec {
            cwd,
            cwd_writable,
            scratch,
            readable: &readable,
            writable: &grant.write_roots,
            masked: &masked,
            home: home.as_deref(),
        },
        argv: &argv,
        env: &env,
        launcher,
        launcher_env: &launcher_env,
        stdin,
        timeout: Duration::from_secs(timeout_seconds),
        scratch_cap_bytes: grant.limits.scratch_max_bytes,
    })
    .map_err(|error| {
        refuse(
            script_refusal::SANDBOX_UNAVAILABLE,
            format!("the script profile could not start: {error}"),
        )
    })?;
    record.duration_ms = Some(u64::try_from(run.duration.as_millis()).unwrap_or(u64::MAX));
    let log = redact_all_and_home(&String::from_utf8_lossy(&run.log)).into_bytes();
    Ok(ScriptStepRan {
        end: match run.end {
            ScriptRunEnd::Exited(code) => ScriptStepEnd::Exited(code),
            ScriptRunEnd::Signaled(signal) => ScriptStepEnd::Signaled(signal),
            ScriptRunEnd::TimedOut => ScriptStepEnd::TimedOut,
            ScriptRunEnd::ScratchCapExceeded => ScriptStepEnd::ScratchCapExceeded,
        },
        stdout: run.stdout,
        stdout_over_cap: run.stdout_over_cap,
        output_schema: asset.spec.output_schema_json.clone(),
        log,
        record: record.clone(),
    })
}

/// The script's whole environment (§3): fixed values and the step context.
fn script_env(
    request: &ScriptStepRequest,
    digest: &str,
    grant: &ScriptGrant,
) -> Vec<(String, String)> {
    let mount = Path::new(SCRIPT_SANDBOX_MOUNT);
    let mut path_dirs: Vec<String> = grant
        .interpreter_paths
        .first()
        .and_then(|program| program.parent())
        .map(|dir| dir.display().to_string())
        .into_iter()
        .collect();
    path_dirs.extend(["/usr/bin".to_string(), "/bin".to_string()]);
    path_dirs.dedup();
    vec![
        ("PATH".into(), path_dirs.join(":")),
        ("HOME".into(), mount.join("home").display().to_string()),
        ("TMPDIR".into(), mount.join("tmp").display().to_string()),
        ("LANG".into(), "C.UTF-8".into()),
        (
            "TZ".into(),
            std::env::var("TZ").unwrap_or_else(|_| "UTC".to_string()),
        ),
        ("ORBIT_SCRIPT_NAME".into(), request.script.clone()),
        ("ORBIT_SCRIPT_DIGEST".into(), digest.to_string()),
        ("ORBIT_SCRIPT_RUN_ID".into(), request.run_id.clone()),
        ("ORBIT_SCRIPT_STEP_ID".into(), request.step_id.clone()),
        (
            "ORBIT_SCRIPT_IN".into(),
            mount.join("in").display().to_string(),
        ),
        (
            "ORBIT_SCRIPT_OUT".into(),
            mount.join("out").display().to_string(),
        ),
    ]
}
