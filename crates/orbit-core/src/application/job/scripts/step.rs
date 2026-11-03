//! One attempt of a `target: script:<name>` step on this host (script steps
//! §2–§5): resolve the cwd, snapshot the script, check its grant, probe its
//! granted roots, probe the script profile (`linux-bwrap-script-v1` or
//! `macos-sandbox-script-v1`), then run the snapshot under it.
//!
//! The granted roots are probed again here, in the job's process, before
//! anything is created or spawned: on macOS, privacy permissions belong to
//! the process that runs the job, not the one that granted. On macOS,
//! `strict` worker containment refuses every step, as macOS has no kernel
//! containment for a script.
//!
//! Every check that can refuse runs before anything is spawned, and each
//! refusal names its `script_refusal` code. The job executor decides what
//! the answer means for the step and writes the `script_execution` entry.
//! A manifest that declares output files gets them kept as one run-owned
//! set ([`super::outputs`]) after a successful attempt whose stdout
//! the step accepts, before the scratch directory is removed. Declared
//! input files ([`super::inputs`]) are resolved and digested before
//! spawn and bound read-only at `in/<as>`.
//!
//! The grant's `network` decides the network the script reaches, and its
//! `env_pass` names are the only host variables copied into the script's
//! environment. Each passed value is redacted, through the shared redaction
//! path, from the captured log, the stdout that becomes the step output and
//! the recorded argv before any of them leaves this host boundary, so the
//! audit record, `orbit run show` and `orbit script history` never hold it.

use std::path::{Path, PathBuf};

use orbit_engine::activity_job::{ScriptSnapshot, load_script_snapshot, render_script_args};
use orbit_engine::{ScriptStepRan, ScriptStepRequest, ScriptStepRun};
use orbit_exec::{ScriptRootAccess, ScriptSandboxInput, ScriptSandboxNetwork};
use orbit_types::workflow::activity_job::{
    ScriptExecutionRecord, ScriptGrant, ScriptHostRole, ScriptNetwork, script_refusal,
};
use serde_json::Value;

use super::inputs::{ScriptInputSources, admit_script_inputs};
use crate::OrbitRuntime;
use crate::application::job::pipeline::{
    ScriptScopeLauncher, script_containment_is_strict, script_scope_launcher,
};
use crate::application::job::script_outputs::keep_declared_outputs;

use super::execution::{
    RunPlan, ScriptScratch, run_in_scratch, script_launcher_env, script_passed_env,
};

/// This host's script profile, as `orbit doctor` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptSandboxReport {
    /// The profile a script step would run under, or why none can be
    /// applied here (the `script_sandbox_unavailable` reason).
    pub profile: Result<&'static str, String>,
    /// The profile's documented gaps against the Linux profile (macOS).
    pub gaps: Option<&'static str>,
    /// Whether `machine.worker_containment = strict` refuses every script
    /// step on this host (macOS, which has no kernel containment).
    pub strict_refuses: bool,
}

impl OrbitRuntime {
    /// Probe the script profile the way a step does before spawn.
    pub fn script_sandbox_report(&self) -> ScriptSandboxReport {
        let macos = cfg!(target_os = "macos");
        ScriptSandboxReport {
            profile: orbit_exec::probe_script_sandbox(),
            gaps: macos.then_some(orbit_exec::SCRIPT_MACOS_PROFILE_GAPS),
            strict_refuses: macos
                && script_containment_is_strict(self.context.settings().worker_containment()),
        }
    }
}

/// Why `machine.worker_containment = strict` refuses a script step on macOS.
const MACOS_STRICT_REFUSAL: &str = "machine.worker_containment is strict, and macOS gives a \
     script step no kernel containment (no memory or task limits), so strict refuses every \
     script step on macOS; set machine.worker_containment to warn to run it under the profile \
     alone";

pub(super) struct Refusal {
    code: &'static str,
    message: String,
    /// Whether an operator's grant would clear this refusal, so it raises a
    /// `script_grant` decision request (a missing, expired, stale or
    /// insufficient grant; never a witness failure or a sandbox gap).
    grant_would_clear: bool,
}

pub(super) fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
        grant_would_clear: false,
    }
}

