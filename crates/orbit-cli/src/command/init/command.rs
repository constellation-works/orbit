use clap::Args;
use orbit_core::bootstrap::init::{InitOptions, allocated_task_prefix, init_global};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry::global_orbit_dir;
use orbit_registry::{
    MachineIdentityOutcome, MachineIdentityState, NewMachineIdentity, ensure_machine_identity,
    inspect_machine_identity, os_hostname,
};
use orbit_types::identity::validate_new_task_prefix;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use super::collect_config_seed_for_init;
use super::prompt_stdin;
use serde_json::json;

use crate::command::{CommandOut, CommandOutput, Execute, Payload};

#[derive(Args)]
#[command(
    about = "Initialize the global Orbit root (~/.orbit)",
    after_help = "On Linux, sandbox preparation failures warn and initialization continues.\n\
                  linux-bwrap dispatch stays blocked until orbit doctor providers reports\n\
                  the sandbox ready. Fix the host using docs/runbooks/linux-sandbox.md,\n\
                  then retry with orbit init --host-prerequisites-only. JSON output includes\n\
                  linux_sandbox.status (ready, skipped, or not_ready) and its reason."
)]
pub struct InitCommand {
    /// Output the initialization result as JSON.
    #[arg(long)]
    pub json: bool,

    /// Reset the global Orbit root (~/.orbit/) to shipped defaults before
    /// initialization, including executor sandbox settings
    #[arg(long)]
    pub force: bool,

    /// Internal installer entry point: prepare the Linux host without creating
    /// a machine identity or writing Orbit state.
    #[arg(long, hide = true)]
    pub host_prerequisites_only: bool,

    /// On Linux, leave Bubblewrap packages and AppArmor profiles to the host's
    /// administrator. Orbit still seeds `linux-bwrap` executors, and dispatch
    /// stays fail-closed until `orbit doctor providers` reports the sandbox
    /// ready
    #[arg(
        long,
        env = "ORBIT_SKIP_HOST_PREREQUISITES",
        value_parser = clap::builder::FalseyValueParser::new()
    )]
    pub skip_host_prerequisites: bool,

    /// Skip interactive prompts. config.toml is still seeded from detected
    /// agent surfaces, but a CI runner that pipes nothing into stdin will not
    /// hang.
    #[arg(long)]
    pub non_interactive: bool,

    /// Operator-chosen display name for this machine. Used only when no
    /// identity exists yet (first init). Required with --non-interactive on a
    /// fresh machine; interactively, the OS hostname is the default.
    #[arg(long)]
    pub machine_name: Option<String>,

    /// Immutable task-id namespace for this machine (2-5 uppercase ASCII
    /// letters). Required on first init; reserved artifact namespaces cannot
    /// be chosen.
    #[arg(long, value_name = "PREFIX")]
    pub task_prefix: Option<String>,
}

impl Execute for InitCommand {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        self.run(None)
    }
}

