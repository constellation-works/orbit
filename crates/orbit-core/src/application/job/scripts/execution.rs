//! Sandbox launch, environment and scratch lifecycle for a script attempt.

use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::security::redaction::{
    argv_redactor, redact_all_and_home, redact_secret_values_text,
};
use orbit_engine::activity_job::ScriptSnapshot;
use orbit_engine::{ScriptStepEnd, ScriptStepOutputs, ScriptStepRan, ScriptStepRequest};
use orbit_exec::{
    SCRIPT_SCRATCH_DIRS, ScriptRunEnd, ScriptSandboxInput, ScriptSandboxNetwork,
    ScriptSandboxRequest, ScriptSandboxSpec,
};
use orbit_types::workflow::activity_job::{ScriptExecutionRecord, ScriptGrant, script_refusal};

use crate::OrbitRuntime;
use crate::application::job::pipeline::USER_BUS_LOCATOR_ENV_VARS;

use super::step::{Refusal, refuse};
/// The timeout a step gets when neither the step nor the manifest sets one.
const DEFAULT_SCRIPT_TIMEOUT_SECONDS: u64 = 600;

/// The step's scratch directory under the workspace state, removed after
/// the attempt.
pub(super) struct ScriptScratch {
    pub(super) path: PathBuf,
}