/// A refusal a grant at the current digest and requests would clear.
fn refuse_for_grant(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        grant_would_clear: true,
        ..refuse(code, message)
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
    let mut snapshot = None;
    match prepare_and_run(runtime, request, &mut record, &mut snapshot) {
        Ok(ran) => ScriptStepRun::Ran(Box::new(ran)),
        Err(Refusal {
            code,
            mut message,
            grant_would_clear,
        }) => {
            if grant_would_clear && let Some(snapshot) = snapshot.as_ref() {
                message = with_grant_request(runtime, request, snapshot, code, message);
            }
            ScriptStepRun::Refused {
                code,
                message,
                record: Box::new(record),
            }
        }
    }
}

/// Raise or reuse the `script_grant` request for a refusal a grant would
/// clear, and name it in the refusal message. Raising never grants, and a
/// raise that cannot be recorded (a replica checkout, a store error) leaves
/// the refusal exactly as it was.
fn with_grant_request(
    runtime: &OrbitRuntime,
    request: &ScriptStepRequest,
    snapshot: &ScriptSnapshot,
    code: &'static str,
    message: String,
) -> String {
    match runtime.raise_script_grant_request(
        snapshot,
        code,
        &message,
        Some(&request.run_id),
        Some(&request.step_id),
    ) {
        Ok(raised) => format!(
            "{message}. {} grant request {} on task {} asks the operator to approve these \
             bytes (`orbit decision show {}`)",
            if raised.deduplicated {
                "The existing"
            } else {
                "A new"
            },
            raised.request.id,
            raised.request.task_id,
            raised.request.id
        ),
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id = %request.run_id,
                step_id = %request.step_id,
                script = %request.script,
                error = %error,
                "could not raise a script grant request for the refused step",
            );
            message
        }
    }
}