impl InitCommand {
    /// Prepare the Linux sandbox prerequisites unless the operator opted out.
    #[cfg(target_os = "linux")]
    fn prepare_linux_host(&self) -> Result<LinuxSandboxReadiness, OrbitError> {
        if self.skip_host_prerequisites {
            Ok(LinuxSandboxReadiness::skipped(
                "host preparation skipped; `orbit doctor providers` reports readiness",
            ))
        } else {
            super::linux_host::prepare(self.non_interactive).map(|reason| LinuxSandboxReadiness {
                status: LinuxSandboxStatus::Ready,
                reason,
            })
        }
    }

    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        self.run(root_override)
    }

    fn run(self, root_override: Option<&Path>) -> CommandOut {
        if self.host_prerequisites_only {
            if root_override.is_some()
                || self.force
                || self.machine_name.is_some()
                || self.task_prefix.is_some()
            {
                return Err(OrbitError::InvalidInput(
                    "--host-prerequisites-only cannot be combined with --root, --force, --machine-name, or --task-prefix"
                        .to_string(),
                ));
            }
            #[cfg(target_os = "linux")]
            eprintln!("Linux sandbox: {}", self.prepare_linux_host()?.reason);
            return Ok(CommandOutput::Silent);
        }
        // Reject a malformed or (non-interactively) missing --machine-name/
        // --task-prefix before anything is written: skills, activities, jobs,
        // executors, and config.toml all seed ahead of the machine identity,
        // so a late validation error left a half-initialized root behind
        // [ORB-12112].
        reject_invalid_fresh_identity_inputs(
            root_override,
            self.non_interactive,
            self.machine_name.as_deref(),
            self.task_prefix.as_deref(),
        )?;
        // A custom root is used for isolated fixtures and does not authorize
        // changes to the machine's package/security policy. Normal init and
        // the shell installer share this preparation path.
        #[cfg(target_os = "linux")]
        let linux_sandbox = if root_override.is_some() {
            LinuxSandboxReadiness::skipped("host preparation skipped for a custom Orbit root")
        } else {
            self.prepare_linux_host()
                .unwrap_or_else(|error| LinuxSandboxReadiness {
                    status: LinuxSandboxStatus::NotReady,
                    reason: error.to_string(),
                })
        };
        #[cfg(target_os = "linux")]
        linux_sandbox.report();
        let config_seed =
            collect_config_seed_for_init(root_override, self.force, self.non_interactive)?;
        let result = init_global(
            root_override,
            InitOptions {
                force: self.force,
                refresh_defaults: true,
                config_seed: Some(config_seed),
                ..Default::default()
            },
        )?;
        // Machine identity is seeded into the global config.toml here —
        // `orbit init` is its sole writer. This runs after `init_global` has
        // written that file so the `[machine]` table joins an existing
        // document rather than replacing one.
        let identity = ensure_machine_identity_for_init(
            root_override,
            self.non_interactive,
            self.machine_name,
            self.task_prefix,
        )?;
        let paths = reported_init_paths(root_override);
        Ok(init_payload(
            &identity,
            InitOutput {
                skills_root: paths.skills_root,
                refreshed_skill_files: result.refreshed_skill_files,
                created_skills_symlink: result.created_skills_symlink,
                config_path: paths.config_path,
                created_config: result.created_config,
                refreshed_default_activities: result.refreshed_default_activities,
                retired_default_activities: result.retired_default_activities,
                refreshed_default_jobs: result.refreshed_default_jobs,
                retired_default_jobs: result.retired_default_jobs,
                managed_asset_warnings: result.managed_asset_warnings,
                refreshed_default_executors: result.refreshed_default_executors,
                refreshed_default_policies: result.refreshed_default_policies,
                #[cfg(target_os = "linux")]
                linux_sandbox,
            },
        ))
    }
}

fn resolve_global_root(root_override: Option<&Path>) -> Result<PathBuf, OrbitError> {
    match root_override {
        Some(root) => Ok(root.to_path_buf()),
        None => global_orbit_dir(),
    }
}

/// Validate operator-supplied `--machine-name`/`--task-prefix` inputs for a
/// fresh machine identity. Called both before `orbit init` writes anything (so
/// a rejected or, under `--non-interactive`, missing value leaves no partial
/// root [ORB-12112]) and again inside the identity-creation closure, which
/// stays self-sufficient against a racing concurrent create. A present
/// identity never reaches this function — both callers only consult it when
/// the identity is confirmed absent.
fn validate_fresh_identity_flags(
    non_interactive: bool,
    machine_name: Option<&str>,
    task_prefix: Option<&str>,
) -> Result<(), OrbitError> {
    match machine_name {
        Some(name) if name.trim().is_empty() => {
            return Err(OrbitError::InvalidInput(
                "machine name must not be empty".to_string(),
            ));
        }
        None if non_interactive => {
            return Err(OrbitError::InvalidInput(
                "machine identity is absent; pass --machine-name and --task-prefix \
                 to initialize a fresh machine non-interactively"
                    .to_string(),
            ));
        }
        _ => {}
    }
    match task_prefix {
        Some(prefix) => {
            validate_new_task_prefix(prefix)?;
        }
        None if non_interactive => {
            return Err(OrbitError::InvalidInput(
                "machine identity is absent; pass --task-prefix <PREFIX> (2-5 uppercase ASCII letters) \
                 to initialize a fresh machine non-interactively"
                    .to_string(),
            ));
        }
        None => {}
    }
    Ok(())
}