impl ScriptScratch {
    pub(super) fn create(
        runtime: &OrbitRuntime,
        request: &ScriptStepRequest,
    ) -> Result<Self, Refusal> {
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

    pub(super) fn remove(self) {
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

pub(super) fn path_segment(value: &str) -> String {
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

pub(super) struct RunPlan<'a> {
    pub(super) request: &'a ScriptStepRequest,
    pub(super) snapshot: &'a ScriptSnapshot,
    pub(super) grant: &'a ScriptGrant,
    pub(super) interpreter: &'a [String],
    pub(super) args: &'a [String],
    pub(super) cwd: &'a Path,
    pub(super) cwd_writable: bool,
    pub(super) scratch: &'a Path,
    pub(super) masked: &'a [PathBuf],
    pub(super) inputs: &'a [ScriptSandboxInput],
    pub(super) network: ScriptSandboxNetwork,
    pub(super) passed: ScriptPassedEnv,
    pub(super) launcher: &'a [String],
    pub(super) launcher_env: &'a [(String, String)],
    pub(super) stdin: Vec<u8>,
    pub(super) record: &'a mut ScriptExecutionRecord,
}

pub(super) fn run_in_scratch(plan: RunPlan<'_>) -> Result<ScriptStepRan, Refusal> {
    let RunPlan {
        request,
        snapshot,
        grant,
        interpreter,
        args,
        cwd,
        cwd_writable,
        scratch,
        masked,
        inputs,
        network,
        passed,
        launcher,
        launcher_env,
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

    let mount = orbit_exec::script_scratch_path_in_sandbox(scratch);
    let mut argv: Vec<String> = interpreter.to_vec();
    argv.push(
        mount
            .join("snapshot")
            .join(&entry_name)
            .display()
            .to_string(),
    );
    argv.extend(args.iter().cloned());

    let secrets = passed.values();
    record.argv = argv
        .iter()
        .map(|argument| redact_secret_values_text(&argv_redactor().apply_str(argument), &secrets))
        .collect();

    let mut env = script_env(request, asset.digest.as_str(), grant, &mount);
    env.extend(passed.vars);
    let mut readable: Vec<PathBuf> = grant
        .interpreter_paths
        .first()
        .into_iter()
        .cloned()
        .collect();
    readable.extend(grant.read_roots.iter().cloned());
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
            masked,
            home: home.as_deref(),
            inputs,
            network,
        },
        argv: &argv,
        env: &env,
        launcher,
        launcher_env,
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
    // The passed values are redacted first so their `[REDACTED]` placeholder
    // wins when a passed name is also sensitive (`redact_all_and_home` would
    // otherwise replace it with `[REDACTED_ENV]` first); the later `redact_all`
    // pass leaves `[REDACTED]` untouched.
    let log = redact_all_and_home(&redact_secret_values_text(
        &String::from_utf8_lossy(&run.log),
        &secrets,
    ))
    .into_bytes();
    // The step output, and every record built from it, is parsed from this
    // stdout: a passed value echoed into it is redacted before it is parsed.
    // Without passed values the bytes stay exactly as the script wrote them.
    let stdout = if secrets.is_empty() {
        run.stdout
    } else {
        redact_secret_values_text(&String::from_utf8_lossy(&run.stdout), &secrets).into_bytes()
    };
    Ok(ScriptStepRan {
        end: match run.end {
            ScriptRunEnd::Exited(code) => ScriptStepEnd::Exited(code),
            ScriptRunEnd::Signaled(signal) => ScriptStepEnd::Signaled(signal),
            ScriptRunEnd::TimedOut => ScriptStepEnd::TimedOut,
            ScriptRunEnd::ScratchCapExceeded => ScriptStepEnd::ScratchCapExceeded,
        },
        stdout,
        stdout_over_cap: run.stdout_over_cap,
        output_schema: asset.spec.output_schema_json.clone(),
        log,
        record: record.clone(),
        outputs: ScriptStepOutputs::None,
    })
}

/// The environment of the scope launcher (`systemd-run --user`) and the
/// sandbox wrapper it execs, composed from this process's environment.
///
/// The user-bus locators are admitted here, and only here: without them the
/// launcher cannot reach the user manager (jrun-20261101-0131-t1, every
/// contained script step exiting 1 in ~2 ms). They never reach the script,
/// whose environment is exactly [`script_env`]; the wrapper clears its
/// environment and sets only that inside the sandbox.
pub(super) fn script_launcher_env() -> Vec<(String, String)> {
    orbit_common::security::child_env::allowlisted_child_env(&[], &USER_BUS_LOCATOR_ENV_VARS)
}

/// Snapshot-driven form of [`script_launcher_env`], so a test states the
/// ambient environment it is reasoning about.
#[cfg(test)]
pub(crate) fn script_launcher_env_from(parent: &[(String, String)]) -> Vec<(String, String)> {
    orbit_common::security::child_env::allowlisted_child_env_from(
        parent,
        &[],
        &USER_BUS_LOCATOR_ENV_VARS,
    )
}

/// The granted `env_pass` variables this process holds, and the granted
/// names it does not.
pub(super) struct ScriptPassedEnv {
    vars: Vec<(String, String)>,
    pub(super) unset: Vec<String>,
}

impl ScriptPassedEnv {
    pub(super) fn names(&self) -> Vec<String> {
        self.vars.iter().map(|(name, _)| name.clone()).collect()
    }

    fn values(&self) -> Vec<String> {
        self.vars.iter().map(|(_, value)| value.clone()).collect()
    }
}

/// The variables a grant passes into the script (§3 Environment), read from
/// `parent`. Only names the grant lists are copied, and a name the parent
/// does not hold (or holds empty) is unset, never an empty string. An
/// `ORBIT_*` name or one [`script_env`] fixes (`PATH`, `HOME`, …) is never
/// copied, so a grant cannot carry Orbit authority or override the profile's
/// own values.
pub(super) fn script_passed_env(
    grant: &ScriptGrant,
    parent: &[(String, String)],
) -> ScriptPassedEnv {
    let mut passed = ScriptPassedEnv {
        vars: Vec::new(),
        unset: Vec::new(),
    };
    for name in &grant.env_pass {
        if name.starts_with("ORBIT_")
            || SCRIPT_FIXED_ENV.contains(&name.as_str())
            || passed.vars.iter().any(|(seen, _)| seen == name)
            || passed.unset.contains(name)
        {
            continue;
        }
        match parent
            .iter()
            .find(|(key, value)| key == name && !value.is_empty())
        {
            Some((_, value)) => passed.vars.push((name.clone(), value.clone())),
            None => passed.unset.push(name.clone()),
        }
    }
    passed
}

/// The names [`script_env`] sets itself; a grant's `env_pass` never
/// replaces them.
const SCRIPT_FIXED_ENV: [&str; 5] = ["PATH", "HOME", "TMPDIR", "LANG", "TZ"];

/// The script's fixed environment (§3): fixed values and the step context.
/// The granted `env_pass` variables ([`script_passed_env`]) are added to it.
/// `mount` is where the script sees its scratch directory
/// ([`orbit_exec::script_scratch_path_in_sandbox`]).
pub(crate) fn script_env(
    request: &ScriptStepRequest,
    digest: &str,
    grant: &ScriptGrant,
    mount: &Path,
) -> Vec<(String, String)> {
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