fn prepare_and_run(
    runtime: &OrbitRuntime,
    request: &ScriptStepRequest,
    record: &mut ScriptExecutionRecord,
    loaded: &mut Option<ScriptSnapshot>,
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
    *loaded = Some(snapshot.clone());
    let asset = &snapshot.asset;
    record.entry = asset.spec.entry.clone();
    record.digest = asset.digest.clone();
    record.network = asset.spec.requests.network;

    let grant = admitted_grant(runtime, &request.script, record)?;
    if grant.digest != asset.digest {
        return Err(refuse_for_grant(
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
    // The grant, not the manifest's request, decides what the script gets.
    record.network = grant.network;
    record.read_roots = grant.read_roots.clone();
    record.write_roots = grant.write_roots.clone();
    // Which granted names pass is known before anything can refuse for the
    // host, so every record after the grant check names them.
    let parent: Vec<(String, String)> = std::env::vars().collect();
    let passed = script_passed_env(&grant, &parent);
    record.env_pass = passed.names();
    record.env_pass_unset = passed.unset.clone();

    let input_text = admitted_input(&snapshot, &request.input)?;
    let home = std::env::var_os("HOME");
    probe_granted_roots(&grant, home.as_deref())?;

    // Declared inputs resolve after the grant checks, so a refusal records
    // the grant, and before the profile probe and anything created or
    // spawned, so the refusal names the input on any host.
    let masked = script_masked_trees(runtime, &cwd);
    let admitted = admit_script_inputs(
        &asset.spec.files.inputs,
        &request.input,
        &ScriptInputSources {
            cwd: &cwd,
            read_roots: &grant.read_roots,
            masked: &masked,
            home: home.as_deref(),
        },
    )
    .map_err(|message| refuse(script_refusal::FILE_INPUT_UNAVAILABLE, message))?;
    let (inputs, input_files): (Vec<ScriptSandboxInput>, Vec<_>) = admitted.into_iter().unzip();
    record.input_files = input_files;

    // macOS gives a script no kernel containment (no memory or task
    // limits), so strict containment refuses every script step there. It is
    // a host policy, decided before the profile is probed.
    if cfg!(target_os = "macos")
        && script_containment_is_strict(runtime.context.settings().worker_containment())
    {
        return Err(refuse(
            script_refusal::SANDBOX_UNAVAILABLE,
            MACOS_STRICT_REFUSAL,
        ));
    }
    // After the grant checks, so a refusal on a host without the profile
    // still records which script and grant it refused.
    let profile = orbit_exec::probe_script_sandbox().map_err(|reason| {
        refuse(
            script_refusal::SANDBOX_UNAVAILABLE,
            format!("the script profile cannot run on this host: {reason}"),
        )
    })?;
    record.profile = Some(profile.to_string());
    let network = ScriptSandboxNetwork::for_grant(grant.network);
    orbit_exec::probe_script_sandbox_network(network).map_err(|reason| {
        refuse(
            script_refusal::SANDBOX_UNAVAILABLE,
            format!(
                "script `{}` is granted network `{}`, which this host cannot give it: {reason}",
                request.script, grant.network
            ),
        )
    })?;

    let args = render_script_args(&asset.spec.args, &request.input)
        .map_err(|error| refuse(script_refusal::INPUT_INVALID, error.to_string()))?;
    // One environment for both the availability probe and the launch, so
    // the probe answers for the environment `systemd-run` will actually
    // get [ORB-20661].
    let mut launcher_env = script_launcher_env();
    let scope = script_scope_launcher(
        &request.run_id,
        &request.step_id,
        runtime.context.settings().worker_containment(),
        &launcher_env,
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
            // No `systemd-run` runs, so the bus locators have no consumer:
            // the wrapper keeps the plain allowlisted environment.
            launcher_env = orbit_common::security::child_env::allowlisted_child_env(&[], &[]);
            Vec::new()
        }
    };

    let scratch = ScriptScratch::create(runtime, request)?;
    let mut result = run_in_scratch(RunPlan {
        request,
        snapshot: &snapshot,
        grant: &grant,
        interpreter: &interpreter,
        args: &args,
        cwd: &cwd,
        cwd_writable,
        scratch: &scratch.path,
        masked: &masked,
        inputs: &inputs,
        network,
        passed,
        launcher: &launcher,
        launcher_env: &launcher_env,
        stdin: input_text.into_bytes(),
        record,
    });
    // Keep declared outputs while `out/` still exists.
    if let Ok(ran) = result.as_mut() {
        keep_declared_outputs(
            runtime,
            &asset.spec.files.outputs,
            &grant,
            request,
            &scratch.path.join("out"),
            ran,
        );
    }
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

/// The trees a script must not see: the cwd's `.orbit/`, the global root
/// and the workspace's own `.orbit/`.
fn script_masked_trees(runtime: &OrbitRuntime, cwd: &Path) -> Vec<PathBuf> {
    let mut masked = vec![cwd.join(".orbit"), runtime.global_root()];
    if let Ok(orbit_dir) = runtime.paths().orbit_dir.canonicalize() {
        masked.push(orbit_dir);
    }
    masked
}

/// Repeat the grant-time root probe in the job's process before anything
/// is created or spawned: every granted read and write root must exist,
/// lie outside the credential paths and be readable (and a write root
/// writable) by this process. On macOS, privacy permissions are granted to
/// the process that runs the job, which need not be the one that granted.
fn probe_granted_roots(grant: &ScriptGrant, home: Option<&std::ffi::OsStr>) -> Result<(), Refusal> {
    let roots = grant
        .read_roots
        .iter()
        .map(|root| (root, ScriptRootAccess::Read))
        .chain(
            grant
                .write_roots
                .iter()
                .map(|root| (root, ScriptRootAccess::Write)),
        );
    for (root, access) in roots {
        orbit_exec::probe_script_root(root, access, home).map_err(|reason| {
            refuse(
                script_refusal::ROOT_UNAVAILABLE,
                format!("{reason}; nothing was run"),
            )
        })?;
    }
    Ok(())
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
            refuse_for_grant(
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
            return Err(refuse_for_grant(
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

/// A manifest request beyond the grant refuses the step before anything
/// is spawned.
fn check_requests_within_grant(
    snapshot: &ScriptSnapshot,
    grant: &ScriptGrant,
) -> Result<(), Refusal> {
    let asset = &snapshot.asset;
    let requests = &asset.spec.requests;
    let beyond = |what: String| {
        refuse_for_grant(
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
    // Output caps are never widened for the worker: a manifest asking for
    // more than the operator acknowledged refuses before anything runs.
    let outputs = &asset.spec.files.outputs;
    if let Some(files) = outputs.max_files
        && files > grant.limits.output_max_files
    {
        return Err(beyond(format!(
            "{files} output files (the grant allows {}; acknowledge them with \
             `orbit script grant {} --output-max-files {files}`)",
            grant.limits.output_max_files, asset.name
        )));
    }
    if let Some(bytes) = outputs.max_file_bytes
        && bytes > grant.limits.output_max_file_bytes
    {
        return Err(beyond(format!(
            "output files of {bytes} bytes (the grant allows {}; acknowledge them with \
             `orbit script grant {} --output-max-file-bytes {bytes}`)",
            grant.limits.output_max_file_bytes, asset.name
        )));
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