/// Reject a malformed, or under `--non-interactive` missing, `--machine-name`/
/// `--task-prefix` before `orbit init` writes anything. These flags are only
/// consulted when the machine identity is absent (a fresh create) — a present
/// identity ignores them entirely, so this check is skipped on the idempotent
/// re-init path, matching [`ensure_machine_identity_for_init`]'s own condition.
fn reject_invalid_fresh_identity_inputs(
    root_override: Option<&Path>,
    non_interactive: bool,
    machine_name: Option<&str>,
    task_prefix: Option<&str>,
) -> Result<(), OrbitError> {
    let global_root = resolve_global_root(root_override)?;
    if !matches!(
        inspect_machine_identity(&global_root)?,
        MachineIdentityState::Absent
    ) {
        return Ok(());
    }
    validate_fresh_identity_flags(non_interactive, machine_name, task_prefix)?;
    reject_prefix_contradicting_minted_ids(&global_root, task_prefix)
}

/// Refuse to create an identity whose task prefix contradicts ids this machine
/// already minted. `orbit workspace init` (or any task command) on a machine
/// with no identity allocates under the historical `ORB` prefix; an identity
/// naming another prefix would then be written and every later command would
/// fail at the allocator. Checked before anything is written.
fn reject_prefix_contradicting_minted_ids(
    global_root: &Path,
    requested_prefix: Option<&str>,
) -> Result<(), OrbitError> {
    match minted_prefix_conflict(allocated_task_prefix(global_root)?, requested_prefix) {
        Some(message) => Err(OrbitError::InvalidInput(message)),
        None => Ok(()),
    }
}

/// The refusal for a requested prefix that cannot bind to the ids already
/// minted, or `None` when it can. Adopting exactly the prefix in use is fine.
fn minted_prefix_conflict(
    allocated: Option<(String, u32)>,
    requested_prefix: Option<&str>,
) -> Option<String> {
    let (minted_prefix, minted) = allocated?;
    if requested_prefix == Some(minted_prefix.as_str()) {
        return None;
    }
    let wanted = requested_prefix
        .map(|prefix| format!(" '{prefix}'"))
        .unwrap_or_default();
    let how = if validate_new_task_prefix(&minted_prefix).is_ok() {
        format!("pass --task-prefix {minted_prefix} to adopt it")
    } else {
        format!(
            "'{minted_prefix}' is the historical default and cannot be chosen for a new \
             identity, so this store cannot be given one"
        )
    };
    Some(format!(
        "task prefix{wanted} cannot be chosen: this machine already minted {minted} task id(s) \
         under '{minted_prefix}' before `orbit init` ran (tasks created without a machine \
         identity mint under the historical default, e.g. after `orbit workspace init` alone). A task \
         prefix is fixed once ids exist, and {how}. Nothing was written. To pick a prefix, run \
         `orbit init` before creating tasks on a machine whose task store is still empty."
    ))
}

/// Create this machine's identity in the global `config.toml`. Machine name
/// and task prefix are only consulted when the identity is absent (a fresh
/// create); a present identity is preserved unchanged and a pre-ORB-12725
/// `host.toml` is folded in without prompting.
fn ensure_machine_identity_for_init(
    root_override: Option<&Path>,
    non_interactive: bool,
    machine_name: Option<String>,
    task_prefix: Option<String>,
) -> Result<IdentityReport, OrbitError> {
    let global_root = resolve_global_root(root_override)?;
    let requested_name = machine_name.clone();
    let requested_prefix = task_prefix.clone();
    let closure_root = global_root.clone();
    let outcome = ensure_machine_identity(&global_root, move || {
        validate_fresh_identity_flags(
            non_interactive,
            machine_name.as_deref(),
            task_prefix.as_deref(),
        )?;
        let machine_name = match machine_name {
            Some(name) => name,
            None => prompt_machine_name()?,
        };
        let task_prefix = match task_prefix {
            Some(prefix) => prefix,
            None => prompt_task_prefix()?,
        };
        // Also under the identity lock, after any prompt: the answer may name
        // a prefix the pre-write check could not see.
        reject_prefix_contradicting_minted_ids(&closure_root, Some(&task_prefix))?;
        Ok(NewMachineIdentity {
            name: machine_name,
            task_prefix,
        })
    })?;

    let identity = outcome.identity();
    // The identity is immutable once created, so a differing flag would
    // otherwise be dropped without a word.
    if matches!(outcome, MachineIdentityOutcome::Unchanged(_)) {
        if let Some(name) = requested_name.filter(|name| name.trim() != identity.name) {
            eprintln!(
                "warning: --machine-name '{name}' ignored: this machine is already named '{}' \
                 (change it with `orbit config set --global machine.name`)",
                identity.name
            );
        }
        if let Some(prefix) = requested_prefix.filter(|prefix| *prefix != identity.task_prefix) {
            eprintln!(
                "warning: --task-prefix '{prefix}' ignored: the task prefix '{}' was fixed when \
                 this machine was initialized and cannot change",
                identity.task_prefix
            );
        }
    }
    Ok(IdentityReport {
        outcome: match outcome {
            MachineIdentityOutcome::Created(_) => "created",
            MachineIdentityOutcome::Migrated(_) => "migrated",
            MachineIdentityOutcome::Unchanged(_) => "unchanged",
        },
        name: identity.name.clone(),
        id: identity.id.to_string(),
        task_prefix: identity.task_prefix.to_string(),
    })
}

/// The machine identity as `orbit init` reports it.
struct IdentityReport {
    outcome: &'static str,
    name: String,
    id: String,
    task_prefix: String,
}

fn prompt_machine_name() -> Result<String, OrbitError> {
    let default = os_hostname();
    let prompt = match default.as_deref() {
        Some(name) => format!("Machine name [{name}]: "),
        None => "Machine name: ".to_string(),
    };
    let answer = read_line(&prompt)?;
    if answer.is_empty() {
        default.ok_or_else(|| {
            OrbitError::InvalidInput(
                "no machine name entered and the OS hostname is unavailable; \
                 re-run with --machine-name"
                    .to_string(),
            )
        })
    } else {
        Ok(answer)
    }
}

const MAX_TASK_PREFIX_ATTEMPTS: usize = 4;

fn prompt_task_prefix() -> Result<String, OrbitError> {
    let stderr = io::stderr();
    let mut output = stderr.lock();
    collect_task_prefix(&mut output, |prompt, output| {
        prompt_stdin::read_trimmed_line(prompt, output).map_err(prompt_io_to_orbit)
    })
}

fn collect_task_prefix<W, F>(output: &mut W, mut read_answer: F) -> Result<String, OrbitError>
where
    W: Write,
    F: FnMut(&str, &mut W) -> Result<String, OrbitError>,
{
    for _ in 0..MAX_TASK_PREFIX_ATTEMPTS {
        let answer = read_answer("Task prefix (2-5 uppercase ASCII letters): ", output)?;
        match validate_new_task_prefix(&answer) {
            Ok(prefix) => return Ok(prefix),
            Err(error) => {
                writeln!(output, "{error}").map_err(|error| OrbitError::Io(error.to_string()))?;
            }
        }
    }

    Err(OrbitError::InvalidInput(format!(
        "task prefix remained invalid after {MAX_TASK_PREFIX_ATTEMPTS} attempts; pass --task-prefix or --non-interactive"
    )))
}

fn read_line(prompt: &str) -> Result<String, OrbitError> {
    let stderr = io::stderr();
    let mut output = stderr.lock();
    prompt_stdin::read_trimmed_line(prompt, &mut output).map_err(prompt_io_to_orbit)
}

fn prompt_io_to_orbit(error: io::Error) -> OrbitError {
    match error.kind() {
        ErrorKind::UnexpectedEof | ErrorKind::TimedOut => {
            OrbitError::InvalidInput(error.to_string())
        }
        _ => OrbitError::Io(error.to_string()),
    }
}

fn init_payload(identity: &IdentityReport, output: InitOutput) -> CommandOutput {
    let text = format!(
        "machine identity ({}): name=\"{}\", id={}, task_prefix={}\n\
         skills: root={}, refreshed={}, symlink_created={}; config: path={}, created={}; default_activities_refreshed={}, retired={}; default_jobs_refreshed={}, retired={}; default_executors_refreshed={}; default_policies_refreshed={}",
        identity.outcome,
        identity.name,
        identity.id,
        identity.task_prefix,
        output.skills_root,
        output.refreshed_skill_files,
        output.created_skills_symlink,
        output.config_path,
        output.created_config,
        output.refreshed_default_activities,
        output.retired_default_activities,
        output.refreshed_default_jobs,
        output.retired_default_jobs,
        output.refreshed_default_executors,
        output.refreshed_default_policies,
    );
    for warning in &output.managed_asset_warnings {
        eprintln!("warning: {warning}");
    }
    let doc = json!({
        "machine": {
            "outcome": identity.outcome,
            "name": identity.name,
            "id": identity.id,
            "task_prefix": identity.task_prefix,
        },
        "skills": {
            "root": output.skills_root,
            "refreshed": output.refreshed_skill_files,
            "symlink_created": output.created_skills_symlink,
        },
        "config": {
            "path": output.config_path,
            "created": output.created_config,
        },
        "defaults": {
            "activities_refreshed": output.refreshed_default_activities,
            "activities_retired": output.retired_default_activities,
            "jobs_refreshed": output.refreshed_default_jobs,
            "jobs_retired": output.retired_default_jobs,
            "executors_refreshed": output.refreshed_default_executors,
            "policies_refreshed": output.refreshed_default_policies,
        },
        "warnings": output.managed_asset_warnings,
    });
    #[cfg(target_os = "linux")]
    let doc = {
        let mut doc = doc;
        doc["linux_sandbox"] = json!(output.linux_sandbox);
        doc
    };
    Payload::detail(doc, text).into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InitOutput {
    skills_root: &'static str,
    refreshed_skill_files: usize,
    created_skills_symlink: bool,
    config_path: &'static str,
    created_config: bool,
    refreshed_default_activities: usize,
    retired_default_activities: usize,
    refreshed_default_jobs: usize,
    retired_default_jobs: usize,
    managed_asset_warnings: Vec<String>,
    refreshed_default_executors: usize,
    refreshed_default_policies: usize,
    #[cfg(target_os = "linux")]
    linux_sandbox: LinuxSandboxReadiness,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum LinuxSandboxStatus {
    Ready,
    Skipped,
    NotReady,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct LinuxSandboxReadiness {
    status: LinuxSandboxStatus,
    reason: String,
}

#[cfg(target_os = "linux")]
impl LinuxSandboxReadiness {
    fn skipped(reason: &str) -> Self {
        Self {
            status: LinuxSandboxStatus::Skipped,
            reason: reason.to_string(),
        }
    }

    fn report(&self) {
        if self.status == LinuxSandboxStatus::NotReady {
            eprintln!(
                "warning: Linux sandbox is not ready: {}; initialization continues. \
                 Fix the host using docs/runbooks/linux-sandbox.md, then retry \
                 `orbit init --host-prerequisites-only`. linux-bwrap dispatch stays blocked \
                 until `orbit doctor providers` reports the sandbox ready.",
                self.reason
            );
        } else {
            eprintln!("Linux sandbox: {}", self.reason);
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct ReportedInitPaths {
    skills_root: &'static str,
    config_path: &'static str,
}

fn reported_init_paths(root_override: Option<&Path>) -> ReportedInitPaths {
    if root_override.is_some_and(|path| !orbit_core::runtime::is_global_orbit_root(path)) {
        ReportedInitPaths {
            skills_root: "<custom orbit root>/skills",
            config_path: "<custom orbit root>/config.toml",
        }
    } else {
        ReportedInitPaths {
            skills_root: "~/.orbit/skills",
            config_path: "~/.orbit/config.toml",
        }
    }
}
